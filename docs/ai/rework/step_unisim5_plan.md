# Phase 5 — Replay + log overhaul

**Parent plan:** `docs/ai/rework/unisim.md` §5.5
**Predecessor:** `docs/ai/rework/step_unisim4_plan.md` (Phase 4 — tagged `unisim/phase4-complete`)
**Goal:** Make replay first-class. Introduce an engine-Event JSONL trace (Action stream + Event stream + per-step RNG call-count canary) alongside the existing AI-decision log; ship a `replay_engine_trace` binary that re-runs the engine from the trace and asserts identical final state plus byte-equal Event sequence; bump schema to v37 as a clean break; harden with `cargo fuzz`. After Phase 5, every combat is byte-exactly reproducible from disk on the recording host, and any engine refactor that drifts state or events fails CI loudly.
**Timebox:** ~2 weeks (matches §5.5 estimate). Sub-step 5a (serde) and 5b (RNG-instrumentation API change) carry the most risk; remainder is wiring + tooling.

**Out of scope (deferred to later phases):**
- `CombatEvent` retirement (D6 — stays as UI-facing projection). UI rewrite that drops `Res<CombatLog>` lands in Phase 6 alongside projection cleanup.
- Bevy combat-system deletions (already mostly done in Phase 4; the remainder — `apply_effects.rs` / `movement.rs` / `status_apply.rs` / `status_tick.rs` — is Phase 6 §5.6).
- Cross-platform byte-equal replay (D10 — best-effort with warn; full cross-host determinism is not promised).
- Trace compression, rotation, network upload — Phase 5 ships a plain JSONL on disk; size mitigation is a future concern.
- AI mining schema features beyond the v36→v37 bump itself (Phase 4 retrospective flagged this carry-over; Phase 5 v37 is the bump moment).
- Bridge-side projection correctness validation (replay validates ENGINE determinism only — see §8 gotcha "Projection drift is out of replay's scope").
- HexPositions respawn-over-corpse panic. Orthogonal; stays separate.

---

## 1. Scope

**Clarification up front:** §5.5 says "Combat log = engine Event stream (JSONL append-only) — not a derived `CombatEvent` log". This is slightly misleading. `CombatEvent` lives in `Res<CombatLog>` (an in-memory `Vec`) consumed by UI/popups; it has never been a JSONL log. Phase 5 introduces a NEW JSONL engine-trace alongside the existing AI-decision JSONL log (`src/combat/ai/log/`). `CombatEvent` stays as the UI projection of the engine Event stream (D6). Engine traces live at `logs/engine/*.jsonl` relative to CWD, mirroring the established AI-log location pattern (`src/combat/ai/log/mod.rs:482`).

**IN:**

**Engine extensions:**
- `#[derive(Serialize, Deserialize)]` on `Action`, `Effect`, `Event` and all dependent payload types (`CritFailOutcome`, `TurnSkipReason`, `SpawnBlockedReason`, `DamageCtx`, `ApplyCtx` sub-fields, `AuraStatusGained/Lost` triples, `PhaseEntered` fields, `EnterPhase` payloads, `PhaseTransition`, `AuraDef`, `AuraEffects`).
- `crates/combat_engine/Cargo.toml` gains `serde` (default-features=false, features=["derive"]) and `blake3` (for D3 content hash). Engine remains Bevy-free (§6.7).
- New `crates/combat_engine/src/content_hash.rs` — `fn hash_content(toml_files: &[(&str, &str)]) -> [u8; 32]` over canonical-sorted-key BLAKE3. Pure function, callable from both the recording-side bridge and replay-side `TomlContentView`.
- New `crates/combat_engine/src/trace.rs` — pure serialization helpers: `fn serialize_init(...) -> String`, `fn serialize_step(action, events, rng_calls, post_state_hash) -> String`, parser counterparts. **No file I/O, no Bevy types.** This factoring lets engine-crate tests do in-process record/replay (5d sub-step) without pulling Bevy in.
- `DiceSource` trait gains `call_count(&self) -> u64` accessor; both `ExpectedValue` and `DiceRng` impls track this. `step()` reports the per-action call count delta via a return-shape change OR `ApplyCtx` extension (5b decides which is cleaner once the trait expansion lands).
- `CombatState::aura_membership_set` switches `HashSet<(UnitId, UnitId, StatusId)>` → `BTreeSet<...>` (§8 gotcha "HashSet iteration" — only non-deterministic iteration in the engine after Phase 4). Diff-on-move/death emission becomes byte-deterministic on the recording host.

