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

**1. Trait `PlanStage` + типизированный `ScoredPool`.** Stages становятся объектами; `ScoredPool { plans, annotations }` — типизированная пара. **Все per-plan данные** (`score`, `raw_factors`, viability/sanity/adaptation/contract/repair_affinity/outcomes/terminal/chosen/pick) — внутри `PlanAnnotation`. Один parallel array вместо четырёх.

**2. Adaptation становится регулярной stage.** `IntentReason::Adapted` wrap в pick_action убирается — finalizer читает `annotation.adaptation` и собирает reason из section. Реализовано в 7.2.

**3. `pick_action` — pure function** возвращает `PickResult { decision, chosen, pool, debug_snapshot }`. Не принимает `&mut logger` или `&mut reservations`. Все side effects — в orchestrator (7.4).

**4. `enemy_turn.rs` — единственный orchestrator.** Lifecycle (`goal_lifecycle::pre_tick`/`post_tick`), logging (`write_actor_tick_log`), reservations — всё из одного места (7.4).

**5. Pipeline assembly.** `Pipeline` struct (7.0) удаляется в 7.4 — заменяется free function `run_pool_pipeline`. Stages вызываются по именам, compile-time order, zero indirection. `pick_action` body становится setup + run_pool_pipeline + return — ~25 строк вместо 200.

**6. `PickBestStage` — pick тоже stage.** `chosen: bool` + `pick: Option<PickInfo>` пишутся в annotation. Уход от free function `pick_best_plan` (7.4).

**7. Закрытие 6.7 carry-over (полностью).** `goal_lifecycle::pre_tick` (7.3) централизует TTL decay + clear stale. `tick_skipped` log записывается на early-return path через `write_actor_tick_log` с `decision: Skip` (7.5). FIXME(step 7) исчезает.

**8. Schema v27 — clean break.** Единый `actor_tick` event объединяет `actor_turn` + `plan_divergence` + skip case. Raw data only (outcome derive'ится mining tool'ом). v26 logs не читаются (явно одобрено пользователем). Tools (`mine_ai_logs`, `replay_ai_log`) переписываются под v27 only.

**9. Закрытие 6.9 carry-over.** Расширение `ai_scenarios` fixture format'а: `[ai_memory]` секция позволяет инъекцировать `last_goal` (и др. memory fields) в runner до pick_action. 4 pending `continuation_*` fixtures становятся reachable (в v27 формате, 7.6).

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

- **7.0–7.4** — golden 0/N (no behavior change), `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- **7.4** — pipeline assembly — самый рискованный сабшаг; per-entry golden review (≤10/N допустимо для tie-breaking flakiness'а в edge cases, не больше).
- **7.5** — schema clean break v27, v27 round-trip 0/N, mining baseline (post-6.8B) воспроизводится на свежем v27 corpus.
- **7.6** — 4 pending continuation_* fixtures (`target_dies_replan`, `cosmetic_rage_tick_no_replan`, `setup_aoe_two_ticks`, `ttl_expires`) green в v27 формате.

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

### 7.3. `RepairAffinityStage` + `goal_lifecycle` module

**Scope (refined после Q1/Q2/Q3 review).**

Изначальный план §7.3 описывал GoalRepairStage как pre-stage в pipeline с side-channel для divergence-log на early-return path. После архитектурного критика (см. «Refined architecture decisions» ниже) убрали `is_pre_stage()` / `run_pre_stages_only()` усложнения. Lifecycle = explicit module, не stage.

**1. `RepairAffinityStage`** — bonus computation вынесен из inline-цикла `pick_action` в stage:

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

**2. `goal_lifecycle` module** (`src/combat/ai/repair/lifecycle.rs` или подобное) — pre/post-tick helpers:

```rust
/// Pre-tick: TTL decay + clear stale goals (TTL expired / Invalidating severity).
/// Called by orchestrator BEFORE pick_action. Idempotent on stale memory.
pub fn pre_tick(memory: &mut AiMemory, snap: &BattleSnapshot, actor: &UnitSnapshot) {
    let Some(g) = &memory.last_goal else { return };
    let age = snap.round.saturating_sub(g.created_round);
    if age >= g.ttl as u32 {
        memory.last_goal = None;
        return;
    }
    let target = g.target_entity().and_then(|t| snap.unit(t));
    if matches!(g.check_continuation(actor, target), Some(check) if check.severity == ContinuationSeverity::Invalidating) {
        memory.last_goal = None;
    }
}

