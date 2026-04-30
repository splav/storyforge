# Шаг 11 — Priority bands + agenda + scorecard intent: декомпозиция на сабшаги

Декомпозиция в стиле step 7/8/9/10: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §11.

## Preamble

### Текущее состояние

`src/combat/ai/intent.rs::select_intent` (`:411-664`) — плоская if/else-лестница над `NeedSignals` + snapshot. Каждый блок генерирует кандидатов через локальный `consider(intent, score, reason)`; победитель выбирается аргмаксом по `score`. Лестница:

| Слой | Логика | Источник |
|---|---|---|
| PanicOverride (early return) | `self_preserve ≥ panic_threshold && danger > danger_panic` | NeedSignals + DifficultyProfile |
| ProtectSelf | `self_preserve > soft_threshold && danger > 0` | NeedSignals |
| ProtectAlly (healer-gated) | `most_wounded` ally < threshold(role.support) | snapshot scan |
| Taunt branch | `FORCES_TARGETING` enemy → FocusTarget+ApplyCC | tags |
| FocusTarget killable | `threat ≥ eff_hp && reach ≤ budget` | snapshot + threat |
| FocusTarget priority | best `target_priority` | snapshot |
| ApplyCC | `CAN_CC` → max DPR enemy | snapshot |
| SetupAOE | `HAS_AOE` && `cluster_count > 0` | snapshot |
| Reposition | `need_signals.reposition > floor` | NeedSignals |
| Stickiness modifier | `last_intent + continue_commitment` | AiMemory |
| ConserveResource bonus | `conserve_resource > threshold` для cheap intents | NeedSignals |

`select_intent` возвращает один `IntentChoice { intent, reason }`. Затем `pick_action` (`utility/mod.rs:200-395`) генерирует **один** pool планов через `generate_plans`, скорит их относительно этого единого intent через `score_plans_with_raw`, и прогоняет через pipeline (Viability → Sanity → Critics → Adaptation → ProtectSelf → KillableGate → Repair → Modifiers → PickBest).

`PlanAnnotation` сейчас несёт outcome/terminal/repair/viability/sanity/adaptation/contract/score/factors/critics. Лога band/agenda нет.

`AdaptationStage` (см. backlog **B3**) перерасчитывает `ann.score` из raw factors через `apply_adaptation` — стирает Sanity/Critics modifiers для всего pool'а при триггере LastStand. Step 11 вынужден решить эту проблему до bands, потому что band selection работает с post-adaptation scores.

### Проблемы текущей схемы

1. **Лестница хрупка к комбинированным сценариям**: low-HP актор с killable target и угрозой союзнику → один из веток выигрывает по абсолютному score, два других теряются. Нет «второго мнения».
2. **Score scale несовместим между ветками**: killable=1.2, priority=0.5..0.8, ApplyCC=0.8..0.9, Reposition=0.3..1.0 — magic numbers без общей семантики (urgency? leverage? feasibility?).
3. **Один pool на один intent**: если победил FocusTarget, но лучший доступный план на самом деле даёт больше leverage через ApplyCC — это никогда не сравнивается.
4. **Stickiness живёт в `consider()`**, не выделен как ось — дублируется с RepairAffinity (step 6) и continue_commitment (step 3).
5. **Adaptation B3 wipe** ломает заявленную семантику Sanity/Critics для adaptation-триггерных pool'ов.
6. **Taunt/PanicOverride обрабатываются как «жёсткое короткое замыкание»** (early return) — не отделены от обычных интентов в логе/анализе.

### Что закрывает шаг 11

1. **`PriorityBand` enum**: ForcedTargeting / CriticalSelfPreservation / HardRescueOpportunity / NormalTactical. Hard rules + threshold gate.
2. **`Agenda { items: Vec<AgendaItem>; band: PriorityBand }`** — top-N (N=2..3 в зависимости от band) кандидатов; каждый item = `{ kind, target, considerations, confidence, reason_breakdown }`.
3. **`IntentConsiderations { urgency, feasibility, leverage, safety, role_affinity, continuation_value }`** — 6 осей; источники: NeedSignals, ActionOutcomeEstimate, FactorTerminalScore, RepairAffinity. Поля `f32` 0..1.
4. **Per-agenda-item planning**: pool планов генерируется один раз (shared), затем для каждого agenda item плану присваивается per-item score (re-scoring через `score_plans_with_raw` с разным `intent`); лучший план каждого item конкурирует с лучшими других items.
5. **Лог в `PlanAnnotation.band` (top-level pool field) + `PlanAnnotation.agenda_item: Option<usize>`** — какой item этот план «обслуживает».
6. **Реархитектура `AdaptationStage`** (B3 закрывается как 11.0 pre-requisite): split на (a) mode selection, (b) initial finalize. Critics/Sanity больше не стираются.
7. **Schema bump v31→v32** atomic в финальном сабшаге.

### Что НЕ в scope шага 11

- **Mid-plan reflow (step 12)**, **TeamTasks (step 13)**.
- **TOML-configurable bands/agenda size** — composition в коде (step 7 invariant).
- **Новые need signals или terminal axes** — спецификация явно запрещает.
- **Замена `TacticalIntent` enum** — agenda item.kind переиспользует существующие варианты (FocusTarget/ApplyCC/ProtectSelf/...).
- **Полное удаление `select_intent`** — остаётся как legacy путь для тестов и для NormalTactical-band fallback в первой волне; deprecate в 11.6.
- **Portfolio search / MCTS** (B1).

