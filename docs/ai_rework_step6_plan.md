# Шаг 6 — Goal-preserving plan repair: декомпозиция на сабшаги

Декомпозиция в стиле фаз 2a / step 3 / step 4 / step 5: коммит-на-сабшаг, gate на каждом.
Спецификация: `docs/ai_rework.md` §6 (бывшие #2, #3, #4 объединены).

## Preamble

### Текущее состояние plan freeze

Сейчас механизм держится на **жёстком binary**: либо точное продолжение сохранённого плана, либо полный replan от fresh-плана.

Архитектура (`enemy_turn.rs:160–212`):

1. `pick_action` всегда строит **fresh plan** (нужен и как fallback, и для divergence-логов).
2. Если `memory.last_plan: Option<StoredPlan>` существует и `settings.ai_freeze_plan_after_move`:
   - `PlanSnapshot::mismatch(actor, target)` — bool-проверка из 7 классов (`actor_pos_mismatch`, `actor_hp_drop`, `actor_rage_changed`, `actor_status_changed`, `target_gone`, `target_entity_changed`, `target_hp_drop`, `target_moved`).
   - При `None` (no mismatch) → `continuation_from_stored` исполняет сохранённый Cast step без переоценки.
   - При `Some(reason)` → fresh plan, `replan_reason = reason`.
3. После Move-decision `memory.last_plan = Some(StoredPlan { steps, step_index, snapshot, intent, cast_ability, cast_target, score })`.
4. Логирование: `write_plan_divergence(stored, fresh, used_continuation, replan_reason)` — есть всегда, когда есть stored+fresh.

### Проблемы текущей схемы

**Хрупкость.** Любой mismatch — даже косметический rage tick (`actor_rage_changed`) — выбрасывает stored plan и стартует с нуля. Mining post-step-3 (`docs/ai_need_signals.md:184`) показал `actor_hp_drop` 21.6% → 0% после step 3, но `actor_rage_changed` остаётся ненулевым source шумных replan'ов; `target_hp_drop` (4.8% continuation-failure до step 3) — нормальная ситуация (свой удар), которая обнуляет план.

**Бинарность.** Stored plan либо исполняется bit-for-bit, либо игнорируется. Нет градации «цель та же, но удобнее ударить с другого тайла», «способ изменился, цель сохранена». Это и есть основной источник oscillation, который step 3.3 (`continue_commitment` → stickiness modulation) **частично** закрыл — но stickiness работает на уровне intent, не на уровне goal+method.

**Семантический пробел.** `mismatch()` сравнивает factual snapshot (HP, pos, rage, status hash). Не сравнивает **замысел**: «я добиваю того раненого справа», «я разогнался в коридор», «я зарядил setup для AoE на следующем ходу». Эти намерения — невидимые для текущей freeze-логики.

**Hard cliff replanning.** Когда плана нет (первый tick после Cast/EndTurn), evaluator один и тот же — `discovery`. Когда план был и сорвался, тот же evaluator оценивает fresh plans без учёта уже совершённого commitment'а: AoO съеденный, mana потраченная, союзник под прикрытием. На втором tick'е после Move актор формально «начинает с нуля».

### Что закрывает step 6

**Goal context vs StoredPlan.** Вместо «сохранённый план как BLOB шагов» хранится **замысел** (goal kind + target + region + setup + TTL + confidence). Это позволяет на каждом tick'е строить **новый** fresh plan, но премировать те fresh plans, которые сохраняют сохранённый замысел.

**Repair affinity.** Каждый fresh plan на втором (и далее) tick'е получает скоринговый бонус, пропорциональный соответствию stored goal. `goal_preserved` — крупный бонус; `method_preserved` — дополнительный мелкий бонус; `goal_abandoned` — ноль.

**Семантическая инвалидация.** `PlanContinuationCheck` классифицирует mismatch-ы на три класса:
- `Cosmetic` — изменение, не отражающее на goal'е (rage tick, status duration tick без статус-set delta, mana spent на эту же цель).
- `Relevant` — изменение требует переоценки method'а, но goal остаётся валидным (target moved, hp dropped — repair affinity ослабляется, но goal alive).
- `Invalidating` — goal недостижим (target dead, target out of all reach, actor stunned/disabled, TTL expired).

**Два evaluator'а.** `DiscoveryEvaluator` (default) и `ContinuationEvaluator` (когда есть stored goal). Continuation поднимает веса `continue_commitment`, `goal_alignment`, `path_stability`; снижает шум noise term; смягчает self-preserve флор для целей внутри сохранённого замысла (commitment-aware). Это **не** mode switch (как `Adapted`/`ProtectSelfNoDefensive`), а **смена набора весов** при aggregator'е.

### Что НЕ в scope шага 6

- **PlanStage trait + pipeline (step 7)** — формализация «GoalRepair» как именованной стадии откладывается до 7. В step 6 — точечная вставка в `pick_action` после `finalize_scores`, перед `pick_best_plan`.
- **Critics decomposition (step 10)** — некоторые goal-aware critics (например, `OvercommitIntoDanger` с учётом TTL) логично делать после step 10. В step 6 — только bonus/penalty в финальном score.
- **Bands+agenda+scorecard (step 11)** — band-уровень goal'ы (`HardRescueOpportunity`, `ForcedTargeting`) появятся как отдельные goal_kind'ы в step 11.
- **Team blackboard (step 13)** — `setup_marker` ограничен per-actor; collective intent — step 13.
- **Telegraphing (step 15)** — тоже использует `PlanContinuationCheck` (бесплатный re-use), но fork-point добавляется в step 15.

### Зафиксированные решения по развилкам

**1. StoredGoalContext — расширение, не замена `StoredPlan`** (1a).

Не удаляем `StoredPlan` целиком. Вводим новый тип `StoredGoalContext` рядом, и `AiMemory` хранит **оба**: `last_plan: Option<StoredPlan>` (для backward-compatible exact continuation как ceiling), `last_goal: Option<StoredGoalContext>` (для repair affinity). Точное продолжение остаётся доступно как «redundancy ceiling» — если fresh plan **тождественно совпадает** со stored, exact-continuation срабатывает без пересчёта (cheap optimization). Полный replace `StoredPlan` → backlog после step 7 pipeline.

**Альтернатива (1b):** немедленно удалить `StoredPlan`, оставить только `StoredGoalContext`. Отвергнута: `continuation_from_stored` сейчас — единственное место, где cast выполняется без re-validation, его удаление ломает нюансы `validate_action_system` lifecycle (см. `enemy_turn.rs:312–342`); миграция этого пути — отдельная работа и распадается на step 7.

**2. Repair affinity как scoring bonus, не как mode** (2a).

Repair affinity вживляется в `finalize_scores` как **modifier** к финальному score (после `factor_sum + terminal_sum + summon_bonus + trade_bonus`), не как переключение evaluator'а. Это сохраняет «`SelfLethalWithoutPayoff`-инварианты sanity действуют». Continuation evaluator (см. ниже) меняет **веса** в aggregator'е, не sanity-инварианты.

**Альтернатива (2b):** EvaluationMode::Continuation как 3-й режим (после Default/Adapted). Отвергнута: сложность ProtectSelf mask + adaptation интеракции с третьим mode'ом непропорциональна выгоде; goal preservation решается additively.

**3. Все mismatch-классы переедут в `PlanContinuationCheck` сразу** (3a).

Все 7 текущих кодов из `mismatch()` классифицируются в `Cosmetic/Relevant/Invalidating` в 6.0 (без поведенческих изменений). Дальше уже работаем со структурированной классификацией.

**Альтернатива (3b):** добавлять классы по мере необходимости. Отвергнута: лучше один проход с per-code решением, чем итеративные правки.

**4. `goal_kind` минимальный enum в первой волне** (4a).

```rust
pub enum GoalKind {
    Finish { target: Entity },              // FocusTarget kill
    Pressure { target: Entity },            // FocusTarget damage без kill
    DisableEnemy { target: Entity },        // ApplyCC
    HealAlly { ally: Entity },              // ProtectAlly heal
    Retreat { region_anchor: Hex },         // ProtectSelf, LastStand с retreat
    SetupAOE { region_center: Hex },        // SetupAOE positioning
    Reposition { region_center: Hex },      // pure positioning
}
```

Никаких `region/corridor` объектов как отдельных типов на этом шаге — всё через единичный `Hex` + max-radius из `tuning.thresholds.repair_region_radius` (default = 2). `setup_marker` — `Option<AbilityId>` (если планировался cast следующим). TTL — `u8` rounds (default 2). Confidence — `f32` 0–1, заполняется как `chosen.score / max_pool_score` в момент store.

**Альтернатива (4b):** `GoalKind` с corridor/zone/blueprint типами сразу. Отвергнута: излишняя generality для волны 1; corridor становится load-bearing после step 17 (geometry awareness).

### Природа gate'ов в step 6

Как в step 3 — **сами решения должны двигаться** (это и есть цель шага), но в более узком диапазоне (continuation-related только).

- **6.0–6.2** (scaffolding + producer + repair_affinity computation без consumers): golden 0/131 diff (reading-only).
- **6.3** (consumer — repair affinity bonus в finalize_scores): per-entry diff допустим. Целевые сдвиги: oscillation-cases от mining'а пропадают; replan-spam в стабильных commitments снижается.
- **6.4** (continuation evaluator — два набора весов): per-entry diff допустим. Целевые сдвиги: continuation tighter на self-preserve флоре, looser на goal alignment.
- **6.5** (log overhaul + mining расширение): goldens могут двигаться из-за removed-redundant-fields; per-entry разбор.
- **6.6** (migration `continuation_from_stored` → repair-only path + schema bump): rebaseline golden как новый baseline.

**Real gate** — это **classification-table в `mine_ai_logs`**: `goal_preserved|method_preserved`, `goal_preserved|method_changed`, `goal_abandoned|reason`. Цель:
- `goal_preserved|method_preserved` >= 60% (естественное FocusTarget commitment).
- `goal_abandoned|target_dead` ≈ свободно (правильный replan).
- `goal_abandoned|cosmetic_mismatch` = 0 (раньше был ненулевой через `actor_rage_changed`).
- Stable через сотни тиков на одной встрече (новые scenarios test это явно).

## Сабшаги

### 6.0. Scaffolding: `PlanContinuationCheck` + классификация mismatch-кодов

**Scope.**

Новый модуль `src/combat/ai/repair/mod.rs`:

```rust
/// Семантический результат сравнения сохранённого goal'а с текущим миром.
/// Заменяет binary `mismatch() -> Option<&str>` в downstream-слоях.
///
/// Cosmetic — изменение, не влияет на достижимость goal'а (rage tick, mana spent,
///   status duration tick).
/// Relevant — goal остаётся достижим, но method может измениться (target moved,
///   target hp drop, AoO съедено).
/// Invalidating — goal недостижим (target dead, actor stunned, TTL expired,
///   reach gap > all_abilities range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContinuationSeverity {
    Cosmetic,
    Relevant,
    Invalidating,
}

#[derive(Debug, Clone)]
pub struct PlanContinuationCheck {
    pub severity: ContinuationSeverity,
    pub reason_code: &'static str,  // re-use existing mismatch codes for telemetry
}

/// Классифицирует код из `PlanSnapshot::mismatch()` в семантический severity.
/// Pure function, no state.
pub fn classify_mismatch(code: &'static str) -> ContinuationSeverity {
    match code {
        "actor_rage_changed"        => ContinuationSeverity::Cosmetic,    // rage tick, AoO эффект сам по себе не goal-relevant
        "actor_status_changed"      => ContinuationSeverity::Relevant,    // could be CC — Invalidating after step 9 semantic tags
        "actor_hp_drop"             => ContinuationSeverity::Relevant,    // affords self-preserve re-eval, goal alive
        "actor_pos_mismatch"        => ContinuationSeverity::Invalidating,// real-pipeline дёрнул actor куда-то ещё
        "target_gone"               => ContinuationSeverity::Invalidating,
        "target_entity_changed"     => ContinuationSeverity::Invalidating,
        "target_hp_drop"            => ContinuationSeverity::Relevant,    // damage другим actor'ом, goal possibly faster
        "target_moved"              => ContinuationSeverity::Relevant,    // method может смениться, goal жив
        _                           => ContinuationSeverity::Invalidating, // unknown → safe
    }
}
```

Producer добавляется параллельно `mismatch()`:

```rust
impl PlanSnapshot {
    pub fn check_continuation(
        &self,
        actor: &UnitSnapshot,
        target: Option<&UnitSnapshot>,
    ) -> Option<PlanContinuationCheck> {
        self.mismatch(actor, target).map(|code| PlanContinuationCheck {
            severity: classify_mismatch(code),
            reason_code: code,
        })
    }
}
```

**Plumbing в `enemy_turn.rs`.**

```rust
// Старая ветвь:
let mismatch = stored.snapshot.mismatch(actor_snap, target_snap);
if let Some(reason) = mismatch { … }

// Новая (поведение идентично, just reads severity):
let check = stored.snapshot.check_continuation(actor_snap, target_snap);
if let Some(ck) = &check {
    replan_reason = Some(ck.reason_code);
    fresh_decision  // как и раньше — exact continuation выкидывается
}
```

Severity пока **не используется** для решений — только для логов:

```rust
// в write_plan_divergence — добавить поле continuation_severity:
let entry = PlanDivergenceEntry {
    ...,
    replan_reason,
    continuation_severity: check.as_ref().map(|c| c.severity),  // None если no mismatch
    ...
};
```

**Schema.** Поле добавляется с `#[serde(default)]`. SCHEMA_VERSION **не bump'аем** на 6.0 — поле опциональное в JSONL, старые логи читаются.

**Юнит-тесты в `repair/mod.rs::tests`:**
- `classify_all_existing_codes_have_explicit_severity` — exhaustive match (защищает от silent fall-through на unknown).
- `cosmetic_codes_dont_invalidate_goal`.
- `invalidating_codes_safe_default`.

**Gate.** `cargo test/clippy`, `ai_scenarios` зелёный, golden **0 / 131 diff** (никто severity не читает в decision-path).

**Эстимейт:** 0.5 дня.

---

### 6.1. Goal extraction: `StoredGoalContext` + producer

**Scope.**

Новый тип в `repair/goal.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalKind {
    Finish { #[serde(with = "...")] target: Entity },
    Pressure { #[serde(with = "...")] target: Entity },
    DisableEnemy { #[serde(with = "...")] target: Entity },
    HealAlly { #[serde(with = "...")] ally: Entity },
    Retreat { region_anchor: Hex },
    SetupAOE { region_center: Hex, planned_ability: AbilityId },
    Reposition { region_center: Hex },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGoalContext {
    pub kind: GoalKind,
    /// Hex anchor for region/corridor checks. For target-bound kinds == target_pos at store time.
    pub region_anchor: Hex,
    /// Radius (hex distance) within which positions are considered "on goal".
    /// Read from `tuning.thresholds.repair_region_radius` at store time.
    pub region_radius: u32,
    /// Optional: ability we expected to cast as the climax of this goal.
    /// Used to bonus method_preserved over method_changed.
    pub planned_ability: Option<AbilityId>,
    /// Rounds remaining before the goal expires. Decremented per turn,
    /// invalidated when 0.
    pub ttl: u8,
    /// 0..1 — нормализованная "уверенность" в goal'е, влияет на размер repair-bonus'а.
    /// Заполняется как chosen.score / pool_max_score в момент store.
    pub confidence: f32,
    /// Round when the goal was created. Used for ttl decay & telemetry.
    pub created_round: u32,
}
```

Producer в `repair/goal.rs`:

```rust
/// Extract a `StoredGoalContext` from a chosen plan + intent. Called when
/// `AiMemory.last_plan` would be set (after Move decision in run_ai_turn).
/// Pure function over chosen / snap / tuning.
pub fn extract_goal_context(
    chosen: &ChosenInfo,
    snap: &BattleSnapshot,
    round: u32,
    pool_max_score: f32,
    tuning: &AiTuning,
) -> Option<StoredGoalContext> {
    let kind = match chosen.intent {
        TacticalIntent::FocusTarget { target } => {
            // Distinguish Finish vs Pressure by target HP and outcome.
            let t = snap.unit(target)?;
            let p_kill_now: f32 = chosen.plan.annotation.outcomes.iter()
                .map(|o| o.p_kill_now).sum::<f32>().min(1.0);
            if p_kill_now >= tuning.thresholds.goal_finish_p_kill || t.hp_pct() < 0.30 {
                GoalKind::Finish { target }
            } else {
                GoalKind::Pressure { target }
            }
        }
        TacticalIntent::ApplyCC { target } => GoalKind::DisableEnemy { target },
        TacticalIntent::ProtectAlly { ally } => GoalKind::HealAlly { ally },
        TacticalIntent::ProtectSelf | TacticalIntent::LastStand => {
            GoalKind::Retreat { region_anchor: chosen.plan.final_pos }
        }
        TacticalIntent::SetupAOE => {
            // Recover planned ability from step[1] if it's an AoE Cast.
            let ability = chosen.plan.steps.get(1).and_then(|s| match s {
                PlanStep::Cast { ability, .. } => Some(ability.clone()),
                _ => None,
            })?;
            GoalKind::SetupAOE {
                region_center: chosen.plan.final_pos,
                planned_ability: ability,
            }
        }
        TacticalIntent::Reposition => {
            GoalKind::Reposition { region_center: chosen.plan.final_pos }
        }
    };

    let region_anchor = match &kind {
        GoalKind::Finish { target } | GoalKind::Pressure { target }
            | GoalKind::DisableEnemy { target } => snap.unit(*target)?.pos,
        GoalKind::HealAlly { ally } => snap.unit(*ally)?.pos,
        GoalKind::Retreat { region_anchor } => *region_anchor,
        GoalKind::SetupAOE { region_center, .. } | GoalKind::Reposition { region_center } => *region_center,
    };

    let planned_ability = chosen.plan.steps.get(1).and_then(|s| match s {
        PlanStep::Cast { ability, .. } => Some(ability.clone()),
        _ => None,
    });

    Some(StoredGoalContext {
        kind,
        region_anchor,
        region_radius: tuning.thresholds.repair_region_radius,
        planned_ability,
        ttl: tuning.thresholds.repair_default_ttl,
        confidence: (chosen.score / pool_max_score.max(1e-6)).clamp(0.0, 1.0),
        created_round: round,
    })
}
```

**`AiMemory` extension.**

```rust
pub struct AiMemory {
    pub last_intent: Option<IntentKind>,
    pub last_target: Option<Entity>,
    pub turns_committed: u8,
    pub last_plan: Option<StoredPlan>,
    /// NEW (6.1): goal context extracted from the last chosen plan.
    /// Set in parallel with last_plan; used by repair affinity (6.2).
    pub last_goal: Option<StoredGoalContext>,
    pub hp_ratio_at_last_turn: Option<f32>,
    pub last_turn_was_defensive: bool,
    pub turns_in_low_hp: u8,
}
```

**Plumbing.**

`run_ai_turn` (после `if let AiDecision::Move = decision`) — рядом с `last_plan = Some(StoredPlan { ... })` добавить:

```rust
let pool_max = chosen.score.max(1.0);  // если есть better — score < pool_max
let round = ctx.world.round;            // нужен в ScoringCtx; возможно already there
memory_ref.last_goal = extract_goal_context(&chosen, &snap, round, pool_max, &tuning);
```

Поле `round` уже доступно через `world.round` в `ScoringCtx` после step 5; верифицировать. `pool_max` — sanity-fallback (если `pool_max_score` не сохранён, confidence = 1.0).

**TOML дополнения (`assets/data/ai_tuning.toml`):**

```toml
[thresholds]
# ... existing ...
repair_region_radius = 2          # Hex radius for "on-goal position" checks
repair_default_ttl = 2            # rounds before stored goal expires
goal_finish_p_kill = 0.6          # threshold for Finish vs Pressure classification
```

`AiTuning::Thresholds` extension — 3 новых scalar поля.

**TTL decay.** Когда `last_goal` восстанавливается на следующем tick'е, делаем `ttl -= 1` если `round > created_round`. При `ttl == 0` стираем (= `goal_abandoned|ttl_expired`).

**Юнит-тесты в `repair/goal.rs::tests`:**
- `extract_finish_for_low_hp_target`.
- `extract_pressure_for_high_hp_target`.
- `extract_setupaoe_recovers_planned_ability`.
- `extract_retreat_uses_final_pos_anchor`.
- `confidence_clamps_to_unit_interval`.

**Schema.** SCHEMA_VERSION **не bump'аем** в 6.1. `last_goal` пока никем не сериализуется — runtime-only. Появится в JSONL вместе с logs overhaul (6.5).

**Gate.** `cargo test/clippy`, `ai_scenarios` зелёный, golden **0 / 131 diff** (producer пишет `last_goal`, никто не читает).

**Эстимейт:** 1.0 день.

---

### 6.2. Repair affinity computation (read-only)

**Scope.**

Pure-functional модуль `repair/affinity.rs`:

```rust
/// Compositional affinity components — each axis is 0..1, aggregated with
/// tuned weights into a single repair_affinity bonus in 6.3.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RepairAffinity {
    /// goal_kind+target match (0 = abandoned, 1 = exact match)
    pub goal_alignment: f32,
    /// fresh plan ends within stored region_radius of region_anchor
    pub region_alignment: f32,
    /// fresh plan's step[1] uses stored planned_ability
    pub method_alignment: f32,
    /// 1 - severity_penalty (Cosmetic=1, Relevant=0.7, Invalidating=0)
    pub severity_factor: f32,
    /// ttl decay multiplier (1.0 at fresh, 0 at expired)
    pub ttl_factor: f32,
    /// stored goal confidence at store-time
    pub confidence: f32,
}

impl RepairAffinity {
    /// Aggregate to single signed bonus in [-1, +1]. Weights from tuning.
    /// All-zeros input → 0 (no goal stored).
    pub fn aggregate(&self, weights: &RepairWeights) -> f32 {
        let combined = self.goal_alignment * weights.goal_w
            + self.region_alignment * weights.region_w
            + self.method_alignment * weights.method_w;
        combined * self.severity_factor * self.ttl_factor * self.confidence
    }
}
```

Producer:

```rust
pub fn compute_repair_affinity(
    fresh: &TurnPlan,
    fresh_intent: TacticalIntent,
    stored: &StoredGoalContext,
    severity: ContinuationSeverity,
    current_round: u32,
) -> RepairAffinity {
    let goal_alignment = match (&stored.kind, fresh_intent) {
        (GoalKind::Finish { target: a }, TacticalIntent::FocusTarget { target: b }) if *a == b => 1.0,
        (GoalKind::Pressure { target: a }, TacticalIntent::FocusTarget { target: b }) if *a == b => 0.85,
        (GoalKind::DisableEnemy { target: a }, TacticalIntent::ApplyCC { target: b }) if *a == b => 1.0,
        (GoalKind::HealAlly { ally: a }, TacticalIntent::ProtectAlly { ally: b }) if *a == b => 1.0,
        (GoalKind::Retreat { .. }, TacticalIntent::ProtectSelf) => 0.9,
        (GoalKind::Retreat { .. }, TacticalIntent::LastStand) => 0.9,
        (GoalKind::SetupAOE { .. }, TacticalIntent::SetupAOE) => 0.95,
        (GoalKind::Reposition { .. }, TacticalIntent::Reposition) => 0.85,
        // cross-goal — partial credit when actor-target pair matches:
        (GoalKind::Finish { target: a }, TacticalIntent::ApplyCC { target: b }) if *a == b => 0.4,
        _ => 0.0,
    };

    let region_alignment = {
        let dist = fresh.final_pos.unsigned_distance_to(stored.region_anchor);
        if dist <= stored.region_radius { 1.0 - dist as f32 / (stored.region_radius + 1) as f32 }
        else { 0.0 }
    };

    let method_alignment = match (&stored.planned_ability, fresh.steps.get(1)) {
        (Some(stored_ab), Some(PlanStep::Cast { ability, .. })) if stored_ab == ability => 1.0,
        _ => 0.0,
    };

    let severity_factor = match severity {
        ContinuationSeverity::Cosmetic => 1.0,
        ContinuationSeverity::Relevant => 0.7,
        ContinuationSeverity::Invalidating => 0.0,
    };

    let age = current_round.saturating_sub(stored.created_round);
    let ttl_factor = if age >= stored.ttl as u32 { 0.0 }
                     else { 1.0 - age as f32 / stored.ttl as f32 };

    RepairAffinity { goal_alignment, region_alignment, method_alignment, severity_factor, ttl_factor, confidence: stored.confidence }
}
```

**Plumbing.** В `pick_action` после `finalize_scores` (но до `pick_best_plan`) — посчитать affinity для каждого fresh plan'а, сохранить в `plan.annotation.repair_affinity: RepairAffinity` (новое поле в `PlanAnnotation`). Никто не читает в decision-path.

**`PlanAnnotation` extension.**

```rust
pub struct PlanAnnotation {
    // existing: outcomes, terminal, ...
    /// 6.2: repair affinity per plan, computed when AiMemory has last_goal.
    /// Default (zero-filled) when no stored goal exists.
    pub repair_affinity: RepairAffinity,
}
```

`#[serde(default)]` — runtime-only до 6.5/6.6.

**TOML — `axis_repair_weights` table** в `[tables]` (5 ролей × 3 axes — goal/region/method):

```toml
[tables.axis_repair_weights]
# Тоже самое симметрично factor_weights / terminal_weights:
# rows = roles (Tank/Melee/Ranged/Control/Support), cols = (goal, region, method)
tank    = [0.5, 0.3, 0.2]
melee   = [0.6, 0.2, 0.2]
ranged  = [0.5, 0.3, 0.2]
control = [0.4, 0.3, 0.3]   # method matters больше для setup'а
support = [0.7, 0.2, 0.1]   # goal_alignment почти всё
```

`AxisProfile::repair_weights(tuning) -> RepairWeights` симметрично `terminal_weights`.

**Юнит-тесты:**
- `goal_alignment_perfect_for_same_target_finish`.
- `goal_alignment_zero_for_different_intent`.
- `region_alignment_decays_with_distance`.
- `method_alignment_set_when_planned_ability_matches`.
- `severity_factor_zero_for_invalidating`.
- `ttl_factor_zero_when_age_exceeds_ttl`.
- `aggregate_zero_when_severity_invalidating` (multiplicative gate).

**Gate.** Golden **0 / 131 diff** (поле populated, никто не читает в финальном score'е).

**Эстимейт:** 1.5 дня (несколько вариантов goal_alignment, edge cases).

---

### 6.3. Consumer: repair affinity bonus в `finalize_scores`

**Scope.**

Это первый сабшаг с golden diff'ом. В `finalize_scores`:

```rust
// after factor_sum + terminal_sum + summon_bonus + trade_bonus, before pick_best_plan:
if let Some(stored_goal) = &ctx.world.memory.last_goal {
    let weights = ctx.active.role.repair_weights(&tuning);
    for plan in plans.iter_mut() {
        let affinity = plan.annotation.repair_affinity;
        let bonus = affinity.aggregate(&weights);
        // Modulated by need_signals.continue_commitment (step 3.3 already provides this).
        let modulated = bonus * (1.0 + ctx.need_signals.continue_commitment);
        plan.score += modulated * tuning.thresholds.repair_bonus_scale;
    }
}
```

`tuning.thresholds.repair_bonus_scale` — единый scalar для калибровки (старт `0.4`, тюнится через golden diff per-entry).

**Замечания о semantics.**

- Bonus **additive**, не multiplicative. Это корректно: factor_sum измеряется в HP-equivalent, repair affinity — добавочная ценность сохранения замысла.
- Bonus **signed not** в общем случае: aggregate ≥ 0. Negative repair penalty (за abandonment) — не вводим: это было бы hard hammer в духе sanity-mask, который step 6 как раз и убирает.
- Bonus **умножается на confidence**: low-confidence stored goal даёт мелкий repair bonus (правильно — мы не уверены в нём с самого начала).

**Калибровка.**

Стартовое значение `repair_bonus_scale = 0.4`:

- Полный goal+region+method match → bonus ≈ `0.4 * (1 + 0.6) = 0.64` (assuming continue_commitment = 0.6 typical).
- Goal-only match → bonus ≈ `0.4 * 0.5 * 1.6 = 0.32`.

Это сопоставимо с typical factor_sum в районе 1.0–3.0 (~10–30% от score) — соразмерно terminal contribution.

Тюнится по результатам golden diff per-entry в 6.3.

**Gate.** Golden diff > 0 ОЖИДАЕТСЯ. Цель: ≤ 20 / 131 (~15%).

Per-entry breakdown:
- **Целевой**: на втором tick'е после Move, FocusTarget→same target побеждает FocusTarget→other target с +Δ ~0.3 (repair bonus).
- **Целевой**: на втором tick'е, Reposition (если best_pos сместился из-за свежего danger) уступает FocusTarget сохранённому (continuation).
- **Целевой**: panic_override при Cosmetic mismatch (rage tick) — теперь не сбрасывает план, fresh plan с repair bonus побеждает.
- **Подозрительный**: ProtectSelf вытесняется FocusTarget при HP ≤ 30% и Cosmetic mismatch — calibrate severity_factor для `actor_hp_drop` (Relevant ≥ 0.5? сейчас 0.7).
- 9 ai_scenarios остаются зелёными.

**Эстимейт:** 1.5 дня (калибровка через per-entry diff review).

---

### 6.4. Continuation evaluator: два набора весов

**Scope.**

Введение второго набора role-axis весов для случая «есть stored goal»:

```toml
[tables.axis_factor_weights]   # existing — discovery evaluator
tank    = [...]
melee   = [...]
# ...

[tables.axis_factor_weights_continuation]   # new — continuation evaluator
tank    = [...]   # tighter self-preserve floor, higher commitment-related axes
melee   = [...]
# ...

[tables.axis_terminal_weights_continuation]  # new — same idea on terminal
# ...
```

В `finalize_scores`:

```rust
let factor_weights = if ctx.world.memory.last_goal.is_some() {
    ctx.active.role.factor_weights_continuation(&tuning)
} else {
    ctx.active.role.factor_weights(&tuning)
};
let terminal_weights = if ctx.world.memory.last_goal.is_some() {
    ctx.active.role.terminal_weights_continuation(&tuning)
} else {
    ctx.active.role.terminal_weights(&tuning)
};
```

**Конкретные сдвиги в weights_continuation** (best-guess, calibration через golden):

- `next_turn_lethality` weight **снижается** на 30–50% (commitment value перевешивает: «если уже стою в опасной зоне после Move, хуже EndTurn вместо Cast»).
- `exposure_at_end` weight **тоже снижается**, но меньше (~20%).
- `secure_kill` weight **повышается** на 30% (committed kill ценнее spec'а).
- `board_control_gain` weight **повышается** (committed reposition реализуется).

**Self-preserve смягчается, но не выключается.** Sanity-mask `apply_protect_self_mask` (выживание < `survival_floor`) остаётся в силе — это контракт. Меняются только axis weights, не contract.

**Альтернатива (отвергнута).** Hardcode shift via constant в `finalize_scores`. Хуже — нельзя per-role тюнить через TOML.

**Юнит-тесты:**
- `continuation_eval_used_when_last_goal_present`.
- `discovery_eval_used_when_no_goal`.
- `continuation_doesnt_break_protect_self_mask` — sanity contract стоит.

**Gate.** Golden diff допустим. Per-entry:
- **Целевой**: Tank/Melee committed cast не уступает freshly-better protect_self при Cosmetic mismatch.
- **Подозрительный**: Support actor с stored HealAlly уступает protect_self при `next_turn_lethality > 0.7` — это правильно (rescue self over ally), не tune'ится.

Цель: ≤ 25 / 131 cumulative (6.3 + 6.4).

**Эстимейт:** 1.0 день (калибровка).

---

### 6.5. Log overhaul: structured continuation events + mining extension

**Scope.**

**`PlanDivergenceEntry` rewrite (additive, backward-compat).**

```rust
pub struct PlanDivergenceEntry {
    // existing: event_type, schema_version, timestamp_ms, actor_id, stored, fresh,
    //           used_continuation, replan_reason, intent_changed, ability_changed,
    //           target_changed, score_delta
    // NEW (6.5):
    pub continuation_severity: Option<ContinuationSeverity>,
    pub continuation_outcome: ContinuationOutcome,  // см. ниже
    pub repair_affinity: Option<RepairAffinity>,
    pub repair_bonus: Option<f32>,
    pub goal_kind: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationOutcome {
    /// Goal preserved, same ability/target as stored.
    GoalPreservedMethodPreserved,
    /// Goal preserved, but method (ability or specific target) differs.
    GoalPreservedMethodChanged,
    /// Goal abandoned (severity Invalidating, TTL expired, или fresh plan
    /// ушёл в другой goal_kind с big enough score margin).
    GoalAbandoned { reason: &'static str },
    /// No stored goal — first turn or after Cast/EndTurn.
    NoStoredGoal,
}
```

Producer `classify_continuation_outcome` в `repair/mod.rs`:

```rust
pub fn classify_continuation_outcome(
    stored: Option<&StoredGoalContext>,
    fresh_intent: TacticalIntent,
    fresh_step1: Option<&PlanStep>,
    severity: Option<ContinuationSeverity>,
) -> ContinuationOutcome {
    let Some(stored) = stored else { return ContinuationOutcome::NoStoredGoal };
    if let Some(ContinuationSeverity::Invalidating) = severity {
        return ContinuationOutcome::GoalAbandoned { reason: "invalidating_mismatch" };
    }
    let goal_match = goal_kind_matches_intent(&stored.kind, fresh_intent);
    if !goal_match {
        return ContinuationOutcome::GoalAbandoned { reason: "intent_diverged" };
    }
    let method_match = match (&stored.planned_ability, fresh_step1) {
        (Some(a), Some(PlanStep::Cast { ability, .. })) if a == ability => true,
        _ => false,
    };
    if method_match { ContinuationOutcome::GoalPreservedMethodPreserved }
    else { ContinuationOutcome::GoalPreservedMethodChanged }
}
```

**`mine_ai_logs.rs` extension.**

Новая агрегация:

```rust
struct ContinuationStats {
    total: usize,
    no_stored: usize,
    goal_preserved_method_preserved: usize,
    goal_preserved_method_changed: usize,
    goal_abandoned_by_reason: BTreeMap<String, usize>,
    severity_distribution: BTreeMap<String, usize>, // cosmetic/relevant/invalidating
}
```

Печать в репорте:

```
=== Continuation analysis (N divergence events) ===
goal_preserved | method_preserved : XX% (target: ≥60%)
goal_preserved | method_changed   : YY%
goal_abandoned | invalidating     : ZZ%
goal_abandoned | intent_diverged  : WW%
goal_abandoned | ttl_expired      : VV%
goal_abandoned | cosmetic_mismatch: 0% (target: 0%, прежде ненулевой)
severity: cosmetic XX% / relevant YY% / invalidating ZZ%
```

**Schema bump.** SCHEMA_VERSION v23 → v24 — на этом сабшаге, не на 6.6.

Backward compat:
- `continuation_severity: Option<...> + #[serde(default)]` — старые v23-логи дают `None`.
- `continuation_outcome` — `#[serde(default = "ContinuationOutcome::NoStoredGoal")]`.
- `repair_affinity`, `repair_bonus`, `goal_kind` — `Option`, default `None`.

**`replay_assertion.rs` integration.** Новые ассертеры:

```toml
# example overlay snippet:
continuation_outcome = "goal_preserved_method_preserved"
goal_kind = "finish"
```

Опциональные поля; existing scenarios без них — продолжают работать.

**Gate.** Schema migration test (старые logs читаются с `None`-полями), `cargo test/clippy`, `ai_scenarios` зелёный.

**Эстимейт:** 1.0 день.

---

### 6.6. Migration `continuation_from_stored` → repair-only path + rebaseline

**Scope.**

Это финальная сабшаг — снять костыль exact-continuation, оставить только repair-based path.

**Удаляется:**

- `enemy_turn.rs::continuation_from_stored` — функция полностью.
- Settings flag `ai_freeze_plan_after_move` — deprecated (флаг остаётся в `Settings` структуре до бакэнда live-config, но логика читать его удаляется).
- Ветка `if let Some(ref stored) = old_plan { ... continuation_from_stored ... }` — удаляется.
- 4 теста continuation_* в `enemy_turn.rs::tests` — заменяются на ai_scenarios (см. ниже).

**`AiMemory.last_plan: Option<StoredPlan>`.** Тоже удаляется. Остаётся только `last_goal: Option<StoredGoalContext>`.

**Что приходит на замену.**

```rust
// run_ai_turn (упрощённая логика после удаления):
let (decision, debug_snapshot, fresh_chosen) = pick_action(
    actor, ..., memory_ref, ...
);
// pick_action САМ применяет repair affinity bonus (через ScoringCtx → memory_ref.last_goal).
// Никакого дополнительного branching'а в run_ai_turn — fresh_chosen уже учитывает goal preservation.

// Store goal for next tick:
if let AiDecision::Move { .. } | AiDecision::MoveAndCast { .. } = decision {
    memory_ref.last_goal = extract_goal_context(&fresh_chosen, &snap, round, pool_max, &tuning);
}

// On Cast/EndTurn — clear:
if let AiDecision::CastInPlace { .. } | AiDecision::EndTurn = decision {
    memory_ref.last_goal = None;
}
```

`run_ai_turn` укорачивается с 100+ строк до 30–40.

**Логика `write_plan_divergence` тоже упрощается.** Fresh plan **всегда** тот, что выбрал `pick_best_plan`. Termin `used_continuation` устаревает (всегда `false` в новом мире — нет точного проигрывания stored), оставляется в schema на 1 версию для backward compat, removed в v25.

**Schema bump.** SCHEMA_VERSION v24 → v25:
- `StoredPlanSnapshot` — удаляется из JSONL, replaced `StoredGoalContextSnapshot`.
- `used_continuation` deprecated, всегда `false`, читается с `#[serde(default)]`.
- Новое поле `last_goal: Option<StoredGoalContextSnapshot>` в `AiLogEntry` (по аналогии со step 1.1).

**Replay support.** `replay_ai_log.rs` читает `last_goal` вместо `last_plan`. Pre-v25 path — `last_goal = None` fallback (актер начинает «с чистого листа»; для self-contained scenarios достаточно).

**Rebaseline.**

```bash
cargo run --bin replay_ai_log -- --capture-golden \
  logs/golden_post_step6.jsonl \
  logs/<corpus_v24>.jsonl
```

`logs/golden_post_step5.jsonl` → `logs/golden_post_step6.jsonl` (новый baseline). Корпус регенерируется на v25-логах из 3–4 свежих playtest'ов (как в step 5.6 / 2.0).

**Sync docs:**
- `docs/ai_rework.md` §6 — переписать под реальный API (`StoredGoalContext`, `RepairAffinity`, `ContinuationOutcome`).
- `docs/ai_rework_plan.md` — `6 ✓` в Wave-1 sequence; обновить gate-таблицу.
- `docs/ai.md` — добавить раздел «Goal-preserving repair».
- `docs/ai_need_signals.md` — приписать «P1 закрыт через repair affinity» к секции continue_commitment.

**Новые ai_scenarios cases (закоммитить вместе с 6.6).**

Из реальных playtest'ов на 3–4 встречах:

1. **`continuation_target_dies_replan`** — overlay ассертит, что после смерти стартовой цели актор переключается на следующую, не EndTurn'ит. (Сейчас уже работает, но проверить explicit.)
2. **`continuation_cosmetic_rage_tick_no_replan`** — overlay ассертит `continuation_outcome = goal_preserved_method_preserved` после Cosmetic rage tick.
3. **`continuation_actor_hp_drop_relevant`** — Relevant mismatch, but goal preserved (self-preserve fired only when need_signals demanded it, not from mismatch alone).
4. **`continuation_setup_aoe_two_ticks`** — multi-tick SetupAOE, planned_ability preserved через 2 хода.
5. **`continuation_ttl_expires`** — overlay на 3-й tick'е после store (ttl=2): `continuation_outcome = goal_abandoned, reason = ttl_expired`.

**Gate.**
- `cargo test/clippy/ai_scenarios` зелёные (5 новых scenarios + 9 старых).
- Golden rebaselined как новая baseline.
- `mine_ai_logs --continuation` отчёт показывает целевые проценты (см. 6.5):
  - `goal_preserved|method_preserved` ≥ 60%.
  - `goal_abandoned|cosmetic_mismatch` = 0%.

**Эстимейт:** 1.5 дня (rebaseline + 5 scenarios + docs sync + миграция замены `last_plan` → `last_goal`).

---

## Итого

| # | Шаг | Эстимейт | Gate | Статус |
|---|---|---|---|---|
| 6.0 | scaffolding (`PlanContinuationCheck` + classify_mismatch + telemetry) | 0.5 | golden 0/131, no behavior change | pending |
| 6.1 | goal extraction (`StoredGoalContext` + producer + AiMemory) | 1.0 | golden 0/131 | pending |
| 6.2 | repair affinity computation (read-only) | 1.5 | golden 0/131 | pending |
| 6.3 | consumer: repair affinity bonus в `finalize_scores` | 1.5 | per-entry golden review (≤20/131) | pending |
| 6.4 | continuation evaluator (два набора role-axis весов) | 1.0 | per-entry, cumulative ≤25/131 | pending |
| 6.5 | log overhaul + mining extension + schema v23→v24 | 1.0 | schema migration test, scenarios зелёные | pending |
| 6.6 | migration `continuation_from_stored` → repair-only + rebaseline + 5 new scenarios + schema v24→v25 | 1.5 | golden rebaseline + mining таргеты | pending |

**Суммарно ~8 дней.**

## Зафиксированные решения

1. **`StoredGoalContext` рядом со `StoredPlan`** (не вместо — до 6.6). Exact-continuation остаётся как ceiling до миграции.
2. **Repair affinity — additive bonus в `finalize_scores`**, не EvaluationMode. Sanity-mask и contract stay intact.
3. **`PlanContinuationCheck` сразу классифицирует все 7 mismatch-кодов** в Cosmetic/Relevant/Invalidating. Никаких itterative pass'ов.
4. **`GoalKind` минимальный enum в первой волне** (7 вариантов). `region` через единичный `Hex` + radius из `tuning`. Corridor/zone — backlog.
5. **TTL = 2 rounds default**, decay per turn. Expired → `goal_abandoned|ttl_expired`. Калибруется в 6.6 через mining.
6. **Confidence = chosen.score / pool_max_score** в момент store. Multiplicative gate на repair bonus.
7. **Schema bump единократный в 6.5 (v23→v24) и 6.6 (v24→v25)**. v23→v24 — adds-only (поля в plan_divergence + AiLogEntry). v24→v25 — `last_plan` → `last_goal` (breaking, но replay reads via `#[serde(default)]`).
8. **5 новых ai_scenarios — обязательная часть 6.6**, не отдельный backlog. Покрывают 5 классов: target death, cosmetic, relevant-with-preservation, setup-multi-tick, ttl-expiration.

## Критические файлы

- `src/combat/ai/repair/mod.rs` — новый модуль (`PlanContinuationCheck`, `ContinuationSeverity`, `classify_mismatch`, `classify_continuation_outcome`).
- `src/combat/ai/repair/goal.rs` — `GoalKind`, `StoredGoalContext`, `extract_goal_context`.
- `src/combat/ai/repair/affinity.rs` — `RepairAffinity`, `compute_repair_affinity`.
- `src/combat/ai/intent.rs` — `AiMemory.last_goal: Option<StoredGoalContext>` (6.1).
- `src/combat/ai/outcome.rs` — `PlanAnnotation.repair_affinity` (6.2).
- `src/combat/ai/tuning.rs` — `Thresholds` extension + `RepairWeights` + `Tables.axis_repair_weights`, `axis_factor_weights_continuation`, `axis_terminal_weights_continuation` (6.2, 6.4).
- `src/combat/ai/role.rs` — `repair_weights`, `factor_weights_continuation`, `terminal_weights_continuation` методы.
- `src/combat/ai/planning/scorer.rs` — `finalize_scores` reads continuation evaluator + applies repair bonus (6.3, 6.4).
- `src/combat/ai/enemy_turn.rs` — упрощается в 6.6 (удаление `continuation_from_stored`).
- `src/combat/ai/log.rs` — `PlanDivergenceEntry` extension v23→v24 (6.5); `AiLogEntry.last_goal` v24→v25 (6.6).
- `src/bin/mine_ai_logs.rs` — continuation analysis (6.5).
- `assets/data/ai_tuning.toml` — 3 thresholds + 3 tables.
- `tests/ai_scenarios/snapshots/continuation_*` — 5 новых scenarios (6.6).

## Ожидаемые сдвиги (gate-критерии)

После 6.6 mining (повторный прогон `mine_ai_logs --continuation` на 3–4 свежих v25-playtest'ах):

| Метрика | Baseline (post-step-3 mining) | Таргет post-step-6 |
|---------|-------------------------------|--------------------|
| `goal_preserved\|method_preserved` | (нет аналога) | ≥ 60% |
| `goal_preserved\|method_changed` | — | 10–25% |
| `goal_abandoned\|cosmetic_mismatch` | (включался в `actor_rage_changed`/`status` ~ 5–8% всех replan'ов) | **0%** |
| `goal_abandoned\|invalidating_mismatch` | (mishmash) | 5–10% (target dead и т.п.) |
| `goal_abandoned\|ttl_expired` | — | 2–5% |
| `goal_abandoned\|intent_diverged` | — | 5–15% |
| FocusTarget-switches с живой целью ≥50% HP | 7.3% (post-step-3) | ≤ 2.5% |
| Stable monotone_focus через 5+ ticks | (testing-only сейчас) | yes (новый scenario) |

Качественно — **меньше oscillation в commitment'е** на live-плейтестах: actors доводят goal до конца либо явно abandon'ят с структурированной reason'ой.

## Что откладывается

- **Telegraphing reuse (step 15)** — `PlanContinuationCheck` готов как fork-point, но сама фаза telegraphed-vs-executed решений делается в шаге 15.
- **Team-aware goals (step 13)** — `setup_marker` per-actor; collective `team_goal_alignment` появится после blackboard'а.
- **Goal-aware critics (step 10)** — `OvercommitIntoDanger` с TTL-aware penalty; `BuffIntoVoid` с goal-context awareness — после step 10 PlanCritic decomposition.
- **Corridor/zone goals** — `GoalKind::Corridor { path: SmallVec<Hex> }` появится с geometry awareness (step 17).
- **Confidence calibration через outcomes (step 4 follow-up)** — сейчас confidence из `score / pool_max`, может быть улучшено через `p_kill_now` + `p_kill_soon` aggregate. Backlog.

## Чего не делать в шаге 6

- **Не вводить EvaluationMode::Continuation** — additive bonus + per-evaluator weights этого хватает, mode-switching умножает интеракции с adaptation/protect_self.
- **Не удалять `mismatch()`** — он остаётся как low-level detail; `check_continuation` оборачивает его. Удаление `mismatch()` = breaking change для replay-fixtures.
- **Не делать сложную `region` геометрию** — единичный `Hex` + radius. Corridor — geometry awareness step 17.
- **Не пытаться выводить `goal_kind` retrospect'ивно** — extract'ится в момент store, не реверс-инжинирится из снапшота.
- **Не вводить goal-aware sanity penalties** — это уже близко к step 10 critics. В шаге 6 — только positive bonus, никаких penalty.
- **Не калибровать `repair_bonus_scale` в 6.3 на эвристиках** — только через golden per-entry diff review, как и `axis_terminal_weights` в step 5.4.