/// Post-tick: store new goal after Move, clear after Cast/MoveAndCast.
/// EndTurn preserves goal across rounds (covered by pre_tick TTL/clear).
pub fn post_tick(
    memory: &mut AiMemory, decision: &AiDecision, chosen: Option<&ChosenInfo>,
    snap: &BattleSnapshot, actor: &UnitSnapshot, round: u32, tuning: &AiTuning,
) {
    match decision {
        AiDecision::Move { path, .. } => {
            if let (Some(c), Some(dest)) = (chosen, path.last().copied()) {
                memory.last_goal = extract_goal_context(c, snap, actor, dest, round, tuning);
            }
        }
        AiDecision::CastInPlace { .. } | AiDecision::MoveAndCast { .. } => {
            memory.last_goal = None;
        }
        AiDecision::EndTurn => {
            // Preserve — pre_tick will handle TTL on next call.
        }
    }
}
```

**3. `enemy_turn.rs` упрощается.**

Сейчас (после 6.7):
- `enemy_turn.rs:103–118` — early-return inline TTL clear с `FIXME(step 7)`.
- `enemy_turn.rs:262–299` — decision-block с `goal_obsolete` flag и conditional clear.

После 7.3:

```rust
fn run_ai_turn(...) {
    let actor_snap = build_actor_snapshot(...);  // нужен для goal_lifecycle::pre_tick
    goal_lifecycle::pre_tick(memory, &snap, actor_snap);  // ← TTL decay + invalidating clear

    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // tick_skipped log переедет сюда в 7.5; пока без log'а.
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    let (decision, debug_snapshot, fresh_chosen) = pick_action(...);
    // ... existing divergence-log block остаётся как есть до 7.5 ...

    goal_lifecycle::post_tick(memory, &decision, fresh_chosen.as_ref(), &snap, actor_snap, round, tuning);
    execute_decision(decision, msgs);
}
```

Inline TTL clear (lines 103–118) и decision-block (262–299) удаляются — заменены вызовами `goal_lifecycle::pre_tick` / `post_tick`. FIXME(step 7) проактивный clear исчезает (теперь в pre_tick централизован).

Divergence-log в full path **остаётся** в `enemy_turn.rs` без изменений до 7.5 — там его перенесут в `pick_action` финализатор и обогатят tick_skipped event'ом.

**Юнит-тесты:**
- `pre_tick_clears_when_ttl_expired` — last_goal с age=ttl → None.
- `pre_tick_clears_when_invalidating` — target_dead → None.
- `pre_tick_preserves_when_relevant_or_cosmetic` — Relevant/Cosmetic не очищает.
- `post_tick_stores_after_move` — Move → last_goal Some.
- `post_tick_clears_after_cast` — CastInPlace/MoveAndCast → None.
- `post_tick_preserves_after_endturn` — EndTurn не трогает.
- `repair_affinity_stage_populates_annotation` — annotations[i].repair_affinity correct.
- `repair_affinity_stage_no_stored_goal_is_noop` — без stored_goal stage не пишет.

**Gate.**
- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden **0/N** на post-6.8B corpus — поведение идентично (только refactor lifecycle).
- Divergence-log в full path не меняется (заработает в 7.5).

**Эстимейт:** 0.5 дня (small refactor).

---

### 7.4. Pipeline assembly + architectural converge

**Scope (расширенный после refined architecture review).**

Самый рискованный сабшаг — финальная сборка с архитектурной конвергенцией. После 7.0–7.3 есть все stages + lifecycle module, но остались overengineered абстракции из 7.0 (Pipeline struct, parallel scored/raw_factors arrays). В 7.4:

**1. Drop `Pipeline` struct → free function.**

`Pipeline { stages: Vec<Box<dyn PlanStage>> }` исчезает. Заменяется простой функцией:

```rust
pub fn run_pool_pipeline(pool: &mut ScoredPool, ctx: &mut StageCtx) {
    ViabilityStage.apply(pool, ctx);
    SanityStage.apply(pool, ctx);
    AdaptationStage.apply(pool, ctx);
    ProtectSelfMaskStage.apply(pool, ctx);
    KillableGateStage.apply(pool, ctx);
    RepairAffinityStage.apply(pool, ctx);
    PickBestStage.apply(pool, ctx);  // ← новый, см. п.3
}
```

Compile-time order, zero indirection, читается линейно. `Pipeline::new`, `add`, `run` — все удаляются.

**2. Move `score` + `raw_factors` INTO `PlanAnnotation`.**

```rust
// Было:
pub struct ScoredPool {
    pub plans: Vec<TurnPlan>,
    pub annotations: Vec<PlanAnnotation>,
    pub scored: Vec<f32>,         // ← удаляется
    pub raw_factors: Vec<PlanFactors>,  // ← удаляется
}

