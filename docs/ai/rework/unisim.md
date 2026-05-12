# Sim/Real Unification — design spike

**Статус:** идея, не запланировано. Возникла в обсуждении step 12.3 (rage drift).

## Проблема

Sim (AI планировщик) и real combat — два параллельных кода, исполняющих **одну механику**:

| Layer | Real combat | Sim |
|-------|-------------|-----|
| Damage application | `apply_effects.rs::apply_effects_system` (Bevy system, Query/Commands) | `plan/sim.rs::apply_primary` (direct mut на `BattleSnapshot::units`) |
| Status apply | `status_apply.rs` | `plan/sim.rs::apply_statuses` |
| AoO | `movement.rs::movement_system` (228-236) | `plan/sim.rs::apply_move` |
| Status tick | `status_tick.rs` | — (sim не модулирует ticks внутри плана) |
| Rage gain | `apply_effects.rs:117-129`, `movement.rs:228-236` | `plan/sim.rs::apply_primary` + `apply_move` |

Каждое расширение механики в real → нужно вспомнить и **зеркально** обновить sim. **Класс багов** «drift между sim и real»:
- drift #speed (12.1): статус Haste обновлял ECS Speed но не `UnitSnapshot.speed`.
- drift #status (12.1): armor_bonus/vuln/CC не пересчитывались в sim после status apply.
- drift #3 (12.3): rage gain в real был, в sim не было — ни для direct damage, ни для AoO.
- AoO suicide (12.2): real убивал актора на AoO, sim считал что актор выживает.

Это **не сложные баги**, это **повторяющаяся забывчивость**. Каждый — отдельный 1-2-дневный сабшаг с tests и parity-харнессом.

## Решение

Единая точка истины для механики через **abstraction over state container**:

```rust
trait MutableBattleState {
    type Unit: BattleUnit;

    fn unit(&self, entity: Entity) -> Option<&Self::Unit>;
    fn unit_mut(&mut self, entity: Entity) -> Option<&mut Self::Unit>;
    fn units_iter(&self) -> impl Iterator<Item = &Self::Unit>;
    fn enemies_of(&self, team: Team) -> impl Iterator<Item = &Self::Unit>;
    // ... narrow accessors enough for damage/status/move logic
}

trait BattleUnit {
    fn hp(&self) -> i32;
    fn set_hp(&mut self, value: i32);
    fn rage(&self) -> Option<(i32, i32)>;
    fn gain_rage(&mut self);
    fn add_status(&mut self, status: ActiveStatus, content: &ContentView);
    // ... + armor, mitigation, position, etc
}
```

Реализации:
- **`BevyBattleState<'w, 's>`** — обёртка над `World` + типизированные queries (`Vital`, `Rage`, `StatusEffects`, ...).
- **`SnapshotBattleState`** — `&mut BattleSnapshot`.

**Pure logic functions** (одна реализация на обе стороны):

```rust
pub fn apply_damage_event(
    state: &mut impl MutableBattleState,
    source: Entity,
    target: Entity,
    raw: f32,
    pierces_armor: bool,
) -> DamageResult {
    let Some(target_unit) = state.unit(target) else { return DamageResult::Miss; };
    let mitigation = target_unit.armor() + target_unit.armor_bonus();
    let vuln = target_unit.damage_taken_bonus();
    let dealt = final_damage_f32(raw, mitigation, vuln, pierces_armor);

    if let Some(u) = state.unit_mut(target) {
        u.set_hp((u.hp() as f32 - dealt).max(0.0) as i32);
    }
    // Rage rule: +1 source, +1 target (real-mirror, identical for sim).
    for &e in &[source, target] {
        if let Some(u) = state.unit_mut(e) { u.gain_rage(); }
    }
    DamageResult { dealt, killed: state.unit(target).is_some_and(|u| u.hp() == 0) }
}
```

Та же функция вызывается из:
- `apply_effects_system` (real) — оборачивает `World` в `BevyBattleState`, вызывает `apply_damage_event` для каждого `damages.iter()`.
- `plan/sim.rs::apply_primary` (sim) — оборачивает `BattleSnapshot` в `SnapshotBattleState`, вызывает на каждом hit.

**Drift невозможен by construction.**

## Преимущества

