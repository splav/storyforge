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

`Damage`, `Heal`, `Died`, `StatusApplied`, `StatusTicked`, `StatusExpired`,
`AoO`, `Move`, `TurnStarted`, `TurnEnded`, `TurnSkipped`,
`RoundStarted`, `PhaseEntered`, `UnitSpawned`, `SpawnBlocked`.

Also serde-derived — written verbatim into `engine.jsonl` (schema v38).

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
    pub id:                  UnitId,
    pub team:                Team,
    pub pos:                 Hex,
    pub hp:                  i32,
    pub max_hp:              i32,
    pub armor:               i32,          // base equipment armor
    pub armor_bonus:         i32,          // bonus from active statuses
    pub damage_taken_bonus:  i32,          // vulnerability: positive = more damage taken
    pub base_speed:          i32,
    pub speed:               i32,          // effective = base + status speed bonuses
    pub action_points:       i32,
    pub max_ap:              i32,
    pub movement_points:     i32,
    pub reactions_left:      i32,
    pub reactions_max:       i32,          // populated from Reactions.max at bootstrap
    pub rage:                Option<Pool>,
    pub mana:                Option<Pool>,
    pub energy:              Option<Pool>,
    pub summoner:            Option<UnitId>, // Some(_) if spawned via Effect::Spawn
    pub statuses:            Vec<ActiveStatus>,
    pub caster_context:      CasterContext, // weapon dice, modifiers, crit-fail outcome
    pub auras:               Vec<AuraDef>,  // passive auras emitted by this unit
    pub enemy_phases:        Vec<PhaseEntry>, // boss phase thresholds (empty for non-bosses)
    pub aoo_dice:            Option<DiceExpr>, // None = cannot AoO
}
```

`armor_bonus`, `speed`, and `damage_taken_bonus` are derived aggregates —
they are recomputed from `statuses` by `Effect::RefreshAggregates`.

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
