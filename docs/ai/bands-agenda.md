# Bands & Agenda

*Шаг 11 (schema v32). Источники: `src/combat/ai/intent/bands.rs`, `src/combat/ai/intent/agenda.rs`, `src/combat/ai/intent/considerations.rs`, `src/combat/ai/pipeline/stages/{item_scoring,overlay_considerations,pick_best}.rs`.*

## Концепция

До step 11 AI выбирал один глобальный `TacticalIntent` через `select_intent` и скорил весь пул планов относительно этого единственного intent. Step 11 вводит двухуровневую структуру:

1. **Priority Band** (`PriorityBand`) — контекст принятия решений текущего тика. Определяет какие интент-кандидаты вообще рассматриваются и как взвешиваются оси рассуждений.
2. **Agenda** (`Agenda`) — список из N кандидатов (`AgendaItem`), каждый со своим `kind`, `target`, `raw_score` и 6-осевым `IntentConsiderations`. Финальный выбор `plan × agenda_item` выполняется `PickBestStage` через аддитивную формулу.

Результат: каждый план оценивается против каждого agenda item, лучшая пара побеждает. Это позволяет хиллеру в одном пуле планов рассматривать «ProtectAlly → союзник A» и «FocusTarget → враг B» без перегенерации планов.

## Четыре band'а

| Band | Критерий назначения | N items | Допустимые IntentKind |
|---|---|---|---|
| `ForcedTargeting` | Живой враг с `FORCES_TARGETING` тэгом (харг/taunt) | 1 | FocusTarget (taunter) |
| `CriticalSelfPreservation` | `self_preserve ≥ panic_threshold` И `danger > danger_panic` | 2 | ProtectSelf, FocusTarget (если выживание достигнуто атакой) |
| `HardRescueOpportunity` | `rescue_ally ≥ hard_rescue_threshold` И актор `CAN_HEAL` | 2 | ProtectAlly (most endangered ally), FocusTarget (главный угрожающий) |
| `NormalTactical` | Fallback — ни один из выше | 1-3 | FocusTarget, ApplyCC, SetupAOE, Reposition |

Band определяется функцией `assign_band` по первому совпадению (priority order сверху вниз).

## Шесть осей IntentConsiderations

| Ось | Смысл | Источник |
|---|---|---|
| `urgency` | Давление «нужно действовать прямо сейчас» | `NeedSignals` по типу intent |
| `feasibility` | Вероятность успеха плана | `ViabilityResult.passed` или `1.0` без плана |
| `leverage` | Тактический эффект (урон, kill, rescue value) | `outcome.enemy_damage`, `p_kill_now`, `hp_restored` |
| `safety` | `1 - exposure` (self-damage + danger на конечной позиции) | `terminal.exposure`, `outcome.self_damage` |
| `role_affinity` | Соответствие роли актора данному интенту | `AxisProfile` (support/offense/mobility) |
| `continuation_value` | Стоимость продолжения goal/stickiness | `repair_affinity` + `AiMemory.last_goal` |

Item-level baseline (urgency, role_affinity) вычисляется в `build_agenda`. Plan-aware overlay (feasibility, leverage, safety, continuation_value) вычисляется `OverlayConsiderationsStage` — после `RepairAffinityStage`, чтобы видеть `ann.repair_affinity`.

## Аддитивная формула композиции

`PickBestStage` вычисляет composed score для каждой пары (план × agenda item):

```
composed = score_initial + intent_delta + tempo_delta + W_intent × cdot
```

где:

- `score_initial` — score после `ItemScoringStage` pass (до остальных pipeline-stages применения мутаций)
- `intent_delta` = `contrib_intent(item)` − `contrib_intent(primary)` — разница вклада Intent фактора
- `tempo_delta` — аналогично для TempoGain
- `W_intent` — вес Intent фактора из `AxisProfile.factor_weights`
- `cdot` = `considerations.weighted_dot(band_weights)` — нормированное скалярное произведение

`band_weights` хардкожены в `PriorityBand::weights()` и не читаются из TOML (первая волна).

## Fallback асимметрия

Если план не имеет eligible agenda items (все `per_item[i].eligible = false`), `ann.score` остаётся pipeline-значением, `ann.agenda_item = None`. В логах это называется «unattributed» — sanity сигнал для mining.

## Сериализация (schema v32)

- `ActorTickEvent.band: Option<PriorityBand>` — `None` на skip-пути
- `ActorTickEvent.band_reason: Option<BandReason>` — структурированная причина band assignment
- `ActorTickEvent.agenda: Vec<AgendaItemLog>` — легковесная форма (kind, target, raw_score, considerations, reason)
- `PlanAnnotation.agenda_item: Option<u8>` — winning item index
- `PlanAnnotation.considerations_per_item: Vec<IntentConsiderations>` — overlay considerations из PickBestStage

## Mining-сигналы H1/H2 (`mine_ai_logs`)

**H1 — Band coverage:**

- Per-band tick count — базовая частота каждого band'а в corpus'е
- Winner-intent distribution per band — какой IntentKind побеждает в каждом band'е
- Per-axis consideration histograms (urgency / feasibility / leverage / safety / role_affinity / continuation_value) — распределения по всем agenda items

**H2 — Agenda-item win-rate:**

- Per band: какой item index (0 / 1 / 2) обычно побеждает
- Sanity check: NormalTactical не должен вырождаться в «всегда побеждает item 0» — это сигнал что N=2/3 expansion бесполезен или considerations-скоринг сломан

Подробнее — [`rework/step11_plan.md`](rework/step11_plan.md), [`rework/step11_8_design.md`](rework/step11_8_design.md), [`rework/step11_8_findings.md`](rework/step11_8_findings.md).
