# План реализации первой волны доработок движка (ch2 Bell Under Veil)

Зафиксирован 2026-05-27. Источник архитектурных решений: раздел
«Архитектурные решения (зафиксировано 2026-05-27)» в
[engine-requirements.md](./engine-requirements.md).

Волна 1 — две независимые подгруппы, параллелизуемые:

- **Подгруппа 1.1 — NPC-объект + двойная victory** (T1.1.1 – T1.1.5)
- **Подгруппа 1.2 — Преграды + LOS** (T1.2.1 – T1.2.6)

Оценка: 4–5 чел-дней.

---

## Подгруппа 1.1 — NPC-объект + двойная victory

### Тикет T1.1.1. Маркерный компонент `NonActingNpc` и фильтрация очереди ходов

**Цель.** Ввести ECS-маркер для статичных NPC, исключаемых из инициативы.

**Затрагиваемые файлы.**
- `src/game/components.rs` — добавить `#[derive(Component, Clone, Copy, Default)] pub struct NonActingNpc;` рядом с `VictoryTarget`.
- `src/combat/turn_order.rs` — в `build_turn_order` добавить параметр `npc_q: Query<(), With<NonActingNpc>>` и пропускать таких при формировании `order`.
- `src/combat/engine_bridge.rs` — в `init_state_from_ecs` фильтровать NPC при заполнении `CombatState.units`.

**Принципиальное решение.** NPC живёт **только в ECS** — engine про него не знает (его нет в `CombatState.units`). Это согласуется с правилом «engine знает только про сущностей с ходами». Лечение/урон по NPC применяется bridge-side.

**Тесты** (новый файл `tests/non_acting_npc.rs`):
- `non_acting_npc_excluded_from_turn_queue` — `queue.order` содержит ровно players + enemies (без NPC).
- `non_acting_npc_can_be_damaged` — NPC HP=6, damage 4 → HP=2.
- `non_acting_npc_can_be_healed` — clamp к max_hp при overheal.
- `non_acting_npc_death_inserts_dead_marker` — damage до 0 → `Dead`.
- Property-test: для `players=1..3, enemies=1..3, npcs=0..3` `order.len() == players + enemies`.

**Доки.** `docs/combat/engine.md` — короткая ремарка: «NPC объекты (`NonActingNpc`) живут только в ECS, не в `CombatState.units`».

**Definition of Done.**
- [ ] Компонент `NonActingNpc` добавлен.
- [ ] `build_turn_order` пропускает NPC.
- [ ] `init_state_from_ecs` пропускает NPC.
- [ ] 4 unit-теста + 1 property-test зелёные.
- [ ] `cargo nextest run` целиком зелёный.

---

### Тикет T1.1.2. Подсчёт `players_alive` исключает NPC

**Цель.** Защита от ложной победы/поражения: NPC не должен «держать» команду живой.

**Затрагиваемые файлы.**
- `src/combat/advance_turn.rs::check_combat_end` — расширить Query с `Option<&NonActingNpc>`, пропускать таких в подсчёте `players_alive` / `enemies_alive`.

**Сигнатура:**
```rust
fn check_combat_end(
    combatants: &Query<(&Vital, &Faction, Option<&VictoryTarget>, Option<&NonActingNpc>), With<Combatant>>,
    objective: &VictoryCondition,
) -> Option<bool>
```

**Тесты** (`#[cfg(test)] mod tests` в advance_turn.rs):
- `outcome_party_with_only_npc_alive_counts_as_defeat`.
- `outcome_npc_does_not_satisfy_enemies_alive_for_all_enemies_dead`.

**Definition of Done.**
- [ ] `check_combat_end` обновлён, оба новых теста зелёные.
- [ ] Существующие `outcome_*` тесты остались зелёными.

---

### Тикет T1.1.3. Enum-расширение `VictoryCondition` + `determine_outcome` обходит дерево

