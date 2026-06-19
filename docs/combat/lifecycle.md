# Combat Lifecycle — Start, Teardown, Restart, Dynamic Spawn

This file covers how a combat encounter is initialized, torn down, restarted,
and how units are dynamically spawned mid-combat.

For the StartRound chain and AwaitCommand schedule, see
[`pipeline.md`](pipeline.md).
For the engine and `bootstrap_combat_state` internals, see
[`bridge.md`](bridge.md) and [`engine.md`](engine.md).

---

## 1. Combat start

```
AppState::Combat entered
  └── start_combat_system (Update, Overworld only — transitions to Combat)
        └── spawn_combat_scene  (scenario/combat_scene.rs)
              ├── spawn combatants (hero_bundle / enemy_bundle)
              ├── insert HexPositions, TurnQueue, CombatContext, CombatObjective
              └── set CombatPhase::StartRound
```

Once `CombatPhase::StartRound` is active the pipeline's StartRound chain runs:

```
project_state_to_ecs
  → assign_hex_positions
  → build_turn_order          (initiative d20 + DEX mod, round 1 only)
  → bootstrap_combat_state    (seeds CombatStateRes from ECS — one-shot)
  → write_engine_trace_init_system
```

`write_engine_trace_init_system` writes the `InitLine` to `engine.jsonl`:
`{ units, round, phase, turn_queue, rng_seed, content_hash, session_id }`.

After this chain `CombatPhase::AwaitCommand` becomes active and the regular
per-turn loop begins.

---

## 2. Bootstrap idempotency

`bootstrap_combat_state` (in `src/combat/bridge/bootstrap.rs`) is **one-shot per
encounter**:

```rust
if !combat_state.0.units().is_empty() { return; }
```

On the first StartRound entry it:
1. Calls `from_ecs(combatants, positions, round, id_map, &content)` —
   content-aware recompute of `armor_bonus`, `speed_bonus`,
   `damage_taken_bonus` from active statuses.
2. Populates per-unit fields: `caster_context`, `aoo_dice`, `auras`,
   `enemy_phases`.
3. Sets the turn queue.
4. Primes the first actor (`start_actor_turn` + `translate_tick_events`).

Subsequent StartRound entries (round 2+, triggered by queue wrap) hit the
guard and return immediately — the engine state is authoritative from that
point. ECS is never re-imported from round 2 onward.

`reactions_max` is initialised from `Reactions.max` (not `.remaining`) so the
first actor always starts with a full reaction budget.

---

## 3. Combat teardown

On Victory or Defeat `AppState::Combat` is exited:

```
OnExit(AppState::Combat)
  └── reset_engine_mirrors_on_exit_combat  (bridge/queues.rs)
        ├── CombatStateRes  → cleared (units vec emptied)
        ├── UnitIdMap       → cleared
        └── PendingPhaseTransitions → cleared
```

ECS combatant entities are despawned by `despawn_combatants` in
`scenario/combat_scene.rs` as part of the normal scene transition. The engine
resources are cleared separately so the next combat starts with a clean slate.

---

## 4. Restart flow (Defeat → Restart)

When the player chooses Restart from the defeat overlay, `RestartCombat` is
dispatched. The restart does **not** exit `AppState::Combat` — it is an
in-place reset:

```
RestartCombat message
  └── reset_engine_mirrors_on_restart  (bridge/queues.rs)
        ├── CombatStateRes  → cleared
        ├── UnitIdMap       → cleared
        └── PendingPhaseTransitions → cleared

  └── restart_combat_system  (scenario/combat_scene.rs:216)
        1. Save initiative for all combatants by name into PresetInitiative
        2. Despawn all combatants, tokens, popups
        3. Spawn fresh combatants via spawn_combatants
        4. reset_combat_state:
             ctx.round = 0, ctx.encounter = None
             CombatLog cleared, AnimationQueue cleared, cursors reset
             ActiveCombatant removed with entity despawn
        5. Set CombatPhase::StartRound
```

The StartRound chain then runs normally, with one difference:

**Initiative preservation:** `build_turn_order` checks
`first_round && !preset.is_empty()`. When true, it reads values from
`PresetInitiative` (a `HashMap<String, i32>` keyed by unit name) instead of
rolling, then clears the preset. This keeps the same turn order across
restarts — no initiative re-roll on retry.

Initiative is stored by name, not entity, because entities are fully
re-created during the despawn/spawn cycle.

---

## 5. Second combat in same session

A second combat (different encounter in the same app run) goes through the
normal `OnExit(AppState::Combat)` teardown followed by re-entry. The
`units().is_empty()` guard in `bootstrap_combat_state` ensures the engine
initializes fresh for the new encounter.

Regression coverage: `tests/combat/handoff.rs::combat_2_bootstraps_fresh_after_combat_1`
verifies that a second combat in the same `App` session bootstraps a clean
engine state and does not inherit any unit data from the first combat.

---

## 6. Dynamic spawn (Effect::Spawn)

Mid-combat unit summoning is fully handled inside the engine:

```
step(Cast { ability with EffectDef::Summon })
  └── Effect::Spawn { summoner, template_id, max_active }
        ├── ContentView::unit_template(template_id)  → UnitTemplate
        ├── Ring search around summoner hex          → free position
        ├── Check active summons ≤ max_active cap
        └── On success:
              emit Event::UnitSpawned { summoner, unit: <new Unit> }
              unit.summoner = Some(summoner_id)
        └── On failure:
              emit Event::SpawnBlocked { reason }
```

`process_action_system` handles `Event::UnitSpawned` by calling
`spawn_ecs_entity_from_engine_unit`, which creates the Bevy entity with the
full `CombatantBundle` and inserts it into `HexPositions` and `UnitIdMap`.

**Ordering invariant — spawn is a PRE-pass, before `translate_events`.**
A summon cast by the round's *last* actor exhausts its AP (and MP), so the
engine auto-ends the turn and wraps the round **inside the same `step(Cast)`**:
the event stream contains `UnitSpawned … TurnEnded … RoundStarted …
TurnStarted { summon }` when the summon wins initiative for the new round.
`translate_events` resolves `TurnStarted`'s actor through `UnitIdMap` to assign
`ActiveCombatant`. If the entity were created *after* translation (the old
post-pass), that lookup returned `None`, no unit received `ActiveCombatant`,
and the turn loop had no actor to drive → **the game hung**. So the
`UnitSpawned` handling runs before `translate_events`, guaranteeing the entity
is in `UnitIdMap` when the `TurnStarted` for it is translated.
Regression: `bridge_cast::summon_by_last_actor_hands_active_combatant_to_summon`.

The spawned unit joins the **next** StartRound turn queue — it does not
participate in the current round's queue.  Its `d20 + dex_mod` initiative is
rolled at spawn time (`step.rs`, on `Effect::Spawn`); insertion into
`turn_queue.order` is deferred to the next `BumpRound`'s `reconcile_turn_order`
(same sort as all other units), so the summon first acts in the next round.

**Action economy rule:** a spawned unit does **not** trigger Attacks of
Opportunity in the round it is spawned. AoO is only provoked by movement
(leaving a threatened hex), not by appearing via spawn.

**Concurrency cap:** `max_active` limits how many units with the same
`template_id` the caster can have alive simultaneously. The engine enforces
this cap before resolving the spawn.

**Summoner link:** `unit.summoner = Some(summoner_id)` is set on the engine
`Unit`. This can be used to identify summoned vs. originally-placed units and
to clean up orphaned summons if the summoner dies (game-rule decision — not
yet enforced; the field exists for future use).
