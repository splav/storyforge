# Шаг 4 — `ActionOutcomeEstimate`: декомпозиция + финализация

Декомпозиция в стиле фазы 2a: коммит-на-сабшаг, golden-replay gate на каждом.
Спецификация: `docs/ai_rework.md` §4 и `docs/ai_rework_plan.md` §4 + gates.

**Две волны:**
- **Волна 1 (4.0–4.5)** ✓ DONE — outcome тип + producer + первые consumers + score_action removal.
- **Волна 2 (4.6–4.13)** — re-foundation outcome как fact vector + policy module top-level + clean break v27 → v28. Доводит шаг до финального чистого состояния без legacy-адаптеров.

## Preamble

**Текущее состояние скоринга.** 10 factors в `factors/mod.rs` (`PlanFactors`, `NUM_FACTORS = 10`). Центральная HP-equivalent функция — `score_action` в `scoring.rs:58`. Имеет **ровно 6 call-sites**:
- `factors/offensive.rs` — 3 (single-target damage/heal, aoe per-enemy, friendly-fire splash).
- `planning/future_value.rs` — 3 (λ_attack: FocusTarget / ApplyCC / default top-3).
- `planning/picker.rs:223` — 1 (reservations post-pick).

**Natural seam sim ↔ consumers.** `generate_plans` вызывает `ext_sim.apply_step(...) -> StepOutcome` на `generator.rs:154` и складывает в `plan.outcomes: Vec<StepOutcome>` (`types.rs:46`). **Это единственное место с plan-step granularity.** Туда заходит `ActionOutcomeEstimate` — параллельно с `StepOutcome`, с той же cardinality. Чище — **новый vec**, не расширять `StepOutcome` (иначе ломает JSONL schema и parity-тесты).

**Самый хитрый dependency-edge.** `intent_score` (`intent.rs:858,880`) внутри себя вызывает `compute_factors(step_ctx, step)` — значит, branches FocusTarget/ApplyCC апгрейдятся даром вместе с 4.3. А `future_value.rs::λ_attack` — **отдельный** consumer `score_action`: оценивает hypothetical committed state, где sim step НЕ запущен. Туда нужен on-demand `estimate_hypothetical(...)` helper (4.4).

**Sanity.rs** в scope step 4 НЕ трогаем — survival/AoO читают raw snapshot и path, не factors. Эти паттерны — кандидаты на critics в шаге 10.

## Сабшаги волны 1

### 4.0. `ActionOutcomeEstimate` type + `PlanAnnotation` scaffolding (zero-filled) ✓ DONE

**Scope.**
- Новый `src/combat/ai/outcome.rs`: `ActionOutcomeEstimate { expected_damage, p_kill_now, p_kill_soon, deny_value, rescue_value, board_pressure, exposure_delta, geometry_gain, resource_swing }` — все `f32`, `Default` = zeros.
- Новый `PlanAnnotation { outcomes: Vec<ActionOutcomeEstimate> }` (в `outcome.rs` или `planning/types.rs`).
- Поле `pub annotation: PlanAnnotation` в `TurnPlan` (`types.rs:35`). `#[serde(skip)]` — runtime-only, не попадает в JSONL в рамках step 4.
- В `generator.rs`: после seed и каждого extend — `annotation.outcomes.push(Default::default())`. Инвариант: `outcomes.len() == steps.len()`.
- `#[allow(dead_code)]` на полях с TODO → снимается в 4.1+.

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff**. Никто не читает, ничего не изменилось.

**Коммит:** `cb94250`. **Golden-replay:** 0 / 131 diff.

### 4.1. Sim заполняет `expected_damage` ✓ DONE

**Scope.** В generator.rs после `apply_step` → `ActionOutcomeEstimate { expected_damage: outcome.damage, ..Default::default() }`. **Point-of-computation: generator** (не `apply_step`, у которого 35+ call-sites — menять signature накладно).

**Gate.** Unit-тест invariant `annotation.outcomes[i].expected_damage == plan.outcomes[i].damage`. Golden **0 / 131 diff** — no consumer.

**Коммит:** `7de5c30`. **Golden-replay:** 0 / 131 diff.

### 4.2. Sim заполняет остальные 8 полей (producer-complete) ✓ DONE

**Scope.** Каждое поле — явный источник:
- `p_kill_now` ← `1.0 if !outcome.killed.is_empty() else 0.0`.
- `p_kill_soon` ← экстракция `factors::offensive::split_kill` в `outcome::estimate_kill_soon(def, target, caster, content)`.
- `deny_value` ← extract из `status_cc_value` + `stun_denial_value` (exactly as in current `compute_offensive` cc-summ).
- `rescue_value` ← heal-branch `score_action` on SingleAlly + urgency (hp_missing + danger). Копия 1:1.
- `board_pressure` ← **0.0** (hook для шага 5, terminal eval).
- `exposure_delta` ← `worst_path_danger(plan, maps)` для Move; `0.0` для Cast.
- `geometry_gain` ← **0.0** (hook для шага 17).
- `resource_swing` ← `-def.cost_ap` (Cast) + resource costs (signed: negative = spent).

**Gate.** Unit-тесты per field + parity тест `offensive::split_kill` не задет. Golden **0 / 131 diff** — no consumer, ничего не должно двигаться.

**Коммит:** `88da91f`. **Golden-replay:** 0 / 131 diff.

### 4.3. `factors::offensive` читает outcome (первый real consumer) ✓ DONE

**Scope.**
- `compute_factors` signature → `compute_factors(ctx, step, outcome: &ActionOutcomeEstimate)`. Два call-sites: `scorer::compute_plan_factors_sans_intent:466`, `intent::intent_score:858,880`.
- `offensive::compute_offensive` переписывается: damage/heal/kill_now/kill_promised/cc ← из outcome.
- `score_action` жив. Зависимости в friendly-fire и aoe per-enemy остаются до 4.4.

**Gate.** `factors::offensive::tests::*` + `scorer::tests::rescore_matches_full_score_under_same_intent` зелёные. Golden **0 / 131 diff** — extracted формулы 1:1 из `score_action`. Любой diff → откат, разбор.

**Коммит:** `7aae9c9`. **Golden-replay:** 0 / 131 diff.

### 4.4. `future_value::λ_attack` + `picker` reservations на outcome ✓ DONE

**Scope.**
- Новый helper `outcome::estimate_hypothetical(def, target, caster_ctx, content, committed_pos, danger) -> ActionOutcomeEstimate` — для committed state (без sim). Формулы те же что `score_action` — hypothetical outcome заполняется так, чтобы λ_attack = `0.5 * est.expected_damage` давал buchstabelich тот же результат.
- `picker.rs:223` → `reserve_damage(ent, outcome.expected_damage)`.
- `score_action` переименовывается в `score_action_legacy` + `#[deprecated]`. Остаётся только для AoE friendly-fire внутри `factors/offensive.rs`.

**Gate.** Tolerance **≤3 epsilons / 131 entries** (Q1 решение). В коммите фиксируется письменное обоснование: «λ_attack переехал с HP-equivalent scalar на weighted outcome fields; веса выбраны чтобы совпасть на типичных abilities ±1e-6».

**Коммит:** `d2cf7c6`. **Golden-replay:** 0 / 131 diff. Q1 reconsidered → literal rewire (см. commit message).

### 4.5. Cleanup ✓ DONE

**Scope.**
- AoE friendly-fire branch переписывается на hypothetical outcome (helper `estimate_hypothetical` на ally). `score_action` **удаляется полностью** (Q2).
- `ScoringCtx::current_outcome: Option<&_>` → unconditional `&ActionOutcomeEstimate` (API cleanup; Q4 не требует Option, но intent_score fallback добавленный в 4.3 надо почистить).
- `intent.rs` убрать fallback ветки на `score_action` (если остались).
- **JSONL schema bump v18 → v19** (Q3). `PlanAnnotation` добавляется в `PlanLogEntry` с `#[serde(default)]`. В `SCHEMA_VERSION` история: «v18→v19: PlanLogEntry.annotation added (outcome vector scaffolding + future critics diagnostics)».

**Gate.** `cargo test/clippy`, `ai_scenarios`, golden **0 / 131 diff** — pure cleanup (semantic change был в 4.4).

**Коммит:** `6ae1429`. **Golden-replay:** 0 / 131 diff. `ya tool ast-index usages "score_action"` → 0.

## Итого волны 1

