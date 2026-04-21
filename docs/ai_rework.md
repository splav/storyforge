# AI Rework Roadmap: Goal-Axis Impact Model

План поэтапного рефакторинга AI scoring, исходящий из анализа трёх свежих replay-логов (апрель 2026) и дискуссии по критике пяти точечных правок. Цель — **заменить набор ad-hoc факторов на непрерывную импакт-модель по осям цели**, сохранив hard-контракты там, где они реально нужны.

Параллельный документ: [`docs/ai.md`](ai.md) — текущая архитектура. Этот файл — целевая картина и порядок переходов.

---

## 1. Контекст: что поймали в логах

Три последних боя (`logs/20260421T07*.jsonl`) вскрыли повторяющиеся паттерны:

| № | Симптом | Пример |
|---|---|---|
| S1 | **Холостой ход** на full HP | r3/r5 Старшина: `Move→(6,0)→(6,1)→(6,0)`, displacement = 0, dmg = 0 |
| S2 | **Summon-спам** без учёта насыщения | Старшина кастует `summon_storm_spirit` в r1/r2/r4 подряд |
| S3 | **Killable intent → DoT** | r5 Старшина (HP 14/22, intent=killable): выбран `burn` вместо melee, dmg=0, killed=0 |
| S4 | **ProtectSelf-контракт пропускает summon/EndTurn** | r2 Старшина HP 4/22 panic_override → `summon_storm_spirit` вместо heal; r7 Бурешаман HP 3/30 panic_override → EndTurn при доступном heal |
| S5 | **Low-value атака через armor** | r4 Буревестник: `Move×2 → melee` за `dmg=1` по armored-цели, полный intent-bonus |

**Общая диагностика.** Scoring видит наличие action (step был потрачен, target был правильный), но не видит **результат** этого action по отношению к цели. Каждый точечный факт сейчас выражается через свой ad-hoc фактор или маску; их композиция непредсказуема. План, который гоняет фишки по карте, выглядит как план, меняющий исход матча.

---

## 2. Принцип

Рефакторинг опирается на одно правило, вычитанное в дискуссии:

> **Непрерывные сигналы — да. Hard-контракты — оставить, но сформулировать через ту же ось, что и сигнал.**

Отсюда следуют три ограничения дизайна:

- **Нельзя** заменять `ProtectSelf` mask на «просто новый фактор» — маска выражает поведенческий инвариант «в панике не делать не-оборонительных вещей», который не сводится к чистому весу.
- **Можно** переформулировать маску как `ε-threshold` по соответствующей оси импакта — тогда инвариант сохраняется, а критерий становится калиброван на общей метрике.
- **Нельзя** делать формулы вида `raw_displacement / mp_spent` или `damage / hp` в лоб — они ломают валидные edge cases (кастанул-вернулся, low-damage utility hit). Импакт оси должен суммировать **все релевантные виды работы**, не только numerical yield самой жирной колонки.

---

## 3. Целевая модель

### 3.1. Impact axes

Каждый `TurnPlan` агрегирует **вектор Δ** по каноническим осям. Оси устойчивы к росту content'а: новые способности/эффекты вливаются в существующую ось, не множа их число.

| Ось | Что включает | Агрегация по шагам |
|---|---|---|
| `damage_now` | EV-урон, нанесённый в commit-prefix | discounted sum |
| `damage_promised` | Отложенный урон (DoT-тики после плана) | discounted sum, γ ≈ 0.5 на тик |
| `kill_now` | Смерть цели, случающаяся **внутри** плана | max (1 / 0 per victim) |
| `kill_promised` | Лицо цели, гарантированно умрёт от already-applied DoT'а без нового касто́в | max, дополнительно × γ^time |
| `cc_impact` | stun / silence / disable (threat × duration) | discounted sum |
| `ally_rescue` | heal + buff-защита + taunt-redirect | discounted sum |
| `self_survival` | Δ self_hp + выход из AoO-зон + armor-buff + дистанция от threat-центра | plan-terminal |
| `tempo_gain` | Полезный прогресс позиции: вход в cast-range, LoS, выход из danger, сокращение до фокус-цели. Инвариант: displacement = 0 ≠ impact = 0, если достигнут любой из этих подцелей. | plan-terminal |
| `saturation_penalty` | Отрицательный вклад: сколько уже есть того же эффекта (активных summons того же class, одинаковых buffs на той же цели) | per-effect |