**Bridge:**
- New `src/game/engine_trace.rs` containing:
  - `EngineTraceWriter` Bevy `Resource` wrapping `BufWriter<File>` opened on combat start (path `logs/engine/<combat_id>.jsonl`).
  - `init_engine_trace_system` (OnEnter combat phase) — opens file, writes `init` line via `combat_engine::trace::serialize_init`.
  - `flush_engine_trace_system` (OnExit combat phase) — explicit flush + close.
  - All serialization delegated to `combat_engine::trace::*`. The Bevy `Resource` is a thin file-I/O wrapper.
- `process_action_system` calls `writer.write_step(action, events, rng_calls, post_state_hash)` BEFORE `project_state_to_ecs` so animation/UI lag does not corrupt the line if a downstream system panics mid-frame.
- `post_state_hash` = BLAKE3 over `(state.round, state.phase, state.turn_queue, sorted-by-id alive units)` — cheap per-step canary that catches drift in the middle of a trace before final-state full-equal fires.

**Replay tooling:**
- New `crates/combat_engine/src/toml_content_view.rs` (D9, locked: pure-Rust loader) — non-Bevy `ContentView` impl that reads `assets/data/*.toml` directly and resolves abilities/statuses/auras/enemy phases the same way `EcsContentView` does. Used by the replay binary and by content-hash recomputation. ~1-2 days of work; large but unblocks standalone replay forever.
- New `src/bin/replay_engine_trace.rs` — reads trace, builds `TomlContentView`, instantiates `CombatState` from `init` line, re-seeds `DiceRng`, replays each step. Asserts: (a) returned `Vec<Event>` byte-equals recorded; (b) `rng_calls` matches; (c) `post_state_hash` matches each step; (d) final-state full-equal. On mismatch, dumps diff to stderr + exits non-zero. Flags: `--strict-content` (escalates hash mismatch from warn to error per D3); `--tolerance <f32>` (D10 — relaxes f32 equality for cross-host replay).
- `crates/combat_engine/tests/replay.rs` — 5 canonical scenarios (single-target Cast, AoE multi-phase boss, summon, aura stun skip, full encounter with deaths). Each scenario records via pure `trace::serialize_step` helpers then replays in-process. No Bevy in this test path.

