//! python_binding : PythonModelData × RunId ⇀ (CanonicalLogBytes × LogDigest × PresenceQueries)
//!
//! This crate is only a containment and transport boundary. Domain validation, execution, and
//! presence semantics remain in `incidence-core`.

use incidence_core::dense_projection::{
    DenseTransferCountProjection, DenseTransferProjection, ValidatedTransferLog,
};
use incidence_core::execution::execute_model;
use incidence_core::identity::{CompartmentId, SubstanceId};
use incidence_core::ledger::{AuthoritativeLog, QuantumCount, RunId, replay_with_artifact};
use incidence_core::model_artifact::{ModelArtifact, RuleParameterSubstitution};
use incidence_core::model_document::ModelDocument;
use incidence_core::non_negative_amount::NonNegativeAmount;
use incidence_core::numerical_semantics::{NumericalSemanticsVersion, ScalarComparison};
use incidence_core::presence::ValueState;
use incidence_core::projection::AuthoritativeFactSelector;
use incidence_core::rule_expression::RuleExpr;
use incidence_core::rule_reference::{
    ExpressionValueKind, ForcingId, ForcingRef, InputId, InputRef, InterpolatedTableRef,
    ParameterId, ParameterRef, ProjectionId, ProjectionRef, ProjectionValueKind, TableId,
};
use incidence_core::temporal::TimestepIndex;
use incidence_core::versions::RuleIrVersion;
use pyo3::exceptions::{PyIndexError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::any::Any;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

const MAX_DOCUMENT_NESTING: usize = 256;
const MAX_SERIES_LENGTH: u64 = 10_000_000;

/// An immutable model validated and owned by the Rust core.
#[pyclass(frozen, module = "incidence._incidence")]
struct CompiledModel {
    artifact: Arc<ModelArtifact>,
}

/// A completed authoritative run bound to the exact artifact that produced it.
#[pyclass(frozen, module = "incidence._incidence")]
struct CompletedRun {
    artifact: Arc<ModelArtifact>,
    log: Arc<AuthoritativeLog>,
    validated: OnceLock<Result<ValidatedTransferLog, String>>,
}

impl CompletedRun {
    fn new(artifact: Arc<ModelArtifact>, log: AuthoritativeLog) -> Self {
        Self {
            artifact,
            log: Arc::new(log),
            validated: OnceLock::new(),
        }
    }

    fn validated(&self) -> PyResult<&ValidatedTransferLog> {
        match self.validated.get_or_init(|| {
            ValidatedTransferLog::new(self.log.clone(), self.artifact.clone())
                .map_err(|error| error.to_string())
        }) {
            Ok(validated) => Ok(validated),
            Err(error) => Err(PyValueError::new_err(error.clone())),
        }
    }
}

/// A sequence of result values that retains the presence state for every position.
#[pyclass(frozen, module = "incidence._incidence", name = "_PresenceValues")]
struct PresenceValues {
    values: Vec<Option<f64>>,
    presence: Vec<&'static str>,
}

#[pymethods]
impl PresenceValues {
    /// Per-position states corresponding exactly to this value sequence.
    #[getter]
    fn presence(&self) -> Vec<&'static str> {
        self.presence.clone()
    }

    fn __len__(&self) -> usize {
        self.values.len()
    }

    fn __getitem__(&self, index: isize) -> PyResult<Option<f64>> {
        let length = isize::try_from(self.values.len())
            .map_err(|_| PyIndexError::new_err("presence value sequence is too large to index"))?;
        let resolved = if index < 0 {
            length.checked_add(index)
        } else {
            Some(index)
        };
        let value = resolved
            .filter(|resolved| *resolved >= 0)
            .and_then(|resolved| usize::try_from(resolved).ok())
            .and_then(|resolved| self.values.get(resolved))
            .ok_or_else(|| PyIndexError::new_err("presence value index out of range"))?;
        Ok(*value)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Vec<Option<f64>>>()
            .is_ok_and(|values| values == self.values)
    }
}

/// A time-indexed result whose values can never be separated from their presence states.
#[pyclass(frozen, module = "incidence._incidence")]
struct PresenceSeries {
    timesteps: Vec<u64>,
    values: Vec<Option<f64>>,
    presence: Vec<&'static str>,
}

#[pymethods]
impl PresenceSeries {
    /// The requested timestep coordinates, in ascending order.
    #[getter]
    fn timesteps(&self) -> Vec<u64> {
        self.timesteps.clone()
    }

