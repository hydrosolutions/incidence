//! dense_transfer_history : ModelArtifact × CompletedAuthoritativeLog × AuthoritativeFactSelector ⇀ (RunHorizon → ValueState<NonNegativeAmount>)   (pure, deterministic)
//! dense_transfer_count_history : ModelArtifact × CompletedAuthoritativeLog × AuthoritativeFactSelector ⇀ (RunHorizon → ValueState<QuantumCount>)   (pure, deterministic)
//!
//! A dense transfer history is a disposable view. It folds transfer facts in authoritative
//! record order and materialises the additive identity at every timestep proved present by a
//! completion seal. It never treats an unsealed suffix, an out-of-horizon timestep, or an
//! unmodelled substance as zero.

use std::sync::Arc;

use crate::identity::{CompartmentId, SubstanceId};
use crate::ledger::{
    AuthoritativeLog, CompletenessReader, QuantumCount, ReplayError, RunStatus, Transfer,
    TransferEndpoint,
};
use crate::model_artifact::{MAX_EXACT_WHOLE_MULTIPLE_COUNT, ModelArtifact, ModelArtifactArchive};
use crate::non_negative_amount::NonNegativeAmount;
use crate::numerical_semantics::NumericalSemanticsError;
use crate::presence::ValueState;
use crate::projection::AuthoritativeFactSelector;
use crate::temporal::{
    CalendarInstant, FixedStepCalendar, RunHorizon, TemporalError, TimestepIndex,
};

/// Failures constructing or querying a dense authoritative-fact projection.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DenseProjectionError {
    /// Fires when authoritative replay rejects the log or its bound artifact.
    #[error("authoritative replay rejected dense projection: {source}")]
    Replay {
        #[from]
        source: ReplayError,
    },
    /// Fires when the log has no completion seal proving every timestep in the horizon present.
    #[error("dense projection requires a completed run; log is a resumable prefix")]
    IncompleteRun,
    /// Fires when a selector names no endpoint in the bound model topology.
    #[error("dense projection selector names unknown compartment {compartment}")]
    UnknownCompartment { compartment: CompartmentId },
    /// Fires if a validated transfer unexpectedly carries a different substance registry.
    #[error(
        "dense projection transfer registry omits modelled substance {substance} at timestep {timestep:?}"
    )]
    TransferRegistryMismatch {
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when an inclusive horizon cannot be represented by this process.
    #[error("dense projection horizon from {first} through {last} is too large to materialise")]
    HorizonTooLarge { first: u64, last: u64 },
    /// Fires when ordered accumulation cannot produce a finite amount.
    #[error(
        "dense projection accumulation failed for compartment {compartment}, substance {substance}, timestep {timestep:?}: {source}"
    )]
    Accumulation {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        source: NumericalSemanticsError,
    },
    /// Fires if a numerical implementation ever returns a non-amount from non-negative inputs.
    #[error(
        "dense projection produced invalid amount bits {bits:#018x} for compartment {compartment}, substance {substance}, timestep {timestep:?}"
    )]
    InvalidAccumulatedAmount {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        bits: u64,
    },
    /// Fires when exact count accumulation exceeds the conserved-state count ceiling.
    #[error(
        "dense count projection exceeds count ceiling {ceiling} for compartment {compartment}, substance {substance}, timestep {timestep:?}"
    )]
    CountAboveCeiling {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        ceiling: u64,
    },
    /// Fires when a calendar instant cannot be converted to a timestep coordinate.
    #[error("dense projection calendar query failed: {source}")]
    Calendar {
        #[from]
        source: TemporalError,
    },
}

/// An immutable log/artifact pair whose complete independent replay succeeded once.
/// Valid prefixes remain prefixes; dense constructors still require a completion seal.
pub struct ValidatedTransferLog {
    log: Arc<AuthoritativeLog>,
    artifact: Arc<ModelArtifact>,
    status: RunStatus,
}

impl ValidatedTransferLog {
    /// Bind immutable inputs after the ordinary independent replay checks.
    ///
    /// # Errors
    /// Returns the existing replay errors for mismatched artifacts, malformed records,
    /// failed conservation or invalid seals. The pair cannot be mutated after validation.
    pub fn new(
        log: Arc<AuthoritativeLog>,
        artifact: Arc<ModelArtifact>,
    ) -> Result<Self, DenseProjectionError> {
        let replay = CompletenessReader::read_with_artifact(&log, artifact.clone())?;
        Ok(Self {
            log,
            artifact,
            status: replay.status(),
        })
    }
}

