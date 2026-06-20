# Combat Bridge — src/combat/bridge/

`src/combat/bridge/` is the single module that talks to
both Bevy ECS and the pure engine (split into per-concern submodules: `ids`, `bootstrap`, `content_view`, `translate`, `process`, `project`, `queues`, `phases`; all items re-exported flat from `mod.rs`). Nothing else may call `step()` directly
from the game side (only the AI sim does, via `plan/sim.rs`).

For the engine internals (`step`, `CombatState`, `ContentView`, determinism),
see [`engine.md`](engine.md).
For the system schedule and turn chains, see [`pipeline.md`](pipeline.md).
For combat start/teardown/restart flows, see [`lifecycle.md`](lifecycle.md).

---

## 1. Layer overview

```
              ┌─────────────────────────────────┐
              │           UI / Input            │
              │  src/ui/, src/combat/command_input.rs
              └──────────────┬──────────────────┘
                             │ ActionInput messages
                             │ (Move / Cast / EndTurn)
                             ▼
              ┌─────────────────────────────────┐
              │  Bridge — src/combat/bridge/     │
              │  process_action_system           │
              │  project_state_to_ecs            │
              │  translate_events / translate_one│
              │  bootstrap_combat_state          │
              └──┬──────────────┬──────────┬────┘
     step(state, │              │ events   │ projection
     action, rng,│              ▼          ▼
     content)    │         CombatLog   ECS components
                 ▼         (UI events) (Vital/AP/...)
     ┌───────────────────────┐
     │     Engine (pure)     │
     │  crates/combat_engine/│
     └───────────────────────┘
                 ▲
                 │ same step() API
                 ▼
     ┌───────────────────────┐
     │   AI — src/combat/ai  │
     │  plan/sim.rs          │
     └───────────────────────┘
```

**Three concerns:**
1. **Engine** — pure Rust, no Bevy, authoritative state.
2. **Bridge** — the only place that touches both sides.
3. **Consumers** — AI and UI read projections; they never write engine-owned data.

---

## 2. System table

### Lifecycle

| System | Schedule | Role |
|--------|----------|------|
| `bootstrap_combat_state` | `CombatPhase::StartRound` chain, after `build_turn_order` | Seed `CombatStateRes` from ECS — **once per encounter** (idempotent via `units().is_empty()` guard) |
| `reset_engine_mirrors_on_exit_combat` | `OnExit(AppState::Combat)` | Clear `CombatStateRes` / `UnitIdMap` / `PendingPhaseTransitions` on Victory/Defeat |
| `reset_engine_mirrors_on_restart` | `Update`, reads `RestartCombat` | Same clear, fires on in-place restart without AppState transition |

### Action processing

| System | Schedule | Role |
|--------|----------|------|
| `process_action_system` | `CombatStep::Execute`, first in chain | Consume `ActionInput` messages, call `step()`, translate events into `BridgeQueues` |
| `apply_bridge_queues_pre_projection` | `CombatStep::Execute`, after `process_action_system` | Drain deaths + turn-lifecycle queues (Dead marker, ActiveCombatant) |
| `project_state_to_ecs` | `CombatStep::Execute`, after pre-projection apply | Write engine state back to ECS components (D6 contract) |
| `apply_bridge_queues_post_projection` | `CombatStep::Execute`, after `project_state_to_ecs` | Drain animations + phase queues; `apply_phase_ecs_writes` for `PhaseEntered` (Name, Abilities, AxisProfile) |

### Event translation (called from process_action_system)

Post-Phase A, translation is unified into two functions:

| Function | Role |
|----------|------|
| `translate_events(events, &mut ctx)` | Iterates `Vec<Event>`; calls `translate_one` for each |
| `translate_one(event, &mut ctx)` | Single exhaustive `match` over all `Event` variants (`bridge/translate.rs`) |

`TranslateCtx<'a>` bundles all mutable output sinks:
- `log: &mut CombatLog` — text entries for the combat log
- `id_map: &UnitIdMap` — engine ↔ ECS entity mapping
- `queues: &mut BridgeQueues` — deferred side-effect queues
- `cast: CastCtx` — per-cast accumulator (ability, caster entity)
- `move_: MoveCtx` — per-move accumulator (path, entity)

The four former functions (`translate_move_events`, `translate_cast_events`,
`translate_end_turn_events`, `translate_tick_events`) were collapsed into
`translate_one` in Phase A (`a7048f6`).

Translators do NOT write engine-projected components. They produce
`CombatLog` entries, populate `BridgeQueues`, and build animation/popup data
for UI consumption.