    /// Values and their corresponding presence states at the requested coordinates.
    #[getter]
    fn values(&self) -> PresenceValues {
        PresenceValues {
            values: self.values.clone(),
            presence: self.presence.clone(),
        }
    }

    /// Per-position states: `present`, `absent`, or `not_modelled`.
    #[getter]
    fn presence(&self) -> Vec<&'static str> {
        self.presence.clone()
    }

    fn __len__(&self) -> usize {
        self.timesteps.len()
    }
}

/// A sequence of authoritative counts that retains the presence state for every position.
#[pyclass(frozen, module = "incidence._incidence", name = "_PresenceCountValues")]
struct PresenceCountValues {
    values: Vec<Option<u64>>,
    presence: Vec<&'static str>,
}

#[pymethods]
impl PresenceCountValues {
    /// Per-position states corresponding exactly to this count sequence.
    #[getter]
    fn presence(&self) -> Vec<&'static str> {
        self.presence.clone()
    }

    fn __len__(&self) -> usize {
        self.values.len()
    }

    fn __getitem__(&self, index: isize) -> PyResult<Option<u64>> {
        let length = isize::try_from(self.values.len())
            .map_err(|_| PyIndexError::new_err("presence count sequence is too large to index"))?;
        let resolved = if index < 0 {
            length.checked_add(index)
        } else {
            Some(index)
        };
        let value = resolved
            .filter(|resolved| *resolved >= 0)
            .and_then(|resolved| usize::try_from(resolved).ok())
            .and_then(|resolved| self.values.get(resolved))
            .ok_or_else(|| PyIndexError::new_err("presence count index out of range"))?;
        Ok(*value)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Vec<Option<u64>>>()
            .is_ok_and(|values| values == self.values)
    }
}

/// A time-indexed result whose authoritative counts retain their presence states.
#[pyclass(frozen, module = "incidence._incidence")]
struct PresenceCountSeries {
    timesteps: Vec<u64>,
    values: Vec<Option<u64>>,
    presence: Vec<&'static str>,
}

#[pymethods]
impl PresenceCountSeries {
    /// The requested timestep coordinates, in ascending order.
    #[getter]
    fn timesteps(&self) -> Vec<u64> {
        self.timesteps.clone()
    }

    /// Counts and their corresponding presence states at the requested coordinates.
    #[getter]
    fn values(&self) -> PresenceCountValues {
        PresenceCountValues {
            values: self.values.clone(),
            presence: self.presence.clone(),
        }
    }

    /// Per-position states: `present`, `absent`, or `not_modelled`.
    #[getter]
    fn presence(&self) -> Vec<&'static str> {
        self.presence.clone()
    }

    fn __len__(&self) -> usize {
        self.timesteps.len()
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn contain<T>(operation: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(payload) => Err(PyRuntimeError::new_err(format!(
            "incidence boundary contained panic: {}",
            panic_message(payload)
        ))),
    }
}

fn parse_run_id(value: &Bound<'_, PyAny>) -> PyResult<RunId> {
    if let Ok(bytes) = value.cast::<PyBytes>() {
        let bytes: [u8; 16] = bytes
            .as_bytes()
            .try_into()
            .map_err(|_| PyValueError::new_err("run id bytes must contain exactly 16 bytes"))?;
        return Ok(RunId::from_bytes(bytes));
    }
    if let Ok(text) = value.extract::<&str>() {
        let compact = text
            .chars()
            .filter(|character| *character != '-')
            .collect::<String>();
        if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PyValueError::new_err(
                "run id text must contain exactly 32 hexadecimal digits, with optional hyphens",
            ));
        }
        let mut bytes = [0_u8; 16];
        for (index, destination) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *destination =
                u8::from_str_radix(&compact[offset..offset + 2], 16).map_err(|error| {
                    PyValueError::new_err(format!("invalid hexadecimal run id: {error}"))
                })?;
        }
        return Ok(RunId::from_bytes(bytes));
    }
    Err(PyValueError::new_err(
        "run id must be 16 bytes or hexadecimal text",
    ))
}

fn series_selector(
    direction: &str,
    compartment: CompartmentId,
    substance: SubstanceId,
) -> PyResult<AuthoritativeFactSelector> {
    match direction {
        "incoming" => Ok(AuthoritativeFactSelector::IncomingTransferAmount {
            compartment,
            substance,
        }),
        "outgoing" => Ok(AuthoritativeFactSelector::OutgoingTransferAmount {
            compartment,
            substance,
        }),
        other => Err(PyValueError::new_err(format!(
            "transfer direction must be `incoming` or `outgoing`, got `{other}`"
        ))),
    }
}

