# Enemy AI

Обзор архитектуры. Каждый слой — отдельный документ; здесь карта модулей, ответственность каждого, и правила «куда что класть».

## Что это

AI выбирает действие для вражеских юнитов (и героев под `pact_control`). Работает в `CombatStep::Command`: `enemy_ai_system` для `Team::Enemy`, `pact_ai_system` для героев под `ai_controlled`-статусом.

Каждый AI-тик строит **свежий `pick_action`** — beam-search генерирует пул планов, типизированный pipeline считает score, выбирается лучший. Reservations координируют параллельно действующих юнитов, резервируя только закоммиченный prefix плана.

**Goal-preserving repair.** `AiMemory.last_goal` хранит `StoredGoalContext` (kind, region, ttl, confidence). Fresh план всегда строится с нуля; планы, продолжающие сохранённый goal, получают `repair_bonus` через `RepairAffinityStage`.

Файлы: `src/combat/ai/` + shared core в `src/combat/effects_*` (вне `ai/`).

## Архитектура: 3 слоя

```
┌──────────────────────────────────────────────────────────────────┐
│  ORCHESTRATION layer                                             │
│  • system.rs          — Bevy ECS system: snapshot + maps + tick  │
│  • orchestration/     — pick_action + fallback + AiDecision      │
└──────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌──────────────────────────────────────────────────────────────────┐
│  PIPELINE layer  (orchestrates 12 typed stages on a ScoredPool)  │
│  • pipeline/order.rs    — PRODUCTION_PIPELINE (single src truth) │
│  • pipeline/spec.rs     — StageSpec + reads/writes validator     │
│  • pipeline/score_trace — ScoreTrace + compute() + JSONL mirror  │
│  • pipeline/stages/     — 12 stages absorb critics/modifiers/... │
└──────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────┬─────────────────────┬──────────────────────┐
│ INPUT (read-only)   │ FACTS               │ COMPUTE              │
│ • world/            │ • appraisal/        │ • scoring/           │
│ • config/           │ • intent/           │ • plan/              │
│ • memory/           │ • outcome/          │ • adapt/             │
│ • repair/           │                     │                      │
└─────────────────────┴─────────────────────┴──────────────────────┘

  log/        — JSONL types + serde helpers + debug overlay (schema v34)
  replay/     — assertion DSL + replay pipeline (executor in src/bin/)
  action_state.rs — per-actor action availability tracking
```

### Pipeline stages (production order)

`PRODUCTION_PIPELINE` в `pipeline/order.rs`:

1. **Viability** — PreScoreGate; фильтрует non-viable планы.
2. **ItemScoring** — populates `per_item[]` для агенды.
3. **ModeSelection** — пишет `EvaluationMode` (default / last_stand) в annotation.
4. **Finalize** — Rescore: переписывает `ann.score` из raw factors с учётом `mode`. Обнуляет `score_trace.base`, `rescore_mode`.
5. **Sanity** — Multiplier: 3 правила (`HealerExposure`, `RetreatTrap`, `SynergyBonus`) → push в `score_trace.multipliers`.
6. **Critics** — Multiplier: 6 первой волны (`OvercommitIntoDanger`, `SelfLethalWithoutPayoff`, `BlindspotRanged`, `BuffIntoVoid`, `RareResourceForLowImpact`, `HealWithoutRescueValue`) → `score_trace.multipliers`.
7. **ProtectSelfMask** — Mask: пушит `MaskHit { kind: Poison, source: "protect_self" }` в `score_trace.masks` для не-defensive планов под `ProtectSelf`. Selectability — через `ScoreTrace::is_masked()`. `ann.score` остаётся финитным (Phase 3).
8. **KillableGate** — Gate: пушит `GateHit { outcome: Reject }` в `score_trace.gates` для FocusTarget-планов, не атакующих цель. Selectability — через `ScoreTrace::is_gated()`. После Phase 3 — Gate-only emit, без Mask.
9. **RepairAffinity** — пишет `ann.repair_affinity` (vs `memory.last_goal`).
10. **OverlayConsiderations** — per-item considerations overlay (плотность boards / влияние).
11. **PlanModifiers** — Addend: `summon_bonus`, `trade_bonus`, `repair_bonus` → `score_trace.addends`.
12. **PickBest** — argmax по `SelectionKey { selectable, score }` (2-bucket: selectable plans first, masked/gated — fallback) + jitter; ставит `ann.chosen`.