### BridgeQueues

`BridgeQueues` (Resource, post-Commit 1 consolidation `505ffa7`) groups the four
formerly-separate `Pending*` Resources into one:

| Sub-field | Drained by | Contents |
|-----------|-----------|---------|
| `deaths: Vec<UnitId>` | `apply_bridge_queues_pre_projection` | Units to mark `Dead` |
| `turn_lifecycle: BridgeTurnLifecycle` | `apply_bridge_queues_pre_projection` | `ActiveCombatant` inserts/removes + round-start flag |
| `animations: Vec<PendingAnim>` | `apply_bridge_queues_post_projection` | Movement animations → `AnimationQueue` |
| `phases: Vec<(UnitId, usize)>` | `apply_bridge_queues_post_projection` | Phase-transition pairs → `apply_phase_ecs_writes` |

Pre-projection queues fire before `project_state_to_ecs`; post-projection
queues fire after, so Vital/AP components are fresh when animations read them.

---

## 3. Turn lifecycle ownership

Engine `step(EndTurn)` is the **sole authority** for turn transitions. The
cascade emits `TurnEnded → [TurnSkipped]* → TurnStarted` for the next actor,
then calls `start_actor_turn` for the new actor and folds its events (AP/MP
refill, mana/energy regen, status ticks) into the same stream.

When an actor dies mid-action (e.g. AoO kills the mover), `Effect::Death` of
the turn-holder derives `Effect::AdvanceTurn` automatically — involuntary turn
end is handled by the engine, no bridge workaround needed.

An **alive** enemy with no actionable abilities (e.g. a perma-stunned non-acting
NPC like Тэо / Хорст / the accumulator, whose `Abilities` is empty) must still
relinquish its turn: `enemy_ai_system` detects the empty-abilities case and
writes `ActionInput::EndTurn { actor }` explicitly instead of returning early,
which would leave the active turn permanently stuck (combat hangs). Regression:
`tests/combat/ai_no_abilities.rs`.

`CombatStep::TurnStart` set is empty (kept as a stable hook for future
bridge-side systems). All turn-lifecycle events flow through
`process_action_system → project_state_to_ecs` in a single frame — ECS is
always in sync at frame boundary.

Two bridge-side systems deleted in V3:
- `engine_turn_start_system` — turn-start refill now flows through the engine
  cascade.
- `engine_start_first_turn_system` — round-1 first-actor priming is folded
  into `bootstrap_combat_state`.

---

## 4. ECS-projected components (D6 contract)

`project_state_to_ecs` writes these components — and **only
the `src/combat/bridge/` module** may write them (enforced by
`tests/projection_isolation.rs`):

| Component | Field(s) written |
|-----------|-----------------|
| `HexPositions` | the full position map |
| `Vital` | `.hp` (sourced from `unit.hp()` → `pools[PoolKind::Hp].current`) |
| `ActionPoints` | `.action_points`, `.movement_points` |
| `Reactions` | `.remaining`, `.max` |
| `Rage` | `.current` |
| `Mana` | `.current` |
| `Energy` | `.current` |
| `StatusEffects` | `.0` (merges engine-known + ECS-only entries) |
| `BonusMovement` | removed when `movement_points == 0` |

Allowed write exceptions documented in
`tests/projection_isolation.rs::ALLOWED_FILES`:
- `src/combat/turn_order.rs` — `Reactions` write at round start (engine
  refills `reactions_left` via `state.start_round` on round wrap; the ECS
  component is initialized here).
- `src/game/components.rs` — `Vital::apply_damage`/`apply_heal` method impls
  (dead in production after Phase 5; present only for legacy call sites).

---

## 5. EcsContentView

`EcsContentView` (defined in `bridge/content_view.rs`) is the live-combat
implementation of the engine's `ContentView` trait. It reads from
`Res<ActiveContent>` for ability and status definitions, and computes real
`StatusBonuses` (including `armor_bonus`, `speed_bonus`) from the active
scenario's status definitions. `status_bonuses` is default-implemented on top
of `status_def` (post-V4); no explicit override is needed.

Offline tools (`replay_ai_log`, `replay_engine_trace`) reuse this same app
content path via `ActiveContentData::load_layered` — there is no separate
offline parser. `TomlContentView` (`crates/combat_engine/src/toml_content_view.rs`)
is only a content-free `ContentView` stub for tests that need a trivial view.

---

## 6. Content adapter — `src/content/to_engine.rs`