fn append_state(
    state: ValueState<NonNegativeAmount>,
    values: &mut Vec<Option<f64>>,
    presence: &mut Vec<&'static str>,
) {
    match state {
        ValueState::Present(amount) => {
            values.push(Some(amount.value()));
            presence.push("present");
        }
        ValueState::Absent => {
            values.push(None);
            presence.push("absent");
        }
        ValueState::NotModelled => {
            values.push(None);
            presence.push("not_modelled");
        }
    }
}

fn append_count_state(
    state: ValueState<QuantumCount>,
    values: &mut Vec<Option<u64>>,
    presence: &mut Vec<&'static str>,
) {
    match state {
        ValueState::Present(count) => {
            values.push(Some(count.value()));
            presence.push("present");
        }
        ValueState::Absent => {
            values.push(None);
            presence.push("absent");
        }
        ValueState::NotModelled => {
            values.push(None);
            presence.push("not_modelled");
        }
    }
}

fn parse_substitutions(
    substitutions: Option<&Bound<'_, PyAny>>,
) -> PyResult<Vec<RuleParameterSubstitution>> {
    let Some(substitutions) = substitutions.filter(|value| !value.is_none()) else {
        return Ok(Vec::new());
    };
    let mut parsed = Vec::new();
    for (index, item) in substitutions.try_iter()?.enumerate() {
        let item = item?;
        let target = item.repr()?.to_string_lossy().into_owned();
        let mapping = item.cast::<PyDict>().map_err(|_| {
            PyValueError::new_err(format!(
                "substitution target at index {index} `{target}` is not substitutable; expected a mapping with exactly compartment, substance, parameter, and value"
            ))
        })?;
        let expected = ["compartment", "substance", "parameter", "value"];
        let keys = mapping
            .keys()
            .iter()
            .map(|key| key.extract::<String>())
            .collect::<PyResult<HashSet<_>>>()?;
        if keys.len() != expected.len() || expected.iter().any(|key| !keys.contains(*key)) {
            return Err(PyValueError::new_err(format!(
                "substitution target at index {index} `{target}` is not substitutable; expected exactly compartment, substance, parameter, and value"
            )));
        }
        let field = |name: &str| {
            mapping.get_item(name)?.ok_or_else(|| {
                PyValueError::new_err(format!(
                    "substitution target at index {index} `{target}` is missing `{name}`"
                ))
            })
        };
        let compartment_text = field("compartment")?.extract::<String>()?;
        let substance_text = field("substance")?.extract::<String>()?;
        let parameter_text = field("parameter")?.extract::<String>()?;
        let value = field("value")?.extract::<f64>()?;
        let compartment = CompartmentId::parse(&compartment_text).map_err(|error| {
            PyValueError::new_err(format!(
                "substitution target compartment `{compartment_text}` is not substitutable: {error}"
            ))
        })?;
        let substance = SubstanceId::parse(&substance_text).map_err(|error| {
            PyValueError::new_err(format!(
                "substitution target substance `{substance_text}` is not substitutable: {error}"
            ))
        })?;
        let parameter = ParameterId::parse(&parameter_text).map_err(|error| {
            PyValueError::new_err(format!(
                "substitution target parameter `{parameter_text}` is not substitutable: {error}"
            ))
        })?;
        parsed.push(RuleParameterSubstitution::new(
            compartment,
            substance,
            parameter,
            value,
        ));
    }
    Ok(parsed)
}

fn quantum_value(artifact: &ModelArtifact, substance: &str) -> PyResult<f64> {
    let substance =
        SubstanceId::parse(substance).map_err(|error| PyValueError::new_err(error.to_string()))?;
    artifact
        .quantum(&substance)
        .map(|quantum| quantum.value())
        .ok_or_else(|| PyValueError::new_err(format!("substance `{substance}` is not modelled")))
}

#[pymethods]
impl CompiledModel {
    /// The content digest of the unchanged held artifact.
    #[getter]
    fn model_digest(&self) -> String {
        self.artifact.digest().to_hex()
    }

    /// The declared arithmetic quantum for one modelled substance.
    fn quantum(&self, substance: &str) -> PyResult<f64> {
        quantum_value(&self.artifact, substance)
    }