**Цель.** Добавить варианты `KeepAlive` и `AllOf` в `VictoryCondition` и переписать `determine_outcome` под рекурсивный обход.

**Затрагиваемые файлы.**
- `src/content/encounters.rs::VictoryCondition` — расширить enum, обновить `objective_text`.
- `src/combat/advance_turn.rs::determine_outcome` — рекурсия + closure `is_named_alive`.

**Enum:**
```rust
pub enum VictoryCondition {
    #[default]
    AllEnemiesDead,
    KillTarget { enemy_name: String, marker_color: [f32; 3], description: Option<String> },
    /// Combat fails immediately if `target_name` (by `Name`) is dead.
    /// Leaf — succeeds only when paired in `AllOf`.
    KeepAlive { target_name: String, marker_color: [f32; 3] },
    /// Conjunction — all sub-conditions must hold; any defeat fails the tree.
    AllOf(Vec<VictoryCondition>),
}
```

**Логика `determine_outcome`:**
- `KeepAlive` если target мёртв → `Some(false)`, иначе `None` (leaf не побеждает сам).
- `AllOf` short-circuit на defeat; victory если все `Some(true)`.

**Тесты:**
- `outcome_keep_alive_target_dead_is_defeat`.
- `outcome_keep_alive_target_alive_with_enemies_alive_is_none`.
- `outcome_keep_alive_target_alive_no_enemies_is_victory`.
- `outcome_allof_short_circuits_on_first_defeat`.
- `outcome_nested_allof_evaluates_recursively`.
- `objective_text_renders_allof_with_and_separator`.

**Доки.**
- `docs/content-guide.md` — раздел «Encounters → Victory»: новые типы `keep_alive` и `all_of` с примером TOML.
- `docs/combat/engine.md` — пометка про рекурсию `VictoryCondition`.

**Definition of Done.**
- [ ] Enum расширен, `objective_text` рекурсивный.
- [ ] `determine_outcome` рекурсивный.
- [ ] `check_combat_end` собирает Name+Vital для `is_named_alive`.
- [ ] 6 unit-тестов зелёные.

---

### Тикет T1.1.4. TOML-парсинг для `keep_alive`, `all_of` и секции `[[encounters.npcs]]`

**Цель.** Контент-формат для двойной victory и для NPC.

**Затрагиваемые файлы.**
- `src/content/encounters.rs`:
  - `VictoryRecord` расширить `conditions: Option<Vec<VictoryRecord>>`, `target_name: Option<String>`.
  - `load_encounters_from_str` — новые ветки `"keep_alive"`, `"all_of"` (рекурсия).
  - `EncounterDef` + `EncounterRecord` — добавить `npcs: Vec<NpcDef>` / `Vec<NpcRecord>`.
  - Новые структуры `NpcDef` / `NpcRecord` + функция `resolve_npc`.

**Формат TOML:**
```toml
victory = { type = "all_of", conditions = [
    { type = "all_enemies_dead" },
    { type = "keep_alive", target_name = "Магистр", marker_color = [0.3, 0.6, 1.0] },
] }

[[encounters.npcs]]
name = "Магистр"
template = "wounded_magister"
hp_current = 6
hp_max = 6
hex_col = 6
hex_row = 4
```

**Тесты** (`tests/encounter_toml_v2.rs`):
- `parses_all_of_combined_with_keep_alive`.
- `parses_nested_all_of`.
- `parses_npcs_section_with_hex_pos`.
- `npcs_default_empty_when_section_omitted`.
- `keep_alive_without_target_name_panics`.
- `all_of_with_empty_conditions_is_legal`.

**Доки.** `docs/content-guide.md` — раздел про `npcs` секцию и victory типы.

**Definition of Done.**
- [ ] `VictoryRecord` поддерживает рекурсию.
- [ ] `load_encounters_from_str` парсит новые варианты.
- [ ] `EncounterDef.npcs` добавлен.
- [ ] 6 unit-тестов зелёные.
- [ ] Старые TOML ch1 парсятся без ошибок.

