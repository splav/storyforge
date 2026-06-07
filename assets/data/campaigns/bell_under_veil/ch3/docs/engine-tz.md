# ch3 — ТЗ на реализацию движка (planner + critic)

> Реализационное ТЗ к `engine-requirements.md`. Прошло planner (привязка к коду) и
> plan-critic-reviewer (стресс-тест). Все пути — от корня репо. Тесты:
> `cargo nextest run --workspace --features dev` (без `--workspace` inline-тесты движка
> молча пропускаются). Engine-тесты: white-box inline в `src`/`crates/combat_engine/src`;
> публичные engine+bridge — `tests/combat_engine/*.rs`; full-app — `tests/combat/*.rs`.
> **Каталог `crates/combat_engine/tests/` не создавать.**

## 0. Решения до кодинга

| # | Развилка | Решение | Почему |
|---|---|---|---|
| A | **Теги цели (Атом 3)** | **РЕАЛИЗОВАТЬ** — заказчик выбрал гарантию. `Unit.tags: Set<Tag>` через стек | один примитив, 2 потребителя: предикат хила пастуха (Б.2, `requires: symbiote`) + аура накопителя и её гарантированный обрыв в фазе 3 (Б.4). SCHEMA-bump легитимен |
| B | Хранилище флагов | **Единый `flags: BTreeSet<String>`** | строковый contract уже есть (`requires_flag: Option<String>`); убирает `BoutKey`/`Outcome`/`Map` из сейва; миграция ch1-флагов тривиальна (они уже строки) |
| C | `injured` на Aldric | **Обобщено в Атоме 5** (status-fold): `PERMANENT`-статус, `add` перед боем 2; снять/ослабить/забаффать — `remove`+`add` в поздней сцене (арка восстановления) | «однобоевая рана» — частный паттерн фолда; нарративная гибкость без потери C-поведения |
| D | Оценка objectives | **Чистая `fn objective_met(cond, final_state) -> bool`** отдельно от `determine_outcome` | `determine_outcome` несёт семантику «KeepAlive-death → defeat», для objective не годится |
| E | SCHEMA-bump | **Атомы 3 и 4 — одним инкрементом** (46→47): `Unit.tags` + `Effect::AddTag/RemoveTag` (А3) и `Effect::TickHeal` (А4) | один разрыв совместимости трейсов вместо двух |

> Теги — плоский `Set<Tag>`; 3 оси (вид / тело / жизнь) — документационная группировка,
> не enforced, авторятся по необходимости. Заменяют узкую «расу». Детали — Атом 3.

---

## Атом 1 — Persistent-флаги + `on_defeat: proceed` + objectives (L)

> **Прогресс:** ✅ **срез A готов** (коммит `80300ab`) — хранилище `CampaignState.flags` +
> `CampaignProgress.flags` (V1 in-place), запись `on_victory_flags` на `OnEnter(Victory)` до
> autosave, load-восстановление, `requires_flag` читает из набора, `active_flags` удалён,
> `record_progress(&CampaignState)`. Поведение ch1/ch2 сохранено, SCHEMA не тронут, +6 тестов (1405).
> ✅ **срез B готов** — `EncounterDef.{objectives, on_defeat}` + парс TOML, чистая `objective_met`,
> система `write_objective_flags` на `OnEnter(Victory|Defeat)` (на defeat пишет только при
> `on_defeat=Proceed`), defeat-overlay ветвится Retry/Proceed, `defeat_proceed`-input →
> `AdvanceScenario`, `advance_scenario_system` работает в `CombatPhase::Defeat`. BLOCKER снят:
> `CombatPhase` — SubState от `AppState::Combat`, при proceed авто-`OnExit(Defeat)` чистит оверлей.
> +27 тестов (1431). SCHEMA не тронут.
>
> **Итог Atom 1 (пересмотр):** срез A **сохранил** `on_victory_flags` как рабочий путь записи
> victory-флагов (не legacy), а `objectives` вышли **аддитивным** механизмом для условных/secondary/
> proceed целей → **два механизма с разными ролями** (см. ниже «Миграция»). Atom 1 закрыт.

