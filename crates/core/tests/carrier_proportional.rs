//! carrier_proportional_tests : ModelDocument → ExactCountExecutionAndReplayAssertions
#![allow(clippy::expect_used)]

use incidence_core::execution::{ExecutionError, execute_model, resume_from_prefix};
use incidence_core::identity::{CompartmentId, SubstanceId};
use incidence_core::ledger::{
    AuthoritativeLog, QuantumCount, Record, ReplayError, RunId, replay_with_artifact,
};
use incidence_core::model_artifact::ModelArtifactError;
use incidence_core::model_document::ModelDocumentError;
use incidence_core::model_document::{
    CalendarDocument, ConnectionDocument, HorizonDocument, InitialStockDocument, ModelDocument,
    ModelDocumentVersion, ModelVersionsDocument, RuleDocument, SubstanceAmountDocument,
    TransferBindingDocument, UnitDocument,
};
use incidence_core::numerical_semantics::NumericalSemanticsVersion;
use incidence_core::partition_expression::{
    CarrierBranch, ExpressionBranch, PartitionExpr, PartitionExprError,
};
use incidence_core::projection::ProjectionSet;
use incidence_core::rule_expression::RuleExpr;
use incidence_core::rule_reference::TransferBranchId;
use incidence_core::versions::RuleIrVersion;

const R: RuleIrVersion = RuleIrVersion::V1;
const S: NumericalSemanticsVersion = NumericalSemanticsVersion::V1;

fn substance(name: &str) -> SubstanceId {
    SubstanceId::parse(name).expect("substance")
}
fn branch(name: &str) -> TransferBranchId {
    TransferBranchId::parse(name).expect("branch")
}
fn literal(value: f64) -> RuleExpr {
    RuleExpr::literal(R, S, value).expect("literal")
}
fn coupled(carrier: &str, names: &[&str]) -> PartitionExpr {
    PartitionExpr::carrier_proportional(
        R,
        S,
        substance(carrier),
        names
            .iter()
            .map(|name| CarrierBranch::new(branch(name), branch(name)))
            .collect(),
    )
    .expect("carrier partition")
}

fn document(
    carrier: &str,
    water: u64,
    flows: [u64; 2],
    dependents: &[(&str, u64)],
) -> ModelDocument {
    let names = ["left", "right"];
    let mut rules = vec![RuleDocument {
        compartment: "source".into(),
        substance: carrier.into(),
        expression: literal(0.0),
        disposition: PartitionExpr::expression_partition(
            R,
            S,
            names
                .iter()
                .zip(flows)
                .map(|(name, count)| ExpressionBranch::new(branch(name), literal(count as f64)))
                .collect(),
        )
        .expect("ordinary carrier partition"),
        parameters: vec![],
    }];
    rules.extend(dependents.iter().map(|(name, _)| RuleDocument {
        compartment: "source".into(),
        substance: (*name).into(),
        expression: literal(0.0),
        disposition: coupled(carrier, &names),
        parameters: vec![],
    }));
    let stocks: Vec<_> = std::iter::once((carrier, water))
        .chain(dependents.iter().copied())
        .collect();
    ModelDocument {
        document_version: ModelDocumentVersion::V1,
        versions: ModelVersionsDocument::default(),
        finite_compartments: vec!["source".into()],
        boundary_accounts: names.iter().map(|s| (*s).into()).collect(),
        connections: names
            .iter()
            .map(|name| ConnectionDocument {
                source: "source".into(),
                target: (*name).into(),
            })
            .collect(),
        substances: stocks.iter().map(|(name, _)| (*name).into()).collect(),
        initial_stocks: vec![InitialStockDocument {
            compartment: "source".into(),
            amounts: stocks
                .iter()
                .map(|(name, count)| SubstanceAmountDocument {
                    substance: (*name).into(),
                    amount: *count as f64,
                })
                .collect(),
        }],
        calendar: CalendarDocument {
            origin_unix_seconds: 0,
            timestep_seconds: 1,
        },
        horizon: HorizonDocument { first: 0, last: 0 },
        projections: ProjectionSet::new(vec![], vec![]).expect("projections"),
        forcings: vec![],
        interpolation_tables: vec![],
        rules,
        transfer_bindings: stocks
            .iter()
            .flat_map(|(name, _)| {
                names
                    .iter()
                    .map(move |destination| TransferBindingDocument {
                        compartment: "source".into(),
                        substance: (*name).into(),
                        branch: (*destination).into(),
                        destination: (*destination).into(),
                    })
            })
            .collect(),
        input_bindings: vec![],
        units: stocks
            .iter()
            .map(|(name, _)| UnitDocument {
                substance: (*name).into(),
                unit: "count".into(),
                quantum: 1.0,
            })
            .collect(),
    }
}

