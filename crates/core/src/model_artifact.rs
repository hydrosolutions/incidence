//! model_artifact : ModelComponents ⇀ (ModelDigest, ImmutableModelArtifact)   (pure, deterministic)
//!
//! The digest is SHA-256 over the artifact's canonical V1 bytes.  Every value required to
//! reproduce execution is owned by the artifact; callers can observe, but cannot mutate, it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::canonical_encoding::{
    CanonicalEncode, CanonicalEncodingError, CanonicalField, CanonicalPayloadWriter,
};
use crate::execution_bindings::{
    ExecutionBindings, RuleInputBinding, RuleInputSource, TransferBranchBinding,
};
use crate::forcing::ForcingSeries;
use crate::identity::{CompartmentId, SubstanceId};
use crate::initial_stocks::InitialStocks;
use crate::interpolation_table::InterpolationTable;
use crate::numerical_semantics::NumericalSemanticsVersion;
use crate::partition_expression::{PartitionExpr, PartitionExprView};
use crate::projection::{
    AuthoritativeFactSelector, ProjectionSet, ProjectionSource, ProjectionSpecView,
    RecurrenceInputSource,
};
use crate::rule_expression::{RuleExpr, RuleExprView};
use crate::rule_reference::{
    ExpressionValueKind, ForcingId, InputId, ParameterId, ProjectionId, ProjectionValueKind,
    TableId, TransferBranchId,
};
use crate::substance_registry::SubstanceRegistry;
use crate::temporal::{FixedStepCalendar, RunHorizon};
use crate::topology::{Topology, TopologyEndpoint};
use crate::versions::{CanonicalEncodingVersion, InterpreterVersion, RuleIrVersion};

/// The protocol versions that participate in a model artifact's identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelVersions {
    rule_ir: RuleIrVersion,
    interpreter: InterpreterVersion,
    numerical_semantics: NumericalSemanticsVersion,
    canonical_encoding: CanonicalEncodingVersion,
}

impl ModelVersions {
    /// Constructs an explicit version selection.
    #[must_use]
    pub fn new(
        rule_ir: RuleIrVersion,
        interpreter: InterpreterVersion,
        numerical_semantics: NumericalSemanticsVersion,
        canonical_encoding: CanonicalEncodingVersion,
    ) -> Self {
        Self {
            rule_ir,
            interpreter,
            numerical_semantics,
            canonical_encoding,
        }
    }

    #[must_use]
    pub fn rule_ir(self) -> RuleIrVersion {
        self.rule_ir
    }
    #[must_use]
    pub fn interpreter(self) -> InterpreterVersion {
        self.interpreter
    }
    #[must_use]
    pub fn numerical_semantics(self) -> NumericalSemanticsVersion {
        self.numerical_semantics
    }
    #[must_use]
    pub fn canonical_encoding(self) -> CanonicalEncodingVersion {
        self.canonical_encoding
    }
}

impl Default for ModelVersions {
    fn default() -> Self {
        Self::new(
            RuleIrVersion::V1,
            InterpreterVersion::V1,
            NumericalSemanticsVersion::V1,
            CanonicalEncodingVersion::V1,
        )
    }
}

/// A canonical unit identity. Units remain opaque to the substance-agnostic engine.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnitId(String);

impl UnitId {
    /// Parses a non-empty ASCII unit identity without leading or trailing whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`ModelArtifactError::InvalidUnitIdentity`] for a non-canonical identity.
    pub fn parse(value: &str) -> Result<Self, ModelArtifactError> {
        if value.is_empty()
            || value.trim() != value
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ModelArtifactError::InvalidUnitIdentity {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for UnitId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Largest consecutive whole-number count exactly representable by binary64.
pub const MAX_EXACT_WHOLE_MULTIPLES: f64 = 9_007_199_254_740_992.0;
pub(crate) const MAX_EXACT_WHOLE_MULTIPLE_COUNT: u64 = 1_u64 << 53;

/// The smallest representable amount for one substance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantum(f64);

impl Quantum {
    /// Returns the positive finite amount represented by one whole quantum.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }

    /// Returns the largest total for which every whole-quantum count remains exact.
    #[must_use]
    pub fn countable_ceiling(self) -> f64 {
        self.0 * MAX_EXACT_WHOLE_MULTIPLES
    }

    /// Floors a computed extensive value to an independently represented whole-quantum count.
    pub(crate) fn floor_count(self, value: f64) -> Option<u64> {
        if !value.is_finite() || value < 0.0 || value > self.countable_ceiling() {
            return None;
        }
        if let Some(count) = self.whole_count(value) {
            return Some(count);
        }
        let quotient = (value / self.0).floor();
        if !quotient.is_finite() || !(0.0..=MAX_EXACT_WHOLE_MULTIPLES).contains(&quotient) {
            return None;
        }
        let mut count = quotient as u64;
        if self.to_value(count)? > value {
            count = count.checked_sub(1)?;
        }
        Some(count)
    }

    /// Returns the count when an external amount is exactly the binary64 image of whole quanta.
    pub(crate) fn whole_count(self, value: f64) -> Option<u64> {
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        let quotient = (value / self.0).round();
        if !quotient.is_finite() || !(0.0..=MAX_EXACT_WHOLE_MULTIPLES).contains(&quotient) {
            return None;
        }
        let count = quotient as u64;
        if self.to_value(count)?.to_bits() != value.to_bits() {
            return None;
        }
        let collides_below = count
            .checked_sub(1)
            .and_then(|neighbor| self.to_value(neighbor))
            .is_some_and(|neighbor| neighbor.to_bits() == value.to_bits());
        let collides_above = count
            .checked_add(1)
            .filter(|neighbor| *neighbor <= MAX_EXACT_WHOLE_MULTIPLE_COUNT)
            .and_then(|neighbor| self.to_value(neighbor))
            .is_some_and(|neighbor| neighbor.to_bits() == value.to_bits());
        (!collides_below && !collides_above).then_some(count)
    }

    pub(crate) fn has_ambiguous_whole_count(self, value: f64) -> bool {
        if !value.is_finite() || value < 0.0 {
            return false;
        }
        let quotient = (value / self.0).round();
        if !quotient.is_finite() || !(0.0..=MAX_EXACT_WHOLE_MULTIPLES).contains(&quotient) {
            return false;
        }
        let count = quotient as u64;
        let matches = |candidate| {
            self.to_value(candidate)
                .is_some_and(|projected| projected.to_bits() == value.to_bits())
        };
        matches(count)
            && (count.checked_sub(1).is_some_and(matches)
                || count
                    .checked_add(1)
                    .filter(|neighbor| *neighbor <= MAX_EXACT_WHOLE_MULTIPLE_COUNT)
                    .is_some_and(matches))
    }

    /// Converts an authoritative count to its deterministic public binary64 value.
    pub(crate) fn to_value(self, count: u64) -> Option<f64> {
        if count > MAX_EXACT_WHOLE_MULTIPLE_COUNT {
            return None;
        }
        let value = (count as f64) * self.0;
        value.is_finite().then_some(value)
    }

    /// Converts a signed boundary count to its deterministic public binary64 value.
    pub(crate) fn to_signed_value(self, count: i64) -> Option<f64> {
        if count.unsigned_abs() > MAX_EXACT_WHOLE_MULTIPLE_COUNT {
            return None;
        }
        let value = (count as f64) * self.0;
        value.is_finite().then_some(value)
    }
}

impl TryFrom<f64> for Quantum {
    type Error = ModelArtifactError;

    /// Parses a strictly positive finite quantum and canonicalizes no values.
    ///
    /// # Errors
    ///
    /// Returns [`ModelArtifactError::InvalidQuantum`] when `value` is zero, negative, NaN, or
    /// infinite.
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if !value.is_finite() || value <= 0.0 {
            return Err(ModelArtifactError::InvalidQuantum { value });
        }
        Ok(Self(value))
    }
}

/// The display unit and arithmetic quantum declared for one substance.
#[derive(Clone, Debug, PartialEq)]
pub struct SubstanceUnit {
    unit: UnitId,
    quantum: Quantum,
}

impl SubstanceUnit {
    #[must_use]
    pub const fn new(unit: UnitId, quantum: Quantum) -> Self {
        Self { unit, quantum }
    }

    #[must_use]
    pub fn unit(&self) -> &UnitId {
        &self.unit
    }

    #[must_use]
    pub const fn quantum(&self) -> Quantum {
        self.quantum
    }
}

/// One executable rule and its immutable scalar parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleDefinition {
    compartment: CompartmentId,
    substance: SubstanceId,
    expression: RuleExpr,
    disposition: PartitionExpr,
    parameters: BTreeMap<ParameterId, u64>,
}

impl RuleDefinition {
    /// Constructs a rule after canonicalising parameters and rejecting repeated identities.
    ///
    /// # Errors
    ///
    /// Returns [`ModelArtifactError::DuplicateParameter`] or
    /// [`ModelArtifactError::NonFiniteParameter`] when parameters are not canonical.
    pub fn new(
        compartment: CompartmentId,
        substance: SubstanceId,
        expression: RuleExpr,
        disposition: PartitionExpr,
        parameters: impl IntoIterator<Item = (ParameterId, f64)>,
    ) -> Result<Self, ModelArtifactError> {
        let semantics = expression.numerical_semantics_version();
        let mut values = BTreeMap::new();
        for (parameter, value) in parameters {
            let normalized =
                semantics
                    .normalize(value)
                    .map_err(|_| ModelArtifactError::NonFiniteParameter {
                        compartment: compartment.clone(),
                        substance: substance.clone(),
                        parameter: parameter.clone(),
                        bits: value.to_bits(),
                    })?;
            if values
                .insert(parameter.clone(), normalized.to_bits())
                .is_some()
            {
                return Err(ModelArtifactError::DuplicateParameter {
                    compartment,
                    substance,
                    parameter,
                });
            }
        }
        Ok(Self {
            compartment,
            substance,
            expression,
            disposition,
            parameters: values,
        })
    }