## Зафиксированные решения по развилкам

**1. Band assignment — hybrid (hard rules + threshold).**

- `ForcedTargeting`: `snap.enemies_of(active.team).any(FORCES_TARGETING)` — hard rule. Перебивает всё.
- `CriticalSelfPreservation`: `need_signals.self_preserve ≥ panic_threshold && danger > danger_panic` — текущий PanicOverride gate.
- `HardRescueOpportunity`: `need_signals.rescue_ally ≥ tuning.thresholds.hard_rescue_threshold` (новый порог, default 0.7) AND `actor` имеет CAN_HEAL/CAN_RESCUE kit.
- `NormalTactical`: fallback. Покрывает FocusTarget/ApplyCC/SetupAOE/Reposition/обычный ProtectSelf без паники.

Bands evaluated в порядке Forced → CriticalSelf → HardRescue → Normal; first match wins.

**Альтернатива (1b):** scoring threshold для всех. Отвергнута: Forced/Panic — boolean по природе, scoring добавляет ложную «непрерывность».

**2. Agenda размер per-band, фиксирован.**

- ForcedTargeting: N=1 (taunter — единственный target).
- CriticalSelfPreservation: N=2 (ProtectSelf + best Reposition-away).
- HardRescueOpportunity: N=2 (ProtectAlly + FocusTarget на угрозу).
- NormalTactical: N=3 (best FocusTarget, best ApplyCC/SetupAOE, best Reposition или ProtectAlly preventive).

**Альтернатива (2b):** dynamic N по «entropy» соседних кандидатов. Отвергнута: усложняет тесты и mining без явного выигрыша на текущем content scope.

**3. Per-agenda-item planning — shared pool, re-score per item.**

`generate_plans` запускается **один раз**. Затем для каждого agenda item:

```
let (scores_i, raw_i) = score_plans_with_raw(&mut plans, &item.intent, scoring_ctx);
```

Per-item score хранится в `PlanAnnotation.per_item_scores: Vec<f32>` (длина = agenda.len). Финальный score = max по items, item-attribution = argmax index.

**Альтернатива (3a):** отдельный generate_plans на каждый item. Отвергнута: 3× cost на actor — критично для encounter с 6+ NPC. Generator текущей формы intent-агностичен (генерирует все viable plan skeletons), достаточно re-scoring.

**Альтернатива (3c):** один scoring проход с «multi-intent» factor. Отвергнута: factor stack уже сложен (Step 8); добавление multi-intent аксона ломает factor invariants.

**4. `IntentConsiderations` — flat struct, не enum.**

```rust
#[derive(Default, Clone, Copy, Serialize, Deserialize)]
pub struct IntentConsiderations {
    pub urgency: f32,        // self_preserve, rescue_ally, finish_target
    pub feasibility: f32,    // outcome.p_action_success, viability score
    pub leverage: f32,       // outcome.enemy_damage_norm, terminal.secure_kill
    pub safety: f32,         // 1 - outcome.self_damage_ratio - terminal.exposure_at_end
    pub role_affinity: f32,  // role.support × intent (healer→ProtectAlly, dps→FocusTarget)
    pub continuation_value: f32, // RepairAffinity.continuation_severity + need.continue_commitment
}
```

Composition score = взвешенная сумма с весами per-band (например, urgency dominates в CriticalSelf, leverage в Normal). Веса — захардкожены в `PriorityBand::weights()`.

**Альтернатива (4b):** enum + StepFactor-style table. Отвергнута: 6 осей — фиксированный набор без планируемого расширения; struct проще для агрегации.

**5. `select_intent` deprecation — non-destructive.**

Новая функция `select_band_and_agenda` в `intent.rs` — рядом со старым `select_intent`. `pick_action` переключается на неё. `select_intent` остаётся как private helper для NormalTactical-band fallback в 11.1 (одиночный кандидат) и для существующих тестов (`focus_target_*`); удаление — в 11.6 после всех сабшагов или в backlog.

**6. B3 (adaptation rescore wipe) — встроен как 11.0 prerequisite.**

Обоснование выбора (a): band selection в 11.1 будет читать post-adaptation scores; если adaptation wipe'ает Critics, band «HardRescueOpportunity» с лучшим планом-героем рискует выбрать критически штрафанутый Critics-ом план как победителя. Закрываем сначала B3, потом наслаиваем bands.

11.0 — реархитектура: `apply_adaptation` разделён на `select_evaluation_modes(pool, ctx) → Vec<EvaluationMode>` (без rescore) + `finalize_with_modes(pool, modes)`. Pipeline: Viability → ModeSelectionStage → finalize → Sanity → Critics → ProtectSelf → KillableGate → Repair → Modifiers → PickBest. Старый `AdaptationStage` удаляется/превращается в `ModeSelectionStage`.

**7. Adaptation timing относительно band — band ПОСЛЕ adaptation.**

Band selection — на этапе intent (до plan generation). Adaptation работает per-plan internally уже после bands проинициализированы. Это упрощает: band — once-per-actor, adaptation — per-plan mode dispatch.

**8. Schema bump v31→v32 atomic в 11.6.**

Добавляются: `pool.band: PriorityBand`, `pool.agenda: Vec<AgendaItem>`, `PlanAnnotation.agenda_item: Option<u8>`, `PlanAnnotation.considerations: [IntentConsiderations; ≤3]` per agenda item. v31 logs дают `LogError::UnsupportedSchema` (clean break, как в 8.x/10.4).

