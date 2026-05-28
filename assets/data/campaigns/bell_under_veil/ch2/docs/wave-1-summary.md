# Wave 1 ch2 — итог выполненной работы

Историческая сводка всех изменений сделанных в рамках первой волны
доработок движка под главу II «Bell Under Veil». Источники:
- ТЗ + архитектурные решения: [engine-requirements.md](engine-requirements.md).
- Детальный план реализации: [wave-1-plan.md](wave-1-plan.md).

Эта сводка — то, что **фактически попало в код**, и что осталось открытым.

---

## Закрытые механики из ТЗ

| # из ТЗ | Механика | Статус |
|---|---|---|
| 2 | NPC-объект на поле | ✅ через `party_add` + `initial_statuses` |
| 3 | Двойная цель победы `AllOf(kill_all, KeepAlive(NPC))` | ✅ recursive `VictoryCondition` |
| 4 | Преграды на гексах (movement + LOS) | ✅ `CombatState.blocked_hexes` |
| 5 | LOS для дальнобойных атак | ✅ `AbilityDef.requires_los` + `has_los` |

**Не входило в волну 1:**
- 1. Ловушки на местности (env subsystem) — волна 3.
- 6. Смена цели победы между фазами — волна 2.
- 7. Поведение «бегство» (AI) — волна 2.

---

## Хронология коммитов

### Волна 1 — основной цикл (11 тикетов плана)

| Commit | Что |
|---|---|
| `32c54dd` | **T1.1.1** `NonActingNpc` маркер + фильтр `build_turn_order` |
| `2279236` | **T1.1.2** `check_combat_end` исключает NPC из `players_alive` |
| `6e04d25` | **T1.1.3** `VictoryCondition::{KeepAlive, AllOf}` + рекурсивный `determine_outcome` |
| `c01abe1` | **T1.2.1** `CombatState.blocked_hexes` + schema bump v42→v43 |
| `706365f` | **T1.2.3** `ActionState::is_blocked_los` + `AbilityDef.requires_los` + `IllegalReason::NoLineOfSight` |
| `51c071a` | **T1.2.2** `MovementEnv.blocked_hexes` — pathfinding учитывает obstacles |
| `dd1d72a` | **T1.1.4** TOML парсинг `keep_alive` / `all_of` / `[[encounters.npcs]]` |
| `3248541` | **T1.2.4** LOS parity: `has_los` через `combat_engine::geom`, default-impl в trait |
| `9ba11c8` | **T1.2.5** TOML `[[encounters.obstacles]]` + bootstrap в `CombatState.blocked_hexes` |
| `31d053a` | **T1.1.5** Spawn NPC + e2e `KeepAlive` victory/defeat |
| `4913ce7` | **T1.2.6** `requires_los` TOML wiring + LOS AI e2e + ch2_portside fixture |

Итог волны 1: 1211 → 1211 тестов зелёные, 11 тикетов закрыты.

### Code review + последствия

Внешний обзор выявил 22 findings. Ниже — те, что отработали:

| Commit | Что |
|---|---|
| `51fae43` | **F1+F3+F5+F11 rework**: NPC через `party_add` + `template` + `initial_statuses` engine-side (вместо ad-hoc `[[encounters.npcs]]`). Сложный refactor нарушения арх. решения 5 |
| `7dc661a` | **F2** boundary: `Effect::Spawn` (mid-combat summon) тоже применяет `initial_statuses` через общий helper |
| `2fd53d4` | **F4**: унификация `is_blocked_los` через trait default-impl над абстрактным `blocked_hexes()` getter'ом. Логика LOS в одном физическом месте |
| `f0e3e80` | **F7**: `BlindspotRanged` critic учитывает `state.blocked_hexes` + 1-step kite-yard lookahead |
| `2097db7` | **F6**: e2e тест `generate_plans` не предлагает Cast сквозь obstacle (sandwich-pattern: control без obstacle → есть Cast; с obstacle → нет) |
| `4f58d9c` | **F10+F17**: валидация `KeepAlive.target_name` / `KillTarget.enemy_name` при `validate_scenario` (fail-fast на typo) |
| `a837d26` | **F8** ч.1: документация (hex-grid, engine.md, extension-checklist) |
| `557ee59` | **F8** ч.2: `docs/content-guide.md` приведён в соответствие с party_add-rework |

