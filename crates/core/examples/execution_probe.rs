//! execution_probe : ModelDocument × OutputPath → execution/replay measurements and canonical log
//! Disposable assessment driver; compilation, replay and serialization are timed separately.

use incidence_core::execution::execute_model;
use incidence_core::ledger::{RunId, replay_with_artifact};
use incidence_core::model_document::ModelDocument;
use sha2::{Digest, Sha256};
use std::{env, fs, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 3 {
        return Err("usage: execution_probe model.json output-prefix".into());
    }
    let bytes = fs::read(&args[1])?;
    let document: ModelDocument = serde_json::from_slice(&bytes)?;
    let start = Instant::now();
    let artifact = document.artifact()?;
    let build_seconds = start.elapsed().as_secs_f64();
    let mut execution_seconds = Vec::new();
    let mut expected = None;
    for _ in 0..3 {
        let start = Instant::now();
        let log = execute_model(&artifact, RunId::from_bytes([0; 16]))?;
        execution_seconds.push(start.elapsed().as_secs_f64());
        let canonical = log.canonical_bytes();
        if let Some(previous) = &expected {
            assert_eq!(previous, &canonical, "execution must be deterministic");
        }
        expected = Some(canonical);
        let start = Instant::now();
        let replay = replay_with_artifact(&log, &artifact)?;
        let replay_seconds = start.elapsed().as_secs_f64();
        let state = format!("{:?}", replay.final_state());
        let result = serde_json::json!({
            "model_sha256": format!("{:x}", Sha256::digest(&bytes)),
            "model_digest": artifact.digest().to_hex(),
            "build_seconds": build_seconds,
            "execution_seconds": execution_seconds,
            "replay_seconds": replay_seconds,
            "transfer_count": log.transfer_count(),
            "log_digest": log.digest().to_hex(),
            "state_sha256": format!("{:x}", Sha256::digest(state.as_bytes())),
            "status": format!("{:?}", replay.status()),
            "seal": format!("{:?}", log.records().last()),
        });
        fs::write(
            format!("{}.json", args[2]),
            serde_json::to_vec_pretty(&result)?,
        )?;
        fs::write(format!("{}.log", args[2]), log.canonical_bytes())?;
    }
    Ok(())
}
