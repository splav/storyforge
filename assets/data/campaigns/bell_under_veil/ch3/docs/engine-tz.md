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
| E | SCHEMA-bump | **Пересмотрено: два инкремента** (делаем атомы раздельно). Атом 4 (HoT): **46→47** ✅. Атом 3 (теги, `Unit.tags`): **47→48**. Committed-фикстур нет → каждый бамп самосогласован; «один инкремент» был оптимизацией под совместную посадку | честнее: каждое изменение формата = своя версия |

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

> **Поправка по факту реализации (Волна 2).** Строка «ветвление через
> существующий `DialogueLine.requires_flag`» оказалась неполной: line-level
> гейт прячет только **реплики**, поток сцен линеен и **не умеет пропускать
> бой**. Для Б.3 («переговоры → бой 3 пропущен») и 3-исходного эпилога
> добавлены два примитива (Атом 2.5).

## Атом 2.5 — Ветвление потока сцен (Волна 2, app-side, без SCHEMA) ✅ ГОТОВ

- **scene-level `requires_flag: Option<String>`** на всех вариантах `SceneDef` + аксессор `requires_flag()` (`c32cbcd`). `skip_invisible`→`should_skip`/`skip_skipped(flags)`; флаги протянуты в `advance_scenario_system` и `enter_scenario_at` (save-load симметрия); нет кампании ⇒ пустой набор. `enter_scenario_at` больше не паникует на all-gated-tail / non-campaign — graceful MainMenu.
  - **Контракт:** скип `Combat`-сцены отбрасывает её `on_victory_flags`/objective-флаги → ветка-замена сама выставляет нужные downstream-флаги (`docs/architecture.md`).
- **line-level `excludes_flag: Option<String>`** (`a3eff0b`) — негатив к `requires_flag` (`line_visible(l, flags)` на обоих фильтр-сайтах story_ui). «Else»-ветки без позитивного флага: лодка потеряна = `excludes boat_saved`; Маркен одержим = `requires theo_killed` + `excludes marken_killed`.
- Тесты: +12 (scene skip/play, branch, skipped-combat flag-loss, all-gated tail no-panic, save-load reentry, none-campaign) + 5 (excludes truth-table).

---

## Атом 3 — Теги цели: `Unit.tags` + предикаты (L–XL) ✅ ГОТОВ (A `f30ad92`, B `3a175b3`, C1 `a464dce`, C2 `b00638e`)

Плоский `Set<Tag>` на юните, **аддитивно** (движкового `race` нет — content `race` остаётся
display-only). Предикаты `requires`/`excludes` у способностей, `affects_tags` у ауры, смена тегов
в фазе. Покрывает Б.2 (хил пастуха) и Б.4 (аура + обрыв фазы-3). Прошёл planner→plan-critic:
**APPROVE WITH CHANGES** (Срез A — как есть; B/C — 4 поправки ниже).

### Словарь тегов (оси — документационные, технически один Set)
- **вид:** `humanoid` · `beast` · `symbiote` · `construct` · `aberration`
- **тело:** `corporeal` (дефолт) · `incorporeal` — **жизнь:** `living` (дефолт) · `undead` · `inanimate`

Для ch3 авторим только: `symbiote`, `aberration`, `corporeal`, `incorporeal`, `living`.

### Решения (locked)
- `TagId` через макрос `string_id!` (как `StatusId`); `Unit.tags: BTreeSet<TagId>`, в `UnitWire`
  `#[serde(default)]`; дефолт-поле + сеттер (как `passives`), **не 21-й арг** `Unit::new`. Мутабельно (фаза) → в `post_state_hash`.
- **AI без нового поля** — читает `u.tags` через `UnitView` Deref (иначе коллизия с `AiTags` bitflags).
- Трейт-предикат `has_tags(target, req, excl) -> bool` (owned, не `&Set`) в **3 impl**; в `check_legality`
  **только `SingleEnemy/SingleAlly`** (skip Ground/Myself) → `IllegalReason::WrongTargetTags`.
- Общий `aura_targets` из **обоих** сайтов (`aura_effects_on` ~1214 + `aura_membership_set` ~1271);
  пустой `affects_tags` ⇒ subset-true ⇒ существующие ауры не меняются.
- Семантика обрыва (критик подтвердил): накопитель = **источник**, Контейнер = **цель**; `affects_tags`
  фильтрует цель → Контейнер сбрасывает свой `symbiote` → аура отваливается корректно.