---

### Тикет T1.1.5. Spawn NPC entity + интеграционный e2e

**Цель.** Прикрутить `EncounterDef.npcs` к spawn-системе боя, провести полный e2e.

**Затрагиваемые файлы.**
- `src/scenario/combat_scene.rs::spawn_combatants` — после цикла `enc.enemies` добавить цикл `enc.npcs`. Spawn с `Faction(Team::Player) + NonActingNpc`, без `AiMemory/Abilities/Initiative`.
- `src/game/bundles.rs` — посмотреть нужна ли отдельная функция; для одного use-case spawn вручную.

**Тесты** (`tests/non_acting_npc_e2e.rs`):
- `e2e_kill_all_with_alive_npc_is_victory` — все враги мертвы, NPC жив → Victory.
- `e2e_kill_npc_mid_combat_is_defeat` — NPC убит до окончания → Defeat сразу.
- `e2e_npc_in_turn_queue_is_skipped` — flow start_actor_turn для NPC не вызывается.

**Контент fixture** для бой 2: `assets/data/campaigns/bell_under_veil/ch2/scenarios/ch2_shrine/encounters.toml` + `wounded_magister` в `unit_templates.toml` (минимум: max_hp=6, без abilities).

**Доки.** `docs/combat/engine.md` — раздел «Non-acting NPCs».

**Definition of Done.**
- [ ] `spawn_combatants` спаунит entity на каждый `NpcDef`.
- [ ] 3 e2e-теста зелёные.
- [ ] Schema bump НЕ нужен (NPC только в ECS).
- [ ] `cargo nextest run` полный зелёный.

---

## Подгруппа 1.2 — Преграды + LOS

### Тикет T1.2.1. `CombatState.blocked_hexes` + schema bump v42→v43

**Цель.** Положить в pure engine множество блокирующих гексов.

**Затрагиваемые файлы.**
- `crates/combat_engine/src/state.rs`:
  - `CombatState` — добавить `pub blocked_hexes: HashSet<Hex>`.
  - `CombatStateRepr` — добавить `blocked_hexes` как **сортированный `Vec<Hex>`** для детерминированной сериализации.
  - `CombatState::new` инициализирует пустым множеством.
- `src/combat/ai/log/mod.rs` — `SCHEMA_VERSION = 43`, комментарий по миграции v42→v43.
- `measurements/*.jsonl` — регенерация baseline.

**Команда регенерации:**
- `cargo run --bin mine_ai_logs -- --regenerate-baseline` (точное имя бинарника проверить в `src/bin/`).

**Bump-комментарий:**
```rust
/// v42 → v43: `CombatState.blocked_hexes: HashSet<Hex>` added (Wave 1
/// ch2 — static obstacles for movement and LOS). Serialized as sorted
/// `Vec<Hex>` in CombatStateRepr for deterministic output. Old v42 logs
/// are incompatible — clean break.
pub const SCHEMA_VERSION: u32 = 43;
```

**Тесты** (`crates/combat_engine/tests/serde_roundtrip.rs`):
- `state_with_blocked_hexes_serde_roundtrip`.
- `state_blocked_hexes_serialization_is_deterministic`.
- `parse_actor_tick_v42_returns_unsupported_schema`.

**Доки.**
- `docs/combat/engine.md` — секция `CombatState` упоминает `blocked_hexes`.
- `docs/ai/extension-checklist.md` — schema bump v42→v43 запись.

**Definition of Done.**
- [ ] Поле добавлено в `CombatState` и `CombatStateRepr`.
- [ ] `SCHEMA_VERSION = 43`, комментарий обновлён.
- [ ] Все `measurements/*.jsonl` baseline регенерированы.
- [ ] 3 теста на serde зелёные.
- [ ] **Полный `cargo nextest run`** — обязательная checkpoint.

---

### Тикет T1.2.2. `MovementEnv.blocked_hexes` — pathfinding учитывает преграды

