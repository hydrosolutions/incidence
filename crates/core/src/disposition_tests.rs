//! Regression checks for the execution-owned fold and all-or-nothing publication.

#[path = "../tests/support/mod.rs"]
mod support;

use crate::disposition::{
    Allocation, CurrentRun, Disposition, SubstanceDisposition, TransactionError, commit_disposition,
};
use crate::endpoints::BoundaryAccount;
use crate::initial_stocks::InitialStocks;
use crate::ledger::{
    AuthoritativeLog, QuantumAmount, QuantumCount, ReplayError, RunId, Transfer,
    replay_with_artifact,
};
use crate::model_artifact::{ModelArtifact, Quantum, SubstanceUnit, UnitId};
use crate::sparse_substance_vector::SparseSubstanceVector;
use crate::temporal::TimestepIndex;
use crate::topology::{DirectedConnection, Topology, TopologyEndpoint};

fn partition(
    artifact: &ModelArtifact,
    day: u64,
    water: f64,
    salt: f64,
    release: f64,
) -> Disposition {
    Disposition::new(
        TimestepIndex::new(day),
        support::finite(artifact, "store"),
        [
            SubstanceDisposition::new(
                support::substance("water"),
                support::amount(water),
                [Allocation::new(
                    support::endpoint(artifact, "route"),
                    support::amount(release),
                )],
            ),
            SubstanceDisposition::new(support::substance("salt"), support::amount(salt), []),
        ],
    )
}

#[test]
fn working_balances_match_independent_replay_after_each_commit_including_dry_turns() {
    let artifact = support::fixture();
    let mut run = CurrentRun::new(&artifact, RunId::from_bytes([3; 16])).expect("initial state");
    let mut external = AuthoritativeLog::for_run(RunId::from_bytes([3; 16]), &artifact);
    let mut water = 10.0;
    for (day, release) in [2.0, 0.0, 3.0].into_iter().enumerate() {
        water -= release;
        let disposition = partition(&artifact, day as u64, water, 4.0, release);
        run.commit(disposition.clone()).expect("working commit");
        commit_disposition(&mut external, &artifact, disposition).expect("public commit");
        let replay = replay_with_artifact(run.log(), &artifact).expect("independent replay");
        assert_eq!(run.state(), replay.final_state());
        assert_eq!(run.log().canonical_bytes(), external.canonical_bytes());
    }
}

#[test]
fn early_refusals_preserve_log_and_working_state_and_allow_retry() {
    let artifact = support::fixture();
    let mut run = CurrentRun::new(&artifact, RunId::from_bytes([3; 16])).expect("initial state");
    run.commit(partition(&artifact, 1, 8.0, 4.0, 2.0))
        .expect("first commit");
    let before_state = run.state().clone();
    let before_log = run.log().canonical_bytes();
    // Invalid retention, aggregate overdraw, and a backwards date.
    for disposition in [
        partition(&artifact, 2, 7.0, 4.0, 2.0),
        partition(&artifact, 2, 0.0, 4.0, 9.0),
        partition(&artifact, 0, 6.0, 4.0, 2.0),
        Disposition::new(
            TimestepIndex::new(2),
            support::finite(&artifact, "store"),
            [],
        ),
    ] {
        assert!(run.commit(disposition).is_err());
        assert_eq!(run.state(), &before_state);
        assert_eq!(run.log().canonical_bytes(), before_log);
    }
    run.commit(partition(&artifact, 2, 6.0, 4.0, 2.0))
        .expect("retry");
    assert_eq!(
        run.state(),
        replay_with_artifact(run.log(), &artifact)
            .expect("replay")
            .final_state()
    );
}

