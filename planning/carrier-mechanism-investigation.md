# Realised-carrier mechanism investigation

Status: historical investigation recorded before implementation on 2026-09-20. The no-edit/no-repair statements and red results below describe that checkpoint. See [carrier-delivery.md](carrier-delivery.md) for the delivered implementation and final validation.

Effort: https://github.com/hydrosolutions/taqsim/issues/22

## Scope and reproducibility

Checkout created: `/Users/nicolaslazaro/Desktop/work/incidence/.worktrees/visions/effort-22-carrier-experiments`, branch `investigate/realised-carrier`, fetched `origin/main` at `84f707b8f1dbc2e5c3d18121bdfab959d2021f91`. No production changes, commits, pushes or other checkouts. Root checkouts untouched.

Read complete authorised vision, repository instructions, engine ADRs, public Python README/helpers/tests, and relevant execution, disposition, projection, model-document, identity, forcing, partition, canonical and artifact code. Source bundle is held privately; this report contains original analysis and synthetic fixtures only.

From checkout `bindings/python`:

```
uv sync --quiet
uv run --no-sync pytest tests/test_carrier_experiments.py -q -s
```

Final result: **2 intentionally retained failures, 10 passes**. `planning/carrier-regression-red.txt` preserves the first zero-carrier failure before any repair. `planning/carrier-experiments-final.txt` preserves final results. No repair has been made.

## Executed findings

1. The desired public-interpreter regression fails exactly as predicted: initial water 1 at quantum 1, mass 1 at quantum 0.1; requested half splits produce water counts `[0,0]`, mass counts `[5,5]`. This is not mocked arithmetic. Both `compile_model(...).run(...)` and public authoritative transfer count readback are exercised.
2. Existing staged primitives can satisfy the identical behavioural assertion when mass forcing is derived from realised water counts: water `[0,0]`, mass `[0,0]`. This changes the declared composition, not the semantics of independent partitions. The original red test remains red.
3. Complete mixing, non-round allocation, retention and two independent constituents execute through three timesteps. Initial water 2 plus incoming `[3,2,0]`; requested branches `[1.5,2.4,1]` and `[2.5,1.6,0]` realise `[1,2,1]` and `[2,1,0]`. Initial salt 4, incoming `[6,3,0]` gives branch counts `[2,3,3]` and `[4,1,0]`; initial tracer 11, incoming `[7,5,0]` gives `[3,6,4]` and `[7,3,0]`. Retained water/salt/tracer is `(2,4,8)`, `(1,3,4)`, `(0,0,0)`. Initial tracer concentration differs from incoming, discriminating complete pooling from incoming-only mixing. Integer recurrence preserves remainder. Engine executes and conserves the supplied derived transfers; the staging prototype computes recurrence outside the engine.
4. Staged synthetic pool cases pass A2, A3 actual capped release, A4 and A7 amounts/counts. A3's request-to-availability clipping is supplied explicitly in this prototype, not a demonstrated new capacity operation. Full A1 two-source attribution and broader physical acceptance remain future implementation tests.
5. Staged count-to-float reinjection is **not count-authoritative**. An existing public merging model creates exactly `8788444088577676` incoming counts at quantum `1e-6`. A subsequent literal of `count * quantum` is accepted and emits `8788444088577675`; one count remains. Replacing that literal with the existing same-substance incoming authoritative projection emits exactly `8788444088577676`. Stock is assembled from valid existing initial inputs, not fabricated by an invalid initial count. Therefore refusal of large supported counts or float forcing is not a complete solution.
6. A same-turn outgoing carrier projection cannot solve coupling: an executed carrier emits 2 counts but the dependent expression sees 0 because all partitions are evaluated against the pre-commit log.
7. Same run ID/model produces identical authoritative bytes; replay accepts the identical completed run. Changing precision changes digest and replay refuses. Changing an unused forcing ID containing carrier digest/mixing version also changes identity and replay refuses. This proves content-addressing covers forcing identity, **not** that metadata hidden in forcing names is a sound coupling schema. Do not ship this workaround.
8. Single-substance bounded lag delivers `[0,10]` from incoming `[10,0]`, retaining inventory at t0. A two-substance lag fails at t1 because `RuleInterpreter::fact` selects sparse records for the endpoint/time but then requires the selected substance on every record. Water-only records incorrectly cause `cannot resolve authoritative fact substance mass`. Separate genuine failing regression retained as `test_multisubstance_delay_existing_primitives`. Fix is directly relevant independent of selected mechanism; absent substance in a sparse transfer must contribute zero only when the substance is registered, not zero-fill unmodelled inputs.
9. Explicit evaporation/rewet scheduling retains dry mass: initial 2 water/1 mass, t0 water-only removal, clean t1 incoming 4, declared t1 remobilisation emits 4/1. This proves bookkeeping support, not a physical default.

