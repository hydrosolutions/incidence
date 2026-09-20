//! canonical_encoding : CanonicalEncodingVersion × CanonicalValue → Bytes ⊎ CanonicalEncodingError   (pure, deterministic)

use crate::initial_stocks::InitialStocks;
use crate::numerical_semantics::NumericalSemanticsVersion;
use crate::projection::{
    AuthoritativeFactSelector, InitialProjectorState, ProjectionSource, ProjectionSpec,
    ProjectionSpecView, ProjectionValue, RecurrenceInputSource, RollingAggregate,
};
use crate::rule_reference::{ExpressionValueKind, ProjectionValueKind};
use crate::sparse_substance_vector::SparseSubstanceVector;
use crate::substance_registry::SubstanceRegistry;
use crate::temporal::{FixedStepCalendar, RunHorizon};
use crate::topology::{Topology, TopologyEndpoint};
use crate::versions::{CanonicalEncodingVersion, InterpreterVersion, RuleIrVersion};

const ENVELOPE: &[u8; 4] = b"INCD";
const VERSION_V1: u16 = 1;

/// A typed location within a canonical payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalField {
    RegistryMembers,
    RegistrySubstanceIdentity,
    SparseEntries,
    SparseSubstanceIdentity,
    SparseAmount,
    TopologyEndpoints,
    TopologyEndpointIdentity,
    TopologyConnections,
    ConnectionSourceIdentity,
    ConnectionTargetIdentity,
    InitialStockEntries,
    InitialStockCompartmentIdentity,
    RuleInputIdentity,
    RuleParameterIdentity,
    RuleForcingIdentity,
    RuleProjectionIdentity,
    RuleTableIdentity,
    RuleLiteral,
    PartitionBranches,
    PartitionCarrierSubstance,
    PartitionBranchIdentity,
    PartitionFraction,
    ProjectionIdentity,
    FactCompartmentIdentity,
    FactSubstanceIdentity,
    ProjectionReferenceIdentity,
    ProjectionCount,
    ProjectionInputIdentity,
    ProjectionParameterIdentity,
    ProjectorStateValues,
    ProjectorStateValue,
    ScalarProbe,
    ForcingSeriesIdentity,
    ForcingSeriesValues,
    ForcingSeriesValue,
    InterpolationTableIdentity,
    InterpolationTablePoints,
    InterpolationTableAbscissa,
    InterpolationTableOrdinate,
    ArtifactProjections,
    ArtifactProjectorStates,
    ArtifactForcings,
    ArtifactTables,
    ArtifactRules,
    ArtifactRuleCompartment,
    ArtifactRuleSubstance,
    ArtifactParameters,
    ArtifactParameterIdentity,
    ArtifactParameterValue,
    ArtifactTransferBindings,
    ArtifactTransferBindingCompartment,
    ArtifactTransferBindingSubstance,
    ArtifactTransferBindingBranch,
    ArtifactTransferBindingDestination,
    ArtifactInputBindings,
    ArtifactInputBindingCompartment,
    ArtifactInputBindingSubstance,
    ArtifactInputBindingIdentity,
    ArtifactInputSourceIdentity,
    ArtifactUnits,
    ArtifactUnitSubstance,
    ArtifactUnitIdentity,
    ArtifactUnitQuantum,
}

/// A checked failure to produce canonical bytes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CanonicalEncodingError {
    /// Fires when a sequence or string length cannot be represented by the V1 unsigned 64-bit field.
    #[error("canonical {field:?} length {length} does not fit unsigned 64-bit encoding")]
    LengthOutOfRange {
        field: CanonicalField,
        length: usize,
    },
    /// Fires when a binary64 field is NaN or infinite.
    #[error("canonical {field:?} rejects non-finite binary64 bits {bits:#018x}")]
    NonFiniteScalar { field: CanonicalField, bits: u64 },
}

/// The payload sink available to canonical implementations inside `incidence-core`.
///
/// Its typed mutators are crate-private so callers cannot inject unframed bytes.
pub struct CanonicalPayloadWriter {
    bytes: Vec<u8>,
}

