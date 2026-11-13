//! validated_transaction : ModelArtifact × AuthoritativeLog × Disposition ⇀ AuthoritativeLog   (deterministic, atomic)
//!
//! A disposition explicitly partitions every registered substance held by one finite
//! compartment into a retained amount and outgoing transfers. Validation is performed against
//! the authoritative fold and transfers are appended only after every generated transfer folds.
//! Substance entries commit in registry order; allocations retain their authored order.

use std::collections::BTreeMap;

use crate::endpoints::FiniteCompartment;
use crate::identity::{CompartmentId, SubstanceId};
use crate::ledger::{
    AuthoritativeLog, LogError, QuantumAmount, QuantumCount, ReplayError, RunId, StockState,
    Transfer, TransferEndpoint, TransferError, apply_transfer, replay_with_artifact, seed_state,
};
use crate::model_artifact::ModelArtifact;
use crate::non_negative_amount::NonNegativeAmount;
use crate::presence::ValueState;
use crate::temporal::TimestepIndex;
use crate::topology::TopologyEndpoint;

#[cfg(test)]
#[path = "disposition_tests.rs"]
mod tests;

/// One explicitly authored destination and amount in a substance partition.
#[derive(Clone, Debug, PartialEq)]
pub struct Allocation {
    target: TransferEndpoint,
    amount: NonNegativeAmount,
    authoritative_count: Option<QuantumCount>,
}

impl Allocation {
    #[must_use]
    pub fn new(target: impl Into<TransferEndpoint>, amount: NonNegativeAmount) -> Self {
        Self {
            target: target.into(),
            amount,
            authoritative_count: None,
        }
    }

    pub(crate) fn from_count(
        target: impl Into<TransferEndpoint>,
        amount: NonNegativeAmount,
        authoritative_count: QuantumCount,
    ) -> Self {
        Self {
            target: target.into(),
            amount,
            authoritative_count: Some(authoritative_count),
        }
    }

    #[must_use]
    pub fn target(&self) -> &TransferEndpoint {
        &self.target
    }

    #[must_use]
    pub const fn amount(&self) -> NonNegativeAmount {
        self.amount
    }
}

/// An explicit partition of one substance into retained and transferred amounts.
#[derive(Clone, Debug, PartialEq)]
pub struct SubstanceDisposition {
    substance: SubstanceId,
    retained: NonNegativeAmount,
    allocations: Vec<Allocation>,
}

