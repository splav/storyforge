# Шаг 0: Sim↔Real parity + telemetry mining

## Зачем

Два контракта, которые план опирает молчаливо, должны стать явно проверяемыми до того, как поверх них строится что-то ещё.

**Sim faithfulness.** В `sim.rs` честно написано, что часть производных полей «stale». Это признание возможного drift'а модели мира. Шаги 4 (outcome vector) и 5 (terminal eval) строят оценку будущего на результатах sim — если sim расходится с real resolution, весь value function считает не то. Без property-тестов равенства это риск, который не виден в логах.

**Telemetry mining.** Шаг 3 (need layer) проектируется под **реальные патологии** поведения AI, а не под гипотезу о них. JSONL уже пишется со всеми нужными полями (почти — см. 0.3). Прогон анализа по существующим логам даёт таблицу реальных проблем — на неё и опирается дизайн need signals.

## Что делаем

### 0.1. Property-battery parity — на уровне `AbilityOutcome` фактов

**Уточнение ожиданий.** Production resolver (`combat/resolution.rs`) и sim (`combat/ai/planning/sim.rs`) **оба** вызывают общий `compute_ability_outcome` (`effects_outcome.rs`) — dice-математика и primary-outcome совпадают by construction. Drift живёт **после** outcome: real применяет эффекты через Bevy messaging (`ApplyDamage` / `ApplyHeal` / `ApplyStatus` / `SpawnUnit`) с отложенным системным прогоном; sim мутирует `UnitSnapshot` сразу. Plus: death-cleanup ordering, rage-gain timing, DoT tick ordering, reaction consumption.

Поэтому честная parity-батарея разрезается на два уровня:

**0.1a. Outcome-facts parity (дёшево, обязательно в волне 1).**

Для каждой ability прогнать: `compute_ability_outcome(ctx, EV)` vs `sim::apply_step(ctx, EV)` — сверка **набора фактов**, которые sim декларирует: damage deltas, applied statuses, resource deltas, spawn instructions. Это не end-to-end, это «соглашается ли sim с shared core о том, ЧТО произошло». Ловит 90% реальных drift-багов за 2–3 дня.

**Контекст для исполнителя:** `sim::apply_cast` **уже** вызывает `compute_ability_outcome` под `ExpectedValue` (`sim.rs:161–168`), т.е. dice-математика by construction совпадает. Реальный предмет проверки — **интерпретация** возвращённого `AbilityOutcome` в sim: правильно ли `apply_primary` / `apply_statuses` / `pay_costs` переводят outcome в state deltas.

**Scope задачи для 0.1a:**

Создать `src/combat/ai/planning/parity_tests.rs` (новый модуль, `#[cfg(test)]`) с двумя слоями тестов:

*Слой 1 — focused invariants per `OutcomePrimary` variant.*
Для каждой ветки `OutcomePrimary::{Damage, Heal, GrantMovement, RestoreResources, Summon, None}` — 1–2 целевых теста, которые:
1. Строят канонический fixture (actor + релевантные targets) через `test_helpers::UnitBuilder`.
2. Вызывают `SimState::apply_step` для cast-ability с нужным effect.
3. Ассертят, что конкретное поле `AbilityOutcome` (например, `Damage.raw`) отражено в state delta ровно так, как декларирует shared core: `final_damage_f32(raw, armor+bonus, vuln, pierces)` для Damage, `min(missing_hp, amount - dot_consumed)` для Heal, `+distance` к movement_points для GrantMovement, и т.д.

*Слой 2 — property sweep по всем abilities из контента.*
Единый тест, который:
1. Загружает `ContentView::load_global_for_tests()`.
2. Итерирует по `content.abilities` (~18 штук сейчас — см. `assets/data/abilities.toml`).
3. Для каждой ability:
   - Строит минимальный snapshot с actor + target(ами) в соответствии с `target_type` и достаточными ресурсами для `can_afford`.
   - Вызывает `compute_ability_outcome` напрямую — получает ожидаемый `AbilityOutcome`.
   - Параллельно вызывает `SimState::apply_step` на копии того же snapshot.
   - **Сверяет инварианты:**
     - HP delta каждого `affected` таргета соответствует `final_damage_f32(outcome.primary.raw, …)` (для Damage) или `min(missing, effective_heal)` (для Heal).
     - Каждый `outcome.statuses[i]` присутствует в `target.statuses` post-cast с правильным `rounds_remaining` и `dot_per_tick = sd.dot_dice.expected().round()`.
     - `def.costs` вычтены из соответствующих ресурсов; AP cost уменьшён на `def.cost_ap`.
     - Для `skips_turn` статусов target получил `AiTags::IS_STUNNED`.
     - Killed targets (hp → 0) появились в `StepOutcome.killed`.