### Модель данных
- `CampaignState` (`src/game/resources.rs:468`): `+ flags: BTreeSet<String>`.
- `CampaignProgress` (`src/persistence/save_repo.rs:25`): `+ #[serde(default)] flags: Vec<String>` (V1 расширяется in-place, новой версии не надо). **`record_progress`** (`save_repo.rs:104`) — передавать `&CampaignState` целиком вместо позиционных полей (заодно лечит too_many_arguments).
- `EncounterDef` (`src/content/encounters.rs`): `+ on_defeat: OnDefeat{Retry,Proceed}` (дефолт Retry), `+ objectives: Vec<ObjectiveDef{id, condition: VictoryCondition, hidden}>`. TOML-records: `#[serde(default)]` на оба; `condition` через существующий `resolve_victory`.

### Новая обвязка (критик: этого пути СЕЙЧАС НЕТ)
1. **Система записи на конце боя** — `OnEnter(CombatPhase::Victory)` и (для proceed) `OnEnter(CombatPhase::Defeat)`: читает финальные `Vital`/`Name`/`Faction`, пишет в `CampaignState.flags`:
   - маркер `"<scenario>.<bout>.victory"` при победе;
   - для каждой `objectives[i]`: `if objective_met(&cond, final) { flags.insert(id) }` — **при любом завершённом исходе** (победа ИЛИ proceed-поражение).
   - Разместить в расписании **до** `advance_scenario_system` (который пишет autosave) — иначе флаги не попадут в сейв.
2. **`fn objective_met`** (новая, чистая): leaf-предикаты (`KeepAlive`→«name жив», `AllEnemiesDead`→«нет живых врагов», `AllOf`→конъюнкция). Тестируется без ECS.
3. **Defeat→proceed путь** (критик BLOCKER — отдельный кусок UI+state-machine):
   - defeat-overlay (`src/ui/combat_ui.rs:312`) — развилка по `EncounterDef.on_defeat`: `Proceed` → кнопка «дальше» → `AdvanceScenario`; `Retry` (дефолт) → текущее `RestartCombat`.
   - новый `defeat_proceed_input_system` (или ветка в существующем), `run_if(in_state(CombatPhase::Defeat))`, в `src/scenario/input.rs` + регистрация в `src/main.rs`.
   - reset партии: партийцы и так пере-спавнятся из `class`/`template` при входе в следующую combat-сцену (party-стейт между боями **не персистится** — подтверждено критиком). Явный reset не нужен; **зафиксировать инвариант** «defeat-proceed не сохраняет `Vital`/`StatusEffects`».
4. **Чтение:** `story_ui.rs:30` — заменить `active_flags(scen, idx)` на `CampaignState.flags.contains(...)`. **Load-путь:** при загрузке слота `CampaignProgress.flags` → `CampaignState.flags` (иначе эпилог после load слепнет — критик).

### Миграция (пересмотрено — `active_flags` уже удалён в срезе A)
`active_flags` снят в срезе A; `on_victory_flags` **сохранён** как путь безусловных victory-флагов.
Поэтому конвертация 3 читаемых ch1-флагов (`found_glassworks_token`, `kael_found`, `novice_saved`)
в `objectives` **не делается**: они пишутся безусловно при победе, бои обязательны к выигрышу →
перевод в `AllEnemiesDead`-objective поведенчески тождествен, только churn. `requires_flag` в ch1
не меняется. Два механизма документированы в `docs/content-guide.md` («Combat-outcome flags»);
`on_victory_flags` строго подмножество выразительности `objectives` → при будущей нужде в условном
victory-only флаге сворачивается в сахар над `objectives` (вариант C), не сейчас.

Опционально (косметика): 4 нечитаемых маркера (`bridge_routed`, `glassworks_cleared`,
`grove_anchor_broken`, `bell_silenced`) — дроп (0 `requires_flag`-консьюмеров).

### Инвариант (критик)
**Лодка (`boat_saved`) — только в `objectives`, НИКОГДА в `victory`.** Иначе `determine_outcome` даст немедленный Defeat при гибели лодки. Victory боя 1 = `AllEnemiesDead`; лодка — отдельный objective. Провалидировать.

### Тесты
- inline: парс `on_defeat`/`objectives`; `objective_met` (лодка жива→true, мертва→false; AllOf); `CampaignProgress.flags` round-trip + чтение старого слота без поля.
- full-app: proceed-defeat пишет objective + продвигает сценарий; retry-defeat (дефолт) → рестарт (регрессия); `requires_flag` читает флаг из прошлой сцены; load восстанавливает flags.

---

## Атом 2 — Story-choice → флаг (M, зависит от Атома 1)

