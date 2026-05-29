# Adaptation Layer

*Источники: `src/combat/ai/adapt/{mod.rs,select.rs}` (типы + алгоритм) + stage-ы `pipeline/stages/{mode_selection,finalize}.rs` (шаг 11.0 split, R2 extraction).*

Слой между SANITY (мягкие штрафы) и CONTRACT (intent-coherence masks). Отвечает за **value-function reassessment**: если факты, обнаруженные после measurement+correction, делают текущий `TacticalIntent` неадекватным оценочной моделью плана — переключает **режим оценки** (`EvaluationMode`) для этого плана и пересчитывает его intent-column.

> **Stage layout (step 11.0).** Монолитный `AdaptationStage` расщеплён на:
> - `ModeSelectionStage` — определяет `EvaluationMode` для каждого плана (без мутации `ann.score`).
> - `FinalizeStage` — применяет режимы через `rescore_with_per_plan_modes` и обновляет `ann.score`.
> Между ними находится граница «никаких per-plan score изменений» — это упрощает тестирование и делает каждую фазу наблюдаемой.

## Зачем

Sanity работает с *ценой* (cost corrector). Intent определяет *функцию ценности*. Есть случаи, когда функция ценности сама становится неправильной по отношению к плану — например, план гарантирует смерть актора, значит `continue-to-exist value = 0`, и оценка «что я ещё хочу сохранить» неуместна. Раньше такие случаи лечились hard-маской `−∞` (lethal AoO) или rescue-веткой внутри `apply_protect_self` — оба костыли в неправильных слоях.

## Invariants

Слой узкий. Зафиксировано:

1. **ONE PASS** — вызывается один раз в `pick_action` (через `ModeSelectionStage` + `FinalizeStage`).
2. **FACTS ONLY** — триггеры только snapshot-факты (`expected_aoo_damage ≥ hp`, `plan_is_defensive`, `global_intent`). Никаких post-score сравнений.
3. **NO PENALTIES / NO MASKS** — слой только маппит `(plan → EvaluationMode)` и триггерит rescore intent-column. Не умножает, не обнуляет.
4. **IDEMPOTENT** — повторный вызов на уже адаптированном состоянии — no-op. `EvaluationMode` меняется ≤ 1 раз на план.
5. **CONTRACT-NEUTRAL** — не знает про contract masks. `ProtectSelfMaskStage` применяется ПОСЛЕ и только к планам с `mode = Default`.

## `EvaluationMode`

```rust
enum EvaluationMode { Default, LastStand, Flee }
```

`Default` использует глобальный `TacticalIntent` для скоринга intent-column. `LastStand` переиспользует `evaluate_last_stand_step` — оценивает «последнее полезное действие». `Flee` переиспользует `evaluate_flee_step` — юнит максимизирует дистанцию до ближайшего врага, offensive-касты подавлены (score `-1.0`), self-heal/self-buff разрешены (`+0.3`), Move оценивается дельтой расстояния. И `LastStand`, и `Flee` обходят глобальный intent целиком в `intent_score` (ранний return).

### `Flee` — особенности (forced mode)

В отличие от fact-driven `LastStand`, `Flee` **навязывается контентом**: boss-фаза с `ai_behavior = "flee"` (`PhaseDef`) → `apply_phase_ecs_writes` вешает компонент `AiBehaviorOverride { kind: Flee }` → `build_snapshot` проецирует его в `UnitAiCache.forced_mode: Option<EvaluationMode>`. `select_evaluation_modes` читает `active.forced_mode()` **первым** (highest precedence) и при наличии форсит режим на **все** планы, short-circuit-я ProtectSelf/ExpectedSelfLethal правила.

