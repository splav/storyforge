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
