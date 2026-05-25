# Engine Bootstrap Refactor (V3)

> **Historical record.** V1 — V3 (включая 3.6 two-combats regression
> и 3.7 dynamic spawn) — закрыты в коммитах `6a4f24b`, `db29cb0`,
> `9c08d60`, `d56720c`. V4 (engine status reflow unification) теперь
> отслеживается в [`engine-migration.md`](../engine-migration.md), который
> является текущим источником правды по миграции ECS→engine. Этот
> документ остаётся как запись research-фазы и архитектурных решений
> V3.

**Статус**: planning. V1 (fix `EcsContentView::status_bonuses` stub) сделан и закоммичен отдельно. Этот документ описывает дальнейшую работу.

## Контекст: почему вообще копаем

В мае 2026 при playtest всплыл баг "Провокация не даёт прирост брони": после каста taunt у Алдрика статус `defending` (`armor_bonus = 4`) применяется, но удар по нему режется только базовой бронёй 3, а не 7. Корень — stub-реализация `EcsContentView::status_bonuses` в [src/combat/engine_bridge.rs:249](../../../src/combat/engine_bridge.rs:249), которая всегда возвращала `StatusBonuses::default()`. `RefreshAggregates` суммировал нули. Заодно `forces_targeting` (`taunted`) продолжал работать корректно — он читается через **другой** метод трейта (`status_def`), не stubbed.

V1 закрыл сам симптом (одна правка `status_bonuses` + регресс-тесты). Но раскопки выявили несколько связанных архитектурных долгов вокруг engine ↔ ECS bridge. V3 — план как их закрыть в правильном порядке.

## Что нашли при раскопке

1. **Stub status_bonuses (V1, fixed)** — `EcsContentView::status_bonuses` всегда возвращал default. Production-implementation никогда не дёргалась в тестах — `StubContent` в engine-тестах реализует метод корректно через `with_armor`, AI-snapshot использует `StatusTagCache::bonuses`. Дыра существовала с Phase 2 и зафиксирована в коде комментарием "Aggregates ... may later be derived from this".

2. **Хардкод `armor_bonus: 0` в `from_ecs`** ([engine_bridge.rs:211](../../../src/combat/engine_bridge.rs:211)). При re-import engine state из ECS поля `armor_bonus`/`damage_taken_bonus`/`speed_bonus` зануляются. Сейчас компенсируется тем, что `Effect::RefreshAggregates` пересчитывает их при следующем `ApplyStatus`. Но если юнит **стартует** бой с уже навешенным статусом (aura, persistent buff, summon с initial status) — бонус потеряется до первого ApplyStatus. **Future foot-gun**, не баг сегодня.

3. **`init_state_from_ecs` запускается на каждом `OnEnter(AwaitCommand)` в раунде 1.** Гейт `combat_context.round < 2` ([engine_bridge.rs:1463](../../../src/combat/engine_bridge.rs:1463)). История гейта непростая (см. коммент): сначала пробовали `ctx.round != 1` — сломались тесты (бегут на ctx.round == 0); потом encounter-identity — сломалась second-combat-in-session. Текущий guard `>= 2` — компромисс. В принципе init на round 1 один раз (через StartRound→AwaitCommand), но логически re-import каждый round — лишний.

4. **Двойственная роль `combat_context.round`** — и счётчик раундов для ECS, и guard для init. Инкремент 0→1 делает `build_turn_order`, а не engine. Когда engine эмитит `Event::RoundStarted`, ECS round инкрементит translate-layer. Два владельца счётчика.

5. **Два пути пересчёта status bonuses** — `RefreshAggregates` effect читает `status_def.damage_taken_bonus` через `content.status_def(...)`, а `armor_bonus`/`speed_bonus` через `content.status_bonuses(...)`. Это часть Phase 2 долга "may later be derived from this". Источник риска рассинхрона.

6. **caster_context/aoo_dice/auras/enemy_phases для динамически spawn'нутых юнитов**. Сейчас init populate'ит эти поля **только один раз** на round 1. Для summon'нутых в середине боя юнитов поля не заполняются. Это **уже сломано сегодня**, не V3-introduced, но V3 lifecycle-рефактор должен это явно адресовать (либо scope-out с TODO).

## Решение, разбитое по фазам

### V2 — content-aware `from_ecs` (recommended next)

**Цель**: убрать хардкод `armor_bonus: 0` и устранить foot-gun. Минимальный risk profile.

**Шаги**:
1. Сигнатура: `from_ecs(combatants, positions, round, id_map, content: &ActiveContent) -> CombatState`.
2. Внутри `from_ecs` после построения units пройти по каждому и пересчитать `armor_bonus`/`speed_bonus`/`damage_taken_bonus` — суммируя через тот же `ContentView::status_bonuses` + `status_def.damage_taken_bonus`. Можно построить временный `EcsContentView` внутри либо передавать готовый.
3. Удалить stale doc-комменты "Phase 0 deferred to step 8+" / "step 8+" в [engine_bridge.rs:160-167](../../../src/combat/engine_bridge.rs:160) и [:211](../../../src/combat/engine_bridge.rs:211).
4. Обновить callsite'ы:
   - `init_state_from_ecs` ([engine_bridge.rs:1469](../../../src/combat/engine_bridge.rs:1469)) — добавить параметр content.
   - `tests/combat_engine/state.rs:42` `run_from_ecs` — добавить content загрузку (критика-агент указал, что я этот callsite пропустил в V3 драфте; в V2 он критичен).
   - `tests/common/mod.rs:162`, `tests/combat_engine/bridge_smoke.rs:139`, `tests/combat_engine/legality_parity.rs:145`, `tests/engine_step_range_correlation.rs:104` — все зовут `run_system_once(init_state_from_ecs)`, signature change потащит обновление SystemParam — может потребовать просто добавить `Res<ActiveContent>` (уже есть в init).