Bevy content → engine type conversions were consolidated into
`src/content/to_engine.rs` in Phase B-α/β (`0813083`, `12e2fd8`). Key helpers:

| Helper | Produces |
|--------|---------|
| `ability_def(…)` | `AbilityDef` from `ActiveContent` ability entry |
| `status_def(…)` | `StatusDef` from `ActiveContent` status entry |
| `crit_fail_outcome(…)` | `CritFailOutcome` from `ActiveContent` |

Bridge is now pure translation; all content-construction logic lives in
`to_engine.rs`.

---

## 7. HexMap façade (two-layer)

`HexPositions` tracks alive units only. `HexCorpses` tracks dead units'
last positions. The `HexMap` façade composes both for queries that need
the full picture (e.g. pathfinding obstacle checks, AoO range).

- `HexCorpses.generation` is tracked by `ui_dirty_bridge` for UI invalidation
  (C-corpse / C-ui).

---

## 8. Legality tooltips

`src/combat/legality_adapter.rs` wires `combat_engine::check_legality`
against live ECS queries to power UI tooltips ("why can't I use this?").
The engine returns `Result<LegalAction, IllegalReason>`; the adapter
formats the reason for the ability panel.

---

## 9. By-design surface (NOT debt)

These items live in the bridge by design and should not reappear in "what's left" surveys.
See also `engine-migration.md §5` for the authoritative list.

| Item | Why it stays |
|------|-------------|
| `spawn_ecs_entity_from_engine_unit` in `process_action_system` | id_map insertion must be atomic with `UnitSpawned` event — a separate system creates a lookup race |
| Engine-trace writer | Records `(Action, &[Event], rng_calls, hash)`; `Action` is not in the event stream |
| `EcsContentView` | Bridge boundary: wires Bevy queries to the engine's `ContentView` trait |
| `from_ecs` heavy bootstrap mapper | Engine must not know about ECS `Equipment`/`CombatStats`/`EnemyPhases`; mapping is correct in shape |

## 10. Phase overrides — engine-authoritative (resolved)

**Principle.** Any mutation to engine-owned unit state that the engine's *derived
state* depends on — aura membership, stat aggregates, legality, the event stream —
must go through an engine `Effect` so the `step()` cascade recomputes dependents and
emits the diff events uniformly. Bridge-side "mutate an ECS component" lands the
*value* but skips the engine's recompute + event diff.

Boss phases (HP-triggered template swaps) now apply **all** engine-owned state
through the `EnterPhase` cascade. Resolution of equipment-derived scalars stays
Bevy-side (the engine crate is Bevy-free); the engine receives only resolved values:

| State | Carried by | Applied by | Recompute / events |
|-------|-----------|-----------|--------------------|
| `new_max_hp`, `heal_to_full` | `PhaseEntry` | `EnterPhase` → `SetMaxHp` (+ `Heal`) | yes |
| tags | `PhaseEntry.tags` | `EnterPhase` replaces `Unit.tags` in-arm | yes — `EnterPhase` is in `effect_changes_aura_membership`, so `AuraStatusGained/Lost` fire |
| armor / magic_resist / base_speed | `PhaseEntry.runtime: Option<RuntimeStats>` | `EnterPhase` sets `Unit.runtime` in-arm → `RefreshAggregates` recomputes effective armor/speed | yes |

`PhaseEntry.runtime` is resolved at the two bootstrap/build sites
(`bridge/bootstrap.rs`, `scenario/init_fight.rs`) from the phase template's
equipment via `equipment_armor`/`equipment_magic_resist` — the same Bevy-side
derivation used for the base unit. `apply_phase_ecs_writes` then **mirrors** the
post-`EnterPhase` engine `Unit.runtime` into the ECS `RuntimeStatsMirror`
component as a single POD assignment (`runtime.0 = engine_unit.runtime` — single
source of truth, it reads the engine and never re-derives), and re-infers
`AxisProfile` afterwards. (History: armor/speed used to be dropped at
parse time — `PhaseDef` carried only `CombatStats`, which has no armor/speed — so
`check_phase_trigger` hardcoded `new_armor: 0` and the `SetArmor` derivation was
dead; bell ch3's "armor medium→0" landed nowhere. Audit #6. `SetArmor`/`SetBaseSpeed`
effects were deleted in favour of the direct in-arm `runtime` replace.)