*Whitelist задокументированных divergences.*
Оформить как `const KNOWN_DIVERGENCES: &[(AbilityFilter, &str)]` с комментарием-объяснением для каждого случая:
- `Summon`: sim не спавнит юнит (out-of-scope по `apply_primary`); проверка только в том, что outcome declares Summon и sim не изменяет state кроме costs.
- `ManaOverload` / crit-fail: sim жёстко передаёт `crit_failed=false`; весь crit-path не проверяется (задокументировано в `sim.rs:157–160`).
- Любые новые расхождения, обнаруженные во время написания — добавляются в whitelist с комментарием «почему» и ссылкой на issue/drift номер.

*Что НЕ входит в scope 0.1a:*
- End-to-end Bevy App parity (это 0.1b, после шага 12).
- Исправление обнаруженных drift'ов (только документирование в whitelist).
- Изменения в production-коде кроме тестов.
- Perf-оптимизация test loop (~18 abilities × 2 слоя тестов = тривиально быстро).

*Критерии готовности 0.1a:*
- `cargo test --lib combat::ai::planning::parity_tests` зелёный.
- Whitelist коммитится вместе с тестами; каждая запись имеет комментарий причины.
- Добавить в docstring нового модуля: «property-battery for shared-core ↔ sim parity; extends automatically as new abilities land in content».
- Один commit, message mentions шаг 0.1a.

**0.1b. End-to-end parity (opt-in, после шага 12).**

Полный Bevy-прогон мини-App'а с `ValidatedAction` → захват всех компонент post-resolution → сравнение с `SimState` post-resolution. Scaffolding уже есть в `tests/effects.rs` / `tests/pipeline.rs` (150–200 строк на сценарий). Property-раннер поверх всех abilities — неделя. **Это не блокер волны 1**, делается когда mid-plan reflow (шаг 12) делает status-timing и AoO-ordering load-bearing для планирования.

**Whitelist.** Задокументированные осознанные расхождения (crit-fail в sim отключён; status-application timing в 0.1a игнорируется; в 0.1b учитывается) — перечислить в тесте с комментариями.

### 0.2. Инвариант в документации

Добавить пятый инвариант в «что не ломаем»:

> **Sim faithfulness.** `SimState::apply_step` под `ExpectedValue` даёт тот же `AbilityOutcome` fact-set, что production `compute_ability_outcome`. Для фактов, которые sim применяет немедленно (status/DoT timing), end-to-end parity гарантируется отдельной батареей (0.1b). Новый эффект добавляется сперва в shared effects core, затем параллельно в sim — property-тест гарантирует равенство. Задокументированные исключения — whitelist.

### 0.3. Mining существующих логов

Mining метрик делится на три класса по готовности JSONL:

**Класс A — mineable из текущего лога напрямую:**

- **Adaptation reason frequency**: `PlanLogEntry.adaptation_reason` (`log.rs:227`) — как часто стреляют `ExpectedSelfLethal`, `ProtectSelfNoDefensive`, `ProtectSelfFutile`.
- **Panic-override frequency**: `IntentBlock.reason.code == "panic_override"` — как часто hard override vs сколько раз ProtectSelf выбрался бы через обычный scoring (`base_score` vs `score` ранжирование).
- **Plan depth utilization**: `plan.steps.len()` в `PlanLogEntry.steps` — насколько часто выбирается план глубины 2 vs 3.
- **Continuation invalidation reasons**: `replan_reason` в существующих `plan_divergence` entries (`log.rs:540`). Какие `mismatch()` codes срабатывают постоянно, какие — никогда. Прямой вход для step 6.

**Класс B — требует реконструкции последовательности (день-два работы):**

- **Intent transition stability**: `(prev_intent, current_intent)` за ход. В JSONL `prev_intent` не хранится per-entry; скрипт группирует по `actor_id` в пределах одного combat_log_path и упорядочивает по `plan_id` / `timestamp_ms`. Выход: матрица переходов.

**Класс C — требует предварительной правки `log.rs`:**

- **Sanity hit histogram**: какие правила sanity срабатывают чаще 0.5%, какие — шум. В текущем логе хранится только финальный `score` / `base_score`, **per-rule breakdown отсутствует**. Либо добавить `sanity_breakdown: Vec<SanityHit>` в `PlanLogEntry` перед mining (полдня работы), **либо** перенести эту метрику в step 10 (там benchmark-driven decomposition всё равно понадобится). Выбор: добавить поле в 0.3, это дёшево и окупается.

### 0.4. Таблица патологий как вход для шага 3

Из mining'а собирается артефакт `docs/ai_need_signals.md` с форматом «патология → частота → гипотеза → need signal». Именно этот документ потом коммитится **вместе** с реализацией step 3 как входной спецификацией. Не висящая в воздухе заметка — load-bearing артефакт следующего шага.

