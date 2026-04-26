# Шаг 7 — `PlanAnnotation` + `PlanStage` pipeline: декомпозиция на сабшаги

Декомпозиция в стиле фаз 2a / step 3 / step 4 / step 5 / step 6: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §7.

## Preamble

### Текущее состояние pipeline

`pick_action` (`src/combat/ai/utility/mod.rs:174–364`) — 200 строк. Уже частично декомпозирован: `PlanRanking` (`utility/ranking.rs:34–227`) держит `(intent, reason, scored, raw_factors, adaptation, sanity_breakdown, gate_stats)` и имеет `apply_*` методы как полу-стадии:

```rust
let mut ranking = PlanRanking::initial(&mut plans, ...);
ranking.apply_viability(&mut plans, actor_pos, &scoring_ctx);
ranking.apply_sanity(&mut plans, &scoring_ctx);
let base_scored = ranking.scored.clone();          // ← snapshot для лога
ranking.apply_adaptation(&mut plans, &scoring_ctx);
if matches!(ranking.intent, ...) {
    ranking.apply_protect_self(...);                // ← conditional
}
if matches!(ranking.intent, ...) {
    ranking.apply_killable_gate(...);               // ← conditional
}
// Inline repair_affinity вне ranking:
if let Some(stored_goal) = &memory.last_goal {
    for plan in plans.iter_mut() {
        plan.annotation.repair_affinity = compute_repair_affinity(...);
    }
}
let (best_idx, mech) = ranking.pick(world, rng);
```

`PlanAnnotation` (`outcome.rs:50`) на текущий момент — три поля: `outcomes`, `terminal`, `repair_affinity`. Каждое populated своей стадией, но без формального контракта «stage X пишет в section Y».

### Проблемы текущей схемы

**Stages — методы, не объекты.** Trait не оформлен — нет общего интерфейса `apply()`. Поэтому:
- Нельзя переставить порядок stages декларативно.
- Нельзя добавить stage без правки `pick_action` body.
- Conditional stages (`apply_protect_self`, `apply_killable_gate`) встроены в pick_action через `if matches!`, а не через stage-internal predicate.
- Adaptation — «особенная»: в `pick_action` после `pick()` есть spec-cased wrap intent_reason в `IntentReason::Adapted` (строки 301–307 utility/mod.rs). Это утечка abstraction'а.

**ScoredPool — рассыпан.** `(plans: Vec<TurnPlan>, scored: Vec<f32>, raw_factors: Vec<PlanFactors>)` живут параллельно. Каждая stage синхронизирует индексы. Нет invariant'а «scored.len() == plans.len()» в типе.

**Annotation — half-baked.** Поля `outcomes`/`terminal`/`repair_affinity` уже в `PlanAnnotation`, но другие production-релевантные данные (sanity hits, adaptation reasons, contract masks, pick info) живут на стороне `PlanRanking` и в JSONL log сериализуются параллельно, не как часть annotation. Структура расходится: одни данные per-plan in annotation, другие per-pool в logs.

**Repair affinity — не stage.** Inline-cycle в `pick_action` body (строки 274–292) — не вписан в pipeline. Не может быть toggle'нут / переставлен / тестирован отдельно.

**Carry-over из 6.7 — telemetry-разрыв.** Early-return path в `enemy_turn.rs:103–106` (нет AP/MP) обходит divergence-log. FIXME(step 7) внутри указывает: telemetry должна лежать в start-of-turn stage, не в run_ai_turn body.

**Carry-over из 6.9 — fixture-разрыв.** `ai_scenarios` runner не snapshot'ит runtime `AiMemory.last_goal`. Replay всегда стартует с `last_goal = None`, поэтому cross-round preservation fixture'ы не тестируются на low-margin случаях. 4 из 5 `continuation_*` fixture'ов из §6.6/§6.9 заблокированы на этом ограничении.

### Что закрывает step 7

**1. Trait `PlanStage` + типизированный `ScoredPool`.** Stages становятся объектами, pool — типизированной парой `(plans, annotations)` с invariant'ами на len.

**2. Все производные данные в `PlanAnnotation`.** Sanity hits, adaptation reasons, contract masks, pick info — каждое попадает в свою section. JSONL serialize'ит annotation как единое целое; миграции через `#[serde(default)]`.

**3. Adaptation становится регулярной stage.** `IntentReason::Adapted` wrap в pick_action убирается — finalizer читает `annotation.adaptation` и собирает reason из section.

**4. Pipeline assembly.** `pick_action` body становится `pipeline.run(&mut pool, &ctx)` + setup + finalize. ~30 строк вместо 200.

