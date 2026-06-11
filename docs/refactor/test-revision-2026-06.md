# Test-suite revision — June 2026

> Status: **PLAN — not started.** Follow-up to the (fully completed)
> [test-suite-audit-plan.md](test-suite-audit-plan.md) and the partially-done
> [helpers-normalization-plan.md](helpers-normalization-plan.md) (H1/H2/H4 still
> pending). This pass is a fresh full read of the suite (~16k of 26.6k LOC in
> `tests/` read line-by-line; the rest structurally scanned — see §6 Coverage).

Canonical run (must stay green throughout):
`cargo nextest run --workspace --features dev`.

---

## Phase overview

| Phase | What | Effort | Risk | Status |
|---|---|---|---|---|
| **R1** | Test honesty — tests that don't verify what they claim | M | Medium (touches semantics) | ⬜ |
| **R2** | `StubContent.with_template` + canonical `template()` helper | S | Low | ⬜ |
| **R3** | `bevy_ability()` / `bevy_status()` wrappers | S | Very low | ⬜ |
| **R4** | Mechanical dedup (trap merge, NoContent, copy-paste, case helpers) | M | Low | ⬜ |
| **R5** | Unified headless-app builder | L | Medium (7+ callsites) | ⬜ |

Ordering: R1 first (semantic fixes are independent and highest-value), R2 before
R4 (several R4 items depend on the template-capable stub), R5 last (largest
blast radius, do when the resource set is stable).

---

## R1 — Test honesty (priority 1)

Tests that pass today but do not verify the property their name/comment claims.

### R1.1 `src/combat/ai/plan/parity_tests.rs` — 7 × `*_real_vs_sim` have no "real" leg

None of the Layer-1b tests (`parity_haste_speed_real_vs_sim`,
`parity_armor_buff_mitigation_real_vs_sim`, `parity_aoo_real_vs_sim`,
`parity_aoo_decrements_reactions_real_vs_sim`, `parity_rage_real_vs_sim`,
`parity_rage_aoe_real_vs_sim`, `parity_aoo_grants_rage_real_vs_sim`) ever call
`combat_engine::step()`. They run only `sim.apply_step(...)` and assert against
hand-computed constants — i.e. they are sim rule tests, not parity tests. If the
engine drifts, they stay green.

**Fix (pick one per test):**
- (a) Add the real leg: build the equivalent `CombatState`, run
  `combat_engine::step()` with `ExpectedValue`, diff sim deltas vs engine deltas.
- (b) Rename to `sim_<rule>` (e.g. `sim_haste_applies_speed_bonus`) and drop the
  parity claim from the doc comments.

