//! Cached execution must equal the history-scanning interpreter, including provisional days.

#[path = "../tests/support/hydrology.rs"]
mod hydrology;

use std::cell::RefCell;

use crate::disposition::{Allocation, CurrentRun, Disposition, SubstanceDisposition};
use crate::execution::{ExecutionCache, RuleInterpreter, StepExecutor};
use crate::forcing::ForcingSeries;
use crate::identity::{CompartmentId, SubstanceId};
use crate::ledger::{RunId, replay_with_artifact};
use crate::model_artifact::ModelArtifact;
use crate::model_document::{ConnectionDocument, InitialStockDocument, SubstanceAmountDocument};
use crate::non_negative_amount::NonNegativeAmount;
use crate::temporal::{RunHorizon, TimestepIndex};
use crate::topology::TopologyEndpoint;

fn long_artifact(days: u64) -> ModelArtifact {
    let mut doc = hydrology::fixture();
    doc.horizon.last = days - 1;
    let horizon =
        RunHorizon::new(TimestepIndex::new(0), TimestepIndex::new(days - 1)).expect("horizon");
    doc.forcings = doc
        .forcings
        .iter()
        .map(|series| {
            let values: Vec<_> = series.values().collect();
            ForcingSeries::new(
                series.id().clone(),
                horizon,
                (0..days as usize)
                    .map(|i| values[i % values.len()])
                    .collect(),
            )
            .expect("forcing")
        })
        .collect();
    for stock in &mut doc.initial_stocks {
        stock.amounts[0].amount = 1_000_000.0;
    }
    doc.artifact().expect("long artifact")
}

#[test]
fn cached_execution_matches_uncached_canonical_bytes_and_independent_replay() {
    for days in [3, 17, 90] {
        let artifact = long_artifact(days);
        let cache = RefCell::new(ExecutionCache::default());
        let run_id = RunId::from_bytes([42; 16]);
        let cached =
            StepExecutor::generate_with_cache(&artifact, run_id, Some(&cache)).expect("cached");
        let reference =
            StepExecutor::generate_with_cache(&artifact, run_id, None).expect("reference");
        assert_eq!(cached.canonical_bytes(), reference.canonical_bytes());
        assert_eq!(
            replay_with_artifact(&cached, &artifact)
                .expect("cached replay")
                .final_state(),
            replay_with_artifact(&reference, &artifact)
                .expect("reference replay")
                .final_state()
        );
        // Two finite recurrences; each past day can need a final recomputation after its last
        // commit, but must never rebuild the whole run. This fails the old triangular replay.
        assert!(cache.borrow().recurrence_updates <= 4 * days as usize);
    }
}

#[test]
fn duration_doubling_has_linear_recurrence_work_and_fact_visits() {
    let mut measurements = Vec::new();
    for days in [90, 180, 360] {
        let artifact = long_artifact(days);
        let cache = RefCell::new(ExecutionCache::default());
        StepExecutor::generate_with_cache(&artifact, RunId::from_bytes([43; 16]), Some(&cache))
            .expect("run");
        let cache = cache.into_inner();
        measurements.push((cache.recurrence_updates, cache.fact_visits));
    }
    for pair in measurements.windows(2) {
        assert!(
            pair[1].0 <= 2 * pair[0].0 + 4,
            "recurrence work: {measurements:?}"
        );
        assert!(
            pair[1].1 <= 2 * pair[0].1 + 100,
            "fact visits: {measurements:?}"
        );
    }
}

