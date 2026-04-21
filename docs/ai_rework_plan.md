# AI Rework — Developer Plan

Практическое сопровождение к [`docs/ai_rework.md`](ai_rework.md). Описывает **что и где править** для перехода на goal-axis impact model, с привязкой к текущему коду и смыслу каждого шага. Детализация — средняя: даёт карту правок, но не подменяет дизайн-доки.

Связанные документы:
- [`docs/ai_rework.md`](ai_rework.md) — целевая картина, принципы, маппинг симптомов.
- [`docs/ai.md`](ai.md) — текущая архитектура scoring / adaptation.
- [`docs/ai-replay.md`](ai-replay.md) — offline-replay и будущие regression metrics.

---

## Принципы работы

1. Каждая фаза — отдельная PR-ветка от `ai/axis-impact`. Мерж только после прогонки replay corpus'а из Phase 0 и подтверждения, что целевая метрика фазы достигнута, а соседние не ухудшились >5%.
2. Каждая фаза, меняющая layout факторов → **бамп `SCHEMA_VERSION`** в `src/combat/ai/log.rs:73`, новые поля через `#[serde(default)]` у reader'а.
3. Тесты в `planning/scorer.rs`, `intent.rs`, `adaptation.rs`, `role.rs` должны проходить на каждой фазе. Правила обновлять, а не удалять — fixture может переписываться, но инвариант должен быть тот же (или явно заменён).
4. Если фаза по пути упирается в неожиданную связность — стоп, документируем в `ai_rework.md`, корректируем scope.

---

## Текущее состояние кода (reference map)

| Файл | Роль сегодня | Что трогаем |
|---|---|---|
| `src/combat/ai/factors/mod.rs` | Определение `PlanFactors` (9 полей), `NUM_FACTORS`, `SIGNED_FACTOR`, `compute_factors` | Все фазы: добавление осей, переименования |
| `src/combat/ai/factors/offensive.rs` | Per-step `compute_offensive`, `single_target_kill`, `status_cc_value` | Phase 2 (kill split), Phase 4 (per-target breakdown) |
| `src/combat/ai/factors/scarcity.rs` | Resource-vs-swing штраф Cast-ов | Не трогаем |
| `src/combat/ai/factors/adjustments.rs` | Reservation / crit-fail нерфы | Не трогаем |
| `src/combat/ai/role.rs` | `AxisProfile`, `AXIS_FACTOR_WEIGHTS` (колонки на 9 факторов), композитные веса | Каждая фаза, добавляющая ось, добавляет колонку |
| `src/combat/ai/intent.rs` | `TacticalIntent`, `intent_score`, `select_intent`, `pursuit_move_score` | Phase 4 (refactor `intent_score` в dot+geometry), Phase 5 (ProtectSelf contract) |
| `src/combat/ai/planning/scorer.rs` | `compute_plan_factors`, `finalize_scores`, `plan_summon_bonus` | Phase 1-5 (агрегация новых осей), Phase 3 (global summon saturation) |
| `src/combat/ai/planning/sanity.rs` | `plan_is_defensive`, `apply_protect_self_mask` | Phase 5 — `plan_is_defensive` переезжает на axis threshold |
| `src/combat/ai/planning/adaptation.rs` | `apply_adaptation`, `EvaluationMode`, `ProtectSelfNoDefensive` trigger | Phase 5 — `any_defensive` вычисляется через `self_survival ≥ ε` |
| `src/combat/ai/position_eval.rs` | `evaluate_position` (danger + ally_support + opportunity) | Phase 6 — возможная замена production-читалкой `tempo_gain + self_survival_terminal` |
| `src/combat/ai/log.rs` | `SCHEMA_VERSION`, `PlanLogEntry`, raw factors | Каждая фаза — новая ось в wire-формате |
| `src/bin/replay_ai_log.rs` | Offline replay/recompute | Phase 0 — добавление regression metrics; далее `#[serde(default)]` для новых полей |

---

## Phase 0 — Baseline и инструментарий

**Цель.** Иметь числовой критерий регрессии до того, как что-либо ломать.

### Шаги