**Цель.** A*/BFS считают `blocked_hexes` непроходимыми (и для движения, и для can_stop).

**Затрагиваемые файлы.**
- `src/game/pathfinding.rs`:
  - `MovementEnv` — поле `pub blocked_hexes: HashSet<Hex>` (default пусто).
  - `reach_from` — расширить closures для `is_passable` и `can_stop_on`.
- `src/combat/engine_bridge.rs` — места конструирования `MovementEnv` прокидывают `state.blocked_hexes.clone()`. Через `find_referencing_symbols MovementEnv` найти **все** callsites (AI sim, UI movement preview).

**Сигнатура:**
```rust
pub struct MovementEnv {
    pub enemy_positions: HashSet<Hex>,
    pub stop_blockers: HashSet<Hex>,
    /// Static obstacles — blocks both pass-through and stopping.
    pub blocked_hexes: HashSet<Hex>,
}
```

**Тесты:**
- `obstacle_blocks_movement_through`.
- `obstacle_blocks_stopping_on`.
- `obstacle_does_not_block_diagonal_path`.
- `empty_blocked_hexes_does_not_change_behavior`.

**Доки.** `docs/hex-grid.md` — раздел «Movement Rules»: `blocked_hexes`.

**Definition of Done.**
- [ ] `MovementEnv.blocked_hexes` добавлено.
- [ ] `reach_from` учитывает преграды для pass-through и stopping.
- [ ] 4 unit-теста зелёные.
- [ ] Все существующие pathfinding-тесты зелёные.

---

### Тикет T1.2.3. `ActionState::is_blocked_los` + `AbilityDef.requires_los` + `IllegalReason::NoLineOfSight`

**Цель.** Engine-side проверка LOS для дальнобойных атак.

**Затрагиваемые файлы.**
- `crates/combat_engine/src/content.rs::AbilityDef` — поле `pub requires_los: bool` (default false).
- `crates/combat_engine/src/legality.rs`:
  - `IllegalReason::NoLineOfSight` вариант.
  - `ActionState::is_blocked_los(&self, from: Hex, to: Hex) -> bool` с default-impl `false`.
  - `check_legality` — после блока range check, если `def.requires_los && def.range.max > 1 && state.is_blocked_los(actor.pos, action.target_pos)` → `Err(NoLineOfSight)`.

**Тесты** (`crates/combat_engine/tests/legality.rs`):
- `legality_blocks_ranged_attack_through_obstacle`.
- `legality_allows_ranged_attack_with_clear_los`.
- `legality_skips_los_for_melee_range_1`.
- `legality_skips_los_when_requires_los_false`.
- `default_action_state_returns_no_los_blocking`.

**Доки.**
- `docs/combat/engine.md` — раздел «Legality / Range»: LOS.
- `docs/content-guide.md` — `requires_los` поле в abilities.

**Definition of Done.**
- [ ] `AbilityDef.requires_los`, `IllegalReason::NoLineOfSight`, `ActionState::is_blocked_los` добавлены.
- [ ] `check_legality` проверяет LOS только для `requires_los && range.max > 1`.
- [ ] 5 unit-тестов зелёные.

---

### Тикет T1.2.4. `BevyActions::is_blocked_los` + `SnapshotActions::is_blocked_los` + engine-side impl через единую `has_los`

**Цель.** Все три бэкенда `ActionState` реализуют `is_blocked_los` через **одну и ту же функцию** `has_los` из `src/game/hex.rs`. Гарантия parity между AI-sim и ECS-side.

**Затрагиваемые файлы.**
- `src/combat/legality_adapter.rs::BevyActions` — поле `pub blocked_hexes: &'a HashSet<Hex>`, impl `is_blocked_los`.
- `src/combat/validation.rs` — синхронизировать вторую `BevyActions` (если она дубль — унифицировать).
- `src/combat/ai/plan/generator.rs` (или соответствующий) — `SnapshotActions::is_blocked_los`. Через `find_referencing_symbols ActionState` в `src/combat/ai/plan/` найти точное место impl.
- `src/combat/ai/world/snapshot.rs::BattleSnapshot::new` — копировать `state.blocked_hexes`.
- `crates/combat_engine/src/step.rs` — engine-side `ActionState` impl: `is_blocked_los` через `state.blocked_hexes`.

