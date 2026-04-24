# Шаг 4 — `ActionOutcomeEstimate`: декомпозиция на сабшаги

Декомпозиция в стиле фазы 2a: коммит-на-сабшаг, golden-replay gate на каждом.
Спецификация: `docs/ai_rework.md` §4 и `docs/ai_rework_plan.md` §4 + gates.

## Preamble

**Текущее состояние скоринга.** 10 factors в `factors/mod.rs` (`PlanFactors`, `NUM_FACTORS = 10`). Центральная HP-equivalent функция — `score_action` в `scoring.rs:58`. Имеет **ровно 6 call-sites**:
- `factors/offensive.rs` — 3 (single-target damage/heal, aoe per-enemy, friendly-fire splash).
- `planning/future_value.rs` — 3 (λ_attack: FocusTarget / ApplyCC / default top-3).
- `planning/picker.rs:223` — 1 (reservations post-pick).

**Natural seam sim ↔ consumers.** `generate_plans` вызывает `ext_sim.apply_step(...) -> StepOutcome` на `generator.rs:154` и складывает в `plan.outcomes: Vec<StepOutcome>` (`types.rs:46`). **Это единственное место с plan-step granularity.** Туда заходит `ActionOutcomeEstimate` — параллельно с `StepOutcome`, с той же cardinality. Чище — **новый vec**, не расширять `StepOutcome` (иначе ломает JSONL schema и parity-тесты).

**Самый хитрый dependency-edge.** `intent_score` (`intent.rs:858,880`) внутри себя вызывает `compute_factors(step_ctx, step)` — значит, branches FocusTarget/ApplyCC апгрейдятся даром вместе с 4.3. А `future_value.rs::λ_attack` — **отдельный** consumer `score_action`: оценивает hypothetical committed state, где sim step НЕ запущен. Туда нужен on-demand `estimate_hypothetical(...)` helper (4.4).

**Sanity.rs** в scope step 4 НЕ трогаем — survival/AoO читают raw snapshot и path, не factors. Эти паттерны — кандидаты на critics в шаге 10.

## Сабшаги

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

## Итого

| # | Шаг | Эстимейт | Golden-replay | Статус |
|---|---|---|---|---|
| 4.0 | scaffolding | 0.5 | 0 diff | **DONE** (`cb94250`) |
| 4.1 | expected_damage | 0.5 | 0 diff | **DONE** (`7de5c30`) |
| 4.2 | full 9-field | 1.5 | 0 diff | **DONE** (`88da91f`) |
| 4.3 | offensive consumer | 1.5 | 0 diff | **DONE** (`7aae9c9`) |
| 4.4 | future_value + picker | 1.0 | 0 diff (literal rewire) | **DONE** (`d2cf7c6`) |
| 4.5 | cleanup + JSONL v18→v19 | 1.0 | 0 diff | **DONE** (`6ae1429`) |

**Суммарно ~6 дней** (4.5 расширен: +0.5 дня friendly-fire рефактор, +0.3 дня schema bump).

## Зафиксированные решения

1. **Tolerance в 4.4 → `(b) semantic weighted sum`.** λ_attack в 4.4 становится взвешенной суммой полей outcome (`expected_damage + w_cc * deny_value + w_kill * p_kill_now`). Gate: **tolerance ≤3 epsilons / 131 entries** с письменным обоснованием в коммите. Веса подбираются чтобы совпасть со score_action на типичных abilities ±1e-6.
2. **`score_action` → `(b) полный рефакторинг`.** AoE friendly-fire branch переписывается на hypothetical outcome на ally; `score_action` удаляется целиком в 4.5. +0.5 дня к сабшагу 4.5.
3. **JSONL → `(b) сериализовать сразу в 4.5`.** `PlanAnnotation` попадает в `PlanLogEntry`, `SCHEMA_VERSION` v18 → v19, `#[serde(default)]` на обеих сторонах для bwd compat.
4. **Плумбинг outcome → `(a) явный параметр`.** `compute_factors(ctx, step, outcome: &ActionOutcomeEstimate)` — 2 call-sites (scorer + intent_score). Data flow видим в signature.
5. **`rescue_value` в 4.2 → `(a) копия 1:1`.** Буквальный перенос формулы `score_action.heal` с urgency baked-in. Golden 0 diff в 4.x. Step 3 (need layer) разделит на outcome.rescue_value (effect magnitude) + NeedSignals.rescue_ally (urgency) семантически.
6. **Golden baseline → `(c) переименовать + пересобрать`.** `logs/golden_pre_2a.jsonl` удаляется, `logs/golden_pre_step4.jsonl` (тот же корпус: 4 v17-лога, 131 entries, пересобран с HEAD после 2.7). Diff между pre_2a и pre_step4 = 0 (код не менялся).

## Критические файлы

- `src/combat/ai/scoring.rs` — `score_action` legacy.
- `src/combat/ai/factors/offensive.rs` — первый real consumer (4.3).
- `src/combat/ai/planning/generator.rs` — producer site.
- `src/combat/ai/planning/types.rs` — `TurnPlan`, `PlanAnnotation`.
- `src/combat/ai/planning/future_value.rs` — второй consumer (4.4).
- `src/combat/ai/intent.rs` — автоапгрейд через `compute_factors`, плумбинг через ScoringCtx.
- `src/combat/ai/planning/scorer.rs` — `compute_plan_factors_sans_intent` + outcome index.