    /// Execute the held model with optional declared-rule-parameter replacements.
    #[pyo3(signature = (run_id, *, substitutions = None))]
    fn run(
        &self,
        py: Python<'_>,
        run_id: &Bound<'_, PyAny>,
        substitutions: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<CompletedRun> {
        contain(|| {
            let run_id = parse_run_id(run_id)?;
            let substitutions = parse_substitutions(substitutions)?;
            let held_artifact = self.artifact.clone();
            py.detach(move || {
                let artifact = if substitutions.is_empty() {
                    held_artifact
                } else {
                    Arc::new(
                        held_artifact
                            .with_rule_parameter_substitutions(substitutions)
                            .map_err(|error| PyValueError::new_err(error.to_string()))?,
                    )
                };
                let log = execute_model(&artifact, run_id)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?;
                Ok(CompletedRun::new(artifact, log))
            })
        })
    }
}

#[pymethods]
impl CompletedRun {
    /// Return the canonical authoritative-log bytes and their lowercase SHA-256 digest.
    fn authoritative_log<'py>(&self, py: Python<'py>) -> PyResult<(Bound<'py, PyBytes>, String)> {
        contain(|| {
            self.validated()?;
            let bytes = self.log.canonical_bytes();
            let digest = self.log.digest().to_hex();
            Ok((PyBytes::new(py, &bytes), digest))
        })
    }

    /// The digest of the exact base or parameter-substituted artifact used by this run.
    #[getter]
    fn model_digest(&self) -> String {
        self.artifact.digest().to_hex()
    }

    /// The declared arithmetic quantum of the exact artifact used by this run.
    fn quantum(&self, substance: &str) -> PyResult<f64> {
        quantum_value(&self.artifact, substance)
    }

    /// Replay this run against another run's artifact, rejecting any digest mismatch.
    fn replay_against(&self, other: &CompletedRun) -> PyResult<()> {
        contain(|| {
            replay_with_artifact(&self.log, &other.artifact)
                .map(|_| ())
                .map_err(|error| PyValueError::new_err(error.to_string()))
        })
    }

    /// Read one dense transfer amount series with a presence state for every requested timestep.
    #[pyo3(signature = (compartment, substance, *, direction = "outgoing", first = None, last = None))]
    fn transfer_series(
        &self,
        compartment: &str,
        substance: &str,
        direction: &str,
        first: Option<u64>,
        last: Option<u64>,
    ) -> PyResult<PresenceSeries> {
        contain(|| {
            let compartment = CompartmentId::parse(compartment)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
            let substance = SubstanceId::parse(substance)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
            let selector = series_selector(direction, compartment, substance)?;
            let projection =
                DenseTransferProjection::from_validated_log(self.validated()?, selector)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?;

            let first = first.unwrap_or_else(|| projection.horizon().first().value());
            let last = last.unwrap_or_else(|| projection.horizon().last().value());
            if last < first {
                return Err(PyValueError::new_err(format!(
                    "series range is reversed: first timestep {first}, last timestep {last}"
                )));
            }
            let length = last
                .checked_sub(first)
                .and_then(|span| span.checked_add(1))
                .ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "series range from {first} through {last} cannot be represented"
                    ))
                })?;
            if length > MAX_SERIES_LENGTH {
                return Err(PyValueError::new_err(format!(
                    "series range from {first} through {last} exceeds maximum length {MAX_SERIES_LENGTH}"
                )));
            }
            let capacity = usize::try_from(length).map_err(|_| {
                PyValueError::new_err(format!(
                    "series range from {first} through {last} cannot be materialised"
                ))
            })?;
            let mut timesteps = Vec::new();
            let mut values = Vec::new();
            let mut presence = Vec::new();
            timesteps.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            values.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            presence.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            for timestep in first..=last {
                timesteps.push(timestep);
                append_state(
                    projection.value_at(TimestepIndex::new(timestep)),
                    &mut values,
                    &mut presence,
                );
            }
            Ok(PresenceSeries {
                timesteps,
                values,
                presence,
            })
        })
    }

    /// Read one dense authoritative transfer count series with presence at every requested timestep.
    #[pyo3(signature = (compartment, substance, *, direction = "outgoing", first = None, last = None))]
    fn transfer_count_series(
        &self,
        compartment: &str,
        substance: &str,
        direction: &str,
        first: Option<u64>,
        last: Option<u64>,
    ) -> PyResult<PresenceCountSeries> {
        contain(|| {
            let compartment = CompartmentId::parse(compartment)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
            let substance = SubstanceId::parse(substance)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
            let selector = series_selector(direction, compartment, substance)?;
            let projection =
                DenseTransferCountProjection::from_validated_log(self.validated()?, selector)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?;

            let first = first.unwrap_or_else(|| projection.horizon().first().value());
            let last = last.unwrap_or_else(|| projection.horizon().last().value());
            if last < first {
                return Err(PyValueError::new_err(format!(
                    "series range is reversed: first timestep {first}, last timestep {last}"
                )));
            }
            let length = last
                .checked_sub(first)
                .and_then(|span| span.checked_add(1))
                .ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "series range from {first} through {last} cannot be represented"
                    ))
                })?;
            if length > MAX_SERIES_LENGTH {
                return Err(PyValueError::new_err(format!(
                    "series range from {first} through {last} exceeds maximum length {MAX_SERIES_LENGTH}"
                )));
            }
            let capacity = usize::try_from(length).map_err(|_| {
                PyValueError::new_err(format!(
                    "series range from {first} through {last} cannot be materialised"
                ))
            })?;
            let mut timesteps = Vec::new();
            let mut values = Vec::new();
            let mut presence = Vec::new();
            timesteps.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            values.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            presence.try_reserve_exact(capacity).map_err(|error| {
                PyValueError::new_err(format!("cannot allocate requested series: {error}"))
            })?;
            for timestep in first..=last {
                timesteps.push(timestep);
                append_count_state(
                    projection.value_at(TimestepIndex::new(timestep)),
                    &mut values,
                    &mut presence,
                );
            }
            Ok(PresenceCountSeries {
                timesteps,
                values,
                presence,
            })
        })
    }
}