impl CanonicalPayloadWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_count(
        &mut self,
        field: CanonicalField,
        length: usize,
    ) -> Result<(), CanonicalEncodingError> {
        let count = u64::try_from(length)
            .map_err(|_| CanonicalEncodingError::LengthOutOfRange { field, length })?;
        self.write_u64(count);
        Ok(())
    }

    pub(crate) fn write_string(
        &mut self,
        field: CanonicalField,
        value: &str,
    ) -> Result<(), CanonicalEncodingError> {
        let length =
            u64::try_from(value.len()).map_err(|_| CanonicalEncodingError::LengthOutOfRange {
                field,
                length: value.len(),
            })?;
        self.write_u64(length);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    pub(crate) fn write_scalar(
        &mut self,
        field: CanonicalField,
        value: f64,
    ) -> Result<(), CanonicalEncodingError> {
        if !value.is_finite() {
            return Err(CanonicalEncodingError::NonFiniteScalar {
                field,
                bits: value.to_bits(),
            });
        }
        self.write_u64(if value == 0.0 { 0 } else { value.to_bits() });
        Ok(())
    }
}

/// A value with a canonical root tag and deterministic V1 payload.
pub trait CanonicalEncode {
    /// Returns the fixed V1 root type tag.
    fn root_tag(&self) -> u16;

    /// Writes the value's framed, typed V1 payload.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalEncodingError`] when a payload field cannot be represented.
    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError>;
}

impl CanonicalEncodingVersion {
    /// Encodes one root value using the selected canonical envelope and payload grammar.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalEncodingError`] when any payload field cannot be represented.
    pub fn encode<T: CanonicalEncode>(self, value: &T) -> Result<Vec<u8>, CanonicalEncodingError> {
        let mut writer = CanonicalPayloadWriter::new();
        writer.bytes.extend_from_slice(ENVELOPE);
        match self {
            Self::V1 => writer.write_u16(VERSION_V1),
        }
        writer.write_u16(value.root_tag());
        value.encode_payload(&mut writer)?;
        Ok(writer.bytes)
    }
}

macro_rules! version_encoding {
    ($type:ty, $tag:expr) => {
        impl CanonicalEncode for $type {
            fn root_tag(&self) -> u16 {
                $tag
            }

            fn encode_payload(
                &self,
                writer: &mut CanonicalPayloadWriter,
            ) -> Result<(), CanonicalEncodingError> {
                match self {
                    Self::V1 => writer.write_u16(VERSION_V1),
                }
                Ok(())
            }
        }
    };
}

version_encoding!(RuleIrVersion, 0x0001);
version_encoding!(InterpreterVersion, 0x0002);
version_encoding!(CanonicalEncodingVersion, 0x0003);
impl CanonicalEncode for NumericalSemanticsVersion {
    fn root_tag(&self) -> u16 {
        0x0004
    }
    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        writer.write_u16(match self {
            Self::V1 => VERSION_V1,
            Self::V2 => 2,
        });
        Ok(())
    }
}

fn encode_registry_payload(
    registry: &SubstanceRegistry,
    writer: &mut CanonicalPayloadWriter,
) -> Result<(), CanonicalEncodingError> {
    writer.write_count(CanonicalField::RegistryMembers, registry.iter().len())?;
    for substance in registry.iter() {
        writer.write_string(
            CanonicalField::RegistrySubstanceIdentity,
            substance.as_str(),
        )?;
    }
    Ok(())
}

impl CanonicalEncode for SubstanceRegistry {
    fn root_tag(&self) -> u16 {
        0x0010
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        encode_registry_payload(self, writer)
    }
}

fn encode_sparse_payload(
    vector: &SparseSubstanceVector,
    writer: &mut CanonicalPayloadWriter,
) -> Result<(), CanonicalEncodingError> {
    encode_registry_payload(vector.registry(), writer)?;
    let entries = vector.iter().collect::<Vec<_>>();
    writer.write_count(CanonicalField::SparseEntries, entries.len())?;
    for (substance, amount) in entries {
        writer.write_string(CanonicalField::SparseSubstanceIdentity, substance.as_str())?;
        writer.write_scalar(CanonicalField::SparseAmount, amount.value())?;
    }
    Ok(())
}

impl CanonicalEncode for SparseSubstanceVector {
    fn root_tag(&self) -> u16 {
        0x0011
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        encode_sparse_payload(self, writer)
    }
}