Пример:
- «Переключение FocusTarget A → FocusTarget B между ходами без смерти A» → 8% тиков → need `continue_commitment` с высоким весом.
- «Panic_override стреляет при hp=25% но без урона по актору между ходами» → 3% тиков → порог `survival_hp_threshold` слишком высокий, либо нужен `recent_damage_taken` signal.

## Что это даёт и что стоит

| Подзадача | Стоимость | Permanent? | Статус |
|-----------|-----------|------------|--------|
| 0.1a parity (outcome-facts) | 2–3 дня | да, в CI | pending (формулировка закоммичена) |
| 0.1b parity (end-to-end) | 1 неделя | да, в CI — но **делается после шага 12** | deferred |
| 0.3 A+B mining | 2 дня | нет, повторяется перед каждой волной | pending |
| 0.3 C (sanity_breakdown патч log.rs) | 0.5 дня | да, поле остаётся | **done** (commit `00b2feb`, schema v15→v16) |
| 0.4 артефакт need_signals.md | 0.5 дня | коммитится с step 3 | **done** (`docs/ai_need_signals.md`) |

Суммарно для волны 1: **4–5 дней** до старта step 1.

---

# Шаг 1: Assertion overlay поверх существующего replay

## Зачем

Почти всё для serialization-first регрессии **уже есть**:

- `BattleSnapshot` и всё внутри — `Serialize/Deserialize` с `#[serde(default)]`.
- JSONL пишет snapshot, intent, план-пул, raw factors, adaptation, trade breakdown, schema_version.
- `replay_ai_log` десериализует, пересчитывает influence maps детерминированно, прогоняет production `finalize_scores` + `sanity_adjust_plans` + `pick_best_plan`, умеет diff.
- `IntentReason` — structured enum с `code()`.

Недостаёт: assertion overlay, runner с pass/fail, pre-merge CI, и — для **части** сценариев — расширение лога.

## Что делаем

### 1.1. Расширение лога (pre-step, не откладываемый) — **done**

`replay_ai_log` сегодня хардкодит `DifficultyProfile::normal()`, fresh `Reservations::default()`, fresh `AiMemory::default()`. Для self-contained сценариев этого достаточно. Для continuation / team-coord сценариев — нет.

Добавить в `AiLogEntry` перед решением:

- `difficulty: DifficultyProfileSnapshot` — название тира + override-секция, чтобы replay восстановил тот же `DifficultyProfile`, с которым работала игра.
- `ai_memory: Option<AiMemorySnapshot>` — `last_intent`, `last_target`, `turns_committed`, `last_plan` (без `sim_snapshots`). Актор-скоуп.
- `reservations: ReservationsSnapshot` — на момент перед `pick_action`. Не после.

Это 50–80 строк в `log.rs` + bump `SCHEMA_VERSION`. Полдня. **Без этого patch'а** сценарии из 1.4 — класс «Plan freeze» и «Team coordination» — строят тесты на дефолтах, которые не воспроизводят реальную ситуацию.

**Статус:** реализовано. Schema v16 → v17. Три snapshot-типа в `src/combat/ai/log.rs`, `Reservations::to_snapshot`/`from_snapshot` в `src/combat/ai/reservations.rs`, `LogEntry` в `replay_ai_log.rs` — с `#[serde(default)]` для backward compat со старыми v16 логами. Хардкоды `DifficultyProfile::normal()` / `Reservations::default()` в replay намеренно оставлены до шага 1.3.

### 1.2. Assertion overlay

Рядом с каждым JSONL-снапшотом — `*.expected.toml`. Описывает ожидания, а не состояние.

Два типа assertion'ов смешиваются в одном наборе:

- **Точные** — когда решение единственно верное: `decision_kind`, `cast_ability`, `cast_target`, `end_position`.
- **Категориальные** — когда несколько равноценных ответов: `intent ∈ {...}`, `primary_effect ∈ {Damage, Kill}`, `end_position ∈ {...}`, `not_target = ...`.

Правило: assertion'ы — только на наблюдаемое поведение, не на внутренние скоры. Иначе тесты падают при любой безобидной доводке весов.

### 1.3. `--assert` флаг в `replay_ai_log`

Режим: читает overlay, прогоняет production `pick_action` на десериализованном снапшоте с восстановленным `AiMemory` / `Reservations` / `DifficultyProfile` из 1.1, сравнивает результат. Non-zero exit code при несовпадении, структурированный diff ожидаемого vs полученного.

### 1.4. `cargo test --test ai_scenarios` — **done**

**Статус:** реализовано в два коммита.