    #[must_use]
    pub fn compartment(&self) -> &CompartmentId {
        &self.compartment
    }
    #[must_use]
    pub fn substance(&self) -> &SubstanceId {
        &self.substance
    }
    #[must_use]
    pub fn expression(&self) -> &RuleExpr {
        &self.expression
    }
    #[must_use]
    pub fn disposition(&self) -> &PartitionExpr {
        &self.disposition
    }
    pub fn parameters(&self) -> impl ExactSizeIterator<Item = (&ParameterId, f64)> {
        self.parameters
            .iter()
            .map(|(id, bits)| (id, f64::from_bits(*bits)))
    }
}

/// One scalar replacement addressed only to a declared rule parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleParameterSubstitution {
    compartment: CompartmentId,
    substance: SubstanceId,
    parameter: ParameterId,
    value: f64,
}

impl RuleParameterSubstitution {
    #[must_use]
    pub fn new(
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
        value: f64,
    ) -> Self {
        Self {
            compartment,
            substance,
            parameter,
            value,
        }
    }

    #[must_use]
    pub fn compartment(&self) -> &CompartmentId {
        &self.compartment
    }

    #[must_use]
    pub fn substance(&self) -> &SubstanceId {
        &self.substance
    }

    #[must_use]
    pub fn parameter(&self) -> &ParameterId {
        &self.parameter
    }

    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }
}

/// A SHA-256 content address for canonical model-artifact bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelDigest([u8; 32]);

impl ModelDigest {
    /// Constructs a digest value from its fixed-width representation.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = String::with_capacity(64);
        for byte in self.0 {
            result.push(char::from(HEX[usize::from(byte >> 4)]));
            result.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        result
    }
}

impl Display for ModelDigest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// A complete immutable model value whose identity covers all execution inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelArtifact {
    topology: Topology,
    registry: SubstanceRegistry,
    initial_stocks: InitialStocks,
    initial_quantum_counts: BTreeMap<(CompartmentId, SubstanceId), u64>,
    projections: ProjectionSet,
    calendar: FixedStepCalendar,
    horizon: RunHorizon,
    forcings: BTreeMap<ForcingId, ForcingSeries>,
    tables: BTreeMap<TableId, InterpolationTable>,
    rules: BTreeMap<(CompartmentId, SubstanceId), RuleDefinition>,
    execution_bindings: ExecutionBindings,
    units: BTreeMap<SubstanceId, SubstanceUnit>,
    versions: ModelVersions,
    canonical_bytes: Box<[u8]>,
    digest: ModelDigest,
}

impl ModelArtifact {
    /// Starts a builder with the structurally bound components.
    #[must_use]
    pub fn builder(
        topology: Topology,
        registry: SubstanceRegistry,
        initial_stocks: InitialStocks,
        calendar: FixedStepCalendar,
        horizon: RunHorizon,
    ) -> ModelArtifactBuilder {
        ModelArtifactBuilder {
            topology,
            registry,
            initial_stocks,
            calendar,
            horizon,
            projections: None,
            forcings: Vec::new(),
            tables: Vec::new(),
            rules: Vec::new(),
            execution_bindings: ExecutionBindings::empty(),
            units: Vec::new(),
            versions: ModelVersions::default(),
        }
    }

    #[must_use]
    pub fn digest(&self) -> ModelDigest {
        self.digest
    }
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
    #[must_use]
    pub fn topology(&self) -> &Topology {
        &self.topology
    }
    #[must_use]
    pub fn registry(&self) -> &SubstanceRegistry {
        &self.registry
    }
    #[must_use]
    pub fn initial_stocks(&self) -> &InitialStocks {
        &self.initial_stocks
    }
    pub(crate) fn initial_quantum_count(
        &self,
        compartment: &CompartmentId,
        substance: &SubstanceId,
    ) -> Option<u64> {
        if !matches!(
            self.topology.endpoint(compartment),
            Some(TopologyEndpoint::Finite(_))
        ) || !self.registry.contains(substance)
        {
            return None;
        }
        Some(
            self.initial_quantum_counts
                .get(&(compartment.clone(), substance.clone()))
                .copied()
                .unwrap_or(0),
        )
    }
    #[must_use]
    pub fn projections(&self) -> &ProjectionSet {
        &self.projections
    }
    #[must_use]
    pub fn calendar(&self) -> FixedStepCalendar {
        self.calendar
    }
    #[must_use]
    pub fn horizon(&self) -> RunHorizon {
        self.horizon
    }
    #[must_use]
    pub fn versions(&self) -> ModelVersions {
        self.versions
    }
    pub fn forcings(&self) -> impl ExactSizeIterator<Item = &ForcingSeries> {
        self.forcings.values()
    }
    pub fn tables(&self) -> impl ExactSizeIterator<Item = &InterpolationTable> {
        self.tables.values()
    }
    pub fn rules(&self) -> impl ExactSizeIterator<Item = &RuleDefinition> {
        self.rules.values()
    }
    #[must_use]
    pub fn execution_bindings(&self) -> &ExecutionBindings {
        &self.execution_bindings
    }
    /// Returns the destination for a validated transfer branch.
    #[must_use]
    pub fn transfer_destination(
        &self,
        compartment: &CompartmentId,
        substance: &SubstanceId,
        branch: &TransferBranchId,
    ) -> Option<&CompartmentId> {
        self.execution_bindings
            .destination(compartment, substance, branch)
    }
    /// Returns the source for a validated generic rule input.
    #[must_use]
    pub fn rule_input_source(
        &self,
        compartment: &CompartmentId,
        substance: &SubstanceId,
        input: &InputId,
    ) -> Option<&RuleInputSource> {
        self.execution_bindings
            .input_source(compartment, substance, input)
    }
    pub fn units(&self) -> impl ExactSizeIterator<Item = (&SubstanceId, &SubstanceUnit)> {
        self.units.iter()
    }

    /// Returns the declared arithmetic quantum for `substance`, if it is modelled.
    #[must_use]
    pub fn quantum(&self, substance: &SubstanceId) -> Option<Quantum> {
        self.units.get(substance).map(SubstanceUnit::quantum)
    }

