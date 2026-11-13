#![allow(clippy::expect_used)]

use incidence_core::dense_projection::{DenseProjectionError, DenseTransferProjection};
use incidence_core::endpoints::{BoundaryAccount, FiniteCompartment};
use incidence_core::identity::{CompartmentId, SubstanceId};
use incidence_core::initial_stocks::InitialStocks;
use incidence_core::ledger::{AuthoritativeLog, QuantumAmount, QuantumCount, RunId, Transfer};
use incidence_core::model_artifact::{ModelArtifact, Quantum, SubstanceUnit, UnitId};
use incidence_core::non_negative_amount::NonNegativeAmount;
use incidence_core::presence::ValueState;
use incidence_core::projection::{AuthoritativeFactSelector, ProjectionSet};
use incidence_core::sparse_substance_vector::SparseSubstanceVector;
use incidence_core::substance_registry::SubstanceRegistry;
use incidence_core::temporal::{
    CalendarInstant, CalendarOrigin, FixedStepCalendar, RunHorizon, TimestepDuration, TimestepIndex,
};
use incidence_core::topology::{DirectedConnection, Topology, TopologyEndpoint};

fn compartment(value: &str) -> CompartmentId {
    CompartmentId::parse(value).expect("fixture compartment")
}

fn fixture() -> ModelArtifact {
    let upstream = compartment("upstream");
    let store = compartment("store");
    let downstream = compartment("downstream");
    let topology = Topology::new(
        [
            TopologyEndpoint::Boundary(BoundaryAccount::new(upstream.clone())),
            TopologyEndpoint::Finite(FiniteCompartment::new(store.clone())),
            TopologyEndpoint::Boundary(BoundaryAccount::new(downstream.clone())),
        ],
        [
            DirectedConnection::new(upstream, store.clone()),
            DirectedConnection::new(store.clone(), downstream),
        ],
    )
    .expect("fixture topology");
    let water = SubstanceId::parse("water").expect("fixture substance");
    let registry = SubstanceRegistry::new([water.clone()]).expect("fixture registry");
    let initial = SparseSubstanceVector::new(
        &registry,
        [(
            water.clone(),
            NonNegativeAmount::try_from(5.0).expect("amount"),
        )],
    )
    .expect("initial vector");
    let stocks = InitialStocks::new(&topology, &registry, [(store, initial)]).expect("stocks");
    let calendar = FixedStepCalendar::new(
        CalendarOrigin::new(CalendarInstant::from_unix_seconds(0)),
        TimestepDuration::from_seconds(86_400).expect("duration"),
    );
    let horizon = RunHorizon::new(TimestepIndex::new(10), TimestepIndex::new(59)).expect("horizon");
    ModelArtifact::builder(topology, registry, stocks, calendar, horizon)
        .with_projections(ProjectionSet::new(vec![], vec![]).expect("projection set"))
        .with_units(vec![(
            water,
            SubstanceUnit::new(
                UnitId::parse("m3").expect("unit"),
                Quantum::try_from(1.0e-6).expect("valid quantum"),
            ),
        )])
        .build()
        .expect("artifact")
}

fn endpoint(artifact: &ModelArtifact, id: &str) -> TopologyEndpoint {
    artifact
        .topology()
        .endpoint(&compartment(id))
        .expect("fixture endpoint")
        .clone()
}

#[test]
fn dense_projection_preserves_a_forty_step_dry_stretch() {
    let artifact = fixture();
    let water = SubstanceId::parse("water").expect("fixture substance");
    let _amount = SparseSubstanceVector::new(
        artifact.registry(),
        [(
            water.clone(),
            NonNegativeAmount::try_from(1.0).expect("amount"),
        )],
    )
    .expect("transfer vector");
    let mut log = AuthoritativeLog::for_run(RunId::from_bytes([5; 16]), &artifact);
    for timestep in [14, 55] {
        log.append(
            Transfer::new(
                TimestepIndex::new(timestep),
                endpoint(&artifact, "store"),
                endpoint(&artifact, "downstream"),
                artifact.registry(),
                [(
                    water.clone(),
                    QuantumAmount::new(
                        artifact.quantum(&water).expect("quantum"),
                        QuantumCount::try_from(1000000).expect("count"),
                    )
                    .expect("projection"),
                )],
            )
            .expect("count-bound transfer"),
        )
        .expect("append transfer");
    }
    log.seal(artifact.horizon().last()).expect("seal log");

    let history = DenseTransferProjection::from_log_with_artifact(
        &log,
        &artifact,
        AuthoritativeFactSelector::OutgoingTransferAmount {
            compartment: compartment("store"),
            substance: water,
        },
    )
    .expect("dense projection");

    assert_eq!(history.iter().len(), 50);
    for timestep in 15..55 {
        assert_eq!(
            history.value_at(TimestepIndex::new(timestep)),
            ValueState::Present(NonNegativeAmount::ZERO)
        );
    }
    assert_eq!(
        history.value_at(TimestepIndex::new(14)),
        ValueState::Present(NonNegativeAmount::try_from(1.0).expect("amount"))
    );
    assert_eq!(history.value_at(TimestepIndex::new(9)), ValueState::Absent);
    assert_eq!(history.value_at(TimestepIndex::new(60)), ValueState::Absent);
}