## Mechanism decision

Recommend **one narrow count-native partition**, not a general coupled executor and not staged float forcings. Existing staged primitives demonstrate physical feasibility, but delivering them generally requires count-native public injection, attributable carrier events, a validated composite artifact linking both runs and assumptions, and a second recurrence/topology execution layer to generate mass forcings. These are wider changes than evaluating a dependent partition from an already validated carrier plan in one existing atomic transaction. Small-count staged success alone does not establish general sufficiency.

Candidate plain-data partition (illustrative, final naming belongs to implementation):

```json
{"rule_ir_version":"v1","numerical_semantics_version":"v1",
 "partition":{"kind":"carrier_proportional","carrier":"water",
 "branches":[{"branch":"to-left","carrier_branch":"left"},
             {"branch":"to-right","carrier_branch":"right"}]}}
```

The rule's `substance` is the dependent conserved quantity. `carrier` identifies a registered substance with an ordinary partition at the same finite compartment. Each dependent branch uses the existing transfer binding. Its destination must equal the referenced carrier branch destination. A subset of carrier branches is allowed: omitted carrier branches transport none of this dependent substance, leaving unallocated dependent counts at the source. This permits water-only evaporation without assigning chemistry in Incidence.

For pre-turn available dependent count D, pre-turn available carrier count C, and realised carrier branch count B, allocation is `floor(u128(D) * u128(B) / u128(C))`. C=0 allocates zero and retains D. Counts use the same source replay state, including initial stock plus already-committed incoming transfers, before any source withdrawal. Independently validate the carrier plan's total <= C before proportional evaluation. Use canonical branch order and one atomic existing Disposition commit. Quantised remainder is explicit retained inventory and naturally recurs. If carrier fully leaves while floor residue remains, the mass is not deleted.

Reject unknown carrier/substance/branch, missing carrier rule, self-reference, any carrier that is itself dependent (initial supported scope), duplicate dependent branch, duplicate carrier mapping, destination mismatch and incompatible protocol references. Do not add speculative dependency DAG scheduling. Multiple independent dependent substances share the same carrier plan without lexical-order dependence. Preserve existing ordinary partition semantics and old document/digest compatibility.

This proposal is **not executed or implemented**. Its mathematical allocation is the integer rule exercised by staged prototypes. Engine-path implementation still needs red-to-green tests at count limits and all public-boundary witnesses.

## Exact change map

