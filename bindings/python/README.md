# incidence Python binding

The binding accepts a complete plain-data model document and delegates all model validation to
`incidence-core`. It does not duplicate domain validation in Python.

```python
from incidence import compile_model

model = compile_model(document)  # `document` is a plain Python mapping
run = model.run(bytes.fromhex("000102030405060708090a0b0c0d0e0f"))
release = run.transfer_series("reservoir", "water", direction="outgoing")
assert len(release.timesteps) == len(release.values) == len(release.presence)
```

## Public entry points

- `compile_model(document)` decodes a plain-data Python value through Serde into the public Rust
  `ModelDocument`, then validates it into an opaque `CompiledModel`.
- `CompiledModel` holds the validated core artifact. `CompiledModel.run(run_id)` executes it and
  returns an opaque sealed `CompletedRun`. The overload
  `run(run_id, substitutions=[{"compartment": ..., "substance": ..., "parameter": ..., "value": ...}])`
  derives a run artifact by changing only declared scalar rule parameters. The held model remains
  immutable. Any other target is refused as not substitutable.
- `CompiledModel.model_digest` names the held artifact. `CompletedRun.model_digest` names the exact
  derived artifact used for that run. `CompletedRun.replay_against(other)` uses authoritative core
  replay and rejects cross-artifact logs with the model digest mismatch.
- `CompletedRun.transfer_series(...)` returns a `PresenceSeries`. The result carries timestep,
  value, and presence arrays of equal length. Dry modelled timesteps contain `0.0` with
  `"present"`; positions outside the horizon contain `None` with `"absent"`; substances outside
  the model registry contain `None` with `"not_modelled"`.
- `CompletedRun.transfer_count_series(...)` applies the same selector, range, and presence
  semantics but returns authoritative whole-quantum Python integers. It folds stored transfer
  counts directly and never decodes them from projected floating-point amounts.
- `CompletedRun.authoritative_log()` returns the authoritative log's canonical bytes and its
  lowercase hexadecimal digest as a tuple. The bytes are the exact payload authenticated by the
  core digest.

All Rust panics in exported operations are contained at this module boundary and converted to
`RuntimeError`. Decode and core validation failures are `ValueError`, so callers can safely
isolate bad models with `except Exception`.

## Reproducible development build

From `bindings/python`, the package driver can be run repeatedly without changing the command:

```console
uv venv --allow-existing --quiet
uv sync --quiet
uv run --no-sync maturin develop --uv -q
uv run --no-sync pytest tests/test_import.py -q
uv run --no-sync pytest tests/test_panic_containment.py -q
```

The project requires CPython 3.12 or newer. `uv.lock` pins the build and test tools.

## Plain-data authoring

Expression helpers such as `literal`, `param`, `input`, `forcing`, `projection`, `add`,
`mul`, `power`, `min`, `max`, `clamp`, `compare`, `select`, and `table_lookup` call the Rust IR
constructors and return only JSON-serialisable dictionaries. `model_document`, `rule`,
`retain_all`, and `release_all` assemble the surrounding document shape without duplicating
domain validation. `compile_model` remains the single validation entry point.


## Realised-carrier partitions

A dependent substance can explicitly follow an ordinary carrier partition in the same finite
compartment. For example, use this disposition on a rule for `tracer`:

```python
disposition = {
    "rule_ir_version": "v1",
    "numerical_semantics_version": "v1",
    "partition": {
        "kind": "carrier_proportional",
        "carrier": "carrier",
        "branches": [
            {"branch": "tracer-out", "carrier_branch": "carrier-out"},
        ],
    },
}
```

The carrier rule must declare `carrier-out`. The dependent rule must bind `tracer-out` to the same
destination. Both substances need declared quanta. This disposition uses the complete available
dependent stock, not the scalar `expression` field of its surrounding rule; use `literal(0)` for
that unused field. Independent partitions keep their existing semantics and do not imply coupling.

For available dependent count `D`, available carrier count `C`, and realised carrier branch count
`B`, the dependent branch receives `floor(D * B / C)` counts. The product and division use integer
arithmetic. Both available counts come from the same pre-withdrawal stock, including initial stock
and incoming transfers already committed in topology order. If `C` is zero, all dependent stock is
retained. Any quantisation remainder also remains at the source, including when all carrier stock
leaves. Modelled zero does not mean unmodelled substance or unknown physical composition.

A mapping may select only a subset of carrier branches. Unmapped carrier branches transport none
of that dependent substance. Duplicate dependent branches, repeated carrier mappings, mismatched
destinations, self-reference, missing carriers and carriers that are themselves dependent are
refused. Multiple independent dependents may share one carrier. Ordinary carrier plans are
validated first; all substance dispositions are then committed in one atomic transaction.

The operation and its canonical mappings participate in model identity. Identical models and run
IDs reproduce identical authoritative logs. Temporal delay still requires an explicit projection
such as `bounded_lag` and a finite inventory compartment; an extra topology node alone is not a
delay. Physical mixing, process order, supported remobilisation and interpretation belong to the
caller, not to this substance-neutral engine.

`transfer_count_series` reads exact destination or source totals by substance and timestep. When
several branches share a destination their counts are summed. It does not identify named branches,
or separate multiple senders in a receiver total. `authoritative_log` exposes authenticated opaque
bytes, not a Python exchange-table or restart API. Native prefix continuation remains available
through the Rust core. Saved result data should not be described as a restart checkpoint.

## Naive sweep baseline

`benchmarks/sweep_baseline.py` is the reproducible full-document baseline for parameter sweeps.
Every trial authors and submits a fresh 50-compartment document containing 2,191 daily steps and
20 forcing series. It then calls `compile_model`, runs the model, and reads a
presence-carrying result. The default is 1,000 trials. The committed
`benchmarks/sweep-baseline-v1.json` records phase timings, wall time, machine identity, and Python,
Rust, uv, and maturin versions. Its versioned schema and `load_record` function are the input for
the held-model comparison.

Build the release extension before recording. Use more than one worker only when the held-model
comparison will use the same worker count:

```console
uv run --no-sync maturin develop --uv --release -q
PYTHONPATH=. uv run --no-sync python -m benchmarks.sweep_baseline --workers 12
```

The normal pytest target validates the committed 1,000-trial record and performs a small
end-to-end harness probe; it does not repeat the long measurement.

## Held-model sweep comparison

`benchmarks/sweep_held_model.py` compiles the IPB6 basin document once, then sends only a typed
parameter record and run id for each trial. It records one full-document decode-and-validate operation and zero
forcing values crossing the boundary after compilation. Execution releases the GIL so its 12
threads match the committed baseline's worker count while sharing one held Rust model.
`benchmarks/sweep-held-model-v1.json` reports both the IPB6 baseline figures and the held-model
figures on the same machine. The recorded held sweep removes repeated document decoding but is
slower overall on that machine: 2,014.48 seconds versus the naive baseline's 1,182.21 seconds.
The record retains both wall-time and phase totals rather than presenting flat decode work as a
total-speed claim.

```console
uv run --no-sync maturin develop --uv --release -q
PYTHONPATH=. uv run --no-sync python -m benchmarks.sweep_held_model --trials 1000 --workers 12
```