enum TraversalFrame<'py> {
    Enter {
        value: Bound<'py, PyAny>,
        depth: usize,
    },
    Exit(usize),
}

fn check_document_nesting(document: &Bound<'_, PyAny>) -> PyResult<()> {
    let mut frames = vec![TraversalFrame::Enter {
        value: document.clone(),
        depth: 0,
    }];
    let mut active_containers = HashSet::new();

    while let Some(frame) = frames.pop() {
        let TraversalFrame::Enter { value, depth } = frame else {
            if let TraversalFrame::Exit(identity) = frame {
                active_containers.remove(&identity);
            }
            continue;
        };
        if depth > MAX_DOCUMENT_NESTING {
            return Err(PyValueError::new_err(format!(
                "invalid model document encoding: document nesting depth exceeds maximum {MAX_DOCUMENT_NESTING}"
            )));
        }

        let children = if let Ok(mapping) = value.cast::<PyDict>() {
            mapping.iter().map(|(_, child)| child).collect::<Vec<_>>()
        } else if let Ok(sequence) = value.cast::<PyList>() {
            sequence.iter().collect::<Vec<_>>()
        } else if let Ok(sequence) = value.cast::<PyTuple>() {
            sequence.iter().collect::<Vec<_>>()
        } else {
            continue;
        };

        let identity = value.as_ptr() as usize;
        if !active_containers.insert(identity) {
            return Err(PyValueError::new_err(
                "invalid model document encoding: cyclic Python container",
            ));
        }
        frames.push(TraversalFrame::Exit(identity));
        frames.extend(children.into_iter().map(|child| TraversalFrame::Enter {
            value: child,
            depth: depth + 1,
        }));
    }
    Ok(())
}

fn compile_document(document: &Bound<'_, PyAny>) -> PyResult<CompiledModel> {
    check_document_nesting(document)?;
    let decoded: ModelDocument = pythonize::depythonize(document).map_err(|error| {
        PyValueError::new_err(format!("invalid model document encoding: {error}"))
    })?;
    let artifact = decoded
        .into_artifact()
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(CompiledModel {
        artifact: Arc::new(artifact),
    })
}

/// Decode and validate a complete plain-data model document.
///
/// Every panic raised while crossing into the Rust decoder or core is converted to an ordinary
/// Python `RuntimeError`. It can never become PyO3's `PanicException`.
#[pyfunction]
fn compile_model(document: &Bound<'_, PyAny>) -> PyResult<CompiledModel> {
    contain(|| compile_document(document))
}