**9. ai_scenarios — fixture per band.** Новые: `band_forced_targeting_taunt`, `band_critical_self_panic`, `band_hard_rescue_low_hp_ally_threatened`, `band_normal_tactical_default`. Assertions на `pool.band == <Kind>` + agenda content.

**10. `continue_commitment` ось vs RepairAffinity bonus — интегрированы.**

`continuation_value` = `0.5 × need_signals.continue_commitment + 0.5 × repair_affinity.severity_score` (нормализовано). RepairBonus modifier (Step 6) **остаётся** в PlanModifiersStage — он работает на post-pick relax-fallback. `continuation_value` ось работает на pre-pick agenda evaluation. Не дубликат — разные слои.

**11. `EvaluationMode` после step 11 — остаётся 2 (Default/LastStand).**

Bands не вытесняют EvaluationMode: band — про intent selection, EvaluationMode — про factor weights в финализации scoring. Они ортогональны. После 11.0 `apply_adaptation` мутирует только modes, не scores.

## Природа gate'ов

- **11.0** (B3 reorder): golden 0/N. Behavior identical для не-adaptation-триггерных тиков; для adaptation-pool'ов — Critics/Sanity multipliers теперь сохраняются; ожидаемо ≤10% планов меняют score (но не winner).
- **11.1** (Band assignment): golden 0/N для NormalTactical (большинство); ForcedTargeting/CriticalSelf обязаны побайтно совпадать с прежним PanicOverride/Taunt путём.
- **11.2** (Agenda construction): golden 0/N — только observability, agenda не влияет на pick.
- **11.3** (Considerations scoring): observability-only, agenda пока не управляет планами.
- **11.4** (Per-agenda-item planning + cross-comparison): **тут возможен behavioral diff**, до 25% планов могут поменять winner. Per-entry attribution.
- **11.5** (`select_intent` integration): зеркалит 11.4 — должен быть byte-identical к 11.4.
- **11.6** (schema v32 + cleanup + mining): v32 round-trip 0/N.

Per-сабшаг unit-тесты:
- `band_<kind>_fires_on_canonical_case`.
- `band_<kind>_passes_to_<next>_when_threshold_below`.
- `agenda_<band>_emits_n_items`.
- `considerations_<axis>_<src>_normalized_to_unit_range`.
- `pick_best_attributes_winner_to_agenda_item`.

## Сабшаги

### 11.0. Adaptation reorder (B3 prerequisite)

**Scope.**

`src/combat/ai/planning/adaptation.rs` — split `apply_adaptation`:
- `select_evaluation_modes(plans, raw_factors, intent, ctx) → AdaptationOutcome { modes: Vec<EvaluationMode>, reasons: Vec<Option<AdaptationReason>> }` — только выбирает modes, **не** мутирует scores.
- Финализация скоринга через новую `finalize_scores_with_modes(plans, raw_factors, modes, ctx)` — заменяет текущий внутренний rescore.

`pipeline/stages/`:
- `mode_selection.rs::ModeSelectionStage` — заменяет `AdaptationStage`. Записывает `ann.adaptation` (mode + reason + original_score), но **не** трогает `ann.score`.
- `finalize.rs::FinalizeStage` (новый) — применяет finalize_scores_with_modes, перезаписывает `ann.score` на base mode-aware score.

Pipeline order в `pick_action`:
```
Viability → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask → KillableGate → RepairAffinity → PlanModifiers → PickBest
```

Базовый initial scoring блок (`score_plans_with_raw` + write `ann.score/factors`) остаётся, но Finalize теперь — отдельный stage; initial pass даёт «Default-mode» score, Finalize пересчитывает только если ModeSelection поменял mode для плана.

**Юнит-тесты:** `mode_selection_does_not_mutate_score`, `finalize_applies_per_plan_modes`, `critics_survive_through_adaptation_path` (regression test для B3), `winner_unchanged_for_non_adaptation_pools`.

**Gate.** Golden ≤10% diff на adaptation-trigger тиках, attribution: only adaptation pools, score delta == lost-Critics multiplier. Cargo all green. ai_scenarios re-baseline.

**Эстимейт:** 1.5 дня.

---

### 11.1. Band assignment

**Scope.**

`src/combat/ai/intent/bands.rs` (новый):

```rust
pub enum PriorityBand { ForcedTargeting, CriticalSelfPreservation, HardRescueOpportunity, NormalTactical }
pub struct BandWeights { pub urgency: f32, pub feasibility: f32, pub leverage: f32,
                         pub safety: f32, pub role_affinity: f32, pub continuation_value: f32 }
impl PriorityBand { pub fn weights(self) -> BandWeights { /* hardcoded per band */ } }

pub fn assign_band(active: &UnitSnapshot, snap: &BattleSnapshot, maps: &InfluenceMaps,
    needs: &NeedSignals, difficulty: &DifficultyProfile, tuning: &AiTuning) -> (PriorityBand, BandReason)
```

`BandReason` — sum-type по варианту, аналогичен `IntentReason`. Сериализуется. `tuning.thresholds.hard_rescue_threshold` — новый порог (default 0.7).

`pick_action` вычисляет band до `select_intent`. Лога ещё нет (добавим в 11.6); пока используем band только для маршрутизации в 11.5. В 11.1 band вычисляется и **отбрасывается** — golden 0/N (никакого behavior change).

**Юнит-тесты:** 4 (по одному на band-вариант) + `band_priority_order_forced_beats_critical`.

**Gate.** Golden 0/N (band не используется).

