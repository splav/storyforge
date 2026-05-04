# Plan Pipeline: generation, sanity, pick best

*Источники: `src/combat/ai/planning/{generator,sanity,picker}.rs`, `src/combat/ai/pipeline/stages/{sanity,killable_gate,pick_best}.rs`.*

Полный список stage-ов и их порядок — в [decision-cycle.md](decision-cycle.md). Здесь — детали трёх ключевых блоков: hard constraints на этапе генерации планов, residual sanity и финальный выбор.

## Hard Constraints (в `generate_plans`)

Применяются на стадии beam-search, до scoring:

1. **Taunt** — SingleEnemy Cast только на taunted-целях.
2. **Team safety** — `pick_targets` из `allies_of` / `enemies_of`.
3. **Overheal** — SingleAlly на цели > 90% HP отбрасывается.
4. **Wasted CC** — single-target CC на оглушённой цели отбрасывается.
5. **Self-AoE friendly-fire** — если `enemies_hit < allies_hit × 2`.

## Plan Sanity Adjust

`SanityStage` (`pipeline/stages/sanity.rs`) — мультипликативные штрафы после scoring. **Инвариант слоя: только мягкие penalty, никаких hard-масок.** Ранее существовавший «lethal AoO → −∞» переехал в [adaptation](adaptation.md) как `ExpectedSelfLethal` переключение режима оценки.

После переноса в `CriticsStage` большинства sanity-rules (step 10) в `planning/sanity.rs` остались три general-purpose правила:

| Проверка | Эффект | Условие |
|----------|--------|---------|
| **HealerExposure** | `× 0.5` | non-support уходит от единственного healer'а |
| **RetreatTrap** | `× 0.5` | final_pos с `< 2` свободных соседей |
| **SynergyBonus** | `× 1.1` | move в safer/better tile + useful cast (не штраф) |

Также в `sanity.rs` остались как `pub(crate)` helpers, переиспользуемые критиками: `expected_aoo_damage`, `plan_has_self_aoe`, `plan_has_useful_cast`. `apply_protect_self_mask` — hard mask (≠ critic).

Прежние sanity-checks `Survival` / `AoOBleed` / `LosBlindspot` / `SelfAoe` мигрировали в [`critics/`](critics.md).

## Killable Gate

`KillableGateStage` — защита от kill-conversion regression: если в pool'е есть план с `p_kill_now > 0` на target, который убиваем, — план без kill_now не должен побеждать без сильного оправдания. Соответствующая mining-метрика — `kill_conversion_rate` в [replay.md](replay.md#kill_conversion_rate).

## Pick Best Plan + Commit

`PickBestStage` (`pipeline/stages/pick_best.rs`) после всех остальных stage-ов:

1. **Per-(plan × agenda_item) композиция**: `composed = score_initial + intent_delta + tempo_delta + W_intent × cdot` (см. [bands-agenda.md](bands-agenda.md#аддитивная-формула-композиции)).
2. **Mercy окно** `[best − mercy, best]` → rerank по `score − mercy × cruelty`, где `cruelty = kill_now + kill_promised × 0.5 + min(0.5, cc × 0.1)`.
3. **Similarity window** для top-K: pool = top-K с `score ≥ best_after_mercy − window`.
4. **Случайный выбор** в пределах pool.
5. Маркер `ann.chosen = true` на победителе.

После выбора:

- `commit_plan(plan, actor_pos)` (`planning/picker.rs`) → `(AiDecision, consumed)` — единственный source-of-truth для bundling rules (1 для solo / 2 для Move→Cast).
- `record_committed_reservations(plan, consumed, ...)` — только consumed prefix + end-tile.

## StageSpec и pipeline validator (P2)

`src/combat/ai/pipeline/spec.rs` — типизированные read/write контракты для каждой production stage. Реализовано в P2 roadmap'а.

### Структуры

```rust
struct StageSpec {
    id: StageId,
    reads:  &'static [AnnotationField],
    writes: &'static [AnnotationField],
    score_effect: Option<ScoreEffect>,
}
```

`AnnotationField` — coarse enum полей `PlanAnnotation`: `RawFactors`, `Outcomes`, `Plan`, `SnapshotFacts`, `InitialScoreFacts`, `ScoreBase`, `ScoreEffects`, `FinalScore`, `RepairAffinity`, `PerItem`, `Eligibility`, `EvaluationMode`.

`ScoreEffect` — вид эффекта на score: `PreScoreGate` (до Finalize, не трогает score), `Rescore` (устанавливает ScoreBase), `Multiplier`, `Addend`, `Mask`, `PostScoreGate`.

### STAGE_SPECS

`pub const STAGE_SPECS: &[StageSpec]` — таблица spec'ов для 12 production stages в том же порядке что `PRODUCTION_PIPELINE`. Хранится отдельно от `StageEntry` (не встроена в него): spec не зависит от split PRE_MASK / POST_MASK, поэтому дублировать данные в трёх константах было бы избыточно.

### validate_pipeline

`fn validate_pipeline(specs: &[StageSpec]) -> Result<(), ValidationError>` — проверяет три инварианта:

1. **reads-writes**: каждое поле в `reads[i]` либо в `INITIAL_FIELDS`, либо в `writes[j]` для `j < i`.
2. Ровно одна `Rescore` стадия.
3. `Rescore` не может идти после `Multiplier | Addend | Mask`.
4. Каждая `PostScoreGate` стадия идёт после `Rescore`.

`PreScoreGate` до `Rescore` разрешено (`Viability` живёт до `Finalize`).

Тест `production_pipeline_order_is_valid` запускает `validate_pipeline(STAGE_SPECS)` — ошибка порядка стадий = падение теста, не runtime-баг.

## ScoreTrace — typed effect log

`src/combat/ai/pipeline/score_trace.rs` — типизированный лог score-affecting effects, накапливаемых стадиями pipeline'а. Single source of truth для score state и selectability.

**Архитектурный статус:** Phase 2 (R8-lite) централизовала запись в drive-loop (`pipeline/effects.rs::apply_score_effect_stage`). Phase 3 убрала NEG_INFINITY-as-mask-channel: `compute()` всегда финитен, selectability через `is_masked()`/`is_gated()` + `SelectionKey`. TLE обогатил per-hit detail (SanityRule, CriticKind+CriticReason, mask original_score) — legacy mirror fields в `PlanAnnotation` удалены.

Накопление через production pipeline (стадии возвращают `Vec<EmittedEffect>` через `ScoreEffectStage` trait, drive-loop аккумулирует):
- `FinalizeStage` — устанавливает `score_trace = ScoreTrace { base: new_score, rescore_mode: Some(mode), ..Default::default() }` (rescore через `aggregate_factors_to_score`). Очищает upstream effects. `rescore_mode` фиксирует `EvaluationMode` (Default или LastStand). Это единственная стадия вне ScoreEffectStage trait — она сама пишет, не через drive-loop.
- `SanityStage` emit'ит `Multiplier(MultiplierHit { kind: Sanity, value, detail: Some(Sanity{rule}) })` + `EffectObservation::Sanity(SanityHit)`. Drive-loop derives detail из observation.
- `CriticsStage` emit'ит `Multiplier(MultiplierHit { kind: Critic, value, detail: Some(Critic{critic, reason}) })` + `EffectObservation::Critic(CriticHit)`.
- `ProtectSelfMaskStage` emit'ит `Mask(MaskHit { kind: Poison, source: "protect_self", original_score })` + `EffectObservation::Contract(ContractMaskHit)`. Plan остаётся в pool с финитным score; `is_masked() = true` → PickBest dispreferreds.
- `KillableGateStage` emit'ит **только** `Gate(GateHit { outcome: Reject, source: "killable_gate" })` + `EffectObservation::Contract(...)`. Phase 3 Step 4 убрал double-emit Mask.
- `PlanModifiersStage` emit'ит `Addend(AddendHit { name, value })` + `EffectObservation::Modifier(ModifierContribution)` для каждого из 3 modifier'ов.

После каждой score-effect стадии drive-loop вызывает `recompute_score_from_trace()` для всех annotations — invariant `ann.score == trace.compute()` держится by construction. Тест `pipeline_runs_modifiers_after_repair_before_pick` верифицирует pipeline через PRODUCTION_PIPELINE.

### Структура

```rust
struct ScoreTrace {
    base: f32,                         // ScoreBase: результат Finalize/Rescore
    rescore_mode: Option<EvaluationMode>,
    multipliers: Vec<MultiplierHit>,   // sanity, critics
    addends:     Vec<AddendHit>,       // modifiers (summon/trade/repair_bonus)
    masks:       Vec<MaskHit>,         // protect_self → Poison
    gates:       Vec<GateHit>,         // killable_gate (post-score)
}
```

Каждый `*Hit` — thin struct с kind/value/source для диагностики.

### compute() алгебра

После Phase 3 — **всегда финитный**, без poison-логики:

1. `score = base`
2. `score *= ∏ multipliers` (в порядке push'а — sanity → critics)
3. `score += Σ addends` (modifiers, аддитивны)

Mask и Gate — НЕ модификаторы score, а selectability flags. Проверяются через `ScoreTrace::is_masked()` / `is_gated()` и `PlanAnnotation::is_selectable()` / `selection_key()`. PickBest сортирует через `SelectionKey { selectable, score }` — selectable plans ranked перед masked/gated независимо от score.

### Интеграция с PlanAnnotation

`PlanAnnotation.score_trace` добавлено в P3a.0 с `#[serde(skip)]`. Поле `ann.score` остаётся как cached `trace.compute()` результат и не удаляется; JSONL schema не меняется до P3b.

`ScoreTrace::reset_effects()` очищает все Vec'ы (multipliers/addends/masks/gates) без изменения `base` — вызывается `FinalizeStage` при rescore (через struct-literal assignment с `..Default::default()`).