| # | Шаг | Эстимейт | Golden-replay | Статус |
|---|---|---|---|---|
| 4.0 | scaffolding | 0.5 | 0 diff | **DONE** (`cb94250`) |
| 4.1 | expected_damage | 0.5 | 0 diff | **DONE** (`7de5c30`) |
| 4.2 | full 9-field | 1.5 | 0 diff | **DONE** (`88da91f`) |
| 4.3 | offensive consumer | 1.5 | 0 diff | **DONE** (`7aae9c9`) |
| 4.4 | future_value + picker | 1.0 | 0 diff (literal rewire) | **DONE** (`d2cf7c6`) |
| 4.5 | cleanup + JSONL v18→v19 | 1.0 | 0 diff | **DONE** (`6ae1429`) |

**Суммарно ~6 дней** (4.5 расширен: +0.5 дня friendly-fire рефактор, +0.3 дня schema bump).

## Зафиксированные решения волны 1

1. **Tolerance в 4.4 → `(b) semantic weighted sum`.** λ_attack в 4.4 становится взвешенной суммой полей outcome (`expected_damage + w_cc * deny_value + w_kill * p_kill_now`). Gate: **tolerance ≤3 epsilons / 131 entries** с письменным обоснованием в коммите. Веса подбираются чтобы совпасть со score_action на типичных abilities ±1e-6.
2. **`score_action` → `(b) полный рефакторинг`.** AoE friendly-fire branch переписывается на hypothetical outcome на ally; `score_action` удаляется целиком в 4.5. +0.5 дня к сабшагу 4.5.
3. **JSONL → `(b) сериализовать сразу в 4.5`.** `PlanAnnotation` попадает в `PlanLogEntry`, `SCHEMA_VERSION` v18 → v19, `#[serde(default)]` на обеих сторонах для bwd compat.
4. **Плумбинг outcome → `(a) явный параметр`.** `compute_factors(ctx, step, outcome: &ActionOutcomeEstimate)` — 2 call-sites (scorer + intent_score). Data flow видим в signature.
5. **`rescue_value` в 4.2 → `(a) копия 1:1`.** Буквальный перенос формулы `score_action.heal` с urgency baked-in. Golden 0 diff в 4.x. Step 3 (need layer) разделит на outcome.rescue_value (effect magnitude) + NeedSignals.rescue_ally (urgency) семантически.
6. **Golden baseline → `(c) переименовать + пересобрать`.** `logs/golden_pre_2a.jsonl` удаляется, `logs/golden_pre_step4.jsonl` (тот же корпус: 4 v17-лога, 131 entries, пересобран с HEAD после 2.7). Diff между pre_2a и pre_step4 = 0 (код не менялся).

## Критические файлы волны 1

- `src/combat/ai/scoring.rs` — `score_action` legacy.
- `src/combat/ai/factors/offensive.rs` — первый real consumer (4.3).
- `src/combat/ai/planning/generator.rs` — producer site.
- `src/combat/ai/planning/types.rs` — `TurnPlan`, `PlanAnnotation`.
- `src/combat/ai/planning/future_value.rs` — второй consumer (4.4).
- `src/combat/ai/intent.rs` — автоапгрейд через `compute_factors`, плумбинг через ScoringCtx.
- `src/combat/ai/planning/scorer.rs` — `compute_plan_factors_sans_intent` + outcome index.

---

# Финализация: волна 2 (сабшаги 4.6–4.13)

## Preamble волны 2

### Текущее состояние после волны 1

