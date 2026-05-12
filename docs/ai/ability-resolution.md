# Ability Resolution (shared core)

*Источник: `src/combat/effects_math.rs`, `effects_state.rs`, `effects_outcome.rs`.*

Общее ядро разрешения способности живёт в `src/combat/effects_*.rs`. Оба потребителя — live pipeline (`combat/resolution.rs`) и AI sim (`combat/ai/planning/sim.rs`) — вызывают одну и ту же pure-функцию, отличаясь только реализациями traits.

## `TargetState` (`effects_state.rs`)

Read-only абстракция над battle state для AoE target enumeration и friendly-fire фильтрации:

```rust
pub trait TargetState {
    fn actor_pos(&self, actor: Entity) -> Option<Hex>;
    fn unit_at_cell(&self, pos: Hex) -> Option<TargetRef>;
    fn team_of(&self, entity: Entity) -> Option<Team>;
}
pub fn compute_affected_targets<S: TargetState>(
    actor, def, primary_target, target_pos, state,
) -> Vec<Entity>;
```

Backend'ы: `BevyTargetState` (в `resolution.rs`) и `SnapshotTargetState` (в `sim.rs`).

## `DiceSource` (`effects_outcome.rs`)

Абстракция над dice rolls:

```rust
pub trait DiceSource {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String);
    fn roll_crit_fail(&mut self, crit_fail_die: u32) -> bool;
}
```

- `RngDice<'a>(&mut DiceRng)` — реальный роллер.
- `ExpectedValue` — EV round'ится до i32; `roll_crit_fail` → **false** (sim допускает MAP-estimate: «самый вероятный исход — попадание»). Документированное решение; плюс scoring дисконтирует crit-chance через `crit_fail_adjusted` на факторе damage, и каждый тик — fresh replan.

## `AbilityOutcome` + `compute_ability_outcome`

Нейтральная структура-результат:

```rust
pub struct AbilityOutcome {
    pub affected: Vec<Entity>,
    pub primary: OutcomePrimary,  // Damage{raw, pierces} | Heal{amount} | GrantMovement{distance} | RestoreResources | Summon{...} | None
    pub statuses: Vec<StatusApply>,
    pub breakdown: String,
    pub crit_fail: Option<CritFail>,   // Miss / SelfStatus / SelfDamage
    pub mana_overload: bool,           // effects fire, mana cost doubles
}
```

`compute_ability_outcome` вычисляет всё это за один проход: rolls crit-fail, применяет crit mapping (BrokenFaith / CircuitBreach / Exhaustion / PactControl → `CritFail::SelfStatus` / `SelfDamage`; ManaOverload → `mana_overload = true`, primary fires), определяет primary branch из `EffectDef`, разворачивает statuses для `StatusOn::Target` и `MySelf`.

**Backends:**

- Real (`resolution.rs`): конвертирует outcome в `ApplyDamage` / `ApplyHeal` / `ApplyStatus` / `SpawnUnit` messages + `BonusMovement` tag + log events.
- Sim (`sim.rs::apply_primary`): мутирует snapshot напрямую. `Damage` применяет `final_damage_f32(raw, armor, vuln, pierces)` с floor `max(1.0)`. `Heal` сначала нейтрализует target DoT (`dot_per_tick`), затем восстанавливает HP. `apply_statuses` пушит `ActiveStatusView` в `unit.statuses` с `dot_per_tick = dice.expected().round()` и обновляет агрегаты через `refresh_status_aggregates` — следующий шаг плана видит свежую броню/vuln от только что применённого статуса.

## Drift scorecard (sim ↔ real)

Фиксированные drift'ы:

- ✅ #1 — damage floor `max(1)` единый
- ✅ #2 — DoT cleanse on heal в sim
- ✅ #4 — crit-fail mapping в shared core (sim игнорирует, но через явный `roll_crit_fail → false`)
- ✅ #5 — mid-plan status-adjusted armor

Остаются:

- ⏳ #3 — rage gain (+1 rage attacker/defender на damage в real, sim не моделирует).
- **Speed mid-plan**: base speed не трекается отдельно в `UnitSnapshot`, поэтому speed-изменяющие статусы, применённые в step[k], не re-flow в pathing следующих шагов.