fn encode_topology_payload(
    topology: &Topology,
    writer: &mut CanonicalPayloadWriter,
) -> Result<(), CanonicalEncodingError> {
    writer.write_count(
        CanonicalField::TopologyEndpoints,
        topology.endpoints().len(),
    )?;
    for endpoint in topology.endpoints() {
        writer.write_u8(match endpoint {
            TopologyEndpoint::Finite(_) => 0x00,
            TopologyEndpoint::Boundary(_) => 0x01,
        });
        writer.write_string(
            CanonicalField::TopologyEndpointIdentity,
            endpoint.id().as_str(),
        )?;
    }
    writer.write_count(
        CanonicalField::TopologyConnections,
        topology.connections().len(),
    )?;
    for connection in topology.connections() {
        writer.write_string(
            CanonicalField::ConnectionSourceIdentity,
            connection.source().as_str(),
        )?;
        writer.write_string(
            CanonicalField::ConnectionTargetIdentity,
            connection.target().as_str(),
        )?;
    }
    Ok(())
}

impl CanonicalEncode for Topology {
    fn root_tag(&self) -> u16 {
        0x0012
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        encode_topology_payload(self, writer)
    }
}

impl CanonicalEncode for InitialStocks {
    fn root_tag(&self) -> u16 {
        0x0013
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        encode_topology_payload(self.topology(), writer)?;
        encode_registry_payload(self.registry(), writer)?;
        let entries = self.iter().collect::<Vec<_>>();
        writer.write_count(CanonicalField::InitialStockEntries, entries.len())?;
        for (compartment, vector) in entries {
            writer.write_string(
                CanonicalField::InitialStockCompartmentIdentity,
                compartment.as_str(),
            )?;
            encode_sparse_payload(vector, writer)?;
        }
        Ok(())
    }
}

impl CanonicalEncode for FixedStepCalendar {
    fn root_tag(&self) -> u16 {
        0x0014
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        writer.write_i64(self.origin().instant().unix_seconds());
        writer.write_u64(self.timestep_duration().seconds());
        Ok(())
    }
}

impl CanonicalEncode for RunHorizon {
    fn root_tag(&self) -> u16 {
        0x0015
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        writer.write_u64(self.first().value());
        writer.write_u64(self.last().value());
        Ok(())
    }
}

fn projection_kind_tag(kind: ProjectionValueKind) -> u8 {
    match kind {
        ProjectionValueKind::Extensive => 0,
        ProjectionValueKind::Scalar => 1,
        ProjectionValueKind::Truth => 2,
    }
}

fn expression_kind_tag(kind: ExpressionValueKind) -> u8 {
    match kind {
        ExpressionValueKind::Scalar => 0,
        ExpressionValueKind::Truth => 1,
    }
}

fn encode_selector(
    selector: &AuthoritativeFactSelector,
    writer: &mut CanonicalPayloadWriter,
) -> Result<(), CanonicalEncodingError> {
    writer.write_u8(match selector {
        AuthoritativeFactSelector::IncomingTransferAmount { .. } => 0,
        AuthoritativeFactSelector::OutgoingTransferAmount { .. } => 1,
    });
    writer.write_string(
        CanonicalField::FactCompartmentIdentity,
        selector.compartment().as_str(),
    )?;
    writer.write_string(
        CanonicalField::FactSubstanceIdentity,
        selector.substance().as_str(),
    )
}

fn encode_projection_source(
    source: &ProjectionSource,
    writer: &mut CanonicalPayloadWriter,
) -> Result<(), CanonicalEncodingError> {
    match source {
        ProjectionSource::AuthoritativeFact(selector) => {
            writer.write_u8(0);
            encode_selector(selector, writer)
        }
        ProjectionSource::Projection(reference) => {
            writer.write_u8(1);
            writer.write_u8(projection_kind_tag(reference.value_kind()));
            writer.write_string(
                CanonicalField::ProjectionReferenceIdentity,
                reference.id().as_str(),
            )
        }
    }
}