fn transferred(log: &AuthoritativeLog, name: &str, destination: &str) -> u64 {
    log.transfers()
        .iter()
        .filter(|transfer| transfer.target().id().as_str() == destination)
        .filter_map(|transfer| transfer.quantum_count(&substance(name)))
        .map(QuantumCount::value)
        .sum()
}

fn assert_retained(document: &ModelDocument, log: &AuthoritativeLog, name: &str, count: u64) {
    let artifact = document.artifact().expect("artifact");
    let replay = replay_with_artifact(log, &artifact).expect("replay");
    assert_eq!(
        replay.final_state().finite_quantum_count(
            &CompartmentId::parse("source").expect("source"),
            &substance(name)
        ),
        Some(count)
    );
}

fn assert_artifact_error(doc: &ModelDocument, reason: &str) {
    assert_eq!(
        doc.artifact().expect_err("invalid carrier model"),
        ModelDocumentError {
            component: "artifact",
            reason: reason.into(),
        }
    );
}

#[test]
fn non_round_multiple_dependents_use_available_stock_and_realised_carrier() {
    let doc = document("water", 7, [2, 3], &[("dye", 11), ("salt", 17)]);
    let artifact = doc.artifact().expect("artifact");
    let log = execute_model(&artifact, RunId::from_bytes([22; 16])).expect("execution");
    for (name, initial, expected) in [
        ("water", 7, [2, 3]),
        ("dye", 11, [3, 4]),
        ("salt", 17, [4, 7]),
    ] {
        assert_eq!(transferred(&log, name, "left"), expected[0]);
        assert_eq!(transferred(&log, name, "right"), expected[1]);
        assert_retained(&doc, &log, name, initial - expected[0] - expected[1]);
    }
}

#[test]
fn near_ceiling_multiplication_is_exact_in_u128() {
    let carrier = 9_007_199_254_740_629_u64;
    let dependent = 9_007_199_254_739_724_u64;
    let flow = 5_198_452_529_468_928_u64;
    let expected = ((u128::from(dependent) * u128::from(flow)) / u128::from(carrier)) as u64;
    assert_ne!(
        expected,
        ((dependent as f64) * (flow as f64) / (carrier as f64)) as u64
    );
    let doc = document("water", carrier, [flow, 1], &[("salt", dependent)]);
    let artifact = doc.artifact().expect("artifact");
    let log = execute_model(&artifact, RunId::from_bytes([23; 16])).expect("execution");
    assert_eq!(transferred(&log, "salt", "left"), expected);
    assert_eq!(transferred(&log, "salt", "right"), 0);
    assert_retained(&doc, &log, "salt", dependent - expected);
}

#[test]
fn subset_mapping_retains_unmapped_share() {
    let mut doc = document("water", 7, [2, 3], &[("salt", 11)]);
    doc.rules[1].disposition = coupled("water", &["left"]);
    doc.transfer_bindings
        .retain(|binding| binding.substance != "salt" || binding.branch == "left");
    let artifact = doc.artifact().expect("subset artifact");
    let log = execute_model(&artifact, RunId::from_bytes([24; 16])).expect("execution");
    assert_eq!(transferred(&log, "salt", "left"), 3);
    assert_eq!(transferred(&log, "salt", "right"), 0);
    assert_retained(&doc, &log, "salt", 8);
}

#[test]
fn dry_carrier_retains_all_dependent_stock() {
    let doc = document("water", 0, [0, 0], &[("salt", 11)]);
    let artifact = doc.artifact().expect("dry artifact");
    let log = execute_model(&artifact, RunId::from_bytes([25; 16])).expect("dry execution");
    assert_eq!(transferred(&log, "salt", "left"), 0);
    assert_eq!(transferred(&log, "salt", "right"), 0);
    assert_retained(&doc, &log, "salt", 11);
}