    /// Derives an artifact by replacing declared rule parameters and recomputing its identity.
    ///
    /// The source artifact is never mutated. This is the only supported model substitution path;
    /// forcings, topology, initial stocks, calendar fields, and every other artifact component are
    /// therefore not substitutable.
    ///
    /// # Errors
    ///
    /// Returns [`RuleParameterSubstitutionError`] when a coordinate is repeated, does not name a
    /// declared rule parameter, has a non-finite value, or cannot be canonically encoded.
    pub fn with_rule_parameter_substitutions(
        &self,
        substitutions: impl IntoIterator<Item = RuleParameterSubstitution>,
    ) -> Result<Self, RuleParameterSubstitutionError> {
        let mut derived = self.clone();
        let mut addressed = BTreeSet::new();
        for substitution in substitutions {
            let coordinate = (
                substitution.compartment.clone(),
                substitution.substance.clone(),
                substitution.parameter.clone(),
            );
            if !addressed.insert(coordinate.clone()) {
                return Err(RuleParameterSubstitutionError::DuplicateTarget {
                    compartment: coordinate.0,
                    substance: coordinate.1,
                    parameter: coordinate.2,
                });
            }
            let rule_coordinate = (
                substitution.compartment.clone(),
                substitution.substance.clone(),
            );
            let rule = derived.rules.get_mut(&rule_coordinate).ok_or_else(|| {
                RuleParameterSubstitutionError::RuleNotFound {
                    compartment: substitution.compartment.clone(),
                    substance: substitution.substance.clone(),
                }
            })?;
            let normalized = rule
                .expression
                .numerical_semantics_version()
                .normalize(substitution.value)
                .map_err(|_| RuleParameterSubstitutionError::NonFiniteValue {
                    compartment: substitution.compartment.clone(),
                    substance: substitution.substance.clone(),
                    parameter: substitution.parameter.clone(),
                    bits: substitution.value.to_bits(),
                })?;
            let value = rule
                .parameters
                .get_mut(&substitution.parameter)
                .ok_or_else(|| RuleParameterSubstitutionError::ParameterNotDeclared {
                    compartment: substitution.compartment.clone(),
                    substance: substitution.substance.clone(),
                    parameter: substitution.parameter.clone(),
                })?;
            *value = normalized.to_bits();
        }
        derived
            .refresh_identity()
            .map_err(|source| RuleParameterSubstitutionError::CanonicalEncoding { source })?;
        Ok(derived)
    }

    fn refresh_identity(&mut self) -> Result<(), CanonicalEncodingError> {
        let bytes = self.versions.canonical_encoding.encode(self)?;
        let mut hasher = Sha256::new();
        hasher.update(b"incidence:model-artifact:v1\0");
        hasher.update(&bytes);
        let hash: [u8; 32] = hasher.finalize().into();
        self.canonical_bytes = bytes.into_boxed_slice();
        self.digest = ModelDigest(hash);
        Ok(())
    }
}

/// Reports why a held artifact could not derive a parameter-substituted artifact.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RuleParameterSubstitutionError {
    /// Fires when one substitution coordinate occurs more than once.
    #[error(
        "rule parameter substitution repeats compartment `{compartment}`, substance `{substance}`, parameter `{parameter}`"
    )]
    DuplicateTarget {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
    },
    /// Fires when the addressed compartment and substance do not own a rule.
    #[error(
        "compartment `{compartment}`, substance `{substance}` has no rule; only declared rule parameters are substitutable"
    )]
    RuleNotFound {
        compartment: CompartmentId,
        substance: SubstanceId,
    },
    /// Fires when the addressed rule does not declare the named parameter.
    #[error(
        "parameter `{parameter}` is not declared by compartment `{compartment}`, substance `{substance}` and is not substitutable"
    )]
    ParameterNotDeclared {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
    },
    /// Fires when a replacement is NaN or infinite under the artifact's numerical semantics.
    #[error(
        "non-finite substitution for parameter `{parameter}` in compartment `{compartment}`, substance `{substance}`: bits {bits:#018x}"
    )]
    NonFiniteValue {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
        bits: u64,
    },
    /// Fires when the derived artifact cannot be represented by its canonical encoding version.
    #[error("parameter-substituted artifact cannot be canonically encoded: {source}")]
    CanonicalEncoding { source: CanonicalEncodingError },
}

/// Builder for a model artifact's optional collections and version selection.
pub struct ModelArtifactBuilder {
    topology: Topology,
    registry: SubstanceRegistry,
    initial_stocks: InitialStocks,
    calendar: FixedStepCalendar,
    horizon: RunHorizon,
    projections: Option<ProjectionSet>,
    forcings: Vec<ForcingSeries>,
    tables: Vec<InterpolationTable>,
    rules: Vec<RuleDefinition>,
    execution_bindings: ExecutionBindings,
    units: Vec<(SubstanceId, SubstanceUnit)>,
    versions: ModelVersions,
}

impl ModelArtifactBuilder {
    #[must_use]
    pub fn with_projections(mut self, projections: ProjectionSet) -> Self {
        self.projections = Some(projections);
        self
    }
    #[must_use]
    pub fn with_forcings(mut self, forcings: Vec<ForcingSeries>) -> Self {
        self.forcings = forcings;
        self
    }
    #[must_use]
    pub fn with_tables(mut self, tables: Vec<InterpolationTable>) -> Self {
        self.tables = tables;
        self
    }
    #[must_use]
    pub fn with_rules(mut self, rules: Vec<RuleDefinition>) -> Self {
        self.rules = rules;
        self
    }
    #[must_use]
    pub fn with_execution_bindings(mut self, bindings: ExecutionBindings) -> Self {
        self.execution_bindings = bindings;
        self
    }
    /// Concise alias for [`Self::with_execution_bindings`].
    #[must_use]
    pub fn with_bindings(self, bindings: ExecutionBindings) -> Self {
        self.with_execution_bindings(bindings)
    }
    /// Replaces transfer bindings while preserving configured rule-input bindings.
    ///
    /// # Errors
    ///
    /// Returns an error if either collection contains duplicate coordinates.
    pub fn with_transfer_bindings(
        mut self,
        bindings: Vec<TransferBranchBinding>,
    ) -> Result<Self, crate::execution_bindings::ExecutionBindingsError> {
        self.execution_bindings =
            ExecutionBindings::new(bindings, self.execution_bindings.input_bindings().cloned())?;
        Ok(self)
    }
    /// Replaces rule-input bindings while preserving configured transfer bindings.
    ///
    /// # Errors
    ///
    /// Returns an error if either collection contains duplicate coordinates.
    pub fn with_rule_input_bindings(
        mut self,
        bindings: Vec<RuleInputBinding>,
    ) -> Result<Self, crate::execution_bindings::ExecutionBindingsError> {
        self.execution_bindings = ExecutionBindings::new(
            self.execution_bindings.transfer_bindings().cloned(),
            bindings,
        )?;
        Ok(self)
    }
    #[must_use]
    pub fn with_units(mut self, units: Vec<(SubstanceId, SubstanceUnit)>) -> Self {
        self.units = units;
        self
    }
    #[must_use]
    pub fn with_versions(mut self, versions: ModelVersions) -> Self {
        self.versions = versions;
        self
    }