**Дилемма зависимостей.** `has_los` живёт в `src/game/hex.rs` (storyforge crate). Engine crate (`crates/combat_engine`) сейчас pure. Решение:

- **Опция A (предпочтительно):** перенести `has_los` (и `hex_line`) в `combat_engine` (новый модуль `geom`), если `hexx` уже в зависимостях engine crate. Storyforge re-export'ит для обратной совместимости.
- **Опция B (fallback):** дублировать функцию в engine с fixpoint-тестом equality.

**Решение принимается имплементером** по факту проверки `crates/combat_engine/Cargo.toml`.

**Тесты** (`tests/los_parity.rs`):
- `bevy_actions_is_blocked_los_matches_has_los`.
- `snapshot_actions_is_blocked_los_matches_has_los`.
- `engine_action_state_is_blocked_los_matches_has_los`.
- **Property-test `prop_all_three_backends_agree_on_los`** — для случайных `blocked_hexes` и пары `(from, to)` все три impl'а дают одинаковый ответ.

**Доки.** `docs/combat/engine.md` — раздел про `ActionState` упоминает parity-контракт.

**Definition of Done.**
- [ ] `is_blocked_los` реализован во всех трёх бэкендах.
- [ ] Все бэкенды используют одну функцию `has_los`.
- [ ] 4 теста parity зелёные.
- [ ] Property-test покрывает >100 случаев.

---

### Тикет T1.2.5. TOML-парсинг `[[encounters.obstacles]]` + bootstrap в `CombatState.blocked_hexes`

**Цель.** Контент-формат для преград и его проброс в engine state.

**Затрагиваемые файлы.**
- `src/content/encounters.rs`:
  - `EncounterDef.obstacles: Vec<hexx::Hex>` (новое поле).
  - `EncounterRecord.obstacles: Vec<ObstacleRecord>` (с `#[serde(default)]`).
  - Новая `ObstacleRecord { hex_col, hex_row }`.
- `src/combat/engine_bridge.rs::bootstrap_combat_state` — после создания `CombatState` присвоить `state.blocked_hexes = enc.obstacles.iter().copied().collect()`.
- `src/combat/engine_bridge.rs::reset_engine_mirrors[_on_exit_combat]` — очищать `blocked_hexes` при reset.

**Формат TOML:**
```toml
[[encounters.obstacles]]
hex_col = 5
hex_row = 3
```

**Тесты:**
- `parses_obstacles_section`.
- `obstacles_section_optional`.
- `bootstrap_combat_state_populates_blocked_hexes`.

**Доки.** `docs/content-guide.md` — раздел про `[[encounters.obstacles]]`.

**Definition of Done.**
- [ ] `EncounterDef.obstacles` парсится.
- [ ] `bootstrap_combat_state` заполняет `state.blocked_hexes`.
- [ ] `reset_*` очищают.
- [ ] 3 теста зелёные.

---

### Тикет T1.2.6. E2E: `requires_los = true` блокирует AI-выбор цели за преградой + UI-target-disable

**Цель.** Полный сквозной тест: AI-юнит со стрелковой `requires_los=true` не выбирает цель за преградой.

**Затрагиваемые файлы.**
- `src/content/abilities.rs::AbilityRecord` — `#[serde(default)] requires_los: bool`, парсер пробрасывает.
- `crates/combat_engine/src/toml_content_view.rs::convert_ability` — то же.
- `src/ui/hex_grid/render.rs` (или эквивалентное target-selection место) — `NoLineOfSight` автоматически дезактивирует цель. Минимум: не выбираемая.

