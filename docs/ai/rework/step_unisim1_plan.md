# Phase 1 — Move canonical (`Action::Move` end-to-end)

**Parent plan:** `docs/ai/rework/unisim.md` §5.1
**Predecessor:** `docs/ai/rework/step_unisim0_spike.md` (Phase 0 — 5/5 ✅)
**Goal:** `Action::Move` is the canonical path on **both** real (Bevy runtime) and sim (AI planner). `CombatState` becomes the source of truth for movement state; ECS `pos`/`movement_points`/`reactions_left` become read-only projection.
**Timebox:** 2 weeks.

---

## 1. Scope

**IN:**
- New Bevy system `process_action_system` consumes `ActionInput::Move` and calls `combat_engine::step()`.
- Replaces `movement_system` (which currently mutates ECS components directly + emits `OpportunityAttack`).
- ECS components (`HexPosition`, `ActionPoints.movement_points`, `Reactions.remaining`) are written *only* by a new `project_state_to_ecs` system; everything else reads them.
- Animation handlers react to `combat_engine::Event::UnitMoved` and `Event::ReactionFired` (instead of `OpportunityAttack` / component-change observers).
- AI Move candidates score via engine (`pick_action` cones through `step()`); `sim::apply_move` shim → drop the snapshot-roundtrip layer and call `step()` directly on `CombatState`.
- `mirror_state_from_ecs` deleted — flow reverses: engine writes `CombatState`, projector mirrors to ECS.

**OUT (deferred to Phase 2+):**
- `Action::Cast` / damage / status / heal effects.
- `Action::EndTurn` and round-tick mechanics.
- Replay / log overhaul (Phase 5).
- ECS component cleanup (`#[engine_projected]` newtype enforcement — Phase 6).

---

## 2. Architecture diff vs Phase 0

```
Phase 0 (current):                           Phase 1 (target):
  ECS components (authoritative)               CombatState (authoritative)
        │                                            │
        │ mirror_state_from_ecs                      │ project_state_to_ecs
        ▼                                            ▼
  CombatState (read-only mirror)               ECS components (read-only projection)
        │                                            │
        ▼                                            ▼
  Engine step() unused on real path            Engine step() drives every Move
  movement_system mutates ECS                  process_action_system → step()
```

Engine glue:
- `Res<CombatStateRes>` stays; semantics flip from mirror → authoritative.
- `UnitIdMap` stays; same role.
- `from_ecs` becomes `init_state_from_ecs` — called *once* at combat start, not every frame.

---

## 3. File-level change list