`ann.score` всегда финитен (Phase 3). Drive-loop (`pipeline/effects.rs::apply_score_effect_stage`) — sole writer `score_trace` + cached `ann.score = trace.compute()` после каждой score-effect стадии. Стадии возвращают `Vec<EmittedEffect>` через `ScoreEffectStage` trait, не мутируют annotation напрямую. Selectability — через `ScoreTrace::is_masked()` / `is_gated()`, **не** через `score.is_finite()`.

`StageSpec` в `pipeline/spec.rs` фиксирует `reads/writes/score_effect` для каждой стадии в типах. Validator runs в тесте `production_pipeline_order_is_valid` — runtime-error если порядок стадий нарушает контракты (Rescore после Multiplier и т.п.).

### `ScoreTrace` — typed effect log

`ScoreTrace { base, rescore_mode, multipliers, addends, masks, gates }`. Все score-mutation механизмы пушат typed hits, `compute()` возвращает финальный score по канонической алгебре:

```
1. mask Poison present       → -∞ (early exit)
2. score = base
3. score *= ∏ multipliers     (sanity → critics, push order)
4. score += Σ addends         (modifiers)
5. gates: флаг для PickBest, не зануляют score
```

`ann.score` остаётся как cached `trace.compute()` (читатели не меняются). JSONL экспортирует `score_trace_log: Option<ScoreTraceLog>` с schema v33+.

## Карта модулей

### Top-level layout

| Модуль | Назначение |
|---|---|
| `system.rs` | Bevy ECS system: snapshot + maps + AI tick + fallback dispatch. |
| `orchestration/` | `pick_action` оркестратор: build pool → run pipeline → commit. `AiWorld`, `ScoringCtx`, `AiDecision`. Включает `fallback.rs` для случаев пустого pool. |
| `action_state.rs` | Per-actor action availability tracking (cooldowns, AP, used resources). |

### Input layer (read-only world view + state)

| Модуль | Назначение |
|---|---|
| `world/snapshot.rs` | `BattleSnapshot`, `UnitSnapshot.statuses`, `refresh_status_aggregates`. |
| `world/influence.rs` | Карты влияния (`InfluenceMaps`, `InfluenceConfig`). |
| `world/reservations.rs` | Координация параллельно ходящих юнитов; reset на round-start. |
| `world/tags/` | `AbilityTag`, `StatusTag`, `AiTags` (bitflags) — single source of truth классификации. |
| `config/tuning.rs` | `AiTuning` resource: `thresholds`, `tables`, difficulty curves. |
| `config/role.rs` | `AxisProfile` (5-мерная роль) + инференс по kit'у. |
| `config/difficulty.rs` | `DifficultyProfile` ручки качества решений. |
| `memory/ai_memory.rs` | `AiMemory` Component + `PlanSnapshot` + `status_hash`. |
| `memory/goal/` | `StoredGoalContext` + lifecycle (commit / invalidate / TTL decay). |
| `repair/affinity.rs` | `RepairAffinity` — насколько новый план продолжает stored goal. |
| `repair/mod.rs` | Repair lookup + intent-vs-goal compatibility helpers. |

### Facts layer (snapshot → numbers)

| Модуль | Назначение |
|---|---|
| `appraisal/` | Need signals (per-actor): `self_preserve`, `rescue_ally`, `apply_cc`, `setup_aoe`, `continue_commitment`, `finish_target`, `reposition`, `conserve_resource`. Каждый — отдельный файл. `mod.rs` orchestrates `compute_need_signals`. |
| `intent/kinds.rs` | `TacticalIntent`, `IntentKind`, `IntentReason` (data types). |
| `intent/select.rs` | `select_intent` — выбор global intent под need signals. |
| `intent/score.rs` | `intent_score`, `pursuit_move_score`, `cc_reach`, `IntentWeights`. |
| `intent/agenda.rs` | `Agenda` + `build_agenda` (несколько intent items с приоритетами). |
| `intent/bands.rs` | `PriorityBand` + `assign_band` (категоризация интентов). |
| `intent/considerations.rs` | `IntentConsiderations` overlay (6 осей). |
| `outcome/` | `ActionOutcomeEstimate` (17 fact-полей) + `PlanAnnotation` + builder. |
| `adapt/mod.rs` | `EvaluationMode`, `AdaptationReason` (data types). |
| `adapt/select.rs` | `select_evaluation_modes` (per-plan triggers: ExpectedSelfLethal, ProtectSelfNoDefensive, ProtectSelfFutile). |