#[test]
fn lexical_carrier_order_does_not_change_counts() {
    for carrier in ["a-carrier", "z-carrier"] {
        let doc = document(carrier, 7, [2, 3], &[("salt", 11)]);
        let artifact = doc.artifact().expect("artifact");
        let log = execute_model(&artifact, RunId::from_bytes([26; 16])).expect("execution");
        assert_eq!(transferred(&log, "salt", "left"), 3);
        assert_eq!(transferred(&log, "salt", "right"), 4);
    }
}

#[test]
fn every_transfer_prefix_resumes_to_identical_completed_log() {
    let doc = document("water", 7, [2, 3], &[("dye", 11), ("salt", 17)]);
    let artifact = doc.artifact().expect("artifact");
    let full = execute_model(&artifact, RunId::from_bytes([27; 16])).expect("execution");
    for length in 0..=full.transfers().len() {
        let records = std::iter::once(Record::Genesis(full.genesis().clone())).chain(
            full.transfers()[..length]
                .iter()
                .cloned()
                .map(Record::Transfer),
        );
        let mut prefix = AuthoritativeLog::from_records(records).expect("prefix");
        resume_from_prefix(&artifact, &mut prefix).expect("resume");
        assert_eq!(prefix.canonical_bytes(), full.canonical_bytes());
        assert_eq!(prefix.digest(), full.digest());
    }
}

#[test]
fn mapping_and_author_order_have_canonical_identity() {
    let original = document("water", 7, [2, 3], &[("salt", 11)]);
    let mut reordered = original.clone();
    reordered.rules[1].disposition = coupled("water", &["right", "left"]);
    reordered.rules.reverse();
    reordered.transfer_bindings.reverse();
    reordered.substances.reverse();
    let encoded = serde_json::to_value(&reordered).expect("encode");
    let partition = &encoded["rules"][0]["disposition"]["partition"];
    assert_eq!(partition["kind"], "carrier_proportional");
    assert_eq!(partition["carrier"], "water");
    assert_eq!(partition["branches"][0]["branch"], "left");
    assert_eq!(partition["branches"][0]["carrier_branch"], "left");
    let decoded: ModelDocument = serde_json::from_value(encoded).expect("decode");
    assert_eq!(
        original.artifact().expect("original").canonical_bytes(),
        decoded.artifact().expect("decoded").canonical_bytes()
    );
}

#[test]
fn duplicate_carrier_branch_is_rejected_at_construction() {
    assert_eq!(
        PartitionExpr::carrier_proportional(
            R,
            S,
            substance("water"),
            vec![
                CarrierBranch::new(branch("left"), branch("left")),
                CarrierBranch::new(branch("right"), branch("left")),
            ]
        ),
        Err(PartitionExprError::DuplicateCarrierBranch {
            branch: branch("left")
        })
    );
}

#[test]
fn self_carrier_and_dependent_carrier_are_rejected() {
    let mut doc = document("water", 7, [2, 3], &[("dye", 11), ("salt", 17)]);
    doc.rules[1].disposition = coupled("dye", &["left", "right"]);
    assert_artifact_error(
        &doc,
        "compartment `source`, substance `dye` cannot be its own carrier",
    );
    doc.rules[1].disposition = coupled("salt", &["left", "right"]);
    assert_artifact_error(
        &doc,
        "carrier `salt` for compartment `source`, substance `dye` is itself dependent",
    );
}

#[test]
fn missing_carrier_branch_and_mismatched_destination_are_rejected() {
    let mut doc = document("water", 7, [2, 3], &[("salt", 11)]);
    doc.rules[1].disposition = PartitionExpr::carrier_proportional(
        R,
        S,
        substance("water"),
        vec![
            CarrierBranch::new(branch("left"), branch("missing")),
            CarrierBranch::new(branch("right"), branch("right")),
        ],
    )
    .expect("locally valid mapping");
    assert_artifact_error(
        &doc,
        "unknown carrier branch `missing` on `water` for compartment `source`, substance `salt`",
    );
    doc.rules[1].disposition = coupled("water", &["left", "right"]);
    doc.transfer_bindings
        .iter_mut()
        .find(|binding| binding.substance == "salt" && binding.branch == "left")
        .expect("salt left binding")
        .destination = "right".into();
    assert_artifact_error(
        &doc,
        "carrier destination `left` differs from dependent destination `right` for compartment `source`, substance `salt`, branch `left`",
    );
}