### Срезы (каждый — отдельный зелёный чекпойнт)
**A — данные + SCHEMA (M):** `TagId`; `Unit.tags` + `UnitWire #[serde(default)]` + 2 литерала
(`From<UnitWire>`, `Unit::new` body) + сеттер; `UnitTemplate.tags`; `Effect::Spawn` копирует
`template.tags`; ECS `Tags(BTreeSet<TagId>)` + bootstrap carry-in `Query<(Entity,&Tags)>` (отсутствие
компонента ⇒ пусто, back-compat); **SCHEMA 47→48** (обе константы + история). Без поведения.

**B — предикаты на статике (L):** `AbilityDef.{requires,excludes}_tags` (content-only, **не в wire**),
`AuraDef.affects_tags` (**в wire** — `Unit.auras`); `has_tags`×3 + legality-чек; `aura_targets`-рефактор;
TOML+bridge-зеркала (`EnemyDef.tags`, `AuraSource.affects_tags`, `ValidationTargetQ.tags: Option<&Tags>`).
Контент: симбионты `{symbiote,corporeal,living}`, пастух `requires={symbiote}`, накопитель `affects={symbiote}`.

**C — смена тегов в фазе (S→M):** `PhaseEntry.tags: Option<Set>` (**в wire** — `Unit.enemy_phases`) →
`check_phase_trigger` (state.rs ~649, мёртвый armor/speed-путь) → применяется в арме `EnterPhase`
(effect.rs ~831); aura-guard `step.rs:685` → именованный `effect_changes_aura_membership` (+`EnterPhase`
**only**; `Spawn` отложить — сместит summon-трейсы). Контейнер ф.3 `tags={aberration,incorporeal}`.

### Поправки критика к B/C (зафиксировать перед кодом)
1. **Двойная запись тегов фазы:** движковый in-arm + **ECS-зеркало** в `apply_phase_ecs_writes`
   (engine_bridge:538, по `Event::PhaseEntered`) + поле `tags` на **bridge** `encounters::PhaseDef`
   (отдельно от engine `PhaseEntry`) — иначе `BevyActions` читает stale ECS `Tags` после ф.3.
2. **SCHEMA v48 = ТРИ wire-добавления** (`Unit.tags` A, `AuraDef.affects_tags` B, `PhaseEntry.tags` C),
   прирастают по срезам; бамп один (47→48 в A), байты v48 разные по A→B→C (фикстур нет — ок).
3. **Развилка механизма (решить на C):** `Effect::SetTags{unit,tags}` (replace + само-описывающий
   трейс/seam для ECS-зеркала) ИЛИ ECS-write по `PhaseEntered` (тогда Effect не нужен). НЕ in-arm-«молча».
4. `Tags` ECS-компонент — нетривиальный скоуп (новый компонент + спавн 2 ветки + ValidationTargetQ).

### Тесты
- A: serde-roundtrip `Unit.tags` (старый трейс без ключа → пусто); determinism (BTreeSet порядок); SCHEMA-reject v47.
- B: legality `requires/excludes` ±, Ground/Myself игнор; `aura_targets` по `affects_tags`; пустой ⇒ существующие aura-тесты зелёные (регрессия); 3-backend parity.
- C: **headline** — ф.3 эмитит `AuraStatusLost` по симбионтам + `aura_effects_on(boss)`=default (обрыв «по агрегату и событиям»); `Unit.tags`=={aberration,incorporeal}; `effect_changes_aura_membership` enum-тест.

### Вне скоупа (техдолг `task_06f4cb24`)
Унификация bridge-фаз в движок; мёртвый armor/speed phase-override; `Spawn`-in-aura события.

---

## Атом 4 — Heal-over-time (M, «Вливание жизни» Орена) ✅ ГОТОВ (коммит `0e32201`)

> Реализовано по дизайну ниже. **SCHEMA 46→47** (HoT отдельно от тегов — два честных
> инкремента: теги пойдут 47→48; решение E пересмотрено, см. §0). 1454→1465 тестов.
> Попутно: brittle-пины `required: 46` в SCHEMA-version тестах развязаны (per-version —
> только `found` через `..`; один — `required == SCHEMA_VERSION`).

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

## Атом 6 — Verify AI-хила (S→M, аудит) ✅ ГОТОВ (`02dc047`)

**Вердикт: AI хилит вменяемо — без правок скоринга.** Подтверждено тестами:
- Распознавание хила — **по эффекту** (`effect=Heal` + `single_ally` → `AiTags::CAN_HEAL`
  / `AbilityTag::Rescue`, `world/snapshot.rs:926`, `world/tags/classify.rs:98`);
  `requires_tags` распознаванию НЕ мешает.
