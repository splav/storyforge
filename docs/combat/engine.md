# Combat Engine — Pure Internals

The engine (`crates/combat_engine/`) is pure Rust with no Bevy dependency.
It owns all canonical state mutations: damage, healing, status apply/tick,
AoO, phase transitions, auras, turn queue, end-turn, and move.

For the bridge that wires the engine to Bevy ECS, see [`bridge.md`](bridge.md).
For the schedule and per-step chains, see [`pipeline.md`](pipeline.md).

---

## 1. Public API

```rust
pub fn step(
    state:   &mut CombatState,
    action:  Action,
    rng:     &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<(Vec<Event>, ApplyCtx), ActionError>;
```

### Action

```rust
Action::Move    { actor, path }
Action::Cast    { actor, ability, target, target_pos }
Action::EndTurn { actor }
```

Serde-derived so the engine trace can serialize/deserialize every call.

### Event

The observable consequence stream emitted after each `step()`:

`ActionStarted`, `UnitMoved`, `UnitDamaged`, `UnitHealed`, `UnitDied`,
`StatusApplied`, `StatusTicked`, `StatusRemoved`, `DotDamaged` (atomic, post-S5),
`ReactionFired`, `CritFailed`, `AoO`, `TurnStarted`,
`TurnEnded { cause: TurnEndCause }`, `TurnSkipped`, `RoundStarted`,
`AuraStatusGained`, `AuraStatusLost`, `PhaseEntered`,
`UnitSpawned`, `SpawnBlocked`, `ActionFinished`,
`PoolChanged { pool, delta, new_current, new_max, cause }`.

`PoolChanged` is the **sole** pool-mutation surface for non-HP pools (post-C6). Six pool kinds
in state: `Hp`, `Mana`, `Rage`, `Energy`, `Ap`, `Mp`. Five causes: `Regen`, `Refill`, `Spent`,
`Gained`, `MaxChanged`. Legacy events `ManaRegenerated`/`EnergyRegenerated`/`RageGained`
were removed in C6. HP mutations continue to use `UnitDamaged`/`UnitHealed`/`UnitDied` events
(HP is tracked in `pools[Hp]` state, but damage/heal events are dedicated).

`TurnEnded` carries `cause: TurnEndCause` — `Manual`, `ResourcesExhausted`
(AP+MP=0 after Cast, emitted inline by engine — post-S6/B-γ), or `DeathOfActor`.

`DotDamaged` is the atomic fusion of the former `(StatusTicked, UnitDamaged)` pair
(post-S5). Buff-status ticks (zero damage) still emit `StatusTicked`.

All variants are serde-derived — written verbatim into `engine.jsonl` (**schema v44**).

### ApplyCtx

Carries `rng_calls: u64` — a per-step RNG canary used by the replay harness
to detect drift (even if events match, an unexpected RNG call count flags a
divergence).

### CombatState

```rust
pub struct CombatState {
    units:             Vec<Unit>,   // tombstones kept; pools[Hp].current == 0 means dead
    round:             u32,
    phase:             RoundPhase,
    turn_queue:        TurnQueue,
    random_seed:       u64,
    next_synthetic_uid: u64,        // counter for dynamically spawned units
    pub blocked_hexes: HashSet<Hex>, // static obstacles (Wave 1 ch2, schema v43)
}
```

Cloneable — the AI sim clones state for beam-search rollback without
disturbing live state. `CombatState::new(units, round, rng_seed)` is the
primary constructor.

### VictoryCondition recursion

`VictoryCondition` is a recursive enum evaluated by `determine_outcome`:
- `AllEnemiesDead` — victory when no enemies remain alive.
- `KillTarget { enemy_name }` — victory when the named enemy dies.
- `KeepAlive { target_name }` — defeat immediately if the named unit dies;
  victory when target still alive and no enemies remain.
- `AllOf(Vec<VictoryCondition>)` — all sub-conditions must hold; short-circuits
  on the first `Some(false)`; victory when every sub-condition returns `Some(true)`.

### Non-acting NPCs

NPC objects (`NonActingNpc` ECS marker) live **only in ECS**, not in
`CombatState.units`. They are filtered out in `from_ecs` (bridge) and in
`build_turn_order` (initiative system). The engine never sees them. Damage and
healing to NPCs is applied bridge-side via direct `Vital` mutation.