- `SceneDef::Choice { prompt: Vec<DialogueLine>, options: Vec<ChoiceOption{label, set_flag}> }` (`src/content/scenarios.rs`); TOML `type="choice"`; `is_invisible()→false`.
- Ветвление веток — через **существующий** `DialogueLine.requires_flag` (атом 1 даёт persistent-чтение). Новый код только: запись флага + UI выбора.
- UI (`src/ui/story_ui.rs`): отдельные `ChoiceButton(usize)` (НЕ переиспользовать `StoryContinueButton` — критик), по клику `flags.insert(options[i].set_flag)` + `AdvanceScenario`.
- Расписание (`src/scenario/mod.rs`): `Choice => AppState::Story`; не скипать как invisible.
- Валидация (`validate_scenario`): options непусты, `set_flag` непуст.
- **Разграничение (критик):** `theo_fate`/`kasian_choice` — через story-choice; `marken_fate` — через **objective боя 3** (Атом 1), не choice.

### Тесты
- inline: парс `type="choice"`; пустые options → panic.
- full-app: выбор пишет флаг; ветка-реплика по `requires_flag` показывается/скрывается; пройденный выбор не повторяется.

---

## Атом 3 — Теги цели: `Unit.tags` + предикаты способностей/ауры (L)

Заказчик выбрал **гарантию**. Реализуем теги (не узкую «расу»): плоский `Set<Tag>` на
юните, предикаты `requires`/`excludes` у способностей и ауры, add/remove-tag в фазах.
Покрывает Б.2 (хил пастуха) и Б.4 (аура + обрыв фазы-3) одним примитивом + задел на будущее
(бестелесный/нежить).

### Словарь тегов (оси — документационные, технически один Set)
- **вид:** `humanoid` · `beast` · `symbiote` · `construct` · `aberration`
- **тело:** `corporeal` (дефолт) · `incorporeal`
- **жизнь:** `living` (дефолт) · `undead` · `inanimate`

Для ch3 авторим только: `symbiote`, `aberration`, `corporeal`, `incorporeal`, `living`.
Остальное — контент по мере появления механик.

### Модель данных (через стек; Tier-1 снял тест-construction ripple)
- `Unit.tags: BTreeSet<TagId>` (engine `state.rs`; `TagId`-newtype как `StatusId`). В `UnitWire`
  `#[serde(default)]`; в `Unit::new`/builder — пустой дефолт. **Мутабельно** (фазы) → входит в `post_state_hash`.
- ECS-компонент `Tags` (`components.rs`), вставка в спавне; `UnitTemplate.tags` + `Effect::Spawn`
  (саммоны); `UnitSnapshot.tags` (AI).
- `AbilityDef`: `+ requires_tags`, `+ excludes_tags` (`Set<TagId>`). `AuraDef`: `+ affects_tags`
  (requires-семантика). TOML-records `#[serde(default)]`.
- **SCHEMA-bump 46→47** (объединить с Атомом 4).

### Поведение
- `ActorView` + `ActionState::target_tags(id) -> &Set<TagId>` в **3 impl** (`EngineCheckState`
  `step.rs:67`, `SnapshotActionState` `action_state.rs:22`, Bevy-адаптер `legality_adapter.rs`).
- `check_legality` (рядом с target-type, `legality.rs:250`): цель имеет **все** `requires_tags`
  и **ни одного** `excludes_tags`, иначе `IllegalReason::WrongTargetTags`. **AI наследует
  бесплатно** (берёт цели через `check_legality`).
- Аура: общий хелпер `aura_targets(src, tgt, aura, content)` (team + `affects_tags`), вызвать из
  **ОБОИХ** call-site — `aura_effects_on` (`state.rs:1206`) И `aura_membership_set`
  (`state.rs:1263`); иначе drift бонусов и событий членства.
- **Фаза-3 мутация:** теги в `PhaseDef`→`PhaseEntry`/`PhaseTransition`→`check_phase_trigger`;
  `Effect::AddTag/RemoveTag { unit, tag }` деривируется из `EnterPhase` (`effect.rs:772`, по
  образцу `SetArmor`).

### Применение
- **Б.2 хил пастуха:** `requires_tags = {symbiote}`. Симбионты — `{symbiote, corporeal, living}`.
- **Б.4 аура накопителя:** `affects_tags = {symbiote}`. Контейнер ф.1-2 несёт `symbiote`; **ф.3
  снимает `symbiote`+`corporeal`, ставит `incorporeal`** → аура отваливается **гарантированно** +
  физ-атаки слабеют. (Если позже добавим «стрелы не бьют incorporeal» — тот же предикат `excludes`.)

