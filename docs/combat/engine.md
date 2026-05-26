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

`PoolChanged` is the **sole** pool-mutation surface (post-C6). Five pool kinds:
`Mana`, `Rage`, `Energy`, `Ap`, `Mp`. Five causes: `Regen`, `Refill`, `Spent`,
`Gained`, `MaxChanged`. Legacy events `ManaRegenerated`/`EnergyRegenerated`/`RageGained`
were removed in C6.

`TurnEnded` carries `cause: TurnEndCause` — `Manual`, `ResourcesExhausted`
(AP+MP=0 after Cast, emitted inline by engine — post-S6/B-γ), or `DeathOfActor`.

`DotDamaged` is the atomic fusion of the former `(StatusTicked, UnitDamaged)` pair
(post-S5). Buff-status ticks (zero damage) still emit `StatusTicked`.

All variants are serde-derived — written verbatim into `engine.jsonl` (**schema v42**).

### ApplyCtx

Carries `rng_calls: u64` — a per-step RNG canary used by the replay harness
to detect drift (even if events match, an unexpected RNG call count flags a
divergence).

### CombatState

```rust
pub struct CombatState {
    units:             Vec<Unit>,   // tombstones kept; hp == 0 means dead
    round:             u32,
    phase:             RoundPhase,
    turn_queue:        TurnQueue,
    random_seed:       u64,
    next_synthetic_uid: u64,        // counter for dynamically spawned units
}
```

Cloneable — the AI sim clones state for beam-search rollback without
disturbing live state. `CombatState::new(units, round, rng_seed)` is the
primary constructor.

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
    pub hp:                 i32,
    pub max_hp:             i32,
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

**ResourceTable invariants (post-Phase C):**
- `pools[Mana/Rage/Energy]`: `Some` iff unit has that mechanic; `None` otherwise.
- `pools[Ap]` / `pools[Mp]`: always `Some` for alive combatants.
- Five pool kinds in declaration order: `Mana, Rage, Energy, Ap, Mp` (iteration order load-bearing for replay determinism).
- HP is excluded: damage/heal/death paths are special-cased throughout the engine.
- Legacy scalar fields (`action_points`, `mana`, `rage`, `energy`, etc.) were removed in Phase C-6.

`armor_bonus`, `speed`, and `damage_taken_bonus` are derived aggregates —
recomputed from `statuses` by `Effect::RefreshAggregates`.

The bridge projector (`project_state_to_ecs`) reads exclusively from `pools`
(post-C5). `ContentView::status_bonuses` is default-implemented on top of
`status_def` (post-V4) — no override required.

---

## 4. Determinism contract

- **`DiceRng`** is seeded once per combat from `CombatStateRes.random_seed`.
  Engine calls only `roll_d()`. No `SystemTime`, no thread-local state.
- **`engine_purity.rs` test** greps all `crates/combat_engine/src/**/*.rs`
  for forbidden imports (`std::time`, `std::env`, `std::process`,
  `thread_local!`) — zero finds = engine is pure.
- **`aura_membership_set: BTreeSet`** (was HashSet) — the only known
  iteration-order risk was fixed in Phase 5a.
- **`post_state_hash(state)`** = BLAKE3 over canonical serialization of
  `{round, phase, turn_queue, alive_units sorted by id}`. Written as a
  canary in every `StepLine`; the replay binary compares hash per step to
  localize drift.

Replay guarantee: `crates/combat_engine/tests/replay.rs` (8 scenarios) asserts
byte-equal events + matching `rng_calls` + matching `post_state_hash` for
every step. Two intentional divergence sentinels prove the harness catches
real drift.

**Schema version history (trace.rs `SCHEMA_VERSION`):**

| Version | Change |
|---------|--------|
| v39 | `ManaRegenerated` also emitted after `PayCost` (inline, replacing bridge-side diff) |
| v40 | `DotDamaged` atomic (S5) + `TurnEnded{cause}` field (S6/B-γ) |
| v41 | `PoolChanged` introduced (C4); dual-emit alongside legacy events; AP/MP refill now visible |
| v42 | Legacy `Unit` fields + `ManaRegenerated`/`EnergyRegenerated`/`RageGained` removed (C6) |

---

## 5. AI uses the same step()

`src/combat/ai/plan/sim.rs::SimState::apply_step` calls
`combat_engine::step::step()` directly for beam-search candidate evaluation.
This is the **same entry point** the live bridge uses — guaranteeing that
"what AI thinks will happen" matches "what actually happens" (zero sim/real
drift by construction).

See [`bridge.md`](bridge.md) for the live path,
[`../ai/ai.md`](../ai/ai.md) for AI architecture.

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