fn expression(document: &Bound<'_, PyAny>) -> PyResult<RuleExpr> {
    check_document_nesting(document)?;
    pythonize::depythonize(document)
        .map_err(|error| PyValueError::new_err(format!("invalid rule expression: {error}")))
}

fn authored_expression<'py>(py: Python<'py>, expression: &RuleExpr) -> PyResult<Bound<'py, PyAny>> {
    pythonize::pythonize(py, expression)
        .map_err(|error| PyValueError::new_err(format!("cannot encode rule expression: {error}")))
}

fn scalar_kind(value_kind: &str) -> PyResult<ExpressionValueKind> {
    match value_kind {
        "scalar" => Ok(ExpressionValueKind::Scalar),
        "truth" => Ok(ExpressionValueKind::Truth),
        other => Err(PyValueError::new_err(format!(
            "expression value kind must be `scalar` or `truth`, got `{other}`"
        ))),
    }
}

fn projection_kind(value_kind: &str) -> PyResult<ProjectionValueKind> {
    match value_kind {
        "extensive" => Ok(ProjectionValueKind::Extensive),
        "scalar" => Ok(ProjectionValueKind::Scalar),
        "truth" => Ok(ProjectionValueKind::Truth),
        other => Err(PyValueError::new_err(format!(
            "projection value kind must be `extensive`, `scalar`, or `truth`, got `{other}`"
        ))),
    }
}

fn rule_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// Author a finite scalar literal as a serialisable rule-expression document.
#[pyfunction]
fn literal<'py>(py: Python<'py>, value: f64) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let value = RuleExpr::literal(RuleIrVersion::V1, NumericalSemanticsVersion::V1, value)
            .map_err(rule_error)?;
        authored_expression(py, &value)
    })
}

/// Author a declared rule-parameter reference.
#[pyfunction(signature = (id, value_kind = "scalar"))]
fn param<'py>(py: Python<'py>, id: &str, value_kind: &str) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let id = ParameterId::parse(id).map_err(rule_error)?;
        let value = RuleExpr::parameter(
            RuleIrVersion::V1,
            NumericalSemanticsVersion::V1,
            ParameterRef::new(id, scalar_kind(value_kind)?),
        );
        authored_expression(py, &value)
    })
}

/// Author a generic bound-input reference.
#[pyfunction(signature = (id, value_kind = "scalar"))]
fn input<'py>(py: Python<'py>, id: &str, value_kind: &str) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let id = InputId::parse(id).map_err(rule_error)?;
        let value = RuleExpr::input(
            RuleIrVersion::V1,
            NumericalSemanticsVersion::V1,
            InputRef::new(id, scalar_kind(value_kind)?),
        );
        authored_expression(py, &value)
    })
}

/// Author a forcing-series reference.
#[pyfunction]
fn forcing<'py>(py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let id = ForcingId::parse(id).map_err(rule_error)?;
        let value = RuleExpr::forcing(
            RuleIrVersion::V1,
            NumericalSemanticsVersion::V1,
            ForcingRef::new(id),
        );
        authored_expression(py, &value)
    })
}

/// Author a typed projection reference.
#[pyfunction(signature = (id, value_kind = "extensive"))]
fn projection<'py>(py: Python<'py>, id: &str, value_kind: &str) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let id = ProjectionId::parse(id).map_err(rule_error)?;
        let value = RuleExpr::projection(
            RuleIrVersion::V1,
            NumericalSemanticsVersion::V1,
            ProjectionRef::new(id, projection_kind(value_kind)?),
        );
        authored_expression(py, &value)
    })
}

fn binary<'py>(
    py: Python<'py>,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
    constructor: fn(
        RuleExpr,
        RuleExpr,
    ) -> Result<RuleExpr, incidence_core::rule_expression::RuleExprError>,
) -> PyResult<Bound<'py, PyAny>> {
    let value = constructor(expression(lhs)?, expression(rhs)?).map_err(rule_error)?;
    authored_expression(py, &value)
}

/// Author an ordered addition.
#[pyfunction]
fn add<'py>(
    py: Python<'py>,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| binary(py, lhs, rhs, RuleExpr::add))
}

/// Author an ordered multiplication.
#[pyfunction]
fn mul<'py>(
    py: Python<'py>,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| binary(py, lhs, rhs, RuleExpr::multiply))
}

/// Author a scalar power expression.
#[pyfunction]
fn power<'py>(
    py: Python<'py>,
    base: &Bound<'_, PyAny>,
    exponent: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| binary(py, base, exponent, RuleExpr::power))
}