#[test]
fn an_unsealed_prefix_cannot_claim_that_a_dry_suffix_is_zero() {
    let artifact = fixture();
    let log = AuthoritativeLog::for_run(RunId::from_bytes([6; 16]), &artifact);
    let error = DenseTransferProjection::from_log_with_artifact(
        &log,
        &artifact,
        AuthoritativeFactSelector::OutgoingTransferAmount {
            compartment: compartment("store"),
            substance: SubstanceId::parse("water").expect("fixture substance"),
        },
    )
    .expect_err("prefix must not become a dense complete history");
    assert_eq!(error, DenseProjectionError::IncompleteRun);
}

#[test]
fn validated_log_matches_raw_projections_and_keeps_immutable_prefix_identity() {
    use incidence_core::dense_projection::{DenseTransferCountProjection, ValidatedTransferLog};
    use std::sync::Arc;

    let artifact = Arc::new(fixture());
    let water = SubstanceId::parse("water").expect("water");
    let mut log = Arc::new(AuthoritativeLog::for_run(
        RunId::from_bytes([6; 16]),
        &artifact,
    ));
    let prefix = ValidatedTransferLog::new(log.clone(), artifact.clone()).expect("valid prefix");
    let selector = AuthoritativeFactSelector::OutgoingTransferAmount {
        compartment: compartment("store"),
        substance: water.clone(),
    };
    assert!(matches!(
        DenseTransferProjection::from_validated_log(&prefix, selector.clone()),
        Err(DenseProjectionError::IncompleteRun)
    ));
    Arc::make_mut(&mut log)
        .append(
            Transfer::new(
                TimestepIndex::new(14),
                endpoint(&artifact, "store"),
                endpoint(&artifact, "downstream"),
                artifact.registry(),
                [(
                    water.clone(),
                    QuantumAmount::new(
                        artifact.quantum(&water).expect("quantum"),
                        QuantumCount::try_from(1_000_003).expect("count"),
                    )
                    .expect("amount"),
                )],
            )
            .expect("transfer"),
        )
        .expect("append");
    Arc::make_mut(&mut log)
        .seal(artifact.horizon().last())
        .expect("seal");
    // Mutating the caller's copy cannot upgrade the old immutable prefix proof.
    assert!(matches!(
        DenseTransferCountProjection::from_validated_log(&prefix, selector.clone()),
        Err(DenseProjectionError::IncompleteRun)
    ));
    let validated =
        ValidatedTransferLog::new(log.clone(), artifact.clone()).expect("completed proof");
    for name in ["store", "downstream", "upstream", "ghost"] {
        for substance in ["water", "unmodelled"] {
            for incoming in [false, true] {
                let compartment = compartment(name);
                let substance = SubstanceId::parse(substance).expect("substance");
                let selector = if incoming {
                    AuthoritativeFactSelector::IncomingTransferAmount {
                        compartment,
                        substance,
                    }
                } else {
                    AuthoritativeFactSelector::OutgoingTransferAmount {
                        compartment,
                        substance,
                    }
                };
                assert_eq!(
                    DenseTransferProjection::from_validated_log(&validated, selector.clone()),
                    DenseTransferProjection::from_log_with_artifact(
                        &log,
                        &artifact,
                        selector.clone()
                    )
                );
                assert_eq!(
                    DenseTransferCountProjection::from_validated_log(&validated, selector.clone()),
                    DenseTransferCountProjection::from_log_with_artifact(&log, &artifact, selector)
                );
            }
        }
    }
}

#[test]
fn validated_log_refuses_wrong_artifact_and_overdraw() {
    use incidence_core::dense_projection::ValidatedTransferLog;
    use std::sync::Arc;

    let artifact = Arc::new(fixture());
    let water = SubstanceId::parse("water").expect("water");
    let log = AuthoritativeLog::for_run(RunId::from_bytes([7; 16]), &artifact);
    let different = ModelArtifact::builder(
        artifact.topology().clone(),
        artifact.registry().clone(),
        artifact.initial_stocks().clone(),
        artifact.calendar(),
        RunHorizon::new(TimestepIndex::new(10), TimestepIndex::new(60)).expect("horizon"),
    )
    .with_projections(ProjectionSet::new(vec![], vec![]).expect("projection set"))
    .with_units(vec![(
        water.clone(),
        SubstanceUnit::new(
            UnitId::parse("m3").expect("unit"),
            Quantum::try_from(1.0e-6).expect("quantum"),
        ),
    )])
    .build()
    .expect("different artifact");
    assert!(matches!(
        ValidatedTransferLog::new(Arc::new(log.clone()), Arc::new(different)),
        Err(DenseProjectionError::Replay { .. })
    ));
    let mut overdraw = log;
    overdraw
        .append(
            Transfer::new(
                TimestepIndex::new(14),
                endpoint(&artifact, "store"),
                endpoint(&artifact, "downstream"),
                artifact.registry(),
                [(
                    water.clone(),
                    QuantumAmount::new(
                        artifact.quantum(&water).expect("quantum"),
                        QuantumCount::try_from(6_000_000).expect("count"),
                    )
                    .expect("amount"),
                )],
            )
            .expect("transfer"),
        )
        .expect("structurally valid append");
    assert!(matches!(
        ValidatedTransferLog::new(Arc::new(overdraw), artifact),
        Err(DenseProjectionError::Replay { .. })
    ));
}