// Стало:
pub struct ScoredPool {
    pub plans: Vec<TurnPlan>,
    pub annotations: Vec<PlanAnnotation>,
}

pub struct PlanAnnotation {
    pub score: f32,                    // ← переехал
    pub raw_factors: PlanFactors,      // ← переехал
    pub viability: ViabilityResult,
    pub sanity: Vec<SanityHit>,
    pub adaptation: Option<AdaptationData>,
    pub contract: Option<ContractMaskHit>,
    pub repair_affinity: RepairAffinity,
    pub outcomes: Vec<ActionOutcomeEstimate>,
    pub terminal: TerminalScore,
    pub chosen: bool,                  // ← новый, set by PickBestStage
    pub pick: Option<PickInfo>,        // ← новый, set by PickBestStage только для chosen plan
}
```

Один parallel array вместо четырёх. Все per-plan данные = one source of truth.

**3. `PickBestStage` — pick тоже stage.**

```rust
pub struct PickBestStage;
impl PlanStage for PickBestStage {
    fn name(&self) -> &'static str { "pick_best" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let (best_idx, mech) = compute_best_idx(pool, ctx.rng, ...);
        pool.annotations[best_idx].chosen = true;
        pool.annotations[best_idx].pick = Some(PickInfo { mech, ... });
    }
}
```

`pick_best_plan` legacy free function исчезает. Логика — внутри stage.

**4. `pick_action` становится pure function.**

```rust
pub struct PickResult {
    pub decision: AiDecision,
    pub chosen: Option<ChosenInfo>,  // derive'ится из pool.annotations[i].chosen
    pub pool: ScoredPool,             // вернёт orchestrator'у для лога
    pub debug_snapshot: Option<AiDebugSnapshot>,
}