Ось `position` из текущей модели распадается на `tempo_gain` (положительный вклад движения к цели) + `self_survival` (отрицательный из-за danger). Ось `risk` растворяется в `self_survival` как per-path bleed. `focus` исчезает — «импакт по правильной цели» становится суммой axes, отфильтрованной на intent target.

### 3.2. Intent как weight vector

Сейчас `intent_score(step, intent, …)` возвращает скаляр с ad-hoc формулами на каждый intent. В целевой модели intent хранит веса по осям + **step-geometry hook** для случаев, где геометрия не сводится к оси:

```rust
struct IntentContract {
    weights: AxisVector,
    geometry: Option<fn(&ScoredStep, &Context) -> f32>,  // pursuit_move, reposition tiers
    hard_threshold: Option<(Axis, f32)>,                 // contract mask
}
```

- `weights`: большая часть скоринга. Напр. `ProtectSelf.weights = { self_survival: 2.0, damage_now: 0.2, … }`.
- `geometry`: для случаев, где неудобно расщеплять на оси (pursuit_move'ы с нелинейным reward за «вошёл в threat bubble», reposition tiers). Хук читает `ScoredStep` и добавляет скаляр — не заменяет weights, а дополняет.
- `hard_threshold`: `ProtectSelf` требует `Δ.self_survival ≥ ε`; `FocusTarget(killable)` требует `Δ.damage_now ≥ target_hp × α` или `Δ.kill_now ≥ 1`. План, не прошедший threshold, получает `-∞` как сейчас — **контракт сохранён**, просто сформулирован на axis.

### 3.3. Contract mask = ε-порог на оси

Главная идея, которая делает переход безопасным: `ProtectSelf` больше **не делает** проверку «план не-defensive». Он проверяет «план улучшает self_survival хоть на ε». Это автоматически:

- режет `summon_storm_spirit` и `EndTurn` — они не меняют self_survival → Δ = 0 < ε → `-∞`;
- пропускает `heal`, `retreat_to_safer_tile`, `armor_up` — у них Δ.self_survival > 0;
- работает для новых способностей без правки маски — если новая способность увеличивает self_survival, она автоматически становится допустимой защитной опцией.

Инвариант `ProtectSelfNoDefensive` в adaptation layer продолжает работать: «нет ни одного плана с Δ.self_survival ≥ ε» — тот же триггер, та же реакция (LastStand globally).

---

## 4. Маппинг симптомов → оси

| Симптом | Где лечится в новой модели |
|---|---|
| S1 (holostoy move) | `tempo_gain` ≈ 0 И `self_survival` ≈ 0 И `damage_now` = 0 → dot product с любым intent близок к 0 → план проигрывает любому осмысленному. Не нужен штраф за «displacement = 0» — работает через отсутствие положительного вклада. |
| S2 (summon spam) | `saturation_penalty` растёт с каждым активным summon того же класса → summon-bonus падает нелинейно (`0.65^N`, только для уже **живых на поле** spirits, не внутри плана — последнее уже есть). |
| S3 (killable → DoT) | `kill_now` и `kill_promised` — разные оси. `FocusTarget(killable).weights.kill_now = 1.5`, `.kill_promised = 0.5`. Mели melee-план убивает сразу → kill_now = 1, burn-план промахивает kill_now и кладёт только kill_promised с discount. |
| S4 (ProtectSelf leak) | Contract mask = `Δ.self_survival ≥ ε`. `summon_storm_spirit` и `EndTurn` оба дают Δ.self_survival = 0 → оба режутся в `-∞`. Heal побеждает естественно. |
| S5 (low-value armor hit) | `damage_now` учитывает armor (оно уже так в sim — `final_damage_f32`). Intent.weights домножают `damage_now`, не сам факт каст'а. 1 dmg по 20-HP = 0.05 по оси damage_now, intent-вклад пропорционально мал. |

Все пять симптомов лечатся перестройкой **структуры** скоринга, не добавлением новых if'ов.

---

## 5. Roadmap

Порядок продиктован принципом «максимум регрессионной ценности на единицу риска». Сильные идеи из критики (S1, S3) идут первыми — они требуют минимального изменения архитектуры. ProtectSelf (S4) — последний, потому что это самое архитектурное изменение.

### Phase 0. Baseline и инструментарий

**Цель:** зафиксировать стартовую точку для замера прогресса.

1. Собрать **replay corpus**: 10–20 свежих боёв, разнообразие сценариев (stormborn / beastblood / bell_under_veil).
2. Расширить `replay_ai_log` метриками regression: `wasted_mp_ratio` (displacement=0 shots / total), `panic_leak_rate` (panic_override с non-defensive committed), `killable_closure_rate` (killable intent → kill_now=1).
3. Прогнать baseline metrics на главной ветке, сохранить в `logs/baseline_20260421.txt`.
4. Добавить в `docs/ai-replay.md` раздел «regression metrics» с формулами.

**Выход:** числовая база. Каждая следующая фаза замеряет свою метрику и не ухудшает остальные более чем на 5%.

---

### Phase 1. Axis `tempo_gain` — лечит S1 (holostoy move)

**Почему первой.** Самая частая странность в логах, минимум архитектурного риска (новая колонка в factors, не меняет структуру).

**Что делать.**

1. В `factors/mod.rs` добавить ось `tempo_gain: f32` (signed, `[-1, 1]`).
2. Реализация `compute_tempo_gain(plan, active, intent_target, maps)`:
   - `+ Δdistance_to(intent_target)` если fokus есть, нормировано на speed
   - `+ entered_cast_range_bonus` (bool → 0.3)
   - `+ gained_los_bonus` (bool → 0.2)
   - `+ exit_danger_bonus` = `max(0, danger(start) − danger(end))`
   - `− 0` если всё это 0 (не штрафует активно, просто не даёт кредита)
3. Убрать ось `position` или свести её к `(tempo_gain + self_survival_terminal_proxy)` — final value будет тем же, формулировка чище. На этой фазе можно просто добавить `tempo_gain` как девятую ось и оставить `position` — переход плавный.
4. В `AXIS_FACTOR_WEIGHTS` добавить колонку: Tank 0.8, Melee 1.0, Ranged 1.2, Control 1.0, Support 0.8.
5. `replay_ai_log` поддерживает новое поле через `#[serde(default)]`.

**Проверка.** На replay corpus:
- `wasted_mp_ratio` должен упасть ≥ 50%.
- `killable_closure_rate` не ухудшиться.

**Файлы.** `src/combat/ai/factors/mod.rs`, `src/combat/ai/factors/tempo.rs` (новый), `src/combat/ai/planning/scorer.rs`, `src/combat/ai/role.rs`, `src/combat/ai/log.rs` (+schema v9).

---

### Phase 2. Расщепить `kill` → `kill_now / kill_promised` — лечит S3 (DoT overcredit)

**Почему сейчас.** После `tempo_gain` есть доверие к методу добавления осей. `kill` — простейшее расщепление.

**Что делать.**

1. В `factors/offensive.rs::single_target_kill` уже есть сигнал «expected ≥ hp». Разделить на:
   - `kill_now` = 1 если убиваем в commit-prefix шага (damage_now ≥ target.hp).
   - `kill_promised` = 1 если после всех applied-этим-планом DoT'ов tick-sum ≥ target.hp **и** kill_now = 0.
2. В scorer агрегация: `kill_now` — max, `kill_promised` — max × 0.6 (γ discount).
3. В `AXIS_FACTOR_WEIGHTS` kill-строка становится двумя: текущие веса → `kill_now`, `kill_promised` = `kill_now × 0.5` для всех ролей кроме Control (там 0.8 — DoT всё же ценен для Control).
4. `FocusTarget(killable).weights.kill_now = 2.0`, `.kill_promised = 0.3` — делает DoT неэффективным в killable intent.

**Проверка.**
- `killable_closure_rate` ≥ +25% pp на corpus.
- DoT-планы продолжают выбираться в `FocusTarget(default)` на danger цепи HP — это not regression.

**Файлы.** `src/combat/ai/factors/offensive.rs`, `src/combat/ai/factors/mod.rs`, `src/combat/ai/planning/scorer.rs`, `src/combat/ai/role.rs`.

---

### Phase 3. Axis `saturation_penalty` — лечит S2 (summon spam, buff-оверлей)

**Почему сейчас.** Близко к существующему `summon_bonus`-механизму, легко расширить.

**Что делать.**

1. В `planning/scorer.rs::plan_summon_bonus` уже есть per-plan saturation (`count/cap` decay). Добавить **global saturation** — читать уже-живых summon'ов того же класса из snapshot: `active_count = snapshot.units.iter().filter(|u| u.summoner == Some(active.entity) && u.class == summon_class).count()`. Помножить bonus на `0.65^active_count`.
2. Для buffs (более деликатно — критик прав, глобальный DR опасен):
   - ввести `buff_class` (haste / armor / dmg_up / shield) в `StatusDef`.
   - саturation только при **совпадении (target, buff_class)**: если у target X уже висит haste, второй haste получает × 0.4.
   - разные buff'ы на одном target → отдельные saturation buckets → не мешают стэку.
   - разные target'ы → один класс → никакого штрафа (haste на carry-1 и haste на carry-2 независимы).
3. Не делать глобального `0.65^N` на все бафы — только same-target, same-class.

**Проверка.**
- Replay corpus: Старшина больше не делает 3 summon подряд при 3 активных spirits.
- Legitimate buff-стэк (haste + armor на танке) не ломается.

**Файлы.** `src/combat/ai/planning/scorer.rs`, `src/content/statuses.rs` (добавить `buff_class`), `src/combat/ai/factors/mod.rs` (saturation axis).

---

### Phase 4. Intent alignment через axis impact — лечит S5 (low-value armor hit)

**Почему сейчас.** Предыдущие фазы наполнили оси; можно начать пере-выражать intent через них.

**Что делать.**

1. Оставить `intent_score` как функцию-контейнер, но заменить её тело на:
   - `dot(plan_impact_vector, intent.weights)` — core.
   - `+ intent.geometry(step, ctx).unwrap_or(0)` — hook для pursuit_move / reposition tiers.
2. Конкретные переходы:
   - `FocusTarget { target }`: `weights = { damage_now_vs_target: 1.0, kill_now_vs_target: 1.5, cc_impact_vs_target: 0.5 }`. Геометрия `pursuit_move_score` остаётся как есть.
   - `ApplyCC { target }`: `weights = { cc_impact_vs_target: 1.5, damage_now_vs_target: 0.3 }`. Геометрия как есть.
   - `SetupAOE`: через `aoe_hits_axis` (количество целей в зоне). `weights.aoe_hits = 1.0`.
   - `Reposition`: `weights = { tempo_gain: 1.0 }` + геометрия tiered.
3. «X vs target» — фильтрация оси на intent target. Реализация: ось хранит `Vec<(Entity, f32)>`, intent.weights домножается на долю, попавшую в intent target.
4. **Результат для S5.** Удар на 1 dmg по 20-HP цели → `damage_now_vs_target = 0.05` → intent-вклад 0.075 вместо 1.0. Useful debuff на правильной цели (cc_impact > 0) сохраняет полный intent-credit — критик был прав, это не «intent × damage», а «intent × total impact».

**Проверка.**
- Low-damage armor hits больше не выигрывают тай-брейк у обходных/ranged манёвров.
- CC/setup/taunt планы сохраняют позиции в ранкинге (regression guard).

**Файлы.** `src/combat/ai/intent.rs` (рефакторинг `intent_score`), `src/combat/ai/planning/scorer.rs` (фильтрация по target), `src/combat/ai/factors/offensive.rs` (per-target breakdown).

---

### Phase 5. Axis `self_survival` + переформулированный ProtectSelf contract — лечит S4 (panic leak)

**Почему последней.** Самое архитектурное изменение — трогает сразу intent.rs, scoring.rs, планинг-ветку ProtectSelf и contract mask. Делать после того, как остальные оси уже стабильны и можно сравнить «до / после» на corpus.

**Что делать.**

1. Ввести ось `self_survival: f32` (signed):
   - `+ heal_self / max_hp`
   - `+ armor_buff_self / max_hp × 3` (3 хода = средний срок статуса)
   - `+ exit_aoo / aoo_damage` (EV вылета из AoO зон)
   - `+ distance_gained_from_threat_center × 0.1`
   - `− new_aoo_exposure / hp`
2. В `AXIS_FACTOR_WEIGHTS` для всех ролей: 0.8 (Tank / Support — выше: 1.0 / 1.2).
3. **Contract re-formulation.** В `pick_action` после adaptation:
   - если `intent == ProtectSelf`: план выживает ⇔ `plan.impact.self_survival ≥ ε` (ε = 0.15 — ~15% max_hp).
   - если `intent == FocusTarget(killable)`: план выживает ⇔ `plan.impact.kill_now ≥ 1 OR plan.impact.damage_now_vs_target ≥ target.hp × 0.3`.
   - планы с `mode == LastStand` mask не применяется (unchanged).
4. **Удалить** старую проверку `plan_is_defensive` (по target-type Cast'а). Заменить на пороговый фильтр. В логах `AdaptationReason::ProtectSelfNoDefensive` становится эквивалентным «нет ни одного плана с self_survival ≥ ε».
5. Миграция тестов в `adaptation.rs` и `intent.rs` — не на type `SingleAlly`, а на ось.

**Проверка.**
- `panic_leak_rate` должен упасть до ≤ 2% (было ~15% на corpus).
- Существующие ProtectSelf-тесты проходят без изменения поведения, но с новой формулировкой fixture'ов.
- Bias regression: убедиться, что heal-на-союзника при ProtectSelf (self не хилится, но плохой союзник хилится и self_survival поднимается через `ally_support` ноду) всё ещё работает как раньше. Если нет — ε-thresh нужен многоосевой: `self_survival + ally_rescue × 0.5 ≥ ε`.

**Файлы.** `src/combat/ai/factors/survival.rs` (новый), `src/combat/ai/intent.rs` (contract), `src/combat/ai/planning/scorer.rs`, `src/combat/ai/planning/adaptation.rs`, `src/combat/ai/role.rs`.

---

### Phase 6 (optional). Полная канонизация факторов

После фаз 1–5 структура факторов уже импакт-ориентирована, но старые поля (`position`, `focus`, `risk`) живут параллельно. Эта фаза — чистящая:

- `position` → удалить, заменить derived-читалкой `tempo_gain + self_survival_terminal`.
- `focus` → удалить, все consumer'ы переходят на `target_priority × (damage_now_vs_target + cc_impact_vs_target)`.
- `risk` → удалить, self_survival уже содержит per-path bleed.

**Когда делать.** После 2–3 недель стабильной работы фаз 1–5 в мастере. Не пытаться объединить с ними — это чистый cleanup, не меняющий поведения.

---

## 6. Чего не трогаем

Обсуждение было очень конкретным; вне scope явно остаются:

- **Trade economy** (`trade.rs`). Работает чисто и изолированно, не пересекается с impact axes.
- **Adaptation layer.** `ExpectedSelfLethal`, `ProtectSelfFutile` — fact-based триггеры режима оценки; они не про скоринг, они про выбор функции ценности. Новая модель осей использует adaptation без изменений.
- **Intent selection** (`select_intent`). Какой intent выбрать — отдельный вопрос. Мы меняем только как план **оценивается** под выбранным intent.
- **Sanity pipeline.** Multiplicative penalties — ценовая корректировка, не value function. Не пересекается с импакт-моделью.
- **Difficulty knobs.** Калибруются после phase 5; на MVP фазах остаются теми же.

---

## 7. Риски и миграция

### 7.1. Log schema

Каждая фаза, вводящая ось, бампит `SCHEMA_VERSION`. `replay_ai_log` читает новые поля через `#[serde(default)]` — старые логи остаются читаемыми, новые оси = 0 для них. В отчёте replay'а помечать такие записи как «partial axis coverage» чтобы не интерпретировать 0 как «план не произвёл tempo_gain».

### 7.2. Параметр-тюнинг

Веса в `AXIS_FACTOR_WEIGHTS` и `ε-thresholds` — новые knobs. Не добавлять все сразу в `difficulty.rs` — сначала жёстко зашить значения, калибровать на corpus, и только после стабильности выносить в difficulty profile (если вообще есть сигнал, что разные уровни нуждаются в разных значениях).

### 7.3. Регрессии

На каждой фазе прогонять full corpus + baseline metrics. Правило: Δ метрики, которую фаза **не должна** трогать, ≤ 5%. Если больше — значит фаза затрагивает что-то сверх объявленного, пересмотр scope.

### 7.4. Порядок критичен

Phase 5 (ProtectSelf) после Phase 1–4 не случайно. Contract mask — самая чувствительная точка; ломать её до того, как self_survival-ось стабилизирована, означает риск «все planы проходят» или «ни один не проходит» в интервале, пока веса подбираются.

---

## 8. Метрики успеха

| Метрика | Baseline (apr 2026) | Цель после phase 5 |
|---|---|---|
| `wasted_mp_ratio` (displacement=0 planы, committed) | ~12% | ≤ 3% |
| `panic_leak_rate` (panic_override → non-defensive committed) | ~15% | ≤ 2% |
| `killable_closure_rate` (killable intent → kill_now=1 в plane) | ~60% | ≥ 85% |
| `summon_spam_rate` (≥3 summon одного класса подряд одним актором) | ~8% из Storm-боёв | ≤ 1% |
| `low_value_hit_rate` (committed melee с `damage_now / target_hp < 0.1`) | ~10% | ≤ 4% |
| Replay-corpus ranking deltas | — | 5–15% на фазу |

Метрики фиксируются в `replay_ai_log --metrics-summary`; фаза не мержится, пока своя метрика не попадает в цель и соседние не ухудшаются больше 5%.

---

## 9. Связь с текущей архитектурой

- **Shared effects core** (`effects_*.rs`) не трогаем — оси читают `AbilityOutcome` как чёрный ящик.
- **Adaptation Layer** сохраняется. Его триггеры остаются fact-based, но «defensive option» пересматривается через ось: `plan_is_defensive(plan) := plan.impact.self_survival ≥ ε` вместо текущего target-type sniff'а.
- **Reservations / trade** не затронуты.
- **Influence maps** остаются как есть — `tempo_gain` и `self_survival` читают те же `danger` / `opportunity` / `ally_support`, просто композируют их в осевую метрику вместо плоского `position_eval`.

---

## 10. Что делать прямо сейчас

1. Завести отдельную ветку `ai/axis-impact`.
2. Phase 0: расширить `replay_ai_log` regression-метриками + зафиксировать baseline на текущем corpus.
3. Phase 1: `tempo_gain` — MVP, измеримый эффект на S1. Ожидаемо: 1–2 дня на реализацию + 1 день на калибровку весов.
4. После Phase 1 — смотреть на corpus и решать, стоит ли Phase 2 делать сразу, или нужны ещё логи, вскрывающие S3 на большем разнообразии.

Сильные фазы (1, 2, 3) мержатся быстро. Phase 5 — отдельная ветка, отдельный review, возможно отдельный contributor-pass.