/// One dense point, retaining its typed timestep coordinate beside its amount.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DenseHistoryPoint {
    timestep: TimestepIndex,
    amount: ValueState<NonNegativeAmount>,
}
impl DenseHistoryPoint {
    #[must_use]
    pub const fn timestep(self) -> TimestepIndex {
        self.timestep
    }
    #[must_use]
    pub const fn amount(self) -> ValueState<NonNegativeAmount> {
        self.amount
    }
}

/// A complete, dense, registry-aware history of one typed transfer fact.
#[derive(Clone, Debug, PartialEq)]
pub struct DenseTransferProjection {
    selector: AuthoritativeFactSelector,
    horizon: RunHorizon,
    calendar: FixedStepCalendar,
    modelled: bool,
    values: Vec<NonNegativeAmount>,
}

impl DenseTransferProjection {
    /// Project an already replayed immutable pair without repeating its full replay.
    ///
    /// # Errors
    /// Retains the usual completeness, coordinate and ordered accumulation errors.
    pub fn from_validated_log(
        validated: &ValidatedTransferLog,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        Self::from_validated(
            &validated.log,
            &validated.artifact,
            validated.status,
            selector,
        )
    }

    /// Rebuilds a dense projection using the historical artifact selected by Genesis.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError`] when the artifact/log binding is invalid, the run is not
    /// sealed through its declared horizon, the selector endpoint is unknown, or an ordered sum
    /// cannot be represented.
    pub fn from_log(
        log: &AuthoritativeLog,
        archive: &ModelArtifactArchive,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        let replay = CompletenessReader::read(log, archive)?;
        Self::from_validated(log, replay.artifact(), replay.status(), selector)
    }

    /// Rebuilds a dense projection against an explicitly supplied immutable artifact.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError`] when the supplied artifact does not match Genesis, the
    /// run is incomplete, the selector endpoint is unknown, or accumulation fails.
    pub fn from_log_with_artifact(
        log: &AuthoritativeLog,
        artifact: &ModelArtifact,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        let replay = CompletenessReader::read_with_artifact(log, Arc::new(artifact.clone()))?;
        Self::from_validated(log, replay.artifact(), replay.status(), selector)
    }

    fn from_validated(
        log: &AuthoritativeLog,
        artifact: &ModelArtifact,
        status: RunStatus,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        if !matches!(status, RunStatus::Complete { .. }) {
            return Err(DenseProjectionError::IncompleteRun);
        }
        if artifact
            .topology()
            .endpoint(selector.compartment())
            .is_none()
        {
            return Err(DenseProjectionError::UnknownCompartment {
                compartment: selector.compartment().clone(),
            });
        }

        let horizon = artifact.horizon();
        let first = horizon.first().value();
        let last = horizon.last().value();
        let length_u64 = last
            .checked_sub(first)
            .and_then(|span| span.checked_add(1))
            .ok_or(DenseProjectionError::HorizonTooLarge { first, last })?;
        let length = usize::try_from(length_u64)
            .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
        let modelled = artifact.registry().contains(selector.substance());
        let mut values = Vec::new();
        values
            .try_reserve_exact(length)
            .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
        values.resize(length, NonNegativeAmount::ZERO);

        if modelled {
            let semantics = artifact.versions().numerical_semantics();
            for transfer in log.transfers() {
                if !matches_selector(&selector, transfer) {
                    continue;
                }
                let amount = match transfer.amounts().amount(selector.substance()) {
                    ValueState::Present(amount) => amount,
                    ValueState::Absent | ValueState::NotModelled => {
                        return Err(DenseProjectionError::TransferRegistryMismatch {
                            substance: selector.substance().clone(),
                            timestep: transfer.timestep(),
                        });
                    }
                };
                let offset = transfer.timestep().value() - first;
                let index = usize::try_from(offset)
                    .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
                let accumulated = semantics
                    .add(values[index].value(), amount.value())
                    .map_err(|source| DenseProjectionError::Accumulation {
                        compartment: selector.compartment().clone(),
                        substance: selector.substance().clone(),
                        timestep: transfer.timestep(),
                        source,
                    })?;
                values[index] = NonNegativeAmount::try_from(accumulated).map_err(|_| {
                    DenseProjectionError::InvalidAccumulatedAmount {
                        compartment: selector.compartment().clone(),
                        substance: selector.substance().clone(),
                        timestep: transfer.timestep(),
                        bits: accumulated.to_bits(),
                    }
                })?;
            }
        }

        Ok(Self {
            selector,
            horizon,
            calendar: artifact.calendar(),
            modelled,
            values,
        })
    }