    /// Validates cross-component bindings, canonicalises the complete value, and hashes it.
    ///
    /// # Errors
    ///
    /// Returns [`ModelArtifactError`] for a component mismatch or canonical encoding failure.
    pub fn build(self) -> Result<ModelArtifact, ModelArtifactError> {
        if self.initial_stocks.topology() != &self.topology {
            return Err(ModelArtifactError::InitialStocksTopologyMismatch);
        }
        if self.initial_stocks.registry() != &self.registry {
            return Err(ModelArtifactError::InitialStocksRegistryMismatch);
        }
        let projections = self
            .projections
            .ok_or(ModelArtifactError::MissingProjectionSet)?;
        for specification in projections.iter() {
            if specification.rule_ir_version() != self.versions.rule_ir {
                return Err(ModelArtifactError::ProjectionRuleIrVersionMismatch {
                    projection: specification.id().clone(),
                });
            }
            if specification.numerical_semantics_version() != self.versions.numerical_semantics {
                return Err(ModelArtifactError::ProjectionNumericalVersionMismatch {
                    projection: specification.id().clone(),
                });
            }
            validate_projection_fact_selectors(
                specification.view(),
                &self.topology,
                &self.registry,
            )?;
        }
        let mut forcings = BTreeMap::new();
        for forcing in self.forcings {
            if forcing.horizon() != self.horizon {
                return Err(ModelArtifactError::ForcingHorizonMismatch {
                    forcing: forcing.id().clone(),
                });
            }
            let id = forcing.id().clone();
            if forcings.insert(id.clone(), forcing).is_some() {
                return Err(ModelArtifactError::DuplicateForcing { forcing: id });
            }
        }
        let mut tables = BTreeMap::new();
        for table in self.tables {
            let id = table.id().clone();
            if table.numerical_semantics_version() != self.versions.numerical_semantics {
                return Err(ModelArtifactError::TableNumericalVersionMismatch { table: id });
            }
            if tables.insert(id.clone(), table).is_some() {
                return Err(ModelArtifactError::DuplicateTable { table: id });
            }
        }
        let mut rules = BTreeMap::new();
        for rule in self.rules {
            let key = (rule.compartment.clone(), rule.substance.clone());
            match self.topology.endpoint(&key.0) {
                None => {
                    return Err(ModelArtifactError::UnknownRuleCompartment { compartment: key.0 });
                }
                Some(TopologyEndpoint::Boundary(_)) => {
                    return Err(ModelArtifactError::BoundaryAccountRule { account: key.0 });
                }
                Some(TopologyEndpoint::Finite(_)) => {}
            }
            if !self.registry.contains(&key.1) {
                return Err(ModelArtifactError::UnknownRuleSubstance { substance: key.1 });
            }
            if rule.expression.rule_ir_version() != self.versions.rule_ir
                || rule.disposition.rule_ir_version() != self.versions.rule_ir
            {
                return Err(ModelArtifactError::RuleIrVersionMismatch {
                    compartment: key.0,
                    substance: key.1,
                });
            }
            if rule.expression.numerical_semantics_version() != self.versions.numerical_semantics
                || rule.disposition.numerical_semantics_version()
                    != self.versions.numerical_semantics
            {
                return Err(ModelArtifactError::RuleNumericalVersionMismatch {
                    compartment: key.0,
                    substance: key.1,
                });
            }
            validate_rule_references(&rule, &forcings, &tables, &projections)?;
            if rules.insert(key.clone(), rule).is_some() {
                return Err(ModelArtifactError::DuplicateRule {
                    compartment: key.0,
                    substance: key.1,
                });
            }
        }
        validate_execution_bindings(
            &self.execution_bindings,
            &rules,
            &self.topology,
            &forcings,
            &tables,
            &projections,
        )?;
        validate_carrier_partitions(&rules, &self.registry, &self.execution_bindings)?;
        let mut units = BTreeMap::new();
        for (substance, unit) in self.units {
            if !self.registry.contains(&substance) {
                return Err(ModelArtifactError::UnknownUnitSubstance { substance });
            }
            if units.insert(substance.clone(), unit).is_some() {
                return Err(ModelArtifactError::DuplicateUnit { substance });
            }
        }
        let mut initial_quantum_counts = BTreeMap::new();
        for substance in self.registry.iter() {
            let declaration =
                units
                    .get(substance)
                    .ok_or_else(|| ModelArtifactError::MissingUnit {
                        substance: substance.clone(),
                    })?;
            let quantum = declaration.quantum();
            let mut total_count = 0_u128;
            let mut diagnostic_total = 0.0;
            for (compartment, stock) in self.initial_stocks.iter() {
                let Some((_, amount)) = stock.iter().find(|(id, _)| id == &substance) else {
                    continue;
                };
                let value = amount.value();
                let count = quantum.whole_count(value).ok_or_else(|| {
                    if quantum.has_ambiguous_whole_count(value) {
                        ModelArtifactError::AmbiguousInitialStock {
                            compartment: compartment.clone(),
                            substance: substance.clone(),
                            value,
                            quantum: quantum.value(),
                        }
                    } else {
                        ModelArtifactError::MisalignedInitialStock {
                            compartment: compartment.clone(),
                            substance: substance.clone(),
                            value,
                            quantum: quantum.value(),
                        }
                    }
                })?;
                initial_quantum_counts.insert((compartment.clone(), substance.clone()), count);
                total_count += u128::from(count);
                diagnostic_total += value;
            }
            if total_count > u128::from(MAX_EXACT_WHOLE_MULTIPLE_COUNT) {
                return Err(ModelArtifactError::UncountableInitialTotal {
                    substance: substance.clone(),
                    total: diagnostic_total,
                    countable_ceiling: quantum.countable_ceiling(),
                });
            }
        }
        let mut artifact = ModelArtifact {
            topology: self.topology,
            registry: self.registry,
            initial_stocks: self.initial_stocks,
            initial_quantum_counts,
            projections,
            calendar: self.calendar,
            horizon: self.horizon,
            forcings,
            tables,
            rules,
            execution_bindings: self.execution_bindings,
            units,
            versions: self.versions,
            canonical_bytes: Box::new([]),
            digest: ModelDigest([0; 32]),
        };
        artifact.refresh_identity()?;
        Ok(artifact)
    }
}

