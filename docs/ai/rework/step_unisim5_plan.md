# Phase 5 — Replay + log overhaul

**Parent plan:** `docs/ai/rework/unisim.md` §5.5
**Predecessor:** `docs/ai/rework/step_unisim4_plan.md` (Phase 4 — tagged `unisim/phase4-complete`)
**Goal:** Make replay first-class. Introduce an engine-Event JSONL trace (Action stream + Event stream + per-step RNG call-count canary + per-step state hash) as an independent stream from the existing AI-decision log; reorganize log filesystem so both streams for one fight live in a shared folder; ship a `replay_engine_trace` binary that re-runs the engine from the trace and asserts identical final state plus byte-equal Event sequence; harden with `cargo fuzz`. After Phase 5, every combat is byte-exactly reproducible from disk on the recording host, and any engine refactor that drifts state or events fails CI loudly.
**Timebox:** ~2 weeks (matches §5.5 estimate). Sub-step 5a (serde) and 5b (RNG-instrumentation API change) carry the most risk; 5c (TomlContentView) is the biggest unknown after re-scoping.

**Status:** 5a shipped (commit `fb81964`). 5b shipped (commit `27e37e5`). Plan updated 2026-05-17 with seven post-5a discoveries (see §10) — most recently option (A) for per-combat state, which split 5c into 5c.1 + 5c.2.

**Out of scope (deferred to later phases):**
- `CombatEvent` retirement (D6 — stays as UI-facing projection). UI rewrite dropping `Res<CombatLog>` is Phase 6 alongside projection cleanup.
- Bevy combat-system deletions (mostly done in Phase 4; the remainder — `apply_effects.rs` / `movement.rs` / `status_apply.rs` / `status_tick.rs` — is Phase 6 §5.6).
- Cross-platform byte-equal replay (D10 — best-effort with warn; full cross-host determinism not promised).
- Trace compression, rotation, network upload — Phase 5 ships plain JSONL on disk; size mitigation is future concern.
- **AI log schema bump (was 5f).** AI log content doesn't change in Phase 5; bumping its `SCHEMA_VERSION` was bogus coupling to engine work (§10 discovery #2). The "mining v36+ migration" Phase 4 retro flagged is real cleanup but moves to a separate follow-up commit.
- Bridge-side projection correctness validation (replay validates ENGINE determinism only — see §8 gotcha "Projection drift is out of replay's scope").
- TomlContentView ↔ Bevy-asset-loader factor-out (D9 sub-decision: duplicate parsing in Phase 5, factor out only if maintenance burden materializes).
- Scenario-run-wide correlation (D11 covers per-combat join only; per-scenario telemetry is a future concern).
- HexPositions respawn-over-corpse panic. Orthogonal; stays separate.

---

## 1. Scope