1. **Собрать replay corpus** в `logs/corpus_20260421/`: 15–20 jsonl-логов разных сценариев (stormborn / beastblood / bell_under_veil). Источник — несколько быстрых прохождений демо + провокационные encounter'ы, где уже ловили S1–S5.
2. **Расширить `src/bin/replay_ai_log.rs`** секцией regression metrics. Вводим три счётчика:
   - `wasted_mp_ratio` — доля committed-планов, где `committed_prefix` тип `MoveOnly` и displacement=0 (актор вернулся в ту же клетку, см. `PlanLogEntry::committed_prefix`).
   - `panic_leak_rate` — доля записей с `adaptation_reason ∈ {ProtectSelfNoDefensive, ProtectSelfFutile}`, где committed action — не heal/buff/retreat (простейшая проверка: `target_type != SingleAlly|Myself` И не снижает danger на destination).
   - `killable_closure_rate` — среди записей с `intent_reason.kind == "killable"`: доля, где хоть один cast в committed-prefix даёт `raw_kill > 0` (читаем из raw factors, индекс `KILL_IDX`).
3. **Добавить флаг `--metrics-summary`** в replay-tool: при его наличии агрегирует метрики по всем переданным файлам и выводит сводку. Формулы — в `docs/ai-replay.md` в новый раздел «Regression metrics».
4. **Зафиксировать baseline.** Прогнать `replay_ai_log --metrics-summary logs/corpus_20260421/*.jsonl > logs/baseline_20260421.txt`, закоммитить в репо (только этот файл и corpus).

**Выход.** Базовые значения метрик в txt-файле. Каждая следующая фаза сравнивает новую прогонку (на том же corpus, но обновлённой версии replay-tool) с этим baseline'ом.

**Риски.** Baseline замеряет поведение **до** правок, но если corpus содержит старые schema-версии, метрики могут быть не определены для них (например, `trade_breakdown` появился в v6). Помечать такие записи как partial и не делить на них.

---

## Phase 1 — Ось `tempo_gain` (лечит S1)

**Смысл.** Move-шаги, реально приближающие к цели / в cast-range / из danger, должны получать положительный вклад. Сейчас `position` и `risk` их учитывают только через tile-evaluation, и если start-/end-тайл имеют одинаковый score (открытое поле без маркировки), то move вклад = 0 → холостой ход получает полный intent-bonus без полезного движения.

### Шаги

1. **Расширить layout факторов** в `src/combat/ai/factors/mod.rs`:
   - `NUM_FACTORS` → 10.
   - Добавить `TEMPO_IDX = 9`.
   - `SIGNED_FACTOR` — добавить `true` (ось signed).
   - `PlanFactors` — поле `pub tempo_gain: f32`. Обновить `as_array` / `from_array`.
2. **Новый модуль `src/combat/ai/factors/tempo.rs`** с функцией `pub(super) fn compute_tempo_gain(step, ctx, intent_target) -> f32`. Формула (ref. `ai_rework.md` §3.1):
   - движение — базис `Δdistance_to(intent_target) / speed`, clamp [-1, 1];
   - `+0.3` если шаг входит в cast-range (проверка: max attack range > 0 и `distance(caster_tile, target) ≤ max_attack_range`, где target берётся из intent);
   - `+0.2` за gained LoS (пока нет LoS-модели — оставляем `TODO`, возвращаем 0);
   - `+max(0, danger(active.pos) − danger(caster_tile))` как «exit_danger_bonus»;
   - move без intent_target → 0.
   - Cast: если cast попал без предшествующего move — `tempo_gain = 0`; если с move перед ним — unified через `ScoredStep::caster_tile()`, формула та же.
