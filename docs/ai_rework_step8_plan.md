# Шаг 8 — `StepFactor` / `PlanFactor` / `TerminalFactor` + `PlanModifier`

Декомпозиция на 3 крупных сабшага. Спецификация: `docs/ai_rework.md` §8.

## Preamble

### Текущее состояние (после step 7)

- `PlanFactors` (`src/combat/ai/factors/mod.rs:181`) — flat struct из 10 полей. Layout пинится константами `DAMAGE_IDX..SELF_SURVIVAL_IDX` и `SIGNED_FACTOR: [bool; 10]`.
- `compute_factors` (`factors/mod.rs:241`) — единая функция, вызывается из `compute_plan_factors_sans_intent` (`scorer.rs:549`) и из `intent.rs:950, 972`.
- `TerminalScore` (`planning/terminal.rs:46`) — отдельная 8-полевая struct, заполняется `terminal_state_score`. Aggregator inline в `finalize_scores:274–294`.
- Post-composition модификаторы — четыре inline-блока в `finalize_scores`: `plan_summon_bonus` (`:394`), `plan_trade_bonus` (`:443`), repair affinity bonus (`:306–319`), `plan_noise` (`:354`).
- Pipeline (после step 7): `Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate → RepairAffinity → PickBest`.
- Schema v28 (post step 7.5 clean break).

### Проблемы