| File | Change |
|---|---|
| `src/combat/movement.rs` | Delete `movement_system` + `OpportunityAttack` event; keep path/passability helpers (consumed by `process_action_system`'s pre-validation if needed) |
| `src/combat/pipeline.rs` | Register `process_action_system` in place of `movement_system`; add `project_state_to_ecs` to `PostUpdate` |
| `src/combat/engine_bridge.rs` | Add `process_action_system` + `project_state_to_ecs`; delete `mirror_state_from_ecs` |
| `src/combat/mod.rs` | Re-export and module wiring |
| `src/combat/ai/plan/sim.rs` | Drop `snapshot_to_combat_state` + `project_engine_to_snapshot` round-trip from `apply_move`; sim works on a cloned `CombatState` directly |
| `src/combat/ai/plan/snapshot.rs` | Stop building `BattleSnapshot` from ECS for the Move path; AI clones `CombatStateRes` instead (Cast still uses snapshot until Phase 2) |
| `src/animation/*` (TBD) | Observers / readers switch from `OpportunityAttack` to `combat_engine::Event::ReactionFired` |
| `src/combat/messages.rs` | `MoveUnit` → renamed/repurposed as `ActionInput::Move` |
| `tests/engine_parity.rs` | Expand: 8 scenarios from spike §6 + Phase-0 retro additions become long-term regression set |
| `tests/aoo.rs`, `tests/movement.rs`, `tests/golden_smoke.rs` | Pass without modification (gate criterion) |
| `benches/engine_move.rs` | New baseline captured post-removal of snapshot roundtrip |

---

## 4. Implementation order

Land each step as its own commit; CI green before next.

1. **`ActionInput::Move` message + `process_action_system` skeleton.** No engine call yet — system just receives the message and logs. Wire into pipeline next to `movement_system` (both run; old wins). `cargo test` green.
2. **Engine call inside `process_action_system`.** Build `Action::Move`, call `step()`, ignore output for now. Both systems still mutate state; assert via test that the engine call produces identical events.
3. **`project_state_to_ecs` system.** New `PostUpdate` system writes `HexPosition`, `ActionPoints.movement_points`, `Reactions.remaining`, `Vital.hp` from `CombatStateRes`. Initially no-op (engine still mirror).
4. **Flip authority for Move.** Disable `movement_system`'s ECS mutations behind a feature gate; `process_action_system` becomes authoritative. Run `tests/aoo.rs` + `tests/movement.rs` — must stay green.
5. **Delete `movement_system` + `OpportunityAttack` event.** Animation handlers migrate to `Event::ReactionFired` consumers.
6. **`mirror_state_from_ecs` → `init_state_from_ecs`.** Run once at combat start (state transition into `CombatPhase::AwaitCommand` from `StartRound`). Drop the per-frame mirror.
7. **AI sim cleanup.** `sim::apply_move` drops snapshot roundtrip — works on a cloned `CombatState` directly. `pick_action` reads `CombatStateRes` instead of building a snapshot for Move scoring (Cast still uses snapshot in Phase 1).
8. **Bench capture.** New baseline: engine Move + projector vs Phase 0 numbers. Threshold: ≤ 1.2× of pure-engine Phase 0 (1.51 µs).

---

## 5. Existing code to consult

| When writing | Read |
|---|---|
| `process_action_system` | `combat/movement.rs::movement_system`, `combat/pipeline.rs` |
| `project_state_to_ecs` | `combat/engine_bridge.rs::from_ecs` (inverse direction) |
| Animation migration | `combat/messages.rs` (`OpportunityAttack` consumers) |
| Sim cleanup | Current `combat/ai/plan/sim.rs::apply_move` shim |
| Parity test additions | `tests/engine_parity.rs` (Phase 0 baseline), `tests/parity.rs` (legacy regression suite) |

Run `ya tool ast-index outline <file>` before reading any file > 500 lines.

---

## 6. Test plan

`tests/engine_parity.rs` expands from 4 → 8+ scenarios (Phase 0 spike §6 list as the long-term regression set):

1. `move_basic` (covered by `parity_pure_move_no_enemies`).
2. `move_no_aoo_when_disengaged` (covered by `parity_move_no_aoo_stays_adjacent`).
3. `move_triggers_aoo` — new; single AoO with rage gain on both sides.
4. `aoo_kills_mover` — covered by `parity_aoo_kills_mover_mid_path_rollback`.
5. `aoo_chain_two_enemies` — covered.
6. `reaction_recursion_capped` — deferred (no recursion mechanic until reactions extend).
7. `strict_failure_target_gone` — covered by engine_step.rs.
8. `parity_sim_vs_real` — *new*: real (Bevy) path runs `process_action_system`; sim path runs `step()` directly; assert byte-equal final `CombatState`.

Existing must stay green:
- `tests/aoo.rs` (legacy AoO regression).
- `tests/movement.rs` (path/MP validation).
- `tests/golden_smoke.rs` (E2E scenario).
- `tests/parity.rs` (8 pre-existing real-vs-sim scenarios — these may need rewrite for the new event sequence; see decision 6.3).

---

## 7. Gates (pass/fail per task list §5.1)

| # | Criterion | Verify |
|---|---|---|
| 1 | `golden_smoke` green | `cargo test --test golden_smoke` |
| 2 | Phase-0 parity tests (8/8) | `cargo test --test engine_parity --test parity` |
| 3 | No playtest regressions | Manual run vs pre-Phase-1 snapshot |
| 4 | No `movement_system` left in source | `ya tool ast-index search "fn movement_system"` returns empty |
| 5 | Bench ≤ 1.2× Phase-0 engine baseline | `cargo bench --bench engine_move` |

---

## 8. Risks / flags

Updated 2026-05-13 after pre-implementation discovery pass.

- **Path-occupancy validation in `process_action_system` (resolved 2026-05-13).** Folded into engine `step()` as a pre-validate, **pure-state** check (no `ContentView` extension). Discovery: the game has no static terrain layer — all "blockers" in `src/game/hex.rs::is_passable` are dynamic enemy positions, and bounds are just a grid constant. Unit positions are already in `CombatState`, so engine has everything it needs. Contract: intermediate hex held by an enemy of the actor → `PathBlockedByEnemy { hex }`; destination hex held by anyone → `DestinationOccupied { hex }`. Friendlies in intermediate hexes pass through (matches current `find_path` rule `|h| is_passable(h, &enemy_positions)`). Sim ≡ real for free, since sim already routes through `step()`.
- **Animation timing (lower risk than feared).** `src/ui/animation.rs` does not observe `HexPosition`/`Vital` changes and does not read `OpportunityAttack`. Movement tween path is driven elsewhere — quick audit of `MoveUnit` consumers (only `combat/movement.rs:31`) confirms no animation handler is wired to the legacy event stream. Phase 1 must still wire tween triggers to `Event::UnitMoved` / `Event::ReactionFired`, but no animation-system refactor blocker.
- **`tests/parity.rs` — passes by construction.** All 8 `parity_*_real_vs_sim` scenarios exercise `SimState::apply_step` against a hand-derived formula; the "real" half is a `TODO(12.2)` (see `parity.rs:530`). Sim now routes through `step()`, so these become free regression coverage. **No rewrite needed.**
- **`tests/aoo.rs` is the actual real-side integration test.** 9 scenarios writing `MoveUnit` messages and reading `CombatEvent::OpportunityAttack` from the combat log. This is the primary test migration target in Phase 1 step 5 (legacy event removal).
- **`MoveUnit` producer audit done.** 3 callsites: `ui/hex_grid/input.rs:208` (player), `combat/ai/system.rs:324,328` (AI). No animation/UI ghost producers.
- **`OpportunityAttack` consumer audit done.** 2 non-test consumers: `combat/movement.rs:238` (emitter — being deleted), `game/combat_log.rs:61,151` (log enum + formatter). Trivial scope.

---

## 9. Rollback

Two-step revert:
1. Restore `movement_system` from `unisim/phase0-complete` tag.
2. Restore `mirror_state_from_ecs` registration; drop `process_action_system` and `project_state_to_ecs`.

Spike-style throwaway is no longer possible (Phase 1 is the production path).

---

## 10. Items to flag back

Stop and ask before proceeding if:

- **Path occupancy contract differs** between current `find_path` rules and what `step()` validation expects (friendly pass-through, terrain).
- **Animation handler refactor** needs touching > 2 files outside `src/animation/`.
- **`tests/parity.rs` cases** start failing — may indicate decision 6.3 spill-over from AoO scenarios; revisit per-target ordering scope.
- **Bench regression > 1.5×** vs Phase 0 — profile before optimizing; likely culprit is full-state projection on every frame (should be diff-driven).

---

## 11. Done = merged + gates + retro

After gates pass:
1. Append `## 12. Retrospective` with surprises, deviations, perf numbers, decisions for Phase 2.
2. Open `step_unisim2_plan.md` from §5.2 template in `unisim.md`.
3. Tag commit `unisim/phase1-complete`.