/// Author an ordered minimum.
#[pyfunction(name = "min")]
fn minimum<'py>(
    py: Python<'py>,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| binary(py, lhs, rhs, RuleExpr::minimum))
}

/// Author an ordered maximum.
#[pyfunction(name = "max")]
fn maximum<'py>(
    py: Python<'py>,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| binary(py, lhs, rhs, RuleExpr::maximum))
}

/// Author a scalar clamp.
#[pyfunction]
fn clamp<'py>(
    py: Python<'py>,
    value: &Bound<'_, PyAny>,
    lower: &Bound<'_, PyAny>,
    upper: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let value = RuleExpr::clamp(expression(value)?, expression(lower)?, expression(upper)?)
            .map_err(rule_error)?;
        authored_expression(py, &value)
    })
}

/// Author a scalar comparison used by `select` conditions.
#[pyfunction]
fn compare<'py>(
    py: Python<'py>,
    comparison: &str,
    lhs: &Bound<'_, PyAny>,
    rhs: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let comparison = match comparison {
            "equal" => ScalarComparison::Equal,
            "not_equal" => ScalarComparison::NotEqual,
            "less_than" => ScalarComparison::LessThan,
            "less_than_or_equal" => ScalarComparison::LessThanOrEqual,
            "greater_than" => ScalarComparison::GreaterThan,
            "greater_than_or_equal" => ScalarComparison::GreaterThanOrEqual,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown scalar comparison `{other}`"
                )));
            }
        };
        let value = RuleExpr::comparison(comparison, expression(lhs)?, expression(rhs)?)
            .map_err(rule_error)?;
        authored_expression(py, &value)
    })
}

/// Author a truth-directed scalar selection.
#[pyfunction]
fn select<'py>(
    py: Python<'py>,
    condition: &Bound<'_, PyAny>,
    when_true: &Bound<'_, PyAny>,
    when_false: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let value = RuleExpr::select(
            expression(condition)?,
            expression(when_true)?,
            expression(when_false)?,
        )
        .map_err(rule_error)?;
        authored_expression(py, &value)
    })
}

/// Author an interpolation-table lookup.
#[pyfunction]
fn table_lookup<'py>(
    py: Python<'py>,
    table: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| {
        let table = TableId::parse(table).map_err(rule_error)?;
        let value =
            RuleExpr::interpolated_table(InterpolatedTableRef::new(table), expression(value)?)
                .map_err(rule_error)?;
        authored_expression(py, &value)
    })
}

/// Decode and re-encode an expression through the authoritative Rust IR.
#[pyfunction]
fn roundtrip_expression<'py>(
    py: Python<'py>,
    value: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    contain(|| authored_expression(py, &expression(value)?))
}

fn initialize(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CompiledModel>()?;
    module.add_class::<CompletedRun>()?;
    module.add_class::<PresenceValues>()?;
    module.add_class::<PresenceSeries>()?;
    module.add_class::<PresenceCountValues>()?;
    module.add_class::<PresenceCountSeries>()?;
    module.add_function(wrap_pyfunction!(compile_model, module)?)?;
    module.add_function(wrap_pyfunction!(literal, module)?)?;
    module.add_function(wrap_pyfunction!(param, module)?)?;
    module.add_function(wrap_pyfunction!(input, module)?)?;
    module.add_function(wrap_pyfunction!(forcing, module)?)?;
    module.add_function(wrap_pyfunction!(projection, module)?)?;
    module.add_function(wrap_pyfunction!(add, module)?)?;
    module.add_function(wrap_pyfunction!(mul, module)?)?;
    module.add_function(wrap_pyfunction!(power, module)?)?;
    module.add_function(wrap_pyfunction!(minimum, module)?)?;
    module.add_function(wrap_pyfunction!(maximum, module)?)?;
    module.add_function(wrap_pyfunction!(clamp, module)?)?;
    module.add_function(wrap_pyfunction!(compare, module)?)?;
    module.add_function(wrap_pyfunction!(select, module)?)?;
    module.add_function(wrap_pyfunction!(table_lookup, module)?)?;
    module.add_function(wrap_pyfunction!(roundtrip_expression, module)?)?;
    Ok(())
}

/// Python module initialiser.
#[pymodule]
fn _incidence(module: &Bound<'_, PyModule>) -> PyResult<()> {
    contain(|| initialize(module))
}