**1.4a — library extraction** (commit `b018f9c`). Пайплайн пересборки решения из JSONL-снапшота (`finalize_scores` → `sanity_adjust_plans` → `apply_protect_self_mask` → `pick_best_plan` → `build_actual_decision`) вместе с serde-миррор типами (`LogEntry`, `PlanLog`, `IntentBlock`, `LoggedTradeBlock`, `LoggedEvaluationMode`, `LoggedAdaptationReason`) переехал из `src/bin/replay_ai_log.rs` в новый `src/combat/ai/replay.rs`. Публичный API: `assert_log_file(jsonl, overlay, &content, &inf_cfg) -> Result<AssertOutcome, AssertError>`. Бинарь стал тонкой I/O-оболочкой; поведение PASS/FAIL/exit-codes сохранено bit-for-bit (прошли 9 тестов `tests/replay_assert.rs` без правок).

**1.4b — harness.** `tests/ai_scenarios.rs` обходит `tests/ai_scenarios/snapshots/`, по парам `<name>.jsonl` + `<name>.jsonl.expected.toml` вызывает `assert_log_file` напрямую (без subprocess). Контент и `InfluenceConfig` загружаются один раз на весь batch. При падении печатается путь к JSONL, overlay, фактическое решение и per-variant failure diff. README в `tests/ai_scenarios/README.md` описывает формат и правила.

Стартовый сценарий: `focus_target_melee_basic.jsonl` (schema v15, отрабатывает через pre-v17 fallback — см. `ReservationsSnapshot`/`DifficultyProfileSnapshot` комментарии в `replay.rs`). Scope 1.4 — harness + 1 smoke scenario; наполнение корпусом на 10–15 снапшотов — отдельная задача 1.5.

Время harness'а: <100ms для 1 сценария, целевой budget `<5s` для 10–15 сценариев достижим (один процесс, без ребилда).

### 1.5. Первая партия — **done (первый batch)**

**Статус:** закоммичено (`b2b6a2c`). Разметка `snapshots/<group>/log.jsonl + N <case>.expected.toml`; имя кейса `p<plan_id>_<desc>.expected.toml` — plan_id сразу виден. Harness рекурсивен по подпапкам; в каждой требует ровно один `*.jsonl` + ≥1 overlay, иначе panic с понятным сообщением. Имя кейса (`<group>/<stem>`) печатается при fail.