1. **Уровни агрегации захардкожены.** Per-step / per-plan / per-terminal различаются только тем, в какой struct поле падает. Нет общей абстракции «фактор + aggregate policy».
2. **Имя фактора живёт в трёх местах** — `*_IDX` константы, struct field, TOML колонки. Drift возможен.
3. **Нормализация — батчевая магия в одном месте** (`finalize_scores:194–213`). Нет места «фактор сам говорит, как себя нормализует».
4. **Post-composition модификаторы рассыпаны** (4 разных kind'а: scarce-resource bonus / actor-economic / goal-affinity / tie-break noise). Нельзя выключить один, добавить новый, переставить порядок без правки `finalize_scores`. Логи показывают только сумму, не вклады.
5. **Terminal aggregator monolithic** — 8-axis формула с захардкоженной NeedSignals модуляцией. Step 17 (geometry axes) потребует править finalize_scores.
6. **`compute_factors` ↔ `intent_score` ↔ `compute_plan_intent_sum`** — двойной вход в `compute_offensive` для одного step'а.

### Что закрывает step 8

1. **Три enum'а через generic macro_rules!** `StepFactor` (7), `PlanFactor` (3), `TerminalFactor` (8). Один `factor_kind!` macro генерирует enum + match arms (name, compute, signed, normalize, need_modulation) + slice + count const.
2. **`PlanFactorValues` / `TerminalScore` → `[f32; N]`** typed wrapper с `get(StepFactor)/get_plan(PlanFactor)/get_terminal(TerminalFactor)`. Имя живёт только в макрос-вызове.
3. **`PlanModifier` trait + `PlanModifiersStage`.** `summon_bonus`, `trade_bonus`, `repair_bonus` — три унифицированных сигнатуры signed addendum. Apply repair_affinity переезжает сюда.
4. **`apply_pick_jitter` внутри PickBestStage.** `plan_noise` — pre-sort step внутри picking, не отдельный stage.
5. **Schema v28 → v29 clean break.** Custom serde пишет `factors`/`terminal` как named map через enum names. `raw_factors` удаляется, `factor_breakdown` не нужен (single source of truth — `factors`). Новое поле `modifiers: Vec<ModifierContribution>`. `pick.noise_applied: f32`.
6. **NeedSignals доступен в StepFactor::compute** — explicit параметр (closure carry-over из step 3, отложенное использование до step 11).

### Что НЕ в scope

- **Critics decomposition** (step 10).
- **Bands+agenda+scorecard** (step 11).
- **Mid-plan reflow** (step 12).
- **Geometry awareness axes** (step 17).
- **Использование NeedSignals в формулах факторов** (только параметр, без модуляции — это step 11).
- **OffensiveCache** — без него; если профайлинг покажет regression, отдельный follow-up.
- **TOML schema rewrite для factor weights** — `axis_factor_weights` остаётся `[[f32; 10]; 5]` массивом.

### Зафиксированные решения

| # | Решение | Альтернатива (отвергнутая) |
|---|---|---|
| 1 | enum + per-file modules + generic `macro_rules!` | `&[&'static dyn Trait]` (стрингли-typed special cases) |
| 2 | `PlanFactorValues = [f32; N]`, struct `PlanFactors` удалён | сохранить struct (drift между enum и fields) |
| 3 | `TerminalFactor` enum через тот же макрос (3rd инстанциация) | один TerminalFactor с aggregate_policy() |
| 4 | StepFactor / PlanFactor — два отдельных enum'а | один `Factor` с FactorCtx (runtime контракты) |
| 5 | `repair_bonus` как PlanModifier | оставить inline в finalize_scores (inconsistency) |
| 6 | noise как `apply_pick_jitter` внутри PickBestStage | отдельный NoiseStage (overhead абстракции) |
| 7 | (производное) Pipeline: `… RepairAffinity → PlanModifiers → PickBest (jitter+pick)` | — |
| 8 | NeedSignals → explicit param в StepFactor::compute (8.A) | отложить до step 11 (carry-over живёт долго) |
| 9 | Schema v29 clean break, single `factors` named map | сохранить `raw_factors` для backward compat |
| 10 | Без OffensiveCache — каждый StepFactor::compute self-contained | trait extension `compute_with_cache` (premature) |

---

## Сабшаг 8.A — Реестры факторов и сериализация (~3 дня)

**Scope.** Фундамент: macro_rules!, три enum'а, `[f32; N]` storage, custom serde, schema bump. Все fact computations переезжают в реестр; `compute_factors` исчезает.

### Изменения

**1. Новый модуль `src/combat/ai/factors/registry.rs`:**

```rust
//! Generic factor registry via `factor_kind!` macro.

#[derive(Clone, Copy, Debug)]
pub struct BatchStats { pub min: f32, pub max: f32 }

#[derive(Clone, Copy, Debug)]
pub enum NeedAxis { SelfPreserve, FinishTarget, RescueAlly, Reposition, SetupAOE, None }

impl NeedAxis {
    pub fn amplify(self, n: &NeedSignals) -> f32 {
        match self {
            Self::SelfPreserve => 1.0 + n.self_preserve,
            Self::FinishTarget => 1.0 + n.finish_target,
            Self::RescueAlly   => 1.0 + n.rescue_ally,
            Self::Reposition   => 1.0 + n.reposition,
            Self::SetupAOE     => 1.0 + n.setup_aoe,
            Self::None         => 1.0,
        }
    }
}

pub fn default_norm(raw: f32, batch: &BatchStats, signed: bool) -> f32 {
    let denom = if signed { batch.min.abs().max(batch.max.abs()) } else { batch.max };
    if denom > f32::EPSILON { raw / denom } else { 0.0 }
}

// macro_rules! factor_kind { ... }
```

`factor_kind!` macro генерирует:
- enum (`#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]`).
- `impl Enum { fn name(self) -> &'static str; fn signed(self) -> bool; fn compute(self, sig...) -> f32; fn normalize(self, raw, batch) -> f32; fn count() -> usize; fn iter() -> impl Iterator<Item = Self> }`.
- (для TerminalFactor) `fn need_modulation(self) -> NeedAxis`.
- `pub static <NAME>S: &[Enum]` slice.
- `from_name(s: &str) -> Option<Enum>` для десериализации.

**2. Три инстанциации в `factors/{step,plan,terminal}/mod.rs`:**

```rust
// factors/step/mod.rs
factor_kind! {
    name: StepFactor,
    sig: (ctx: &ScoringCtx, step: &ScoredStep, outcome: &ActionOutcomeEstimate, needs: &NeedSignals),
    variants: {
        Damage       => damage,
        KillNow      => kill_now,
        KillPromised => kill_promised,
        Cc           => cc,
        Heal         => heal,
        Scarcity     => scarcity,
        Saturation   => saturation,
    }
}

// factors/plan/mod.rs
factor_kind! {
    name: PlanFactor,
    sig: (plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx),
    variants: {
        Intent       => intent       (signed: true),
        TempoGain    => tempo_gain,
        SelfSurvival => self_survival,
    }
}

// factors/terminal/mod.rs
factor_kind! {
    name: TerminalFactor,
    sig: (plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx),
    variants: {
        ExposureAtEnd       => exposure_at_end       (need: SelfPreserve),
        NextTurnLethality   => next_turn_lethality   (need: SelfPreserve),
        SecureKill          => secure_kill           (need: FinishTarget),
        AllyRescue          => ally_rescue           (need: RescueAlly),
        BoardControlGain    => board_control_gain    (need: Reposition),
        LineActionability   => line_actionability    (need: None),
        DensityValue        => density_value         (need: SetupAOE),
        PressureSpacingZone => pressure_spacing_zone (need: None),
    }
}
```

**3. Per-factor файлы.** Каждый — `pub fn compute(...) -> f32; pub const NAME: &str = "..."; pub const SIGNED: bool = false;`. Логика мигрирует:
- StepFactor: из `factors::offensive::compute_offensive` (extracted per column), `factors::scarcity::compute_scarcity`, `factors::saturation::buff_saturation_penalty`.
- PlanFactor: `intent` → `compute_plan_intent_sum` (`scorer.rs:632`); `tempo_gain` → `factors::compute_plan_tempo_gain` (`tempo.rs:24`); `self_survival` → `compute_plan_self_survival` (`survival.rs:50`).
- TerminalFactor: 8 free functions из `planning/terminal.rs:93–319`.

**4. `PlanFactorValues` и `TerminalScore` → typed wrappers:**

```rust
pub struct PlanFactorValues([f32; StepFactor::count() + PlanFactor::count()]);
impl PlanFactorValues {
    pub fn get(&self, f: StepFactor) -> f32 { self.0[f as usize] }
    pub fn get_plan(&self, f: PlanFactor) -> f32 { self.0[StepFactor::count() + f as usize] }
    pub fn set(&mut self, f: StepFactor, v: f32) { self.0[f as usize] = v }
    pub fn set_plan(&mut self, f: PlanFactor, v: f32) { self.0[StepFactor::count() + f as usize] = v }
}

pub struct TerminalScore([f32; TerminalFactor::count()]);
// аналогично
```

**5. Custom Serialize/Deserialize** для обоих — пишут как named map `{"damage": 1.2, ...}`. На десериализации `StepFactor::from_name(key)` → enum index → array slot.

**6. Миграция `finalize_scores` (terminal aggregator).** Inline 8-line блок (`scorer.rs:274–294`) → `for f in TerminalFactor::iter() { sum += score.get_terminal(f) * tw[f as usize] * f.need_modulation().amplify(needs); }`.

**7. Миграция `compute_plan_factors_sans_intent` step-loop.** Внутренний цикл по Cast steps теперь:
```rust
for f in StepFactor::iter() {
    sums[f as usize] += f.compute(&step_ctx, &scored_step, &step_outcome, ctx.need_signals) * step_weight;
}
```

**8. Удаление `compute_factors` + миграция `intent.rs:950, 972`.** Узкий API:
```rust
fn intent_offensive_value_on_target(focus: Entity, ctx, step, outcome, needs) -> f32 {
    if step.target() != Some(focus) { return 0.0; }
    let weights = IntentWeights::default().kill_now(2.0).damage(1.0).cc(0.5);
    weights.damage * StepFactor::Damage.compute(ctx, step, outcome, needs)
        + weights.kill_now * StepFactor::KillNow.compute(...)
        + weights.cc * StepFactor::Cc.compute(...)
        + weights.kill_promised * StepFactor::KillPromised.compute(...)
}
```

**9. Schema v28 → v29 clean break.** `SCHEMA_VERSION = 29`. `PlanAnnotation`:
- `raw_factors: PlanFactors` → `factors: PlanFactorValues` (custom serde).
- `terminal: TerminalScore` (custom serde).
- v28 logs дают `LogError::UnsupportedSchema`.

### Удаляется

- `factors::compute_factors` (`factors/mod.rs:241`).
- `PlanFactors` struct + `as_array`/`from_array` (`factors/mod.rs:181–217`).
- `*_IDX` константы (`factors/mod.rs:152–161`), `SIGNED_FACTOR` массив (`:167`), `NUM_FACTORS` const.
- `OffensiveFactors` как column-aggregate (остаётся как internal helper для `compute_offensive`, used by некоторыми StepFactor::compute через прямой вызов).
- `IntentWeights::dot(&PlanFactors)` (`intent.rs:836`) — если callers мигрировали.

### Тесты

- `factor_kind_macro_generates_correct_enum_metadata` — pin name/signed/count для трёх enum'ов.
- `step_factor_compute_pure_for_known_outcome` × 7 inline tests.
- `plan_factor_compute_matches_legacy` × 3 (intent/tempo/self_survival pin).
- `terminal_factor_compute_matches_legacy` × 8.
- `terminal_aggregator_via_registry_matches_legacy_formula` — fixture pin.
- `compute_plan_factors_via_step_registry_matches_legacy` — fixture pin.
- `intent_score_via_narrow_offensive_api_matches_legacy` (FocusTarget + ApplyCC).
- `factor_values_serde_round_trip_named_map`.
- `terminal_score_serde_round_trip_named_map`.
- `actor_tick_v29_round_trip`.
- `actor_tick_v28_load_yields_unsupported_schema_error`.

### Gate

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (после rebuild на v29 формате; per-entry FP-edge ≤5/N допустим).
- v28 logs не читаются (clean error).
- Mining baseline (post-step-7) воспроизводится bit-for-bit на свежем v29 corpus.

---

## Сабшаг 8.B — `PlanModifier` + `PlanModifiersStage` (~2 дня)

**Scope.** Унификация трёх additive bonus'ов (summon, trade, repair) через PlanModifier trait. Repair affinity apply переезжает в новую стадию pipeline. `finalize_scores` теряет три bonus apply блока.

### Изменения

**1. Новый модуль `src/combat/ai/modifiers/`:**

```rust
// modifiers/mod.rs
pub trait PlanModifier: Sync {
    fn name(&self) -> &'static str;
    fn modify(&self, plan: &TurnPlan, ann: &PlanAnnotation, ctx: &ModifierCtx) -> f32;
}

pub struct ModifierCtx<'a> {
    pub stage: &'a StageCtx<'a>,
    pub summon_dpr: &'a HashMap<String, f32>,
    pub actor_value: f32,
    pub last_goal: Option<&'a StoredGoalContext>,
    pub repair_weights: [f32; 6],  // role-mixed
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModifierContribution { pub name: String, pub contribution: f32 }

pub static PLAN_MODIFIERS: &[&dyn PlanModifier] = &[
    &summon_bonus::MODIFIER,
    &trade_bonus::MODIFIER,
    &repair_bonus::MODIFIER,
];
```

**2. Три файла модификаторов:**

- `modifiers/summon_bonus.rs` — мигрирует `plan_summon_bonus` (`scorer.rs:394`). Cache `summon_dpr` берёт из `ModifierCtx`.
- `modifiers/trade_bonus.rs` — мигрирует `plan_trade_bonus` (`scorer.rs:443`). Использует `ctx.actor_value`. Формула не меняется.
- `modifiers/repair_bonus.rs` — мигрирует apply из `finalize_scores:306–319`:
  ```rust
  pub struct RepairBonus;
  impl PlanModifier for RepairBonus {
      fn modify(&self, plan, ann, ctx) -> f32 {
          if ctx.last_goal.is_none() { return 0.0; }
          let bonus_scale = ctx.stage.scoring.world.tuning.thresholds.repair_bonus_scale;
          let affinity = ann.repair_affinity.aggregate(&ctx.repair_weights);
          affinity * (1.0 + ctx.stage.scoring.need_signals.continue_commitment) * bonus_scale
      }
  }
  ```

**3. Новая стадия `pipeline/stages/plan_modifiers.rs`:**

```rust
pub struct PlanModifiersStage;
impl PlanStage for PlanModifiersStage {
    fn name(&self) -> &'static str { "plan_modifiers" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        let summon_dpr = build_summon_dpr_cache(&pool.plans, ctx.scoring.world);
        let actor_value = unit_value(ctx.scoring.active, ctx.scoring.world.content);
        let last_goal = ctx.scoring.last_goal;
        let repair_weights = ctx.scoring.active.role.repair_weights(ctx.scoring.world.tuning);
        let mctx = ModifierCtx { stage: ctx, summon_dpr: &summon_dpr, actor_value, last_goal, repair_weights };
        for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
            if !ann.score.is_finite() { continue; }
            for m in PLAN_MODIFIERS {
                let c = m.modify(plan, ann, &mctx);
                ann.modifiers.push(ModifierContribution { name: m.name().into(), contribution: c });
                ann.score += c;
            }
        }
    }
}
```

**4. Pipeline order updated** в `pipeline/mod.rs::run_pool_pipeline`:

```
Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate
→ RepairAffinity (annotate) → PlanModifiers (apply summon+trade+repair) → PickBest
```

**5. `PlanAnnotation` extension:** новое поле `modifiers: Vec<ModifierContribution>` (для observability).

### Удаляется

- `plan_summon_bonus` (`scorer.rs:394`) — переезжает в `modifiers/summon_bonus.rs`.
- `plan_trade_bonus` (`scorer.rs:443`) — переезжает в `modifiers/trade_bonus.rs`.
- `build_summon_dpr_cache` (`scorer.rs:458`) — переезжает в PlanModifiersStage::apply.
- Inline `score += summon` / `score += trade` (`scorer.rs:246, 252`).
- Repair affinity apply block (`scorer.rs:306–319`) полностью.

### Тесты

- `summon_bonus_matches_legacy_formula` — pin один plan.
- `summon_bonus_zero_for_no_summon_plan`.
- `trade_bonus_matches_legacy_formula` — pin один plan.
- `trade_bonus_zero_for_neutral_plan` (мигрирует из `scorer.rs:2092`).
- `repair_bonus_zero_when_no_stored_goal`.
- `repair_bonus_matches_legacy_formula` — pin (мигрирует из `scorer.rs:2375, 2447`).
- `plan_modifiers_stage_skips_masked_plans` (-inf score → no modify).
- `plan_modifiers_stage_writes_contributions_per_modifier` — annotation.modifiers populated.
- `plan_modifiers_stage_total_matches_sum_of_contributions`.
- `pipeline_runs_modifiers_after_repair_before_pick`.

### Gate

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N (формулы не меняются, порядок: было repair→summon→trade в одном loop, стало то же в registry).
- Per-entry FP-edge review ≤3/N допустим.

---

## Сабшаг 8.C — Picking jitter + cleanup (~1.5 дня)

**Scope.** `plan_noise` переезжает в PickBestStage как pre-sort step. `finalize_scores` cleanup. Dead code sweep. 15 callers refactor. Tools update. Docs.

### Изменения

**1. `apply_pick_jitter` в `pipeline/stages/pick_best.rs`:**

```rust
fn apply_pick_jitter(pool: &mut ScoredPool, ctx: &StageCtx) {
    let noise_amp = ctx.scoring.world.difficulty.score_noise();
    if noise_amp <= 0.0 { return; }
    let (s_min, s_max) = pool.annotations.iter()
        .map(|a| a.score).filter(|s| s.is_finite())
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| (lo.min(s), hi.max(s)));
    if !s_min.is_finite() || !s_max.is_finite() { return; }
    let spread = (s_max - s_min).max(0.05);
    let eff_amp = noise_amp * spread;
    let active = ctx.scoring.active.entity;
    let round = ctx.scoring.snap.round;
    for (plan, ann) in pool.plans.iter().zip(pool.annotations.iter_mut()) {
        if !ann.score.is_finite() { continue; }
        let n = plan_noise_internal(plan, round, active, eff_amp);
        ann.score += n;
        if let Some(pi) = ann.pick.as_mut() { pi.noise_applied = n; }
        // Если pi.is_none() — создаётся в argmax pass с noise_applied записанным.
    }
}

impl PlanStage for PickBestStage {
    fn name(&self) -> &'static str { "pick_best" }
    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        apply_pick_jitter(pool, ctx);
        // existing argmax + tie-break + write annotation.chosen / annotation.pick
    }
}
```

**2. `PickInfo` extension:** новое поле `noise_applied: f32` (default 0.0 если jitter не сработал).

**3. `finalize_scores` cleanup.** После 8.B + 8.C `finalize_scores` теряет:
- summon/trade bonus inline (8.B).
- repair affinity apply block (8.B).
- noise pass (8.C).
- terminal aggregator inline (8.A → registry walk).

После 8.C `finalize_scores` — это:
1. Build per-factor batch stats (min/max).
2. Per-plan: `for f in StepFactor::iter() { sum += score.get(f) * weights[f as usize]; }` + plan factors + terminal walk.
3. Возвращает `Vec<f32>`.

Размер: ~80 строк против текущих ~200.

**4. 15 callers рефактор.** `.intent` / `.damage` / etc → `.get(StepFactor::Damage)` / `.get_plan(PlanFactor::Intent)`. Затронутые файлы (по `agrep PlanFactors`):
- `debug.rs:548` (`raw_factors: &[PlanFactors]` → `&[PlanFactorValues]`).
- `pipeline/stages/{viability,killable_gate,protect_self,adaptation}.rs`.
- `planning/{killable_gate,picker,adaptation}.rs`.
- `intent.rs` (где остаются legacy чтения).

**5. Dead code sweep.** Удаляется:
- `factors::compute_plan_self_survival` re-export (`factors/mod.rs:34`) — только PlanFactor impl остаётся.
- `factors::compute_plan_tempo_gain` re-export — то же.
- `aoe_area`, `buff_saturation_penalty`, `aoe_hits` re-exports — оставляются (используются вне scorer'а).
- Header docs в `factors/mod.rs`, `planning/scorer.rs`, `planning/terminal.rs`, `combat/ai/mod.rs` — обновляются.

**6. Tools rewrite (v29):**
- `bin/replay_ai_log.rs` — читает actor_tick events v29; парсит `factors`/`terminal` named map.
- `bin/mine_ai_logs.rs` — читает v29; новые секции:
  - `=== Modifier contributions ===` — распределение `summon_bonus` / `trade_bonus` / `repair_bonus` per-actor / per-plan.
  - `=== Picking jitter ===` — distribution of `pick.noise_applied` (mean / max / sign of effect).

**7. Архитектурная страница** в `factors/mod.rs` heredoc:

```text
Factors hierarchy:
  StepFactor   — one value per Cast step. Discounted sum into PlanFactorValues.
                 Lives in factors/step/*.rs. Registry: STEP_FACTORS.
  PlanFactor   — one value per plan. Lives in factors/plan/*.rs. Registry: PLAN_FACTORS.
  TerminalFactor — one value per plan from final sim snapshot. Lives in factors/terminal/*.rs.
                   Registry: TERMINAL_FACTORS.
  PlanModifier — signed addendum applied after composition. Lives in modifiers/*.rs.
                 Registry: PLAN_MODIFIERS.

Pipeline:
  Viability → Sanity → Adaptation → ProtectSelfMask → KillableGate
  → RepairAffinity → PlanModifiers → PickBest (jitter+pick)
```

### Удаляется

- `plan_noise` (`scorer.rs:354`) — переезжает как `plan_noise_internal` в `pipeline/stages/pick_best.rs`.
- `plan_start_tile` helper (`scorer.rs:377`) — туда же (только noise его использует).
- Inline noise block (`scorer.rs:321–345`) полностью.

### Тесты

- `pick_jitter_no_op_when_noise_amp_zero`.
- `pick_jitter_skips_masked_plans` (-inf untouched).
- `pick_jitter_records_noise_applied_in_pick_info` — annotation.pick.noise_applied populated.
- `pick_jitter_is_plan_order_invariant` — мигрирует из `scorer.rs:1160`.
- `pipeline_pick_runs_jitter_before_argmax`.
- `finalize_scores_no_longer_writes_modifiers_or_noise` — pin output идентичен legacy без jitter/modifiers post-8.B+8.C.
- `replay_v29_round_trip_zero_diff`.
- `mine_v29_corpus_produces_modifier_section`.

### Gate

- `cargo test/clippy --all-targets/build/ai_scenarios` зелёные.
- Golden 0/N.
- v29 corpus mining воспроизводит post-step-7 behavioral baseline.

---

## Итого

| # | Сабшаг | Эстимейт | Gate |
|---|---|---|---|
| 8.A | Реестры факторов (3 enum'а через макрос) + custom serde + schema v29 + миграция compute_factors | 3.0 | golden 0/N (≤5 FP-edge), v29 round-trip, mining bit-for-bit |
| 8.B | PlanModifier trait + summon/trade/repair миграция + PlanModifiersStage | 2.0 | golden 0/N (≤3 FP-edge), pipeline order pinned |
| 8.C | PickBestStage с apply_pick_jitter + finalize_scores cleanup + 15 callers + tools v29 | 1.5 | golden 0/N, mine_v29 produces new sections |

**Суммарно ~6.5 дней.**

## Критические файлы

**Новые:**
- `src/combat/ai/factors/registry.rs` — `factor_kind!` macro + `BatchStats` + `NeedAxis`.
- `src/combat/ai/factors/{step,plan,terminal}/mod.rs` — три инстанциации.
- `src/combat/ai/factors/{step,plan,terminal}/<factor>.rs` — 18 per-factor файлов.
- `src/combat/ai/modifiers/{mod,summon_bonus,trade_bonus,repair_bonus}.rs`.
- `src/combat/ai/pipeline/stages/plan_modifiers.rs`.

**Меняются:**
- `src/combat/ai/factors/mod.rs` — pub mod registry; удаляются `compute_factors`, `*_IDX`, `SIGNED_FACTOR`, `PlanFactors` struct.
- `src/combat/ai/planning/scorer.rs` — `finalize_scores` ~200 → ~80 строк; удалены summon/trade/repair/noise блоки; terminal aggregator → registry walk; step-loop → STEP_FACTORS walk.
- `src/combat/ai/planning/terminal.rs` — 8 free functions переезжают; `terminal_state_score` body → registry walk.
- `src/combat/ai/intent.rs` — Cast ветви FocusTarget/ApplyCC → узкий offensive API.
- `src/combat/ai/outcome.rs` — `PlanAnnotation` теряет `raw_factors`, получает `factors: PlanFactorValues`, `modifiers: Vec<ModifierContribution>`; `pick.noise_applied`.
- `src/combat/ai/pipeline/{mod,stages/pick_best}.rs` — pipeline order; jitter в PickBestStage.
- `src/combat/ai/log.rs` — `SCHEMA_VERSION = 29`; clean break.
- `src/bin/{mine_ai_logs,replay_ai_log}.rs` — v29 only.
- `src/combat/ai/debug.rs:548` — `&[PlanFactorValues]`.

## Ожидаемые сдвиги поведения

После 8.C — **никаких behavioral сдвигов** относительно post-step-7 baseline. Step 8 — pure refactor. Mining-метрики post-step-7 v28 corpus должны воспроизвести picture bit-for-bit на свежем v29 corpus.

**Возможные FP-сдвиги:** registry walk loops vs hand-coded sum могут дать ±epsilon на отдельных entries из-за порядка summation. Допустим:
- 8.A: ≤5/N (самый inner-loop).
- 8.B / 8.C: ≤3/N.
- >5 — расследовать.

**Новые сигналы (observability, не behavior):**
- `annotation.modifiers` — per-modifier contribution (раньше summon/trade/repair утопали в финальном score).
- `annotation.pick.noise_applied` — jitter contribution.
- `factors` / `terminal` в логах как named map (раньше — typed struct shape).

## Что откладывается / Чего не делать

- **Не использовать NeedSignals в формулах факторов** — параметр добавляется в сигнатуру, но никто его пока не читает. Использование — step 11 (bands+agenda+scorecard).
- **Не оптимизировать через OffensiveCache** — каждый StepFactor self-contained. Если профайлинг покажет regression — отдельный follow-up step с trait extension.
- **Не разделять PlanFactor / StepFactor на subkinds** (per-step / per-plan / per-actor / per-team) — оставляем два enum'а как есть.
- **Не менять формулы** — pure refactor. Calibration / formula evolution — отдельные шаги.
- **Не переводить `axis_factor_weights` в map TOML** — массив остаётся, schema rewrite — backlog.
- **Не делать factors / modifiers configurable per-encounter** — registry compile-time. Encounter-specific overrides — encounter scripting (step 14).
- **Не разделять PlanModifier на «soft» и «hard»** — все signed additions. Critics (step 10) дают penalty / mask.

## Открытые вопросы реализации

1. **Macro и custom serde.** `factor_kind!` генерирует `from_name` метод (для десериализации), но Serialize/Deserialize impl для `PlanFactorValues` / `TerminalScore` пишутся **отдельно** (manual impl), потому что они не enum, а array wrappers. Macro и serde — две разные responsibility.

2. **`signed()` и `need_modulation()` в макрос-синтаксисе.** Атрибуты варианта в parens: `Intent => intent (signed: true)`, `ExposureAtEnd => exposure_at_end (need: SelfPreserve)`. Default: `signed = false`, `need_modulation = None`. Macro распарсивает optional attributes — потребует tt-muncher pattern в macro_rules!.

3. **Тестирование макроса.** `cargo expand` для проверки сгенерированного кода (опционально, не CI). Юнит-тесты пиньтут `count()`, `iter()`, `from_name()` round-trip — этого достаточно.

4. **`PlanFactor` интент-параметр.** `SelfSurvival::compute(plan, intent, ctx)` не использует `intent`. Trait сигнатура унифицирована — параметр игнорируется, документировано в const. Тест `self_survival_ignores_intent_parameter` пин.

5. **Migration order в 8.A.** Реализация macro + регистр + storage сначала; миграция `finalize_scores` aggregator после; миграция `intent.rs` callers последней (закрытие `compute_factors`). Внутри 8.A это три коммита, gate на каждом.