/// Reports why a complete model artifact could not be constructed.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ModelArtifactError {
    /// Fires when a dependent rule selects an unregistered carrier substance.
    #[error(
        "carrier `{carrier}` for compartment `{compartment}`, substance `{substance}` is absent from the registry"
    )]
    UnknownCarrierSubstance {
        compartment: CompartmentId,
        substance: SubstanceId,
        carrier: SubstanceId,
    },
    /// Fires when a dependent rule selects its own substance as carrier.
    #[error("compartment `{compartment}`, substance `{substance}` cannot be its own carrier")]
    SelfCarrier {
        compartment: CompartmentId,
        substance: SubstanceId,
    },
    /// Fires when no rule exists for the carrier at this compartment.
    #[error(
        "missing carrier rule `{carrier}` for compartment `{compartment}`, substance `{substance}`"
    )]
    MissingCarrierRule {
        compartment: CompartmentId,
        substance: SubstanceId,
        carrier: SubstanceId,
    },
    /// Fires when a carrier rule is itself carrier-proportional; chaining is unsupported.
    #[error(
        "carrier `{carrier}` for compartment `{compartment}`, substance `{substance}` is itself dependent"
    )]
    DependentCarrier {
        compartment: CompartmentId,
        substance: SubstanceId,
        carrier: SubstanceId,
    },
    /// Fires when a mapping names a branch absent from its carrier partition.
    #[error(
        "unknown carrier branch `{branch}` on `{carrier}` for compartment `{compartment}`, substance `{substance}`"
    )]
    UnknownCarrierBranch {
        compartment: CompartmentId,
        substance: SubstanceId,
        carrier: SubstanceId,
        branch: TransferBranchId,
    },
    /// Fires when dependent and carrier branch destinations differ.
    #[error(
        "carrier destination `{carrier_destination}` differs from dependent destination `{destination}` for compartment `{compartment}`, substance `{substance}`, branch `{branch}`"
    )]
    CarrierDestinationMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        branch: TransferBranchId,
        destination: CompartmentId,
        carrier_destination: CompartmentId,
    },

    /// Fires when an initial-stock value is bound to a different topology snapshot.
    #[error("initial stocks are bound to a different topology")]
    InitialStocksTopologyMismatch,
    /// Fires when an initial-stock value is bound to a different registry snapshot.
    #[error("initial stocks are bound to a different substance registry")]
    InitialStocksRegistryMismatch,
    /// Fires when no projection set (including an explicitly empty set) was supplied.
    #[error("model artifact requires an explicit projection set")]
    MissingProjectionSet,
    /// Fires when a projection's rule IR version differs from the artifact selection.
    #[error("projection `{projection}` has a different rule IR version")]
    ProjectionRuleIrVersionMismatch { projection: ProjectionId },
    /// Fires when a projection's numerical version differs from the artifact selection.
    #[error("projection `{projection}` has a different numerical semantics version")]
    ProjectionNumericalVersionMismatch { projection: ProjectionId },
    /// Fires when a projection reads transfer facts for an endpoint absent from the topology.
    #[error(
        "projection `{projection}` refers to compartment `{compartment}`, which is absent from the topology"
    )]
    UnknownProjectionFactCompartment {
        projection: ProjectionId,
        compartment: CompartmentId,
    },
    /// Fires when a projection reads transfer facts for a substance absent from the registry.
    #[error(
        "projection `{projection}` refers to substance `{substance}`, which is absent from the registry"
    )]
    UnknownProjectionFactSubstance {
        projection: ProjectionId,
        substance: SubstanceId,
    },
    /// Fires when a forcing does not cover the artifact horizon exactly.
    #[error("forcing `{forcing}` does not cover the model horizon")]
    ForcingHorizonMismatch { forcing: ForcingId },
    /// Fires when a forcing identity is repeated.
    #[error("duplicate forcing `{forcing}`")]
    DuplicateForcing { forcing: ForcingId },
    /// Fires when an interpolation-table identity is repeated.
    #[error("duplicate interpolation table `{table}`")]
    DuplicateTable { table: TableId },
    /// Fires when a table's numerical semantics version differs from the artifact selection.
    #[error("interpolation table `{table}` has a different numerical semantics version")]
    TableNumericalVersionMismatch { table: TableId },
    /// Fires when a rule names an endpoint absent from the topology.
    #[error("rule compartment `{compartment}` is absent from the topology")]
    UnknownRuleCompartment { compartment: CompartmentId },
    /// Fires when a rule is assigned to a boundary account rather than a finite compartment.
    #[error("boundary account `{account}` cannot own a rule")]
    BoundaryAccountRule { account: CompartmentId },
    /// Fires when a rule names a substance absent from the registry.
    #[error("rule substance `{substance}` is absent from the registry")]
    UnknownRuleSubstance { substance: SubstanceId },
    /// Fires when two rules name the same compartment-substance coordinate.
    #[error("duplicate rule for compartment `{compartment}`, substance `{substance}`")]
    DuplicateRule {
        compartment: CompartmentId,
        substance: SubstanceId,
    },
    /// Fires when a rule's IR version differs from the artifact selection.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` has a different rule IR version"
    )]
    RuleIrVersionMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
    },
    /// Fires when a rule's numerical version differs from the artifact selection.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` has a different numerical semantics version"
    )]
    RuleNumericalVersionMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
    },
    /// Fires when one rule parameter identity occurs twice.
    #[error(
        "duplicate parameter `{parameter}` for compartment `{compartment}`, substance `{substance}`"
    )]
    DuplicateParameter {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
    },
    /// Fires when a rule parameter is NaN or infinite.
    #[error(
        "non-finite parameter `{parameter}` for compartment `{compartment}`, substance `{substance}`: bits {bits:#018x}"
    )]
    NonFiniteParameter {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
        bits: u64,
    },
    /// Fires when an expression refers to an undeclared rule parameter.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` refers to missing parameter `{parameter}`"
    )]
    MissingRuleParameter {
        compartment: CompartmentId,
        substance: SubstanceId,
        parameter: ParameterId,
    },
    /// Fires when an expression or partition refers to an absent forcing series.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` refers to missing forcing `{forcing}`"
    )]
    MissingRuleForcing {
        compartment: CompartmentId,
        substance: SubstanceId,
        forcing: ForcingId,
    },
    /// Fires when an expression refers to an absent interpolation table.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` refers to missing table `{table}`"
    )]
    MissingRuleTable {
        compartment: CompartmentId,
        substance: SubstanceId,
        table: TableId,
    },
    /// Fires when an expression refers to an absent projection specification.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` refers to missing projection `{projection}`"
    )]
    MissingRuleProjection {
        compartment: CompartmentId,
        substance: SubstanceId,
        projection: ProjectionId,
    },
    /// Fires when an expression projection reference misstates the declared value kind.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` expects projection `{projection}` kind {expected:?}, but the reference declares {actual:?}"
    )]
    RuleProjectionKindMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        projection: ProjectionId,
        expected: ProjectionValueKind,
        actual: ProjectionValueKind,
    },
    /// Fires when a partition branch has no declared destination.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` has unbound transfer branch `{branch}`"
    )]
    MissingTransferBranchBinding {
        compartment: CompartmentId,
        substance: SubstanceId,
        branch: TransferBranchId,
    },
    /// Fires when a transfer branch destination is absent from the topology.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}`, branch `{branch}` binds undeclared endpoint `{destination}`"
    )]
    UnknownTransferDestination {
        compartment: CompartmentId,
        substance: SubstanceId,
        branch: TransferBranchId,
        destination: CompartmentId,
    },
    /// Fires when a transfer binding names no branch of the selected rule.
    #[error(
        "transfer binding for compartment `{compartment}`, substance `{substance}` names unknown branch `{branch}`"
    )]
    UnknownTransferBranchBinding {
        compartment: CompartmentId,
        substance: SubstanceId,
        branch: TransferBranchId,
    },
    /// Fires when transfer bindings introduce a cycle into the execution graph.
    #[error(
        "transfer bindings make the execution graph cyclic; blocked compartments: {blocked_compartments:?}"
    )]
    CyclicExecutionBindings {
        blocked_compartments: Vec<CompartmentId>,
    },
    /// Fires when a generic input leaf has no declared source.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}` has unbound input `{input}`"
    )]
    MissingRuleInputBinding {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
    },
    /// Fires when an input binding misstates the value kind declared by its rule leaf.
    #[error(
        "input binding for compartment `{compartment}`, substance `{substance}`, input `{input}` declares {actual:?}, but the rule expects {expected:?}"
    )]
    RuleInputBindingKindMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
        expected: ExpressionValueKind,
        actual: ExpressionValueKind,
    },
    /// Fires when an input binding names no input leaf of the selected rule.
    #[error(
        "input binding for compartment `{compartment}`, substance `{substance}` names unknown input `{input}`"
    )]
    UnknownRuleInputBinding {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
    },
    /// Fires when a bound input source is absent from the artifact source catalogue.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}`, input `{input}` binds missing {source_kind} source `{source_identity}`"
    )]
    MissingRuleInputSource {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
        source_kind: &'static str,
        source_identity: String,
    },
    /// Fires when an input leaf and its bound source have incompatible value kinds.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}`, input `{input}` expects {expected:?}, but its source provides {actual:?}"
    )]
    RuleInputKindMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
        expected: ExpressionValueKind,
        actual: ExpressionValueKind,
    },
    /// Fires when a projection input source misstates the declared projection value kind.
    #[error(
        "rule for compartment `{compartment}`, substance `{substance}`, input `{input}` expects projection `{projection}` kind {expected:?}, but the binding declares {actual:?}"
    )]
    RuleInputProjectionKindMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        input: InputId,
        projection: ProjectionId,
        expected: ProjectionValueKind,
        actual: ProjectionValueKind,
    },
    /// Fires when a unit identity is empty, non-ASCII, or padded with whitespace.
    #[error("invalid canonical unit identity `{value}`")]
    InvalidUnitIdentity { value: String },
    /// Fires when a quantum is not strictly positive and finite.
    #[error("unit quantum must be positive and finite, got {value}")]
    InvalidQuantum { value: f64 },
    /// Fires when an initial stock value is the projection of more than one quantum count.
    #[error(
        "initial stock in compartment `{compartment}` for substance `{substance}` has ambiguous value {value} at quantum {quantum}"
    )]
    AmbiguousInitialStock {
        compartment: CompartmentId,
        substance: SubstanceId,
        value: f64,
        quantum: f64,
    },
    /// Fires when an initial stock is not an exact whole multiple of its substance quantum.
    #[error(
        "initial stock in compartment `{compartment}` for substance `{substance}` has value {value}, not an exact multiple of quantum {quantum}"
    )]
    MisalignedInitialStock {
        compartment: CompartmentId,
        substance: SubstanceId,
        value: f64,
        quantum: f64,
    },
    /// Fires when a substance total has more whole quanta than binary64 can count exactly.
    #[error(
        "substance `{substance}` declared total {total} exceeds the exactly countable ceiling {countable_ceiling}"
    )]
    UncountableInitialTotal {
        substance: SubstanceId,
        total: f64,
        countable_ceiling: f64,
    },
    /// Fires when a unit names an unmodelled substance.
    #[error("unit declaration names unmodelled substance `{substance}`")]
    UnknownUnitSubstance { substance: SubstanceId },
    /// Fires when a substance unit is declared more than once.
    #[error("duplicate unit declaration for substance `{substance}`")]
    DuplicateUnit { substance: SubstanceId },
    /// Fires when a modelled substance has no unit declaration.
    #[error("missing unit declaration for substance `{substance}`")]
    MissingUnit { substance: SubstanceId },
    /// Fires when canonical bytes cannot represent a field.
    #[error(transparent)]
    CanonicalEncoding(#[from] CanonicalEncodingError),
}