NPCs are declared in `[[encounters.npcs]]` TOML sections and spawned by
`spawn_combatants` (`src/scenario/combat_scene.rs`) with
`Faction(Team::Player) + NonActingNpc + Vital` — no `Initiative`, `Abilities`,
or `AiMemory`. They are despawned by `despawn_combatants` (same `With<Combatant>`
query as regular units). Typically used with `KeepAlive` in an `AllOf` victory
condition (see `ch2_shrine` fixture).

---

## 2. ContentView trait

Four methods (contracted from 8 in Phase 5c.1 — per-combat data moved onto
`Unit` fields instead of content callbacks):

```rust
pub trait ContentView {
    fn ability_def(&self, id: &AbilityId)  -> Option<AbilityDef>;
    fn status_def(&self, id: &StatusId)    -> Option<StatusDef>;
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses;
    fn unit_template(&self, id: &str)      -> Option<UnitTemplate>;
}
```

Two implementations:

| Impl | Where | Used for |
|------|-------|----------|
| `EcsContentView` | `src/combat/engine_bridge.rs` | Live combat — reads `Res<ActiveContent>` |
| `TomlContentView` | `crates/combat_engine/src/toml_content_view.rs` | Offline tools: `replay_engine_trace`, benchmarks |

`EcsContentView::status_bonuses` reads real `armor_bonus` / `speed_bonus`
values from `active_content.statuses`. It does NOT return all-zeros (that was
a V1 bug where statuses like Defend failed to contribute their armor bonus
on bootstrap).

---

## 3. Unit struct

```rust
pub struct Unit {
    pub id:                 UnitId,
    pub team:               Team,
    pub pos:                Hex,
    pub armor:              i32,             // base equipment armor
    pub armor_bonus:        i32,             // bonus from active statuses
    pub damage_taken_bonus: i32,             // vulnerability: positive = more damage taken
    pub base_speed:         i32,
    pub speed:              i32,             // effective = base + status speed bonuses
    pub reactions_left:     i32,
    pub reactions_max:      i32,             // populated from Reactions.max at bootstrap
    pub statuses:           Vec<ActiveStatus>,
    pub summoner:           Option<UnitId>,  // Some(_) if spawned via Effect::Spawn
    pub caster_context:     CasterContext,   // weapon dice, modifiers, crit-fail outcome
    pub aoo_dice:           Option<DiceExpr>, // None = cannot AoO
    pub auras:              Vec<AuraDef>,    // passive auras emitted by this unit
    pub enemy_phases:       Vec<PhaseEntry>, // boss phase thresholds (empty for non-bosses)
    pub pools:              EnumMap<PoolKind, Option<(i32, i32)>>,  // (current, max)
    pub regen_per_pool:     EnumMap<PoolKind, RegenRule>,
}
```

Use `unit.hp()` / `unit.max_hp()` accessors — they read from `pools[PoolKind::Hp]`.

**ResourceTable invariants (post HP-as-pool Stage 3c / schema v44):**
- `pools[Hp]`: always `Some` for combat units; `(current_hp, max_hp)`. Sole canonical HP representation — legacy `hp` / `max_hp` fields removed.
- `pools[Mana/Rage/Energy]`: `Some` iff unit has that mechanic; `None` otherwise.
- `pools[Ap]` / `pools[Mp]`: always `Some` for alive combatants.
- Six pool kinds in declaration order: `Hp, Mana, Rage, Energy, Ap, Mp` (iteration order load-bearing for replay determinism).
- Legacy scalar fields (`action_points`, `mana`, `rage`, `energy`, `hp`, `max_hp`, etc.) were removed in Phase C-6 / Stage 3c.

`armor_bonus`, `speed`, and `damage_taken_bonus` are derived aggregates —
recomputed from `statuses` by `Effect::RefreshAggregates`.

The bridge projector (`project_state_to_ecs`) reads exclusively from `pools`
(post-C5). `ContentView::status_bonuses` is default-implemented on top of
`status_def` (post-V4) — no override required.

---

## 4. Determinism contract

**Given:**
- Same engine SCHEMA version (currently **v44**)
- Same `ContentView` impl + content data (same TOML files, same `content_hash`)
- Same RNG seed
- Same sequence of `Action` dispatches

