# Цикл принятия решения

*Источник: `src/combat/ai/utility/mod.rs`, `src/combat/ai/pipeline/mod.rs`.*

Один тик `enemy_ai_system`:

```
1. Проверка AP/MP (ничего нельзя → EndTurn)
2. Построить BattleSnapshot + InfluenceMaps
3. pick_action:
   a. select_intent → TacticalIntent (primary)
   b. assign_band(active, snap, needs, tuning) → (PriorityBand, BandReason)
      build_agenda(band, …) → N агенда-айтемов с baseline considerations
   c. generate_plans: beam-search глубиной plan_max_depth, шириной
      plan_beam_width. Шаг — Cast (из top-N threat ∪ top-M killability)
      или Move (top по escape/opportunity/priority-adj). Hard constraints
      (taunt, overheal, wasted-CC, self-AoE) режут невалидные Cast.
      Дубликаты по logical_key схлопываются.
   d. score_plans_with_raw: 10-факторный utility scoring → ScoredPool
      (raw factor matrix + ann.score per план).
   e. Pipeline (12 stage-ов):
      Viability → ItemScoring → ModeSelection → Finalize → Sanity →
      Critics → ProtectSelfMask → KillableGate → RepairAffinity →
      OverlayConsiderations → PlanModifiers → PickBest
   f. PickBestStage: per-(plan × agenda_item) композиция, mercy окно,
      similarity window, top-K sampling.
4. commit_plan(best, actor_pos) → (AiDecision, consumed):
   - []                  → (EndTurn, 0)
   - [Cast, ..]          → (CastInPlace, 1)
   - [Move, Cast, ..]    → (MoveAndCast, 2) — атомарный бандл
   - [Move, ..]          → (MoveOnly, 1)
5. record_committed_reservations(plan, consumed, ...) — резервирует
   урон/CC/тайл только для закоммиченного prefix.
6. Нет планов вообще (актор пропал из snapshot) → fallback_move.
```

## Stage-ы pipeline

| Stage | Что делает | Источник |
|---|---|---|
| `ViabilityStage` | Intent viability guard: если `max(intent_factor)` ниже порога — fallback intent (mid-panic → ProtectSelf, иначе FocusTarget над достижимой целью) и rescore | `pipeline/stages/viability.rs` |
| `ItemScoringStage` | Per-item scoring: для каждого plan × agenda_item считает `IntentConsiderations`. Запускается до `ModeSelection` чтобы видеть primary-intent raw factors | `pipeline/stages/item_scoring.rs` |
| `ModeSelectionStage` | Выбирает `EvaluationMode` для каждого плана (через `select_evaluation_modes`), пишет `adaptation` annotation | `pipeline/stages/mode_selection.rs`, `planning/adaptation.rs` |
| `FinalizeStage` | Применяет `EvaluationMode` к каждому плану, `rescore_with_per_plan_modes`, мутирует `ann.score` | `pipeline/stages/finalize.rs` |
| `SanityStage` | Residual мягкие штрафы (HealerExposure, RetreatTrap, SynergyBonus) | `pipeline/stages/sanity.rs`, `planning/sanity.rs` |
| `CriticsStage::first_wave()` | 6 critics первой волны (см. [critics.md](critics.md)) | `pipeline/stages/critics.rs`, `critics/` |
| `ProtectSelfMaskStage` | Если intent == ProtectSelf: маскирует не-defensive планы с `mode=Default` в `-∞` | `pipeline/stages/protect_self.rs` |
| `KillableGateStage` | Защита от kill-conversion regression — рассогласование «есть kill_now → выбран не-killing план» | `pipeline/stages/killable_gate.rs`, `planning/killable_gate.rs` |
| `RepairAffinityStage` | Goal-preserving repair: добавляет `repair_bonus` для планов, сохраняющих `last_goal` | `pipeline/stages/repair_affinity.rs`, `repair/` |
| `OverlayConsiderationsStage` | Plan-aware overlay: feasibility, leverage, safety, continuation_value по факту наличия `ann.repair_affinity` | `pipeline/stages/overlay_considerations.rs` |
| `PlanModifiersStage` | Применяет 3 plan-level модификатора (`summon_bonus`, `trade_bonus`, `repair_bonus`) — additive после composition | `pipeline/stages/plan_modifiers.rs`, `modifiers/` |
| `PickBestStage` | Per-(plan × agenda_item) композиция: `composed = score_initial + intent_delta + tempo_delta + W_intent × cdot`. Mercy → similarity window → top-K sampling. Маркирует `ann.chosen` | `pipeline/stages/pick_best.rs` |

## Goal lifecycle

При `MoveOnly` commit план записывает `StoredGoalContext` в `AiMemory.last_goal` (kind, region_anchor, planned_ability, ttl, confidence).
При `Cast`/`MoveAndCast`/`EndTurn` — `last_goal` очищается (или продлевается, если goal достигнут).

Подробнее — [scoring.md → Goal-preserving repair](scoring.md#goal-preserving-repair).

## `GrantMovement` mid-turn

Способности с эффектом `GrantMovement { distance }` **немедленно** добавляют `distance` в пул активного юнита. Следующий AI-тик re-planit уже с расширенным пулом.