### Тесты
- inline (`legality.rs`): `requires`/`excludes` позитив/негатив; пустые предикаты не влияют.
- inline (`state.rs` aura): `aura_targets` по `affects_tags`; после `RemoveTag(symbiote)` ф.3 —
  аура отваливается; `aura_membership_set` синхронен с `aura_effects_on`.
- inline (`effect.rs`): `AddTag`/`RemoveTag` меняют `Unit.tags`; `EnterPhase` их деривит.
- engine+bridge: `from_ecs` переносит `Tags`→`Unit.tags`; AI хилит только `symbiote`.
- serde-roundtrip: `Unit.tags`.

### Риск/оценка
Самый дорогой атом (стек + 3 `ActionState` + фаза-мутация). Tier-1 снял тест-construction
ripple; остаются прод `Unit::new` (~3) + in-crate inline-хелпер + `UnitWire`/`From` +
ActorView/ActionState×3. **Оценка: L.**

---

## Атом 4 — Heal-over-time (M, «Вливание жизни» Орена)

### Дизайн (зеркало `hp_percent_dot` — критик подтвердил прецедент)
- `combat_engine::content::StatusDef` (`content.rs:234`): `+ heal_per_tick: i32` (фикс, не на `ActiveStatus`, не INT-скейл). Читается движком из контента — как `hp_percent_dot`, который движок читает в `TickDot` (engine-cast хардкодит `dot_per_tick=0`, реальный DoT идёт через `hp_percent_dot` — критик подтвердил).
- bridge `StatusDef`/`StatusRecord` (`src/content/statuses.rs`): `+ #[serde(default)] heal_per_tick`.
- **`Effect::TickHeal { target, status }`** (`crates/combat_engine/src/effect.rs`) — **частичное** зеркало `TickDot` (`effect.rs:654`): читает `heal_per_tick` из `content.status_def`, восстанавливает HP, **clamp к max_hp**. НЕ зеркалит: death/`check_phase_trigger` (хил не убивает), rage (хил не даёт). Эмитит новый `Event::HotHealed` (`event.rs` + `effect_to_event`).
- Фанаут в `tick_actor_statuses` (`state.rs:976`) рядом с `TickDot`+`ExpireStatus` для статусов с `heal_per_tick>0` (по `content.status_def`).
- **SCHEMA bump** (новый Effect/Event) → `trace.rs:68` 46→47 + запись в историю.

### Взаимодействия (критик)
- С DoT-нейтрализацией `Effect::Heal` (`effect.rs:417`) — **конфликта нет**: нейтрализация ходит по `dot_per_tick>0`, у HoT-статуса `dot_per_tick=0`.
- DoT и HoT на одном юните: разные `applier` (враг vs Орен) → тикают в разные ходы. **Зафиксировать тестом.**

### Контент
Статус `vital_infusion` (`heal_per_tick=4`, без bonuses); ability «Вливание жизни» = `single_ally`, `cost_ap=1`, `costs=[mana 2]`, `effect=none`, `statuses=[{vital_infusion, on=target, duration=2}]`. За 2 хода Орена = +8.

### Тесты
- inline: `TickHeal` clamp к max_hp; `heal_per_tick=0`→no-op; нет death/phase-триггера от хила.
- inline (`state.rs`): фанаут на TurnStart аппликатора; +8 за 2 хода; на 3-й истёк.
- inline: DoT+HoT сосуществуют (разные applier, разные ходы).
- engine+bridge: каст → статус → +8 за 2 хода; `HotHealed` транслирован в combat-log.
- parity (`tests/toml_content_view_parity.rs`): `heal_per_tick` парсится.

---

## Атом 5 — Нарративные персистентные статусы (S→M, `injured` на Aldric) ✅ ГОТОВ

> **Пересмотрено и обобщено** (план прошёл plan-critic-reviewer: APPROVE WITH CHANGES).
> Вместо узкого `EncounterDef.start_statuses` (по одному энкаунтеру) — **status-fold по сюжету**:
> story-сцены добавляют/снимают персистентные статусы на партийцах, фолдятся по `scene_index`
> (как membership → **без save-стейта**) и ре-применяются на каждом спавне. Это поглощает кейс
> «однобоевая рана» (решение C = `add` перед боем + `remove` после) и даёт арку восстановления
> (рана → слабее → бафф), авторскую в сюжете.