**Эстимейт:** 1.0 день.

---

### 11.2. Agenda construction

**Scope.**

`src/combat/ai/intent/agenda.rs` (новый):

```rust
pub struct AgendaItem {
    pub kind: IntentKind,
    pub target: Option<Entity>,
    pub raw_score: f32,           // legacy score из select_intent
    pub considerations: IntentConsiderations,  // populated в 11.3
    pub reason: IntentReason,
}
pub struct Agenda { pub band: PriorityBand, pub items: SmallVec<[AgendaItem; 3]> }

pub fn build_agenda(band: PriorityBand, ...) -> Agenda
```

Реализация: для каждого band-варианта свой builder. Внутри переиспользуется логика `select_intent` (вызывается с фильтром по band-разрешённым intent kinds) — собирается top-N по `raw_score`. Considerations пока default. Agenda вычисляется, но не используется (golden 0/N).

**Юнит-тесты:** 4 (`agenda_<band>_emits_n_items`) + `agenda_skips_intents_unreachable_in_band`.

**Gate.** Golden 0/N.

**Эстимейт:** 1.0 день.

---

### 11.3. Considerations scoring

**Scope.**

`src/combat/ai/intent/considerations.rs` (новый):

```rust
pub fn compute_considerations(item: &AgendaItem, plan_for_item: Option<&PlanAnnotation>,
    needs: &NeedSignals, role: RoleAxes, repair: &RepairAffinity) -> IntentConsiderations
```

Источники по осям:
- `urgency`: `needs.self_preserve` для ProtectSelf, `needs.rescue_ally` для ProtectAlly, `needs.finish_target` для FocusTarget на killable, ...
- `feasibility`: `viability.adjusted_score` или `outcomes[0].p_action_success` (где есть).
- `leverage`: для Cast-планов — `outcome.enemy_damage_norm` + `terminal.secure_kill`; для Move-only — terminal.line_actionability.
- `safety`: `1 - max(outcome.self_damage_ratio, terminal.exposure_at_end)` (clamp ≥0).
- `role_affinity`: лookup-таблица `(IntentKind × RoleAxes) → f32`. Healer × ProtectAlly = 1.0, DPS × FocusTarget = 1.0, healer × FocusTarget = 0.3.
- `continuation_value`: `0.5 × needs.continue_commitment + 0.5 × repair.severity_score` (clamp).

В 11.3 considerations считаются и пишутся в `agenda.items[i].considerations` для observability; pick — всё ещё legacy путь.

**Юнит-тесты:** 12 (по 2 на ось — границы 0/1 + middle).

**Gate.** Golden 0/N (observability only).

**Эстимейт:** 1.5 дня.

---

### 11.4. Per-agenda-item planning + cross-comparison

**ВНИМАНИЕ:** это первый сабшаг с реальным behavior change. До этого 11.0–11.3 были infrastructure-only (golden 0/N). 11.4 включает agenda в scoring loop и допускает ≤25% diff.

**Цель.** Заменить single-intent ranking на multi-item composed ranking: для каждого плана оценить пригодность под **каждый** agenda item; финальный winner = плот с наибольшим composed score, ему атрибутируется конкретный item, который "оправдал" его выбор.

#### Архитектурное решение

**Ключевой инсайт:** intent влияет на pipeline **только** через два фактора — `PlanFactor::Intent` и `PlanFactor::TempoGain` (см. `rescore_with_per_plan_modes` в `scorer.rs`). Все остальные факторы (offensive/defensive/survival/raw outcomes) — intent-агностичны. Multipliers в Sanity/Critics/PlanModifiers тоже intent-агностичны, кроме двух:
- **`ProtectSelfMaskStage`** — fires только под intent=ProtectSelf. Маскирует non-defensive планы в `-∞`.
- **`KillableGateStage`** — fires только под intent=FocusTarget. Маскирует non-killable планы в `-∞`.

Это требует: per-item scoring должен учитывать intent-specific masks. Иначе план, замаскированный под Default-intent, не сможет участвовать в re-evaluation под другим item.

**Решение — three-layer model:**

1. **Layer A: shared base** — не зависит от intent. Один проход на pool: outcomes, terminal, raw factors кроме `Intent`/`TempoGain`. Сюда же Viability (intent-agnostic gate на reachability/AP).
2. **Layer B: per-item** — intent-зависимое. На каждый item вычисляются: `Intent`/`TempoGain` factors, ProtectSelfMask (если item.kind == ProtectSelf), KillableGate (если item.kind == FocusTarget). Хранится в `PlanAnnotation.per_item: Vec<PerItemEval>`.
3. **Layer C: shared multipliers** — Sanity, Critics, PlanModifiers, RepairAffinity. Применяются к **base score**, потом распространяются на per-item scores через ratio. Эти стадии работают на ann.score один раз; ratio сохраняется и применяется ко всем per-item scores при composition.

#### Концептуальная формула

```
base_score(plan) = finalize_factors(raw_minus_intent) × (composite of intent-agnostic factors)
multiplier_ratio(plan) = ann.score_after_pipeline / ann.score_initial   # captures Sanity × Critics × Modifiers
per_item_score(plan, i) = base_score(plan) × intent_factor(plan, item.kind) × tempo_factor(plan, item.kind)
                          × intent_specific_mask(plan, item.kind)        # 1.0 or -∞ from ProtectSelf/Killable
final_for_item(plan, i) = per_item_score(plan, i) × multiplier_ratio(plan) × consideration_dot(item, weights)

ann.agenda_item = argmax_i final_for_item(plan, i)
ann.score = max_i final_for_item(plan, i)
```