**Then** every `step()` call produces:
- Identical `Event` streams (same variants, same field values, same order)
- Identical `post_state_hash` at every step (BLAKE3 of canonical state serialization:
  `{round, phase, turn_queue, alive_units sorted by id}`)
- Identical `rng_calls` count per step (from `ApplyCtx`)

**What's NOT guaranteed:**
- Cross-SCHEMA replay: a v41 trace will not load correctly in a v42 engine — intentional clean break.
- Mixing content versions: different `content_hash` → divergence.
- Parallel mutation of `CombatState`: not safe; the engine is single-threaded by design.
- Cross-compile bit-identity: determinism holds within a single compile. Float math
  is deterministic given the same binary, but cross-compile (`--release` vs
  `--features dev`, different opt-levels) bit-identity is not guaranteed.

**Implementation invariants that uphold the contract:**
- `DiceRng` is seeded once per combat from `CombatStateRes.random_seed`; engine
  calls only `roll_d()` — no `SystemTime`, no thread-local state.
- `aura_membership_set` uses `BTreeSet` (was `HashSet` pre-5a) — iteration order
  is stable and insertion-order-independent.
- `engine_purity.rs` test greps all `crates/combat_engine/src/**/*.rs` for
  forbidden imports (`std::time`, `std::env`, `std::process`, `thread_local!`);
  zero hits = engine is pure.

**Verification:**
- **Property test** `tests/combat_engine/determinism.rs` runs the engine twice on
  5 representative scenarios and asserts bit-identical traces (events + hash + rng_calls):
  1. `det_cast_ap_exhaustion_s6` — Cast drains last AP → S6 auto-EndTurn cascade
  2. `det_dot_tick_during_dead_skip` — EndTurn with DoT tick on a dead-skipped unit
  3. `det_move_with_aoo_reaction` — Move triggers AoO (real RNG dice roll)
  4. `det_phase_transition` — Damage crosses boss phase threshold → EnterPhase cascade
  5. `det_aoe_multi_target_cast` — AoE fireball hits 3 enemies (per-target ordering)
- **Debug tool** `src/bin/replay_diff.rs` (`cargo run --bin replay_diff -- a.jsonl b.jsonl`)
  performs structured per-step diff with first-divergence reporting across any two
  trace files (events, hash, rng_calls).

**Schema version history (`trace.rs` `SCHEMA_VERSION`):**

| Version | Change |
|---------|--------|
| v39 | `ManaRegenerated` also emitted after `PayCost` (inline, replacing bridge-side diff) |
| v40 | `DotDamaged` atomic (S5) + `TurnEnded{cause}` field (S6/B-γ) |
| v41 | `PoolChanged` introduced (C4); dual-emit alongside legacy events; AP/MP refill now visible |
| v42 | Legacy `Unit` fields + `ManaRegenerated`/`EnergyRegenerated`/`RageGained` removed (C6) |
| v43 | `PoolKind::Hp` added (first variant); `pools` EnumMap shape → 6 pools; `blocked_hexes` + `template_id` added |
| v44 | `Unit.hp` / `Unit.max_hp` legacy fields removed; `UnitWire.hp` / `UnitWire.max_hp` removed from output; `pools[Hp]` sole canonical HP representation (HP-as-pool Stage 3c) |

---

## 5. AI uses the same step()

`src/combat/ai/plan/sim.rs::SimState::apply_step` calls
`combat_engine::step::step()` directly for beam-search candidate evaluation.
This is the **same entry point** the live bridge uses — guaranteeing that
"what AI thinks will happen" matches "what actually happens" (zero sim/real
drift by construction).

See [`bridge.md`](bridge.md) for the live path,
[`../ai/ai.md`](../ai/ai.md) for AI architecture.

### Legality rules (range, LOS, taunt)

`check_legality(action, &state)` (in `legality.rs`) is the single rule-layer
function shared by all three backends. Rejection reasons live in
`IllegalReason` and cover:

- **Range** — actor distance to target must lie within `AbilityDef.range` (min..max). Engine is grid-topology-agnostic; `ActionState::is_in_bounds` supplies the bounds predicate per backend.
- **Line of sight (LOS)** — if `AbilityDef.requires_los == true` and `range.max > 1`, the hex-line from actor to target must not pass through any obstacle. Rejection: `IllegalReason::NoLineOfSight`. Melee (`range = 1..1`) skips this check — adjacent hexes have no intermediates to block. Single canonical algorithm `combat_engine::geom::has_los`; same blocker set (`state.blocked_hexes`) for all three backends. See parity contract below.
- **Target type** — `SingleEnemy` / `SingleAlly` / `Myself` are gated by `ActionState::target_team`. `Taunt` (`forces_targeting` status on an enemy) restricts the legal targets of `SingleEnemy` casts to that enemy only.
- **Resource costs** — AP, MP, mana/rage/energy availability against ability `costs`.
- **Actor liveness + ability knowledge** — actor must be alive, target must be present and (for non-AoE) alive, actor must know the ability.

LOS is the only rule that distinguishes melee from ranged. All other rules apply uniformly regardless of distance.

### ActionState parity contract

`ActionState` has three backends:

| Backend | Where | Context |
|---------|-------|---------|
| `BevyActions` | `src/combat/legality_adapter.rs` | Live ECS (UI tooltip, player input) |
| `SnapshotActionState` | `src/combat/ai/action_state.rs` | AI sim (beam-search, offline replay) |
| `EngineCheckState` | `crates/combat_engine/src/step.rs` | Engine-side `step(Cast)` pre-validate |

**Parity contract:** all three backends must agree on every `ActionState` method
for the same logical inputs. Any divergence means AI predicts something different
from what the engine actually executes.

**LOS specifically:** `is_blocked_los(from, to)` lives **only** in the trait as a
default implementation that calls `combat_engine::geom::has_los` over the abstract
getter `blocked_hexes(&self) -> &HashSet<Hex>`. Each backend overrides only the
getter (one-line `&self.…blocked_hexes`); the LOS algorithm itself has a single
physical copy. The storyforge `src/game/hex::has_los` is a re-export of the same
function for legacy callers.

**Parity is structural by construction:** same `blocked_hexes` set → identical
`is_blocked_los` result. No three-way divergence is possible without overriding
`is_blocked_los` directly (which production backends do not).

**Verification:** `tests/los_parity.rs` contains four tests:
- `bevy_actions_is_blocked_los_matches_has_los`
- `snapshot_actions_is_blocked_los_matches_has_los`
- `engine_action_state_is_blocked_los_matches_has_los`
- `prop_all_three_backends_agree_on_los` — 200 randomized cases asserting two
  backends agree (Bevy backend skipped from the loop for runtime cost; parity
  is structural per above, the dedicated Bevy test exercises the override).

---

## 6. Effect catalog (reference)

The engine applies state changes through `Effect` variants, derived during
`apply_effect`. Key variants:

| Effect | Purpose |
|--------|---------|
| `MovePosition` | Set unit position |
| `DecrementMP / DecrementAP` | Spend movement / action points |
| `Damage { pierces }` | Pre-mitigation damage; `pierces=true` skips armor |
| `Heal` | Restore HP after neutralizing DoT statuses |
| `PayCost` | Spend mana / rage / energy / hp |
| `ApplyStatus` | Add or refresh a status (reapply semantics) |
| `RemoveStatus` | Remove all entries with a given status id |
| `GainRage` | Grant +1 rage (clamped to max) |
| `DecrementReactions` | Spend one reaction |
| `Death` | Mark unit dead (hp already 0) |
| `RefreshAggregates` | Recompute speed / armor_bonus from statuses |
| `TickDot` | Apply one DoT tick (piercing, bypasses armor) |
| `ExpireStatus` | Decrement rounds_remaining; remove at 0 |
| `Spawn` | Summon a new unit (resolves template, ring search, cap check) |
| `AdvanceTurn` | Move turn-queue cursor; derives BumpRound on wrap |
| `BumpRound` | Increment round, reset reactions, emit RoundStarted |
| `EnterPhase` | Boss phase transition (cascades SetMaxHp, SetArmor, optionally Heal) |

`Effect::Spawn` fields: `summoner: UnitId`, `template_id: String`,
`max_active: Option<u32>`. The engine resolves the template via
`ContentView::unit_template`, picks a free hex by ring search around the
summoner, enforces the cap, then emits `Event::UnitSpawned` or
`Event::SpawnBlocked`.