### Compute layer (factors → scores)

| Модуль | Назначение |
|---|---|
| `plan/types.rs` | `PlanStep`, `TurnPlan`, `StepOutcome`. |
| `plan/generator.rs` | Beam-search plan generation. |
| `plan/sim.rs` | Pure simulation (`SimState`) — applies steps to a cloned snapshot. |
| `plan/reach.rs` | Reachability for movement. |
| `plan/future_value.rs` | Look-ahead PFV (post-step future value). |
| `scoring/horizon.rs` | DPR helpers, damage horizon estimation. |
| `scoring/target_selection.rs` | Target selection score (relative ranking). |
| `scoring/position_eval.rs` | Position quality evaluation. |
| `scoring/trade.rs` | `unit_value`, `trade_delta`, economic exchange scoring. |
| `scoring/policy/` | HP-equivalent value functions: `damage`, `heal`, `cc`, `status`, `friendly_fire`. |
| `scoring/factors/aggregate.rs` | `score_plans_with_raw`, `aggregate_factors_to_score`, `compute_plan_factors`, `rescore_with_per_plan_modes`. Pool aggregation primitives. |
| `scoring/factors/terminal_state.rs` | `terminal_state_score` + 8 terminal-axes (exposure_at_end, secure_kill, ally_rescue, board_control_gain, next_turn_lethality, line_actionability, density_value, pressure_spacing_zone). |
| `scoring/factors/step/` | Per-step factor leaves: `cc`, `damage`, `heal`, `kill_now`, `kill_promised`, `saturation`, `scarcity`. |
| `scoring/factors/plan/` | Plan-level factor leaves: `intent`, `self_survival`, `tempo_gain`. |
| `scoring/factors/terminal/` | Terminal-axis leaves (registry interface to terminal_state). |
| `scoring/factors/registry.rs` | Factor registry uniform shape (`NAME`, `SIGNED`, `compute`). |
| `scoring/factors/{adjustments, aoe_hits, offensive}.rs` | Shared helpers. |

### Pipeline layer

| Модуль | Назначение |
|---|---|
| `pipeline/order.rs` | `StageId`, `StageEntry`, `PRODUCTION_PIPELINE` (single source of truth для порядка стадий), `run` runner. Сплит `PRE_MASK`/`POST_MASK` для `base_scored` snapshot между двумя половинами. |
| `pipeline/spec.rs` | `StageSpec`, `ScoreEffect`, `AnnotationField`, `STAGE_SPECS`, `validate_pipeline`. |
| `pipeline/score_trace.rs` | `ScoreTrace` runtime (`MultiplierHit { kind, value, detail: Option<MultiplierDetail> }`, `MaskHit { kind, source, original_score: Option<f32> }`, `AddendHit`, `GateHit`) + `ScoreTraceLog` JSONL mirror + `compute()` (always finite — Phase 3) + `is_masked()`/`is_gated()` selectability flags. |
| `pipeline/effects.rs` | Score Effect Engine: `ScoreHit`, `EffectObservation`, `EmittedEffect`, `AppliedEffect`, `ScoreEffectStage` trait, `apply_score_effect_stage` drive-loop (sole writer score_trace + cached score), `SelectionKey { selectable, score }` for PickBest. |
| `pipeline/mod.rs` | `StageCtx`, `ScoredPool`, `PlanStage` trait. |
| `pipeline/stages/{viability, item_scoring, mode_selection, finalize, protect_self, repair_affinity, overlay_considerations, pick_best}.rs` | Single-purpose stages. |
| `pipeline/stages/sanity/` | `SanityStage` + `{healer_exposure, retreat_trap, synergy_bonus}.rs` (one rule per file). |
| `pipeline/stages/critics/` | `CriticsStage` + 6 critic leaf-files. |
| `pipeline/stages/modifiers/` | `PlanModifiersStage` + `{summon_bonus, trade_bonus, repair_bonus}.rs`. |
| `pipeline/stages/killable_gate/mod.rs` | `KillableGateStage` + algorithm helpers (`apply_killable_gate`, `plan_is_offensive_vs`). |