3. **Интегрировать** в `compute_factors` (`factors/mod.rs:218`): после `focus`-блока вызвать `tempo::compute_tempo_gain`. Агрегация на уровне плана: `plan-terminal` — т.е. значение берётся из последнего ScoredStep плана (в отличие от discounted sum). Добавить ветку в `scorer::compute_plan_factors_sans_intent` — правее `position` аналогичным образом.
4. **Добавить колонку в `AXIS_FACTOR_WEIGHTS`** (`role.rs:39`): `[Tank 0.8, Melee 1.0, Ranged 1.2, Control 1.0, Support 0.8]`. 10-я колонка. `AXIS_FACTOR_WEIGHTS: [[f32; 10]; 5]`, `AxisProfile::factor_weights()` → `[f32; 10]`.
5. **Wire-формат:** в `log.rs` бамп `SCHEMA_VERSION = 10`, `PlanLogEntry::raw_factors` становится `[f32; 10]`. Reader в `replay_ai_log` — через `#[serde(default)]`.
6. **Тесты.** В `tempo.rs` — unit: (a) approach move к известному target даёт положительный tempo; (b) round-trip move (туда-обратно) даёт tempo ≤ 0; (c) cast без предшествующего move даёт tempo = 0 (terminal, но start=end по клетке). В `scorer.rs` — проверить, что holostoy-план проигрывает аналогичному-но-без-move под `FocusTarget`.

**Проверка.** Прогон Phase 0 metrics: `wasted_mp_ratio` падает ≥ 50%; `killable_closure_rate`, `panic_leak_rate` без регрессии > 5%.

**Откат-точка.** Если ось завалит regression — убрать колонку из `AXIS_FACTOR_WEIGHTS` (set to 0), не удаляя сам фактор. Это оставит wire-формат, позволит дотестировать.

---

## Phase 2 — `kill` → `kill_now / kill_promised` (лечит S3)

**Смысл.** Сейчас `single_target_kill` в `factors/offensive.rs:150` возвращает 1 и за burst-kill в commit-prefix, и за DoT, который дотикает к смерти через 3 хода. Скорер трактует их одинаково, и `FocusTarget(killable)` с весом kill=1.6 одинаково поощряет обе опции — burn часто побеждает по сумме (низкий scarcity + добавка damage_now от тика). Разделив оси, DoT получит discount, burst сохранит полный credit.

### Шаги

1. **Выделить `kill_promised`.** В `factors/offensive.rs`:
   - Ввести helper `fn dot_tick_sum(def, target, caster) -> i32` — сумма ожидаемых тиков DoT-эффектов статуса на длительности (читаем `StatusDef::damage_per_turn × duration`).
   - `compute_offensive` возвращает `OffensiveFactors { damage, heal, kill_now, kill_promised, cc }` вместо одного `kill`. Правило:
     - `kill_now = 1` если `damage_now ≥ target.hp` **сейчас** (текущий expected damage, уже считается).
     - `kill_promised = 1` если `kill_now = 0` **и** `damage_now + dot_tick_sum + already_pending_dot_on(target) ≥ target.hp`. `already_pending_dot_on` — сканируем statuses на target в snapshot.
   - AoE-ветка: `kill_now` если хотя бы одна цель умирает сейчас; `kill_promised` если хотя бы одна цель умирает от DoT.
2. **PlanFactors** в `factors/mod.rs`: поле `kill` заменяется на `kill_now` и `kill_promised`. Индексы: `KILL_NOW_IDX = 1`, `KILL_PROMISED_IDX = <next>`. `NUM_FACTORS` — +1 ещё раз (уже 10 из Phase 1, становится 11).
3. **`AXIS_FACTOR_WEIGHTS`** (`role.rs:39`) — колонка `kill` становится двумя:
   - `kill_now` = текущие значения (оставляем 0.6/1.6/1.3/0.5/0.3).
   - `kill_promised` = `kill_now × 0.5` для всех, кроме Control — там 0.8 (DoT стратегически ценен в Control).