Сейчас **9 кейсов** (1 smoke + 8 из 4 свежих v17 playtest'ов от 2026-04-24). Разбивка:

| Группа | Кейс | Категория |
|---|---|---|
| `focus_target_melee_basic` | `p000_basic_melee` | offensive (legacy smoke) |
| `road_bridge` | `p000_padalshchik_basic_melee` | offensive baseline |
| `twisted_grove` | `p010_dvoynik_finisher_1hp` | offensive — finisher на 1HP цель, GATE |
| `twisted_grove` | `p013_iskazhenny_last_stand_trade` | protect-self — LS+SV, Cast "забрать с собой" |
| `twisted_grove` | `p019_dvoynik_monotone_focus` | continuation — r3 focus-fire на ту же цель |
| `twisted_grove` | `p036_iskazhenny_last_stand_no_options` | protect-self — LS+AP=0 → EndTurn |
| `glassworks` | `p001_kontrabandist_backstab` | offensive positional |
| `bell_crypt` | `p003_meron_support_heal` | support — heal раненого ally |
| `bell_crypt` | `p008_bell_bound_retreat_low_hp` | protect-self — hp=7%, retreat |

**Категории плана vs реальность:**

- **Offensive correctness** — 3 кейса ✓
- **Protect-self correctness** — 3 кейса ✓
- **Plan freeze / continuation** — 1 кейс (monotone_focus). Недостаёт: replan при смерти цели, continuation при actor_hp_drop (есть DIV event в bell_crypt, но формат ассерта пока не прорабатывался).
- **Team coordination** — 1 кейс (support heal). Недостаёт: no overkill через reservations, focus fire. Нужен формат для reservations-зависимых ассертов (`not_target` на уже занятого — формально поддерживается, но требуется проверить, что reservations корректно пробрасываются в `ScoringCtx` для v17 логов и выбрать evident entry).

**Правила:**
- Assertion'ы только на наблюдаемое поведение (`decision_kind`, `cast_ability`, `cast_target`, `primary_effect`, `intent_kind`, `end_position`). Не на внутренние скоры — упадут при любой доводке весов.
- Снапшоты только из реальных плейтестов, не синтетика.
- Negative-path verified: замена overlay на противоречащий кейс падает с читаемым diff'ом.

**Доборы на будущее (не блокируют 2a):**
- +2 continuation-кейса (replan on target death, hp_drop re-plan).
- +2 team-coord кейса через reservations (no overkill, focus fire при разрешённом pile-on).

### 1.6. CI — **skipped**

Pre-merge CI-workflow'а сейчас нет. `cargo test --test ai_scenarios` (~100ms) локально закрывает потребность. Когда/если CI заведём, harness уже готов к включению без изменений.

### 1.7. Рост покрытия через баги

Правило: каждый найденный bug сначала становится снапшотом с overlay'ем, потом чинится. Снапшот снимается из существующего JSONL-лога боя, overlay пишется за минуту. **Активный механизм** теперь, когда harness живой.

## Что не делаем в Волне 1 (отложено)

- **Hotkey сохранения в игре** — снимки уже есть через JSONL. Хоткей — по потребности.
- **Auto-capture при падении CI** — удобно, не блокирует.
- **Golden JSONL diff + `UPDATE_GOLDENS=1`** — вторая линия защиты, не нужна сразу. *Исключение:* для шага 2a (см. ниже) golden replay становится обязательным gate.
- **Визуализатор diff** — отдельный инструмент, по необходимости.
- **Integration с save/load системой** — отдельная задача игры. Формат общий (уже есть), интеграцию делаем, когда save/load будет разрабатываться.

## Чего не делать

- Не писать синтетические сценарии в TOML до переработки continuation/repair (шаг 6). Снимки из реальных боёв покрывают лучше.
- Не плодить assertion'ы на внутренние скоры — упадут при любой доводке весов.
- Не делать снапшоты взаимозависимыми.
- Не покрывать «все возможные бои».

---

# Шаг 2: `AiTuning` (фаза 2a) — миграция констант в данные

## Зачем

`DifficultyProfile` уже централизует большинство тюнинг-констант с derived-методами. `InfluenceConfig` уже Resource. Но магических чисел всё ещё много в `factors/*`, `sanity.rs`, `intent.rs`, `scarcity`. Каждое — grep-only при балансировке.

Шаги 2b (response curves) и 2c (difficulty layers, live-reload, визуализатор) — отдельные фазы, по потребности, не в волне 1.

## Классификация того, что мигрируем

Grep «`const`» — оптимистичная формулировка, которая покрывает ~30% реальных точек тюнинга. Честная классификация на три класса:

**Класс А — чистые scalar const'ы (дёшево, механическая миграция).**

Примеры: `MILD_PENALTY`, `STICKINESS_BONUS`, `MAX_COMMITTED_TURNS` (`intent.rs`), `SURVIVAL_FLOOR`, `LOW_HP_FACTOR`, `AOO_PENALTY_K`, `AOO_RISK_FLOOR`, `SELF_SURVIVAL_EPSILON` (`sanity.rs`), λ-values из `InfluenceConfig::default`. Переезжают в `tuning.thresholds` как плоские числа. **1 день.**

**Класс B — role-зависимые таблицы (средняя работа, требует сохранить API).**

Примеры: `AxisProfile::factor_weights()` в `role.rs` — функция от 5 role-осей; `evaluate_position` веса в `position_eval.rs`. Это не «const → tuning», это «таблица коэффициентов (axis, factor) → tuning.tables» + функция читает таблицу по индексу. Контракт `factor_weights()` сохраняется, внутрь завозится Resource. **2–3 дня.**

**Класс C — derived-методы `DifficultyProfile` (средняя работа, рефакторинг Resource).**

Примеры: `survival_hp_threshold()`, `reposition_min_improvement()`, `awareness_danger_threshold()` — lerp'ы между константами с параметром `survival_instinct` / `awareness`. Сейчас это методы; после 2a — параметры lerp'а лежат в TOML, методы читают оттуда. API не меняется, реализация — читает `tuning.difficulty.awareness_danger_curve.{lo, hi}` вместо двух хардкодных чисел. **2 дня.**

Итого 2a = 5–7 дней. Не «дневная миграция».

## Что делаем в 2a

Пошаговый план, **коммит на шаг**, между коммитами — прогон golden-replay (шаг 2.0). Порядок выбран так, что каждый шаг либо расширяет инфраструктуру, либо переносит одно конкретное семейство констант.

### 2.0. Golden-replay tool — **обязательный первый шаг** ✓ DONE

**Зачем.** Без него вся миграция «вслепую»: опечатка `0.15 → 0.015` в TOML не ловится ни scenarios harness'ом (9 кейсов — слишком маленькое зеркало формул), ни unit-тестами (они не покрывают end-to-end scoring).

**Реализация** — два флага в `src/bin/replay_ai_log.rs`:

- `--capture-golden <out.jsonl>` — пройти по всем entries переданных JSONL-логов, прогнать production pipeline (ту же цепочку, что `assert_log_file`), записать в `out.jsonl` по строке на entry: `{log_path, plan_id, actor_id, decision_kind, cast_ability, cast_target, end_position}`. Никакого filter'а — весь decision stream.
- `--compare-golden <baseline.jsonl>` — прогнать сейчас те же логи, сравнить с baseline. Exit 1 при любом расхождении, stderr: `case N diverged: field = <actual> vs <baseline>` per-entry. Итого `diverged / total`.

**Корпус golden** — план предполагал все `logs/*.jsonl` (~50 файлов); базовая линия собрана на 4 v17-логах
(`logs/20260424T121330_*.jsonl`, `*121359_*.jsonl`, `*121431_*.jsonl`, `*121649_*.jsonl`, 131 запись).
Pre-v17 fallback path на этапе 2a заморожен отдельно (расширение корпуса — backlog, не блокер).

**Хранение baseline.** `logs/golden_pre_2a.jsonl` (gitignore'ить не надо — это защитный snapshot). Удалится после завершения 2a.

**Эстимейт:** 0.5 дня (большая часть кода уже есть в `assert_log_file` — нужна лишь stream-обёртка).

### 2.1. Схема `AiTuning` как единый Resource + scaffolding ✓ DONE

Создать:

- `assets/data/ai_tuning.toml` — **пустой на этом шаге** (только секции-заголовки).
- `src/combat/ai/tuning.rs` — `AiTuning { thresholds, tables, difficulty }` с дефолтами из `default()`.
- Resource-регистрация в `main.rs`.
- Loader: `ContentView::load_layered` расширяется чтением `ai_tuning.toml` если файл есть.

Пока ни одно место в коде не переехало — `AiTuning` никем не читается. **Golden-replay должен показать 0 diff'ов** (инфраструктура добавлена, формулы не тронуты).

```
assets/data/ai_tuning.toml
├── thresholds       # класс А: плоские scalar'ы
├── tables           # класс B: role-фактор матрица, pos_eval weights, scarcity, trade weights
└── difficulty       # класс C: параметры lerp'ов DifficultyProfile
```

`InfluenceConfig` остаётся отдельным Resource **пока что**. Слияние в `AiTuning.influence` — опциональная поздняя правка, не блокирует 2a.

**Коммит:** `a099740`. **Golden-replay:** 0 / 131 diff.

### 2.2. Класс А — `sanity.rs` thresholds → TOML ✓ DONE

Мигрировали: `SURVIVAL_FLOOR`, `LOW_HP_FACTOR`, `AOO_PENALTY_K`, `AOO_RISK_FLOOR`, `SELF_SURVIVAL_EPSILON` → `AiTuning.thresholds.*`.
`AiWorld` расширен полем `tuning: &'a AiTuning`. `plan_is_defensive` и `apply_protect_self_mask` принимают `epsilon: f32` параметром. `replay_ai_log.rs` мигрирован аналогично.

**Коммит:** `7d9bbaa`. **Golden-replay:** 0 / 131 diff.

### 2.3. Класс А — `intent.rs` thresholds → TOML ✓ DONE

Мигрированы: `MILD_PENALTY`, `STICKINESS_BONUS`, `TARGET_STICKINESS_BONUS`, `MAX_COMMITTED_TURNS` → `AiTuning.thresholds.*`.
`select_intent` расширена параметром `tuning: &AiTuning`. `intent_score` читает `mild_penalty` через `step_ctx.world.tuning.thresholds`.

**Коммит:** `a31b696`. **Golden-replay:** 0 / 131 diff.

### 2.4. Класс B — `role::AxisProfile::factor_weights()` → таблица ✓ DONE

Матрица `AXIS_FACTOR_WEIGHTS` (5 axes × 10 factors) удалена из `role.rs` и переехала в `AiTuning.tables.axis_factor_weights` как 2D-массив. `AxisProfile::factor_weights` расширена параметром `tuning: &AiTuning` (как в 2.2 / 2.3 — чистая миграция к ровно той же формуле). Callers обновлены: `finalize_scores` (`scorer.rs:204`), тесты `role.rs`, `_touch_axis` в `replay_ai_log.rs`. TOML-секция `[tables]` в `assets/data/ai_tuning.toml` содержит матрицу с комментарием-шапкой колонок.

**Коммит:** `5d45398`. **Golden-replay:** 0 / 131 diff.

### 2.5. Класс B — `position_eval.rs` weights → таблица ✓ DONE

Матрица `AXIS_POSITION_WEIGHTS` (5 axes × 3 influence maps) удалена из `role.rs` и переехала в `AiTuning.tables.axis_position_weights` как 2D-массив. `AxisProfile::position_weights` расширена параметром `tuning: &AiTuning`. `evaluate_position` расширена параметром `tuning: &AiTuning` и пробросила его в `position_weights`. Callers обновлены: `intent.rs` (3 точки), `sanity.rs` (2 точки), `future_value.rs` (через `position_component`), `debug.rs` (`tile_influence_at`, `build_debug_snapshot`, `build_fallback_debug`, `decision_debug`). Тесты `position_eval.rs` передают `&AiTuning::default()`.

**Коммит:** `88ff6b8`. **Golden-replay:** 0 / 131 diff.

### 2.6. Класс C — `DifficultyProfile` lerps → `tuning.difficulty` ✓ DONE

Методы `survival_hp_threshold()`, `reposition_min_improvement()`, `awareness_danger_threshold()` получили параметр `tuning: &AiTuning` и читают `tuning.difficulty.*_curve.{lo, hi}` вместо хардкодов. Добавлен тип `LerpCurve { lo, hi }` с `eval(t)` в `tuning.rs`. Секция `[difficulty]` в `ai_tuning.toml` заполнена тремя кривыми. Callers обновлены: `intent.rs` (3 точки: строки 391–392 в `select_intent`, строка 893 в `intent_score`), `ranking.rs` (строка 104), `replay_ai_log.rs` (строки 1611, 1654, 1822). Sanity-тест `lerp_curve_migration_values_match_original_hardcodes` проверяет 4 тира × 3 метода = 12 значений bit-for-bit.

**Коммит:** `18b62fd`. **Golden-replay:** 0 / 131 diff.

### 2.7. UnitQuirks — пустая override-инфраструктура **DONE**

Реализовано:
- `AiTuningOverride` + `ThresholdsOverride` в `tuning.rs` — все поля `Option<T>`, partial merge.
- `AiTuning::apply_override(&self, ov: &AiTuningOverride) -> AiTuning` — явный per-field merge, no derive-magic.
- Поле `ai_tuning_override: Option<AiTuningOverride>` в `TemplateRecord` и `UnitTemplateDef` (`unit_templates.rs`).
- Поле `ai_tuning_override: Option<AiTuningOverride>` в `UnitSnapshot` (`snapshot.rs`, Schema v18+, `#[serde(default)]` для backward compat).
- Swap-логика в `pick_action` (`utility/mod.rs`) — локальный `per_actor_tuning` + `per_actor_world` до первого обращения к `world.tuning`.
- `SCHEMA_VERSION` 17 → 18 в `log.rs` с записью истории.
- 3 юнит-теста в `tuning.rs`: `apply_override_empty_is_identity`, `apply_override_partial_thresholds`, `apply_override_toml_roundtrip`.

**В текущем scope override покрыт только для `thresholds`** (9 scalar полей). `difficulty` (LerpCurve) и `tables` (role-axis matrices) остались hooks-only (закомментированные поля в `AiTuningOverride`) — расширение по запросу первого quirk'а.

**Не в scope:** наполнение quirk'ами (Berserker / Coward / Focused). Пустая инфраструктура — конкретные override'ы появятся по дизайну позже.

**Коммит:** `66457e9`. **Golden-replay:** 0 / 131 diff (никто override не объявляет — scaffolding inert).

### Итого

| # | Шаг | Эстимейт | Golden-replay | Статус |
|---|---|---|---|---|
| 2.0 | golden-replay tool | 0.5 | — (создание инструмента) | **DONE** (`a1cc460`) |
| 2.1 | AiTuning scaffolding | 0.5 | 0 diff | **DONE** (`a099740`) |
| 2.2 | sanity.rs → TOML | 1.0 | 0 diff | **DONE** (`7d9bbaa`) |
| 2.3 | intent.rs → TOML | 0.5 | 0 diff | **DONE** (`a31b696`) |
| 2.4 | role factor_weights → table | 1.5 | 0 diff | **DONE** (`5d45398`) |
| 2.5 | position_eval → table | 1.0 | 0 diff | **DONE** (`88ff6b8`) |
| 2.6 | DifficultyProfile lerps → TOML | 1.5 | 0 diff | **DONE** (`18b62fd`) |
| 2.7 | UnitQuirks override scaffolding | 0.5 | 0 diff | **DONE** (`66457e9`) |

**Суммарно ~7 дней.** Любой шаг с ≠0 diff → откат коммита, разбор причины, повтор.

## Что откладывается в 2b (по потребности)

Когда шаг 3 (need layer) или шаг 4 (outcome vector) реально потребуют нелинейных зависимостей — ввести `ResponseCurve` как типизированные данные.

Минимальный джентльменский набор:

- **Linear clamped** — пропорциональные связи (killability от missing HP, proximity).
- **Logistic** — urgency-сигналы с порогом в середине (self_preserve, rescue_ally, finish_target). **Самая частая** форма.
- **Exponential decay** — спад с расстоянием/временем (у вас уже неявно через λ в influence propagation).
- **Power** — катастрофическое нарастание в высоких значениях (survival_quadratic = `x²`; role-bias = `x^1.5`).
- **Piecewise linear** — escape hatch.

Эвристика:
- Порог в середине с плавностью → logistic.
- Пропорционально и скучно → linear.
- «В высоких значениях катастрофа» → power, exponent 2–3.
- Ценность падает с расстоянием → exponential.
- Ничего не подходит → piecewise.

Не разворачивать инфраструктуру curves, пока три реальных потребителя не запросят.

## Что откладывается в 2c (по потребности)

- Difficulty override как отдельные файлы с частичным merge.
- Encounter override — когда появится шаг 14 (encounter scripting).
- Live-reload в dev-build — когда балансировщик начнёт тюнить регулярно.
- Визуализатор curves — когда curves станут нетривиальными настолько, что «видеть форму» будет полезнее «читать параметры».

Каждая — день-два работы, делается в момент реальной необходимости.

## Чего не делать

- Не создавать curve-инфраструктуру, пока ни один need signal её не запрашивает.
- Не смешивать миграцию и семантические изменения в одном коммите. 2a — **только** перенос, никаких изменений формул.
- Не превращать `AiTuning` в god-object. Оставлять `InfluenceConfig` отдельно, пока нет явной причины сливать.
- Не писать автоматический derive для merge'а UnitQuirks — явная функция с покрытием тестами безопаснее.

---

## Волна 1 — обновлённая последовательность

**0 ✓ → 1 ✓ → 2a ← сейчас здесь → 4 (+annotation) → 3 → 5 → 6**

Где:

- **0** ✓ (sim parity 0.1a + mining + log patch 0.3C) — закрыт. Артефакт `docs/ai_need_signals.md` готов под step 3.
- **1** ✓ (log extension 1.1 + assertion overlay 1.2–1.7) — закрыт. Ключевые коммиты: `81aa504` (1.1), `fb720b0` (1.2+1.3), `b018f9c` (1.4a library extraction), `2ed7cf2` (1.4b harness), `b2b6a2c` (1.5 первый batch из 9 кейсов). 1.6 (CI) скипнут, 1.7 активен.
- **2a** ← **в работе** (constants migration + golden replay gate). Шаги 2.0–2.5 закрыты (`a1cc460` → `88ff6b8`). Осталось 2.6 (DifficultyProfile lerps) и 2.7 (UnitQuirks scaffolding); все предыдущие шаги прошли gate `--compare-golden` 0 / 131 diff.
- **4** (outcome vector) — общий словарь, **до** need layer. **Внутри step 4** заводится структура `PlanAnnotation` с единственным начальным полем `outcome: Vec<OutcomeEstimate>` — annotation растёт по мере появления потребителей, не как отдельный «пустой» шаг 7a. Step 7a из последовательности убран.
- **3** (need layer) — поверх outcome. **Важно:** дескриптор step 3 в `ai_rework.md` должен быть обновлён: «Входы: NeedSignals считаются из `ActionOutcomeEstimate` + influence maps; raw snapshot достаётся только для tactical facts (hp%, role, статусы)». Синхронизация — коммит одновременно с реализацией.
- **5** (terminal eval) — поверх outcome и sim parity (0.1a). Требование 0.1b (end-to-end) появляется только в шаге 12.
- **6** (goal-preserving repair) — расширяет существующий `mismatch()` + continuation из `enemy_turn.rs:181–212`, не переписывает.

PlanStage-trait (полный 7), critics decomposition (10, benchmark-driven после step 0.3C histograma), bands+agenda+scorecard (11, разрезанный на bands-first) — фаза 2 или позже.

## Гейты между шагами

Каждый шаг следующей цепочки **не стартует**, пока не выполнен gate предыдущего:

| Шаг | Gate для следующего |
|-----|---------------------|
| 0.1a | Property tests зелёные (`cargo test --lib combat::ai::planning::parity_tests`) |
| 0.3 | Артефакт `docs/ai_need_signals.md` закоммичен |
| 1 | Scenario harness живой (`cargo test --test ai_scenarios` зелёный), ≥9 кейсов, ≥3 категории покрыты |
| 2.0 | Golden baseline зафиксирован (`logs/golden_pre_2a.jsonl` закоммичен) |
| 2a | `--compare-golden` 0 diff'ов на каждом шаге 2.1–2.7; scenario harness зелёный |
| 4 | PlanAnnotation растёт, `compute_factors` читает outcome; golden replay не деградирует |
| 3 | Need signals из 0.4 реализованы; `select_intent` читает NeedSignals; сценарии step 1 + новые «need-driven» сценарии зелёные |
| 5 | Terminal score включён в scorer; sim parity 0.1a гарантирует корректность финального снапшота |
| 6 | `continuation_severity` классификация в логах; сценарии Plan freeze / continuation стабильно зелёные через сотни тиков |

Без gate — не идём дальше. Gate провален → возвращаемся к предыдущему шагу.