impl CanonicalEncode for ProjectionSpec {
    fn root_tag(&self) -> u16 {
        0x0018
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        writer.write_u16(match self.rule_ir_version() {
            RuleIrVersion::V1 => 1,
        });
        writer.write_u16(match self.numerical_semantics_version() {
            NumericalSemanticsVersion::V1 => 1,
            NumericalSemanticsVersion::V2 => 2,
        });
        writer.write_string(CanonicalField::ProjectionIdentity, self.id().as_str())?;
        writer.write_u8(projection_kind_tag(self.value_kind()));
        match self.view() {
            ProjectionSpecView::BoundedLag(spec) => {
                writer.write_u8(0);
                encode_projection_source(spec.source(), writer)?;
                writer.write_count(CanonicalField::ProjectionCount, spec.steps())?;
            }
            ProjectionSpecView::OrderedRollingAggregate(spec) => {
                writer.write_u8(1);
                encode_projection_source(spec.source(), writer)?;
                writer.write_count(CanonicalField::ProjectionCount, spec.window())?;
                writer.write_u8(match spec.aggregate() {
                    RollingAggregate::SumOldestToNewest => 0,
                });
            }
            ProjectionSpecView::FiniteRecurrence(spec) => {
                writer.write_u8(2);
                writer.write_count(CanonicalField::ProjectionCount, spec.state_kinds().len())?;
                for kind in spec.state_kinds() {
                    writer.write_u8(projection_kind_tag(*kind));
                }
                writer.write_count(CanonicalField::ProjectionCount, spec.inputs().len())?;
                for binding in spec.inputs() {
                    writer.write_u8(expression_kind_tag(binding.reference().value_kind()));
                    writer.write_string(
                        CanonicalField::ProjectionInputIdentity,
                        binding.reference().id().as_str(),
                    )?;
                    match binding.input_source() {
                        RecurrenceInputSource::AuthoritativeFact(selector) => {
                            writer.write_u8(0);
                            encode_selector(selector, writer)?;
                        }
                        RecurrenceInputSource::PreviousState { index, value_kind } => {
                            writer.write_u8(1);
                            writer.write_count(CanonicalField::ProjectionCount, *index)?;
                            writer.write_u8(projection_kind_tag(*value_kind));
                        }
                    }
                }
                writer.write_count(CanonicalField::ProjectionCount, spec.parameters().len())?;
                for parameter in spec.parameters() {
                    writer.write_u8(expression_kind_tag(parameter.value_kind()));
                    writer.write_string(
                        CanonicalField::ProjectionParameterIdentity,
                        parameter.id().as_str(),
                    )?;
                }
                writer.write_count(CanonicalField::ProjectionCount, spec.updates().len())?;
                for update in spec.updates() {
                    update.encode_payload_unframed(writer)?;
                }
                writer.write_count(CanonicalField::ProjectionCount, spec.output_index())?;
            }
        }
        Ok(())
    }
}

impl CanonicalEncode for InitialProjectorState {
    fn root_tag(&self) -> u16 {
        0x0019
    }

    fn encode_payload(
        &self,
        writer: &mut CanonicalPayloadWriter,
    ) -> Result<(), CanonicalEncodingError> {
        writer.write_string(
            CanonicalField::ProjectionIdentity,
            self.projection().as_str(),
        )?;
        writer.write_count(CanonicalField::ProjectorStateValues, self.values().len())?;
        for value in self.values() {
            writer.write_u8(projection_kind_tag(value.value_kind()));
            match value {
                ProjectionValue::Extensive(number) | ProjectionValue::Scalar(number) => {
                    writer.write_scalar(CanonicalField::ProjectorStateValue, number.value())?;
                }
                ProjectionValue::Truth(value) => writer.write_u8(u8::from(*value)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::canonical_encoding::{
        CanonicalEncodingError, CanonicalField, CanonicalPayloadWriter,
    };

    #[test]
    fn scalar_encoding_canonicalizes_zero_and_rejects_non_finite_values() {
        for zero in [-0.0, 0.0] {
            let mut writer = CanonicalPayloadWriter::new();
            assert_eq!(
                writer.write_scalar(CanonicalField::ScalarProbe, zero),
                Ok(())
            );
            assert_eq!(writer.bytes, 0_u64.to_be_bytes());
        }

        for input in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut writer = CanonicalPayloadWriter::new();
            assert_eq!(
                writer.write_scalar(CanonicalField::ScalarProbe, input),
                Err(CanonicalEncodingError::NonFiniteScalar {
                    field: CanonicalField::ScalarProbe,
                    bits: input.to_bits(),
                })
            );
            assert!(writer.bytes.is_empty());
        }
    }
}