- `crates/core/src/partition_expression.rs`: typed dependent partition/branch form, parse/serialize, canonical encoding and validated local structure. Keep denotation aligned with operation.
- `crates/core/src/model_artifact.rs`: cross-rule registered carrier, existing ordinary carrier partition, carrier branch identity, mapping uniqueness and destination compatibility checks. Include full declaration in content identity through canonical partition encoding.
- `crates/core/src/execution.rs`: preserve named realised branch counts in evaluated ordinary partition plans; evaluate ordinary then dependent plans without committing between them; integer proportional allocation via `Allocation::from_count`; retain exact remainder and single existing `ValidatedTransaction::commit`. Repair sparse multi-substance authoritative fact selection under separate red regression.
- `crates/core/src/disposition.rs`: no architectural change expected; existing explicit retained value and authoritative Allocation counts already validate exhaustive closure. Reuse this boundary.
- `crates/core/src/model_document.rs`: existing RuleDocument contains PartitionExpr, so no top-level schema or external side state required. Only update parsing validation propagation if necessary.
- `bindings/python`: compile_model already carries PartitionExpr as plain data. No callback/count-input API required. A tiny plain-data authoring helper is optional, not prerequisite. Existing transfer_count_series, transfer_series, quantum, authoritative_log and replay_against support basic proofs. Attributable source→destination readback remains a separate narrow result-boundary question; avoid parsing canonical log bytes as an ad hoc protocol.
- Tests: new native/Python public path coverage; preserve old water suite and identity behaviour. No new persistence/restart promise. Resume core tests should confirm deterministic reconstruction from prefix if claiming existing core compatibility.

## Requirement / public operation / observable / test crosswalk

| Requirement | Candidate public input/operation | Observable | Executed evidence / needed implementation proof |
|---|---|---|---|
| T2 realised coupling | compile_model carrier_proportional partition | transfer_count_series by substance/destination | zero regression red; staged same assertion green; same-turn projection insufficiency proven; new node red→green required |
| A1–A4/A7 pool mixing | initial_stocks + upstream rules + carrier branch partitions | realised destination counts; initial+incoming−outgoing inventory | A2/A3/A4/A7 staging passes; mixed initial/incoming tracer three-step case passes; actual new operation tests required |
| T6 exact quantum closure | declared units.quanta + count-native dependent allocation | sum destination counts + retained = available | scalar re-entry loses one supported count; authoritative projection preserves it; new operation near-ceiling test required |
| T3 temporal inventory | bounded_lag projections, finite transit compartment | t0 inventory and t1 arrival exactly once | one-substance lag passes; multi-substance lag red; fix+public green required |
| A5/A9 distinct process routes | explicit subset branch mappings and named finite use/transit compartments | carrier-only route; later located/timed dependent return | explicit dry schedule passes; full A9 coupled use/transit future test |
| A10 dry mass | zero-carrier partition retains dependent; explicit downstream remobilisation selection | retained mass while volume zero; declared subsequent release | dry/rewet explicit primitive schedule passes; coupled residue/dry/remobilisation proof required |
| T7 identity/replay | canonical partition declaration within compiled model | digest changes for mapping/precision; same rerun bytes; replay mismatch refusal | staged precision/replay passes; forcing-identity workaround tested/rejected; node canonical tests required |
| Multiple constituents | multiple dependent rules sharing carrier | independent exact counts/remainders | two-constituent staging passes; multi-substance projection defect red; lexical-order new-node tests needed |
| Presence/physical support | existing presence series plus caller's constituent support metadata | present zero vs absent vs not_modelled | do not infer physical chemistry from registry omission; downstream support owned by Taqsim #23 |
| Exchange result boundary | existing public count series and narrow attributable-event selector if needed | source/receiver/time/substance/count; retained reconstruction | shared destination ambiguity must be explicitly tested, not guessed from aggregate count series |

## Ownership and limits

Incidence owns substance-neutral counts, declaration validation, transactions, canonical identity, replay and public generic results. Taqsim #23 owns physical mixing selection, units/basis, evaporation/remobilisation support, delay/process mapping and physical-entry boundaries. Fishy owns assessment semantics. This work does not add chemistry, unknown-state policy, country methods or a Taqsim constituent facade. It does not claim saved outputs are restart checkpoints.

Physical inventory excludes preloaded future supply stock; count closure of the whole engine alone is not a physical-basin balance. The staged examples include a future supply compartment deliberately. Independent balances for physical pool/use/transit boundaries and volume-weighted transfer-vs-end-storage concentrations remain mandatory implementation proofs.

No ticket-shaped production names found in directly relevant source modules by targeted inspection/search; no production module was modified. No repository-wide cleanup or naming linter proposed.