**Clarification up front:** §5.5 says "Combat log = engine Event stream (JSONL append-only) — not a derived `CombatEvent` log". This is slightly misleading. `CombatEvent` lives in `Res<CombatLog>` (an in-memory `Vec`) consumed by UI/popups; it has never been a JSONL log. Phase 5 introduces a NEW JSONL engine-trace alongside the existing AI-decision log; both are **reorganized into one folder per fight** (§10 discovery #6). `CombatEvent` stays as the UI projection of the engine Event stream (D6).

**New filesystem layout:**
```
logs/
├── 20260517T143022_main_cave_01_combat_a/
│   ├── ai.jsonl       ← was logs/20260517T143022_main_cave_01_combat_a.jsonl
│   └── engine.jsonl   ← new in Phase 5
└── 20260517T151208_main_cave_01_combat_b/
    ├── ai.jsonl
    └── engine.jsonl
```
Folder name = existing AI-log filename stem from `build_combat_log_path` (timestamp + campaign + scenario + encounter, sanitized). The folder name string IS the `session_id` (D11) — no UUID generation.

**IN:**

**Engine extensions (5a — SHIPPED):**
- `#[derive(Serialize, Deserialize)]` on `Action`, `Effect`, `Event` and all dependent payload types. ✓
- `crates/combat_engine/Cargo.toml` gains `serde` + `serde_json` + `blake3` deps. Engine remains Bevy-free (§6.7). ✓
- `crates/combat_engine/src/content_hash.rs` — BLAKE3 fingerprint, pure fn shared by writer + TomlContentView. ✓
- `crates/combat_engine/src/trace.rs` — pure serialization helpers (`InitLine`, `StepLine`, serialize/parse, `post_state_hash`, `SCHEMA_VERSION = 1`). No file I/O, no Bevy. ✓
- `CombatState::aura_membership_set` HashSet → BTreeSet (only known non-determinism in the engine after Phase 4). ✓
- **Post-5a follow-up (lands in 5d):** Add `session_id: String` field to `InitLine`. Already-shipped serde derives + parser absorb the additive field without breakage.

**Engine extensions (5b — pending):**
- `DiceSource` trait gains `call_count(&self) -> u64` accessor; impl on `ExpectedValue` and `DiceRng`.
- `step()` reports per-action RNG-call delta via `ApplyCtx::rng_calls: u64` (locked, see D2 implementation note).
- Engine purity audit (D12): Serena sweep for `SystemTime`, `std::env`, `std::process`, `thread_local` inside `crates/combat_engine/`. Result must be zero or every find documented as deterministic.

**Bridge (5d):**
- `src/combat/ai/log/mod.rs`: rename `build_combat_log_path` → `build_combat_log_dir` returning the folder `PathBuf`. AI log writer writes `<dir>/ai.jsonl`. AI log header gains `session_id: String` (the folder name).
- New `src/game/engine_trace.rs` containing:
  - `EngineTraceWriter` Bevy `Resource` wrapping `BufWriter<File>` opened on combat start (path `<dir>/engine.jsonl` where `<dir>` is the shared fight folder).
  - `init_engine_trace_system` (on `OnEnter(AppState::Combat)`, NOT on internal combat-phase transitions) — receives the fight `<dir>` from the same bootstrap hook that initializes the AI log; opens file; writes `init` line via `combat_engine::trace::serialize_init` with `session_id = dir.file_name()`.
  - `flush_engine_trace_system` (on `OnExit(AppState::Combat)`) — explicit flush + close.
- Single combat-start hook in the bridge: computes `<dir>` from `(now_epoch_s, campaign, scenario, encounter)`, creates it, passes to AI log writer + engine trace writer. Two writers share the same `session_id` by construction.
- `process_action_system` calls `writer.write_step(action, events, rng_calls, post_state_hash)` BEFORE `project_state_to_ecs` so animation/UI lag does not corrupt the line on downstream panic.

**Replay tooling (5c + 5e):**
- New `crates/combat_engine/src/toml_content_view.rs` (5c, D9 locked: pure-Rust loader) — non-Bevy `ContentView` impl reading `assets/data/*.toml` directly. Sub-decision: **duplicate the existing parsing logic** rather than factoring TOML parsing out of the Bevy asset loader. Factor-out is cleaner long-term but adds ~3 days; defer until duplication actually causes maintenance pain.
- Parity test (`tests/toml_content_view_parity.rs`) cross-checks `TomlContentView` vs `EcsContentView` for the same TOML inputs.
- New `src/bin/replay_engine_trace.rs` (5e) — accepts either a fight folder path OR a direct `engine.jsonl` path; reads trace, builds `TomlContentView`, instantiates `CombatState` from `init` line, re-seeds `DiceRng`, replays each step. Asserts: (a) returned `Vec<Event>` byte-equals recorded; (b) `rng_calls` matches; (c) `post_state_hash` matches each step; (d) **final state assertion = last step's `post_state_hash` matches** (NOT a separate final state snapshot). On divergence, optionally reads sibling `ai.jsonl` from the same folder for richer error reporting. Flags: `--strict-content` (D3 escalation), `--tolerance <eps>` (D10).
- `crates/combat_engine/tests/replay.rs` — 5 canonical scenarios; in-process record/replay via pure `trace::serialize_*` helpers. No Bevy.

**Cross-correlation (D11, NEW, lands in 5d):**
- Both `<dir>/engine.jsonl` `init` line and `<dir>/ai.jsonl` header carry `session_id: String` = the folder name (e.g. `"20260517T143022_main_cave_01_combat_a"`).
- AI log per-decision entries gain optional `engine_step_range: Option<(u64, u64)>` (`[start_step, end_step_exclusive)`) populated by the bridge between AI tick and engine apply.
- External cross-tool can join AI-decision N with engine steps `[start, end)` via `(session_id, step_range)`. Since both files live in the same folder, the tool can also just look at sibling — but `session_id` keeps each file self-describing if extracted/moved.

**Existing logs adaptation (5g — minor cleanup only):**
- `src/bin/replay_ai_log.rs` — fix stale `if ver != 27 { unsupported }` gate (broken since Phase 2's v27→v28 bump; free Phase 5 cleanup) AND switch to reading from `<dir>/ai.jsonl` instead of the old flat `<file>.jsonl` path. Glob change: `logs/*.jsonl` → `logs/*/ai.jsonl`.
- `src/bin/mine_ai_logs.rs` — same glob change.
- **NOT** bumping `src/combat/ai/log/mod.rs::SCHEMA_VERSION` — AI log content doesn't change.

**Fuzzing (5f):**
- New `fuzz/` workspace-sibling crate. Root `Cargo.toml` gets `[workspace] exclude = ["fuzz"]`.
- Single target `step_random_actions`: random `Vec<Action>` through `StubContentView`. Property: no panic. Bonus property: every `Err` outcome reproducible by re-running with the same seed.
- Documented in `fuzz/README.md`: requires nightly.

**AI integration:**
- No change to AI sim logic. Engine trace is orthogonal to `mine_ai_logs`.

**Preserved:**
- `Res<CombatLog>` and `CombatEvent` enum (D6).
- AI-decision JSONL log content/schema; only filesystem location changes (flat → folder).
- All Phase 4 behaviour. Phase 5 is purely additive at the engine level + filesystem reorg + tooling.

**OUT (deferred):** See top of file.

---

## 2. Architecture diff vs Phase 4

```
Phase 4:                                    Phase 5 target:

  Engine Event stream returned from         Same step() signature; engine types
  step(), consumed by bridge, never         gain serde derives. step() reports
  serialized.                               RNG count via ApplyCtx.rng_calls.
                                            trace::serialize_* helpers in engine.

  CombatEvent log (Vec in Res):             Unchanged — stays UI-facing (D6).
  UI consumer.                              Engine trace is a separate JSONL.

  DiceSource trait: roll(DiceExpr) → i32.   DiceSource gains call_count() accessor.
  No introspection.                         ApplyCtx carries per-step delta.

  aura_membership_set: HashSet              BTreeSet — deterministic iteration on
  (non-deterministic iter).                 the recording host.

  AI JSONL log: flat `logs/<name>.jsonl`;   AI log moves to `logs/<name>/ai.jsonl`;
  replay_ai_log broken w/ stale ver==27.    folder name carries fight identity.
                                            replay_ai_log stale ver-gate fixed.

  No engine trace. No replay binary.        New `logs/<name>/engine.jsonl` (sibling
  No fuzz harness. No standalone content.   to ai.jsonl). replay_engine_trace
                                            binary + fuzz/ sibling crate +
                                            TomlContentView (Bevy-free content loader).

  Combat reproducibility: re-run game       Combat reproducibility: replay binary
  manually with same RNG seed (and          asserts final state + Event byte-equal
  hope nothing drifted).                    from saved trace on recording host;
                                            cross-host = best-effort + warn (D10).

  No cross-log correlation.                 session_id (= folder name string) in BOTH
                                            files' headers; AI decisions optionally
                                            back-reference engine step ranges.
```

---

## 3. File-level change list

| File | Change | Sub-step |
|---|---|---|
| `crates/combat_engine/Cargo.toml` | Add `serde`/`serde_json`/`blake3` deps | 5a ✓ |
| `crates/combat_engine/src/action.rs` | Serde derives on `Action` | 5a ✓ |
| `crates/combat_engine/src/event.rs` | Serde derives on `Event` + payloads | 5a ✓ |
| `crates/combat_engine/src/effect.rs` | Serde derives on `Effect`/`ApplyCtx`/`DamageCtx`/`SpawnBlockedReason` | 5a ✓ |
| `crates/combat_engine/src/state.rs` | Serde derives on `Unit`/`UnitId`/`RoundPhase`/`Team`/`ActiveStatus`/`TurnQueue`; HashSet → BTreeSet | 5a ✓ |
| `crates/combat_engine/src/content.rs` | Serde derives on `PhaseTransition`/`AuraDef`/`AuraEffects`/`StatusBonuses`/`TeamRelation` | 5a ✓ |
| `crates/combat_engine/src/content_hash.rs` *(new)* | BLAKE3 content fingerprint | 5a ✓ |
| `crates/combat_engine/src/trace.rs` *(new)* | Pure serde helpers; `SCHEMA_VERSION = 1` | 5a ✓ |
| `crates/combat_engine/src/dice.rs` | `DiceSource::call_count(&self) -> u64`; impls track | 5b |
| `crates/combat_engine/src/effect.rs` | `ApplyCtx::rng_calls: u64` field (D2) | 5b |
| `crates/combat_engine/src/step.rs` | Populate `ApplyCtx::rng_calls` from `DiceSource::call_count` deltas | 5b |
| `crates/combat_engine/src/trace.rs` | Add `session_id: String` field to `InitLine` | 5d |
| `crates/combat_engine/src/toml_content_view.rs` *(new)* | D9 pure-Rust ContentView | 5c |
| `crates/combat_engine/src/lib.rs` | Re-exports | 5a ✓ + 5c |
| `crates/combat_engine/tests/serde_roundtrip.rs` | Round-trips for all serde'd types | 5a ✓ |
| `crates/combat_engine/tests/trace_helpers.rs` | Trace helper smoke tests | 5a ✓ |
| `crates/combat_engine/tests/aura_determinism.rs` | BTreeSet iteration determinism | 5a ✓ |
| `crates/combat_engine/tests/toml_content_view_parity.rs` *(new)* | TomlContentView ↔ EcsContentView parity | 5c |
| `crates/combat_engine/tests/replay.rs` *(new)* | 5 canonical record/replay scenarios | 5e |
| `crates/combat_engine/tests/engine_purity.rs` *(new)* | Compile-time assert no SystemTime/env imports (D12) | 5b |
| `src/game/engine_trace.rs` *(new)* | `EngineTraceWriter` Bevy Resource; writes `<dir>/engine.jsonl` | 5d |
| `src/combat/engine_bridge.rs` | Single combat-start hook computes `<dir>`; wires writer; populates AI log `session_id` + `engine_step_range` | 5d |
| `src/combat/ai/log/mod.rs` | Rename `build_combat_log_path` → `build_combat_log_dir` (returns folder); writer writes `<dir>/ai.jsonl`; header gains `session_id`; per-decision entry gains optional `engine_step_range` (D11) | 5d |
| `src/main.rs` | Register `EngineTraceWriter` + init/flush systems | 5d |
| `src/bin/replay_ai_log.rs` | Fix stale `ver != 27` gate + switch glob `logs/*.jsonl` → `logs/*/ai.jsonl` | 5g |
| `src/bin/mine_ai_logs.rs` | Switch glob to `logs/*/ai.jsonl` | 5d (when AI writer moves) |
| `src/bin/replay_engine_trace.rs` *(new)* | Replay binary; accepts fight folder OR `engine.jsonl` path | 5e |
| `Cargo.toml` *(workspace root)* | `[workspace] exclude = ["fuzz"]` | 5f |
| `fuzz/Cargo.toml` *(new)* | Sibling crate | 5f |
| `fuzz/fuzz_targets/step_random_actions.rs` *(new)* | Fuzz target | 5f |
| `fuzz/src/stub_content.rs` *(new)* | Stub `ContentView` for fuzz | 5f |
| `fuzz/README.md` *(new)* | Nightly + run instructions | 5f |
| `docs/ai/rework/step_unisim5_plan.md` | This file | 5a ✓ (initial) + updates |
| `docs/ai/rework/unisim.md` | Append Phase 5 retrospective at §5.5 | 5g |

---

## 4. Sub-step decomposition

Each sub-step lands independently with `cargo check --all-targets` clean and `cargo test` green.

| Step | Title | What lands |
|---|---|---|
| **5a** ✓ | Serde derives + BTreeSet + content_hash + trace helpers | **SHIPPED** in `fb81964`. Engine fully serializable; `trace.rs` already includes `InitLine`/`StepLine`/`post_state_hash` — over-delivered vs original scope. |
| **5b** ✓ | DiceSource::call_count + ApplyCtx::rng_calls + purity audit | **SHIPPED** in `27e37e5`. `step()` return shape became `(Vec<Event>, ApplyCtx)`; bridge + sim + 4 engine test files updated in one pass. Engine purity audit clean. |
| **5c.1** | Engine absorbs per-combat state; ContentView trait contracts to static-only | Add fields to `Unit`: `caster_context: CasterContext`, `auras: Vec<AuraDef>`, `enemy_phases: Vec<PhaseEntry>` (PhaseEntry moves from bridge to engine). Remove 4 methods from `ContentView` trait: `auras_of`, `check_phase_trigger`, `caster_context`, `aoo_dice`. Migrate engine call sites to read `state.unit(id).<field>` instead. `init_state_from_ecs` populates new Unit fields from existing ECS components. `EcsContentView` shrinks (4 methods gone; `aoo_per_unit`/`caster_contexts`/`aura_sources`/`phase_triggers` fields deleted). All `Unit` test stubs updated to default the new fields. **Breaking change** — sets up 5c.2 to be trivial. |
| **5c.2** | TomlContentView (much smaller after 5c.1) | New `TomlContentView` reuses `src/content::ContentView` data struct + existing `load_*()` functions in `src/content/*.rs` (audit showed parsing is already pure Rust). Trait impl covers only 4 remaining static methods: `ability_def`, `status_def`, `status_bonuses`, `unit_template`. ~150 LOC including loader. Parity test pins contract vs `EcsContentView`. |
| **5d** | Folder-per-fight + engine trace writer + bridge wiring + session_id (D11) | Rename `build_combat_log_path` → `build_combat_log_dir`. Single combat-start hook computes `<dir>` from `(now, campaign, scenario, encounter)`, creates it, passes to both writers. AI log moves to `<dir>/ai.jsonl`; gains `session_id` header + optional `engine_step_range` per decision. `EngineTraceWriter` writes `<dir>/engine.jsonl`. Add `session_id: String` to `InitLine`. `mine_ai_logs` glob updated alongside. Manual playtest: verify `logs/<fight_id>/{ai,engine}.jsonl` appears. |
| **5e** | Replay binary + determinism tests | `src/bin/replay_engine_trace.rs` accepts fight folder OR engine.jsonl path; uses `TomlContentView`; `tests/replay.rs` 5 canonical scenarios. Replay asserts events byte-equal, rng_calls match, post_state_hash match each step. **Final-state assertion = last `post_state_hash` match** (NOT a separate snapshot). `--tolerance` flag from D10. On divergence, optionally reads sibling `ai.jsonl` for context. |
| **5f** | Fuzz harness | New `fuzz/` sibling crate; `step_random_actions` target; seed corpus from canonical scenarios. Run 10M iters locally; fix any panics. **No AI log v37 bump** — not Phase 5 scope. |
| **5g** | replay_ai_log cleanup + retrospective | Fix stale `ver != 27` in `replay_ai_log.rs` + switch its glob to `logs/*/ai.jsonl`; final perf bench; draft Phase 5 retrospective in `unisim.md` §5.5; tag `unisim/phase5-complete`. |

---

## 5. Decisions (locked)

### D1. Trace shape — Actions AND Events AND session_id

Each trace line carries:
```jsonl
{ "schema": 1, "init": { "session_id": "20260517T143022_main_cave_01_combat_a", "rng_seed": "0xDEAD...", "units": [...], "next_synthetic_uid": 100, "content_hash": "blake3:..." } }
{ "schema": 1, "step": 0, "action": { "Cast": { ... } }, "events": [...], "rng_calls": 3, "post_state_hash": "blake3:..." }
{ "schema": 1, "step": 1, "action": { "Move": { ... } }, "events": [...], "rng_calls": 0, "post_state_hash": "blake3:..." }
```

Action-only loses regression signal — if engine derivation changes, replay can't notice. Event-only is impossible to re-execute. Both together: Actions are the replay tape, Events are the assertion ground truth. Per-line `post_state_hash` is a canary localizing drift mid-replay.

### D2. RNG recording — seed is the mechanism; call-count is a canary; routed via ApplyCtx

**The replay mechanism is the seed.** `init.rng_seed` is the only thing replay feeds into the RNG; the seeded `DiceRng` produces a deterministic stream consumed by each `step()` call.

**`rng_calls` per line is a per-step canary.** If engine adds an extra `rng.roll()` mid-action, the stream silently shifts from that point on — but `rng_calls` for that line mismatches by 1, and replay can pinpoint the divergence step.

**Implementation locked: route via `ApplyCtx::rng_calls: u64`** rather than changing `step()`'s return type. `step()` records `before = rng.call_count()`, runs the effect cascade, records `after = rng.call_count()`, writes `ctx.rng_calls = after - before`. Caller reads `Vec<Event>` AND `ApplyCtx` (already returned in 4d's `phase_entered` precedent — same pattern). Zero impact on callsites that don't care about the count.

**Contract:** "Any engine change that adds/removes RNG calls = trace schema bump." Document at top of `dice.rs`.

### D3. Content fingerprint — BLAKE3 hash + warn

`init.content_hash` = BLAKE3 over canonical-sorted-key concatenation of `assets/data/*.toml` files. Stable across crate versions, cross-platform deterministic, fast (<1ms for ~100KB). NOT FxHash (rustc-hash semantics aren't stable across versions), NOT SHA-256 (slower, no benefit).

Replay computes hash via `combat_engine::content_hash::hash_content`; on mismatch, prints `WARN: content drift since recording` to stderr and continues. `--strict-content` flag escalates to error.

### D4. Schema bump strategy — engine trace v1, no coupling

Engine trace starts at `SCHEMA_VERSION = 1` and bumps only on engine trace format changes. AI log keeps its own counter, bumps only on AI log content changes. **No shared version constant**, no artificial coupling.

If engine trace evolves: bump trace constant only. If AI log evolves: bump AI log constant only. Either bump is a clean break (no migration shim, no legacy reader retention — see D5).

### D5. Legacy reader retention — none, per-stream

`replay_engine_trace` does not retain pre-v1 readers (there are none; v1 is the floor). When future schema bumps happen, hard-fail with `LogError::UnsupportedSchema { found, min }`. Same posture for AI log when its schema next bumps. Per-stream policy — bumps don't cascade.

Note: Phase 5 ALSO breaks AI log filesystem location (flat → folder-per-fight). Old flat-layout AI logs at `logs/*.jsonl` become orphans; users who need them must `git checkout unisim/phase4-complete`.

### D6. CombatEvent destiny + folder-per-fight + engine trace ↔ AI log independence

`Res<CombatLog>` and `CombatEvent` stay unchanged — UI-facing projection (popups/animations/tooltips), Entity-keyed, human-formatted strings.

**Engine trace and AI log live in the same folder per fight:**
```
logs/<fight_id>/
├── ai.jsonl
└── engine.jsonl
```
where `<fight_id>` = existing `build_combat_log_path` naming pattern (timestamp + sanitized campaign/scenario/encounter). Both files independently versioned (D4); the only shared content is `session_id` = the folder name (D11). Single combat-start hook in the bridge creates the folder and passes to both writers — no race, no name skew.

Phase 6 may revisit `Res<CombatLog>` redundancy after UI projection rewrite — UI question, not replay question.

### D7. Fuzz target — single, stub content

One `fuzz_target!(|input: (u64, Vec<Action>)| { ... })` calling `step()` in a loop with `StubContentView`. Single broad target builds rich corpus over time; multiple narrow targets fragment. Stub content avoids panics that are actually content bugs.

### D8. Replay assertion — per-step events + state hash; final state = last hash

Replay asserts per-step: `events == recorded_events` (byte-equal) AND `rng_calls == recorded_count` AND `post_state_hash == recorded_hash`. **Final state assertion = last step's `post_state_hash` matches** — NOT a separately recorded final state. Recording final state would be redundant with the per-step hash sequence (any state drift propagates into the next step's hash).

NOT per-step full state equality (10× expensive; same failure-detection as the cheap path).

### D9. Replay-time ContentView — pure-Rust TomlContentView; reuse existing parsers

Replay needs a Bevy-free `ContentView`. Build `TomlContentView` in `crates/combat_engine/src/toml_content_view.rs`.

**Audit (post-5b, §10 discovery #7) revised the sub-decision.** Original draft said "duplicate the parsing logic". Audit found that:
- TOML parsing is ALREADY pure Rust — every `src/content/*.rs` file has a `load_X()` function calling `std::fs::read_to_string + toml::from_str`. Zero Bevy in parsing.
- `src/content::ContentView` is a pure-Rust data struct (not the engine trait of the same name — name collision in the codebase) holding `HashMap<Id, Def>` collections. Bevy-free fields.
- `ActiveContent` is just `Resource(pub ContentView)` — a thin Bevy newtype.

**Revised sub-decision: REUSE the existing parsers and data struct.** TomlContentView wraps `src/content::ContentView` directly. No parsing duplication. Loader is a ~30-line constructor calling `load_abilities()` + `load_statuses()` + etc.

**Trait surface contracts** (D11b, §10 discovery #7 / option A): 4 of 8 `ContentView` methods absorbed into engine `Unit` state in 5c.1. TomlContentView only implements the remaining 4 STATIC methods: `ability_def`, `status_def`, `status_bonuses`, `unit_template`. Total ~150 LOC.

**Parity gate:** `tests/toml_content_view_parity.rs` cross-checks remaining 4 methods against `EcsContentView`. Treat parity failures as engine bugs.

### D10. Replay portability — best-effort + warn; f32 tolerance

Replay byte-equal is **only promised on the recording host**. Cross-host (Linux ↔ macOS ↔ Windows; x86_64 ↔ aarch64) is best-effort with warn-on-mismatch.

`final_damage_f32` (and any future f32 chain) may fuse into FMA on ARM with no x86 equivalent. Rust has no stable per-crate fp-contract attribute.

**Mitigation:** `--tolerance <eps>` flag relaxes f32 equality (default 0.0 = strict). Trace assertions use `(recorded - replayed).abs() <= eps` for f32 fields inside `Damage`/`Heal` events. Integer state remains strict-equal always.

### D11. (NEW) Cross-log correlation via session_id = folder name

Engine trace `init` line carries `session_id: String` = the folder name (e.g. `"20260517T143022_main_cave_01_combat_a"`). AI log header carries the same `session_id` when the bridge creates the log files. AI log per-decision entries gain optional `engine_step_range: Option<(u64, u64)>` (`[start_step, end_step_exclusive)`) populated by the bridge between AI tick and engine apply.

**Cross-tool join:** External script can match AI decision N with engine steps `[start, end)` via `(session_id, step_range)`. Since both files live in the same folder, the tool can also simply look at the sibling — `session_id` keeps each file self-describing if extracted/moved.

**No UUID** — folder name is unique by construction (timestamp + scenario + encounter; collision would require two combats starting in the same second with the same identity). Saves a dep, keeps logs human-readable.

**Renaming risk:** if someone renames the folder, embedded `session_id` becomes stale. Document: "session_id should match folder name; tools may warn on mismatch but won't fail".

### D12. (NEW) Engine purity contract

Engine `step()` must be pure given `(state, action, rng, content)`. Forbidden imports inside `crates/combat_engine/`:
- `std::time::{SystemTime, Instant}` — non-deterministic clock.
- `std::env` — environment varies per host/run.
- `std::process::{id, Command}` — varies per process.
- `thread_local!` — non-deterministic ordering under multi-threaded callers.

**Verification:** `tests/engine_purity.rs` greps `crates/combat_engine/src/**/*.rs` for the forbidden imports. Any find: test fails with the line + suggested mitigation.

Rationale: replay determinism (D10) assumes purity. Without an automated check, future contributors will silently break it.

---

## 6. Sub-step kickoff order

Strict order: 5a ✓ → 5b → 5c → 5d → 5e → 5f → 5g.

Each remaining sub-step:
1. `cargo check --all-targets` green.
2. Sub-step's targeted tests green.
3. Full suite (`cargo test`) green.
4. Commit with `ai/unisim Phase 5 step Nx: <title>`.
5. User review before next sub-step.

**Rationale for ordering:**
- **5a ✓** — additive serde + BTreeSet + helpers. Shipped.
- **5b** — breaking trait change. Land before bridge wiring so every `step()` callsite updates in one pass. Engine purity audit + ApplyCtx::rng_calls here.
- **5c** — TomlContentView standalone. Useful before replay binary so parity tests pin the contract.
- **5d** — wires writer; folder layout migration happens here (filesystem break); legacy `CombatEvent` log still drives UI; session_id retrofits into `InitLine`. AI log gains correlation fields.
- **5e** — activates replay; first end-to-end determinism check. In-process scenarios; no playtest.
- **5f** — fuzz. Long-running; run in background; fix panics one at a time. CI integration deferred.
- **5g** — retrospective + replay_ai_log cleanup + tag.

---

## 7. Gate criteria (Phase 5 → Phase 6)

| # | Criterion | Verification |
|---|---|---|
| 1 | All engine types round-trip via serde with byte-equality | `crates/combat_engine/tests/serde_roundtrip.rs` ✓ |
| 2 | RNG call-count accurate: `step(Action::Cast { targets: N })` consumes exactly N rolls (§6.4) | Engine test in 5b |
| 3 | `aura_membership_set` BTreeSet; replay byte-equal across 100 re-runs on recording host with identical seed | Parametrized `tests/replay.rs` ✓ (BTreeSet) + 5e (replay loop) |
| 4 | Replay determinism on recording host: re-run from trace produces identical final state hash + Event sequence + RNG count on all 5 canonical scenarios with `--tolerance 0` | `tests/replay.rs` 5e |
| 5 | `TomlContentView` parity with `EcsContentView`: `ability_def`/`status_def`/`auras_of`/`check_phase_trigger` results identical for every TOML entry | `tests/toml_content_view_parity.rs` 5c |
| 6 | `cargo +nightly fuzz run step_random_actions` → 10M iterations zero engine panics | Local + `fuzz/README.md` 5f |
| 7 | Engine purity audit: zero forbidden imports inside `crates/combat_engine/src/` (D12) | `tests/engine_purity.rs` 5b |
| 8 | `replay_ai_log` stale `ver != 27` gate fixed; works on current AI log version; reads from `logs/*/ai.jsonl` | Manual run; grep clean of `27` in `replay_ai_log.rs` 5g |
| 9 | `engine_trace.jsonl` size: measured + locked after first canonical scenario records (replace placeholder estimate); document size-per-round metric | Bench in 5e |
| 10 | Cross-host replay with default `--tolerance 1.0` warns but does not panic on a known-divergent f32 case | Manual: stub the divergence in a test 5e |
| 11 | Both `<dir>/engine.jsonl` init line AND `<dir>/ai.jsonl` header carry the same `session_id` (= folder name); AI decision entries optionally back-reference engine step ranges | Integration test 5d |
| 12 | Filesystem layout: `logs/<fight_id>/{ai,engine}.jsonl` present after combat; folder name matches `session_id` in both files | Integration test 5d |
| 13 | `process_action_system` param count stays ≤14 (Phase 4 gate carry-over) | Code review 5d |
| 14 | Bridge integration test: full combat encounter produces a trace that re-runs deterministically end-to-end | `tests/combat_engine/bridge_smoke.rs` extension 5e |
| 15 | Full `cargo test` suite green; manual playtest of one encounter produces a trace file that replays cleanly | CI + manual 5g |

---

## 8. Known gotchas

- **Engine purity (D12).** Any `SystemTime`/`std::env`/`thread_local` read inside engine breaks replay silently. `tests/engine_purity.rs` is the automated guard; treat any future find as a bug.
- **f32 FMA non-determinism (D10).** `final_damage_f32` is known. Audit any future f32 chain; if cross-host matters, fix at design time rather than via tolerance.
- **HashMap/HashSet iteration in engine code.** `aura_membership_set` BTreeSet swap (5a ✓) addressed the only known site. Audit post-5b via Serena `find_referencing_symbols HashMap` inside `crates/combat_engine/`; document each as "iteration order never exposed" or "switch to BTree".
- **Synthetic `UnitId` allocation seeding.** `next_synthetic_uid` must be serialized in `init` line so replay seeds the same counter. Already in §5 D1 init shape.
- **RNG seed-vs-stream drift (D2 contract).** Adding any new `rng.roll(...)` mid-action breaks all old traces. Document at top of `dice.rs`: "engine change that adds/removes RNG calls = schema bump". Gate item 2 enforces per-step count match.
- **Bevy `Entity` IDs never reach the engine trace.** Engine has zero Bevy dep (§6.7); only `UnitId(u64)`. Reconfirm in 5d code review.
- **`post_state_hash` couples to `Unit` serde shape.** The hash internally serializes `Unit` to JSON; adding a `Unit` field changes all hashes. Force schema bump on any `Unit` field addition. Document in `trace.rs`.
- **Strict TargetGone under fuzz.** Fuzz generates sequences that legally `Err(ActionError::TargetGone)`. Trace must record `Err` outcomes; replay must reproduce identical `Err`. Otherwise "no panic" property degrades to "no panic except expected ones" — fragile.
- **ContentView TOML drift (D3).** Hash mismatch warns by default; `--strict-content` escalates. Document in `replay_engine_trace.rs --help`.
- **Multi-actor `ActionInput::EndTurn` ordering.** Trace preserves engine step order, not Bevy frame order. Document.
- **Projection drift is out of replay's scope.** `apply_phase_transitions_system` + `project_state_to_ecs` run OUTSIDE `step()` and mutate ECS. Replay validates engine state + event stream; NOT ECS components post-projection. Phase 6 may add a projection-parity gate.
- **`cargo fuzz` requires nightly.** `fuzz/README.md` documents. CI integration deferred.
- **`fuzz/` workspace exclusion.** Root `Cargo.toml` `[workspace]` gets `exclude = ["fuzz"]`. The `fuzz/` crate has its own `Cargo.toml` with no `[workspace]` field.
- **Trace file rotation.** Long combats (50+ rounds) produce multi-MB traces. Phase 5 ships no rotation; one file per combat. Document; propose rotation in Phase 6 if size becomes a problem.
- **`Vec<Unit>` final-state equality requires stable serde field order.** Use `#[serde(rename_all = "snake_case")]` consistently; lock field order in `Unit` struct (do NOT reorder for "readability" without a schema bump). Per gotcha "`post_state_hash` couples to `Unit` serde shape".
- **BTreeSet ordering by `(UnitId, UnitId, StatusId)` triple.** `UnitId(u64)` orders by `u64`; `StatusId(String)` orders lexicographic. Both stable. Confirm `StatusId` derives `Ord` correctly. ✓ (5a)
- **TomlContentView parity drift.** `EcsContentView` may diverge over time (Bevy asset loader defaults, etc.). Parity test (gate 5) catches; treat parity failures as engine bugs.
- **Trace truncation on crash.** If recording crashes mid-step, last JSONL line is partial. Replay must `if line.is_empty() || !line.ends_with('\n') { truncate and warn }`. Document.
- **`session_id` ↔ folder name skew.** Files store `session_id`; folder names may be renamed externally. Replay should warn if folder name mismatches the embedded `session_id` but continue (the embedded value is authoritative for cross-log joining).
- **AI log filesystem migration is a hard break.** Old flat-layout `logs/*.jsonl` files become orphans under the new `logs/<dir>/ai.jsonl` layout. No migration script; old files stay readable from `unisim/phase4-complete` checkout (D5 policy).

---

## 9. Retrospective

(Filled at Phase 5 close.)

---

## 10. Post-5a discoveries

After shipping 5a, re-examination + user feedback surfaced six findings that reshaped the plan:

1. **`InitLine` needs a `session_id` field.** 5a defined `InitLine` without one; D11 (cross-log correlation) requires it. Field is additive — existing serde derives + parsers absorb it without breakage. Lands in 5d.

2. **AI log v37 bump was bogus coupling.** Phase 4 retro flagged "mining v36+ migration" as a follow-up; I bundled it into 5f for no good reason. AI log content doesn't change in Phase 5. Decoupled: engine trace starts at v1; AI log keeps its own counter and bumps only when its content actually changes.

3. **`step()` RNG reporting locked to `ApplyCtx::rng_calls`** (D2 implementation note). Earlier draft left "return-shape change OR ApplyCtx extension" open. Locking to `ApplyCtx` matches the existing 4d precedent (`phase_entered`) and avoids touching every `step()` callsite for a side-channel datum.

4. **"Final state full-equal via `==` on `CombatState`" was misleading** (D8 clarified). Trace doesn't record final state; final-state assertion = last step's `post_state_hash` match. Recording final state separately would be redundant with the per-step hash sequence.

5. **Engine purity needs an automated guard (D12).** Replay determinism assumes engine is pure given `(state, action, rng, content)`. `std::time`/`std::env`/`thread_local` reads would silently break replay. Manual review can't catch future contributions; `tests/engine_purity.rs` greps for forbidden imports as a CI gate. Lands in 5b.

6. **Folder-per-fight filesystem layout (user feedback).** Original plan put engine trace in `logs/engine/<session_id>.jsonl` and AI log in `logs/<combat>.jsonl` — two unrelated locations. Cleaner: one folder per fight (`logs/<fight_id>/`) holding both `ai.jsonl` and `engine.jsonl`. Folder name carries fight identity; UUID generation drops out (folder name string IS the `session_id`). Single combat-start hook owns both writers. Migration is a hard break: old flat-layout AI logs become orphans (consistent with D5 no-legacy-retention policy).

Plus one cross-cutting note: **5a over-delivered.** `trace.rs` already includes `post_state_hash`, parsers (not just serializers), and full `InitLine`/`StepLine` definitions. 5d and 5e have less wiring work than the original plan suggested. Net good; documented here so the 5d/5e implementer doesn't repeat the work.

7. **Option (A) for per-combat state: engine `Unit` absorbs equipment/auras/enemy_phases; `ContentView` trait contracts to static-only.** Audit of `EcsContentView` (post-5b) revealed that 4 of its 8 trait methods return PER-COMBAT-INSTANCE data, not static content: `auras_of`, `check_phase_trigger`, `caster_context`, `aoo_dice`. These are currently built from ECS components (`AuraSource`, `EnemyPhases`, `Equipment`) into precomputed maps on `EcsContentView`. Replay couldn't access them through a pure-Rust `TomlContentView` because the data isn't in TOML.

   Option (A) absorbs the data into engine `Unit` (`caster_context: CasterContext`, `auras: Vec<AuraDef>`, `enemy_phases: Vec<PhaseEntry>`). The 4 trait methods are removed; engine code reads `state.unit(id).<field>` directly. `EcsContentView` shrinks; `TomlContentView` (5c.2) doesn't need them either. `init_state_from_ecs` is the only site that populates the new Unit fields from ECS components.

   Architectural benefit: `ContentView` trait now means what it says (static content). Per-combat state lives where it should (in `CombatState`). Replay determinism becomes trivial: same Unit input = same behaviour, no precomputed map mismatches.

   Cost: ~3-5 days for the 5c.1 refactor (Unit field additions touch every Unit construction site in 5a/5b tests; engine call-site migrations; bridge `init_state_from_ecs` extension; `EcsContentView` trim). 5c.2 (TomlContentView) becomes ~1 day after.

   Locked over options (B) inline per-combat state in trace and (C) explicit per-combat snapshot in TomlContentView constructor — both keep `ContentView` trait surface wide and create awkward "is this content or state?" ambiguity.
