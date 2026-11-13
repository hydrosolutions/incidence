//! `incidence_core : () → ()` (pure conserved-flow domain boundary; no operations yet)

pub mod canonical_encoding;
pub mod dense_projection;
// Let internal regression tests reuse the public integration fixtures.
#[cfg(test)]
extern crate self as incidence_core;

pub mod disposition;
pub mod endpoints;
pub mod execution;
pub mod execution_bindings;
pub mod forcing;
pub mod identity;
pub mod initial_stocks;
pub mod interpolation_table;
pub mod ledger;
pub mod model_artifact;
pub mod model_document;
pub mod non_negative_amount;
pub mod numerical_semantics;
pub mod partition_expression;
pub mod presence;
pub mod projection;
pub mod rule_expression;
pub mod rule_reference;
pub mod signed_boundary_balance;
pub mod sparse_substance_vector;
pub mod substance_registry;
pub mod temporal;
pub mod topology;
pub mod versions;

pub mod workspace {
    //! `workspace_boundary : InitializedIncidenceWorkspace → CoreDomainBoundary` (pure)
}

/// Authoritative record vocabulary and append-only log API.
pub mod authoritative_log {
    pub use crate::ledger::{
        AuthoritativeLog, Genesis, LogDigest, LogError, Record, RunCompleted, RunId, Transfer,
        TransferEndpoint,
    };
}

/// Deterministic authoritative-log folding and completeness inspection.
pub mod replay {
    pub use crate::ledger::{
        CompletenessReader, ConservationTotals, Replay, ReplayError, RunStatus, StockState,
        incidence_column, incidence_columns_close, replay_with_artifact,
    };
}

/// Exhaustive rule dispositions and their atomic authoritative-log write boundary.
pub mod validated_transaction {
    pub use crate::disposition::{
        Allocation, Disposition, DispositionError, SubstanceDisposition, TransactionError,
        ValidatedTransaction, commit_disposition,
    };
}