    #[must_use]
    pub fn selector(&self) -> &AuthoritativeFactSelector {
        &self.selector
    }
    #[must_use]
    pub const fn horizon(&self) -> RunHorizon {
        self.horizon
    }
    #[must_use]
    pub const fn calendar(&self) -> FixedStepCalendar {
        self.calendar
    }
    /// Returns the number of proved timesteps in the dense history.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }
    /// A dense history built from a valid horizon is never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    /// Reports whether the selected substance belongs to the producing model registry.
    #[must_use]
    pub const fn is_modelled(&self) -> bool {
        self.modelled
    }

    /// Returns explicit zero for a dry timestep in the proved horizon, and absence outside it.
    #[must_use]
    pub fn value_at(&self, timestep: TimestepIndex) -> ValueState<NonNegativeAmount> {
        if !self.modelled {
            return ValueState::NotModelled;
        }
        if !self.horizon.contains(timestep) {
            return ValueState::Absent;
        }
        let offset = timestep.value() - self.horizon.first().value();
        usize::try_from(offset)
            .ok()
            .and_then(|index| self.values.get(index).copied())
            .map_or(ValueState::Absent, ValueState::Present)
    }

    /// Resolves a calendar coordinate before performing the typed history query.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError::Calendar`] when the instant precedes or is not aligned to
    /// the fixed-step calendar.
    pub fn value_at_instant(
        &self,
        instant: CalendarInstant,
    ) -> Result<ValueState<NonNegativeAmount>, DenseProjectionError> {
        Ok(self.value_at(self.calendar.index_of(instant)?))
    }

    /// Iterates every proved timestep, including explicit zeroes, in ascending order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = DenseHistoryPoint> + '_ {
        let first = self.horizon.first().value();
        self.values
            .iter()
            .copied()
            .enumerate()
            .map(move |(offset, amount)| DenseHistoryPoint {
                timestep: TimestepIndex::new(first + offset as u64),
                amount: if self.modelled {
                    ValueState::Present(amount)
                } else {
                    ValueState::NotModelled
                },
            })
    }
}

/// One dense point, retaining its typed timestep coordinate beside its authoritative count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DenseCountHistoryPoint {
    timestep: TimestepIndex,
    count: ValueState<QuantumCount>,
}

impl DenseCountHistoryPoint {
    #[must_use]
    pub const fn timestep(self) -> TimestepIndex {
        self.timestep
    }

    #[must_use]
    pub const fn count(self) -> ValueState<QuantumCount> {
        self.count
    }
}

/// A complete, dense history folded directly from authoritative transfer counts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenseTransferCountProjection {
    selector: AuthoritativeFactSelector,
    horizon: RunHorizon,
    calendar: FixedStepCalendar,
    modelled: bool,
    counts: Vec<QuantumCount>,
}

impl DenseTransferCountProjection {
    /// Project exact counts from an already replayed immutable pair.
    ///
    /// # Errors
    /// Retains the usual completeness, coordinate and count-ceiling errors.
    pub fn from_validated_log(
        validated: &ValidatedTransferLog,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        Self::from_validated(
            &validated.log,
            &validated.artifact,
            validated.status,
            selector,
        )
    }

    /// Rebuilds a dense count projection using the historical artifact selected by Genesis.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError`] when artifact/log binding or completeness validation fails,
    /// the selector endpoint is unknown, or an accumulated count exceeds the exact-count ceiling.
    pub fn from_log(
        log: &AuthoritativeLog,
        archive: &ModelArtifactArchive,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        let replay = CompletenessReader::read(log, archive)?;
        Self::from_validated(log, replay.artifact(), replay.status(), selector)
    }

    /// Rebuilds a dense count projection against an explicitly supplied immutable artifact.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError`] under the same conditions as [`Self::from_log`].
    pub fn from_log_with_artifact(
        log: &AuthoritativeLog,
        artifact: &ModelArtifact,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        let replay = CompletenessReader::read_with_artifact(log, Arc::new(artifact.clone()))?;
        Self::from_validated(log, replay.artifact(), replay.status(), selector)
    }