Forced-режим должен быть виден **обоим** scoring-путям, иначе agenda-композиция «отменит» его:
- **Base path** — `FinalizeStage` читает реальный `AdaptationData.mode` (а не `is_some() → LastStand`); для `Flee` дополнительно **re-stamp `score_initial`** на flee-rescored значение, т.к. формула `composed = score_initial + intent_delta + …` (`PickBestStage`) иначе протащила бы Default-offensive базу. Re-stamp keyed на `Flee` — `LastStand` (incidentally safe: его цель совпадает с offensive-базой) не трогаем.
- **Agenda path** — `ItemScoringStage` форсит `item_mode = forced_mode` для всех agenda-items (приоритетнее `IntentKind::LastStand`); `compute_plan_intent_sum` имеет ветку `Flee`, гоняющую каждый шаг через `evaluate_flee_step`.

Cornered edge-case: если ни один Move не увеличивает дистанцию (загнан в угол), все Move ≤ 0, пустой/skip-план = 0 → EndTurn без движения (не зависает).

## `AdaptationReason`

| Reason | Триггер | Gate | Mode | Horizon |
|---|---|---|---|---|
| `ExpectedSelfLethal { aoo_dmg, actor_hp }` | `expected_aoo_damage(plan) ≥ actor_hp` | `intent != ProtectSelf` | `LastStand` (per-plan) | **step-local** (AoO per-transition) |
| `ProtectSelfNoDefensive` | ни один план не `plan_is_defensive` | `intent == ProtectSelf` | `LastStand` (глобально) | — (spatial) |
| `ProtectSelfFutile { pending_dot, actor_hp }` | `pending_dot_before_next_action(active) ≥ hp` **AND** ни один план не `plan_has_self_rescue` | `intent == ProtectSelf`, defensive option ∃ | `LastStand` (глобально) | **end-of-turn** (`sim_snapshots.last()`) |
| `Forced { mode }` | `active.forced_mode().is_some()` (контент-фаза `ai_behavior`) | — (highest precedence) | `mode` (глобально, на все планы) | — (per-turn override) |

**Horizon per threat type.** AoO fires внутри шага → step-local rescue невозможна суффиксом → смотрим per-step AoO bleed. DoT, в движке с гарантией «только текущий актор меняет состояние в рамках хода», тикает на ходу *applier'а*, после окончания хода отравленного — значит правильный horizon для doom-rescue = конец полного плана (`sim_snapshots.last()`). Два разных типа угроз → два разных horizon'а в одном слое.

`ExpectedSelfLethal` под ProtectSelf не срабатывает: если есть defensive options и doom-check не фатален, contract прав — актор не должен сам себя ставить под смертельный AoO. Если defensive нет → `ProtectSelfNoDefensive` делает глобальный switch. Если defensive есть, но pending DoT ≥ hp и ни один план не спасает → `ProtectSelfFutile` делает глобальный switch.

**MVP scope `ProtectSelfFutile`**: gate только под `intent == ProtectSelf`. Doomed-актор, у которого `select_intent` выбрал не-ProtectSelf (например, ушёл на safe tile, urgency не триггернула), — граничный случай, покрывается при появлении replay-свидетельства.

«Expected» в названии `ExpectedSelfLethal` — потому что `expected_aoo_damage` это EV-оценка (sim живёт на EV без crit-fail), а не гарантия смерти в живом бою. `pending_dot_before_next_action` — детерминированный snapshot-факт, без EV-проекции.

## Логи / debug

Для каждого плана в JSONL:

- `evaluation_mode: "default" | "last_stand" | "flee"`
- `adaptation_reason: null | { kind: "expected_self_lethal", …} | { kind: "protect_self_no_defensive" } | { kind: "protect_self_futile", pending_dot, actor_hp } | { kind: "forced", mode }`
- `base_score` — score до adaptation
- `adapted_score` — финальный (= `score`)

Если `adaptation.modes[best_idx] != Default`, `IntentReason` выбранного плана оборачивается в `IntentReason::Adapted { prior, reason }`.

## Что MVP1 НЕ решает

MVP1 — **архитектурный refactor**. Он убирает lethal-AoO hard-mask и перестаёт душить self-lethal планы в `−∞` — они возвращаются в pool и становятся сравнимыми. **Экономику размена** — выгодно ли умереть ради убийства конкретной цели — закрывает MVP2 (см. [trade-economy.md](trade-economy.md)).