fn validate_projection_fact_selectors(
    specification: ProjectionSpecView<'_>,
    topology: &Topology,
    registry: &SubstanceRegistry,
) -> Result<(), ModelArtifactError> {
    match specification {
        ProjectionSpecView::BoundedLag(specification) => validate_projection_source(
            specification.id(),
            specification.source(),
            topology,
            registry,
        ),
        ProjectionSpecView::OrderedRollingAggregate(specification) => validate_projection_source(
            specification.id(),
            specification.source(),
            topology,
            registry,
        ),
        ProjectionSpecView::FiniteRecurrence(specification) => {
            for binding in specification.inputs() {
                if let RecurrenceInputSource::AuthoritativeFact(selector) = binding.input_source() {
                    validate_projection_fact_selector(
                        specification.id(),
                        selector,
                        topology,
                        registry,
                    )?;
                }
            }
            Ok(())
        }
    }
}

fn validate_projection_source(
    projection: &ProjectionId,
    source: &ProjectionSource,
    topology: &Topology,
    registry: &SubstanceRegistry,
) -> Result<(), ModelArtifactError> {
    if let ProjectionSource::AuthoritativeFact(selector) = source {
        validate_projection_fact_selector(projection, selector, topology, registry)?;
    }
    Ok(())
}

fn validate_projection_fact_selector(
    projection: &ProjectionId,
    selector: &AuthoritativeFactSelector,
    topology: &Topology,
    registry: &SubstanceRegistry,
) -> Result<(), ModelArtifactError> {
    if topology.endpoint(selector.compartment()).is_none() {
        return Err(ModelArtifactError::UnknownProjectionFactCompartment {
            projection: projection.clone(),
            compartment: selector.compartment().clone(),
        });
    }
    if !registry.contains(selector.substance()) {
        return Err(ModelArtifactError::UnknownProjectionFactSubstance {
            projection: projection.clone(),
            substance: selector.substance().clone(),
        });
    }
    Ok(())
}