    fn from_validated(
        log: &AuthoritativeLog,
        artifact: &ModelArtifact,
        status: RunStatus,
        selector: AuthoritativeFactSelector,
    ) -> Result<Self, DenseProjectionError> {
        if !matches!(status, RunStatus::Complete { .. }) {
            return Err(DenseProjectionError::IncompleteRun);
        }
        if artifact
            .topology()
            .endpoint(selector.compartment())
            .is_none()
        {
            return Err(DenseProjectionError::UnknownCompartment {
                compartment: selector.compartment().clone(),
            });
        }
        let horizon = artifact.horizon();
        let first = horizon.first().value();
        let last = horizon.last().value();
        let length_u64 = last
            .checked_sub(first)
            .and_then(|span| span.checked_add(1))
            .ok_or(DenseProjectionError::HorizonTooLarge { first, last })?;
        let length = usize::try_from(length_u64)
            .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
        let modelled = artifact.registry().contains(selector.substance());
        let mut counts: Vec<QuantumCount> = Vec::new();
        counts
            .try_reserve_exact(length)
            .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
        counts.resize(length, QuantumCount::ZERO);

        if modelled {
            for transfer in log.transfers() {
                if !matches_selector(&selector, transfer) {
                    continue;
                }
                let count = transfer
                    .quantum_count(selector.substance())
                    .unwrap_or(QuantumCount::ZERO);
                let offset = transfer.timestep().value() - first;
                let index = usize::try_from(offset)
                    .map_err(|_| DenseProjectionError::HorizonTooLarge { first, last })?;
                let count_error = || DenseProjectionError::CountAboveCeiling {
                    compartment: selector.compartment().clone(),
                    substance: selector.substance().clone(),
                    timestep: transfer.timestep(),
                    ceiling: MAX_EXACT_WHOLE_MULTIPLE_COUNT,
                };
                let total = counts[index]
                    .value()
                    .checked_add(count.value())
                    .ok_or_else(&count_error)?;
                counts[index] = QuantumCount::try_from(total).map_err(|_| count_error())?;
            }
        }

        Ok(Self {
            selector,
            horizon,
            calendar: artifact.calendar(),
            modelled,
            counts,
        })
    }

    #[must_use]
    pub fn selector(&self) -> &AuthoritativeFactSelector {
        &self.selector
    }

    #[must_use]
    pub const fn horizon(&self) -> RunHorizon {
        self.horizon
    }

    #[must_use]
    pub const fn calendar(&self) -> FixedStepCalendar {
        self.calendar
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    #[must_use]
    pub const fn is_modelled(&self) -> bool {
        self.modelled
    }

    /// Returns exact zero inside the proved horizon, absence outside it, and not-modelled for an
    /// undeclared substance.
    #[must_use]
    pub fn value_at(&self, timestep: TimestepIndex) -> ValueState<QuantumCount> {
        if !self.modelled {
            return ValueState::NotModelled;
        }
        if !self.horizon.contains(timestep) {
            return ValueState::Absent;
        }
        let offset = timestep.value() - self.horizon.first().value();
        usize::try_from(offset)
            .ok()
            .and_then(|index| self.counts.get(index).copied())
            .map_or(ValueState::Absent, ValueState::Present)
    }

    /// Resolves a calendar coordinate before performing the typed count query.
    ///
    /// # Errors
    ///
    /// Returns [`DenseProjectionError::Calendar`] for a preceding or unaligned instant.
    pub fn value_at_instant(
        &self,
        instant: CalendarInstant,
    ) -> Result<ValueState<QuantumCount>, DenseProjectionError> {
        Ok(self.value_at(self.calendar.index_of(instant)?))
    }

    /// Iterates every proved timestep, including exact zeroes, in ascending order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = DenseCountHistoryPoint> + '_ {
        let first = self.horizon.first().value();
        self.counts
            .iter()
            .copied()
            .enumerate()
            .map(move |(offset, count)| DenseCountHistoryPoint {
                timestep: TimestepIndex::new(first + offset as u64),
                count: if self.modelled {
                    ValueState::Present(count)
                } else {
                    ValueState::NotModelled
                },
            })
    }
}

/// A concise alias for clients that call the disposable view a history rather than a projection.
pub type DenseTransferHistory = DenseTransferProjection;
/// A concise alias for callers projecting one authoritative extensive fact.
pub type DenseProjection = DenseTransferProjection;

/// Builds a dense transfer history against an explicitly supplied artifact.
///
/// # Errors
///
/// Returns [`DenseProjectionError`] under the same conditions as
/// [`DenseTransferProjection::from_log_with_artifact`].
pub fn project_dense_transfer_history(
    log: &AuthoritativeLog,
    artifact: &ModelArtifact,
    selector: AuthoritativeFactSelector,
) -> Result<DenseTransferProjection, DenseProjectionError> {
    DenseTransferProjection::from_log_with_artifact(log, artifact, selector)
}

fn matches_selector(selector: &AuthoritativeFactSelector, transfer: &Transfer) -> bool {
    match selector {
        AuthoritativeFactSelector::IncomingTransferAmount { compartment, .. } => {
            endpoint_matches(transfer.target(), compartment)
        }
        AuthoritativeFactSelector::OutgoingTransferAmount { compartment, .. } => {
            endpoint_matches(transfer.source(), compartment)
        }
    }
}

fn endpoint_matches(endpoint: &TransferEndpoint, compartment: &CompartmentId) -> bool {
    endpoint.id() == compartment
}