pub fn pick_action(
    actor: Entity, actor_pos: Hex, world: &AiWorld, snap: &BattleSnapshot,
    maps: &InfluenceMaps, rng: &mut DiceRng, memory: &AiMemory,  // ← &mut → &
    reservations: &Reservations,                                   // ← &mut → &
    debug: bool, debug_names: &HashMap<Entity, String>,
) -> PickResult {                                                  // ← без logger param
    let setup = setup_for_pick_action(...)?;
    let mut pool = ScoredPool::new(setup.plans);
    let mut ctx = StageCtx::new(&setup.scoring, setup.intent, setup.reason, actor_pos, rng);
    run_pool_pipeline(&mut pool, &mut ctx);
    let chosen_idx = pool.annotations.iter().position(|a| a.chosen);
    let decision = if let Some(idx) = chosen_idx {
        commit_plan(&pool.plans[idx], actor_pos).0
    } else {
        AiDecision::EndTurn  // fallback
    };
    let chosen = chosen_idx.map(|idx| build_chosen_info(&pool, idx, &ctx));
    let debug_snapshot = if debug { Some(build_debug_snapshot_from_pool(&pool, ...)) } else { None };
    PickResult { decision, chosen, pool, debug_snapshot }
}
```

`pick_action` не знает про `logger` или `reservations` mut. Чистый decision-maker. Reservations записываются в orchestrator'е (`enemy_turn.rs`) после получения PickResult. Logger тоже там.

**5. `enemy_turn.rs` = единственный orchestrator.**

```rust
fn run_ai_turn(...) {
    let memory_pre = memory.snapshot();  // capture pre-state for logging
    goal_lifecycle::pre_tick(memory, &snap, actor_snap);

    if c.ap.action_points <= 0 && !c.ap.can_move() {
        // skip-path log пишется здесь в 7.5; до тех пор просто endturn.
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    let result = pick_action(actor, actor_pos, &world, &snap, &maps, rng,
                             memory, &reservations, debug, &debug_names);

    // Log (объединённый actor_tick переедет сюда в 7.5; пока legacy формат).
    if logger.is_enabled() {
        write_legacy_decision_log(logger, ..., &result.pool, ...);
    }

    record_committed_reservations(&result, ..., reservations);
    goal_lifecycle::post_tick(memory, &result.decision, result.chosen.as_ref(), ...);

    execute_decision(result.decision, msgs);
}
```

Single orchestrator → single point для logging, lifecycle, reservations. `pick_action` — pure black box.

**6. Удаляются.**

- `PlanRanking` struct + impl + tests (полностью).
- `ranking.rs::apply_*` все методы.
- `ranking.rs::pick` (логика в PickBestStage).
- `Pipeline` struct, `Pipeline::new`, `add`, `run` (заменены free function'ой).
- `ScoredPool::scored`, `ScoredPool::raw_factors` (переехали в annotation).
- Параллельные `vec![PlanFactors::default(); n]` allocations в `ScoredPool::new` (теперь часть annotation).

**Юнит-тесты:**
- `pick_action_returns_pickresult_with_pool` — chosen.score == pool.annotations[chosen_idx].score.
- `pick_best_stage_marks_chosen` — exactly one annotation has chosen=true after stage.
- `run_pool_pipeline_runs_all_stages` — integration test через side-effect annotations.

**Gate.**
- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- **Per-entry golden review** — допустимо ≤10/N diverged для tie-breaking edge cases. >10 — расследовать.
- Replay roundtrip 0/N.

**Эстимейт:** 1.5 дня (правка ~300 строк pick_action + 200 строк ranking + ScoredPool/annotation refactor + миграция тестов + cleanup).

---

### 7.5. Schema v27 — unified `actor_tick` event + clean break

**Scope (расширенный — clean break, без v26 backward read).**

Полностью переписываем формат логов. Без compat compromises (per явное согласие пользователя «нет задачи поддерживать текущий формат»).

**1. Единственный event-type — `actor_tick`.**

Объединяет старые `actor_turn` + `plan_divergence` + новый `tick_skipped` в один формат:

```jsonl
{
  "event_type": "actor_tick",
  "schema_version": 27,
  "round": 2,
  "timestamp_ms": ...,
  "actor_id": ...,
  "actor_name": "...",
  "snapshot": {...},                    // полный (raздутый, но self-contained)
  "plans": [                             // [] на skip
    {
      "rank": 1,
      "chosen": true,
      "steps": [...],
      "annotation": {                    // unified per-plan
        "score": 2.7,
        "raw_factors": {...},
        "outcomes": [...],
        "terminal": {...},
        "viability": {...},
        "sanity": [...],
        "adaptation": {...} | null,
        "contract": {...} | null,
        "repair_affinity": {...},
        "pick": {...} | null             // только для chosen
      }
    }
  ],
  "decision": {
    "kind": "MoveAndCast" | "CastInPlace" | "Move" | "EndTurn" | "Skip",
    "reason": "no_ap_no_mp" | null,     // для Skip
    "ability": "...",                    // для Cast/MoveAndCast
    "target": ...,
    "path": [...]
  },
  "continuation": {                      // null когда нет stored_goal at tick start
    "stored_goal": {...},                // raw, не derived
    "severity": "Relevant" | "Invalidating" | "Cosmetic" | null,
    "age": 1
    // outcome derive'ится mining tool'ом через classify_continuation_outcome —
    // не пишется в лог (raw vs derived separation)
  }
}
```

Принципы:
- **Self-contained per-tick** — каждая запись standalone (snapshot redundancy ок).
- **Raw data only** — `outcome` строки (`goal_preserved_method_delivered` etc.) НЕ в логе. Mining derive'ит через `classify_continuation_outcome(stored, decision_kind, intent, reason, severity, age)`.
- **Annotation per-plan nested** — не parallel array на top-level.
- **Skip = degenerate `actor_tick`** — `decision.kind = "Skip"`, `plans = []`, snapshot минимальный (actor + units context).

**2. `ActorTickInput` helper для composition.**

```rust
pub struct ActorTickInput<'a> {
    pub round: u32,
    pub actor: Entity,
    pub actor_name: &'a str,
    pub snapshot: &'a BattleSnapshot,
    pub memory_pre: &'a MemoryPreState,    // captured before goal_lifecycle::pre_tick
    pub decision: &'a AiDecision,
    pub pool: Option<&'a ScoredPool>,      // None on skip
    pub debug_names: &'a HashMap<Entity, String>,
}