impl SubstanceDisposition {
    #[must_use]
    pub fn new(
        substance: SubstanceId,
        retained: NonNegativeAmount,
        allocations: impl IntoIterator<Item = Allocation>,
    ) -> Self {
        Self {
            substance,
            retained,
            allocations: allocations.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn substance(&self) -> &SubstanceId {
        &self.substance
    }

    #[must_use]
    pub const fn retained(&self) -> NonNegativeAmount {
        self.retained
    }

    #[must_use]
    pub fn allocations(&self) -> &[Allocation] {
        &self.allocations
    }
}

/// A rule's complete, explicitly authored disposition for one finite compartment and timestep.
#[derive(Clone, Debug, PartialEq)]
pub struct Disposition {
    timestep: TimestepIndex,
    source: FiniteCompartment,
    substances: Vec<SubstanceDisposition>,
}

impl Disposition {
    #[must_use]
    pub fn new(
        timestep: TimestepIndex,
        source: FiniteCompartment,
        substances: impl IntoIterator<Item = SubstanceDisposition>,
    ) -> Self {
        Self {
            timestep,
            source,
            substances: substances.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn timestep(&self) -> TimestepIndex {
        self.timestep
    }

    #[must_use]
    pub fn source(&self) -> &FiniteCompartment {
        &self.source
    }

    #[must_use]
    pub fn substances(&self) -> &[SubstanceDisposition] {
        &self.substances
    }
}

/// Public write boundary for dispositions applied to an externally supplied log.
pub enum ValidatedTransaction {}

/// An execution-owned log and its disposable fold, bound to one immutable artifact.
/// Exclusive ownership prevents a caller from pairing stale balances with a changed log.
pub(crate) struct CurrentRun<'a> {
    artifact: &'a ModelArtifact,
    log: AuthoritativeLog,
    state: StockState,
}

impl<'a> CurrentRun<'a> {
    pub(crate) fn new(artifact: &'a ModelArtifact, run_id: RunId) -> Result<Self, ReplayError> {
        Ok(Self {
            artifact,
            log: AuthoritativeLog::for_run(run_id, artifact),
            state: seed_state(artifact)?,
        })
    }

    pub(crate) fn log(&self) -> &AuthoritativeLog {
        &self.log
    }

    pub(crate) fn state(&self) -> &StockState {
        &self.state
    }

    pub(crate) fn commit(&mut self, disposition: Disposition) -> Result<(), TransactionError> {
        commit_from_state(&mut self.log, self.artifact, &mut self.state, disposition)
    }

    pub(crate) fn into_log(self) -> AuthoritativeLog {
        self.log
    }
}

impl ValidatedTransaction {
    /// Validates an exhaustive disposition and atomically appends its derived transfers.
    ///
    /// The supplied log is unchanged on every error. Retention must be authored explicitly;
    /// it is never inferred by subtracting emitted amounts from available stock.
    ///
    /// # Errors
    /// Returns [`TransactionError`] for an incomplete or invalid partition, an incompatible
    /// authoritative prefix, or a transfer rejected by the authoritative fold.
    pub fn commit(
        log: &mut AuthoritativeLog,
        artifact: &ModelArtifact,
        disposition: Disposition,
    ) -> Result<(), TransactionError> {
        commit_disposition(log, artifact, disposition)
    }
}

/// Validates an exhaustive disposition and atomically appends its derived transfers.
///
/// # Errors
/// Returns [`TransactionError`] without changing `log` when any validation fails.
pub fn commit_disposition(
    log: &mut AuthoritativeLog,
    artifact: &ModelArtifact,
    disposition: Disposition,
) -> Result<(), TransactionError> {
    let before = replay_with_artifact(log, artifact)
        .map_err(|source| TransactionError::Replay { source })?;
    commit_from_state(
        log,
        artifact,
        &mut before.final_state().clone(),
        disposition,
    )
}

/// Shared transaction checks for an independently replayed or execution-owned fold.
fn commit_from_state(
    log: &mut AuthoritativeLog,
    artifact: &ModelArtifact,
    state: &mut StockState,
    disposition: Disposition,
) -> Result<(), TransactionError> {
    validate_transaction_context(log, artifact, &disposition)?;
    let compartment = disposition.source.id().clone();
    let timestep = disposition.timestep;

    let mut authored = BTreeMap::new();
    for entry in &disposition.substances {
        if !artifact.registry().contains(&entry.substance) {
            return Err(TransactionError::UnregisteredSubstance {
                compartment,
                substance: entry.substance.clone(),
                timestep,
            });
        }
        if authored.insert(entry.substance.clone(), entry).is_some() {
            return Err(TransactionError::DuplicateSubstance {
                compartment,
                substance: entry.substance.clone(),
                timestep,
            });
        }
    }

    let mut transfers = Vec::new();
    for substance in artifact.registry().iter() {
        let entry =
            authored
                .get(substance)
                .copied()
                .ok_or_else(|| TransactionError::OmittedSubstance {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    timestep,
                })?;
        let available = match state.finite_stock(&compartment, substance) {
            ValueState::Present(amount) => amount,
            ValueState::Absent | ValueState::NotModelled => {
                return Err(TransactionError::StockUnavailable {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    timestep,
                });
            }
        };
        let quantum =
            artifact
                .quantum(substance)
                .ok_or_else(|| TransactionError::StockUnavailable {
                    compartment: compartment.clone(),
                    substance: substance.clone(),
                    timestep,
                })?;
        let available_count = state
            .finite_quantum_count(&compartment, substance)
            .ok_or_else(|| TransactionError::StockUnavailable {
                compartment: compartment.clone(),
                substance: substance.clone(),
                timestep,
            })?;
        let mut allocation_count = 0_u128;
        for allocation in &entry.allocations {
            let count = if let Some(count) = allocation.authoritative_count {
                let projected = quantum.to_value(count.value()).ok_or_else(|| {
                    TransactionError::NonQuantumAllocation {
                        compartment: compartment.clone(),
                        substance: substance.clone(),
                        timestep,
                        amount_bits: allocation.amount.value().to_bits(),
                        quantum_bits: quantum.value().to_bits(),
                    }
                })?;
                if projected.to_bits() != allocation.amount.value().to_bits() {
                    return Err(TransactionError::NonQuantumAllocation {
                        compartment: compartment.clone(),
                        substance: substance.clone(),
                        timestep,
                        amount_bits: allocation.amount.value().to_bits(),
                        quantum_bits: quantum.value().to_bits(),
                    });
                }
                count
            } else {
                let raw = quantum
                    .whole_count(allocation.amount.value())
                    .ok_or_else(|| TransactionError::NonQuantumAllocation {
                        compartment: compartment.clone(),
                        substance: substance.clone(),
                        timestep,
                        amount_bits: allocation.amount.value().to_bits(),
                        quantum_bits: quantum.value().to_bits(),
                    })?;
                QuantumCount::try_from(raw).map_err(|source| TransactionError::Transfer {
                    compartment: compartment.clone(),
                    timestep,
                    source,
                })?
            };
            allocation_count += u128::from(count.value());
            if count.value() != 0 {
                let quantum_amount = QuantumAmount::new(quantum, count).map_err(|source| {
                    TransactionError::Transfer {
                        compartment: compartment.clone(),
                        timestep,
                        source,
                    }
                })?;
                transfers.push(
                    Transfer::new(
                        timestep,
                        disposition.source.clone(),
                        allocation.target.clone(),
                        artifact.registry(),
                        [(substance.clone(), quantum_amount)],
                    )
                    .map_err(|source| TransactionError::Transfer {
                        compartment: compartment.clone(),
                        timestep,
                        source,
                    })?,
                );
            }
        }
        if allocation_count > u128::from(available_count) {
            return Err(TransactionError::Overdraw {
                compartment: compartment.clone(),
                substance: substance.clone(),
                timestep,
                available_bits: available.value().to_bits(),
                requested_bits: entry.retained.value().to_bits(),
            });
        }
        let retained_count = available_count - allocation_count as u64;
        let expected_retained = quantum.to_value(retained_count).ok_or_else(|| {
            TransactionError::NonFinitePartition {
                compartment: compartment.clone(),
                substance: substance.clone(),
                timestep,
            }
        })?;
        if entry.retained.value().to_bits() != expected_retained.to_bits() {
            return Err(TransactionError::IncompletePartition {
                compartment: compartment.clone(),
                substance: substance.clone(),
                timestep,
                available_bits: available.value().to_bits(),
                partitioned_bits: entry.retained.value().to_bits(),
            });
        }
    }
    let mut staged = state.clone();
    for transfer in &transfers {
        apply_transfer(&mut staged, transfer, artifact)
            .map_err(|source| TransactionError::Replay { source })?;
    }
    for (substance, entry) in authored {
        match staged.finite_stock(&compartment, &substance) {
            ValueState::Present(actual)
                if actual.value().to_bits() == entry.retained.value().to_bits() => {}
            ValueState::Present(actual) => {
                return Err(TransactionError::RetainedMismatch {
                    compartment: compartment.clone(),
                    substance,
                    timestep,
                    declared_bits: entry.retained.value().to_bits(),
                    actual_bits: actual.value().to_bits(),
                });
            }
            ValueState::Absent | ValueState::NotModelled => {
                return Err(TransactionError::StockUnavailable {
                    compartment: compartment.clone(),
                    substance,
                    timestep,
                });
            }
        }
    }
    log.append_batch(transfers)
        .map_err(|source| TransactionError::Log { source })?;
    *state = staged;
    Ok(())
}

fn validate_transaction_context(
    log: &AuthoritativeLog,
    artifact: &ModelArtifact,
    disposition: &Disposition,
) -> Result<(), TransactionError> {
    let timestep = disposition.timestep;
    if log.is_sealed() {
        return Err(TransactionError::Log {
            source: LogError::RecordAfterSeal,
        });
    }
    if let Some(previous) = log.transfers().last().map(Transfer::timestep)
        && timestep < previous
    {
        return Err(TransactionError::Log {
            source: LogError::TimestepOrder {
                previous,
                next: timestep,
            },
        });
    }
    let horizon = artifact.horizon();
    if !horizon.contains(timestep) {
        return Err(TransactionError::Replay {
            source: ReplayError::TimestepOutsideHorizon {
                timestep,
                first: horizon.first(),
                last: horizon.last(),
            },
        });
    }
    match artifact.topology().endpoint(disposition.source.id()) {
        None => {
            return Err(TransactionError::Replay {
                source: ReplayError::UnknownEndpoint {
                    compartment: disposition.source.id().clone(),
                },
            });
        }
        Some(TopologyEndpoint::Boundary(_)) => {
            return Err(TransactionError::Replay {
                source: ReplayError::EndpointKindMismatch {
                    compartment: disposition.source.id().clone(),
                },
            });
        }
        Some(TopologyEndpoint::Finite(_)) => {}
    }
    for allocation in disposition
        .substances
        .iter()
        .flat_map(|entry| &entry.allocations)
    {
        let target = &allocation.target;
        match (target, artifact.topology().endpoint(target.id())) {
            (_, None) => {
                return Err(TransactionError::Replay {
                    source: ReplayError::UnknownEndpoint {
                        compartment: target.id().clone(),
                    },
                });
            }
            (TransferEndpoint::Finite(_), Some(TopologyEndpoint::Finite(_)))
            | (TransferEndpoint::Boundary(_), Some(TopologyEndpoint::Boundary(_))) => {}
            _ => {
                return Err(TransactionError::Replay {
                    source: ReplayError::EndpointKindMismatch {
                        compartment: target.id().clone(),
                    },
                });
            }
        }
        if target.id() == disposition.source.id() {
            return Err(TransactionError::Replay {
                source: ReplayError::SelfTransfer {
                    compartment: target.id().clone(),
                    timestep,
                },
            });
        }
        let connected = artifact.topology().connections().iter().any(|connection| {
            connection.source() == disposition.source.id() && connection.target() == target.id()
        });
        if !connected {
            return Err(TransactionError::Replay {
                source: ReplayError::UndeclaredConnection {
                    connection_source: disposition.source.id().clone(),
                    target: target.id().clone(),
                    timestep,
                },
            });
        }
    }
    Ok(())
}

/// Domain-facing name for failures at the disposition boundary.
pub type DispositionError = TransactionError;

/// Failures which prevent an evaluated disposition from being committed.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TransactionError {
    /// Fires when no explicit disposition is authored for a registered substance.
    #[error(
        "disposition for compartment `{compartment}` omits substance `{substance}` at timestep {timestep:?}"
    )]
    OmittedSubstance {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when a disposition repeats a substance entry.
    #[error(
        "disposition for compartment `{compartment}` repeats substance `{substance}` at timestep {timestep:?}"
    )]
    DuplicateSubstance {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when a disposition names a substance outside the artifact registry.
    #[error(
        "disposition for compartment `{compartment}` names unregistered substance `{substance}` at timestep {timestep:?}"
    )]
    UnregisteredSubstance {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when a partition disposes more than the source currently holds.
    #[error(
        "overdraw of compartment `{compartment}`, substance `{substance}` at timestep {timestep:?}: available bits {available_bits:#018x}, requested bits {requested_bits:#018x}"
    )]
    Overdraw {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        available_bits: u64,
        requested_bits: u64,
    },
    /// Fires when a positive allocation is too small to debit the finite source exactly.
    #[error(
        "allocation from compartment `{compartment}`, substance `{substance}` at timestep {timestep:?} cannot debit stock bits {stock_bits:#018x} by amount bits {amount_bits:#018x}"
    )]
    IneffectiveDebit {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        stock_bits: u64,
        amount_bits: u64,
    },
    /// Fires when an authored allocation is not an exact whole multiple of the declared quantum.
    #[error(
        "allocation from compartment `{compartment}`, substance `{substance}` at timestep {timestep:?} has amount bits {amount_bits:#018x}, not a whole multiple of quantum bits {quantum_bits:#018x}"
    )]
    NonQuantumAllocation {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        amount_bits: u64,
        quantum_bits: u64,
    },
    /// Fires when explicit retention and allocations do not exactly cover available stock.
    #[error(
        "incomplete partition for compartment `{compartment}`, substance `{substance}` at timestep {timestep:?}: available bits {available_bits:#018x}, partitioned bits {partitioned_bits:#018x}"
    )]
    IncompletePartition {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        available_bits: u64,
        partitioned_bits: u64,
    },
    /// Fires when deterministic partition accumulation becomes non-finite.
    #[error(
        "non-finite partition for compartment `{compartment}`, substance `{substance}` at timestep {timestep:?}"
    )]
    NonFinitePartition {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when the authoritative fold cannot supply a registered finite stock coordinate.
    #[error(
        "stock unavailable for compartment `{compartment}`, substance `{substance}` at timestep {timestep:?}"
    )]
    StockUnavailable {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
    },
    /// Fires when committed fold arithmetic does not reproduce explicitly declared retention.
    #[error(
        "retained amount mismatch for compartment `{compartment}`, substance `{substance}` at timestep {timestep:?}: declared bits {declared_bits:#018x}, actual bits {actual_bits:#018x}"
    )]
    RetainedMismatch {
        compartment: CompartmentId,
        substance: SubstanceId,
        timestep: TimestepIndex,
        declared_bits: u64,
        actual_bits: u64,
    },
    /// Fires when projected transfer amounts cannot bind to authoritative counts.
    #[error(
        "cannot construct authoritative transfer for compartment `{compartment}` at timestep {timestep:?}: {source}"
    )]
    Transfer {
        compartment: CompartmentId,
        timestep: TimestepIndex,
        source: TransferError,
    },
    /// Fires when the append-only log boundary rejects a generated transfer.
    #[error(transparent)]
    Log { source: LogError },
    /// Fires when the supplied prefix or staged transfers fail authoritative replay.
    #[error(transparent)]
    Replay { source: ReplayError },
}