### Output / observability

| Модуль | Назначение |
|---|---|
| `log/mod.rs` | `ActorTickEvent`, `LoggedDecision`, `LoggedPlan`, `SCHEMA_VERSION`. JSONL writer. |
| `log/serde_helpers.rs` | Serde адаптеры (entity wrapping). `f32_finite` adapter удалён в Phase 3 (`ann.score` всегда финитен). |
| `log/debug.rs` | Debug overlay + console log. |
| `replay/mod.rs` | Assertion DSL: `Overlay`, `Expectation`, `AssertResult`, `build_actual_decision`. |
| `replay/pipeline.rs` | `assert_v28_log_file`, `GoldenRecord`, file-level assertion pipeline. |
| `src/bin/replay_ai_log.rs` | CLI executor (uses `replay::*` library). |
| `src/bin/mine_ai_logs.rs` | Aggregated log analytics. |

### Shared effects core (вне `ai/`)

`src/combat/effects_math.rs`, `effects_state.rs`, `effects_outcome.rs` — единый источник истины для разрешения способности. Production pipeline (`combat/resolution.rs`) и AI sim (`combat/ai/plan/sim.rs`) вызывают один и тот же `compute_ability_outcome`; различаются backend'ами (RNG vs EV, Bevy components vs snapshot). См. [`ability-resolution.md`](ability-resolution.md).

## Owner map: куда класть новое

| Что добавляешь | Куда | Почему |
|---|---|---|
| Новый factor | `scoring/factors/{step,plan,terminal}/<name>.rs` | Один factor = один leaf-файл с `NAME`, `SIGNED`, `compute`. Implementation полная в leaf'е (P5). |
| Новый pipeline stage | `pipeline/stages/<name>.rs` + регистрация в `pipeline/order.rs::PRODUCTION_PIPELINE` + `StageSpec` в `pipeline/spec.rs` | Stage реализуется через `apply_<name>` fn-pointer + spec, не trait object. |
| Новый critic | `pipeline/stages/critics/<name>.rs` + регистрация в `CriticsStage::first_wave` | Multi-instance stage. |
| Новый score modifier | `pipeline/stages/modifiers/<name>.rs` | Additive plan-level бонус. |
| Новое sanity-правило | `pipeline/stages/sanity/<name>.rs` | Multiplicative penalty. Если ложится на critic-семантику — лучше critic, не sanity. |
| Новый need signal | `appraisal/<name>.rs` | Поле в `NeedSignals` + producer-функция. |
| Новый outcome fact | `outcome/mod.rs` (поле в `ActionOutcomeEstimate`) + `outcome/builder.rs` | Только raw facts, без value judgement. |
| Новая HP-equivalent value function | `scoring/policy/<name>.rs` или существующий `policy::*` | Pure function `fn(facts, context) -> f32`. |
| Новый input в snapshot | `world/snapshot.rs` или `world/influence.rs` | Read-only world view. |
| Новый `AiTag` flag | `world/tags/ai_tags.rs` | Single source of truth для AI bitflags. |
| Новый `AbilityTag` / `StatusTag` | `world/tags/classify.rs` + `world/tags/cache.rs` | Семантический тэг. |
| Новая константа тюнинга | `config/tuning.rs` (`Thresholds` / `Tables` / `Difficulty`) + `assets/data/ai_tuning.toml` | Data-driven, не const в коде. |
| Новый difficulty knob | `config/difficulty.rs` + lerp endpoints в `tuning.toml` | Data-driven. |
| Новый `AdaptationReason` | `adapt/mod.rs` (variant) + `adapt/select.rs` (триггер) | Не в planning, не в pipeline/stages. |
| Новый goal kind | `memory/goal/context.rs` (variant `GoalKind`) | Memory concern. |
| Новый replay assertion | `replay/mod.rs` (DSL остаётся в lib) | Executor — `bin/replay_ai_log.rs`, не библиотека. |
| Новый mining metric | `src/bin/mine_ai_logs.rs` | Tooling, не runtime. |