pub fn build_actor_tick_event(input: ActorTickInput) -> ActorTickEvent {
    // Pure function: assembles ActorTickEvent from components.
    // Used by both skip-path и full-path в enemy_turn.rs.
}

pub fn write_actor_tick_log(logger: &mut AiLogger, input: ActorTickInput) {
    let event = build_actor_tick_event(input);
    logger.write_event(&event);
}
```

DRY: один shape, два call sites uniform. `build_actor_tick_event` pure → юнит-тестируется отдельно.

**3. `enemy_turn.rs` обновляется.**

```rust
fn run_ai_turn(...) {
    let memory_pre = capture_memory_pre_state(memory);
    goal_lifecycle::pre_tick(memory, ...);

    if c.ap.action_points <= 0 && !c.ap.can_move() {
        write_actor_tick_log(logger, ActorTickInput {
            round, actor, actor_name, snapshot: &snap, memory_pre: &memory_pre,
            decision: &AiDecision::EndTurn,  // skip
            pool: None,
            ...
        });
        msgs.end_turn.write(EndTurn { actor });
        return;
    }

    let result = pick_action(...);

    write_actor_tick_log(logger, ActorTickInput {
        round, actor, actor_name, snapshot: &snap, memory_pre: &memory_pre,
        decision: &result.decision,
        pool: Some(&result.pool),
        ...
    });

    record_committed_reservations(...);
    goal_lifecycle::post_tick(memory, ...);
    execute_decision(result.decision, msgs);
}
```

Tick_skipped event закрывается **полностью** — закрывает carry-over из 6.7.

**4. Schema v26 read — не поддерживается.**

`replay_ai_log` и `mine_ai_logs` читают только v27. Pre-v27 logs — **error при загрузке**, понятная ошибка «schema v26 unsupported, v27+ required».

**Что теряется:**
- `logs/` historical — нечитаемо. Но они уже ротируются.
- Golden `logs/golden_post_step6.jsonl` (post-6.8B) — пересобираем на свежем v27 playtest'е.
- 1 existing fixture `tests/ai_scenarios/snapshots/continuation_relevant_preserved/` — пересобираем.

**Что выигрываем:**
- Mining/replay tools на ~30% проще (нет dual-format read).
- Один event-type — нет cross-event correlation.
- Shape стабилен (raw data, не привязан к classifier semantics).

**5. `mine_ai_logs.rs` rewrite.**

- Читает только `actor_tick` events (v27).
- Continuation analysis (C6 + 6.6b refinements) теперь derive'ит outcome через `classify_continuation_outcome` на каждом event'е с continuation section.
- Skip events дают новые метрики:
  - "actor passed with stored goal" %.
  - "ttl_expired on skip path" % (раньше invisible).
- Existing секции A1–C6 переинтерпретируются под новый формат.

**6. `replay_ai_log.rs` rewrite.**

- Читает actor_tick events (v27).
- На каждом не-skip event'е восстанавливает state и сравнивает re-pick decision с logged decision.
- `--capture-golden` пишет golden in v27 формате.
- `--compare-golden` сверяет per actor_tick.

**7. Rebuild artifacts.**

После 7.5 merge:
- Свежий v27 playtest (6 файлов как сейчас).
- `replay_ai_log --capture-golden` → новый `logs/golden_post_step7.jsonl`.
- `continuation_relevant_preserved` fixture пересоздаётся из свежего v27 entry.

**Юнит-тесты:**
- `actor_tick_event_round_trips` — write → read → equal.
- `build_actor_tick_event_skip_has_no_pool` — pool=None → plans=[].
- `build_actor_tick_event_full_has_chosen_annotation` — annotation.chosen exactly one.
- `mine_classifies_continuation_via_classifier_function` — outcome derive consistent.
- `replay_v27_round_trip_zero_diff` — capture + compare = 0/N.

**Gate.**
- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- v27 golden capture на свежем playtest'е → round-trip 0/N.
- v26 logs дают clean error при попытке load (не panic).
- Mining v27 corpus отрабатывает, secретs из C6 (post-6.8B baseline) воспроизводятся: preserved ≈55%, voluntary ≈14%, reactive ≈31%.

**Эстимейт:** 2.0 дня (формат с нуля + двух tools rewrite + rebuild golden + fixture).

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
| 7.2 | Contract stages (Adaptation + ProtectSelfMask + KillableGate) | 1.0 | golden 0/116 | done (`79e7371`) |
| 7.3 | RepairAffinityStage + goal_lifecycle module | 0.5 | golden 0/N, lifecycle централизован | pending |
| 7.4 | Pipeline assembly + architectural converge (drop Pipeline struct, score/raw_factors INTO annotation, PickBestStage, pure pick_action, single orchestrator) | 1.5 | per-entry golden ≤10/N | pending |
| 7.5 | Schema v27 unified `actor_tick` + clean break (closes 6.7 carry-over fully) | 2.0 | v27 round-trip 0/N, mining baseline воспроизводится | pending |
| 7.6 | ai_scenarios state injection (closes 6.9 carry-over) | 1.0 | 4 pending continuation_* fixtures green на v27 формате | pending |

**Суммарно ~7 дней** (0.5 + 1.0 + 1.0 + 0.5 + 1.5 + 2.0 + 1.0 = 7.5; 7.0–7.2 done — оставшиеся 4.5 дня).

## Зафиксированные решения

### Architectural foundations (фиксированные в 7.0–7.2)

1. **Все per-plan данные в `PlanAnnotation`** — `score`, `raw_factors`, `viability`, `sanity`, `adaptation`, `contract`, `repair_affinity`, `outcomes`, `terminal`, `chosen`, `pick`. Один source of truth per plan.
2. **`PlanRanking` удаляется в 7.4** — после миграции всех `apply_*` методов в stages. До 7.4 live дублируется (legacy + pool).
3. **Adaptation перестаёт быть «особенной»** — `IntentReason::Adapted` wrap читает `pool.annotations[best_idx].adaptation` в финализаторе. Реализовано в 7.2.

### Refined architecture decisions (после critique в 7.3 review)

4. **`Pipeline` struct → free function `run_pool_pipeline`** (в 7.4). Убирает overengineered abstraction; compile-time order, zero indirection.
5. **`GoalLifecycle` = explicit module, НЕ stage** (в 7.3). `pre_tick` + `post_tick` вызываются orchestrator'ом (`enemy_turn.rs`) напрямую. Никакого `is_pre_stage()` predicate, никакого `Pipeline::run_pre_stages_only()`.
6. **`PickBestStage` — pick тоже stage** (в 7.4). `chosen: bool` + `pick: Option<PickInfo>` → annotation. Уход от free function.
7. **`pick_action` — pure function** (в 7.4). Возвращает `PickResult { decision, chosen, pool, debug_snapshot }`. Не принимает `&mut logger`. Side effects (logging, reservations, lifecycle) — в orchestrator.
8. **`enemy_turn.rs` — единственный orchestrator** (в 7.4). Все side effects (lifecycle, logging, reservations) централизованы.

### Log format decisions (для 7.5)

9. **Single `actor_tick` event** — объединяет старые `actor_turn` + `plan_divergence` + новый skip case. Self-contained per tick (snapshot redundancy ок).
10. **Schema v27 = clean break** — без v26 backward read. v26 logs дают понятный error при load. Trade-off обсуждён, явно одобрен.
11. **Raw data only в логах** — `outcome` строки derive'ятся mining tool'ом через `classify_continuation_outcome`. Логи стабильны across classifier evolution.
12. **`ActorTickInput` helper для composition** — DRY между skip и full path в `enemy_turn.rs`. `build_actor_tick_event` pure → юнит-тестируется отдельно.

### Scope decisions

13. **`ai_scenarios` state injection в scope step 7 как 7.6** — закрывает 6.9 carry-over в одном PR с pipeline. Fixture format расширяется `[ai_memory]` секцией.

### Отвергнутые альтернативы

- **Schema v26 backward read** — отвергнуто (явный clean break per согласию пользователя).
- **`is_pre_stage()` trait predicate** — overengineering для одного use case; goal_lifecycle module проще.
- **Pre-classified `outcome` в логах** — отвергнуто (raw vs derived separation, mining derives).
- **`round_snapshot` separation** — отвергнуто (mid-round state changes ломают round-level snapshot; redundancy ок, gzip жмёт).
- **EvaluationMode::Continuation как 3-й режим** — отвергнуто ещё в step 6.
- **Repair bonus calibration через `repair_bonus_scale`** — отвергнуто в step 6.8 (см. step6_plan §6.8 pivot).

## Критические файлы

- `src/combat/ai/pipeline/mod.rs` — `PlanStage` trait + `ScoredPool` + `StageCtx`. **`Pipeline` struct удаляется в 7.4** — заменяется free function `run_pool_pipeline`.
- `src/combat/ai/pipeline/stages/` — stages: `viability.rs`, `sanity.rs`, `adaptation.rs`, `protect_self.rs`, `killable_gate.rs`, `repair_affinity.rs` (7.3), `pick_best.rs` (7.4).
- `src/combat/ai/repair/lifecycle.rs` (новый в 7.3) — `pre_tick` / `post_tick` free functions.
- `src/combat/ai/utility/mod.rs` — `pick_action` становится pure в 7.4 (returns `PickResult`).
- `src/combat/ai/utility/ranking.rs` — удаляется в 7.4 (полностью).
- `src/combat/ai/outcome.rs` — `PlanAnnotation` всё per-plan; в 7.4 принимает `score`/`raw_factors`/`chosen`/`pick`.
- `src/combat/ai/enemy_turn.rs` — единственный orchestrator после 7.4. Логика lifecycle через `goal_lifecycle::pre_tick`/`post_tick` (7.3); единое logging через `write_actor_tick_log` (7.5).
- `src/combat/ai/log.rs` — переписывается в 7.5 под `actor_tick` event + `ActorTickInput` helper. SCHEMA_VERSION v26→v27, **clean break (no v26 read)**.
- `src/bin/replay_ai_log.rs` — rewrite в 7.5 под v27 only.
- `src/bin/mine_ai_logs.rs` — rewrite в 7.5 под v27 only; outcome derive через `classify_continuation_outcome`.
- `tests/ai_scenarios.rs` — `[ai_memory]` injection в 7.6.
- `tests/ai_scenarios/snapshots/continuation_*` — пересборка в v27 формате (1 existing + 4 новых в 7.6).
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