4. **`rescore_with_intent` / scorer** (`planning/scorer.rs:108, 229`): агрегация — `kill_now` и `kill_promised` оба discounted sum по `base^k`, как `kill` сейчас. Отдельные max-ы **не** применяем (это уже не binary после плана с двумя kill'ами).
5. **Intent веса — временное.** Перед Phase 4 просто поднять `AXIS_FACTOR_WEIGHTS[...][kill_now]` для всех ролей до ≥ `kill_promised × 2`. В Phase 4 это окончательно переедет в `intent.weights`.
6. **Log schema.** `SCHEMA_VERSION = 11`. Добавить поле `kill_now` / `kill_promised` в raw-массив. `replay_ai_log` — `#[serde(default)]`, для старых логов оба поля = 0.

**Проверка.** `killable_closure_rate` +25 pp на corpus (было ~60%, цель ≥85%). `wasted_mp_ratio`, `panic_leak_rate` — без регрессии > 5%.

**Риск.** `dot_tick_sum` может перекрывать damage_now (если ability наносит и damage и применяет DoT). Бронь: `kill_promised` имеет смысл только как «убийство, которое произойдёт **без** нового каста». В Phase 2 MVP достаточно того, что kill_now и kill_promised не выставляются **одновременно** (guard: `if kill_now == 1 { kill_promised = 0 }`).

---

## Phase 3 — Ось `saturation_penalty` (лечит S2)

**Смысл.** `plan_summon_bonus` (`planning/scorer.rs:310`) уже считает per-plan saturation: внутри одного плана второй summon получает меньше кредита. Но между ходами — свежий план, `count` из snapshot, и если уже 3 спирита — cap=3 → decay=0, но **контракт не гарантирует** чтение cap из content: при cap=5 и активных 3 decay=0.4 → bonus всё ещё большой. Нужен дополнительный nonlinear global penalty. Параллельно — buffs: те же проблемы (haste поверх haste), но более деликатно: штрафовать only same (target, buff_class).

### Шаги

1. **Global summon saturation.** В `plan_summon_bonus` (`scorer.rs:310`):
   - После вычисления `decay` добавить множитель `0.65_f32.powi(active_count as i32)`, где `active_count` — уже учтено в `count` (первые строки функции), т.е. формула меняется на `total += dpr * decay * 0.65_f32.powf(count_at_step)` где `count_at_step` = initial + предыдущие summon'ы этого плана.
   - Смысл: decay относится к cap'у ability, 0.65^N — отдельный архетипный нелинейный штраф, независимый от cap. 3 активных спирита → 0.65³ ≈ 0.27 дополнительно.
2. **Buff saturation — same-target/same-class.**
   - Ввести поле `buff_class: Option<BuffClass>` в `StatusDef` (`src/content/statuses.rs`). Enum: `Haste, ArmorBuff, DamageUp, Shield, None`. Default = `None`. Класс выставляется в TOML или наследуется от effect-signature.
   - В `factors/mod.rs` новая ось `saturation: f32` (signed, обычно ≤ 0). Расчёт per-step для Cast: если cast применяет status с `buff_class = Some(c)` на target, и на target уже висит другой status того же класса → штраф `-0.4`.
   - Агрегация — **discounted sum**. Плюс колонка в `AXIS_FACTOR_WEIGHTS`: 1.0 для всех ролей (saturation уже signed, знак регулирует направление).
   - Ось отдельная от `scarcity`: scarcity — про mana/rage economy, saturation — про buff-overlay redundancy.
3. **Проверки.** Не штрафовать:
   - разные bufftargets с одним classом (haste-ы на двух разных carry'ях);
   - разные buff_class на одном target (haste + armor_buff);
   - per-plan: если план сам собой кастует haste + потом **второй** haste на того же target — штрафуется (внутри плана тоже, через running state как в summon).
4. **Log/schema.** `SCHEMA_VERSION = 12`, `saturation` в raw-array. Для summon-saturation — отдельное логирование не нужно (проявляется в итоговом score).

**Проверка.** Corpus-тест: (a) Старшина не делает 3 summon подряд при 3 активных spirits на поле; (b) легитимный стак (haste + armor на танке) не страдает.

**Риск.** `StatusDef.buff_class` — новое поле. В TOML-контенте нужно проставить хотя бы для 4–6 очевидных бафов. Если пропустить — fallback: `None` → штраф не начисляется, старое поведение сохраняется. Не full-coverage, но без регрессии.

---

## Phase 4 — Intent as weight vector (лечит S5)

**Смысл.** Сейчас `intent_score` (`intent.rs:733`) — длинный match с ad-hoc формулами (0.3 за heal под FocusTarget, 1.0 за direct hit, и т.д.). Пять симптомов S5 показывают: ad-hoc формулы игнорируют **величину** impact'а (1 dmg по armored target даёт тот же intent-credit, что 10 dmg по голому). Переводим на dot-product `plan_impact × intent.weights` + geometry hook для pursuit / reposition, которые не сводятся к осям чисто.

### Шаги

1. **`IntentContract` struct** в `intent.rs`. Поля как в `ai_rework.md` §3.2: `weights: AxisVector`, `geometry: Option<fn(...)>`, `hard_threshold: Option<(Axis, f32)>`. `AxisVector` — typed wrapper over `[f32; NUM_FACTORS]` с builder-методами.
2. **Таблица контрактов.** Для каждого варианта `TacticalIntent` — статическая функция, возвращающая `IntentContract`. Напр.:
   - `FocusTarget { target }` → weights: `kill_now = 2.0, kill_promised = 0.3, damage_now = 1.0, cc = 0.5` (all per-target-filtered — см. шаг 4). Geometry: `Some(pursuit_move_score_hook)`. Hard threshold: `None` (kill-based threshold добавляется в Phase 5 для killable sub-case).
   - `ApplyCC { target }` → weights: `cc = 1.5, damage_now = 0.3`. Geometry: `Some(pursuit_move_score_hook)` с `cc_reach`.
   - `Reposition` → weights: `tempo_gain = 1.0`. Geometry: `Some(reposition_tier_hook)`.
   - `ProtectSelf` → weights: `self_survival = 2.0, damage_now = 0.2, heal = 1.0`. Geometry: `None`. Hard threshold: Phase 5.
   - `ProtectAlly`, `SetupAOE`, `LastStand` — аналогично, перенос существующих формул.
3. **Новый `intent_score`.** Заменить тело на:
   ```rust
   pub fn intent_score(intent, step, active, snap, maps, content, difficulty) -> f32 {
       let contract = contract_for(intent);
       let step_impact = compute_step_impact_vector(step, ctx);  // re-uses compute_factors
       let weighted = dot(step_impact, contract.weights);
       let geom = contract.geometry.map(|f| f(step, ctx)).unwrap_or(0.0);
       weighted + geom
   }
   ```
   Агрегация по плану — пока оставляем discounted sum как сейчас в `compute_plan_intent_sum` (`scorer.rs`).
4. **Per-target filtering.** Для `FocusTarget{target}` damage-вклад должен считаться **только по этому target'у**. Это значит, что `PlanFactors.damage` перестаёт быть scalar'ом — становится `Vec<(Entity, f32)>` **или** (прагматичнее для MVP) мы добавляем в `OffensiveFactors` поле `target_entity: Option<Entity>` и при dot-product в `intent_score` фильтруем по совпадению. Предпочтительно второе — меньше инвазии.
5. **Удалить ad-hoc формулы** в старом `intent_score` (`intent.rs:750-901`). Миграция тестов в `intent.rs::tests` — fixture'ы переписать на проверку `dot(impact, weights)`, а не на точные числа intent_score.

**Проверка.** Corpus: `low_value_hit_rate` (committed melee с `damage_now/target_hp < 0.1`) падает с ~10% до ≤ 4%. CC/setup планы сохраняют ранги — smoke-тест на нескольких существующих encounter'ах.

**Риск.** Самая большая фаза по объёму кода (`intent.rs` — 1264 строки, refactor затронет ~300 из них). Разбить на две сессии: (a) IntentContract + таблица, (b) замена тела `intent_score`. Между — прогон тестов.

---

## Phase 5 — Ось `self_survival` + ProtectSelf contract (лечит S4)

**Смысл.** Сейчас `plan_is_defensive` (`planning/sanity.rs:269`) признаёт план defensive по type-sniff'у action'а: move in safer direction **или** cast с `target_type ∈ {SingleAlly, Myself}`. Это пропускает `summon_storm_spirit` (target_type = Ground или Myself в зависимости от spec) и `EndTurn` (через пустой план). Rework: contract переформулирован как порог по оси `self_survival`. Любое действие, реально поднимающее ось, — defensive; не поднимающее — нет. Автоматически работает для новых способностей.

### Шаги

1. **Ось `self_survival`.**
   - Новый модуль `src/combat/ai/factors/survival.rs` с `pub(super) fn compute_self_survival(step, ctx) -> f32`. Формула (см. `ai_rework.md` §3.3):
     - `+ heal_self / max_hp` (только если cast targets self);
     - `+ armor_buff_self_duration × 3 / max_hp` (self-buff, свеча 3 хода usage-weight);
     - `+ exit_aoo_ev / max_hp` — если move уходит из AoO-зон; опирается на `expected_aoo_damage` (`planning/sanity.rs`) — тот считает для plan-в-целом, нужна per-step версия или diff between «с move» / «без move»;
     - `+ distance_from_threat_centroid × 0.1`, где `threat_centroid` — взвешенный центр по `InfluenceMaps.danger`;
     - `− new_aoo_exposure / max_hp` — если move входит в новую AoO-зону.
   - Plan-terminal aggregation (как tempo_gain).
2. **`PlanFactors` / AXIS_FACTOR_WEIGHTS.** Новое поле `self_survival`. Колонка в `AXIS_FACTOR_WEIGHTS`: Tank 1.0, Melee 0.8, Ranged 0.8, Control 0.8, Support 1.2.
3. **Переформулировать `plan_is_defensive`** (`planning/sanity.rs:269`).
   - Тело: `plan.impact.self_survival ≥ SELF_SURVIVAL_EPSILON` (ε = 0.15 ≈ 15% max_hp).
   - Параметр `defensive_margin` сохраняем или заменяем на ε.
   - Все call-sites (`apply_protect_self_mask`, `apply_adaptation::any_defensive` в `planning/adaptation.rs:288`) автоматически подхватывают новую семантику.
4. **IntentContract для ProtectSelf** (Phase 4) — добавить `hard_threshold: Some((Axis::SelfSurvival, ε))`. Это дублирует фильтр `plan_is_defensive`, но кодирует на уровне intent-контракта. Единый источник правды — один из двух (предпочту `plan_is_defensive`, если выбираю сразу; threshold в контракте — для будущего cleanup в Phase 6). MVP: держать в одном месте, выбрать — **`plan_is_defensive` через ось**, threshold в intent не подключать.
5. **IntentContract для `FocusTarget(killable)`.**
   - `select_intent` (`intent.rs:363`) возвращает `IntentReason::Killable{...}`, но сам `TacticalIntent` — обычный `FocusTarget{target}`.
   - Либо: ввести новый вариант `TacticalIntent::KillTarget { target }` отдельно от `FocusTarget{target}` (чище, но больше кода); либо: IntentContract читает `IntentReason`, если в `Adaptation` данные передаются вниз. MVP: добавить вариант, миграция scorer'а и adaptation — минимальная.
   - Новый контракт: `hard_threshold: Some((Axis::KillNow, 0.5))`, т.е. план с kill_now=0 при killable intent получает -∞ (как ProtectSelf без defensive). Это гарантирует, что burn не выиграет у burst-kill'а даже по сумме, если burst-kill в пуле есть. Если burst-kill нет — план с максимумом kill_promised побеждает.
6. **Удалить старый `plan_is_defensive`-branch по target_type.** После того, как ось стабилизирована и threshold работает — убрать код проверки `matches!(def.target_type, SingleAlly|Myself)` в `sanity.rs:283-293`.
7. **Тесты.**
   - `sanity.rs::tests` — переписать fixture'ы: план с summon у актора HP=4/22 **не** проходит `plan_is_defensive` (self_survival ≈ 0 < ε). План с self-heal проходит. EndTurn (пустой план) — self_survival = 0 → не defensive (было defensive!). Это семантическое изменение, нужно прогнать на corpus, чтобы убедиться, что не сломало LastStand-fallback: если *никакой* план не defensive, adaptation flip'ает в LastStand (существующее поведение).
   - `adaptation.rs::tests` — `ProtectSelfNoDefensive` тригерится при self_survival < ε во всех планах.
   - `intent.rs::tests` — killable intent с burst-kill в пуле → kill_now-план выбирается. С burn только (kill_promised) → проходит, если hard_threshold приходит в killable только для burst.

**Проверка.** `panic_leak_rate` ≤ 2% (было ~15%). Существующие ProtectSelf-тесты компилируются с обновлёнными fixture, все проходят. `killable_closure_rate` — без регрессии > 5% (в идеале даже растёт дальше, т.к. kill-threshold жёстче).

**Риск.** ε = 0.15 — число из воздуха. Калибровать на corpus: прогнать с разными значениями (0.05 / 0.1 / 0.15 / 0.2) и выбрать то, где `panic_leak_rate` минимален при стабильном `wasted_mp_ratio`. Многоосевой threshold (`self_survival + ally_rescue × 0.5`) — fallback, если ProtectSelf + heal-на-ранненого-ally-при-ущербе-себе начнёт резаться.

---

## Phase 6 ✅ — Канонизация (выполнено 2026-04-21)

Удалены старые оси `position`, `focus`, `risk`, дублировавшие сигналы `tempo_gain` и `self_survival`.

Изменения:
- `NUM_FACTORS = 10` (`damage, kill_now, kill_promised, cc, heal, intent, scarcity, tempo_gain, saturation, self_survival`)
- `SCHEMA_VERSION = 14`: `raw_factors` сократился с 13 до 10 элементов; v1–v13 логи несовместимы по индексам (предупреждение в replay tool)
- `evaluate_position` остаётся в `position_eval.rs` как вспомогательная функция для `sanity.rs` и `intent.rs` (Reposition)
- `apply_reservation_adjustments` упрощена: убраны параметры `focus` и `position`, убраны focus-fire bonus и tile-collision penalty
- Тесты обновлены, все проходят

---

## Порядок и зависимости

```
Phase 0 ─┬─ Phase 1 (tempo_gain) ─┐
         │                        │
         ├─ Phase 2 (kill split) ─┤
         │                        ├─→ Phase 4 (intent refactor) ─→ Phase 5 (self_survival + ProtectSelf) ─→ Phase 6 (cleanup)
         └─ Phase 3 (saturation) ─┘
```

Phases 1–3 независимы между собой — можно параллелить по отдельным веткам. Phase 4 требует осей из 1–3. Phase 5 требует Phase 4 (intent contract). Phase 6 — после стабильной Phase 5.

---

## Что **не** трогаем

- `src/combat/ai/trade.rs` — `trade_delta`, `trade_score`. Изолированная система, интегрирована через `plan_trade_bonus` (`scorer.rs:351`). Не пересекается с импакт-осями.
- `src/combat/ai/planning/adaptation.rs::apply_adaptation` — сам layer и его fact-triggers (`ExpectedSelfLethal`, `ProtectSelfFutile`) остаются. Только условие `any_defensive` переформулируется через ось в Phase 5.
- `src/combat/ai/intent.rs::select_intent` — логика выбора intent. Меняем только **оценку** плана под выбранным intent.
- `src/combat/ai/planning/sanity.rs::sanity_adjust_plans` — multiplicative penalty pipeline, не value-function. Не трогаем.
- `src/combat/ai/difficulty.rs` — новые weights и ε жёстко зашиваем в код первые несколько недель; в difficulty-profile выносим только если появится сигнал, что разным уровням нужны разные значения.

---

## Что делать **прямо сейчас**

1. `git checkout -b ai/axis-impact` от `main`.
2. Phase 0: расширить `replay_ai_log`, собрать corpus, зафиксировать baseline. PR `ai/axis-impact-phase0`.
3. Phase 1: `tempo_gain` MVP. PR `ai/axis-impact-phase1`. Ожидание: 1–2 дня реализации + 1 день на калибровку весов на corpus.
4. После Phase 1 — пересмотр плана: если corpus показывает, что S3/S5 выражены слабее, чем казалось по 3 логам — возможно, перепорядочить 2/3 или пропустить одну из фаз на MVP.