**Existing logs adaptation:**
- `src/combat/ai/log/mod.rs::SCHEMA_VERSION: 36 → 37`. `MIN_SUPPORTED_VERSION = 37` (D5).
- `src/bin/replay_ai_log.rs` — stale `if ver != 27 { unsupported }` gate fixed (broken since Phase 2's v27→v28 bump; Phase 5 cleanup). New gate: `if ver < 37`.
- `src/bin/mine_ai_logs.rs` — v37 reader. No legacy split per D5.
- Phase 4 retro flagged "mining v36+ migration" as a follow-up; Phase 5 v37 bump absorbs it.

**Fuzzing:**
- New `fuzz/` workspace-sibling crate. Root `Cargo.toml` `[workspace]` table gets `exclude = ["fuzz"]` (canonical cargo-fuzz pattern).
- Single target `step_random_actions`: generates random `Vec<Action>`, threads through a `StubContentView`, calls `step()` in a loop. Property: no panic for any sequence. Bonus property: every `Err` outcome is reproducible by re-running the same Action stream with the same seed.
- Documented in `fuzz/README.md`: requires nightly, run with `cargo +nightly fuzz run step_random_actions`.

**AI integration:**
- No change to AI sim logic. Engine trace is orthogonal to `mine_ai_logs`; mining keeps reading AI-decision logs, replay reads engine traces.

**Preserved:**
- `Res<CombatLog>` and `CombatEvent` enum (D6, UI consumer).
- AI-decision JSONL log path; only `SCHEMA_VERSION` constant bumps to v37.
- All Phase 4 behaviour. Phase 5 is purely additive at the engine layer + tooling around the new trace.

**OUT (deferred):** See top of file.

---

## 2. Architecture diff vs Phase 4

```
Phase 4:                                    Phase 5 target:

  Engine Event stream returned from         Same step() signature; engine types
  step(), consumed by bridge, never         gain serde derives. step() reports
  serialized.                               RNG call-count delta. trace::serialize_*
                                            helpers in engine crate.

  CombatEvent log (Vec in Res):             Unchanged — stays UI-facing (D6).
  UI consumer.                              Engine trace is a separate JSONL stream.

  DiceSource trait: roll(DiceExpr) → i32.   DiceSource gains call_count() accessor.
  No introspection.                         step() reports per-action consumption.

  aura_membership_set: HashSet              BTreeSet — deterministic iteration on
  (non-deterministic iter).                 the recording host.

  AI JSONL log v36 (in src/combat/ai/log/). v37 — clean break, MIN_SUPPORTED=37.

  replay_ai_log: stale ver==27 gate         Fixed to ver>=37.
  (broken since Phase 2).

  No engine trace. No replay binary.        New logs/engine/*.jsonl + replay_engine_trace
  No fuzz harness. No standalone content.   binary + fuzz/ sibling crate +
                                            TomlContentView (Bevy-free content loader).

  Combat reproducibility: re-run game       Combat reproducibility: replay binary
  manually with same RNG seed (and          asserts final state + Event byte-equal
  hope nothing drifted).                    from saved trace on the recording host;
                                            cross-host = best-effort + warn (D10).
```

---

## 3. File-level change list

| File | Change |
|---|---|
| `crates/combat_engine/Cargo.toml` | Add `serde` (derive) + `blake3` deps |
| `crates/combat_engine/src/action.rs` | Serde derives on `Action` |
| `crates/combat_engine/src/event.rs` | Serde derives on `Event` and all payload sub-types |
| `crates/combat_engine/src/effect.rs` | Serde derives on `Effect`, `ApplyCtx`, `DamageCtx`, `SpawnBlockedReason` |
| `crates/combat_engine/src/state.rs` | Serde derives on `Unit`/`UnitId`/`RoundPhase`/`Team`/`ActiveStatus`/`TurnQueue`; `aura_membership_set` HashSet → BTreeSet |
| `crates/combat_engine/src/content.rs` | Serde derives on `PhaseTransition`, `AuraDef`, `AuraEffects`, `StatusBonuses`, `TeamRelation` |
| `crates/combat_engine/src/dice.rs` | `DiceSource::call_count(&self) -> u64`; impls track count |
| `crates/combat_engine/src/step.rs` | `step()` reports per-action RNG-count delta |
| `crates/combat_engine/src/content_hash.rs` *(new)* | BLAKE3 content fingerprint; pure fn shared by writer + TomlContentView |
| `crates/combat_engine/src/trace.rs` *(new)* | Pure serde helpers: `serialize_init`/`serialize_step` + parsers. No I/O, no Bevy. |
| `crates/combat_engine/src/toml_content_view.rs` *(new)* | D9 — non-Bevy `ContentView` reading `assets/data/*.toml` directly |
| `crates/combat_engine/src/lib.rs` | Re-exports for trace + TomlContentView + content_hash |
| `crates/combat_engine/tests/serde_roundtrip.rs` *(new)* | Round-trip Action/Event/Effect with byte-equal assertions |
| `crates/combat_engine/tests/replay.rs` *(new)* | 5 canonical scenarios: record-then-replay in-process |
| `crates/combat_engine/tests/toml_content_view_parity.rs` *(new)* | TomlContentView vs EcsContentView parity for the same TOML inputs |
| `src/game/engine_trace.rs` *(new)* | `EngineTraceWriter` Bevy Resource; thin I/O wrapper over `trace::*` |
| `src/combat/engine_bridge.rs` | Wire `EngineTraceWriter` into `process_action_system` before `project_state_to_ecs` |
| `src/main.rs` | Register `EngineTraceWriter` resource + init/flush systems |
| `src/combat/ai/log/mod.rs` | `SCHEMA_VERSION: 36 → 37`; `MIN_SUPPORTED_VERSION = 37` |
| `src/bin/replay_ai_log.rs` | Fix stale `ver != 27` gate → `ver < 37` |
| `src/bin/mine_ai_logs.rs` | v37 reader; no legacy split (D5) |
| `src/bin/replay_engine_trace.rs` *(new)* | Replay binary; uses `TomlContentView`; `--strict-content`/`--tolerance` flags |
| `Cargo.toml` *(workspace root)* | Add `exclude = ["fuzz"]` to `[workspace]` |
| `fuzz/Cargo.toml` *(new)* | Sibling crate; cargo-fuzz boilerplate; depends on `combat_engine` |
| `fuzz/fuzz_targets/step_random_actions.rs` *(new)* | Random `Vec<Action>` → `step()` loop |
| `fuzz/src/stub_content.rs` *(new)* | Minimal `ContentView` for fuzz |
| `fuzz/README.md` *(new)* | Nightly requirement, corpus seeding, run instructions |
| `docs/ai/rework/step_unisim5_plan.md` *(new)* | This file |
| `docs/ai/rework/unisim.md` | Append Phase 5 retrospective at §5.5 after gate |

---

## 4. Sub-step decomposition

Each sub-step lands independently with `cargo check --all-targets` clean and `cargo test` green.

| Step | Title | What lands |
|---|---|---|
| **5a** | Serde derives + BTreeSet + content_hash + trace helpers | Serde on all engine types; `aura_membership_set` BTreeSet swap; `content_hash.rs` BLAKE3 fingerprint; `trace.rs` pure serialization helpers + round-trip tests. **Pure additive — no API breakage.** Bench: serde derive should not regress `step()` perf. |
| **5b** | DiceSource::call_count + step() RNG reporting | Add `call_count(&self) -> u64` to `DiceSource` trait; impl on `ExpectedValue`+`DiceRng`. `step()` reports per-action delta (return-shape change OR via `ApplyCtx` extension — decide during implementation). **Breaking trait change** — ripples through every `step()` callsite + test. Land before bridge wiring so callers update once. |
| **5c** | TomlContentView + parity tests | New `TomlContentView` reads `assets/data/*.toml` and resolves abilities/statuses/auras/enemy phases. Parity test cross-checks against `EcsContentView` for the same TOML inputs (same ability_def, status_def, auras_of, check_phase_trigger results). Unblocks both the replay binary AND content-hash recomputation. |
| **5d** | Engine trace writer + bridge wiring | `EngineTraceWriter` Bevy Resource + `init_engine_trace_system`/`flush_engine_trace_system`; `process_action_system` writes per-step. Dual-runs alongside `Res<CombatLog>` (no replacement). Manual playtest: spawn combat, verify `logs/engine/*.jsonl` appears with one line per `step()`. |
| **5e** | Replay binary + determinism tests | `src/bin/replay_engine_trace.rs` end-to-end (uses `TomlContentView`); `tests/replay.rs` with 5 canonical in-process record/replay scenarios. Replay asserts events byte-equal, rng_calls match, post_state_hash match each step, final state full-equal. `--tolerance` flag from D10. |
| **5f** | AI log v37 + fuzz harness | `SCHEMA_VERSION: 36 → 37`; stale `ver != 27` gate fix in `replay_ai_log`; `mine_ai_logs` v37 reader. New `fuzz/` sibling crate; `step_random_actions` target; seed corpus from canonical scenarios. Run 10M iters locally; fix any panics found. CI integration deferred (nightly requirement). |
| **5g** | Hardcutover + retrospective | Remove dual-run safety nets if any remain; final perf bench; draft Phase 5 retrospective in `unisim.md` §5.5; user applies tag `unisim/phase5-complete`. |

---

## 5. Decisions (locked)

### D1. Trace shape — Actions AND Events, not one or the other

Each trace line carries:
```jsonl
{ "schema": 37, "init": { "rng_seed": "0xDEAD...", "units": [...], "next_synthetic_uid": 100, "content_hash": "blake3:..." } }
{ "schema": 37, "step": 0, "action": { "Cast": { ... } }, "events": [...], "rng_calls": 3, "post_state_hash": "blake3:..." }
{ "schema": 37, "step": 1, "action": { "Move": { ... } }, "events": [...], "rng_calls": 0, "post_state_hash": "blake3:..." }
```

**Rationale:** Action-only loses regression signal — if engine derivation changes, replay can't notice. Event-only is impossible to "replay" because Events aren't a re-executable script. Both together: Actions are the replay tape, Events are the assertion ground truth. Per-line `post_state_hash` is a canary that catches drift mid-replay (cheaper than full state diff per step).

### D2. RNG recording — seed is the replay mechanism; call-count is a canary

**The replay mechanism is the seed.** `init.rng_seed` is the only thing replay actually feeds into the RNG; the seeded `DiceRng` produces a deterministic stream consumed in order by each `step()` call.

**`rng_calls` per line is purely a per-step canary** that localizes WHERE drift happened. If engine adds an extra `rng.roll()` mid-action, the stream is silently shifted from that point on — but `rng_calls` for that line mismatches by 1, and replay can pinpoint the divergence. Without the canary, drift would manifest only as "events differ at step N+17" with no clue why.

**Contract:** "Any engine change that adds/removes RNG calls = schema bump." Document at the top of `dice.rs`.

### D3. Content fingerprint — BLAKE3 hash + warn (no full snapshot)

`init.content_hash` = BLAKE3 over canonical-sorted-key concatenation of `assets/data/*.toml` files. BLAKE3 chosen because: stable across crate versions, no platform dependence, fast enough to compute on combat start (<1ms for ~100KB of TOML), reasonable dep weight. NOT FxHash (rustc-hash semantics aren't stable across versions), NOT SHA-256 (slower with no benefit).

Replay computes the hash via `combat_engine::content_hash::hash_content` on `assets/data/*.toml` it loads; if mismatch, prints `WARN: content drift since recording` to stderr and continues. `--strict-content` flag escalates to hard error.

TOML content is ~100KB; embedding inflates trace 10–100×. Hash gives drift detection without bloat. "Warn, don't fail" default acknowledges most replay use is post-balance-tweak debugging where content has legitimately changed.

### D4. Schema bump strategy — clean break at v37

`SCHEMA_VERSION: 36 → 37` is a hard cutover. `MIN_SUPPORTED_VERSION = 37`. Logs with `ver < 37` fail with `LogError::UnsupportedSchema { found, min: 37 }`. No migration shim, no legacy reader.

Matches prior unisim-era clean-break precedent. Test fixtures using old schemas are re-recorded as part of 5f.

### D5. Legacy reader retention — none

`mine_ai_logs` and `replay_ai_log` do not retain v36-and-below readers. Users who need historical mining must `git checkout unisim/phase4-complete` and run the binary from there.

Consistent with §6.5 strict-throughout. Carrying a legacy reader doubles test surface and creates slow drift risk where new mining features accidentally read old schema and produce subtly wrong aggregates.

### D6. `CombatEvent` destiny — stays as UI projection

The Bevy `CombatEvent` enum and `Res<CombatLog>` are kept unchanged. They are the UI-facing projection of the engine Event stream, with different consumer (popups, animations, tooltips) and different shape (Entity-keyed, human-formatted strings).

D1 already gives serialization. Re-using `CombatEvent` for trace would couple UI/animation concerns to replay determinism — every popup-format change would invalidate logs. Keep orthogonal. Phase 6 may revisit if `Res<CombatLog>` becomes redundant after UI projection rewrite — that's a UI question, not a replay question.

### D7. Fuzz target — single, stub content

One fuzz target `step_random_actions.rs`. Signature:
```rust
fuzz_target!(|input: (u64, Vec<Action>)| {
    let (seed, actions) = input;
    let mut state = CombatState::new(/* canonical seed state */);
    let mut rng = DiceRng::from_seed(seed);
    let content = StubContentView::default();
    for action in actions {
        let _ = step(&mut state, action, &mut rng, &content);
    }
});
```

Maximum panic-yield per CPU-hour. Multiple narrow targets fragment the corpus; a single broad target builds one rich corpus over time. Stub content avoids fuzz-finding panics that are actually content bugs (real TOML parsers can panic on malformed input — not what we're hunting).

### D8. Replay assertion granularity — per-step events + final state, not per-step state

Replay asserts per-step: `events == recorded_events` (byte-equal) AND `rng_calls == recorded_count` AND `post_state_hash == recorded_hash`. At end: final state full-equal via `==` on `CombatState`.

NOT per-step full state equality.

Per-step state equality is ~10× more expensive (state can be 10KB+ for large combats; serializing+comparing per step adds seconds to a 1000-step trace). Per-step event equality + per-step hash canary + final state equality has equivalent failure-detection — any state drift propagates into the next step's events or hash. Hash is cheap (~100ns); full state comparison is ~100μs.

### D9. Replay-time ContentView — pure-Rust TomlContentView

Replay needs a `ContentView` impl. `EcsContentView` is Bevy-coupled (reads ECS components), unusable in a standalone binary. Build a new `TomlContentView` in `crates/combat_engine/src/toml_content_view.rs` that reads `assets/data/*.toml` directly and resolves abilities/statuses/auras/enemy phases the same way `EcsContentView` does.

**Cost:** ~1-2 days to factor TOML parsing out of the Bevy asset loader. Worth it: replay binary runs anywhere (CI, headless, no Bevy app boot), can be cron-driven, can be invoked by mining for replay-on-divergence workflows. Also unblocks: content-hash recomputation (D3), future replay-as-test-fixture patterns.

**Parity gate:** `tests/toml_content_view_parity.rs` cross-checks `TomlContentView` against `EcsContentView` for the same TOML — same `ability_def`/`status_def`/`auras_of`/`check_phase_trigger` results for every entry. Strips in Phase 6 if/when `EcsContentView` is deleted.

### D10. Replay portability — best-effort + warn; f32 tolerance

Replay byte-equal is **only promised on the recording host**. Cross-host (Linux ↔ macOS ↔ Windows; x86_64 ↔ aarch64) is best-effort with warn-on-mismatch.

**f32 root cause:** `final_damage_f32` (and any future f32 chain in damage/heal formulas) may be fused into FMA by LLVM on ARM with no equivalent on x86. Rust has no stable per-crate fp-contract attribute we want to commit to.

**Mitigation:** `--tolerance <eps>` flag on `replay_engine_trace` relaxes f32 equality (default 0.0 = strict). Trace assertions use `(recorded - replayed).abs() <= eps` for f32 fields inside `Damage`/`Heal` events. Integer state (HP, AP, MP, round, queue, statuses) remains strict-equal always.

Mining/regression detection on the recording host stays sharp (eps=0 default). Player-side bug reports running on a different platform get a useful "approximately matches; here are the f32 drift sites" report instead of a binary fail.

**Trade-off accepted:** an engine bug that produces +0.01 damage drift is no longer flagged on cross-host replay. If we later need cross-host strictness, escalate to D10b in Phase 6 with `-C llvm-args=-fp-contract=off` build flag.

---

## 6. Sub-step kickoff order

Strict order: 5a → 5b → 5c → 5d → 5e → 5f → 5g.

Each sub-step:
1. `cargo check --all-targets` green.
2. Sub-step's targeted tests green.
3. Full suite (`cargo test`) green.
4. Commit with `ai/unisim Phase 5 step Nx: <title>` (mirror Phase 4 style).
5. User review before next sub-step.

**Rationale for ordering:**
- **5a** is purely additive (derives + BTreeSet + new modules with no readers). Cannot regress runtime behaviour. Round-trip tests prove serialization is sound before any writer reads it.
- **5b** is the breaking trait change. Land before bridge wiring so every `step()` callsite (including all test stubs) updates in one pass.
- **5c** introduces `TomlContentView` standalone — useful to land before the replay binary so parity tests pin the contract before binary wiring depends on it.
- **5d** wires the writer; legacy `CombatEvent` log still drives UI. If trace writer panics or produces malformed JSONL, only the trace file is affected; gameplay continues.
- **5e** activates replay — first end-to-end determinism check. Canonical scenarios in-process; no playtest needed.
- **5f** bumps schema + adds fuzz. Schema bump is mechanical; fuzz is long-running (run in background, fix panics one at a time).
- **5g** retrospective + tag. Clean cleanup.

---

## 7. Gate criteria (Phase 5 → Phase 6)

| # | Criterion | Verification |
|---|---|---|
| 1 | All engine types (`Action`, `Event`, `Effect` + payloads) round-trip via serde with byte-equality | `crates/combat_engine/tests/serde_roundtrip.rs` |
| 2 | RNG call-count introspection accurate: `step(Action::Cast { targets: N })` consumes exactly N rolls per §6.4 | Engine test in `dice.rs` or new `rng_count.rs` |
| 3 | `aura_membership_set` uses `BTreeSet`; replay byte-equal across 100 re-runs on the recording host with identical seed | Parametrized test in `tests/replay.rs` |
| 4 | Replay determinism on the recording host: re-run from trace produces identical final state + Event sequence + RNG count on all 5 canonical scenarios with `--tolerance 0` | `tests/replay.rs` |
| 5 | `TomlContentView` parity with `EcsContentView`: `ability_def`/`status_def`/`auras_of`/`check_phase_trigger` results identical for every TOML entry | `tests/toml_content_view_parity.rs` |
| 6 | `cargo +nightly fuzz run step_random_actions` → 10M iterations zero engine panics | Local + `fuzz/README.md` |
| 7 | Schema v37 reader handles all log fields; v36-and-below hard-fail with `LogError::UnsupportedSchema { found, min: 37 }` | `replay_ai_log` integration test |
| 8 | `replay_ai_log` stale `ver != 27` gate fixed; works on v37 logs | Manual run; grep clean of `27` in `replay_ai_log.rs` |
| 9 | `engine_trace.jsonl` size: ≤8KB per round of a 10-unit combat (absolute target; lock the number after first measurement) | Bench in `tests/replay.rs::trace_size_budget` |
| 10 | Cross-host replay with default `--tolerance 1.0` warns but does not panic on a known-divergent f32 case (e.g. damage formula chain) | Manual: record on x86_64, replay on aarch64 (or stub the divergence in a test) |
| 11 | `process_action_system` param count stays ≤14 (Phase 4 gate carry-over) | Code review |
| 12 | Bridge integration test: full combat encounter produces a trace that re-runs deterministically end-to-end | `tests/combat_engine/bridge_smoke.rs` extension |
| 13 | Full `cargo test` suite green; manual playtest of one encounter produces a trace file that replays cleanly | CI + manual |

---

## 8. Known gotchas

- **f32 FMA non-determinism (covered by D10).** `final_damage_f32` is the known site. Audit any future f32 chain in damage/heal; if a chain risks FMA fusion and matters for cross-host, fix at design time rather than via tolerance.
- **HashMap/HashSet iteration in engine code.** `aura_membership_set` HashSet→BTreeSet (5a) is the only known non-determinism. After 5a, audit all `HashMap`/`HashSet` constructions inside `crates/combat_engine/` via Serena `find_referencing_symbols`; document each as "iteration order never exposed" or "switch to BTree".
- **Synthetic `UnitId` allocation seeding.** `CombatState::alloc_synthetic_uid` increments `next_synthetic_uid`. Trace `init` line MUST include the starting counter value so replay seeds the same counter. Already in §5 D1 init shape.
- **RNG seed-vs-stream drift (covered by D2).** Adding any new `rng.roll(...)` mid-action breaks all old traces. Contract documented at top of `dice.rs`: "engine change that adds/removes RNG calls = schema bump".
- **Bevy `Entity` IDs never reach the engine trace.** Engine has zero Bevy dep (§6.7); only `UnitId(u64)` is logged. Audit complete after Phase 4; reconfirm in 5d code review.
- **Strict TargetGone failures (§6.5) under fuzz.** Fuzz will generate action sequences that legally `Err(ActionError::TargetGone)`. Trace must record `Err` outcomes (not just `Ok` event lists); replay must reproduce identical `Err`. Otherwise fuzz's "no panic" property degrades to "no panic except expected ones" — fragile.
- **ContentView TOML drift between recording and replay (D3).** Content hash mismatch warns to stderr by default; `--strict-content` flag escalates to error. Document in `replay_engine_trace.rs --help`.
- **Multi-actor `ActionInput::EndTurn` ordering.** Phase 4 made `EndTurn` first-class. Trace preserves Action write order, which IS the engine-observed order — but UI/AI may write EndTurn in different Bevy frames. Document: "trace records engine step order, not Bevy frame order".
- **Projection drift is out of replay's scope.** `apply_phase_transitions_system` and `project_state_to_ecs` run OUTSIDE `step()` and mutate ECS. Replay validates engine state + event stream; it does NOT validate that ECS components match engine state post-projection. Phase 6 may add a separate projection-parity gate; Phase 5 does not.
- **`cargo fuzz` requires nightly.** `fuzz/README.md` says: `rustup toolchain install nightly && cargo +nightly fuzz run step_random_actions`. CI integration deferred; local fuzz runs only in Phase 5.
- **`fuzz/` workspace exclusion.** Cargo-fuzz canonical pattern: root `Cargo.toml` `[workspace]` table gets `exclude = ["fuzz"]`. The `fuzz/` crate has its own independent `Cargo.toml` with no `[workspace]` field. Verify after creation.
- **Trace file rotation.** Long combats (50+ rounds) produce multi-MB traces. Phase 5 ships no rotation; one file per combat. Document this and propose rotation in Phase 6 if size becomes a problem.
- **`Vec<Unit>` final-state equality requires stable serde field order.** Use `#[serde(rename_all = "snake_case")]` consistently; lock field order in `Unit` struct (do NOT reorder for "readability" without a schema bump).
- **BTreeSet ordering by `(UnitId, UnitId, StatusId)` triple.** `UnitId(u64)` orders by `u64`; `StatusId(String)` orders lexicographic. Both stable and cross-platform deterministic. Confirm `StatusId` derives `Ord` correctly.
- **TomlContentView parity drift.** `EcsContentView` may diverge from TOML semantics over time (e.g. defaults applied via Bevy asset loader). Parity test (gate item 5) catches this; treat parity failures as engine bugs, not test flakes.
- **Trace truncation on crash.** If recording crashes mid-step, the last JSONL line is partial. Replay must `if line.is_empty() || !line.ends_with('\n') { truncate and warn }`. Document.
- **Phase 4 retrospective carry-over: mining v36+ migration.** Phase 4 retro flagged that pre-4f mining logs are v27 and the current miner expects v36+. Phase 5's v37 bump (sub-step 5f) absorbs this naturally. Re-mining historical corpus is out of scope.

---

## 9. Retrospective

(Filled at Phase 5 close.)