**5. Закрытие 6.7 carry-over.** Start-of-turn `GoalRepairStage` в pipeline пишет divergence-log + decay TTL + clear stale. Early-return path в `enemy_turn.rs` упрощается до bare `EndTurn` (без custom telemetry).

**6. Закрытие 6.9 carry-over.** Расширение `ai_scenarios` fixture format'а: `[ai_memory]` секция позволяет инъекцировать `last_goal` (и при необходимости другие memory fields) в runner до pipeline.run. 4 pending `continuation_*` fixtures становятся reachable.

### Что НЕ в scope шага 7

- **StepFactor / PlanFactor decomposition (step 8)** — даже после 7 факторы остаются монолитом в `finalize_scores`. Decomposition в step 8 — отдельный шаг.
- **Critics decomposition (step 10)** — sanity hits остаются массивом строк (`Vec<SanityHit>`) в annotation. Per-critic decomposition с structured reasons — step 10.
- **TTL decay redesign** — формула остаётся age-based как в 6.7. Если перепроектируем (мутабельный counter / per-mismatch decay), то отдельно.
- **Plan-level mid-tick reflow (step 12)** — `ScoredPool` не пере-вычисляется в середине pipeline на основе deltas. Если нужно, добавляется через NewStage в step 12.
- **Bands+agenda+scorecard (step 11)** — band assignment появится как отдельная stage позже.

### Зафиксированные решения по развилкам

**1. Все stage-данные в `PlanAnnotation` per-stage section** (1a).

`PlanAnnotation` обогащается секциями: `sanity: Vec<SanityHit>`, `adaptation: Option<AdaptationData>`, `contract: Option<ContractMaskHit>`, `pick: Option<PickInfo>`. Каждая stage пишет в строго одну section.

**Альтернатива (1b):** оставить side-channels per-stage в `PlanRanking`-style. Pros: меньше invariant'ов; cons: не закрывает pipeline-philosophy step 7. Отвергнута.

**2. Schema v26→v27 атомарно с pipeline в 7.5** (2a).

Не разносить на step 7 (pipeline) + step 8 (log refactor). Иначе log имеет двойственное состояние (часть полей top-level, часть в annotation), что путает offline-tooling.

**Альтернатива (2b):** schema bump в step 8. Отвергнута: один schema bump меньше двух.

**3. `RepairAffinityStage` отдельно от `GoalRepairStage`** (3a).

Two stages в pipeline:
- `GoalRepairStage` — pre-stage (start-of-turn): lifecycle, TTL decay, divergence-log, clear stale.
- `RepairAffinityStage` — mid-pipeline: bonus computation на каждый plan.

**Альтернатива (3b):** одна stage делает оба. Отвергнута: они работают в разные моменты pipeline (lifecycle до scoring, bonus после). Slip разделён по фазам.

**4. `ai_scenarios` state injection в scope step 7 как 7.6** (4a).

Runner extension натурально опирается на новые stage interfaces — state injection через `StageCtx` или `MemoryInit` структуру в overlay format.

**Альтернатива (4b):** отдельный шаг 7.7 для runner extension. Отвергнута: 7.6 как scope позволяет валидировать pipeline на 4 ранее заблокированных continuation fixture'ах в одном PR.

### Природа gate'ов в step 7