Где `consideration_dot(item, weights) = Σ axis: weights[axis] × item.considerations[axis]` (нормированная свёртка по 6 осям).

#### Конкретная имплементация

**Шаг 1.** В `pick_action` после `build_agenda + compute_considerations`:

```rust
// (a) generate_plans — один раз
let mut plans = generate_plans(...);

// (b) initial scoring через ОДИН представительский intent. Выбор:
//     primary_item = item с максимальным consideration_dot.
//     Это даёт baseline для multipliers; multiplier_ratio будет применён к остальным.
let primary_idx = agenda.items.iter().enumerate()
    .max_by(|(_, a), (_, b)| a.considerations.dot(weights).partial_cmp(&b.considerations.dot(weights)).unwrap())
    .map(|(i, _)| i)
    .unwrap_or(0);
let primary_intent = agenda.items[primary_idx].intent_for_scoring();

let (initial_scores, initial_raw) = score_plans_with_raw(&mut plans, &primary_intent, &ctx);

// (c) сохранить initial_score в ann.score_initial для дальнейшего ratio
for (ann, &s) in pool.annotations.iter_mut().zip(initial_scores.iter()) {
    ann.score = s;
    ann.score_initial = s;  // NEW field в PlanAnnotation, документировать
}
```

**Шаг 2.** Между `ViabilityStage` и `ModeSelectionStage` — новый `ItemScoringStage`:

```rust
impl PlanStage for ItemScoringStage {
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let agenda = ctx.agenda; // прокинуть Agenda через StageCtx
        for (plan_idx, ann) in pool.annotations.iter_mut().enumerate() {
            let plan = &pool.plans[plan_idx];
            for (item_idx, item) in agenda.items.iter().enumerate() {
                let intent_factor = compute_plan_intent_sum(plan, &item.intent, ctx.scoring);
                let tempo_factor = compute_plan_tempo_gain(plan, &item.intent, ctx.scoring);
                let intent_mask = compute_intent_mask(plan, &item.intent, &ann.factors); // -∞ или 1.0
                ann.per_item[item_idx] = PerItemEval {
                    intent_factor,
                    tempo_factor,
                    intent_mask,
                };
            }
        }
    }
}
```

`compute_intent_mask` — pure function, читает существующую логику ProtectSelfMask и KillableGate, возвращает `f32` множитель (0.0 при mask, 1.0 иначе). Реализация: переиспользует `plan_is_defensive` (из sanity.rs) для ProtectSelf и `is_killable_target` (если есть) для FocusTarget.

**Шаг 3.** Pipeline order **(новый):**

```
ItemScoringStage → ModeSelection → Finalize → Sanity → Critics → ProtectSelfMask → KillableGate → RepairAffinity → PlanModifiers → PickBest
```

Где `ProtectSelfMaskStage` / `KillableGateStage` остаются — но **только** для primary_intent (одного представителя). Per-item masks уже посчитаны в ItemScoringStage. Альтернативно: убрать их из pipeline в 11.4 и полагаться только на per-item masks. **Решение:** оставить — это backup для primary item flow и для NormalTactical band где agenda — N=1.

**Шаг 4.** `PickBestStage` modifies:

```rust
impl PlanStage for PickBestStage {
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let agenda = ctx.agenda;
        let weights = agenda.band.weights();
        let mut best_idx = 0;
        let mut best_composed = f32::NEG_INFINITY;
        for (plan_idx, ann) in pool.annotations.iter_mut().enumerate() {
            // multiplier_ratio = ann.score / ann.score_initial (captures Sanity/Critics/Modifiers)
            let ratio = if ann.score_initial.abs() > f32::EPSILON {
                ann.score / ann.score_initial
            } else { 1.0 };
            let mut item_best = f32::NEG_INFINITY;
            let mut item_best_idx = 0_u8;
            for (item_idx, item) in agenda.items.iter().enumerate() {
                let per_item = &ann.per_item[item_idx];
                // Reconstruct per-item base score from raw factors with item's Intent/TempoGain
                let base_for_item = ann.factors.with_intent_tempo(per_item.intent_factor, per_item.tempo_factor)
                    .finalize_intent_aware(&ctx.scoring);
                let masked = base_for_item * per_item.intent_mask;
                let cdot = consideration_dot(&item.considerations, &weights);
                let composed = masked * ratio * cdot;
                if composed > item_best {
                    item_best = composed;
                    item_best_idx = item_idx as u8;
                }
            }
            ann.score = item_best;
            ann.agenda_item = Some(item_best_idx);
            if item_best > best_composed { best_composed = item_best; best_idx = plan_idx; }
        }
        pool.annotations[best_idx].chosen = true;
    }
}
```

(Псевдокод — реальная имплементация подгоняется под `PlanFactorValues` API. Если `with_intent_tempo` нет — добавь helper в `factors/`.)

#### `PlanAnnotation` extensions (НЕ schema bump — поля локальные)

Новые поля в `PlanAnnotation`:
- `pub score_initial: f32` — score сразу после initial scoring, до Sanity/Critics. Используется для ratio.
- `pub per_item: Vec<PerItemEval>` (length == agenda.items.len()) — кэш per-item scoring.
- `pub agenda_item: Option<u8>` — winning item attribution.