**Bridge keeps (correctly — not engine state):** `Name`, the flavor log entry,
`victory_override`/`turn_limit` (scenario objective/deadline), `AiBehaviorOverride`
(AI regime), and the ECS `Tags` mirror. **`ability_ids` is bridge-owned and correct:**
engine legality never gates on a stored active roster (`EngineCheckState::actor_knows_ability`
returns `true`), and the AI plan-sim re-reads the roster from the ECS-rebuilt snapshot
every decision cycle — so a phase ability swap does not drift the engine.

**ECS representation (A+ done).** The ECS now mirrors the engine grouping:
`RuntimeStatsMirror(pub combat_engine::RuntimeStats)` — a thin Bevy newtype over
the Bevy-free engine POD, so armor/magic_resist/base_speed are defined in ONE
place. `Vital` shrank to `{ hp, max_hp }`; the old separate `Speed` component was
deleted (it was vestigial — movement runs off `ActionPoints.movement_points`).
The engine→ECS sync is the single POD copy above. Guarded by the invariant test
`phase_transition_mirrors_runtime_stats_into_ecs` (`RuntimeStatsMirror.0 == engine
runtime`). The component is phase-mirrored (not per-step projected): `runtime`
only changes on `EnterPhase`, so per-step projection would re-copy an unchanging
value — the phase-entry copy is sufficient and keeps `projection_isolation` clean.

**Aura recompute trigger (resolved).** The ad-hoc `matches!(effect, MovePosition | Death)`
snapshot guard is now the named predicate `effect_changes_aura_membership(effect)`
(`step.rs`), which enumerates the membership-input-mutating effects and includes
`EnterPhase` (so phase tag changes diff aura membership). It still avoids
"recompute every step" — `aura_membership_set` is hot on the AI-sim path.

## 11. Battlefield figurines

Each combatant token (`UnitToken` circle) optionally carries a **figurine sprite**
as a Bevy child entity. The circle stays — it reads as the faction color / selection
ring / contact shadow under the figure.

- **Resolution** is app-side (never in the engine — sprites are outside the
  determinism contract). At spawn the content `{race}` + `{gender}` placeholders are
  resolved (`resolve_appearance`); the `{facing}` placeholder stays. Per-path
  precedence + the pattern rules live in [Content Guide → Battle figurines](../content-guide.md#battle-figurines-sprite).
  The resolved key lands on the ECS `UnitSprite(String)` component.
- **Two spawn paths**, both ending in the shared helper
  `spawn_figure_child(parent, asset_server, unit, pattern, facing)` (`ui/hex_grid/render.rs`),
  which substitutes `{facing}`, spawns the `Sprite`, and tags it `UnitFigure { unit, pattern, facing }`:
  - bootstrap / restart — `assign_hex_positions` reads `Option<&UnitSprite>`, computes
    initial `Facing` (toward the nearest opposing-party hex, by world X), inserts the
    `Facing` component on the logic entity, and adds the figure after the `VictoryTarget` ring;
  - summons — `spawn_ecs_entity_from_engine_unit` (`bridge/process.rs`) inserts
    `UnitSprite` + a team-based initial `Facing`, then spawns the same child. `AssetServer`
    rides in via the `VisualAssets` `SystemParam`.
- **Z-order** (figure transform local to the token at abs z `0.15`): figure at `+0.02`
  → above the token, below the `VictoryTarget` ring child (`-0.01`) is *behind*; the
  figure is anchored `BOTTOM_CENTER` and nudged `-6` Y so the circle reads as a contact
  shadow. World-space HP/status badges sit at abs z `0.2`, above the figure.
- **Facing is dynamic — a pre-lit file, not a mirror.** `Facing` is a runtime component;
  `{facing}` resolves to `right`/`left`. The scene light is fixed top-left in screen space,
  so flipping would light the wrong side — each orientation is a separately-drawn asset
  (symmetric art may omit `{facing}` and reuse one file). `sync_figure_facing` runs every
  combat frame, comparing each `UnitFigure.facing` to its unit's live `Facing` and reloading
  the `Sprite` image only on change. Initial facing is toward the nearest opponent.
  Turn-toward-last-interaction is implemented via `PendingAnim::Face` — an instantaneous,
  queue-ordered flip that mutates the `Facing` component (no tween, no blocker). Ordering:
  actor faces target **before** cast (Execute-pushed Face before popup); hostile victim faces
  attacker **after** cast (Finalize-pushed Face after popup via `enqueue_victim_facing` +
  `FacingCursor`); friendly recipients do not turn; movement faces travel direction (Execute);
  Attacks of Opportunity are excluded in v1. There is no `flip_x`.