### Контентные изменения для demo и ch2

| Commit | Что |
|---|---|
| `03c37ab` | Демо-кампания: бой 1 получил `keep_alive(scout)` victory + NPC-spawn, бой 2 — пара obstacles |
| `626a209` | AI awareness keep_alive целей: `KeepAliveTarget` ECS компонент + `AiTags::OPPONENT_OBJECTIVE` + buff в `unit_value` + ось `objective_priority` в `target_selection_score` |
| `bec0f9c` | Layout демо-кампании: партия вниз, разведчик в верхний-левый угол — обеспечен distance > AI turn-1 reach |

### HP-as-pool рефактор (вызван желанием `initial_pools`)

Изначально хотели только `initial_hp` для wounded NPC. Решили сделать **архитектурно правильно** — мигрировать HP в `Unit.pools[PoolKind::Hp]` чтобы все ресурсы (HP/Mana/Rage/Energy/Ap/Mp) имели единый API. Это позволило ввести универсальный `initial_pools` для любого pool через TOML.

| Commit | Stage |
|---|---|
| `d867ab0` | **0+1** Helpers `Unit::hp()/max_hp()` + dual-write safety net (`PoolKind::Hp` variant) |
| `658e055` | **2** Mass-rename reads `u.hp` → `u.hp()` (143 callsites в 33 файлах) |
| `68fa24a` | **3a** Writes pool-first, `hp/max_hp` поля становятся mirror |
| `2d7539d` | **3b** Struct literals → `Unit::new` factory (10 sites) |
| `d4a5fc1` | **3c** Удаление `Unit.hp` / `Unit.max_hp` полей + schema bump v44 |
| `18abdcb` | **5** Documentation update (architecture, content-guide, extension-checklist) |
| `9cff634` | **`initial_pools` feature** на `UnitTemplate` (engine + bridge + TOML); демо `wounded_scout` 6/12, ch2 магистр 4/8 |

7 коммитов, 1216 тестов зелёные (+4 за refactor), HP теперь шестой PoolKind с симметричным API.

### Финальные фиксы боевой playability

| Commit | Что |
|---|---|
| `55771cb` | Bridge fix: template-based party members получают `Speed/AP/Reactions/Mana/Rage/Energy` — без них entity невидим для engine `from_ecs` |
| `90997cc` | Bridge fix follow-up: использовать `hero_bundle` для полного набора компонентов (AI cache требует Abilities/CombatStats/Equipment поверх engine query) |
| `4567558` | Bridge: `Option<>`-fields в `CombatantRow` + fail-loud warn при missing components у `Combatant`-entity. Хвост от прошлых silent-skip багов |
| `d712a16` | **UI**: рендер obstacles коричневым (`CLR_OBSTACLE`) + cast range overlay фильтруется по LOS. + `#[derive(Default)]` на `HexMaterials` (cleanup boilerplate) |

---

## Архитектурные решения (закреплённые в коде)