Audit (выполнен через Explore агента, запротоколирован в Task #1) показал:

**Active consumers (5 полей реально используются):**

| Поле | Читают |
|---|---|
| `expected_damage` | `compute_offensive` (single-target) |
| `p_kill_now` | `compute_offensive`, `terminal::secure_kill`, `repair/goal.rs::extract_goal_context` (Finish vs Pressure классификация) |
| `p_kill_soon` | `compute_offensive`, `terminal::secure_kill` |
| `deny_value` | `compute_offensive` (cc) |
| `rescue_value` | `compute_offensive` (heal) |

**Populated, но никем не читается (2 поля):**
- `exposure_delta` — populated для Move через `step_path_danger`; ни один consumer не читает (комментарий в `outcome.rs:30` это явно признаёт).
- `resource_swing` — populated в `build_step_outcome_estimate`; ни один consumer не читает.

**Pure placeholders, always 0.0 (2 поля):**
- `board_pressure` — комментарий «filled in step 5», но step 5 (done) реализован через `TerminalScore`, а не через outcome (расхождение со спецификацией 4.2).
- `geometry_gain` — комментарий «filled in step 17» (backlog wave 4).

**Residual legacy:**
- `compute_score_core` (`outcome.rs:393`) остаётся public с docstring «inlined former score_action (deleted in step 4.5)». 5 callers: 2 outcome producers (legitimate), 1 hypothetical (legitimate non-sim path), 2 в AoE branch `compute_offensive` через `compute_aoe_damage`/`friendly_fire_penalty` (legitimate boundary, ally splash не в outcome).
- `compute_aoe_damage` в `compute_offensive` AoE branch — пересчитывает damage целиком вместо чтения `outcome.expected_damage`. Legitimate (ally splash не в outcome), но AoE `outcome.expected_damage` оказывается dead для scorer'а.
- `build_step_outcome_estimate` живёт в `generator.rs:219` — скрытое legacy: outcome populator в planner, не в `outcome.rs`.

### Проблемы текущей схемы

**1. Outcome — hybrid, не «общий словарь».** Спецификация обещает «outcome даёт общий словарь для всех потребителей». Реально 9 полей смешивают **факты** и **policy values**:

| Поле | Семантика |
|---|---|
| `expected_damage` (single-target) | `compute_score_core`: `raw × (0.5 + 0.5 × progress) + status_score` — **policy value** (HP-progressive scoring weight) |
| `expected_damage` (AoE) | sim raw total damage (enemies + allies, без netting) — **fact** |
| `p_kill_now` | `outcome.killed.is_empty()` — **fact** |
| `p_kill_soon` | derived из dot + raw damage check — **fact** |
| `deny_value` | `stun_denial_value + Σ(damage_taken_bonus × dur + armor_bonus × dur)` — **policy value** (CC-progressive scoring weight) |
| `rescue_value` | `compute_score_core` (heal branch) → `delta_pct × horizon × urgency` — **policy value** |
| `resource_swing` | `-(AP + costs)` — **fact** |
| `board_pressure`, `geometry_gain`, `exposure_delta` | placeholders / dead |

Hybrid-структура убивает обещанную extensibility:
- Critics (step 10) читать `expected_damage` как факт нельзя — оно уже policy-applied. Критики либо дублируют `compute_score_core` (drift risk), либо принимают pre-judged value (хрупкость).
- Agenda scorecard (step 11) хочет multi-policy evaluation (танк vs DPS оценивают одну и ту же атаку по-разному). Pre-baked policy в outcome закрывает дверь.
- UnitQuirks (step 16) параметризуют policy. Hybrid-outcome требует либо двойного прохода (выкручивая обратно policy чтобы переприменить), либо apply quirk до outcome (рассогласование с другими unit'ами в той же sim'е).
- Adaptation (LastStand) сейчас работает через rescore intent column. С чистой раздельностью adaptation = swap policy module (first-class).

**2. AoE `expected_damage` — двусмысленная семантика в одном поле.** Single-target: `compute_score_core` output. AoE: sim raw total. Scorer для AoE игнорирует outcome.expected_damage и пересчитывает через `compute_aoe_damage` со своей формулой. Поле populated, но dead для AoE — ещё одно «populated but unused», только условно.

**3. `compute_score_core` остаётся public с legacy названием.** Helper нужен (5 legitimate callers), но имя/docstring tell legacy story.

**4. Dead/orphaned fields.** 4 поля dead weight в каждой сериализованной outcome-записи.

**5. Outcome populator живёт в planner.** Скрытое legacy: outcome добавлялся инкрементально в generator, никогда не выносился.

**6. Цепочка sim → outcome → factor смазана.** `compute_offensive` и `build_step_outcome_estimate` оба знают про `compute_score_core`, оба applyмют part of policy. Single-source-of-truth для policy formulas нет.

### Что закрывает финализация (волна 2)

**1. Outcome = строгий fact vector.** Никакого policy weighting в populator'е. Любая `× progress` / `× urgency` / `× horizon` / `× (1 + raw/max_hp)` — это policy, живёт в отдельном модуле.

**2. Policy module top-level.** `combat::ai::policy::{damage, heal, friendly_fire, status, cc}` — каждая policy — pure function `fn(facts, target, caster) -> f32`. Читается factors / critics / terminal / intent / agenda.

**3. AoE проблема исчезает естественно.** Outcome содержит `enemy_damage` + `ally_damage` + `self_damage` (все raw facts, populated через walk по area). Damage factor: `damage::value(enemy_damage, ...) - friendly_fire::penalty(ally_damage, ...) - friendly_fire::penalty(self_damage, ...)`. `compute_aoe_damage` исчезает.

**4. Per-entity damage breakdown.** `enemy_damage_per_entity: Vec<(Entity, f32)>` + `ally_damage_per_entity: Vec<(Entity, f32)>` — для AoE; пустые для single-target. Открывают дверь к step 10 critics типа «high-priority target was damaged X».

**5. Move steps как first-class в outcome.** `path_max_danger`, `mp_spent` — facts для Move; `enemy_damage` / `cc_turns_applied` etc = 0 для Move. Один outcome shape для Cast и Move.

**6. Resource facts split.** Вместо `resource_swing: f32` — `ap_spent`, `mana_spent`, `rage_spent`, `other_resource_spent` (i32 each). Чище для critics («rare resource for low impact»).

**7. Status facts aggregated.** `cc_turns_applied`, `vulnerability_applied`, `armor_shred_applied` — Σ соответствующих status applications. Per-status / per-entity status breakdown — backlog (когда step 10 critics потребуют).

**8. Outcome populator → собственный модуль.** `combat::ai::outcome::builder::{from_sim_step, hypothetical}`. Generator вызывает, не владеет. Hypothetical path (`estimate_hypothetical`) ratify'ed как first-class parallel API.

**9. `compute_score_core` исчезает.** Распределяется в named policies: `policy::damage::value`, `policy::heal::value`, `policy::status::value`. Каждая — pure функция в своём файле.

**10. Schema v27 → v28 — clean break.** Outcome shape change — fundamental, не прячется за общим bump'ом. v27 logs дают понятную ошибку «schema v27 unsupported, v28+ required». Step 8 потом делает свой bump v28 → v29 для factors/modifiers structure.

**11. Phased migration через 8 сабшагов.** Каждый self-contained и revertable. Property tests ловят formula drift раньше, чем golden replay (golden только observable behaviour change).

### Что НЕ в scope волны 2

- **StepFactor / PlanFactor / TerminalFactor decomposition (step 8)** — outcome переоформляется, но factor pipeline остаётся монолитом в `compute_plan_factors_sans_intent` / `finalize_scores`. Decomposition — step 8.
- **Critics decomposition (step 10)** — критиков ещё нет. Outcome готовит почву (per-entity damage, status breakdown), но реализация critics — step 10.
- **Bands+agenda+scorecard (step 11)** — multi-policy evaluation становится возможной благодаря fact/policy split, но реализация — step 11.
- **Geometry awareness (step 17)** — `gained_los`, `entered_cast_arc` etc. — добавляются в outcome когда step 17 наступит. Сейчас не добавляем placeholder поля.
- **UnitQuirks (step 16)** — параметризация policy formulas остаётся на потом. Policies сейчас — plain functions, не trait. Trait добавится когда понадобится swap.
- **Расширение sim StepOutcome** — если 4.6 audit найдёт критические gaps, минимальные расширения sim делаются в 4.7. Крупные изменения sim — step 12 (mid-plan reflow).
- **Ranking-based weight tuning (B2)** — outcome facts открывают дверь, но offline harness — backlog.

### Зафиксированные решения по развилкам волны 2

**1. Outcome = facts only, policy = judgment** (вариант D из обсуждения).

Re-foundation, не косметика. Причины:
- `compute_score_core` остаётся public при любом варианте (нужен outcome producers); rename + docstring (B+) сами по себе fix'ят легаси-вид без архитектурного выигрыша.
- AoE ally splash в outcome (вариант A) добавляет 1 поле ради 1 use case, не меняя hybrid-философию.
- Step 10/11 на этом фундаменте, дешевле сделать раз сейчас, чем перешивать позже.

**Альтернатива (B+):** rename + status quo + комментарии. Отвергнута: cosmetic over architecture, не закрывает hybrid.

**Альтернатива (A):** добавить `ally_splash_damage` поле. Отвергнута: solves косметику AoE ценой новой specific-case ad-hoc.

**2. Per-entity damage breakdown — в scope 4.8** (Q-new2 confirmed).

`enemy_damage_per_entity: Vec<(Entity, f32)>` + `ally_damage_per_entity: Vec<(Entity, f32)>` — populated в 4.8 сразу, не backlog. Step 10 critics получают готовую инфраструктуру.

**3. Per-status / per-entity status breakdown — backlog.**

`cc_turns_applied: f32` — Σ skips_turn × duration, не Vec. Per-status/per-entity breakdown добавится когда step 10 critics типа `BuffIntoVoid` потребуют. Aggregated facts достаточны для всех текущих consumer'ов.

**4. Policy module top-level, не подкаталог factors.**

`src/combat/ai/policy/` — на одном уровне с `outcome`, `factors`, `intent`. Не `factors/policies/`. Critics / terminal / intent / agenda читают одну библиотеку policies.

**5. Schema bump v27 → v28 в step 4.12 — clean break.**

Outcome shape — fundamental data; bump не прячется за общим bump'ом step 8. v27 logs дают понятную ошибку. Step 8 потом v28 → v29. Двойной bump, оба clean break, оба self-contained — согласованно с проектным «без legacy».

**Альтернатива:** один общий bump в step 8.7. Отвергнута: outcome поля удаляются в step 4, mining tools будут читать v27 logs где удалённые поля присутствуют — нужны два code paths до step 8.7. Это и есть legacy.

**6. Phased migration: additive → consumer migration → drop legacy.**

Не big-bang. Сабшаги 4.7 (policy scaffold, no behavior change) → 4.8 (outcome additive: new + old) → 4.9 (builder relocation) → 4.10 (Cast consumers) → 4.11 (non-Cast consumers) → 4.12 (drop legacy + schema bump). Каждый сабшаг golden 0/N, кроме 4.12.

**7. Property tests как gate миграции — scenario-based** (Q-new1 confirmed).

Property tests «new policy(facts) == old compute_score_core(raw)» используют **реальные `(ability, target, caster)` тройки из ai_scenarios fixtures как seeds**, не random inputs. Catch'ат missed corner cases на боевых конфигурациях.

**8. Hypothetical outcome path как first-class API.**

`outcome::builder::hypothetical(ability, target, caster_ctx)` — parallel путь к `from_sim_step` для consumer'ов без sim (`future_value::λ_attack`, `picker::record_committed_reservations`). Ratify'ed как first-class, не «estimate_hypothetical legacy helper».

**9. Policy contract: `fn(outcome, target, caster) -> f32`.**

Не все policies pure от outcome facts — `damage_value(raw, target_hp_pct)` нуждается в target HP. Policy = pure function от outcome + minimal context (target/caster snapshots). Не keeps state. Stateless, swappable (для quirks/adaptation в будущем).

**10. Resource fields split на типизированные.**

Вместо `resource_swing: f32` — `ap_spent: i32`, `mana_spent: i32`, `rage_spent: i32`, `other_resource_spent: i32`. Чище для step 10 critics; убирает «sum of mixed-currency» проблему.

**11. Float `p_kill_*` сохраняем для forward-compat.**

Сейчас `p_kill_now` / `p_kill_soon` — binary 0.0/1.0. Naming `p_*` suggests probability. Сохраняем float для forward-compat (probabilistic AI с dice variance). Docstring «1.0 if guaranteed, 0.0 if not, future may carry probabilities».

### Природа gate'ов в волне 2

В отличие от step 6 (gate'ы на behavioral metrics), волна 2 — re-foundation с инвариантом «behavior unchanged». Gate'ы:

- **4.6** — doc reviewable (no code).
- **4.7** — property tests «new policy == old compute_score_core» pass; `cargo test/clippy/build/ai_scenarios` зелёные. Golden 0/N (functions delegate, no behavior change).
- **4.8** — outcome additive (new fields рядом с old, populator заполняет оба). Golden 0/N.
- **4.9** — builder relocation, формул не трогаем. Golden 0/N.
- **4.10** — Cast consumer migration. Golden 0/N (formulas equivalent через property test'овую гарантию).
- **4.11** — non-Cast consumer audit. Golden 0/N.
- **4.12** — schema clean break v27 → v28. Round-trip 0/N в новом формате; v27 logs дают clean error; mining baseline воспроизводится на v28 corpus.
- **4.13** — doc reviewable.

## Сабшаги волны 2

### 4.6. Audit retrospective + sim alignment review + plan doc

**Scope.**

Этот раздел (волна 2 в `docs/ai_rework_step4_plan.md`) — артефакт сабшага 4.6. Содержит:

1. **Preamble волны 2** — текущее состояние после волны 1, проблемы, что закрывается, scope, развилки, gate'ы (см. выше).
2. **Sim StepOutcome alignment review** — сверка, что `sim::StepOutcome` даёт всё необходимое для new outcome facts. Анализ:
   - `StepOutcome.damage: f32` — сейчас sim возвращает aggregated damage. Для per-entity breakdown нужен `damage_per_entity: HashMap<Entity, f32>` или builder делает walk сам через apply_step side effects.
   - `StepOutcome.killed: Vec<Entity>` — достаточно для `p_kill_now`.
   - `StepOutcome.applied_statuses` (если есть) — нужен для `cc_turns_applied` / `vulnerability_applied` / `armor_shred_applied`. Если отсутствует — builder walk'ает def + target snapshot.
   - **Gap policy:** если sim не даёт нужных facts, расширяем sim (add fields в `StepOutcome`); если builder может derive из (def, target, snap), оставляем builder-side.
3. **Сабшаги 4.7–4.13** с scope/тестами/gate/эстимейтами.
4. **Зафиксированные решения** + критические файлы + ожидаемые сдвиги + что откладывается + чего не делать.

**Side-effect:** аудит шагов 4.0–4.5 (выполнен через Explore агента) запротоколирован в Preamble волны 2 выше. Active/placeholder/orphaned classification фиксируется как baseline для последующих сабшагов.

**Gate.** Doc reviewable. No code.

**Эстимейт:** 0.5 дня.

---

### 4.7. Policy module scaffolding + property tests

**Scope.**

Создать `src/combat/ai/policy/` с extracted formulas. Старый `compute_score_core` становится thin delegator. Behavior-preserving refactor.

**Файловая структура:**

```
src/combat/ai/policy/
├── mod.rs               // re-exports + module-level docstring (facts vs policy contract)
├── damage.rs            // damage::value(raw, target_hp_pct) -> f32
├── heal.rs              // heal::value(restored_hp, target_max_hp, target_hp, danger, horizon_sum) -> f32
├── friendly_fire.rs     // friendly_fire::penalty(raw_dmg, max_hp) -> f32
├── status.rs            // status::stun_denial_value(target, status_def) -> f32
│                        // status::vulnerability_value(damage_taken_bonus, duration) -> f32
│                        // status::armor_shred_value(armor_bonus, duration) -> f32
└── cc.rs                // cc::value(cc_turns, vulnerability, armor_shred) -> f32 (composite)
```

**Контракт policy:**

```rust
//! src/combat/ai/policy/mod.rs
//!
//! Policy formulas — value judgments applied to ActionOutcomeEstimate facts.
//!
//! Invariants:
//! - Each policy is a pure function of (facts, minimal context).
//! - No side effects, no shared state, no caching.
//! - Signature: `fn name(facts, [target: &UnitSnapshot], [caster: &UnitSnapshot]) -> f32`.
//! - Policies are stateless and swappable (forward-compat for UnitQuirks / adaptation).
//!
//! Read by: factors (StepFactor / PlanFactor), critics (step 10), terminal eval,
//! intent scoring, agenda scorecards (step 11). Single source of truth for "how
//! we value this fact."
```

**Extraction mapping (что куда переезжает):**

| Source | Target | Notes |
|---|---|---|
| `compute_score_core` damage branch (`raw × (0.5 + 0.5 × progress)`) | `policy::damage::value(raw, target_hp_pct)` | Pure function; extracted formula 1:1 |
| `compute_score_core` heal branch (`delta_pct × horizon × urgency`) | `policy::heal::value(restored_hp, max_hp, target_hp, danger, horizon_sum)` | Все аргументы explicit; horizon_sum compute'ится в caller'е (`Σ damage_horizon`) |
| `factors::offensive::friendly_fire_penalty` (`raw × (1 + raw/max_hp)`) | `policy::friendly_fire::penalty(raw_dmg, max_hp)` | Перенос 1:1 |
| `scoring::stun_denial_value` | `policy::status::stun_denial_value` | Перенос 1:1 |
| `scoring::status_score` (composite) | `policy::status::value` (composite) | Перенос 1:1 |

**Old `compute_score_core` остаётся** thin delegator до 4.12. Все callers (`offensive::compute_offensive`, `offensive::friendly_fire_penalty`, `outcome::estimate_*`) **не меняются** в 4.7 — продолжают звать `compute_score_core`. Migration на direct `policy::*` calls — в 4.10.

**Property tests** (новый `src/combat/ai/policy/tests.rs` или `mod tests` в каждом policy file):

```rust
/// Scenario-based property test: для каждого (ability, target, caster) tuple
/// из ai_scenarios fixtures, new policy formula даёт bit-identical результат
/// со старым compute_score_core путём.
#[test]
fn policy_damage_matches_legacy_for_all_scenarios() {
    for case in ai_scenarios_fixtures() {
        for (ability, target, caster) in extract_cast_combinations(&case) {
            let legacy = legacy_compute_score_core_pre_4_7(&ability, &target, &caster);
            let new = via_policy_module(&ability, &target, &caster);
            assert!((legacy - new).abs() < 1e-6, "drift in {}: legacy={} new={}", case.name, legacy, new);
        }
    }
}
```

Дополнительно — random inputs property tests (smaller scope, для catch'а missed sim corner cases): bit-identical через 1000 random `(ability, target, caster)` triples.

**Юнит-тесты:**

- `policy::damage::value` — `raw=0 → 0`, `raw>0 → raw × (0.5 + 0.5 × progress)`, monotonic в raw для fixed target_hp_pct.
- `policy::heal::value` — `restored=0 → 0`, `urgency` правильно учитывает hp_missing vs danger.
- `policy::friendly_fire::penalty` — `raw=0 → 0`, monotonic + super-linear в raw/max_hp.
- `policy::status::*` — extracted formulas 1:1 с pre-4.7 поведением.
- `compute_score_core` round-trip: для синтетических (ability, target, caster) — old behavior == new delegator.
- `policy_damage_matches_legacy_for_all_scenarios` — главный gate.

**Gate.**

- Property tests pass на всех ai_scenarios fixtures.
- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (functions delegate, нет behavior change).

**Эстимейт:** 1.5 дня.

---

### 4.8. Outcome fact vector — additive

**Scope.**

Добавить новые fact-поля в `ActionOutcomeEstimate` рядом со старыми. Populator заполняет **оба** набора. Old fields остаются, но помечены `// LEGACY (drop в 4.12)`.

**Новый `ActionOutcomeEstimate`** (`src/combat/ai/outcome.rs`):

```rust
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    // ── Damage facts (raw, populated by sim or hypothetical) ──
    /// Raw damage to all enemies (sum); 0 for Move steps.
    pub enemy_damage: f32,
    /// Per-entity enemy damage breakdown; empty for single-target Cast (use enemy_damage),
    /// populated for AoE Cast. Открывает дверь к step 10 critics.
    pub enemy_damage_per_entity: Vec<(Entity, f32)>,
    /// Raw damage to allies (AoE friendly fire); 0 for single-target / Move.
    pub ally_damage: f32,
    /// Per-entity ally damage breakdown; empty for single-target / Move.
    pub ally_damage_per_entity: Vec<(Entity, f32)>,
    /// Raw damage to actor (AoE self-hit, lifesteal cost); 0 otherwise.
    pub self_damage: f32,

    // ── Kill facts ──
    /// 1.0 if step killed ≥1 enemy this turn. Float reserved для forward-compat
    /// (probabilistic AI с dice variance).
    pub p_kill_now: f32,
    /// 1.0 if direct + dot will kill within damage horizon, else 0.0.
    pub p_kill_soon: f32,

    // ── Status / control facts (aggregated; per-status breakdown — backlog) ──
    /// Σ (skips_turn × duration) over enemies hit.
    pub cc_turns_applied: f32,
    /// Σ (damage_taken_bonus × duration) over enemies hit.
    pub vulnerability_applied: f32,
    /// Σ (armor_bonus × duration) over enemies hit.
    pub armor_shred_applied: f32,

    // ── Support facts ──
    /// Raw HP healed (clamped к missing HP); 0 для не-heal abilities.
    pub hp_restored: f32,

    // ── Movement facts (Move steps; 0 для Cast) ──
    /// Worst danger value along Move path (max over path tiles).
    pub path_max_danger: f32,
    /// Movement points consumed by this Move step.
    pub mp_spent: i32,

    // ── Resource facts ──
    pub ap_spent: i32,
    pub mana_spent: i32,
    pub rage_spent: i32,
    pub other_resource_spent: i32,

    // ── LEGACY (drop в 4.12 после consumer migration) ──
    #[deprecated(note = "Use enemy_damage / ally_damage / hp_restored. Drop in 4.12.")]
    pub expected_damage: f32,
    #[deprecated(note = "Use cc_value(cc_turns_applied, vulnerability_applied, armor_shred_applied). Drop in 4.12.")]
    pub deny_value: f32,
    #[deprecated(note = "Use heal_value(hp_restored, ...). Drop in 4.12.")]
    pub rescue_value: f32,
    #[deprecated(note = "Dead placeholder (никогда не заполнялось). Drop in 4.12.")]
    pub board_pressure: f32,
    #[deprecated(note = "Reserved для step 17 (geometry); добавится когда нужно. Drop in 4.12.")]
    pub geometry_gain: f32,
    #[deprecated(note = "Replaced by path_max_danger. Drop in 4.12.")]
    pub exposure_delta: f32,
    #[deprecated(note = "Replaced by ap_spent / mana_spent / rage_spent / other_resource_spent. Drop in 4.12.")]
    pub resource_swing: f32,
}
```

**Populator update** (`build_step_outcome_estimate` в `generator.rs:219`) — заполняет оба набора:

- Cast branch: walk по AoE area для damage breakdown (enemies/allies/self separately + per-entity); aggregate status applications; clamped heal restored; split resources by ResourceKind. Параллельно — fill старых `expected_damage` / `deny_value` / `rescue_value` / `resource_swing` (как было).
- Move branch: `path_max_danger` + `mp_spent`. Параллельно — fill старых `exposure_delta` + `resource_swing`.

**Helpers (новые в outcome populator, перенесутся в 4.9):**
- `build_damage_facts(...)` — walk по AoE area, separates enemies/allies/self, per-entity breakdown.
- `aoe_p_kill_soon(...)` — `1.0` if any enemy в area returns `estimate_kill_soon == 1.0`.
- `build_status_facts(...)` — walk applied statuses, aggregates cc_turns / vuln / shred.
- `estimate_hp_restored(...)` — heal-clamped restored HP (raw fact, не policy).
- `split_resource_costs(...)` — итерирует def.costs, разделяет по ResourceKind.

**Юнит-тесты** (в `outcome.rs::tests` или `outcome/builder/tests.rs`):

- `enemy_damage_matches_sim_for_single_target` — для damage Cast, `outcome.enemy_damage == sim damage`.
- `enemy_damage_per_entity_populated_for_aoe` — AoE Cast, `len(per_entity) == enemies in area`.
- `ally_damage_zero_for_single_target` — single-target damage не affects allies.
- `ally_damage_populated_for_aoe_friendly_fire` — AoE с allies в area, `ally_damage > 0`.
- `cc_turns_applied_for_stun_ability` — stun ability на enemy, `cc_turns_applied == stun_duration`.
- `hp_restored_clamped_to_missing_hp` — heal на full-HP target, `hp_restored == 0`; на 50% target, `hp_restored == min(expected, missing)`.
- `path_max_danger_for_move` — Move через danger tiles, `path_max_danger == max(danger over path)`.
- `mp_spent_equals_path_len` — Move, `mp_spent == path.len()`.
- `resource_facts_split_by_kind` — Cast с mana cost, `mana_spent > 0` & `rage_spent == 0`.
- **Legacy parity** — для каждой old field, `outcome.expected_damage == old behavior`, `outcome.deny_value == old`, `outcome.rescue_value == old`, etc. Pin invariant что 4.8 — additive.

**Gate.**

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (additive — old fields populated identical к pre-4.8; new fields populated, но никем не читаются).
- Property tests из 4.7 продолжают проходить.

**Эстимейт:** 1.0 день.

---

### 4.9. Builder relocation: `outcome::builder` module

**Scope.**

Вынести outcome populator из `generator.rs` в собственный модуль. Никаких формульных изменений.

**Файловая структура:**

```
src/combat/ai/outcome/
├── mod.rs       // ActionOutcomeEstimate, PlanAnnotation, ViabilityResult, AdaptationData,
│                //   ContractMaskHit, PickInfo (тип-определения)
└── builder.rs   // from_sim_step, hypothetical, build_damage_facts, build_status_facts,
                 //   estimate_kill_soon, estimate_deny_value, estimate_rescue_value,
                 //   estimate_expected_damage, estimate_hp_restored, split_resource_costs,
                 //   step_path_danger
```

**Public API:**

```rust
// src/combat/ai/outcome/builder.rs

/// Builds outcome estimate from sim step result.
/// Used by generator's beam search after each `apply_step`.
pub fn from_sim_step(
    step: &PlanStep,
    sim_outcome: &StepOutcome,
    pre_snap: &BattleSnapshot,
    caster_ctx: &CasterContext,
    crit_fail_effect: &CritFailEffect,
    ctx: &AiWorld,
    maps: &InfluenceMaps,
    caster_tile: Hex,
    actor_team: Team,
) -> ActionOutcomeEstimate {
    // Body = бывший `build_step_outcome_estimate` из generator.rs.
}

/// Builds outcome estimate without sim — for consumers без sim context
/// (`future_value::λ_attack`, `picker::record_committed_reservations`).
///
/// First-class parallel API к `from_sim_step`. Same outcome shape; precision
/// is hypothetical (no sim verification — все поля derived из ability def + target).
pub fn hypothetical(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster_ctx: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
) -> ActionOutcomeEstimate {
    // Body = бывший `estimate_hypothetical` из outcome.rs:244.
}
```

**Migration:**

- `build_step_outcome_estimate` — удалён из `generator.rs:219`. `generator.rs:169` зовёт `outcome::builder::from_sim_step(...)`.
- `estimate_hypothetical` — удалён из `outcome.rs:244`. Callers (`future_value.rs`, `picker.rs`) зовут `outcome::builder::hypothetical(...)`.
- `estimate_*` helpers (`estimate_kill_soon`, `estimate_deny_value`, `estimate_rescue_value`, `estimate_expected_damage`) — переезжают в `outcome::builder` как private helpers (потеряют `pub`).
- `step_path_danger` — переезжает в `outcome::builder` (private).

**`outcome.rs` после relocation** содержит только type definitions (~150 строк вместо 696).

**Юнит-тесты:** existing tests из `outcome.rs::tests` переезжают в `outcome/builder.rs::tests`. Никаких новых.

**Gate.**

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (relocation, no behavior change).

**Эстимейт:** 0.5 дня.

---

### 4.10. Consumer migration — Cast scoring path

**Scope.**

Переписать `compute_offensive` (`src/combat/ai/factors/offensive.rs:31`) как pure outcome reader + policy applier. AoE проблема исчезает — `damage::value(enemy_damage_per_entity) - friendly_fire::penalty(ally_damage_per_entity) - friendly_fire::penalty(self_damage)`.

**Новый `compute_offensive`:**

```rust
pub(super) fn compute_offensive(
    ability: &AbilityId,
    _target_pos: Hex,
    target: Entity,
    _caster_tile: Hex,
    ctx: &ScoringCtx,
    outcome: &ActionOutcomeEstimate,
) -> OffensiveFactors {
    use crate::combat::ai::policy;

    let content = ctx.world.content;
    let Some(def) = content.abilities.get(ability) else {
        return OffensiveFactors::default();
    };
    if matches!(def.effect, EffectDef::Summon { .. }) {
        return OffensiveFactors::default();
    }

    let snap = ctx.snap;
    let active = ctx.active;

    // ── Damage: facts → policies ──
    // Single-target damage: target HP context для damage::value progression.
    // AoE: для каждого enemy в per_entity breakdown, apply damage::value с per-target HP.
    let enemy_damage_value = if outcome.enemy_damage_per_entity.is_empty() {
        // Single-target.
        snap.unit(target).map_or(0.0, |t| {
            let hp_pct = t.hp as f32 / t.max_hp.max(1) as f32;
            policy::damage::value(outcome.enemy_damage, hp_pct)
        })
    } else {
        // AoE: per-entity policy application — captures per-target progression.
        outcome.enemy_damage_per_entity.iter().map(|(e, dmg)| {
            snap.unit(*e).map_or(0.0, |t| {
                let hp_pct = t.hp as f32 / t.max_hp.max(1) as f32;
                policy::damage::value(*dmg, hp_pct)
            })
        }).sum()
    };

    let ally_penalty: f32 = outcome.ally_damage_per_entity.iter().map(|(e, dmg)| {
        snap.unit(*e).map_or(0.0, |t| policy::friendly_fire::penalty(*dmg, t.max_hp))
    }).sum();

    let self_penalty = if outcome.self_damage > 0.0 {
        policy::friendly_fire::penalty(outcome.self_damage, active.max_hp)
    } else { 0.0 };

    let damage_raw = enemy_damage_value - ally_penalty - self_penalty;
    let damage = crit_fail_adjusted(damage_raw, def, &active.crit_fail_effect, ctx.world.crit_fail_chance);

    // ── Heal: facts → policy ──
    let heal = if outcome.hp_restored > 0.0 {
        snap.unit(target).map_or(0.0, |t| {
            let danger = ctx.maps.danger.get(t.pos);
            let horizon_sum: f32 = t.damage_horizon.iter().sum::<f32>().max(t.threat);
            let raw = policy::heal::value(outcome.hp_restored, t.max_hp, t.hp, danger, horizon_sum);
            crit_fail_adjusted(raw, def, &active.crit_fail_effect, ctx.world.crit_fail_chance)
        })
    } else { 0.0 };

    // ── CC: facts → policy ──
    let cc = policy::cc::value(
        outcome.cc_turns_applied,
        outcome.vulnerability_applied,
        outcome.armor_shred_applied,
    );

    // ── Kill signals: pure facts ──
    let kill_now = outcome.p_kill_now;
    let kill_promised = outcome.p_kill_soon;

    OffensiveFactors { damage, heal, kill_now, kill_promised, cc }
}
```

**Что удаляется в этом сабшаге:**
- `factors::offensive::compute_aoe_damage` — больше не нужна (per-entity policy в compute_offensive).
- `factors::offensive::friendly_fire_penalty` — переехал в `policy::friendly_fire::penalty`.
- AoE branch в old compute_offensive (lines 64–78).

**Что остаётся** (до 4.12):
- `outcome::compute_score_core` — всё ещё нужен в `outcome::builder` для `from_sim_step` populating legacy `expected_damage` / `deny_value` / `rescue_value`.

**Test rewrites:**

- `compute_offensive_reads_outcome_not_score_action` → `compute_offensive_reads_facts_and_applies_policy`. Расширяется на spot-check всех новых facts: synth outcome с known `enemy_damage`/`ally_damage`/`self_damage`/`hp_restored`/`cc_turns_applied`/`vulnerability_applied`/`armor_shred_applied`/`p_kill_now`/`p_kill_soon` → assert returned `OffensiveFactors` корректно applies policies.
- `compute_offensive_aoe_per_entity_progression` — synth AoE outcome с per-entity damage breakdown на enemies с разным HP → assert damage applies policy per-target (high-HP target damage worth less than equivalent damage to low-HP target).
- `compute_offensive_friendly_fire_super_linear` — synth AoE outcome с ally_damage → assert penalty super-linear в raw_dmg/max_hp.

**Gate.**

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Property tests из 4.7 проходят.
- Golden 0/N (formulas equivalent через property test'овую гарантию из 4.7).

**Эстимейт:** 1.5 дня.

---

### 4.11. Consumer migration — non-Cast paths + audit

**Scope.**

Audit и migrate всех остальных consumer'ов outcome на new fact fields. Большинство — без изменений (читают `p_kill_now`/`p_kill_soon`, новые имена не нужны).

**Per-consumer audit:**

| Consumer | File:Line | Reads outcome | Action в 4.11 |
|---|---|---|---|
| `compute_offensive` | offensive.rs:31 | `expected_damage`, `p_kill_now`, `p_kill_soon`, `deny_value`, `rescue_value` | Migrated в 4.10 |
| `terminal::compute_secure_kill` | planning/terminal.rs:121 | `p_kill_now + 0.5 × p_kill_soon` | **No change** — facts остались |
| `repair::extract_goal_context` | repair/goal.rs:326 | `p_kill_now` (sum для Finish vs Pressure классификации) | **No change** — fact остался |
| `intent_score` Cast branches | intent.rs:919+ | через `compute_factors` → `compute_offensive` | **No change** — transitive, через factors pipeline |
| `compute_plan_factors_sans_intent` | planning/scorer.rs:510 | через `factors::compute_factors` (передаёт outcome) | **No change** — transitive |
| `compute_plan_intent_sum` | planning/scorer.rs:632 | `intent_score` через outcome | **No change** — transitive |
| `factors::compute_factors` | factors/mod.rs:241 | передаёт outcome в compute_offensive | **No change** — pass-through |
| `trade::*` | trade.rs | НЕ читает outcome | **No change** — actor valuation |
| `terminal::compute_*` (кроме secure_kill) | planning/terminal.rs | НЕ читает outcome | **No change** — end-state metrics |

**Net effect 4.11 — `audit only`:** все non-Cast consumers уже работают на facts (`p_kill_now`/`p_kill_soon`), их семантика не меняется. Сабшаг — verification + ratify в комментариях, что эти reader'ы — fact readers, не policy readers.

**Дополнительно ratify в `outcome.rs` docstring'е** список «authoritative consumers»:

```rust
//! ## Consumers
//! - `factors::offensive::compute_offensive` — primary scoring consumer (4.10).
//! - `planning::terminal::compute_secure_kill` — reads p_kill_now / p_kill_soon.
//! - `repair::goal::extract_goal_context` — reads p_kill_now для Finish/Pressure классификации.
//! - `policy::*` — applied к outcome facts в factor computation.
//!
//! Non-consumers (NOT applicable, не баг):
//! - `trade::*` — actor valuation, не action outcome.
//! - `terminal::compute_*` (кроме secure_kill) — end-state metrics from snapshot/maps.
//! - `intent_score` non-Cast branches (Reposition / ProtectAlly / SetupAOE / LastStand) —
//!   position/ability-type logic, не applicable.
```

**Юнит-тесты:** existing terminal/repair tests должны продолжать pass без изменений (ratify через cargo test).

**Gate.**

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (no behavior change — все fact reads stable).
- Audit table в plan doc updated.

**Эстимейт:** 0.5 дня.

---

### 4.12. Drop legacy + schema clean break v27 → v28

**Scope.**

Удалить legacy fields из `ActionOutcomeEstimate`. Удалить `compute_score_core` (всё в policy). Schema bump v27 → v28 — clean break без backward read. Mining/replay rewrite. Rebuild golden + 1 fixture.

**1. Удалить legacy outcome fields:**

```rust
// src/combat/ai/outcome.rs (после 4.12)
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    // facts only — full list см. 4.8 minus `// LEGACY` block
    pub enemy_damage: f32,
    pub enemy_damage_per_entity: Vec<(Entity, f32)>,
    pub ally_damage: f32,
    pub ally_damage_per_entity: Vec<(Entity, f32)>,
    pub self_damage: f32,
    pub p_kill_now: f32,
    pub p_kill_soon: f32,
    pub cc_turns_applied: f32,
    pub vulnerability_applied: f32,
    pub armor_shred_applied: f32,
    pub hp_restored: f32,
    pub path_max_danger: f32,
    pub mp_spent: i32,
    pub ap_spent: i32,
    pub mana_spent: i32,
    pub rage_spent: i32,
    pub other_resource_spent: i32,
    // Removed: expected_damage, deny_value, rescue_value, board_pressure,
    //          geometry_gain, exposure_delta, resource_swing.
}
```

**2. Удалить `compute_score_core` из `outcome.rs`:**

После 4.10–4.11 единственные callers — `outcome::builder::estimate_*` helpers (для legacy field population). После удаления legacy fields в 4.12 — не нужны.

Estimate helpers tail:
- `estimate_expected_damage` — удалён.
- `estimate_rescue_value` — удалён.
- `estimate_deny_value` — удалён.
- `estimate_kill_soon` — **остаётся** (используется в `from_sim_step` для `p_kill_soon` populating).
- `compute_score_core` — удалён.

**3. `from_sim_step` упрощается** (нет больше legacy populating) — facts only.

**4. `hypothetical` упрощается** — без `compute_score_core`. Walk def + target — populate facts напрямую без policy weighting.

**Note:** consumers `hypothetical` (`future_value::λ_attack`, `picker::record_committed_reservations`) используют outcome fields через **policy** для derive ценности. `future_value::λ_attack` сейчас делает `0.5 × estimate_hypothetical(...).expected_damage`. После 4.12: `0.5 × policy::damage::value(outcome.enemy_damage, target_hp_pct)`.

**5. Schema clean break v27 → v28** (`src/combat/ai/log.rs`):

```rust
pub const SCHEMA_VERSION: u32 = 28;

// Read v27 → понятная ошибка (по примеру step 7.5 clean break).
pub fn parse_actor_tick(line: &str) -> Result<ActorTickEvent, LogError> {
    let header: SchemaHeader = serde_json::from_str(line)?;
    if header.schema_version < SCHEMA_VERSION {
        return Err(LogError::UnsupportedSchema {
            found: header.schema_version,
            required: SCHEMA_VERSION,
            hint: "v27 outcome shape replaced in step 4.12; rebuild logs from v28+ playtest",
        });
    }
    serde_json::from_str(line).map_err(LogError::from)
}
```

**6. `mine_ai_logs.rs` rewrite:**

- Reads только v28 events.
- Per-step outcome секция (если bump'нута) — пересчитывается под new fact fields.
- Выводит `enemy_damage` / `ally_damage` / `self_damage` / `cc_turns_applied` распределения по корпусу.

**7. `replay_ai_log.rs` rewrite:**

- Reads только v28 events.
- На каждом event'е восстанавливает state, сравнивает re-pick decision с logged.
- `--capture-golden` пишет golden in v28 формате.

**8. Rebuild artifacts:**

- Свежий v28 playtest (как в step 7.5: 6 файлов 6-mix corpus).
- `replay_ai_log --capture-golden` → новый `logs/golden_post_step4.jsonl`.
- `tests/ai_scenarios/snapshots/continuation_relevant_preserved/` — пересоздаётся из свежего v28 entry.

**Юнит-тесты:**

- `actor_tick_v28_round_trips` — write → read → equal.
- `parse_v27_returns_unsupported_schema_error` — clean error, не panic.
- `outcome_facts_only_no_legacy_fields` — `ActionOutcomeEstimate` имеет ровно 17 полей, ни одного `expected_damage`/`deny_value`/etc.
- `compute_score_core_does_not_exist` — compile-time gate (file doesn't compile if хвост ссылается).
- `from_sim_step_populates_only_facts` — no calls to policy module from builder (audit через grep gate).

**Gate.**

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- v28 round-trip 0/N на свежем playtest'е.
- v27 logs дают `LogError::UnsupportedSchema`, не panic.
- Mining v28 corpus отрабатывает; baseline (post-7.5) воспроизводится в новом формате с new fact distributions.
- Per-entry golden review — допустимо ≤10/N diverged для tie-breaking edge cases. >10 — расследовать.

**Эстимейт:** 1.5 дня.

---

### 4.13. Documentation finalization

**Scope.**

1. **Extensive docstring** на `src/combat/ai/outcome/mod.rs` фиксирующий contract:

```rust
//! ActionOutcomeEstimate — facts about what a plan step did.
//!
//! ## Contract
//!
//! Outcome contains **facts only** — raw numerical signals about the step's
//! effect on the board. No policy weighting, no value judgment, no progression
//! curves. Any `× progress` / `× urgency` / `× horizon` / `× (1 + raw/max_hp)` —
//! that's policy, lives в `combat::ai::policy`.
//!
//! ## Layered model
//!
//! ```text
//! sim::StepOutcome  →  outcome::builder  →  ActionOutcomeEstimate  →  policy + factors  →  score
//! (raw mechanics)     (structures facts)   (fact vector)             (judgment)            (number)
//! ```
//!
//! ## Invariants
//!
//! 1. Outcome population (in builder) MUST NOT call any function from `policy::*`.
//!    If you need to derive a value, derive it from raw mechanics, not from policy.
//! 2. Policy formulas MUST be pure functions of (outcome, target, caster).
//!    No state, no side effects, no caching beyond the call.
//! 3. Outcome MUST be the same shape for Cast and Move steps. Move-specific facts
//!    are 0 for Cast, vice versa.
//! 4. New mechanics extend outcome by adding fact fields. Не добавляйте policy fields.
//!
//! ## Consumers (authoritative list)
//!
//! ### Active fact readers
//! - `factors::offensive::compute_offensive` — primary scoring consumer.
//! - `planning::terminal::compute_secure_kill` — reads p_kill_now / p_kill_soon.
//! - `repair::goal::extract_goal_context` — reads p_kill_now для Finish/Pressure.
//! - `planning::future_value::λ_attack` — reads outcome from hypothetical path.
//! - `planning::picker::record_committed_reservations` — reads outcome from hypothetical path.
//!
//! ### Non-consumers (NOT applicable, не баг)
//! - `trade::*` — actor valuation, не action outcome.
//! - `terminal::compute_*` (кроме secure_kill) — end-state metrics from snapshot/maps.
//! - `intent_score` non-Cast branches (Reposition/ProtectAlly/SetupAOE/LastStand) —
//!   position/ability-type logic, не applicable.
```

2. **`docs/ai_rework.md` §4 — ✓ DONE marker** с реальными gates и ссылкой на этот план:

```markdown
### 4. `ActionOutcomeEstimate` — outcome vector до factors ✓ DONE

**Сложность:** 4
**Польза:** 5

**Цель:** ...

**Как реализовано** (декомпозиция: `docs/ai_rework_step4_plan.md`, сабшаги 4.0–4.13):

- Модуль `src/combat/ai/outcome/` со структурой `ActionOutcomeEstimate` (17 fact-полей)...
- Policy module `src/combat/ai/policy/` с named pure functions...
- ...
```

3. **`docs/ai.md` audit** — sweep mentions of outcome / scoring / factors на согласованность с new contract.

4. **`docs/ai_rework_step4_plan.md` (этот файл) updates:**
   - Каждый сабшаг волны 2 получает status marker: `done (commit hash)`.
   - «Что отложено» секция с финальным списком backlog'а (per-status breakdown, geometry_gain, etc).

**Gate.** Doc reviewable.

**Эстимейт:** 0.5 дня.

---

## Итого волны 2

| # | Шаг | Эстимейт | Gate | Статус |
|---|---|---|---|---|
| 4.6 | Audit retrospective + sim alignment review + plan doc | 0.5 | doc reviewable | в работе |
| 4.7 | Policy module scaffolding + property tests | 1.5 | property tests pass; golden 0/N | pending |
| 4.8 | Outcome fact vector — additive | 1.0 | golden 0/N (additive) | pending |
| 4.9 | Builder relocation: outcome::builder module | 0.5 | golden 0/N | pending |
| 4.10 | Consumer migration — Cast scoring path | 1.5 | golden 0/N | pending |
| 4.11 | Consumer migration — non-Cast paths + audit | 0.5 | golden 0/N (verification) | pending |
| 4.12 | Drop legacy + schema clean break v27 → v28 | 1.5 | v28 round-trip 0/N; clean break | pending |
| 4.13 | Documentation finalization | 0.5 | doc reviewable | pending |

**Суммарно ~7.5 дней** (0.5 + 1.5 + 1.0 + 0.5 + 1.5 + 0.5 + 1.5 + 0.5).

## Зафиксированные решения волны 2

### Architectural foundations

1. **Outcome = facts only** (вариант D из обсуждения). Никакого policy weighting в populator'е. Policy = отдельный named module.
2. **Policy module — top-level**, не подкаталог factors. Читается всеми (factors / critics / terminal / intent / agenda).
3. **AoE проблема через per-entity facts**. `enemy_damage_per_entity` + `ally_damage_per_entity` + `self_damage` — все raw facts; policy applies per-target.
4. **Outcome unified для Cast и Move** — single shape, не applicable fields = 0.0.
5. **Policy contract: `fn(outcome, target, caster) -> f32`** — pure function от outcome + minimal context. Stateless, swappable (forward-compat для quirks/adaptation).

### Migration strategy

6. **Phased migration**: 4.7 (policy scaffold, no behavior change) → 4.8 (additive outcome) → 4.9 (builder relocation) → 4.10–4.11 (consumer migration) → 4.12 (drop legacy + schema bump). Каждый сабшаг self-contained и revertable.
7. **Property tests как gate** — scenario-based на ai_scenarios fixtures + supplementary random inputs. Pin'ит formula equivalence раньше golden replay.
8. **Schema bump per re-foundation** — clean break v27 → v28 в 4.12. Step 8 потом v28 → v29 для factors/modifiers structure. Двойной bump, оба self-contained.

### Field-level decisions

9. **Per-entity damage breakdown — в scope 4.8** (Q-new2 confirmed). `enemy_damage_per_entity` + `ally_damage_per_entity` populated сразу.
10. **Status facts aggregated, per-status/per-entity backlog**. `cc_turns_applied: f32` aggregate; per-status breakdown добавится когда step 10 critics типа `BuffIntoVoid` потребуют.
11. **Resource fields split на типизированные**: `ap_spent: i32`, `mana_spent: i32`, `rage_spent: i32`, `other_resource_spent: i32`. Не `resource_swing: f32`.
12. **Float `p_kill_*` сохраняем для forward-compat** — probabilistic AI с dice variance в будущем. Docstring «1.0 if guaranteed, 0.0 if not».
13. **Удаляются полностью**: `expected_damage` (mixed semantics), `deny_value` (policy-aggregated), `rescue_value` (policy-applied), `board_pressure` (placeholder, никогда не fill'ался), `geometry_gain` (reserved для step 17 — добавится когда step 17), `exposure_delta` (replaced `path_max_danger`), `resource_swing` (replaced split fields).

### Builder strategy

14. **Outcome populator → собственный модуль**. `combat::ai::outcome::builder::{from_sim_step, hypothetical}`. Generator вызывает, не владеет.
15. **Hypothetical path как first-class API** — `outcome::builder::hypothetical(...)` parallel путь к `from_sim_step` для consumer'ов без sim.

### Отвергнутые альтернативы

- **B+ status quo + comments** — отвергнут: cosmetic over architecture, не закрывает hybrid.
- **A new field `ally_splash_damage`** — отвергнут: solves косметику AoE ценой новой specific-case ad-hoc, не меняет hybrid-философию.
- **Один общий schema bump в step 8.7** — отвергнут: outcome удаляется в step 4, mining tools читали бы v27 logs где удалённые поля присутствуют. Это и есть legacy.
- **Big-bang migration в одном сабшаге** — отвергнут: re-foundation масштаба требует phased для risk mitigation.
- **Per-status breakdown в outcome сразу** — отвергнут: aggregated facts достаточны для всех текущих consumer'ов; backlog для step 10.
- **Trait `Policy` для swap'а formul** — отвергнут на этом шаге: pure functions проще; trait добавится в step 16 (UnitQuirks) если понадобится swap.

## Критические файлы волны 2

### Новые

- `src/combat/ai/policy/mod.rs` — re-exports + module-level docstring (4.7).
- `src/combat/ai/policy/damage.rs` — `damage::value` (4.7).
- `src/combat/ai/policy/heal.rs` — `heal::value` (4.7).
- `src/combat/ai/policy/friendly_fire.rs` — `friendly_fire::penalty` (4.7).
- `src/combat/ai/policy/status.rs` — `status::stun_denial_value` / `vulnerability_value` / `armor_shred_value` (4.7).
- `src/combat/ai/policy/cc.rs` — `cc::value` (composite) (4.7).
- `src/combat/ai/outcome/mod.rs` — type definitions (после 4.9 split).
- `src/combat/ai/outcome/builder.rs` — `from_sim_step` + `hypothetical` + private helpers (4.9).

### Меняются существенно

- `src/combat/ai/outcome.rs` — split на `outcome/mod.rs` + `outcome/builder.rs` в 4.9; legacy fields drop в 4.12.
- `src/combat/ai/factors/offensive.rs` — `compute_offensive` rewrite в 4.10; `compute_aoe_damage` + `friendly_fire_penalty` удалены в 4.10.
- `src/combat/ai/planning/generator.rs` — `build_step_outcome_estimate` удалён в 4.9 (переехал в `outcome::builder::from_sim_step`).
- `src/combat/ai/log.rs` — schema v27 → v28 в 4.12 (clean break).
- `src/bin/replay_ai_log.rs` — rewrite в 4.12 под v28 only.
- `src/bin/mine_ai_logs.rs` — rewrite в 4.12 под v28 only.

### Меняются минимально

- `src/combat/ai/scoring.rs` — `stun_denial_value` / `status_score` / `status_applications` остаются как domain helpers; их wrapping в policy::* — в 4.7.
- `src/combat/ai/planning/terminal.rs::compute_secure_kill` — без change в 4.11 (читает p_kill_now/p_kill_soon, facts остались).
- `src/combat/ai/repair/goal.rs::extract_goal_context` — без change в 4.11 (читает p_kill_now, fact остался).
- `src/combat/ai/planning/future_value.rs` — switch с `estimate_hypothetical().expected_damage` на `outcome::builder::hypothetical(...)` + `policy::damage::value(...)` в 4.12.
- `src/combat/ai/planning/picker.rs::record_committed_reservations` — то же что future_value в 4.12.
- `tests/ai_scenarios/snapshots/continuation_relevant_preserved/` — пересборка в v28 формате (4.12).

## Ожидаемые сдвиги

После 4.13 — **никаких behavioral сдвигов** относительно post-step-7 baseline. Волна 2 — pure refactor с инвариантом «behavior unchanged», pin'нутым property tests'ами + golden replay'ями.

**Mining-метрики на v28 corpus** должны воспроизвести post-7.5 картину с новым fact-распределением:
- `goal_preserved (combined)` ≈ 55% (continuation analysis).
- `actor_hp_drop` divergence ≤ 12%.
- New: `enemy_damage` distribution per turn — баseline для step 10 critics.
- New: `cc_turns_applied` distribution per turn — baseline для disable critics.
- New: `ally_damage` events frequency — мониторинг friendly-fire incidents.

**Что становится возможным:**

- **Step 8 (StepFactor / PlanFactor / TerminalFactor)** — каждый StepFactor читает фиксированные facts + applies named policy. Чёткая иерархия слоёв.
- **Step 10 (critics)** — per-fact thresholds (`SelfLethalWithoutPayoff` reads `self_damage > X`; `BuffIntoVoid` reads applied statuses); critics не дублируют policy.
- **Step 11 (bands+agenda+scorecard)** — multi-policy evaluation: агенда items могут apply разные policies к одним и тем же facts (роль-зависимое scoring).
- **Step 16 (UnitQuirks)** — quirks параметризуют policy formulas, не overload outcome facts.
- **Step 17 (geometry awareness)** — `gained_los` / `entered_cast_arc` / `opened_aoe_line` добавляются как новые facts в outcome; geometry policies в `policy::geometry::*`.

## Что откладывается

- **Per-status breakdown в outcome** — `applied_statuses: Vec<(StatusId, Entity, duration)>` если step 10 critics типа `BuffIntoVoid` потребуют гранулярности beyond aggregated `cc_turns_applied`/`vulnerability_applied`/`armor_shred_applied`. Backlog.
- **`geometry_gain` field** — добавится в step 17 (geometry awareness), не сейчас.
- **Trait `Policy` для swap'а formul** — добавится в step 16 (UnitQuirks) если понадобится runtime swap. Сейчас — plain functions.
- **Расширение sim StepOutcome** — крупные изменения sim (per-entity damage из sim, applied_statuses field) — step 12 (mid-plan reflow) или earlier если 4.6 audit найдёт критические gaps.
- **Probabilistic outcome fields** — `p_kill_now` / `p_kill_soon` остаются binary 0.0/1.0; probability semantics — backlog (step B1 portfolio search).

## Чего не делать в волне 2

- **Не менять scoring formulas** — каждая policy formula остаётся bit-identical с pre-4.7 поведением (pin'нуто property tests'ами). Волна 2 — re-foundation структуры, не tuning.
- **Не делать stages decomposition factors** — `compute_plan_factors_sans_intent` / `finalize_scores` остаются монолитом до step 8.
- **Не вводить bands / agenda / scorecard** — `intent.rs` остаётся step ladder'ом до step 11.
- **Не добавлять placeholder fields в outcome** — `geometry_gain`/`board_pressure`/etc удаляются, не добавляются. Новые добавятся в нужные шаги (17, 5+).
- **Не делать per-step critics** — критиков ещё нет, добавятся в step 10 на готовом outcome.
- **Не делать policies trait-based** — pure functions сейчас, trait в step 16 если понадобится.
- **Не делать parallel/async population** — outcome populates sync inside beam search; параллелизм после profiling.
- **Не оптимизировать outcome shape для serialization size** — `Vec<(Entity, f32)>` для per-entity breakdown приемлемо; gzip жмёт. Optimization после profiling.
- **Не пытаться поддержать v27 logs** — clean break, как в step 7.5. v27 logs дают `LogError::UnsupportedSchema`, не migration shim.