#[test]
fn same_day_commit_and_final_historical_read_match_uncached_projection() {
    let mut doc = hydrology::fixture();
    doc.finite_compartments.push("upstream".into());
    doc.connections.push(ConnectionDocument {
        source: "upstream".into(),
        target: "muskingum".into(),
    });
    doc.initial_stocks.push(InitialStockDocument {
        compartment: "upstream".into(),
        amounts: vec![SubstanceAmountDocument {
            substance: "water".into(),
            amount: 20.0,
        }],
    });
    let artifact = doc.artifact().expect("artifact");
    let mut run = CurrentRun::new(&artifact, RunId::from_bytes([44; 16])).expect("run");
    let cache = RefCell::new(ExecutionCache::default());
    let rule = artifact
        .rules()
        .find(|r| r.compartment().as_str() == "muskingum")
        .expect("rule");
    let day = TimestepIndex::new(0);
    let mut before = RuleInterpreter::new(&artifact, run.log(), rule, day);
    before.cache = Some(&cache);
    let before_value = before.evaluate().expect("before");
    let upstream = CompartmentId::parse("upstream").expect("id");
    let owner = match artifact.topology().endpoint(&upstream).expect("endpoint") {
        TopologyEndpoint::Finite(finite) => finite.clone(),
        TopologyEndpoint::Boundary(_) => panic!("finite"),
    };
    let target = artifact
        .topology()
        .endpoint(rule.compartment())
        .expect("target")
        .clone();
    // A rejected commit cannot publish transfers or invalidate/rewrite the cache. Retrying
    // a valid disposition must yield the same projection as a clean successful commit.
    let original_bytes = run.log().canonical_bytes();
    let original_ranges = cache.borrow().transfer_ranges.clone();
    let original_states = cache.borrow().recurrences.clone();
    let invalid = Disposition::new(
        day,
        owner.clone(),
        [SubstanceDisposition::new(
            SubstanceId::parse("water").expect("water"),
            NonNegativeAmount::try_from(10.0).expect("retained"),
            [Allocation::new(
                target.clone(),
                NonNegativeAmount::try_from(30.0).expect("excess release"),
            )],
        )],
    );
    assert!(run.commit(invalid).is_err());
    assert_eq!(run.log().canonical_bytes(), original_bytes);
    assert_eq!(cache.borrow().transfer_ranges, original_ranges);
    assert_eq!(cache.borrow().recurrences, original_states);
    run.commit(Disposition::new(
        day,
        owner,
        [SubstanceDisposition::new(
            SubstanceId::parse("water").expect("water"),
            NonNegativeAmount::try_from(10.0).expect("retained"),
            [Allocation::new(
                target,
                NonNegativeAmount::try_from(10.0).expect("release"),
            )],
        )],
    ))
    .expect("commit");
    cache.borrow_mut().committed(run.log(), day);
    for ordinal in [0, 1, 0, 2] {
        let day = TimestepIndex::new(ordinal);
        let mut cached = RuleInterpreter::new(&artifact, run.log(), rule, day);
        cached.cache = Some(&cache);
        let actual = cached.evaluate().expect("cached projection");
        let expected = RuleInterpreter::new(&artifact, run.log(), rule, day)
            .evaluate()
            .expect("uncached projection");
        assert_eq!(actual.to_bits(), expected.to_bits());
        if ordinal == 0 {
            assert_ne!(actual.to_bits(), before_value.to_bits());
        }
    }
}

#[test]
fn shared_projection_keeps_rule_parameter_context_separate() {
    let mut doc = hydrology::fixture();
    let mut other = doc
        .rules
        .iter()
        .find(|r| r.compartment == "linear-reservoir")
        .expect("rule")
        .clone();
    other.compartment = "other-reservoir".into();
    other.parameters[0].value = 0.5;
    doc.finite_compartments.push(other.compartment.clone());
    doc.connections.push(ConnectionDocument {
        source: other.compartment.clone(),
        target: "outside".into(),
    });
    let mut binding = doc
        .transfer_bindings
        .iter()
        .find(|b| b.compartment == "linear-reservoir")
        .expect("binding")
        .clone();
    binding.compartment = other.compartment.clone();
    doc.transfer_bindings.push(binding);
    doc.initial_stocks.push(InitialStockDocument {
        compartment: other.compartment.clone(),
        amounts: vec![SubstanceAmountDocument {
            substance: "water".into(),
            amount: 100.0,
        }],
    });
    doc.rules.push(other);
    let artifact = doc.artifact().expect("artifact");
    let id = RunId::from_bytes([45; 16]);
    let cached = StepExecutor::run(&artifact, id).expect("cached");
    let reference = StepExecutor::generate_with_cache(&artifact, id, None).expect("reference");
    assert_eq!(cached.canonical_bytes(), reference.canonical_bytes());
}

#[test]
fn nonzero_horizon_and_before_horizon_recurrence_match_reference() {
    let mut doc = hydrology::fixture();
    doc.horizon.first = 7;
    doc.horizon.last = 9;
    let horizon = RunHorizon::new(TimestepIndex::new(7), TimestepIndex::new(9)).expect("horizon");
    doc.forcings = doc
        .forcings
        .iter()
        .map(|series| {
            ForcingSeries::new(series.id().clone(), horizon, series.values().collect())
                .expect("forcing")
        })
        .collect();
    let artifact = doc.artifact().expect("artifact");
    let run_id = RunId::from_bytes([46; 16]);
    let cached = StepExecutor::run(&artifact, run_id).expect("cached");
    let reference = StepExecutor::generate_with_cache(&artifact, run_id, None).expect("reference");
    assert_eq!(cached.canonical_bytes(), reference.canonical_bytes());
    let cache = RefCell::new(ExecutionCache::default());
    // The public interpreter's existing pre-horizon finite-recurrence value is its initial
    // output state. Do not turn a missing historical transfer into a fabricated observation.
    let rule = artifact
        .rules()
        .find(|r| r.compartment().as_str() == "linear-reservoir")
        .expect("rule");
    for ordinal in [6, 0, 6] {
        let day = TimestepIndex::new(ordinal);
        let mut interpreter = RuleInterpreter::new(&artifact, &cached, rule, day);
        interpreter.cache = Some(&cache);
        let expected = RuleInterpreter::new(&artifact, &cached, rule, day)
            .evaluate()
            .expect("reference");
        assert_eq!(
            interpreter.evaluate().expect("cached").to_bits(),
            expected.to_bits()
        );
        assert_eq!(expected, 0.0);
    }
}