fn validate_rule_references(
    rule: &RuleDefinition,
    forcings: &BTreeMap<ForcingId, ForcingSeries>,
    tables: &BTreeMap<TableId, InterpolationTable>,
    projections: &ProjectionSet,
) -> Result<(), ModelArtifactError> {
    let mut parameters = BTreeSet::new();
    let mut forcing_refs = BTreeSet::new();
    let mut table_refs = BTreeSet::new();
    let mut projection_refs = BTreeSet::new();
    collect_expression_references(
        &rule.expression,
        &mut parameters,
        &mut forcing_refs,
        &mut table_refs,
        &mut projection_refs,
    );
    match rule.disposition.view() {
        PartitionExprView::ExogenousSeries { series, .. } => {
            forcing_refs.insert(series.id().clone());
        }
        PartitionExprView::ExpressionPartition { branches } => {
            for branch in branches {
                collect_expression_references(
                    branch.expression(),
                    &mut parameters,
                    &mut forcing_refs,
                    &mut table_refs,
                    &mut projection_refs,
                );
            }
        }
        PartitionExprView::RetainAll
        | PartitionExprView::ReleaseAll { .. }
        | PartitionExprView::FixedFractionSplit { .. }
        | PartitionExprView::ConstantFractionTransfer { .. }
        | PartitionExprView::CarrierProportional { .. } => {}
    }
    for parameter in parameters {
        if !rule.parameters.contains_key(&parameter) {
            return Err(ModelArtifactError::MissingRuleParameter {
                compartment: rule.compartment.clone(),
                substance: rule.substance.clone(),
                parameter,
            });
        }
    }
    for forcing in forcing_refs {
        if !forcings.contains_key(&forcing) {
            return Err(ModelArtifactError::MissingRuleForcing {
                compartment: rule.compartment.clone(),
                substance: rule.substance.clone(),
                forcing,
            });
        }
    }
    for table in table_refs {
        if !tables.contains_key(&table) {
            return Err(ModelArtifactError::MissingRuleTable {
                compartment: rule.compartment.clone(),
                substance: rule.substance.clone(),
                table,
            });
        }
    }
    for (projection, actual) in projection_refs {
        let Some(specification) = projections
            .iter()
            .find(|specification| specification.id() == &projection)
        else {
            return Err(ModelArtifactError::MissingRuleProjection {
                compartment: rule.compartment.clone(),
                substance: rule.substance.clone(),
                projection,
            });
        };
        let expected = specification.value_kind();
        if expected != actual {
            return Err(ModelArtifactError::RuleProjectionKindMismatch {
                compartment: rule.compartment.clone(),
                substance: rule.substance.clone(),
                projection,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn validate_execution_bindings(
    bindings: &ExecutionBindings,
    rules: &BTreeMap<(CompartmentId, SubstanceId), RuleDefinition>,
    topology: &Topology,
    forcings: &BTreeMap<ForcingId, ForcingSeries>,
    tables: &BTreeMap<TableId, InterpolationTable>,
    projections: &ProjectionSet,
) -> Result<(), ModelArtifactError> {
    for ((compartment, substance), rule) in rules {
        let branches = partition_branches(rule.disposition());
        for branch in &branches {
            let Some(destination) = bindings.destination(compartment, substance, branch) else {
                return Err(ModelArtifactError::MissingTransferBranchBinding {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    branch: branch.clone(),
                });
            };
            if topology.endpoint(destination).is_none() {
                return Err(ModelArtifactError::UnknownTransferDestination {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    branch: branch.clone(),
                    destination: destination.clone(),
                });
            }
        }
        let mut inputs = Vec::new();
        collect_rule_inputs(rule, &mut inputs);
        for reference in &inputs {
            let Some(binding) = bindings.input_binding(compartment, substance, reference.id())
            else {
                return Err(ModelArtifactError::MissingRuleInputBinding {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    input: reference.id().clone(),
                });
            };
            if binding.reference().value_kind() != reference.value_kind() {
                return Err(ModelArtifactError::RuleInputBindingKindMismatch {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    input: reference.id().clone(),
                    expected: reference.value_kind(),
                    actual: binding.reference().value_kind(),
                });
            }
            validate_input_source(
                compartment,
                substance,
                reference,
                binding.source(),
                forcings,
                tables,
                projections,
            )?;
        }
    }
    for binding in bindings.transfer_bindings() {
        let key = (binding.compartment().clone(), binding.substance().clone());
        let known = rules
            .get(&key)
            .is_some_and(|rule| partition_branches(rule.disposition()).contains(binding.branch()));
        if !known {
            return Err(ModelArtifactError::UnknownTransferBranchBinding {
                compartment: binding.compartment().clone(),
                substance: binding.substance().clone(),
                branch: binding.branch().clone(),
            });
        }
    }
    for binding in bindings.input_bindings() {
        let key = (binding.compartment().clone(), binding.substance().clone());
        let mut inputs = Vec::new();
        if let Some(rule) = rules.get(&key) {
            collect_rule_inputs(rule, &mut inputs);
        }
        if !inputs
            .iter()
            .any(|reference| reference.id() == binding.input())
        {
            return Err(ModelArtifactError::UnknownRuleInputBinding {
                compartment: binding.compartment().clone(),
                substance: binding.substance().clone(),
                input: binding.input().clone(),
            });
        }
    }
    validate_execution_graph_is_acyclic(bindings, topology)
}

fn validate_execution_graph_is_acyclic(
    bindings: &ExecutionBindings,
    topology: &Topology,
) -> Result<(), ModelArtifactError> {
    let mut edges = topology
        .connections()
        .iter()
        .map(|connection| (connection.source().clone(), connection.target().clone()))
        .collect::<BTreeSet<_>>();
    edges.extend(
        bindings
            .transfer_bindings()
            .map(|binding| (binding.compartment().clone(), binding.destination().clone())),
    );

    let mut indegrees = topology
        .endpoints()
        .map(|endpoint| (endpoint.id().clone(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = topology
        .endpoints()
        .map(|endpoint| (endpoint.id().clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (source, target) in edges {
        let Some(targets) = adjacency.get_mut(&source) else {
            unreachable!("validated transfer source must be a declared endpoint");
        };
        targets.insert(target.clone());
        let Some(indegree) = indegrees.get_mut(&target) else {
            unreachable!("validated transfer destination must be a declared endpoint");
        };
        *indegree += 1;
    }

    let mut ready = indegrees
        .iter()
        .filter(|(_, indegree)| **indegree == 0)
        .map(|(compartment, _)| compartment.clone())
        .collect::<BTreeSet<_>>();
    while let Some(compartment) = ready.pop_first() {
        let Some(targets) = adjacency.get(&compartment) else {
            unreachable!("declared endpoint must have an adjacency entry");
        };
        for target in targets {
            let Some(indegree) = indegrees.get_mut(target) else {
                unreachable!("validated transfer destination must have an indegree entry");
            };
            *indegree -= 1;
            if *indegree == 0 {
                ready.insert(target.clone());
            }
        }
    }

    let blocked_compartments = indegrees
        .into_iter()
        .filter(|(_, indegree)| *indegree != 0)
        .map(|(compartment, _)| compartment)
        .collect::<Vec<_>>();
    if blocked_compartments.is_empty() {
        Ok(())
    } else {
        Err(ModelArtifactError::CyclicExecutionBindings {
            blocked_compartments,
        })
    }
}

fn validate_carrier_partitions(
    rules: &BTreeMap<(CompartmentId, SubstanceId), RuleDefinition>,
    registry: &SubstanceRegistry,
    bindings: &ExecutionBindings,
) -> Result<(), ModelArtifactError> {
    for ((compartment, substance), rule) in rules {
        let PartitionExprView::CarrierProportional { carrier, branches } =
            rule.disposition().view()
        else {
            continue;
        };
        if !registry.contains(carrier) {
            return Err(ModelArtifactError::UnknownCarrierSubstance {
                compartment: compartment.clone(),
                substance: substance.clone(),
                carrier: carrier.clone(),
            });
        }
        if carrier == substance {
            return Err(ModelArtifactError::SelfCarrier {
                compartment: compartment.clone(),
                substance: substance.clone(),
            });
        }
        let carrier_rule = rules
            .get(&(compartment.clone(), carrier.clone()))
            .ok_or_else(|| ModelArtifactError::MissingCarrierRule {
                compartment: compartment.clone(),
                substance: substance.clone(),
                carrier: carrier.clone(),
            })?;
        if matches!(
            carrier_rule.disposition().view(),
            PartitionExprView::CarrierProportional { .. }
        ) {
            return Err(ModelArtifactError::DependentCarrier {
                compartment: compartment.clone(),
                substance: substance.clone(),
                carrier: carrier.clone(),
            });
        }
        let carrier_branches = partition_branches(carrier_rule.disposition());
        for branch in branches {
            if !carrier_branches.contains(branch.carrier_branch()) {
                return Err(ModelArtifactError::UnknownCarrierBranch {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    carrier: carrier.clone(),
                    branch: branch.carrier_branch().clone(),
                });
            }
            let destination = bindings
                .destination(compartment, substance, branch.branch())
                .ok_or_else(|| ModelArtifactError::MissingTransferBranchBinding {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    branch: branch.branch().clone(),
                })?;
            let carrier_destination = bindings
                .destination(compartment, carrier, branch.carrier_branch())
                .ok_or_else(|| ModelArtifactError::MissingTransferBranchBinding {
                    compartment: compartment.clone(),
                    substance: carrier.clone(),
                    branch: branch.carrier_branch().clone(),
                })?;
            if destination != carrier_destination {
                return Err(ModelArtifactError::CarrierDestinationMismatch {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    branch: branch.branch().clone(),
                    destination: destination.clone(),
                    carrier_destination: carrier_destination.clone(),
                });
            }
        }
    }
    Ok(())
}

fn partition_branches(disposition: &PartitionExpr) -> BTreeSet<TransferBranchId> {
    let mut branches = BTreeSet::new();
    match disposition.view() {
        PartitionExprView::RetainAll => {}
        PartitionExprView::ReleaseAll { branch }
        | PartitionExprView::ExogenousSeries { branch, .. }
        | PartitionExprView::ConstantFractionTransfer { branch, .. } => {
            branches.insert(branch.clone());
        }
        PartitionExprView::FixedFractionSplit {
            branches: split, ..
        } => {
            branches.extend(split.iter().map(|part| part.branch().clone()));
        }
        PartitionExprView::ExpressionPartition { branches: split } => {
            branches.extend(split.iter().map(|part| part.branch().clone()));
        }
        PartitionExprView::CarrierProportional {
            branches: split, ..
        } => {
            branches.extend(split.iter().map(|part| part.branch().clone()));
        }
    }
    branches
}

fn validate_input_source(
    compartment: &CompartmentId,
    substance: &SubstanceId,
    reference: &crate::rule_reference::InputRef,
    source: &RuleInputSource,
    forcings: &BTreeMap<ForcingId, ForcingSeries>,
    tables: &BTreeMap<TableId, InterpolationTable>,
    projections: &ProjectionSet,
) -> Result<(), ModelArtifactError> {
    let missing = |source_kind: &'static str, source_identity: String| {
        ModelArtifactError::MissingRuleInputSource {
            compartment: compartment.clone(),
            substance: substance.clone(),
            input: reference.id().clone(),
            source_kind,
            source_identity,
        }
    };
    match source {
        RuleInputSource::Forcing(forcing) => {
            if !forcings.contains_key(forcing.id()) {
                return Err(missing("forcing", forcing.id().to_string()));
            }
        }
        RuleInputSource::InterpolationTable(table) => {
            if !tables.contains_key(table.id()) {
                return Err(missing("interpolation-table", table.id().to_string()));
            }
        }
        RuleInputSource::Projection(projection) => {
            let Some(specification) = projections.iter().find(|item| item.id() == projection.id())
            else {
                return Err(missing("projection", projection.id().to_string()));
            };
            if specification.value_kind() != projection.value_kind() {
                return Err(ModelArtifactError::RuleInputProjectionKindMismatch {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    input: reference.id().clone(),
                    projection: projection.id().clone(),
                    expected: specification.value_kind(),
                    actual: projection.value_kind(),
                });
            }
        }
    }
    if reference.value_kind() != source.value_kind() {
        return Err(ModelArtifactError::RuleInputKindMismatch {
            compartment: compartment.clone(),
            substance: substance.clone(),
            input: reference.id().clone(),
            expected: reference.value_kind(),
            actual: source.value_kind(),
        });
    }
    Ok(())
}

fn collect_rule_inputs<'a>(
    rule: &'a RuleDefinition,
    inputs: &mut Vec<&'a crate::rule_reference::InputRef>,
) {
    collect_expression_inputs(rule.expression(), inputs);
    if let PartitionExprView::ExpressionPartition { branches } = rule.disposition().view() {
        for branch in branches {
            collect_expression_inputs(branch.expression(), inputs);
        }
    }
}

fn collect_expression_inputs<'a>(
    expression: &'a RuleExpr,
    inputs: &mut Vec<&'a crate::rule_reference::InputRef>,
) {
    match expression.view() {
        RuleExprView::Input(reference) => inputs.push(reference),
        RuleExprView::InterpolatedTable { input, .. } => collect_expression_inputs(input, inputs),
        RuleExprView::Add { lhs, rhs }
        | RuleExprView::Subtract { lhs, rhs }
        | RuleExprView::Multiply { lhs, rhs }
        | RuleExprView::Divide { lhs, rhs }
        | RuleExprView::Power { lhs, rhs }
        | RuleExprView::Minimum { lhs, rhs }
        | RuleExprView::Maximum { lhs, rhs }
        | RuleExprView::Comparison { lhs, rhs, .. } => {
            collect_expression_inputs(lhs, inputs);
            collect_expression_inputs(rhs, inputs);
        }
        RuleExprView::Clamp {
            value,
            lower,
            upper,
        } => {
            collect_expression_inputs(value, inputs);
            collect_expression_inputs(lower, inputs);
            collect_expression_inputs(upper, inputs);
        }
        RuleExprView::Select {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression_inputs(condition, inputs);
            collect_expression_inputs(when_true, inputs);
            collect_expression_inputs(when_false, inputs);
        }
        RuleExprView::Parameter(_)
        | RuleExprView::Forcing(_)
        | RuleExprView::Projection(_)
        | RuleExprView::Literal(_) => {}
    }
}

fn collect_expression_references(
    expression: &RuleExpr,
    parameters: &mut BTreeSet<ParameterId>,
    forcings: &mut BTreeSet<ForcingId>,
    tables: &mut BTreeSet<TableId>,
    projections: &mut BTreeSet<(ProjectionId, ProjectionValueKind)>,
) {
    match expression.view() {
        RuleExprView::Parameter(reference) => {
            parameters.insert(reference.id().clone());
        }
        RuleExprView::Forcing(reference) => {
            forcings.insert(reference.id().clone());
        }
        RuleExprView::Projection(reference) => {
            projections.insert((reference.id().clone(), reference.value_kind()));
        }
        RuleExprView::InterpolatedTable { table, input } => {
            tables.insert(table.id().clone());
            collect_expression_references(input, parameters, forcings, tables, projections);
        }
        RuleExprView::Add { lhs, rhs }
        | RuleExprView::Subtract { lhs, rhs }
        | RuleExprView::Multiply { lhs, rhs }
        | RuleExprView::Divide { lhs, rhs }
        | RuleExprView::Power { lhs, rhs }
        | RuleExprView::Minimum { lhs, rhs }
        | RuleExprView::Maximum { lhs, rhs }
        | RuleExprView::Comparison { lhs, rhs, .. } => {
            collect_expression_references(lhs, parameters, forcings, tables, projections);
            collect_expression_references(rhs, parameters, forcings, tables, projections);
        }
        RuleExprView::Clamp {
            value,
            lower,
            upper,
        } => {
            collect_expression_references(value, parameters, forcings, tables, projections);
            collect_expression_references(lower, parameters, forcings, tables, projections);
            collect_expression_references(upper, parameters, forcings, tables, projections);
        }
        RuleExprView::Select {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression_references(condition, parameters, forcings, tables, projections);
            collect_expression_references(when_true, parameters, forcings, tables, projections);
            collect_expression_references(when_false, parameters, forcings, tables, projections);
        }
        RuleExprView::Input(_) | RuleExprView::Literal(_) => {}
    }
}

impl CanonicalEncode for ModelArtifact {
    fn root_tag(&self) -> u16 {
        0x0020
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        self.versions.rule_ir.encode_payload(writer)?;
        self.versions.interpreter.encode_payload(writer)?;
        self.versions.numerical_semantics.encode_payload(writer)?;
        self.versions.canonical_encoding.encode_payload(writer)?;
        self.topology.encode_payload(writer)?;
        self.registry.encode_payload(writer)?;
        self.initial_stocks.encode_payload(writer)?;
        self.calendar.encode_payload(writer)?;
        self.horizon.encode_payload(writer)?;
        writer.write_count(
            CanonicalField::ArtifactProjections,
            self.projections.iter().len(),
        )?;
        for specification in self.projections.iter() {
            specification.encode_payload(writer)?;
        }
        writer.write_count(
            CanonicalField::ArtifactProjectorStates,
            self.projections.initial_states().len(),
        )?;
        for (_, state) in self.projections.initial_states() {
            state.encode_payload(writer)?;
        }
        writer.write_count(CanonicalField::ArtifactForcings, self.forcings.len())?;
        for forcing in self.forcings.values() {
            forcing.encode_payload(writer)?;
        }
        writer.write_count(CanonicalField::ArtifactTables, self.tables.len())?;
        for table in self.tables.values() {
            table.encode_payload(writer)?;
        }
        writer.write_count(CanonicalField::ArtifactRules, self.rules.len())?;
        for rule in self.rules.values() {
            writer.write_string(
                CanonicalField::ArtifactRuleCompartment,
                rule.compartment.as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactRuleSubstance,
                rule.substance.as_str(),
            )?;
            rule.expression.encode_payload(writer)?;
            rule.disposition.encode_payload(writer)?;
            writer.write_count(CanonicalField::ArtifactParameters, rule.parameters.len())?;
            for (parameter, bits) in &rule.parameters {
                writer.write_string(
                    CanonicalField::ArtifactParameterIdentity,
                    parameter.as_str(),
                )?;
                writer.write_scalar(
                    CanonicalField::ArtifactParameterValue,
                    f64::from_bits(*bits),
                )?;
            }
        }
        writer.write_count(
            CanonicalField::ArtifactTransferBindings,
            self.execution_bindings.transfer_bindings().len(),
        )?;
        for binding in self.execution_bindings.transfer_bindings() {
            writer.write_string(
                CanonicalField::ArtifactTransferBindingCompartment,
                binding.compartment().as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactTransferBindingSubstance,
                binding.substance().as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactTransferBindingBranch,
                binding.branch().as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactTransferBindingDestination,
                binding.destination().as_str(),
            )?;
        }
        writer.write_count(
            CanonicalField::ArtifactInputBindings,
            self.execution_bindings.input_bindings().len(),
        )?;
        for binding in self.execution_bindings.input_bindings() {
            writer.write_string(
                CanonicalField::ArtifactInputBindingCompartment,
                binding.compartment().as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactInputBindingSubstance,
                binding.substance().as_str(),
            )?;
            writer.write_string(
                CanonicalField::ArtifactInputBindingIdentity,
                binding.input().as_str(),
            )?;
            writer.write_u8(match binding.reference().value_kind() {
                ExpressionValueKind::Scalar => 0,
                ExpressionValueKind::Truth => 1,
            });
            match binding.source() {
                RuleInputSource::Forcing(reference) => {
                    writer.write_u8(0);
                    writer.write_string(
                        CanonicalField::ArtifactInputSourceIdentity,
                        reference.id().as_str(),
                    )?;
                }
                RuleInputSource::InterpolationTable(reference) => {
                    writer.write_u8(1);
                    writer.write_string(
                        CanonicalField::ArtifactInputSourceIdentity,
                        reference.id().as_str(),
                    )?;
                }
                RuleInputSource::Projection(reference) => {
                    writer.write_u8(2);
                    writer.write_string(
                        CanonicalField::ArtifactInputSourceIdentity,
                        reference.id().as_str(),
                    )?;
                    writer.write_u8(match reference.value_kind() {
                        ProjectionValueKind::Extensive => 0,
                        ProjectionValueKind::Scalar => 1,
                        ProjectionValueKind::Truth => 2,
                    });
                }
            }
        }
        writer.write_count(CanonicalField::ArtifactUnits, self.units.len())?;
        for (substance, declaration) in &self.units {
            writer.write_string(CanonicalField::ArtifactUnitSubstance, substance.as_str())?;
            writer.write_string(
                CanonicalField::ArtifactUnitIdentity,
                declaration.unit().as_str(),
            )?;
            writer.write_scalar(
                CanonicalField::ArtifactUnitQuantum,
                declaration.quantum().value(),
            )?;
        }
        Ok(())
    }
}

/// An append-only in-memory content-addressed repository.
#[derive(Default)]
pub struct ModelArtifactArchive {
    artifacts: BTreeMap<ModelDigest, Arc<ModelArtifact>>,
}

impl ModelArtifactArchive {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retains an artifact under its derived digest and returns its content address.
    ///
    /// # Errors
    ///
    /// Returns [`ModelArtifactArchiveError::DigestCollision`] rather than guessing if a digest
    /// already identifies different canonical bytes.
    pub fn insert(
        &mut self,
        artifact: ModelArtifact,
    ) -> Result<ModelDigest, ModelArtifactArchiveError> {
        let digest = artifact.digest();
        if let Some(existing) = self.artifacts.get(&digest) {
            if existing.canonical_bytes() != artifact.canonical_bytes() {
                return Err(ModelArtifactArchiveError::DigestCollision { digest });
            }
            return Ok(digest);
        }
        self.artifacts.insert(digest, Arc::new(artifact));
        Ok(digest)
    }

    /// Retrieves any retained historical artifact without granting mutation authority.
    #[must_use]
    pub fn get(&self, digest: &ModelDigest) -> Option<Arc<ModelArtifact>> {
        self.artifacts.get(digest).cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

/// Reports a content-address collision in an artifact archive.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ModelArtifactArchiveError {
    /// Fires if unequal canonical artifacts produce the same digest.
    #[error("model digest collision at `{digest}`")]
    DigestCollision { digest: ModelDigest },
}

/// Backwards-compatible concise name for the in-memory archive.
pub type ModelArtifactStore = ModelArtifactArchive;