5. Регресс-тест: юнит стартует с `defending` в `StatusEffects` → `from_ecs` → `state.unit.armor_bonus == 4` (без RefreshAggregates после).

**Estimated effort**: 1-2 часа. Risk: низкий (signature change + один inline refresh-loop).

### V3 — bootstrap-once lifecycle (defer until needed)

**Цель**: убрать `init_state_from_ecs` из round-cycle, engine state становится authoritative с момента combat-start.

**Открытые вопросы (из критики)**:

- **Где bootstrap-точка?** `OnEnter(AppState::Combat)` НЕ годится — ECS ещё не содержит spawn'нутых combatant'ов в этот момент. `start_combat_system` (`src/combat/mod.rs:35`) ставит `ctx.round = 0` и переключает state, но encounter spawn — отдельная цепочка. Кандидаты:
  1. `OnEnter(CombatPhase::StartRound)` с guard `round == 0`.
  2. Отдельный `CombatBootstrap` message от encounter spawner'а — после того как combatants заспавнились.
- **Кто инкрементит round 0→1?** Сейчас это `build_turn_order` ([turn_order.rs:34](../../../src/combat/turn_order.rs:34)). Если bootstrap уже делает `set_turn_queue` и `start_actor_turn`, `build_turn_order` либо лишний, либо его инкремент должен переехать.
- **Что делать с `engine_start_first_turn_system`?** Сейчас он гейтится `round != 1` и зовёт `start_actor_turn(first_actor)`. В bootstrap-once его логика встраивается в bootstrap. Но он зависит от `turn_queue.current()` — значит bootstrap должен зайти ПОСЛЕ заполнения queue.
- **dynamic spawn**. Сейчас caster_context/aoo_dice не заполняются для summon'нутых юнитов после init. V3 lifecycle разделение должно это либо починить (refresh-on-spawn), либо явно scope-out.

**Шаги (черновик)**:
1. **3.1** — research-задача: каталог всех callsite'ов которые мутируют engine state в run-time (включая summon, phase transitions, status applications). Документ.
2. **3.2** — выделить `bootstrap_combat_state` function (не system). Зовётся либо из event handler'а, либо из system при правильной фазе.
3. **3.3** — `build_turn_order` мигрирует на использование engine queue вместо ECS TurnQueue, либо инкремент round переносится в bootstrap.
4. **3.4** — удалить `engine_start_first_turn_system`, guard `>= 2`, stale TODOs.
5. **3.5** — миграция test harness: тесты зовут `bootstrap_combat_state(world)` напрямую вместо `run_system_once(init_state_from_ecs)`.
6. **3.6** — regression test "two combats in same App session" — explicit.
7. **3.7** — dynamic spawn fix (отдельно): для каждого Effect::Spawn в engine — derive `PopulatePerCombatFields` или эквивалент.

**Estimated effort**: 1-2 дня. Risk: средне-высокий (lifecycle changes, второй combat в сессии, тестовая инфраструктура).

### V4 — engine status reflow unification (separate, future)

Долг: два пути пересчёта (`status_bonuses` vs `status_def.damage_taken_bonus`). Унифицировать в одном месте, либо в `StatusBonuses { armor_bonus, speed_bonus, damage_taken_bonus }`, либо в `status_def.bonuses() -> StatusBonuses`.

**Scope**: только `combat_engine` crate, без bridge. Отдельная задача, не блокирует V2/V3.

## Tradeoff decision

Рекомендация критика-агента: **V2 сначала, V3 только если найдётся второй симптом того же класса**. Аргумент: V2 закрывает foot-gun хардкода `armor_bonus: 0` ценой одной правки сигнатуры и обновления 5 callsite'ов. V3 — это lifecycle-рефактор с непокрытыми scenario (dynamic spawn, second-combat, build_turn_order ordering). Архитектурная чистота V3 не оправдывает риск regression пока единственный известный симптом уже закрыт.

**Критерии чтобы триггернуть V3**:
- Обнаружен второй баг "status_bonus теряется при специфичном lifecycle".
- Планируется feature, требующий dynamic spawn с initial buffs (например, summon с aura).
- При работе над U5/E refactor этот вопрос всплывёт как блокер.

## Регрессионные тесты сделанные в V1

- [src/combat/engine_bridge.rs](../../../src/combat/engine_bridge.rs) — `tests` mod:
  - `ecs_content_view_status_bonuses_reads_real_armor_bonus` — прямая проверка stub-replacement.
  - `refresh_aggregates_via_ecs_content_view_picks_up_defending_armor` — end-to-end через `apply_effect(ApplyStatus + RefreshAggregates)`.

При V2 добавить: `from_ecs_preserves_armor_bonus_from_imported_status`.

## Ссылки

- Bug origin: playtest 2026-05-22, лог боя с Алдриком.
- V1 commit (после фикса taunter): отдельный коммит с fix `EcsContentView::status_bonuses`.
- Связано: [docs/engine-architecture.md](../../engine-architecture.md) — канонический layout post-unisim.
- Связано: U5/A-D рефакторы в `git log` (drop `BattleSnapshot.units`, migrate readers off snap.units).