1. **`pools[PoolKind::Hp]` — единственный источник истины для HP** (ранее `Unit.hp` отдельное поле). Шесть pool-видов в EnumMap, симметричный API.
2. **`initial_pools` контент-параметр** на `UnitTemplate` — задаёт стартовое состояние любого pool (`hp = 6, mana = 0, rage = 5`). Default policy: max для всех кроме Rage (= 0). Clamp в spawn.
3. **`initial_statuses` контент-параметр** на `UnitTemplate` — список статусов применяемых при spawn с `PERMANENT_DURATION` sentinel.
4. **NPC = временный союзник партии** через `party_add` + `template` + `initial_statuses = ["stunned"]`. Не ad-hoc `[[encounters.npcs]]` секция.
5. **`KeepAlive` victory** = leaf-condition в `AllOf`-дереве. Defeat немедленный при смерти target'а.
6. **Static obstacles** в `CombatState.blocked_hexes` (engine knows). Bridge mirror `CombatBlockedHexes` для UI/AI. Pathfinding и LOS используют один источник.
7. **Единый `has_los`** в `combat_engine::geom` — re-export из storyforge. Все три `ActionState` backend'а используют через default-impl trait метода, override только абстрактный getter `blocked_hexes()`.
8. **AI приоритизирует opponent's keep_alive target** через `AiTags::OPPONENT_OBJECTIVE` → `unit_value` boost + `objective_priority` ось в `target_selection_score`. ECS компонент `KeepAliveTarget` вешается в `spawn_combatants` (рекурсивный walk дерева).
9. **`BlindspotRanged` critic** учитывает `state.blocked_hexes` для LOS-blockers + 1-step kite-yard lookahead (ranged не штрафуется если соседняя клетка имеет LOS — поддержка hide-then-shoot тактики).
10. **UI визуализирует obstacles** коричневой заливкой; cast range overlay фильтруется по LOS для `requires_los` ability.
11. **Fail-loud при missing components**: `bootstrap_combat_state` warn'ит когда `Combatant`-entity не имеет Speed/AP/Reactions/Abilities/CombatStats/Equipment. Защита от silent-skip регрессий.
12. **Schema bumps**: v42 → v43 (`blocked_hexes`), v43 → v44 (HP-as-pool, `Unit.hp/max_hp` removed).

---

## Контент готовый к игре

### Бой 1 demo (`beastblood_raid`)
- Layout: партия (Aldric, Lyra) внизу, разведчик в углу (0,1).
- NPC: «Раненый разведчик» (`wounded_scout`, 6/12 HP, permanent stun).
- Victory: `AllOf(all_enemies_dead, keep_alive("Раненый разведчик"))`.
- Враги: Страж (6,2), Налётчик (6,4).
- Distance scout↔enemy ≈ 6 hex > AI turn-1 reach (5) — NPC не one-shot'нится в turn 1.

### Бой 2 demo (`stormborn_camp`)
- Obstacles: (3,2) и (4,4) — поваленные деревья.
- Боевые рамки: партия должна прорваться или обойти.
- Victory: `kill_target("Старшина")` + phase trigger (как было в ch1).

### Бой 2 ch2 (`ch2_shrine`)
- Магистр (`wounded_magister`, 4/8 HP, permanent stun) присоединяется через `party_add` story-сцены.
- Victory: `AllOf(all_enemies_dead, keep_alive("Магистр"))`.
- Враги: 2 культиста.

### Бой 3 ch2 (`ch2_portside`)
- Стена из 3 obstacles по col=5 — разделяет поле.
- Враги: bandit_archer (с `bow_shot { range=1..5, requires_los=true }`), bandit_thug.
- Victory: `all_enemies_dead`.

---

## Что осталось открытым

### Из ТЗ ch2 (волны 2 и 3)
- **Phase victory override + `turn_limit`** (бой 4 — Колм переключается в фазу побега).
- **Поведение «бегство» AI** (EvaluationMode::Flee).
- **Ловушки + env subsystem + Kael perception** — волна 3.

### Из code review (F-findings)
- **F9** — `cargo bench` baseline ещё не зафиксирован для T1.2.2/T1.2.6 acceptance.
- **F12–F22** (minor/nit) — clippy warnings, dead code allow, мелочи. Не блокеры.

### Технический долг
- `Option<>` migration `CombatantRow` сделана частично — fail-loud warn есть, но дефолты ещё не применяются на пропущенных компонентах. Дальнейший refactor сделает minimal-NPC spawn one-liner'ом.
- `EngineCheckState` (`actor_knows_ability` всегда true) — engine не отслеживает per-unit ability lists. Адресуется когда per-unit abilities станут engine-authoritative.

---

## Метрики

| Метрика | Значение |
|---|---|
| Коммитов в волне | 34 |
| Тестов: было / стало | 1217 → 1219 (1216 после refactor + 3 за initial_pools) |
| Schema bump'ов | 2 (v42→v43→v44) |
| Net diff | большой положительный (initial_pools + parity + AI awareness + UI obstacles) |
| Ломающих изменений в content TOML | 0 (старый формат продолжает парситься) |