#[test]
fn unregistered_and_missing_same_compartment_carrier_are_rejected() {
    let mut doc = document("water", 7, [2, 3], &[("salt", 11)]);
    doc.rules[1].disposition = coupled("missing", &["left", "right"]);
    assert_artifact_error(
        &doc,
        "carrier `missing` for compartment `source`, substance `salt` is absent from the registry",
    );
    doc.rules[1].disposition = coupled("water", &["left", "right"]);
    doc.finite_compartments.push("other".into());
    doc.connections
        .extend(["left", "right"].map(|target| ConnectionDocument {
            source: "other".into(),
            target: target.into(),
        }));
    doc.rules[0].compartment = "other".into();
    for binding in &mut doc.transfer_bindings {
        if binding.substance == "water" {
            binding.compartment = "other".into();
        }
    }
    // Keep stock coverage valid so only the missing local carrier fails.
    doc.initial_stocks[0]
        .amounts
        .retain(|entry| entry.substance != "water");
    assert_artifact_error(
        &doc,
        "missing carrier rule `water` for compartment `source`, substance `salt`",
    );
}

#[test]
fn duplicate_dependent_branch_is_rejected_at_construction() {
    assert_eq!(
        PartitionExpr::carrier_proportional(
            R,
            S,
            substance("water"),
            vec![
                CarrierBranch::new(branch("left"), branch("left")),
                CarrierBranch::new(branch("left"), branch("right")),
            ]
        ),
        Err(PartitionExprError::DuplicateBranch {
            branch: branch("left")
        })
    );
}

#[test]
fn carrier_overdraw_is_rejected_before_coupled_transfers() {
    let doc = document("water", 7, [4, 4], &[("salt", 11)]);
    let artifact = doc.artifact().expect("artifact");
    assert!(
        matches!(execute_model(&artifact, RunId::from_bytes([28; 16])),
        Err(ExecutionError::RuleOverdraw { compartment, substance: name, available_bits, requested_bits, .. })
            if compartment.as_str() == "source" && name.as_str() == "water"
            && available_bits == 7_f64.to_bits() && requested_bits == 8_f64.to_bits())
    );
}

#[test]
fn replay_rejects_changed_mapping_or_precision_artifact() {
    let original = document("water", 7, [2, 3], &[("salt", 11)]);
    let artifact = original.artifact().expect("artifact");
    let log = execute_model(&artifact, RunId::from_bytes([29; 16])).expect("execution");
    let mut changed_mapping = original.clone();
    changed_mapping.rules[1].disposition = coupled("water", &["left"]);
    changed_mapping
        .transfer_bindings
        .retain(|binding| binding.substance != "salt" || binding.branch == "left");
    let mut changed_precision = original;
    changed_precision.units[1].quantum = 0.5;
    for doc in [changed_mapping, changed_precision] {
        let different = doc.artifact().expect("different valid artifact");
        assert!(matches!(
            replay_with_artifact(&log, &different),
            Err(ReplayError::ModelDigestMismatch { .. })
        ));
    }
}

#[test]
fn aggregate_initial_stock_one_count_above_ceiling_is_rejected() {
    // The inclusive ceiling is 2^53. Split the excess one across two finite
    // compartments so both authored f64 values remain exactly representable.
    let ceiling = 9_007_199_254_740_992_u64;
    let mut doc = document("water", ceiling, [1, 0], &[("salt", 11)]);
    doc.finite_compartments.push("extra".into());
    doc.initial_stocks.push(InitialStockDocument {
        compartment: "extra".into(),
        amounts: vec![SubstanceAmountDocument {
            substance: "water".into(),
            amount: 1.0,
        }],
    });
    assert_artifact_error(
        &doc,
        &ModelArtifactError::UncountableInitialTotal {
            substance: substance("water"),
            total: ceiling as f64,
            countable_ceiling: ceiling as f64,
        }
        .to_string(),
    );
}