1. **Все step-12 drift-bugs (#speed, #status, #3, AoO suicide) не могут существовать** — одна implementation.
2. Любая будущая механика — пишется один раз. Не надо вспоминать «обновить sim».
3. Параллельные мульти-step plan simulations становятся проще: clone snapshot, apply events, profit.
4. Test surface уменьшается: parity-харнесс становится излишним (sim ≡ real by definition); остаются только unit tests на pure logic.

## Сложности

1. **Bevy coupling в real-pipeline.** `apply_effects_system` пишет в `MessageWriter<CombatEvent>`, читает из `MessageReader<ApplyDamage>`, иногда вызывает `Commands` (despawn corpses). Унификация: либо логика возвращает `Vec<Event>` для real-side bus, либо `BevyBattleState` инкапсулирует event emit. Не невозможно, но 2-3 неделького refactor'а минимум.

2. **Snapshot ≠ ECS structurally.** `UnitSnapshot` — flat struct, у real — раздельные компоненты (`Vital`, `Rage`, `StatusEffects`, ...). Trait `BattleUnit` сглаживает, но реализация для ECS требует tuple-borrow (`get<(Vital, Rage)>`) или внутреннего join.

3. **Borrow patterns.** Sim делает freely `unit_mut` несколько раз; ECS требует SystemParam-уровнего планирования. Trait API может оказаться слишком restrictive (нельзя в одном scope несколько mutable units). Решение: collect → apply pattern (события собираются immutable, потом применяются), либо `state.with_unit_mut(entity, |u| ...)` closure-based API.

4. **Dice/RNG.** Real использует RngDice, sim — ExpectedValue. Trait должен принять `DiceSource` параметром (уже есть в effects_outcome).

5. **Performance.** Trait dispatch (static via generics) даст zero cost, но если случайно мономорфизация прилетит в hot path — следить за compile time / binary size.

## Сценарий реализации (если делать)

**Phase 1** (steel thread, 1 неделя):
- Извлечь ОДНУ функцию: `apply_damage_to_unit(state, target, raw, mitigation, vuln, pierces) -> dealt: f32`. Pure, без events.
- Real и sim вызывают её. Trait `MutableBattleState` с минимальным API.
- Это закрывает `apply_primary` Damage arm + `apply_effects` damage loop.

**Phase 2** (1 неделя):
- Rage gain — общая функция `gain_rage_for_damage_event(state, source, target)`.
- Закрывает 12.3 drift permanently. AoO rage в `apply_move` и в `movement_system` — обе через эту функцию.

**Phase 3** (2 недели):
- Status apply — общая функция, включая `refresh_aggregates` после.
- Закрывает 12.1 drift permanently.

**Phase 4** (1 неделя):
- AoO scan — выделить core, обе стороны зовут.
- Закрывает 12.2.

**Phase 5** (1-2 недели):
- Move + AoO + status tick — полный pipeline. Real-side event emission становится adapter-only.

**Итого:** ~6-8 недель focused work. Параллельная работа возможна с другими wave-tasks (TeamTasks, encounter scripts).

## Когда делать

- **Не сейчас.** Step 12 (12.4 cleanup + schema bump) добиваем на текущей архитектуре.
- **После Wave 3-4** master plan'а (TeamTasks, encounter scripts, telegraphing) — эти добавят ещё больше mechanics, унификация после них сэкономит больше работы.
- **Wave 5 candidate**: новый раздел в `docs/ai/rework/`, отдельный план типа `step_unisim_plan.md` с под-этапами.

## Альтернативы

- **Status quo + дисциплина**: каждый новый mechanic — обязательный parity test. Дешевле в моменте, но drift всё равно проникает (12.x — это пять штук).
- **Event sourcing вместо trait**: real эмиттит events, sim тоже эмиттит, общий `EventProcessor` мутирует state. Чище концептуально, но тяжелее API.
- **Sim как replay of real events**: запустить mini-Bevy world внутри `pick_action`. Concept clean, но Bevy-world setup за step — дорого для beam search.

## TL;DR

Унификация sim ↔ real — правильный долгосрочный путь. Закрывает целый класс drift-bugs by construction. Стоимость 6-8 недель, экономия — все будущие step-12-подобные сабшаги. Делать **после Wave 3-4**, отдельной phase, не вплетать в step 12.x.