- `SceneDef::Story { …, status_ops: Vec<PartyStatusOp> }`; `PartyStatusOp = Add|Remove{unit_name, status_id}` — **единый упорядоченный список** (детерминизм по порядку объявления, без межсписочной неоднозначности). TOML: `[[scenes.status_ops]]` с `op="add"|"remove"`.
- Фолд `active_party_statuses(scen, up_to)` (`scenarios.rs`, рядом с `active_party`): add (дедуп) / remove в порядке.
- Применение: `spawn_combatants` (обе ветки — template до `continue`), `StatusEffects` с `PERMANENT_DURATION` + `applier=Some(self)`. `from_ecs` **запекает** `−1 броня/−1 скорость` при конструировании `Unit` → корректно с 1-го раунда (engine-путь `apply_initial_statuses` сюда НЕ годится — фан-аут `RefreshAggregates` не идёт на раунде 1; критик подтвердил).
- Статус `injured`: `armor_bonus=-1, speed_bonus=-1` в `statuses.toml` (нового engine-кода нет). Стэкается с `defending` через сумму агрегатов.
- Валидация (`validate_scenario`): `unit_name` ∈ `active_party(scen, scene_idx+1)` (член присутствует ПОСЛЕ сцены — иначе тихий no-op → load-ошибка); `status_id` ∈ контент.

### Тесты (есть)
- inline (`scenarios.rs`): парс `status_ops`/`op`; фолд (накопление, add+remove в порядке, дедуп, remove несуществующего — no-op); неизвестный `op` → panic.
- inline (`resources.rs`): validate unknown unit / unknown status → panic; валидный проход (на `load_global_for_tests`).
- bridge (`bridge_projector.rs`): `from_ecs` запекает агрегаты пресид-статуса с 1-го раунда.
- AI (`snapshot_tests.rs`): персистентный статус виден в `UnitSnapshot`.

---

## Атом 6 — Verify AI-хила (S→M, аудит)

Инфра есть и идёт через legality (генератор гонит цели через `check_legality` —
критик подтвердил, `generator.rs:154,483`); скоринг `StepFactor::Heal`
(`src/combat/ai/scoring/factors/step/heal.rs`); критик `heal_without_rescue_value`.
1. Тест: пастух + раненый симбионт рядом → AI выбирает `heal`, не атаку; и атакует, когда лечить некого.
2. Если не хилит — тюнить: role-веса (`config/role.rs:308` `HEAL_IDX`), порог критика `heal_without_rescue_value`, масштаб `compute_offensive_for_step().heal`.
3. Пассив-фолбэк — только если скоринг не чинится (дороже).

---

## Порядок реализации

**Волна 1 (движковые примитивы):**
1. **Атом 1** — фундамент (`CampaignState.flags`, objectives, defeat-proceed wiring). Самый дорогой и тонкий (state-machine × autosave).
2. **Атом 2** — сразу после (пишет в `flags`).
3. **Атомы 3 + 4** — теги + HoT, **одним SCHEMA-bump** (46→47). Атом 3 после/параллельно Атому 1.
4. **Атом 5** ✅ — нарративные персистентные статусы (`status_ops` фолд; обобщил start-status).
5. **Атом 6** — verify (после контента боя 2; зависит от Атома 3 — фильтр в legality).

**Волна 2 (контент):** энкаунтеры боёв (теги юнитов; накопитель + аура `affects_tags`; фаза-3
мутация тегов босса), story-ветки, кит Орена, ребаланс под четверых.

**Минимум для играбельности:** атомы 1, 2, 3, 4, 5.

**Отложить:** Атом 6-fix (если verify зелёный); latched-цели; капитуляция Тэо по ходу боя;
расширение словаря тегов (`undead`/`beast`-сабтипы и пр.) — по мере появления механик.

## Сквозные риски
- **Defeat-proceed × state-machine × autosave-порядок** — самое тонкое (Атом 1); flags пишутся до autosave.
- **CampaignState ↔ CampaignProgress** — две копии истины; синхронизировать запись (обе) и load (CampaignProgress→CampaignState).
- **Лодка строго в objectives**, не в victory.
- SCHEMA-bump (Атомы 3+4, один инкремент) ломает старые трейсы — норма проекта.
- **Теги мутабельны** (фаза-3) → входят в `post_state_hash`; `aura_targets` обязан вызываться
  из обоих aura-call-site (`aura_effects_on` + `aura_membership_set`), иначе drift.
