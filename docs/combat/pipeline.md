# Combat Pipeline — Schedule & Chains

System registration lives in `src/combat/pipeline.rs::CombatPipelinePlugin`.
The plugin encapsulates `configure_sets` and `add_systems`; `main.rs` plugs
in with a single `.add_plugins(CombatPipelinePlugin)`.

For the bridge systems (`process_action_system`, `project_state_to_ecs`),
see [`bridge.md`](bridge.md).
For combat start/teardown/restart, see [`lifecycle.md`](lifecycle.md).

---

## 1. StartRound chain

Runs once per round entry (before `AwaitCommand`):

```
project_state_to_ecs
  → assign_hex_positions
  → build_turn_order
  → bootstrap_combat_state
  → write_engine_trace_init_system
```

`bootstrap_combat_state` is idempotent — only the first pass per encounter
populates engine state (see [`lifecycle.md`](lifecycle.md) for details).

---

## 2. AwaitCommand sets

Gated by `combat_ready()` (no active animations or popups). Sets chain in
order: `TurnStart → Command → Execute → Finalize`.

### TurnStart

Empty — kept as a stable hook for future systems.

### Command

```
pact_ai_system ─┐
                 ├─ .chain() (pact runs before player)
player_command_system ─┘

enemy_ai_system  (independent, same set)
```

`pact_ai_system` handles heroes under `pact_control` status (crit-fail pact
effect) — they run as AI and must intercept before `player_command`.

### Execute

```
process_action_system
  → project_state_to_ecs
  → apply_phase_transitions_system
  → flush_pending_ai_log_system
```

`process_action_system` consumes `ActionInput` messages, calls the engine
`step()`, and translates resulting events into `CombatLog` entries +
`AnimationQueue` items. `project_state_to_ecs` writes the engine state back
to ECS. `apply_phase_transitions_system` applies Bevy-only phase-transition
side effects (Name, Abilities, AxisProfile). `flush_pending_ai_log_system`
patches `engine_step_range` into pending AI log entries and writes them.

### Finalize

```
queue_enemy_popup
  → enqueue_victim_facing
  → advance_turn_system
  → check_victory_system
```

`queue_enemy_popup` scans new `CombatLog` events and emits
`PendingAnim::Popup` for enemy ability use and phase transitions.
`enqueue_victim_facing` runs immediately after — it pushes
`PendingAnim::Face` for the hostile-cast victim so the victim's face lands
*after* the popup in the queue.
`advance_turn_system` applies new statuses, advances the turn-queue cursor.
`check_victory_system` evaluates the `CombatObjective` and transitions to
`Victory` or `Defeat` if the condition is met.

---

## 3. EndTurn ownership

EndTurn handling has two distinct paths after unisim:

**Implicit (engine-emitted).** When `step(Cast)` or `step(Move)` exhausts the
actor's AP and MP, the engine cascade emits `Event::TurnEnded` followed by
`TurnStarted` for the next actor. Dead or stunned actors are also skipped
inside the engine via `Effect::AdvanceTurn` — no bridge-side skip system
exists. The bridge translates these events through
`translate_end_turn_events` and projects new actor state via
`project_state_to_ecs`. Nothing in ECS-land has to "send" EndTurn for these
cases.

**Explicit (`ActionInput::EndTurn` message).** Sent by:

| Source | When |
|--------|------|
| `command_input.rs::player_command_system` | Player presses **E**, or auto-end when both AP/MP exhausted and no usable ability remains |
| `ui/ability_panel.rs` (End Turn button) | Player clicks the explicit End Turn UI button |
| `ai/system.rs::enemy_ai_system` | Enemy AI picks `Intent::EndTurn` (no valid plan, move-only complete, or all targets out of reach) |
| `ai/system.rs::pact_ai_system` | Pact-controlled hero picks `Intent::EndTurn` (same logic as enemy AI) |

`process_action_system` consumes `ActionInput::EndTurn` and calls
`step(Action::EndTurn { actor })` once. The engine then emits the same
`TurnEnded → TurnStarted` cascade as the implicit path.

**Active combatant** is tracked via the `ActiveCombatant` marker component
on the entity (not a resource field). `advance_turn_system` moves the marker
when the engine emits `TurnStarted`.