#[test]
fn late_fold_failure_rolls_back_all_staged_transfers_and_balances() {
    let base = support::fixture();
    let topology = Topology::new(
        base.topology()
            .endpoints()
            .cloned()
            .chain([TopologyEndpoint::Boundary(BoundaryAccount::new(
                support::compartment("supply"),
            ))]),
        base.topology()
            .connections()
            .iter()
            .cloned()
            .chain([DirectedConnection::new(
                support::compartment("supply"),
                support::compartment("store"),
            )]),
    )
    .expect("supply topology");
    let stock = |water, salt| {
        SparseSubstanceVector::new(
            base.registry(),
            [
                (support::substance("water"), support::amount(water)),
                (support::substance("salt"), support::amount(salt)),
            ],
        )
        .expect("stock vector")
    };
    let stocks = InitialStocks::new(
        &topology,
        base.registry(),
        [
            (support::compartment("store"), stock(1.0, 4.0)),
            (
                support::compartment("route"),
                stock(9_007_199_254_740_991.0, 0.0),
            ),
        ],
    )
    .expect("initial stocks");
    let artifact = ModelArtifact::builder(
        topology,
        base.registry().clone(),
        stocks,
        base.calendar(),
        base.horizon(),
    )
    .with_projections(base.projections().clone())
    .with_units(
        ["water", "salt"]
            .into_iter()
            .map(|name| {
                (
                    support::substance(name),
                    SubstanceUnit::new(
                        UnitId::parse("m3").expect("unit"),
                        Quantum::try_from(1.0).expect("quantum"),
                    ),
                )
            })
            .collect(),
    )
    .build()
    .expect("ceiling fixture");
    // A valid credited prefix can bring finite totals above the initial-stock ceiling;
    // the signed supply account balances it. Only an individual endpoint may not overflow.
    let mut log = AuthoritativeLog::for_run(RunId::from_bytes([4; 16]), &artifact);
    log.append(
        Transfer::new(
            TimestepIndex::new(0),
            support::endpoint(&artifact, "supply"),
            support::endpoint(&artifact, "store"),
            artifact.registry(),
            [(
                support::substance("water"),
                QuantumAmount::new(
                    Quantum::try_from(1.0).expect("quantum"),
                    QuantumCount::try_from(9).expect("count"),
                )
                .expect("amount"),
            )],
        )
        .expect("credit"),
    )
    .expect("append credit");
    let state = replay_with_artifact(&log, &artifact)
        .expect("valid credited prefix")
        .final_state()
        .clone();
    let mut run = CurrentRun {
        artifact: &artifact,
        log,
        state,
    };
    let before_state = run.state().clone();
    let before_log = run.log().canonical_bytes();
    // Outside receives one, route receives one, then the third transfer overflows route.
    let disposition = Disposition::new(
        TimestepIndex::new(1),
        support::finite(&artifact, "store"),
        [
            SubstanceDisposition::new(
                support::substance("water"),
                support::amount(7.0),
                ["outside", "route", "route"].into_iter().map(|target| {
                    Allocation::new(support::endpoint(&artifact, target), support::amount(1.0))
                }),
            ),
            SubstanceDisposition::new(support::substance("salt"), support::amount(4.0), []),
        ],
    );
    let error = run
        .commit(disposition.clone())
        .expect_err("third transfer overflows");
    assert!(matches!(error, TransactionError::Replay {
        source: ReplayError::NonFiniteFold { ref compartment, .. }
    } if *compartment == support::compartment("route")));
    assert_eq!(run.state(), &before_state);
    assert_eq!(run.log().canonical_bytes(), before_log);
    let mut external = run.log().clone();
    assert_eq!(
        commit_disposition(&mut external, &artifact, disposition),
        Err(error)
    );
    assert_eq!(external.canonical_bytes(), before_log);
    run.commit(partition(&artifact, 1, 9.0, 4.0, 1.0))
        .expect("valid retry up to ceiling");
    assert_eq!(
        run.state(),
        replay_with_artifact(run.log(), &artifact)
            .expect("replay")
            .final_state()
    );
}

#[test]
fn append_batch_checks_late_date_and_seal_before_any_mutation() {
    let artifact = support::fixture();
    let mut log = AuthoritativeLog::for_run(RunId::from_bytes([5; 16]), &artifact);
    let transfer = |day| {
        Transfer::new(
            TimestepIndex::new(day),
            support::endpoint(&artifact, "store"),
            support::endpoint(&artifact, "route"),
            artifact.registry(),
            [(
                support::substance("water"),
                QuantumAmount::new(
                    artifact
                        .quantum(&support::substance("water"))
                        .expect("quantum"),
                    QuantumCount::try_from(1).expect("count"),
                )
                .expect("amount"),
            )],
        )
        .expect("transfer")
    };
    let before = log.clone();
    assert!(log.append_batch(vec![transfer(1), transfer(0)]).is_err());
    assert_eq!(log, before);
    log.append_batch(vec![transfer(0), transfer(1)])
        .expect("ordered batch");
    log.seal(artifact.horizon().last()).expect("seal");
    let sealed = log.clone();
    assert!(log.append_batch(vec![transfer(2)]).is_err());
    assert_eq!(log, sealed);
}