`PerItemEval`:
```rust
#[derive(Default, Clone, Copy, Debug)]
pub struct PerItemEval {
    pub intent_factor: f32,
    pub tempo_factor: f32,
    pub intent_mask: f32, // 0.0 (masked) or 1.0
}
```

Сериализация: добавить `#[serde(default)]` для backward-compat. **Schema bump v31→v32 — это 11.6**, в 11.4 поля живут только в runtime.

#### `Agenda` через `StageCtx`

`StageCtx` сейчас несёт `intent`, `intent_reason`. Добавь:
- `pub agenda: Option<&'s Agenda>` — Some когда per-item active, None для legacy путей (если 11.4 захватит все pipeline'ы — None не нужен; иначе guard).

#### Edge cases (важные!)

1. **Empty agenda** — `agenda.items.is_empty()`. Возможно для NormalTactical если `select_intent` не вернул winner. Pipeline должен gracefully fallback на legacy single-intent. Решение: при empty agenda, ItemScoringStage no-op, PickBest использует ann.score напрямую (как в 11.0–11.3).

2. **Agenda с одним item (N=1)** — типично для ForcedTargeting и NormalTactical (пока 11.5 не сделает N=3). Composition должна collapse к single-intent: `final = base × ratio × cdot`. Поведение должно быть byte-identical к pre-11.4 пути для N=1 случая (после нормализации `cdot = 1.0` если только один item — см. invariant ниже).

3. **All items masked** — например, ProtectSelf band с одним ProtectSelf item, но нет defensive плана. Все per_item masked в `-∞`. PickBest должен fallback на best non-masked item — или если все masked, фолбек на legacy choice. В этом случае `ProtectSelfNoDefensive` adaptation уже сработал в ModeSelection, и Finalize пересчитал в LastStand mode — что должно дать non-zero scores. Tests на этот flow.

4. **Mode-aware finalize × per-item** — после Finalize, raw factors имеют mode-aware Intent/TempoGain. ItemScoringStage compute свой Intent/TempoGain для каждого item — какой mode использовать? **Решение:** ItemScoringStage runs ДО ModeSelection (он первый в pipeline), значит mode = Default. После ModeSelection/Finalize, `ann.factors` имеет mode-aware Intent для **primary** intent. Per-item evals **не** учитывают LastStand — это имеется в виду как ограничение первой волны. Документировать. Backlog: Mode × per-item interaction.

5. **`multiplier_ratio` non-finite** — если `score_initial == 0.0` или `ann.score == -∞`, ratio undefined. Guard через `if score_initial.abs() > EPSILON { ratio = ann.score / score_initial } else { 1.0 }`.

#### Composition normalization

Без нормализации `consideration_dot` уязвим к артефактам если все consideration оси высокие — composed score становится больше base score, искажая ratio с другими планами. **Решение:** нормировать так, чтобы для **default считераций** (1,1,1,1,1,1) и default band weights composition collapse'ился к raw base score.

```rust
let raw_dot = Σ axis: weights[axis] × considerations[axis];
let weight_sum = Σ axis: weights[axis];
let normalized_dot = raw_dot / weight_sum.max(EPSILON);  // ∈ [0, 1] approximately
```

Тесты: `composition_collapses_to_base_when_considerations_uniform` — pin invariant.

#### `IntentConsiderations::dot` helper

Добавить:
```rust
impl IntentConsiderations {
    pub fn weighted_dot(&self, w: &BandWeights) -> f32 {
        let raw = self.urgency * w.urgency + self.feasibility * w.feasibility
                + self.leverage * w.leverage + self.safety * w.safety
                + self.role_affinity * w.role_affinity + self.continuation_value * w.continuation_value;
        let sum = w.urgency + w.feasibility + w.leverage + w.safety + w.role_affinity + w.continuation_value;
        if sum > f32::EPSILON { raw / sum } else { 1.0 }
    }
}
```

#### Plan-aware overlay в `compute_considerations`

В 11.3 4 оси (feasibility/leverage/safety и часть continuation_value) возвращали defaults при `plan_for_item = None`. **В 11.4 их нужно доопределить** — потому что после ItemScoringStage у нас есть per-item data, и compute_considerations можно вызвать с `Some(ann)` для уточнения осей. Решение:

- В `ItemScoringStage` (после initial fill of per_item), цикл повторного вызова `compute_considerations(item, Some(ann), needs, role, repair_aff_for_plan)` — overlay на agenda-item considerations **per-plan**.
- Это значит **considerations per-plan-per-item**. Хранить в `PerItemEval.considerations: IntentConsiderations`. Item-level (из 11.3) остаётся как fallback baseline.
- В PickBest composition использовать per-plan-per-item considerations (более точные).

Для repair_affinity per plan: после RepairAffinityStage `ann.repair_affinity` populated. Идеально вызывать compute_considerations overlay **после** RepairAffinityStage, не раньше. Решение: `OverlayConsiderationsStage` между RepairAffinity и PlanModifiers (или прямо в PickBest перед composition).

**Альтернатива (упрощение для 11.4):** оставить considerations item-level (без plan overlay) в 11.4 и сделать plan-aware overlay в backlog или в 11.5. Тогда 11.4 имеет меньший scope. **Решение для плана:** plan-aware overlay **в scope 11.4**, потому что без него `feasibility`/`leverage`/`safety` всегда default'ы — composition теряет свою точность, и behavior diff будет не атрибутируем.

#### Tests (обязательные, ≥10)

В `pipeline/stages/pick_best.rs::tests` или `intent/composition.rs::tests`:

1. `composition_collapses_to_base_when_considerations_uniform` — pin normalization invariant.
2. `multi_item_pick_attributes_to_winning_item` — pool с двумя планами, оба viable; разные item's win — каждый план получает item attribution.
3. `single_item_agenda_byte_identical_to_legacy` — N=1 agenda → final score равен (pre-11.4) score within epsilon.
4. `consideration_weights_dominate_in_critical_self_band` — band weights сильно сдвигают urgency → ProtectSelf wins даже при низком raw score.
5. `intent_mask_protects_self_for_non_defensive_plans` — ProtectSelf agenda item + non-defensive plan → masked, final=-∞ для этого item; план может выиграть через другой item.
6. `intent_mask_killable_gate_for_non_killable_focus_target` — аналогично для KillableGate.
7. `multiplier_ratio_preserves_sanity_critic_effect` — план с Critics hit получает соответствующий drop в final score через ratio.
8. `empty_agenda_falls_back_to_legacy_pipeline` — пустая agenda → ItemScoringStage no-op → behavior как 11.3.
9. `agenda_item_attribution_persisted_in_annotation` — после PickBest `ann.agenda_item` соответствует winning item.
10. `plan_aware_overlay_changes_feasibility_axis` — compute_considerations с Some(ann) → feasibility != 1.0 default.
11. `mode_lastestand_does_not_break_per_item_scoring` — ProtectSelfNoDefensive triggers, plans rescored in LastStand, but per_item scores still computed (warning: see edge case 4).

В `intent/considerations.rs::tests` — добавить с-plan тесты для feasibility/leverage/safety теперь когда они реально читают plan data.

#### Risks & mitigations

| Риск | Вероятность | Mitigation |
|---|---|---|
| **Multiplier ratio broken** для plan где Critics/Sanity делают score → 0.0 → div by zero | средняя | guard на EPSILON; tests на pathological ratio |
| **Per-item factor расходится с finalize** (если intent_factor/tempo_factor compute не совпадает с finalize_scores семантикой) | высокая | Reuse `compute_plan_intent_sum`/`compute_plan_tempo_gain` напрямую, не дублировать |
| **Intent mask дублирует ProtectSelfMaskStage** | средняя | После 11.4 решить: оставить Stage в pipeline (для legacy single-item path) или убрать. Если оставить — добавить тест что mask из ItemScoringStage и Stage'а согласованы для primary item |
| **Agenda через StageCtx ломает API** существующих stage'ов | низкая | Сделать `agenda: Option<...>` — старые stages не читают и не ломаются |
| **`composition_collapses_to_base_when_considerations_uniform`** не выполняется из-за плавающей точки | низкая | Tolerance в тесте (≤1e-4) |
| **Adaptive replacement of `select_intent`** — 11.4 ещё не убирает `select_intent`, но build_agenda для NormalTactical зовёт его. Risk: select_intent под Default-mode с глобальным intent VS per-item scoring под item.intent — расхождение | средняя | В 11.4 для NormalTactical agenda item.intent == select_intent.intent (по построению в 11.2). Composition должна давать тот же финальный pick как legacy в N=1 случае — pin тестом #3 |
| **Performance regression** — N=3 items × X plans = 3X scoring passes | низкая (M=N*X где N=3) | Compute_plan_intent_sum дешёв (один проход по plan steps). Профилировать post-11.4 если будет видно |

#### Что **не делать** в 11.4

- **Не двигать ProtectSelfMaskStage / KillableGateStage** — оставить, они работают на primary item (через ann.score path); per-item masks дополнительные в ItemScoringStage.
- **Не убирать `select_intent`** — он вызывается из build_agenda для NormalTactical (N=1 fallback). Удаление — 11.5.
- **Не трогать SanityRule enum / CriticsKind** — Critics/Sanity intent-агностичны, не зависят от per-item intents.
- **Не делать schema bump** — `agenda_item`/`per_item`/`score_initial` локальные runtime поля; сериализация — 11.6.
- **Не вводить ranking-tuning** для consideration weights — захардкодены в `BandWeights`.
- **Не делать commit.**

#### Эстимейт

**3.0 дня** (увеличен с 2.5 из-за plan-aware overlay scope и edge cases). Самый рискованный сабшаг шага 11. Рекомендуется выделить отдельную сессию.

**Gate.** Golden review per-entry. Допустимо ≤25% diff'ов; каждый — атрибутируется к agenda-item swap (winner был low-rank legacy choice). По-fixture снимки для 6 missions corpus'а до/после. Все ≥10 unit-тестов зелёные, ≥3 тестов из 11.3 (с-plan для feasibility/leverage/safety) обновлены и зелёные.

---

### 11.5. `select_intent` integration cleanup

**Scope.**

Удалить дублирование в `select_intent`: новая ветка `NormalTactical` band вызывает урезанный `select_intent_normal` (только FocusTarget/ApplyCC/SetupAOE/Reposition без panic/taunt/ally guards — те живут в build_agenda для других bands). Старый `select_intent` остаётся как `#[deprecated]` с тестовым перенаправлением.

`pick_action` финальный shape:

```
1. assign_band → BandReason
2. build_agenda(band, ...) → Agenda
3. compute_considerations per item → enrich agenda
4. generate_plans (once)
5. score_plans_with_raw per item (re-score loop)
6. pipeline (Viability → ModeSel → Finalize → Sanity → Critics → ... → PickBest)
7. PickBest reads composed score, writes agenda_item attribution
```

Удаление: `IntentReason::PanicOverride`/`TauntForced`/`TauntCc` → переносятся в `BandReason`. Stickiness logic из `consider()` мигрирует в `continuation_value` ось (уже сделано в 11.3) — удалить из `consider`.

**Юнит-тесты:** все существующие `focus_target_*` в `intent.rs::tests` адаптированы (ожидают NormalTactical band + agenda с FocusTarget item). 0 удалённых тестов.

**Gate.** Golden 0/N относительно 11.4.

**Эстимейт:** 1.5 дня.

---

### 11.6. Schema v31→v32 + mining + docs

**Scope.**

- `PlanAnnotation.agenda_item: Option<u8>`, `PlanAnnotation.considerations_per_item: SmallVec<[IntentConsiderations; 3]>` сериализуются.
- `ScoredPool` (или wrapping `actor_tick`): `band: PriorityBand`, `agenda: Vec<AgendaItemLog>` (легковесная сериализационная форма AgendaItem без full PlanAnnotation). Schema v32.
- `bin/replay_ai_log.rs` / `bin/mine_ai_logs.rs` обновляются: parsing v32 only.
- Mining-секция **H1** (band coverage): per-band tick count, per-band winner-intent distribution, per-axis consideration histograms.
- Mining **H2**: agenda-item win-rate (какой item обычно побеждает) per band.
- Документация: `docs/ai.md` секция «Bands & Agenda» (новая, аналогично «Critics layer» step 10).

**Юнит-тесты.** Round-trip v32, mining H1/H2 voiceprints.

**Gate.**
- v32 round-trip 0/N на rebuilt corpus.
- Mining H1 — все 4 bands с non-zero coverage (если corpus рандомный, может потребоваться curated fixture-based corpus).
- Mining H2 — sanity check: NormalTactical agenda items распределены без вырождения в один.

**Эстимейт:** 1.5 дня.

---

## Итого

| # | Шаг | Эстимейт | Gate |
|---|---|---|---|
| 11.0 | Adaptation reorder (B3) | 1.5 | golden ≤10% diff (only adaptation pools), Critics survive |
| 11.1 | Band assignment (computed, unused) | 1.0 | golden 0/N |
| 11.2 | Agenda construction (built, unused) | 1.0 | golden 0/N |
| 11.3 | Considerations scoring (observability) | 1.5 | golden 0/N |
| 11.4 | Per-agenda-item planning + cross-comparison | 2.5 | golden ≤25% diff, attributed |
| 11.5 | `select_intent` integration cleanup | 1.5 | golden 0/N vs 11.4 |
| 11.6 | Schema v31→v32 + mining + docs | 1.5 | v32 round-trip 0/N, H1/H2 voiceprints |

**Суммарно ~10.5 дней.**

## Критические файлы

- `src/combat/ai/intent.rs` — `select_intent` deprecation, BandReason migration.
- `src/combat/ai/intent/{bands,agenda,considerations}.rs` (новые) — band/agenda/considerations модели.
- `src/combat/ai/planning/adaptation.rs` — split `apply_adaptation` на mode-selection + finalize (11.0).
- `src/combat/ai/pipeline/stages/{mode_selection,finalize}.rs` (новые) — replace AdaptationStage; reorder pipeline.
- `src/combat/ai/utility/mod.rs::pick_action` — главный integration point; per-item scoring loop, composed-score PickBest.
- `src/combat/ai/outcome/mod.rs` — `PlanAnnotation.agenda_item` + `considerations_per_item`; ScoredPool band/agenda fields.
- `src/combat/ai/log.rs` — schema v31→v32 в 11.6.
- `src/bin/{replay_ai_log,mine_ai_logs}.rs` — v32 parsing + mining H1/H2.
- `tests/ai_scenarios/snapshots/band_*` — 4 новых fixtures.
- `docs/ai.md` — секция Bands & Agenda.

## Что откладывается

- **TOML-configurable band weights / agenda sizes** — захардкожено в первой волне; миграция в `AiTuning` — backlog после mining-калибровки.
- **Dynamic agenda size** (по entropy кандидатов) — backlog.
- **Удаление `select_intent`** — оставлен deprecated после 11.5; полное удаление — backlog шаг 12+.
- **Per-agenda-item отдельный generator pass** — отвергнуто (см. развилка 3); если mining покажет, что shared pool не хватает разнообразия для HardRescue band — пересмотреть.
- **Encounter-specific bands** (например, `BossPhaseTransition`) — backlog для step 14.
- **Composition weights через ranking-tuning** — backlog B2.

## Чего не делать в шаге 11

- **Не вводить новые need signals или terminal axes** — спецификация явно запрещает.
- **Не менять `TacticalIntent` enum** — agenda item.kind переиспользует существующее.
- **Не делать TOML-configurable bands** в первой волне (step 7 invariant).
- **Не заходить в mid-plan reflow** (step 12 territory).
- **Не делать per-agenda-item отдельный generate_plans pass** — performance regression; см. развилка 3.
- **Не удалять `EvaluationMode`** — он остаётся ортогональной осью.
- **Не объединять RepairBonus modifier и continuation_value** — разные слои pipeline (см. развилка 10).
- **Не делать B3 отдельным шагом до Step 11** — встраиваем как 11.0 (см. развилка 6, обоснование выбора (a)).
- **Не трогать Adaptation reasons enum** — split apply_adaptation сохраняет API `AdaptationReason`.
- **Не делать pipeline async / parallel** — sync sequential по дизайну (master plan).