**Тесты** (`tests/los_ai_e2e.rs`):
- `ai_archer_skips_target_behind_obstacle` — в plan() нет cast'а в hero за obstacle.
- `ai_archer_picks_alternative_target_without_los_constraint` — AI выбирает цель на диагонали.
- `player_target_selection_excludes_obstructed_enemies`.

Расширить `tests/toml_content_view_parity.rs::abilities_eq` — добавить `requires_los` в parity-сравнение.

**Контент fixture** для бой 3: `assets/data/campaigns/bell_under_veil/ch2/scenarios/ch2_portside/encounters.toml` + `bandit_archer` с ability `bow_shot { range = 1..5, requires_los = true }` в `abilities.toml`.

**Доки.** `docs/content-guide.md` — пример ability с `requires_los = true`.

**Definition of Done.**
- [ ] `requires_los` парсится в обоих лоадерах.
- [ ] Parity-тест `toml_content_view_matches_ecs_content_view` зелёный.
- [ ] 3 e2e теста зелёные.
- [ ] `cargo bench` (если есть combat scenarios) не показывает регрессии >5%.

---

## Зависимости (DAG)

```
Подгруппа 1.1:
T1.1.1 ──► T1.1.2
       ──► T1.1.5
T1.1.3 ──► T1.1.4 ──► T1.1.5

Подгруппа 1.2:
T1.2.1 ──► T1.2.2
       ──► T1.2.5
T1.2.3 ──► T1.2.4
T1.2.4 ──► T1.2.6
T1.2.5 ──► T1.2.6
```

Подгруппы НЕ ПЕРЕСЕКАЮТСЯ — можно делать параллельно двумя людьми.

**Рекомендуемый порядок мерджа (для одного исполнителя):**
1. T1.1.1
2. T1.1.2
3. T1.1.3
4. T1.2.1 (**schema bump checkpoint**)
5. T1.2.3
6. T1.2.2
7. T1.1.4
8. T1.2.4
9. T1.2.5
10. T1.1.5 (e2e NPC)
11. T1.2.6 (e2e LOS)

---

## Schema bump план (v42 → v43)

**В каком тикете бампается:** T1.2.1.

**Что регенерируется:**
- Все JSONL baseline в `measurements/`.
- Все golden-логи в `tests/golden_logs/` если такие есть.

**Что НЕ меняется:**
- ECS-only data (`NonActingNpc`, `VictoryTarget`) — не сериализуются в engine.jsonl.
- TOML-форматы — контент, не log schema.

---

## Что НЕ трогаем в этой волне

**Категорически не лезем:**
- Hazards / env-объекты с эффектами (ловушки) — **волна 3**.
- Kael perception / BFS reveal — волна 3.
- Severity для AI pathfinding — волна 3.
- Knowledge model (`knows_env`) — волна 3.
- Phase victory override / `turn_limit` — **волна 2**.
- Flee AI (`EvaluationMode::Flee`) — волна 2.
- `PhaseDef.ai_behavior` — волна 2.

**Опциональные «крючки» для будущих волн** (на усмотрение исполнителя):
- В T1.2.5 — `ObstacleRecord.kind: Option<String>` (default `"obstacle"`) для будущего `"trap"`.
- В T1.2.1 — обернуть `blocked_hexes` в модуль `environment` (даже если содержит только это поле) — упростит миграцию на `EnvObject`.

---

## Риски и mitigation