---

## 4. Status duration rules

- Duration ticks on the **applier's** EndTurn, not the target's.
- New statuses applied AFTER existing ones are ticked (no +1 hack needed).
- Duration 1 = active until end of applier's next turn.
- `advance_turn_system` skips Dead targets when applying new statuses.

---

## 5. Animation & pipeline blocking

Game logic updates instantly (HexPositions, damage, statuses). Visual
animations run independently and block the pipeline via `combat_ready()`:

```
combat_ready() = AnimationQueue.is_empty()
              && no MovePath component on any entity
              && no EnemyActionPopup
```

When `combat_ready()` is false, the entire `AwaitCommand` chain doesn't run.
Animation systems (`process_animation_queue`, `animate_movement`,
`enemy_popup_input`) run every frame regardless.

**Movement flow:**
`process_action_system` (via engine `step(Move)`) → translate_move_events
pushes `PendingAnim::Movement` → `process_animation_queue` pops it → inserts
`MovePath` on token → `animate_movement` lerps at 0.12s/step → removes
`MovePath` when done → chain resumes.

**Popup flow:**
`queue_enemy_popup` pushes `PendingAnim::Popup` → spawned as UI overlay →
player presses Space/Esc → despawned → chain resumes.

**Facing flow (per-turn ordering invariant):**
AnimationQueue ordering within a turn = Execute-pushed items (actor-face,
movement) come before Finalize-pushed items (popup, victim-face). This
ordering holds because `apply_bridge_queues_post_projection` drains
`BridgeQueues.animations` into `AnimationQueue` during Execute, before
Finalize systems run. The actor-face (`PendingAnim::Face` for the caster)
is pushed before the cast popup so the sprite is already turned when the
popup appears. The victim-face is pushed by `enqueue_victim_facing` in
Finalize after `queue_enemy_popup`, so it resolves after the popup clears.

---

## 6. Known edge cases

### Movement before validation (intentional)

Movement is processed inside `step(Move)` before `step(Cast)` — AI writes
both `ActionInput::Move` and `ActionInput::Cast` in the same frame.
`process_action_system` consumes them sequentially: move updates
`HexPositions` inside the engine, then cast validates range against the new
position. Reversing the order would reject a valid attack (range checked
against pre-move position).

### Stunned actor skip (engine-owned)

A unit with `skips_turn = true` status (e.g. `stunned`) has its turn skipped
inside the engine: `step(Action::EndTurn)` or the natural cascade derives
`Effect::AdvanceTurn`, which skips past stunned actors when picking the next
queue position. There is no bridge-side `skip_stunned_turn_system` —
post-unisim, this is engine logic. AI and player command systems check the
active actor's status via `BattleSnapshot` / `CheckLegality` before
attempting to dispatch actions.

### Suicide via ability

If an ability deals damage to the caster, the engine marks them Dead and
derives `Effect::AdvanceTurn` — `advance_turn_system` and
`check_victory_system` handle the outcome correctly via the normal chain order.

### All enemies die in one turn

`check_victory_system` evaluates after `advance_turn_system` in Finalize.
If the last enemy dies during the Execute step, victory is detected
at the next Finalize. No special-casing needed.

---

## 7. Tests

| Test | What it covers |
|------|---------------|
| `valid_use_ability_emits_validated_action` | Valid ActionInput → engine step succeeds |
| `wrong_actor_use_ability_is_rejected` | Non-active actor → step returns error |
| `no_action_point_use_ability_is_rejected` | `ap.action = false` → step returns error |
| `apply_damage_reduces_hp` | Damage effect reduces hp (armor = 0) |
| `killing_all_enemies_sets_victory_phase` | All enemies dead → `CombatPhase::Victory` |
| `killing_all_heroes_sets_defeat_phase` | All heroes dead → `CombatPhase::Defeat` |
| `turn_ending_flag_cleared_on_advance` | EndTurn advances queue, `turn_ending` reset |
| `stunned_unit_skips_turn_and_stun_expires` | Stun skips turn, ticks on applier's EndTurn |
| `stunned_enemy_no_duplicate_end_turn` | Stunned enemy doesn't double-advance queue |