Decision needed per dimension; (a) is the original intent per
[testing.md §9](../testing.md) ("True full-app real-vs-sim parity → NOT YET
IMPLEMENTED").

### R1.2 `tests/combat_engine/legality_parity.rs` — test 1 asserts parity only

The 12 cases in `legality_parity_bevy_vs_engine` assert
`bevy_result == engine_result` but never the expected outcome; the per-case
comments ("— Ok", "— OutOfRange", …) are unverified. If both adapters share a
bug, the test is green. Worse: case 1 "attack_in_range — Ok" runs with a live
taunter in the roster, so the actual agreed result is almost certainly
`Err(TauntForcesTarget)` — the comment is wrong *today*.

**Fix:** extend each case tuple with `expected: Result<…>` and assert all three
(bevy == engine == expected), mirroring what `multi_taunter_both_are_legal_targets`
already does. Re-derive the true expected values (taunt interferes with cases
1–10 — either move taunter cases into a separate roster or document the
interaction).

### R1.3 `tests/combat_engine/serde_roundtrip.rs::state_empty_blocked_hexes_roundtrip`

Comment claims "deserialises correctly even when the field is absent from old
JSON", but the test serialises with the *current* serialiser (field present).

**Fix:** hand-craft the JSON without `blocked_hexes` (string literal or
`serde_json::Value` surgery) and deserialise that.

### R1.4 `tests/toml_content_view_parity.rs` — reference is a copy, not the real mapping

The file replicates `EcsContentView`'s mapping inline because it's `pub(crate)`
(admitted in the header). Drift between the copy and the real mapping is
invisible to the test.

**Fix:** make the real mapping testable — `#[cfg(test)]` re-export, a
`pub(crate)` white-box inline test in `engine_bridge.rs`, or a small
`pub`-for-tests facade. Then compare `TomlContentView` against the *actual*
`EcsContentView` output.
**Bonus:** derive `PartialEq` on `EffectDef` (and friends) to delete the ~60
lines of manual `abilities_eq`/`effect_eq`/`statuses_eq`.

### R1.5 `tests/combat/handoff.rs::entity_for_uid_lookup_works_for_summoned_units`

Half-vacuous: empty `CombatState`, both assertions are "None for an absent uid"
(the second is trivially true). The no-panic regression it pins is covered more
meaningfully by `summoned_unit_can_act_in_ai_turn` in the same file.

**Fix:** fold the synthetic-uid no-panic assertion into
`summoned_unit_can_act_in_ai_turn` and delete the test.

### R1.6 `tests/combat_engine/step.rs::StubContent::with_weapon(_d)`

The `DiceExpr` argument is ignored (post-5c.1 the engine reads `Unit.aoo_dice`).
Documented, but every callsite reads as if the dice matter.

**Fix:** drop the parameter (`StubContent::with_weapon()` / plain `StubContent`),
keep the callsite comment where the semantic marker is useful.

---

## R2 — Template support in the shared stub (unblocks most of R4)

**Root cause:** `tests/common/engine_unit.rs::StubContent::unit_template()`
always returns `None`, so every file needing templates grew its own stub.

**Changes:**
1. Add `templates: HashMap<String, UnitTemplate>` + `with_template(id, tpl)` to
   the shared `StubContent`.
2. Add `pub fn template() -> UnitTemplate` (canonical regen map, empty pools —
   the exact ~35-line `enum_map!` litany currently copy-pasted ~10× across 8
   files) plus a couple of fluent mutators or just "take and mutate" style.

**Then migrate (one file per commit):**
- `tests/combat_engine/initiative.rs` — delete local `SummonContent` + `summon_template`.
- `tests/combat_engine/replay.rs` — delete local `SummonStub` (and the two local
  `NoContent`s → `StubContent::new()`).
- `tests/temporary_ally_e2e.rs` — delete `StubWithTemplate`.
- `tests/combat_engine/effect.rs` — replace `test_template()` / `melee_template()`
  bodies with `template()`-based construction (the local `StubContent` with
  `status_bonuses` overrides may stay; only the template part moves).
- `tests/combat_engine/cast.rs` — replace `imp_template()`.

---

## R3 — Bevy content-def wrappers

The ~28-line Bevy `AbilityDef` wrapper (`magic_domains: vec![]`,
`magic_method: String::new()`, `ai_tags_override: None`, `is_move_toggle: false`
around an `engine:` block) is repeated ~15×: `bridge_cast.rs` ×6,
`bridge_phase.rs` ×4, `legality_parity.rs` ×3, `parity_tests.rs` ×7 (plus the
Bevy `StatusDef` wrapper in 4+ places).

**Changes:**
1. `tests/common/apps/bridge.rs`:
   `pub fn bevy_ability(id: &str, name: &str, engine: combat_engine::AbilityDef) -> AbilityDef`
   and `pub fn bevy_status(id: &str, engine: combat_engine::StatusDef) -> StatusDef`.
2. Same pair in `src/combat/ai/test_helpers.rs` for inline AI tests
   (`parity_tests.rs` can't see `tests/common`).
3. Migrate callsites mechanically; engine-side `AbilityDef` literals stay as-is
   (struct-update from `single_enemy_ability()`-style bases is already idiomatic).

Also in `parity_tests.rs`: replace the `ContentView { …every field…,
..ContentView::default() }` literal with plain `ContentView::default()` + inserts.

---

## R4 — Mechanical dedup & cleanups

| # | File | Change |
|---|---|---|
| R4.1 | `tests/combat_engine/trap.rs` | Make `run()` return `(state, events, ctx)`; merge `trap_fires_on_pass_through` + `trap_on_arrival_triggers_and_halts` + `trap_on_arrival_sets_interrupted_flag` into one test (dup admitted in comments). Replace the two in-test `struct Multi` ContentViews and `NoContent` with the shared stub (needs R2 for `with_ability` pairs). Add a tiny `circle_reveal_ability(radius)` helper to kill the 3× throwaway-`AbilityDef`-for-`aoe_radius` trick. |
| R4.2 | `tests/combat_engine/cast.rs` | Delete duplicated `actor.pools[Mana] = Some((10,10));` lines (291–292, 926–927). Replace `StubContent::with_caster` + `apply_caster_contexts` (legacy from ContentView-era CasterContext) with `EngineUnitBuilder::caster_context()` at build time. |
| R4.3 | `tests/common/engine_unit.rs` | Add pool readers `pub fn ap(&CombatState, u64) -> i32`, `mp`, `mana`, `rage` — replaces the 6-line `pools[…].map(…).unwrap_or(0)` construction repeated dozens of times in `cast.rs` / `step.rs` / `bridge_cast.rs`. Migrate opportunistically (when touching a file), not as a sweep. |
| R4.4 | `tests/combat_engine/legality_parity.rs` | `case(name, (actor, actor_uid), (target, target_uid), pos, &ability)` helper for the mirrored `ProposedAction` pairs (~30 → ~3 lines per case) + `spawn_combatant(app, team, pos, abilities, mutate)` for the 8×15-line spawn blocks. Do together with R1.2. Expected: 1008 → ~450 LOC. |
| R4.5 | `tests/combat/ai_snapshot.rs` | One generic `with_snapshot<R>(app, team, extract: impl Fn(&BattleSnapshot) -> R) -> R` (move-closure system or `run_system_once_with`) replacing 5 copies of the 10-arg `snapshot_system` and both arms of `env_count_for_team`. Expected: ~615 → ~350 LOC. |
| R4.6 | `tests/common/` + `tests/combat/handoff.rs`, `tests/combat/aoo.rs` | `preset_initiative(app, &[("Hero", 20), ("Enemy", 5)])` helper (8-line block × ~8 sites) and `run_start_round_chain(app)` for the `build_turn_order → bootstrap_combat_state → apply_bridge_queues_pre_projection` triple (×4 in handoff.rs). |
| R4.7 | `tests/combat_engine/serde_roundtrip.rs` | Group the 46 five-line tests per type family (one test per enum iterating variants via the existing `roundtrip()`): ~8 tests, adding a variant = one line. Failure output already prints the value. |
| R4.8 | `tests/combat_engine/bridge_phase.rs` | Extract shared boss-with-phase setup for the two phase-transition tests (~80 duplicated lines); `PhaseDef` literals differ only in `trigger`/`tags`/`heal_to_full` → local `phase(trigger) -> PhaseDef` base + struct-update. Depends on R3. |
| R4.9 | `tests/combat_engine/bridge_cast.rs` | The two summon tests share ~100 lines of ability+template setup → extract local `summon_fixture()`. Depends on R2/R3. |
| R4.10 | `tests/temporary_ally_e2e.rs` | Relocate the two pure-engine tests (`magister_skips_turns`, `apply_initial_statuses_engine_side`) to `tests/combat_engine/` per the layering rule in [testing.md §1](../testing.md). |
| R4.11 | `tests/engine_step_range_correlation.rs` | Temp files leak on assertion failure — switch to a drop-guard (`tempfile` dev-dep or a tiny RAII wrapper). |
| R4.12 | `tests/combat_engine/tags.rs` | Check whether the hand-rolled `StubState` (10-method `ActionState` impl mirroring `EngineCheckState`) can be replaced by the real `EngineCheckState` as in `legality_parity.rs`; if not (tag override needed), document why above the struct. |

---

## R5 — Unified headless-app builder (do last)

The ~25-line `init_resource` litany is duplicated in 7+ places:
`tests/common/apps/engine.rs::movement_app`, `tests/common/apps/bridge.rs::bridge_app`,
`tests/engine_step_range_correlation.rs::correlation_app`,
`tests/init_fight_equivalence.rs::scenario_app`, `tests/combat/mana_gear.rs::spawn_app`,
`tests/combat_engine/forecast.rs`, `tests/combat_engine/loadout_overlay.rs`,
`tests/encounter_toml_v2.rs`. Every new bridge resource = 7 edits.

**Shape:** one base builder in `tests/common/apps/` covering the shared resource
set, with options layered on top (state machine yes/no, bridge systems yes/no,
content: default / loaded / injected, AI-log resources yes/no). The existing
`movement_app` / `bridge_app` become thin wrappers — public APIs unchanged, so
no test bodies move.

**Risk note:** subtle ordering differences between the current builders
(e.g. `movement_app` enters `AwaitCommand` at build time, `bridge_app` has no
state machine) must be preserved as explicit options, not silently unified.

---

## Explicitly NOT doing

- **Consolidating regression pins** in `handoff.rs` and friends — forbidden by
  [testing.md §4](../testing.md); only their *setup* is shared (R4.6).
- **Touching `init_fight_equivalence.rs`** — complexity is justified
  (id-remap, tie-break semantics, failure aggregation). Open question recorded:
  if `init_fight` becomes the only bootstrap path, the test is retired with it.
- **Renaming pre-existing tests for naming-convention compliance** —
  `git blame` integrity wins (testing.md §5). R1 renames are semantic fixes,
  not style.
- **Per-file `make_unit` wrappers over `EngineUnitBuilder`** — domain defaults
  per file are fine; not duplication.
- **`record_then_replay`, `run_cast_log_test`, `trap.rs::run()`,
  `common/apps/bridge.rs`** — exemplary harnesses, leave alone.

---

## Coverage of this revision

Read line-by-line: all of `tests/common/`, `effect.rs`, `cast.rs`, `step.rs`,
`legality_parity.rs`, `trap.rs`, `handoff.rs`, `bridge_phase.rs`,
`bridge_cast.rs`, `replay.rs`, `serde_roundtrip.rs`, `initiative.rs`,
`init_fight_equivalence.rs`, `engine_step_range_correlation.rs`,
`toml_content_view_parity.rs`, `temporary_ally_e2e.rs`, `aoo.rs`,
`ai_snapshot.rs`, `mana_gear.rs`, `dice.rs`, plus targeted sections of
`parity_tests.rs`, `test_helpers.rs`, `tags.rs`.

Structurally scanned only (test density, stubs, helper dup — **not** vetted
line-by-line; a future pass may find more): `phase.rs`, `end_turn.rs`,
`aura.rs`, `bridge_movement/projector/trace.rs`, `forecast.rs`, `preview.rs`,
`determinism.rs`, `hot.rs`, `targeting.rs`, `turn_queue.rs`, `reaction.rs`,
`state.rs`, `rng_count.rs`, `loadout_overlay.rs`, `trace_helpers.rs`,
`aura_determinism.rs`, `purity.rs`, `phase_tags.rs`, `env_ownership_e2e.rs`,
`phase_deadline_e2e.rs`, `choice_scene_e2e.rs`, `los_parity.rs`,
`los_ai_e2e.rs`, `encounter_toml_v2.rs`, `replay_diff_smoke.rs`,
`projection_isolation.rs`, `golden_smoke.rs`, `tests/combat/{equipment,movement,
ai_scenarios,replay_assert,ai_no_abilities}.rs`, `snapshot_tests.rs`,
`ai/log/mod.rs` inline tests.

## General verdict (June 2026)

The suite is healthy: correct layering, scripted RNG everywhere, spec-style
names, annotated regression pins, near-zero tautological tests. The problems are
(1) a handful of tests whose names/comments overpromise (R1) and (2) systemic
boilerplate duplication (~2–3k deletable LOC) concentrated in content-def
literals, app builders, and template litanies (R2–R5).