- Tag-предикат идёт через legality в генератор плана (`has_tags`→`WrongTargetTags`;
  `rank_targets`/`check_legality` над `SnapshotActionState`, `generator.rs:476`) →
  не-symbiote союзник отсеивается до скоринга; overheal-гейт (>90% HP) глушит хил по полным.
- (Примечание: «repair»/`repair_bonus` в этом коде = plan-continuation, НЕ хил — ложный след.)

Тесты (`src/combat/ai/orchestration/mod.rs`): legality-гейт (Ok на symbiote /
`WrongTargetTags` на обычном союзнике); полный `pick_action` → хилит раненого
симбионта, не обычного союзника; негатив-контроль → атакует при полном HP союзника.

---

## Порядок реализации

**Волна 1 (движковые примитивы):**
1. **Атом 1** — фундамент (`CampaignState.flags`, objectives, defeat-proceed wiring). Самый дорогой и тонкий (state-machine × autosave).
2. **Атом 2** — сразу после (пишет в `flags`).
3. **Атом 4** ✅ — HoT (SCHEMA 46→47). **Атом 3** ✅ — теги (SCHEMA 47→48), A/B/C1/C2.
4. **Атом 5** ✅ — нарративные персистентные статусы (`status_ops` фолд; обобщил start-status).
5. **Атом 6** — verify (после контента боя 2; зависит от Атома 3 — фильтр в legality).

**Волна 2 (контент + 2 примитива потока):** ✅ **посажено** (Phase 0–4):
- Phase 0 (`146ae65`) — abilities `shepherd_heal`/`living_carapace`/`numbness` + statuses.
- Phase 1 (`c45642b`) — 14 шаблонов ch3 + оружие `lancet` + раса `object`.
- Phase 2 (`ba4bbfd`) — 4 энкаунтера (теги юнитов; накопитель + аура `affects_tags`;
  фаза-3 мутация тегов босса; objectives boat_saved/theo_killed/marken_killed).
- Атом 2.5 (`c32cbcd`+`a3eff0b`) — scene `requires_flag` + line `excludes_flag`.
- Phase 3+4 (`04540c0`) — `scenario.toml` (21 сцена, story-ветки, кит Орена,
  `injured` status_op, оба выбора) + регистрация ch3 в `campaign.toml`.
- Весь контент-DB **грузится и валидируется** с ch3 (1514 тестов зелёные).
- Атом 6 (`02dc047`) — verify AI-хила: ✅ AI хилит вменяемо (3 теста, без правок скоринга).
- ⏳ **Остаётся:** ребаланс под четверых + плейтест веток (Phase 5, runtime).
  Сделано: HP Контейнера **30→60** (бой 4 был ~вдвое короче задуманного);
  плейтест веток + дальнейший тюнинг — ещё впереди.
- 💤 **Headless balance-санити — отложено.** Faithful-сим боя 4 обязан гонять
  фазы босса + аура-рекомпьют, а они живут в бридже (`apply_phase_ecs_writes`,
  `phase_overrides_system`), не в движке → сейчас сим требует поднятия Bevy-app
  (heavy) + AI-vs-AI (AI≠человек, не оракул сложности). Решение: ждать миграции
  фаз/ауры в движковый каскад (bridge.md §10 tech-debt, `task_06f4cb24`); после
  неё сим строится на чистом `combat_engine` (дёшево, детерминированно, без
  бриджа). Оценка вариантов (static-ассерты A / AI-vs-AI B / гибрид) — в истории
  обсуждения; при разблокировке начинать с A.

**Минимум для играбельности:** атомы 1, 2, 3, 4, 5.

**Отложить:** Атом 6-fix (если verify зелёный); latched-цели; капитуляция Тэо по ходу боя;
расширение словаря тегов (`undead`/`beast`-сабтипы и пр.) — по мере появления механик.

## Сквозные риски
- **Defeat-proceed × state-machine × autosave-порядок** — самое тонкое (Атом 1); flags пишутся до autosave.
- **CampaignState ↔ CampaignProgress** — две копии истины; синхронизировать запись (обе) и load (CampaignProgress→CampaignState).
- **Лодка строго в objectives**, не в victory.
- SCHEMA-bump: Атом 4 → 47 ✅, Атом 3 (теги) → 48. `Unit.tags` в `post_state_hash` сломает canary-хеши (Атом 3); HoT хеши не трогал — норма проекта.
- **Теги мутабельны** (фаза-3) → входят в `post_state_hash`; `aura_targets` обязан вызываться
  из обоих aura-call-site (`aura_effects_on` + `aura_membership_set`), иначе drift.
