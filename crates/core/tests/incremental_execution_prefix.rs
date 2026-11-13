//! Prefix recovery must retain deterministic-program checks after the fold optimization.

#[path = "support/hydrology.rs"]
mod hydrology;
#[path = "support/execution.rs"]
mod simple;

use incidence_core::execution::{ExecutionError, execute_model, resume_from_prefix};
use incidence_core::identity::SubstanceId;
use incidence_core::ledger::{
    AuthoritativeLog, QuantumAmount, QuantumCount, Record, RunId, Transfer,
};

#[test]
fn every_hydrology_prefix_including_mid_turn_resumes_to_identical_sealed_records() {
    let artifact = hydrology::fixture().artifact().expect("hydrology model");
    let full = execute_model(&artifact, RunId::from_bytes([7; 16])).expect("run");
    for count in 0..=full.transfers().len() {
        let records = std::iter::once(Record::Genesis(full.genesis().clone())).chain(
            full.transfers()[..count]
                .iter()
                .cloned()
                .map(Record::Transfer),
        );
        let mut prefix = AuthoritativeLog::from_records(records).expect("prefix");
        resume_from_prefix(&artifact, &mut prefix).expect("resume");
        assert_eq!(
            prefix.records().collect::<Vec<_>>(),
            full.records().collect::<Vec<_>>()
        );
    }
}

#[test]
fn balanced_but_wrong_prefix_is_refused_without_mutating_it() {
    let artifact = simple::artifact();
    let full = execute_model(&artifact, RunId::from_bytes([9; 16])).expect("run");
    let first = &full.transfers()[0];
    let water = SubstanceId::parse("water").expect("water");
    let wrong = Transfer::new(
        first.timestep(),
        first.source().clone(),
        first.target().clone(),
        artifact.registry(),
        [(
            water.clone(),
            QuantumAmount::new(
                artifact.quantum(&water).expect("quantum"),
                QuantumCount::try_from(2_000_000).expect("count"),
            )
            .expect("amount"),
        )],
    )
    .expect("transfer");
    let mut prefix = AuthoritativeLog::from_records([
        Record::Genesis(full.genesis().clone()),
        Record::Transfer(wrong),
    ])
    .expect("balanced prefix");
    let before = prefix.clone();
    assert_eq!(
        resume_from_prefix(&artifact, &mut prefix),
        Err(ExecutionError::PrefixDivergence { index: 0 })
    );
    assert_eq!(prefix, before);
}