| Тикет | Риск | Mitigation |
|---|---|---|
| T1.1.1 | NPC с `Combatant` попадёт в `build_turn_order`. | `Without<NonActingNpc>` в Query — явная фильтрация. |
| T1.1.2 | Сломаются ch1 outcome тесты. | Прогон всех `outcome_*` обязателен. |
| T1.1.3 | Изменение сигнатуры `determine_outcome` ломает callers. | Compile-error → правка единственного места (`check_combat_end`). |
| T1.1.4 | Старый TOML ch1 не содержит `npcs`. | `#[serde(default)]` + `npcs_default_empty_when_section_omitted` тест. |
| T1.1.5 | AI партии может «лечить» NPC. | Это **желаемое поведение** (ТЗ решение 5). Если есть особая фильтрация — проверить. |
| T1.2.1 | Schema bump ломает все baseline. | План регенерации; `git diff` baseline для масштаба. |
| T1.2.2 | `MovementEnv.blocked_hexes` нужно во **всех** callsites. | `find_referencing_symbols MovementEnv` — обновить каждый. |
| T1.2.3 | Default `is_blocked_los → false` — забытый override тихо разрешит стрельбу. | Property-test parity в T1.2.4 — главный страж. |
| T1.2.4 | **Главный риск parity** — разные `blocked_hexes` в ECS vs snapshot. | В `BattleSnapshot::new` явно копировать; property-test покрывает. |
| T1.2.5 | Bootstrap при restart — `blocked_hexes` может «утечь» с предыдущего боя. | `reset_engine_mirrors[_on_exit_combat]` очищают. |
| T1.2.6 | `requires_los` в parity-тесте обязан совпадать в обоих лоадерах. | Расширить `abilities_eq` в `tests/toml_content_view_parity.rs`. |

---

## Точки проверки

**Полный `cargo nextest run` обязателен после:**
- T1.2.1 (schema bump — sanity check).
- T1.2.4 (parity check — критическая точка).
- T1.1.5 (NPC e2e — финальный для 1.1).
- T1.2.6 (LOS e2e — финальный для 1.2).

**`cargo bench` (регрессия):**
- После T1.2.2 — pathfinding с `HashSet.contains`. Acceptance: <3%.
- После T1.2.6 — полный combat с LOS. Acceptance: <5%.

---

## Контент-сторона: минимальные fixture'ы ch2

### Fixture для бой 2 (NPC + двойная victory)

`assets/data/campaigns/bell_under_veil/ch2/scenarios/ch2_shrine/encounters.toml`:
```toml
[[encounters]]
id = "ch2_shrine"
name = "Тронутое святилище"

victory = { type = "all_of", conditions = [
    { type = "all_enemies_dead" },
    { type = "keep_alive", target_name = "Магистр", marker_color = [0.3, 0.6, 1.0] },
] }

[[encounters.npcs]]
name = "Магистр"
template = "wounded_magister"
hp_current = 6
hp_max = 6
hex_col = 6
hex_row = 4

[[encounters.enemies]]
template = "cultist_grunt"
hex_col = 2
hex_row = 2

[[encounters.enemies]]
template = "cultist_grunt"
hex_col = 8
hex_row = 5
```

Нужен `wounded_magister` в `unit_templates.toml` (min: max_hp=6, без abilities, без weapon). Если шаблона нет — создать в T1.1.5.

### Fixture для бой 3 (преграды + LOS)

`assets/data/campaigns/bell_under_veil/ch2/scenarios/ch2_portside/encounters.toml`:
```toml
[[encounters]]
id = "ch2_portside"
name = "Портовый квартал"
victory = { type = "all_enemies_dead" }

# Стена из ящиков посредине поля
[[encounters.obstacles]]
hex_col = 5
hex_row = 3
[[encounters.obstacles]]
hex_col = 5
hex_row = 4
[[encounters.obstacles]]
hex_col = 5
hex_row = 5

[[encounters.enemies]]
template = "bandit_archer"
hex_col = 9
hex_row = 4

[[encounters.enemies]]
template = "bandit_thug"
hex_col = 8
hex_row = 3
```

Нужен `bandit_archer` с `bow_shot { range = 1..5, requires_los = true }` в `abilities.toml`. Если abilty/template нет — добавить в T1.2.6.

### Acceptance для fixture'ов

- TOML парсятся через `load_encounters_from_str` без panic.
- `validate_scenario` даёт OK.
- E2E из T1.1.5 и T1.2.6 используют **именно эти** TOML файлы (не inline hardcoded строки).
