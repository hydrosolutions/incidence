# Realised-carrier delivery evidence

Effort: https://github.com/hydrosolutions/taqsim/issues/22

## Chosen mechanism

`carrier_proportional` is a declared count-native partition within the existing interpreter. It
allocates `floor(dependent_available * realised_carrier_branch / carrier_available)` with `u128`
intermediates, retains the remainder, and commits all substances through the existing atomic
`Disposition`. Zero available carrier retains every dependent count. Subset mappings permit
carrier-only routes. Carriers must use ordinary partitions in the same compartment; chaining,
self-reference, duplicate mappings and different destinations are rejected.

The mechanism comparison and preimplementation crosswalk are in
[carrier-mechanism-investigation.md](carrier-mechanism-investigation.md). Its recorded failures
are historical evidence, not the delivered runtime status. Staged execution proved complete
mixing and temporal recurrence at small counts, but scalar reinjection loses a supported exact
count. Hiding provenance in an unused forcing identity was tested and rejected. The chosen
operation requires neither a new log schema nor a staged count-injection protocol.

## Regression provenance

- Commit `8fdaee5` records the actual independent-partition counterexample and sparse
  multi-substance projection failure before production edits. Water `[0,0]`, mass `[5,5]` fails
  the named desired-coupling assertion. Independent partitions are not reinterpreted.
- Commit `e3f8cbd` changes that test's input to explicitly declare `carrier_proportional` while
  retaining the same named test and zero-carrier/zero-mass assertion. The old runtime refuses
  this unknown variant. The independent behaviour is separately pinned as compatible.
- The implementation makes that unchanged explicitly declared-operation test and the unchanged
  multi-substance lag regression pass. Exact original outputs are retained in
  `carrier-regression-red.txt`, `carrier-experiments-final.txt` and `carrier-declaration-red.txt`.

## Public and native evidence

The Python tests execute `compile_model`, `run`, exact `transfer_count_series`, canonical log
readback and replay. They cover zero/non-round transfers, initial+incoming complete mixing,
three-interval retained recurrence, multiple constituents, same-destination branch aggregation,
explicit subsets, dry quantisation remainder, integer products near the count ceiling, deterministic
identity, and the repaired sparse multi-substance lag path.

Native tests additionally inspect exact retained counts and replay every transfer prefix to the
identical completed log. Existing canonical encoding golden tests and the hydrology fixture's
pinned authoritative-log digest remain unchanged and pass. Old independent partition semantics
and document identities are preserved.

## Boundaries

- The operation is substance-neutral. Physical mixing selection, process sequencing, dry-mass
  remobilisation, quality supportedness and physical boundary accounting remain Taqsim-owned.
- Preloaded future supply stock is an engine account, not current physical inventory. Boundary
  witnesses must exclude it until physical entry. No new Taqsim constituent facade is provided.
- Public count series aggregate by selected compartment/substance/time/direction. Same-destination
  branches are summed. They do not identify named branches or separate senders at a common
  receiver. No opaque-byte parser or unsupported branch-attribution claim is introduced.
- Native prefix continuation remains valid. No Python saved-result or restart API is added.
- No private source document is included. All fixtures are synthetic.

## Validation

Final commands all passed: `cargo fmt --all --check`, `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets`, `cargo test --workspace` (256 tests), binding rebuild,
and `uv run --no-sync pytest tests -q` (48 tests). Native carrier tests account for 15 of the
Rust tests; public carrier experiments account for 16 of the Python tests. Full outputs are
recorded in `carrier-validation.txt`.

Known pre-existing clippy warnings remain in unrelated initial-stock, expression, sync-doctrine
and older test code. The standard workspace clippy command succeeds; no naming linter or unrelated
cleanup is added. The full final diff requires independent review before merge.