В отличие от step 6 (gate'ы на behavioral metrics), step 7 — refactor с инвариантом «behavior unchanged». Gate'ы:

- **7.0–7.5** — golden 0/N (no behavior change), `cargo test/clippy/build/ai_scenarios` зелёные.
- **7.4** — pipeline assembly — самый рискованный сабшаг; per-entry golden review (≤10/N допустимо для tie-breaking flakiness'а в edge cases, не больше).
- **7.5** — schema bump v26→v27, replay roundtrip 0/N + schema migration test для v26 logs.
- **7.6** — 4 pending continuation_* fixtures (`target_dies_replan`, `cosmetic_rage_tick_no_replan`, `setup_aoe_two_ticks`, `ttl_expires`) green.

## Сабшаги

### 7.0. Scaffolding: `PlanStage` trait + `ScoredPool` + `StageCtx`

**Scope.**

Новый модуль `src/combat/ai/pipeline/mod.rs`:

```rust
/// Read-only context threaded through every stage. Replaces ad-hoc parameters.
pub struct StageCtx<'w, 's> {
    pub scoring: &'s ScoringCtx<'w, 's>,
    pub intent: TacticalIntent,
    pub intent_reason: IntentReason,
    pub actor_pos: Hex,
    pub rng: &'s mut DiceRng,
}

/// Typed pool of plans + their annotations + scored values.
/// Invariant: plans.len() == annotations.len() == scored.len() == raw_factors.len().
pub struct ScoredPool {
    pub plans: Vec<TurnPlan>,
    pub annotations: Vec<PlanAnnotation>,
    pub scored: Vec<f32>,
    pub raw_factors: Vec<PlanFactors>,
}

impl ScoredPool {
    pub fn new(plans: Vec<TurnPlan>) -> Self { /* zero-fill rest */ }
    pub fn len(&self) -> usize { self.plans.len() }
    pub fn iter_with_annotation(&self) -> impl Iterator<Item = (&TurnPlan, &PlanAnnotation, f32)>;
}

/// Trait for pipeline stages. Each stage mutates the pool in-place.
pub trait PlanStage {
    fn name(&self) -> &'static str;
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx);
}
```

Pipeline композер:

```rust
pub struct Pipeline {
    stages: Vec<Box<dyn PlanStage>>,
}

impl Pipeline {
    pub fn new() -> Self { Self { stages: vec![] } }
    pub fn add(mut self, stage: Box<dyn PlanStage>) -> Self {
        self.stages.push(stage); self
    }
    pub fn run(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        for stage in &self.stages {
            stage.apply(pool, ctx);
        }
    }
}
```

**Юнит-тесты в `pipeline/mod.rs::tests`:**
- `scored_pool_invariant_holds_after_construct` — len consistency.
- `pipeline_runs_stages_in_order` — mock stages, проверяем seq.
- `pipeline_empty_is_noop` — pool unchanged.

**Gate.** `cargo test/clippy/build/ai_scenarios` зелёные. Golden **0/N** (никто из new types не используется в pick_action).

**Эстимейт:** 0.5 дня.

---

### 7.1. Scoring stages: `ViabilityStage` + `SanityStage`

**Scope.**

Конвертировать `PlanRanking::apply_viability` (`ranking.rs:84–157`) в:

```rust
pub struct ViabilityStage;
impl PlanStage for ViabilityStage {
    fn name(&self) -> &'static str { "viability" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        // Same logic as PlanRanking::apply_viability, but writes to:
        // - pool.scored (mutate)
        // - ctx.intent / ctx.intent_reason (mutate via &mut)
        // - pool.annotations[i].viability (new section, see below)
    }
}
```

Аналогично `apply_sanity` → `SanityStage`.

**`PlanAnnotation` extension:**

```rust
pub struct PlanAnnotation {
    // existing: outcomes, terminal, repair_affinity
    /// 7.1: viability gate result (was PlanRanking.gate_stats per-pool, now per-plan).
    #[serde(default)]
    pub viability: ViabilityResult,
    /// 7.1: sanity hits applied to this plan (was PlanRanking.sanity_breakdown[i]).
    #[serde(default)]
    pub sanity: Vec<SanityHit>,
}

pub struct ViabilityResult { pub passed: bool, pub adjusted_score: f32 }
pub struct SanityHit { pub kind: &'static str, pub delta: f32, pub reason: &'static str }
```

`PlanRanking.gate_stats` и `PlanRanking.sanity_breakdown` — **остаются** в этом сабшаге (для backward-compat), но дублируются из annotation. В 7.5 мы их удалим.

**Plumbing в `pick_action`.**

```rust
// Старая ветвь:
let mut ranking = PlanRanking::initial(&mut plans, ...);
ranking.apply_viability(&mut plans, actor_pos, &scoring_ctx);
ranking.apply_sanity(&mut plans, &scoring_ctx);

// Новая ветвь:
let mut pool = ScoredPool::new(plans);
let mut ctx = StageCtx { scoring: &scoring_ctx, intent: choice.intent, intent_reason: choice.reason, actor_pos, rng };
let pipeline = Pipeline::new()
    .add(Box::new(ViabilityStage))
    .add(Box::new(SanityStage));
pipeline.run(&mut pool, &mut ctx);
// Sync pool.scored обратно в ranking для остального legacy-pipeline:
let mut ranking = PlanRanking::from_pool(&pool, ctx.intent, ctx.intent_reason);
// ... rest of pick_action body unchanged ...
```

**Юнит-тесты:**
- `viability_stage_writes_annotation_section` — annotations[i].viability populated.
- `sanity_stage_appends_hits` — annotations[i].sanity не пустой когда были hits.
- `legacy_ranking_matches_pool` — invariant: после стадий, ranking.scored == pool.scored, ranking.sanity_breakdown == derived_from_annotations.

**Gate.** `cargo test/clippy/build/ai_scenarios` зелёные. Golden **0/N** (поведение идентичное).

**Эстимейт:** 1.0 день.

---

### 7.2. Contract stages: `AdaptationStage` + `ProtectSelfMaskStage` + `KillableGateStage`

**Scope.**

Конвертировать три «contract» метода `PlanRanking` в stages:

- `PlanRanking::apply_adaptation` (`ranking.rs:166`) → `AdaptationStage`.
- `PlanRanking::apply_protect_self` (`ranking.rs:190`) → `ProtectSelfMaskStage` с **internal predicate** (skip when `intent != ProtectSelf`).
- `PlanRanking::apply_killable_gate` (`ranking.rs:209`) → `KillableGateStage` с **internal predicate** (skip when `!matches!(intent, FocusTarget {..})`).

Каждая stage сама проверяет, нужно ли ей запускаться — `if matches!` уходит из pick_action body.

**`PlanAnnotation` extension:**

```rust
pub struct PlanAnnotation {
    // existing: outcomes, terminal, repair_affinity, viability, sanity
    /// 7.2: adaptation reason for this plan (was PlanRanking.adaptation.reasons[i]).
    #[serde(default)]
    pub adaptation: Option<AdaptationData>,
    /// 7.2: contract mask hit (ProtectSelf / KillableGate masking applied to this plan).
    #[serde(default)]
    pub contract: Option<ContractMaskHit>,
}

pub struct AdaptationData { pub reason: AdaptationReason, pub original_score: f32 }
pub struct ContractMaskHit { pub mask: &'static str /* "protect_self" | "killable_gate" */, pub original_score: f32 }
```

**Adaptation — больше не «особенная».**

Текущий wrap в `pick_action:301–307`:
```rust
if let Some(adapt_reason) = ranking.adaptation.reasons.get(best_idx).and_then(|r| r.clone()) {
    let prior = std::mem::replace(&mut ranking.intent_reason, IntentReason::NoRuleDefault);
    ranking.intent_reason = IntentReason::Adapted { prior: Box::new(prior), reason: adapt_reason };
}
```

Перемещается в финализатор pick_action как чтение `pool.annotations[best_idx].adaptation`:

```rust
let final_intent_reason = if let Some(adapt) = &pool.annotations[best_idx].adaptation {
    IntentReason::Adapted { prior: Box::new(ctx.intent_reason.clone()), reason: adapt.reason.clone() }
} else {
    ctx.intent_reason.clone()
};
```

**Юнит-тесты:**
- `adaptation_stage_skips_when_no_adaptation_triggers` — annotations[i].adaptation == None.
- `protect_self_mask_skips_when_intent_not_protect_self` — predicate work.
- `killable_gate_skips_when_intent_not_focus_target` — predicate work.
- `adaptation_data_round_trips_through_annotation` — wrap в IntentReason::Adapted получает корректный reason.

**Gate.** Golden **0/N** (поведение идентичное).

**Эстимейт:** 1.0 день.

---

### 7.3. `RepairAffinityStage` + start-of-turn `GoalRepairStage` (carry-over из 6.7)

**Scope.**

**`RepairAffinityStage`** — bonus computation вынесен из inline-цикла `pick_action:274–292` в stage:

```rust
pub struct RepairAffinityStage;
impl PlanStage for RepairAffinityStage {
    fn name(&self) -> &'static str { "repair_affinity" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(stored_goal) = ctx.scoring.last_goal else { return };
        let severity = compute_severity(stored_goal, ctx.scoring);
        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            ann.repair_affinity = compute_repair_affinity(
                ctx.intent, &plan.steps, plan.final_pos,
                stored_goal, severity, ctx.scoring.snap.round,
            );
        }
    }
}
```

Bonus apply (текущий код в `finalize_scores`) **остаётся** там же — RepairAffinityStage только populates annotation, как было до.

**`GoalRepairStage`** — НОВАЯ pre-stage (запускается в самом начале pipeline, **до** scoring stages):

```rust
pub struct GoalRepairStage;
impl PlanStage for GoalRepairStage {
    fn name(&self) -> &'static str { "goal_repair" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let Some(stored_goal) = ctx.scoring.last_goal else { return };
        // Decay TTL, classify continuation, write divergence-log event.
        // Clear stale goals (TTL expired / Invalidating severity).
        // The clear is mediated through StageCtx::memory mut access.
    }
}
```

**Lifecycle перенос из `enemy_turn.rs`.**

Текущий `enemy_turn.rs:180–235` (divergence-log + outcome classification) **уезжает** в `GoalRepairStage`. После step 7:

```rust
// enemy_turn.rs (сильно упрощается):
fn run_ai_turn(...) {
    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // FIXME(step 7) был тут — теперь GoalRepairStage отрабатывает в pipeline
        // даже на этом пути. Просто endturn:
        msgs.end_turn.write(EndTurn { actor });
        return;
    }
    // ... snapshot build, name map ...
    let (decision, debug_snapshot, fresh_chosen) = pick_action(...);
    // ... store/clear last_goal ...
}
```

Wait — early-return path **НЕ запускает pick_action**, поэтому GoalRepairStage не отработает. Нужно либо:
- (a) Вызывать `pipeline.run_pre_stages_only()` на early-return path.
- (b) Вынести GoalRepair из pipeline в `enemy_turn.rs` как explicit pre-call.

**Решение 7.3 — (a):** `Pipeline::run_pre_stages_only()` метод, который запускает только stages с `is_pre_stage(&self) -> bool { true }`. GoalRepairStage возвращает true; остальные false.

```rust
pub trait PlanStage {
    fn name(&self) -> &'static str;
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx);
    /// Pre-stages run even when the actor cannot act this tick (no AP/MP).
    /// Default false. Override true for telemetry/lifecycle stages that
    /// need to fire regardless of whether decision-making proceeds.
    fn is_pre_stage(&self) -> bool { false }
}
```

`enemy_turn.rs` early-return path:

```rust
if c.ap.action_points <= 0 && !c.ap.can_move() {
    let mut empty_pool = ScoredPool::empty();
    let mut ctx = ctx_for_pre_stages(...);
    pipeline.run_pre_stages_only(&mut empty_pool, &mut ctx);
    msgs.end_turn.write(EndTurn { actor });
    return;
}
```

Это закрывает FIXME(step 7) полностью — divergence-log пишется на early-return path тоже.

**Plumbing.**

В `pick_action` body:

```rust
let pipeline = Pipeline::new()
    .add(Box::new(GoalRepairStage))     // pre-stage: lifecycle/telemetry
    .add(Box::new(ViabilityStage))
    .add(Box::new(SanityStage))
    .add(Box::new(AdaptationStage))
    .add(Box::new(ProtectSelfMaskStage))
    .add(Box::new(KillableGateStage))
    .add(Box::new(RepairAffinityStage));
```

**Юнит-тесты:**
- `goal_repair_stage_decays_ttl` — last_goal с age=ttl after stage → cleared.
- `goal_repair_stage_writes_divergence_log` — log entry создан.
- `goal_repair_stage_runs_on_early_return_path` — pre-stage gate работает.
- `repair_affinity_stage_populates_annotation` — annotations[i].repair_affinity correct.

**Gate.**
- Golden **0/N** (lifecycle уже корректен после 6.7; только перемещение в stage).
- Divergence-log entries матчатся 1:1 с pre-step-7 entries (формат идентичен).
- На early-return path появляются НОВЫЕ divergence-log entries — это новый сигнал, а не регрессия. Mining будет показывать дополнительные events типа `goal_abandoned_ttl_expired` на actors, которые раньше silently skipnли это logging.

**Эстимейт:** 1.0 день.

---

### 7.4. Pipeline assembly: `pick_action` body → `pipeline.run`

**Scope.**

Это самый рискованный сабшаг — финальная сборка. После 7.0–7.3 у нас есть все stages, но `pick_action` body всё ещё содержит legacy `PlanRanking` и inline glue. В 7.4:

1. `PlanRanking` **удаляется**.
2. `pick_action` body становится:

```rust
pub fn pick_action(
    actor: Entity, actor_pos: Hex, world: &AiWorld, snap: &BattleSnapshot,
    maps: &InfluenceMaps, rng: &mut DiceRng, memory: &mut AiMemory,
    reservations: &mut Reservations, logger: &mut AiLogger, debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>, Option<ChosenInfo>) {
    // 1. Setup (~10 lines): per-actor tuning, need_signals, intent select, plans.
    let setup = setup_for_pick_action(actor, actor_pos, world, snap, maps, memory)?;

    // 2. Run pipeline (~5 lines).
    let mut pool = ScoredPool::new(setup.plans);
    let mut ctx = StageCtx::new(&setup.scoring, setup.intent, setup.reason, actor_pos, rng);
    PIPELINE.run(&mut pool, &mut ctx);

    // 3. Pick + finalize (~15 lines).
    let (best_idx, mech) = pick_best_plan(&pool, world, ctx.rng);
    let (decision, consumed) = commit_plan(&pool.plans[best_idx], actor_pos);
    let chosen = build_chosen_info(&pool, best_idx, &ctx);
    let debug_snapshot = if debug { Some(build_debug_snapshot_from_pool(&pool, ...)) } else { None };
    record_committed_reservations(&pool.plans[best_idx], consumed, ...);
    if logger.is_enabled() { write_decision_log_from_pool(&pool, ...); }

    (decision, debug_snapshot, chosen)
}
```

`PIPELINE: Lazy<Pipeline>` — глобальный singleton, инициализируется один раз.

3. Все callers `PlanRanking` обновляются (debug, log, ranking tests). `apply_*` методы PlanRanking удаляются — их функционал теперь в stages.

**Удаляются:**
- `PlanRanking` struct + impl.
- `ranking.rs::apply_viability/apply_sanity/apply_adaptation/apply_protect_self/apply_killable_gate`.
- `ranking.rs::pick` → переезжает в utility/mod.rs::pick_best_plan (free function).

**Сохраняются:**
- `PlanRanking::initial` — заменяется на `ScoredPool::new`.
- Тесты ranking.rs — переписываются под stages (если ещё не переписаны в 7.1–7.3).

**Юнит-тесты:**
- `pick_action_pipeline_runs_all_stages` — integration test что все stages запустились.
- `pick_action_chosen_info_matches_pool` — chosen.score == pool.scored[best_idx].

**Gate.**
- `cargo test/clippy/build/ai_scenarios` зелёные.
- **Per-entry golden review (риск flakiness'а)** — допустимо ≤10/N diverged для tie-breaking edge cases. >10 — индикатор упущенной семантики, нужно расследовать перед merge'ем.
- Replay roundtrip 0/N.

**Эстимейт:** 1.0 день (правка ~200 строк pick_action + 100 строк ranking + миграция тестов).

---

### 7.5. `PlanAnnotation` serialization + schema v26→v27 + log overhaul

**Scope.**

**`PlanAnnotation` сериализация.**

`PlanAnnotation` уже `Serialize/Deserialize` (outcome.rs:34, 50). Но в JSONL log сейчас annotation поля разнесены: `outcomes`, `terminal`, `repair_affinity` сериализуются как поля внутри `plan` записи; `sanity`, `adaptation`, `contract`, `viability`, `pick` сейчас на стороне `PlanRanking` и сериализуются параллельно.

**Цель 7.5:** unified annotation per plan в logs.

```jsonl
{
  "plans": [
    {
      "rank": 1, "chosen": true, "steps": [...], "score": 2.7,
      "annotation": {
        "outcomes": [...],
        "terminal": {...},
        "repair_affinity": {...},
        "viability": {...},
        "sanity": [...],
        "adaptation": {...},
        "contract": {...},
        "pick": {...}
      }
    }
  ]
}
```

**Удаляются из top-level log entry:**
- `sanity_breakdown` (per-pool array).
- `adaptation` (per-pool array).
- `gate_stats` (per-pool counts).

**Schema bump v26 → v27.**

Backward compat:
- v26 logs читаются: `annotation` поле отсутствует → `serde(default)` populates с zero. `sanity_breakdown` в v26 → парсится в `PlanRanking`-style legacy поле, **не** мигрируется в annotation (replay стартует с zero annotation).
- v27 → строгий формат.

**Replay support.**
- `replay_ai_log.rs` читает annotation per plan.
- Pre-v27 logs: annotation = default; replay не сравнивает annotation поля для pre-v27.
- Post-v27 logs: full annotation comparison.

**Mining-tool overhaul.**
- `mine_ai_logs.rs` extension: новые секции под annotation analysis (например, `C7. Per-plan adaptation rates`, `C8. Sanity hit distribution`).
- Существующие секции (A1–C6) — без изменений.

**Plumbing.**

- `write_decision_log` (`utility/mod.rs:370+`) — переписывается чтобы читать annotation из pool, а не из PlanRanking-side fields.
- `LogEntry` struct — поля sanity_breakdown / adaptation / gate_stats → `#[serde(default, skip_serializing)]` для backward read, не пишется в новых logs.

**Юнит-тесты:**
- `v26_log_reads_with_default_annotation` — pre-v27 log → annotation = zero.
- `v27_log_round_trip_preserves_annotation` — write → read → equal.
- `mine_v26_log_does_not_panic_on_missing_annotation` — backward compat в miner.

**Gate.**
- `cargo test/clippy/build/ai_scenarios` зелёные.
- Replay v26 corpus (golden_post_step6.jsonl): roundtrip 0/N (pre-v27 read OK).
- Schema migration test: v26 entries deserialize без panic.
- New golden capture на v27 logs.
- `mine_ai_logs --dir <v27_corpus>` отрабатывает без panic.

**Эстимейт:** 1.0 день.

---

### 7.6. `ai_scenarios` state injection (carry-over из 6.9)

**Scope.**

**Fixture format extension.**

`tests/ai_scenarios/snapshots/<group>/<case>.expected.toml` получает опциональную `[ai_memory]` секцию:

```toml
[scope]
plan_id = 6

[ai_memory]
# Optional: inject AiMemory state before pick_action runs.
# All fields optional — unspecified = default (None / 0).
last_goal = { kind = "Pressure", target = 17179868830, region_anchor = [1, 2],
              region_radius = 2, ttl = 2, confidence = 0.9, created_round = 1,
              expected_actor_pos = [3, 3], actor_hp_at_store = 8,
              actor_rage_at_store = 0, actor_status_hash = 0,
              target_hp_at_store = 14, target_pos_at_store = [1, 2] }
last_intent = "FocusTarget"
turns_committed = 1
hp_ratio_at_last_turn = 1.0

[[expectations]]
decision_kind = ["MoveAndCast"]
cast_target = [17179868830]
intent_kind = ["FocusTarget"]
```

**Runner extension** в `tests/ai_scenarios.rs`:

```rust
fn run_case(case: &TestCase) -> Result<(), Error> {
    let snapshot = load_snapshot(&case.log_path, case.plan_id)?;
    let mut memory = case.overlay.ai_memory.clone().unwrap_or_default();  // ← NEW
    let decision = pick_action_with_memory(&snapshot, &mut memory)?;
    case.overlay.assert_matches(&decision)
}
```

**4 pending continuation_* fixtures (создание).**

Pre-condition: corpus с подходящими playtest'ами уже есть (6 файлов post-6.8B). Используем их или просим у пользователя дополнительные.

| Fixture | Source | Что overlay'ится |
|---|---|---|
| `continuation_target_dies_replan` | playtest где target умирает между ходами; overlay устанавливает stored_goal с уже-мертвой target_id | actor switch на следующую цель (не EndTurn) |
| `continuation_cosmetic_rage_tick_no_replan` | melee actor с rage; overlay устанавливает actor_rage_at_store ≠ current rage (Cosmetic mismatch) | decision_kind = Cast/MoveAndCast, intent_kind = FocusTarget на ту же цель |
| `continuation_setup_aoe_two_ticks` | Control class actor (Aldric); overlay устанавливает stored_goal с GoalKind::SetupAOE | decision_kind = Cast, cast_ability = planned aoe |
| `continuation_ttl_expires` | любой playtest; overlay устанавливает created_round так чтобы age >= ttl | decision_kind = Move/EndTurn (НЕ continuing committed goal); intent_kind ≠ stored.kind.intent |

**Юнит-тесты в `ai_scenarios.rs`:**
- `ai_memory_section_optional_in_overlay` — overlay без `[ai_memory]` → memory = default.
- `ai_memory_injection_changes_decision` — overlay с last_goal реально меняет picked plan.
- `4_continuation_fixtures_pass` — все 4 новых fixtures зелёные.

**Gate.**
- `cargo test --test ai_scenarios` все 14 (10 + 4 новых) зелёные.
- 4 pending continuation_* fixtures из §6.6/§6.9 — closed.
- На созданных fixtures mining (если запустить — fixtures это синтетические snapshots, mining через них не запустится напрямую).

**Эстимейт:** 1.0 день (extension + 4 fixtures).

---

## Итого

| # | Шаг | Эстимейт | Gate | Статус |
|---|---|---|---|---|
| 7.0 | Scaffolding (`PlanStage`, `ScoredPool`, `StageCtx`) | 0.5 | golden 0/N, no behavior | done (`3ed749a`) |
| 7.1 | Scoring stages (Viability + Sanity) | 1.0 | golden 0/N | done (`2a3dbe0`) |
| 7.2 | Contract stages (Adaptation + ProtectSelfMask + KillableGate) | 1.0 | golden 0/116 | done |
| 7.3 | RepairAffinity + GoalRepair stages (closes 6.7 carry-over) | 1.0 | divergence-log переехал, формат идентичен | pending |
| 7.4 | Pipeline assembly (`pick_action` body refactor) | 1.0 | per-entry golden ≤10/N | pending |
| 7.5 | Annotation serialization + schema v26→v27 + log overhaul | 1.0 | replay roundtrip 0/N, v26 read OK | pending |
| 7.6 | ai_scenarios state injection (closes 6.9 carry-over) | 1.0 | 4 pending continuation_* fixtures green | pending |

**Суммарно ~6.5 дней.**

## Зафиксированные решения

1. **Все stage-данные в `PlanAnnotation` per-stage section** — `viability`, `sanity`, `adaptation`, `contract`, `pick`. Каждая stage пишет в строго одну.
2. **Schema v26→v27 атомарно с pipeline в 7.5**, не разносить.
3. **`RepairAffinityStage` отдельно от `GoalRepairStage`** — разные фазы pipeline (pre-stage vs mid-pipeline).
4. **`ai_scenarios` state injection в scope step 7 как 7.6** — закрывает 6.9 carry-over в одном PR с pipeline.
5. **`PlanStage::is_pre_stage()` predicate** — `GoalRepairStage` возвращает true; запускается на early-return path в `enemy_turn.rs` через `pipeline.run_pre_stages_only()`. Закрывает FIXME(step 7).
6. **`PlanRanking` удаляется в 7.4** — после миграции всех `apply_*` методов в stages. До этого live дублируется (legacy + pool).
7. **Adaptation перестаёт быть «особенной»** — `IntentReason::Adapted` wrap читает `pool.annotations[best_idx].adaptation` в финализаторе.

## Критические файлы

- `src/combat/ai/pipeline/mod.rs` — новый модуль (`PlanStage`, `ScoredPool`, `StageCtx`, `Pipeline`).
- `src/combat/ai/pipeline/stages/` — каталог под stages: `viability.rs`, `sanity.rs`, `adaptation.rs`, `protect_self.rs`, `killable_gate.rs`, `repair_affinity.rs`, `goal_repair.rs`.
- `src/combat/ai/utility/mod.rs` — `pick_action` body упрощается в 7.4.
- `src/combat/ai/utility/ranking.rs` — удаляется в 7.4.
- `src/combat/ai/outcome.rs` — `PlanAnnotation` extension (`viability`, `sanity`, `adaptation`, `contract`, `pick`).
- `src/combat/ai/enemy_turn.rs` — early-return path упрощается в 7.3 (FIXME(step 7) убран).
- `src/combat/ai/log.rs` — `LogEntry` annotation refactor в 7.5; SCHEMA_VERSION v26→v27.
- `src/bin/replay_ai_log.rs` — read v26 + v27 в 7.5.
- `src/bin/mine_ai_logs.rs` — annotation analysis sections в 7.5.
- `tests/ai_scenarios.rs` — `[ai_memory]` injection в 7.6.
- `tests/ai_scenarios/snapshots/continuation_*` — 4 новых fixtures в 7.6.

## Ожидаемые сдвиги

После 7.6 — **никаких behavioral сдвигов** относительно post-step-6 baseline. Step 7 — pure refactor. Mining-метрики на 6-mix v27 corpus должны воспроизвести post-6.8B картину:
- `goal_preserved (combined)` ≈ 55%.
- `method_delivered` ≈ 19%.
- `voluntary` ≈ 14%.
- `reactive` ≈ 31%.

**Новый сигнал:** на early-return path (нет AP/MP) теперь будут writes divergence-log events — это раньше silently не писалось. Mining покажет:
- `+N` events для actors с stored_goal которые pass'ят целые ходы.
- Возможно появление `ttl_expired` events через cross-round persistence на early-return path.

`invalidating` всё ещё может быть 0% — зависит от scenario coverage (см. §6.9).

## Что откладывается

- **StepFactor / PlanFactor / TerminalFactor decomposition (step 8)** — даже после step 7 факторы остаются монолитом в `finalize_scores`. Decomposition — отдельный шаг.
- **Critics decomposition (step 10)** — `Vec<SanityHit>` остаётся плоским в annotation; per-critic structured reasons — step 10.
- **PlanModifier (step 8 follow-up)** — post-composition бонусы как explicit type, отдельно от sanity hits.
- **Bands+agenda+scorecard (step 11)** — band assignment как stage появится позже.

## Чего не делать в шаге 7

- **Не менять scoring formula** — finalize_scores, factor weights, repair affinity formula — всё остаётся как в step 6. Step 7 только формализует where они вычисляются.
- **Не разделять `PlanFactors` на per-step / per-plan** — это step 8 (StepFactor / PlanFactor enum).
- **Не делать stages async / parallel** — sync sequential по дизайну. Parallelism — backlog после profiling.
- **Не вводить mid-pipeline reflow** — pool вычисляется один раз, не пере-scored на основе deltas. Reflow — step 12.
- **Не пытаться оптимизировать allocations** — `Vec<PlanAnnotation>` clones приемлемы; profiling-driven optimization после.
- **Не делать stages configurable через TOML** — pipeline composition в коде. Если будет нужна dynamic composition — отдельный шаг.