Если ни одна строка не подходит — это сигнал, что концепт не вписывается в текущие слои, и нужен design-discussion, а не «положу куда-нибудь».

## Документы по слоям

| Документ | Что внутри |
|---|---|
| [decision-cycle.md](decision-cycle.md) | Цикл `pick_action`, порядок stage-ов, `GrantMovement` mid-turn. |
| [pipeline.md](pipeline.md) | `PRODUCTION_PIPELINE`, `StageSpec` validator, `ScoreTrace` algebra, plan generation hard constraints. |
| [ability-resolution.md](ability-resolution.md) | `TargetState`, `DiceSource`, `compute_ability_outcome`, drift sim↔real. |
| [snapshot.md](snapshot.md) | `BattleSnapshot`, `UnitSnapshot`, `AiTags`, `AbilityTag` / `StatusTag`. |
| [intent.md](intent.md) | `TacticalIntent`, intent selection, viability guard, intent-scoring, `ProtectSelf` mask. |
| [scoring.md](scoring.md) | Factors (10 осей), outcome vector, terminal-axes, repair, role weights. |
| [policy.md](policy.md) | HP-эквивалентные value functions. |
| [target-priority.md](target-priority.md) | `target_selection_score`, position evaluation, influence maps. |
| [adaptation.md](adaptation.md) | `EvaluationMode`, `AdaptationReason`, MVP scope. |
| [trade-economy.md](trade-economy.md) | `unit_value`, `trade_delta`, resource scarcity. |
| [critics.md](critics.md) | 6 critics первой волны + residual sanity. |
| [bands-agenda.md](bands-agenda.md) | `PriorityBand`, `Agenda`, 6 осей `IntentConsiderations`. |
| [difficulty.md](difficulty.md) | `DifficultyProfile`, lerp curves, per-unit override. |
| [debug.md](debug.md) | Overlay, console log, JSONL. |
| [extension-checklist.md](extension-checklist.md) | Поэтапный чеклист расширения mechanics (новый effect/status/ability). |
| [replay.md](replay.md) | `replay_ai_log`, schema versions, `--assert`, regression metrics. |
| [mining.md](mining.md) | `mine_ai_logs` — агрегированная статистика по корпусу. |
| [tech-debt.md](tech-debt.md) | Followup roadmap: проблемы организации/логики, обнаруженные в пост-restructure-аудите. |
| [rework/](rework/) | Архив step-планов, mining-данных, дизайн-документов. |

## Версии схем

- **`SCHEMA_VERSION = 34`** (`log/mod.rs`) — текущая версия JSONL.
- v34: `IntentReason::Adapted` split → отдельное поле `evaluation_mode_reason`; `TacticalIntent::LastStand` упразднено (stays as `EvaluationMode`); `target_priority` → `target_selection_score` rename.
- v33: `ScoreTraceLog` экспонирован в JSONL как `score_trace_log` (schema-additive).
- v32: `ActorTickEvent.band` / `band_reason` / `agenda`, `PlanAnnotation.{agenda_item, considerations_per_item}`.
- v28+: outcome shape — fundamental data; v27 logs дают `LogError::UnsupportedSchema`.

Validator (`log/mod.rs`): принимает `>= SCHEMA_VERSION - 1` schema-additively (т.е. v33 + v34); v32 и ниже отвергает.

## Тестовая инфраструктура

- `tests/ai_scenarios.rs` — production-grade replay assertions (использует `replay::assert_v28_log_file`).
- `tests/replay_assert.rs` — DSL-уровень.
- `pipeline/spec.rs::tests` — `production_pipeline_order_is_valid` + 4 negative case (Rescore-after-effect, multiple-Rescore, PostScoreGate-before-Rescore, missing-writer).
- `pipeline/score_trace.rs::tests` — `compute()` algebra (8 тестов) + JSONL roundtrip.
- `pipeline/mod.rs::tests::p3a_full_pipeline_trace_compute_equals_ann_score` — invariant `ann.score == trace.compute()` после полного PRODUCTION_PIPELINE.
