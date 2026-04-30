# Implementer Plan — Сабшаг 8.A (3 коммита)

**Source of truth:** `/Users/splav/personal/storyforge/docs/ai_rework_step8_plan.md` (Subshag 8.A section). This plan is the implementer-friendly decomposition; do not duplicate spec content — refer back.

**Working tree:** `/Users/splav/personal/storyforge` (clean post step-7 baseline).

**Final-state file inventory after 8.A** (terminal scoring stays inline in `finalize_scores`; modifiers/jitter are 8.B/8.C):
- New: `src/combat/ai/factors/registry.rs`, `src/combat/ai/factors/{step,plan,terminal}/mod.rs`, plus 18 per-factor leaf files.
- Heavily edited: `factors/mod.rs`, `planning/scorer.rs`, `planning/terminal.rs`, `intent.rs`, `outcome/mod.rs`, `log.rs`, `debug.rs`, `pipeline/stages/{viability,protect_self,killable_gate,adaptation,pick_best}.rs`, `planning/{picker,sanity,adaptation}.rs`, `role.rs` (test only), `bin/replay_ai_log.rs`, `bin/mine_ai_logs.rs`, `tuning.rs:231–296` (TOML `axis_factor_weights` rows re-permuted под новый column order, см. Q1).
- Untouched in 8.A: `factors/{adjustments,aoe_hits}.rs`. Структура TOML (`[[f32; 10]; 5]`) сохраняется — меняется только порядок колонок внутри строк.

---

## Зафиксированные решения и открытые вопросы

(Решения, принятые до старта; реализация следует им без дополнительных подтверждений.)

1. **`PlanFactorValues` array layout — РЕШЕНО: option A.** `PlanFactorValues = [f32; StepFactor::count() + PlanFactor::count()]`, layout `[step_0..step_6, plan_0..plan_2]` = 7 + 3 = 10. Новый column order: `[damage, kill_now, kill_promised, cc, heal, scarcity, saturation, intent, tempo_gain, self_survival]`. **TOML rows в `tuning.rs:231–296` перепермутируются под этот column order** (поведенчески эквивалентно, визуально меняет TOML). К `default()` impl добавить комментарий: `// columns: [damage, kill_now, kill_promised, cc, heal, scarcity, saturation, intent, tempo_gain, self_survival] — было [..., intent, scarcity, tempo_gain, saturation, self_survival] в v28`. Альтернатива (B — interleave variants) отвергнута: ломает contiguous array layout.

2. **Where does `compute_plan_self_survival` get its `intent` parameter?** Spec sig is `(plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx)` but the implementation in `src/combat/ai/factors/survival.rs:50` takes `(plan, ctx)` — no intent. PlanFactor::SelfSurvival must accept `intent` to keep the macro-generated trampoline uniform; the per-factor body will simply ignore it. Pin via `self_survival_ignores_intent_parameter` test (spec §"Открытые вопросы 4").

3. **Saturation factor signature.** `buff_saturation_penalty(ability, target, caster, pre_snap, content)` operates per-step but the existing scorer accumulates `saturation_sum` only when the step is a `Cast` (scorer.rs:559–564) using the **pre-step** snapshot. StepFactor::Saturation::compute receives `(ctx, step, outcome, needs)`; ctx.snap is the pre-step snapshot only via `step_ctx = ctx.with_perspective(&sim_actor, pre_snap)` at scorer.rs:547. This must continue to flow through unchanged — i.e., the StepFactor walk in commit 2 must run inside the `with_perspective` block, not on the outer `ctx`. Confirmed by re-reading scorer.rs:537–568 — keep that perspective shift in place.

4. **`TerminalFactor::compute` signature.** Spec gives `(plan, snap, ctx)`. Existing helpers take `(plan, ctx)` (e.g. `compute_exposure_at_end`) or `(plan, initial_snap, ctx)` (e.g. `compute_next_turn_lethality`). Macro must thread `snap` even when the body ignores it. Keep `_initial_snap` parameter as `_snap` in bodies that don't need it; document.

---

## Commit 1 — Registry, three enums, typed wrappers, custom serde

**Цель.** Vocabulary lands. Three enum types and `[f32; N]` typed wrappers compile and round-trip. `PlanFactors` struct still exists (used by callers); `compute_factors` still exists. **No call sites change yet** — pure additive plumbing. `SCHEMA_VERSION` остаётся 28 (bump переезжает в коммит 2 вместе с реальным изменением wire-формата). Compile-only goal: каждый per-factor `compute()` возвращает корректное значение для fixture pin.

**Файлы (создать).**

- `src/combat/ai/factors/registry.rs` — new module, exports:
  - `pub struct BatchStats { pub min: f32, pub max: f32 }`
  - `pub enum NeedAxis { SelfPreserve, FinishTarget, RescueAlly, Reposition, SetupAOE, None }` + `impl NeedAxis::amplify(self, &NeedSignals) -> f32`
  - `pub fn default_norm(raw: f32, batch: &BatchStats, signed: bool) -> f32`
  - `macro_rules! factor_kind { ... }` — sketched below
- `src/combat/ai/factors/step/mod.rs` — `factor_kind!` instantiation for `StepFactor`. Variant order forces array layout: `Damage, KillNow, KillPromised, Cc, Heal, Scarcity, Saturation` (7 variants, matches legacy positional order minus the three plan-level slots).
- `src/combat/ai/factors/step/{damage,kill_now,kill_promised,cc,heal,scarcity,saturation}.rs` — 7 leaves, each: `pub const NAME: &str = "damage"; pub const SIGNED: bool = false;` (or `true` for `saturation`; spec §"variants" mark `Intent` as signed but `saturation` was historically signed via `SIGNED_FACTOR[8] = true` in `factors/mod.rs:167–169`). Each leaf hosts `pub fn compute(ctx: &ScoringCtx, step: &ScoredStep, outcome: &ActionOutcomeEstimate, needs: &NeedSignals) -> f32`.
- `src/combat/ai/factors/plan/mod.rs` — `factor_kind!` instantiation for `PlanFactor`. Variant order: `Intent, TempoGain, SelfSurvival` (3 variants, must mirror legacy `INTENT_IDX, TEMPO_IDX, SELF_SURVIVAL_IDX` positions in the trailing slots of legacy layout to preserve TOML row alignment under "alternative b").
- `src/combat/ai/factors/plan/{intent,tempo_gain,self_survival}.rs` — 3 leaves with sigs `pub fn compute(plan: &TurnPlan, intent: &TacticalIntent, ctx: &ScoringCtx) -> f32`.
- `src/combat/ai/factors/terminal/mod.rs` — `factor_kind!` instantiation for `TerminalFactor`. 8 variants in legacy order from `terminal.rs:46–55`: `ExposureAtEnd, NextTurnLethality, SecureKill, AllyRescue, BoardControlGain, LineActionability, DensityValue, PressureSpacingZone`.
- `src/combat/ai/factors/terminal/{exposure_at_end,next_turn_lethality,secure_kill,ally_rescue,board_control_gain,line_actionability,density_value,pressure_spacing_zone}.rs` — 8 leaves with sigs `pub fn compute(plan: &TurnPlan, snap: &BattleSnapshot, ctx: &ScoringCtx) -> f32`.

**Файлы (изменить).**

- `src/combat/ai/factors/mod.rs:22–35` — add `pub mod registry; pub mod step; pub mod plan; pub mod terminal;`. Re-export `StepFactor`, `PlanFactor`, `TerminalFactor`, `PlanFactorValues`, `TerminalScore` (new typed wrapper, replaces the struct in `terminal.rs`). **Keep** legacy `PlanFactors` struct, `compute_factors`, `*_IDX`, `SIGNED_FACTOR`, `NUM_FACTORS` for now — they're removed in commit 3.
- `src/combat/ai/factors/mod.rs` (new code): add `pub struct PlanFactorValues([f32; 10]);` with `get(StepFactor)`, `get_plan(PlanFactor)`, `set(StepFactor, f32)`, `set_plan(PlanFactor, f32)`, `Default`, `Clone`, `Copy`, `Debug`, `PartialEq`. Manual `Serialize`/`Deserialize` writes/reads named map `{"damage":…,"kill_now":…,…,"self_survival":…}` via `StepFactor::from_name` + `PlanFactor::from_name`.
- `src/combat/ai/planning/terminal.rs:36–55` — оставить legacy `TerminalScore` struct in place. Новый typed wrapper определяется по другому пути: `pub struct TerminalScore([f32; 8])` в `factors/terminal/mod.rs` (с get/set/Default/Clone и custom serde named-map). **Имена не конфликтуют — пути разные:** `planning::terminal::TerminalScore` (legacy) и `factors::terminal::TerminalScore` (новый) сосуществуют в коммите 1. Swap `PlanAnnotation.terminal` на новый wrapper и удаление legacy struct происходит в коммите 2 (вместе с wire-format break `raw_factors`→`factors`). **`SCHEMA_VERSION` не трогается в коммите 1** — bump переносится в коммит 2.
- `src/combat/ai/log.rs` — **не трогается в коммите 1**. `SCHEMA_VERSION = 29` переносится в коммит 2 (одновременно с wire-format break). Причина: schema version семантически отражает формат записи; в коммите 1 формат ещё v28.

**Подшаги (порядок исполнения).**

1. **Define `BatchStats`, `NeedAxis`, `default_norm`** in `registry.rs`. Trivial.
2. **Sketch `factor_kind!` macro** in `registry.rs`. Skeleton (illustration only — implementer fills bodies):

   ```rust
   macro_rules! factor_kind {
       (
           name: $Enum:ident,
           sig: ( $($pname:ident : $pty:ty),* $(,)? ),
           variants: { $( $Variant:ident => $module:ident $( ( $($attr:tt)+ ) )? ),+ $(,)? }
       ) => { /* enum, impl, slice, count, iter, from_name */ };
   }
   ```

   The `$($attr:tt)+` is the optional-attribute slot. tt-muncher pattern: parse `(signed: true)` and `(need: Foo)` by recursive macro arms. Default to `signed = false` and `need_modulation = NeedAxis::None`. **Recommended pattern** — split into a top-level entry arm + an inner per-variant emitter that takes the attrs as `tt*` and pattern-matches `$($k:ident : $v:ident),*` inside a paren. Test with `cargo expand` locally before hand-rolling more variants.

3. **Generate enum + match arms** for `StepFactor` first (smallest, no `need_modulation`). Verify it compiles as an empty stub: `pub fn compute(self, _ctx, _step, _outcome, _needs) -> f32 { match self { Self::Damage => crate::combat::ai::factors::step::damage::compute(_ctx,_step,_outcome,_needs), … } }`. Stub each leaf with `pub fn compute(...) -> f32 { 0.0 }` so `cargo build` passes before logic migration.

4. **Migrate Step factor logic into 7 leaves:**
   - `step/damage.rs` and `step/{kill_now,kill_promised,cc,heal}.rs`: lift body from `factors::offensive::compute_offensive` (`offensive.rs:24–116`) into 5 separate functions. Each one re-derives the `def`, `EffectDef::Summon` early-out, then computes only its own column from `outcome`. **Critical:** `compute_offensive` shares work (looks up `def` once, derives `damage_progress`, applies `crit_fail_adjusted`). When split, each StepFactor::compute calls `content.abilities.get(ability)` independently — that's `O(K)` extra hash lookups per step. Spec §"Что НЕ в scope" says no OffensiveCache in 8.A; accept the regression. The early-out for `EffectDef::Summon { .. }` must replicate in **each** of the 5 leaves to ensure summon casts contribute zero offensive value. Keep `pub(super) struct OffensiveFactors` and `pub(super) fn compute_offensive` private as an internal helper for the 5 leaves to share, optionally — but spec §"Удаляется" says `OffensiveFactors` may stay as `pub(super)` internal helper. The cleanest path: **keep `compute_offensive` and have each Step leaf call it and pluck its column** (e.g. `step::damage::compute = compute_offensive(…).damage`). FP-edge expectation: this is bit-equivalent if call order is preserved.
   - `step/scarcity.rs`: thin wrapper around `factors::scarcity::compute_scarcity(step, kill_now, ctx)`. Note `compute_scarcity` requires `kill_now` as input — a per-step signal already produced by Step::KillNow::compute. **Watch this dependency** — the macro-walk in commit 2 needs to pass `step::kill_now::compute(...)` into `step::scarcity::compute(...)` or scarcity must recompute kill_now internally. Cleanest: scarcity::compute reads `outcome.p_kill_now` directly (it's the same fact source — see `factors::offensive` lines 112: `let kill_now = outcome.p_kill_now;`). Adjust `step/scarcity.rs::compute(ctx, step, outcome, _needs)` to derive `let kill_now = outcome.p_kill_now;` then call `factors::scarcity::compute_scarcity(step, kill_now, ctx)`.
   - `step/saturation.rs`: wrapper around `factors::saturation::buff_saturation_penalty(ability, target, caster, pre_snap, content)`. Sig is `(ctx, step, outcome, needs)`. Inside: pattern-match `step` → `ScoredStep::Cast { ability, target, .. }`, take `caster = ctx.active.entity`, `pre_snap = ctx.snap` (perspective shift handled by caller), `content = ctx.world.content`. Move-only steps return 0.0.

5. **Migrate Plan factor logic into 3 leaves:**
   - `plan/intent.rs::compute(plan, intent, ctx) -> f32`: lift body from `compute_plan_intent_sum` (`scorer.rs:632–758`). This is a 130-line block — leave it as a single function in the leaf, and re-export back into `scorer.rs` as `compute_plan_intent_sum` for now (commit 2 changes the scorer caller; commit 3 may consolidate naming). **Test:** `plan_factor_compute_matches_legacy` (intent variant) — pin against a fixture-built `[Cast, Cast]` plan under `FocusTarget`, identical to `sum_factors_scale_by_step_weight` (`scorer.rs:870–953`).
   - `plan/tempo_gain.rs::compute(plan, intent, ctx) -> f32`: thin wrapper over existing `factors::tempo::compute_plan_tempo_gain(plan, intent, ctx)`. No body migration; just re-route.
   - `plan/self_survival.rs::compute(plan, _intent, ctx) -> f32`: thin wrapper over existing `factors::survival::compute_plan_self_survival(plan, ctx)`. **Document `_intent` is unused** in a const at the top: `pub const NAME: &str = "self_survival"; /// `intent` is unused — required only for trait uniformity. Pinned by `self_survival_ignores_intent_parameter`.`. Add the corresponding test.

6. **Migrate Terminal factor logic into 8 leaves.** Each is a thin wrapper over the existing free function in `planning/terminal.rs:93–319`, with the macro-uniform sig `(plan, snap, ctx)`. The legacy free functions `compute_exposure_at_end`, `compute_secure_kill`, `compute_board_control_gain`, `compute_pressure_spacing_zone` take `(plan, ctx)` only — wrappers must accept `_snap` and ignore. `compute_next_turn_lethality`, `compute_ally_rescue`, `compute_line_actionability`, `compute_density_value` already take `(plan, initial_snap, ctx)` — match exactly.

   Tests: `terminal_factor_compute_matches_legacy` × 8 inline tests in `factors/terminal/{factor}.rs`. Each pins the leaf against the existing legacy free function on a small fixture (the existing test scaffolding in `terminal.rs:367–1044` is good source material — replicate one fixture per axis as a leaf-local test).

7. **Implement `PlanFactorValues` typed wrapper** in `factors/mod.rs`. Storage `[f32; 10]`. `get`/`set`/`get_plan`/`set_plan` use `StepFactor::count() = 7` as offset for plan slots. Manual serde:
   - `Serialize`: open a `serde::ser::SerializeMap`, iterate `StepFactor::iter()` and `PlanFactor::iter()`, emit `(f.name(), self.get(f))` pairs.
   - `Deserialize`: visitor expects map; for each `(key, val)`, try `StepFactor::from_name(&key)` then `PlanFactor::from_name(&key)`; unknown keys → `serde::de::Error::custom(format!("unknown factor {key}"))`. Missing keys default to 0.0 (so adding a new factor in step 17 doesn't break v29 logs).
   - Round-trip test `factor_values_serde_round_trip_named_map` in `factors/mod.rs::tests`.

8. **Implement new `TerminalScore` typed wrapper** in `factors/terminal/mod.rs`. Same shape as PlanFactorValues (`[f32; 8]`, get/set/serde named map via `TerminalFactor::from_name`). Round-trip test `terminal_score_serde_round_trip_named_map`.

9. **`log.rs` не трогаем.** `SCHEMA_VERSION` остаётся = 28. Wire format в коммите 1 не меняется (`PlanAnnotation.raw_factors: PlanFactors` сериализуется по-старому). Bump схемы и обновление тестов rejection переезжают в коммит 2 вместе с реальным изменением формата.

**Ловушки и риски.**

- **macro_rules! tt-muncher.** Optional `(signed: true)` and `(need: Foo)` parsing. The spec example shows them as parenthesised attribute lists. Recommended pattern: catch each variant as `$Variant:ident => $module:ident $( ( $($a:tt)* ) )?` and pass the optional `$($a:tt)*` block to a per-variant emitter macro that defaults `signed = false`, `need = None` and overrides per attr. Watch: `(signed: true)` and `(need: SetupAOE)` may need to coexist on TerminalFactor — design the inner emitter to accept `key: value` pairs separated by commas. **Test the macro by hand with `cargo expand --bin <crate>`** before scaling up.
- **Macro hygiene.** If the macro body references `crate::combat::ai::factors::step::$module::compute`, the path resolves from the call-site crate root. Confirm by stubbing one leaf and checking the expanded match arm.
- **Naming collision on `TerminalScore`.** The legacy struct lives at `planning::terminal::TerminalScore` and is the type of `PlanAnnotation.terminal` (`outcome/mod.rs:159`). Commit 1 introduces the new typed wrapper at `factors::terminal::TerminalScore`. Do **not** swap `PlanAnnotation.terminal` over yet — that's commit 2. Until then, both types must coexist; expect linter to flag the unused `factors::terminal::TerminalScore`.
- **`compute_offensive` early-out for Summon.** Each Step leaf must replicate `if matches!(def.effect, EffectDef::Summon { .. }) { return 0.0; }` or factor it out. If you keep `compute_offensive` as the shared core (recommended), this stays in one place.
- **Saturation step-leaf access to `pre_snap`.** Spec sigs say `step::saturation::compute(ctx, ...)`. Inside scorer.rs:547, `step_ctx = ctx.with_perspective(&sim_actor, pre_snap)`. The leaf reads `ctx.snap` as the pre-step snapshot — only correct when caller already invoked `with_perspective`. Document the contract on the Step leaf signature: "`ctx.snap` is the pre-step snapshot".
- **`SIGNED_FACTOR` per-leaf.** Legacy `SIGNED_FACTOR[5] = true` (intent), `[6] = true` (scarcity), `[7] = true` (tempo_gain), `[8] = true` (saturation), `[9] = true` (self_survival). Map: `step::scarcity::SIGNED = true`, `step::saturation::SIGNED = true`, `plan::intent::SIGNED = true`, `plan::tempo_gain::SIGNED = true`, `plan::self_survival::SIGNED = true`. Pin via `factor_kind_macro_generates_correct_enum_metadata`.
- **`PlanFactorValues` array layout (Q1 — РЕШЕНО option A).** Variant order: `StepFactor::{Damage, KillNow, KillPromised, Cc, Heal, Scarcity, Saturation}` + `PlanFactor::{Intent, TempoGain, SelfSurvival}`. Slots: `[damage(0), kill_now(1), kill_promised(2), cc(3), heal(4), scarcity(5), saturation(6), intent(7), tempo_gain(8), self_survival(9)]`. Это новый column order — **TOML rows в `tuning.rs:231–296` должны быть перепермутированы** под `[damage, kill_now, kill_promised, cc, heal, scarcity, saturation, intent, tempo_gain, self_survival]`. К `default()` impl добавить комментарий с указанием нового порядка и пометкой "был ... в v28". Поведенчески эквивалентно (это та же permutation), но визуально TOML defaults меняются.
- **`from_name` collisions across enums.** `StepFactor::from_name("intent")` returns None (intent is in PlanFactor). Manual serde must try both. Test: serialise a `PlanFactorValues`, deserialise, check round-trip — also test missing keys default to 0.0 (forward-compat), unknown keys error.
- **Schema bump tied to wire-format change.** В коммите 1 `SCHEMA_VERSION` = 28 (без изменений); в коммите 2 — bump до 29 одновременно с переименованием `raw_factors`→`factors` и переключением `PlanAnnotation.terminal` на новый wrapper. Это инвариант: версия схемы шагает только вместе с реальным изменением формата.

**Gate-проверка (commit 1).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets` зелёный.
- New unit tests:
  - `factor_kind_macro_generates_correct_enum_metadata` (3 enums × `count`/`iter`/`from_name` round-trip).
  - `step_factor_compute_pure_for_known_outcome` × 7 (one per variant).
  - `plan_factor_compute_matches_legacy` × 3 (Intent vs `compute_plan_intent_sum`, TempoGain vs `compute_plan_tempo_gain`, SelfSurvival vs `compute_plan_self_survival`).
  - `terminal_factor_compute_matches_legacy` × 8 (delegates exact match against legacy free fns).
  - `factor_values_serde_round_trip_named_map`.
  - `terminal_score_serde_round_trip_named_map`.
  - `self_survival_ignores_intent_parameter` (passing two different `TacticalIntent` values yields equal output).
- Existing `cargo test` зелёный (no migration yet, all callers still on `PlanFactors`; `SCHEMA_VERSION` остаётся 28).
- `cargo run --bin ai_scenarios` идентичный output (no behavior change).
- **No golden diff.** The wire format hasn't changed; logs still carry `raw_factors: PlanFactors` unchanged.

---

## Commit 2 — Migrate `finalize_scores` aggregator + `compute_plan_factors_sans_intent` step-loop to registry walk

**Цель.** Production scorer code-paths use the new registry. `PlanAnnotation.raw_factors: PlanFactors` swaps to `factors: PlanFactorValues`; `PlanAnnotation.terminal` swaps to the new typed wrapper. Schema-v29 wire format goes live (named-map for both fields). `compute_factors` callsite count drops from 3 to 2 (intent.rs:950, 972 still use it; scorer.rs:549 migrates).

**Файлы (изменить).**

- `src/combat/ai/planning/scorer.rs` — three migration zones:
  - `:38–41` imports: drop `INTENT_IDX, NUM_FACTORS, SCARCITY_IDX, SIGNED_FACTOR, PlanFactors`; add `PlanFactorValues, StepFactor, PlanFactor, TerminalFactor`.
  - `:92–157` `score_plans_with_raw`, `rescore_with_intent`, `rescore_with_per_plan_modes`: change `Vec<PlanFactors>` → `Vec<PlanFactorValues>`. Field assignments `f.intent = …` → `f.set_plan(PlanFactor::Intent, …)`, `f.tempo_gain = …` → `f.set_plan(PlanFactor::TempoGain, …)`.
  - `:174–348` `finalize_scores`: rewrite the body. Replace:
    - `:194–213` (build min/max + denom) → use `BatchStats` per factor, populated by walking `StepFactor::iter().chain(PlanFactor::iter())` and reading via `raw[i].get(f)` / `raw[i].get_plan(f)`. The `denom` array can stay positional since it indexes `[f32; 10]`.
    - `:227–255` (per-plan score loop) → walk `StepFactor::iter()` and `PlanFactor::iter()`, accumulate `score += default_norm(arr[i], &batch[i], factor.signed()) * weights[i]` for the matching slot. **Keep `weights[INTENT_IDX]` modulation by `intent_commitment`** (`:223`) — replace with `weights[StepFactor::count() + PlanFactor::Intent as usize] *= world.difficulty.intent_commitment;`. **Keep `weights[SCARCITY_IDX]` modulation by `resource_discipline`** — replace with `weights[StepFactor::Scarcity as usize] *= …;`. Document the slot indices in a comment.
    - Keep summon/trade/repair-affinity/noise blocks **unchanged** — those move in 8.B/8.C.
    - `:274–294` terminal aggregator: replace inline 8-line block with macro walk:
      ```
      let tw = if ctx.last_goal.is_some() { active.role.terminal_weights_continuation(...) } else { active.role.terminal_weights(...) };
      let needs = ctx.need_signals;
      for (plan, score) in plans.iter().zip(scores.iter_mut()) {
          let t = &plan.annotation.terminal;
          for f in TerminalFactor::iter() {
              *score += t.get(f) * tw[f as usize] * f.need_modulation().amplify(needs);
          }
      }
      ```
      **Critical FP-exact reproduction.** Spec §"Ловушки" reminds: lines 290 (`line_actionability * tw[5]`) and 292 (`pressure_spacing_zone * tw[7]`) **do not multiply by `(1 + needs.*)`** — both have `NeedAxis::None` per spec §"variants" and `NeedAxis::None.amplify(_) = 1.0`. The for-loop reproduces the inline math exactly only if `NeedAxis::None.amplify(_) = 1.0` (defined in commit 1). Pin via `terminal_aggregator_via_registry_matches_legacy_formula`.
  - `:510–582` `compute_plan_factors_sans_intent`: replace the per-step accumulation `damage_sum += raw.damage * step_weight; …` with:
    ```
    let step_outcome = plan.annotation.outcomes.get(idx).cloned().unwrap_or_default();
    let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);
    if let PlanStep::Cast { .. } = step {
        let step_ctx = ctx.with_perspective(&sim_actor, pre_snap);
        for f in StepFactor::iter() {
            let v = f.compute(&step_ctx, &scored_step, &step_outcome, ctx.need_signals);
            sums[f as usize] += v * step_weight;
        }
    }
    ```
    **Watch:** `Saturation` is a StepFactor under the new layout — its compute body lives in `step::saturation::compute`. The legacy code computed saturation separately at `:559–564`. The new walk subsumes that.
    Post-loop, return `PlanFactorValues` with all step slots filled and plan slots zero (intent/tempo/self_survival populated by separate calls, see `:496–505`).
  - `compute_plan_factors` (`:496–505`): adjust to assemble `PlanFactorValues` (plan slots filled here), not `PlanFactors`.
- `src/combat/ai/outcome/mod.rs:147–207`:
  - Line 56: drop `use crate::combat::ai::factors::PlanFactors;`. Add `use crate::combat::ai::factors::{PlanFactorValues, terminal::TerminalScore as FactorTerminalScore};` (or expose the new TerminalScore via `factors::TerminalScore`).
  - Line 159: `pub terminal: crate::combat::ai::planning::terminal::TerminalScore` → `pub terminal: crate::combat::ai::factors::TerminalScore` (new typed wrapper). Keep `#[serde(default)]`.
  - Line 198: `pub raw_factors: PlanFactors` → `pub factors: PlanFactorValues`. **Rename field** `raw_factors` → `factors`. This is the v28→v29 wire-format break per spec §"Изменения 9".
- `src/combat/ai/planning/terminal.rs:42–86` — change `terminal_state_score` return type to `factors::TerminalScore`. Body still calls the 8 legacy free functions, but now writes into the typed wrapper via `set(TerminalFactor::ExposureAtEnd, …)` etc. **Keep legacy struct deleted** OR keep struct for one more commit — recommend delete now since `outcome/mod.rs:159` is the only consumer and it migrates in this commit. Update tests in `terminal.rs:336–1044` accordingly.
- `src/combat/ai/log.rs:158` — bump `SCHEMA_VERSION` 28 → 29. Обновить v27/v26 rejection tests (`log.rs:1418–1452`) под `required: 29`. Добавить новый `actor_tick_v28_load_yields_unsupported_schema_error`. Обновить `hint`-строку в `parse_actor_tick` (`log.rs:1099`) — упомянуть `factors`/`terminal` named map.
- `src/combat/ai/log.rs:282` — `pub raw_factors: [f32; 10]` field of `PlanLogEntry` → drop. Now `PlanLogEntry` has only the named-map `factors` field that comes from `annotation.factors` and `annotation.terminal`. Plan-to-log builder `plan_to_log_entry` (`:458–487`) drops the `raw_factors: [f32; 10]` parameter.
- `src/combat/ai/log.rs:856–860` — `LoggedPlan { rank, steps, annotation: PlanAnnotation }`. Since `PlanAnnotation` now carries `factors: PlanFactorValues` and `terminal: TerminalScore` with custom serde, serialised JSON shape changes from:
  ```
  "raw_factors": [1.2, 0.8, ...],
  "terminal": { "exposure_at_end": 0.4, ... }
  ```
  to:
  ```
  "factors": { "damage": 1.2, "kill_now": 0.8, ..., "self_survival": 0.0 },
  "terminal": { "exposure_at_end": 0.4, ... }
  ```
  The `terminal` block is unchanged in shape (it was already a struct serde-default; now it's a typed wrapper with custom serde producing the same named map). The `raw_factors` → `factors` field rename is the visible v28→v29 break.
- `src/combat/ai/utility/mod.rs:281, 335, 436` — three lines reading/writing `ann.raw_factors`. Change to `ann.factors` and `.as_array()` accessor (line 436). The `as_array()` call goes away — `plan_to_log_entry` now consumes the `PlanFactorValues` wrapper directly via custom serde. Audit `:432–445`:
  ```
  log::plan_to_log_entry(
      &plans[idx],
      rank + 1,
      idx == best_idx,
      &ann.factors,         // was: ann.raw_factors.as_array()
      &ann.terminal,        // new: terminal also flows through entry
      ...
  )
  ```
  Update `plan_to_log_entry` sig accordingly.
- `src/combat/ai/pipeline/mod.rs:6, 61, 63` — doc comments only, update wording `raw_factors` → `factors`.
- `src/combat/ai/pipeline/stages/{viability,protect_self,killable_gate,adaptation,pick_best}.rs` — every place reading `a.raw_factors` (5+ files, ~12 lines total per the grep above) becomes `a.factors`. Field reads change:
  - `viability.rs:34, 91, 100, 137`: `a.raw_factors.intent` → `a.factors.get_plan(PlanFactor::Intent)`. `ann.raw_factors = new_raw` → `ann.factors = new_raw`.
  - `protect_self.rs:45, 47, 84, 107, 120, 138, 139, 162, 163`: `Vec<PlanFactors>` → `Vec<PlanFactorValues>`. Test fixtures `PlanFactors { self_survival: 0.5, ..Default::default() }` → `let mut f = PlanFactorValues::default(); f.set_plan(PlanFactor::SelfSurvival, 0.5); f`.
  - `killable_gate.rs:44, 48, 84, 90, 107, 112, 129, 171, 172, 207`: same pattern. `raw[i].kill_now` → `raw[i].get(StepFactor::KillNow)`. `raw[i].damage` → `raw[i].get(StepFactor::Damage)`.
  - `adaptation.rs:23, 25, 29, 35, 39, 43, 60, 90, 112, 130, 131, 153, 154, 178, 202, 203`: same; also `f.self_survival` (`adaptation.rs:288`) → `f.get_plan(PlanFactor::SelfSurvival)`.
  - `pick_best.rs:25, 27, 40, 77`: `Vec<PlanFactors>` → `Vec<PlanFactorValues>`.
- `src/combat/ai/planning/picker.rs:19, 94, 108, 142` — `mercy_cruelty(raw: &PlanFactors) -> f32` → `mercy_cruelty(raw: &PlanFactorValues) -> f32`. Body: `raw.kill_now` → `raw.get(StepFactor::KillNow)`, etc.
- `src/combat/ai/planning/sanity.rs:12, 339, 345, 346, 352` — `apply_protect_self_mask(raw: &[PlanFactors], …)` → `&[PlanFactorValues]`. `f.self_survival` → `f.get_plan(PlanFactor::SelfSurvival)`.
- `src/combat/ai/planning/adaptation.rs:265, 288, 819, 825` — same field-access pattern.
- `src/combat/ai/role.rs:308, 354, 356, 365` — test code uses `DAMAGE_IDX, HEAL_IDX` constants. Replace with `StepFactor::Damage as usize` and `StepFactor::Heal as usize`. Drop the `use … {DAMAGE_IDX, HEAL_IDX};` import.
- `src/combat/ai/debug.rs:8, 437, 548, 590` — `&[PlanFactors]` → `&[PlanFactorValues]` in `build_debug_snapshot` sig. `raw_factors[i].as_array()` → expand into a `[f32; 10]` via `let arr = std::array::from_fn(|j| raw_factors[i].as_slice()[j]);` or expose `pub fn as_array(&self) -> [f32; 10]` on `PlanFactorValues`. Recommended: add `pub fn as_array(&self) -> [f32; 10] { self.0 }` for callers that need the positional view.

**Подшаги (порядок исполнения).**

1. Add `PlanFactorValues::as_array(&self) -> [f32; 10]` accessor in commit 1's typed wrapper if not already done — debug.rs needs it.
2. **Mechanical rename `raw_factors` → `factors` everywhere.** Use a project-wide rename pass: every `ann.raw_factors`, `a.raw_factors`, `pool.annotations[i].raw_factors`, etc. → `ann.factors`. This is a flat substitution for ~30 callsites. Run `cargo check` between renames.
3. **Type-swap callers to `PlanFactorValues`.** All `Vec<PlanFactors>` → `Vec<PlanFactorValues>`, `&[PlanFactors]` → `&[PlanFactorValues]`, all `.field` reads → `.get(StepFactor::Field)` or `.get_plan(PlanFactor::Field)`. Test fixtures in `pipeline/stages/*.rs` and `planning/sanity.rs` get reworked.
4. **Migrate `compute_plan_factors_sans_intent` step-loop** to `StepFactor::iter()` walk. Pin behaviour via `compute_plan_factors_via_step_registry_matches_legacy` (FP-edge tolerance `≤5/N`).
5. **Migrate `finalize_scores` terminal aggregator** to `TerminalFactor::iter()` walk. Pin via `terminal_aggregator_via_registry_matches_legacy_formula`. **Critical:** ensure `NeedAxis::None.amplify(_) = 1.0` so the no-needs columns (line_actionability slot 5, pressure_spacing_zone slot 7) reproduce inline math exactly.
6. **Migrate batch normalisation in `finalize_scores`** to use registry-driven `signed()` per factor instead of positional `SIGNED_FACTOR[i]`. Build per-factor `BatchStats` by iterating `StepFactor::iter().chain(PlanFactor::iter())` and reading via `get(f)` / `get_plan(f)`.
7. **Update `log.rs::plan_to_log_entry`** signature to take `&PlanFactorValues` instead of `[f32; 10]`. Drop the local `raw_factors` field on `PlanLogEntry`. Replace with two flowing pieces: `factors: &'a PlanFactorValues`, `terminal: &'a TerminalScore`. Custom serde on the wrappers produces the named-map JSON.
8. **Update `outcome/mod.rs::PlanAnnotation`** — `raw_factors: PlanFactors` → `factors: PlanFactorValues`; `terminal: planning::terminal::TerminalScore` → `terminal: factors::TerminalScore`. Run `cargo check`; fix per-stage callers as they fail.
9. **Run full test suite.** Expect **golden diff to manifest only as field-rename** (`raw_factors` → `factors` in JSON keys; values unchanged at FP-exact for terminal aggregator, ≤5/N FP-edge tolerance for the registry-walk step loop). If FP-edge >5/N, recheck summation order in `compute_plan_factors_sans_intent`: legacy did `damage_sum += raw.damage * step_weight; kill_now_sum += raw.kill_now * step_weight; …` (each factor independent across step iterations); registry walk does `for f in iter() { sums[f as usize] += f.compute(...) * step_weight; }` per step before stepping forward — should be bit-exact.

**Ловушки и риски.**

- **`raw_factors` → `factors` rename is wire-visible.** Any external tool reading the JSON expects a v29-format new key. Update `replay_ai_log.rs` and `mine_ai_logs.rs` to read `annotation.factors` (typed PlanFactorValues with custom deser). For 8.A scope: `replay_ai_log.rs` is critical (per the prompt context); `mine_ai_logs.rs` cleanup is in 8.C but **its build must not break** in 8.A — if it deserialises `LoggedPlan` via `PlanAnnotation`, the rename flows through automatically because `PlanAnnotation` is shared. Audit `mine_ai_logs.rs` for any direct `.raw_factors` reads:
  ```
  rg -n "raw_factors|factor_breakdown" src/bin/mine_ai_logs.rs
  ```
  If hits, swap to `.factors` plus accessor calls.
- **Test fixture rebuild.** `pipeline/stages/{protect_self,killable_gate,adaptation}.rs` test fixtures construct `PlanFactors { self_survival: 0.5, ..Default::default() }` directly. After the rename, this becomes:
  ```
  let mut f = PlanFactorValues::default();
  f.set_plan(PlanFactor::SelfSurvival, 0.5);
  f
  ```
  Verbose. Add a test helper `fn pfv(setters: &[(StepFactor or PlanFactor, f32)]) -> PlanFactorValues` to keep fixtures compact. Alternatively, expose `pub fn from_pairs(pairs: &[(impl Into<&str>, f32)])` for tests only.
- **`compute_plan_factors_via_step_registry_matches_legacy` FP-edge.** Registry walk reads `step::damage::compute(...)` etc. Each leaf calls `compute_offensive` (or its sub-formulas), so each step computes the full offensive set 5 times instead of once. **Sum order is identical** — FP-exact match is achievable IF each leaf re-derives the same expected_damage / kill_now / cc value on each call. **Risk:** `compute_offensive` accesses `outcome.enemy_damage_per_entity` which is a `Vec<(Entity, f32)>` — sum order across the vec is deterministic across calls. Should be exact. If golden diff shows ≤5/N FP-edge, that's `f32::EPSILON`-level rounding from intermediate temporaries and is in budget. If >5/N, suspect a leaf reordered an addition; pin and fix.
- **Terminal aggregator FP-exactness.** The new for-loop accumulates into `score` in `TerminalFactor::iter()` order = `ExposureAtEnd, NextTurnLethality, SecureKill, AllyRescue, BoardControlGain, LineActionability, DensityValue, PressureSpacingZone`. The legacy code (`scorer.rs:284–292`) accumulates in the **exact same order**. FP-exact match expected.
- **`compute_factors` still alive.** `intent.rs:950, 972` still call `factors::compute_factors`. That's fine — commit 3 kills those callsites. Don't try to migrate them in commit 2.
- **`compute_plan_factors_sans_intent` perspective shift.** The Cast branch wraps the StepFactor walk in `step_ctx = ctx.with_perspective(&sim_actor, pre_snap)`. **Verify** the registry walk uses `step_ctx`, not `ctx`, when calling `f.compute(...)`. Saturation factor especially needs `pre_snap` via `step_ctx.snap`.
- **`weights[i]` indexing in `finalize_scores`.** TOML weights table is `[[f32; 10]; 5]`; `factor_weights(...)` returns `[f32; 10]`. The new layout puts step-factors in slots 0..6, plan-factors in 7..9 (under option A). Ensure the per-row TOML default (re-permuted in `tuning.rs:231–296`) matches.
- **`intent_commitment` and `resource_discipline` modulation indices.** Legacy: `weights[INTENT_IDX] *= intent_commitment` (idx 5), `weights[SCARCITY_IDX] *= resource_discipline` (idx 6). Under option A: intent slot is `StepFactor::count() + PlanFactor::Intent as usize = 7 + 0 = 7`; scarcity is `StepFactor::Scarcity as usize = 5`. **Update both indices.** Inline a comment with the resolved indices.
- **`PickInfo` and `PickMechanics` are NOT touched in 8.A.** Spec §"Изменения" 9 mentions `pick.noise_applied` — that's commit 8.C. In 8.A they remain unchanged.

**Gate-проверка (commit 2).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets` зелёный.
- New tests (in addition to commit-1 set):
  - `compute_plan_factors_via_step_registry_matches_legacy` (fixture pin against pre-migration golden).
  - `terminal_aggregator_via_registry_matches_legacy_formula` (fixture pin).
  - `actor_tick_v29_round_trip` (build a `PlanAnnotation` via prod path, serialise, deserialise, compare bit-for-bit on `factors` and `terminal` fields).
  - All commit-1 tests still green.
  - Обновлённые v26/v27 rejection-тесты (теперь `required: 29`) + новый `actor_tick_v28_load_yields_unsupported_schema_error`.
- `cargo run --bin ai_scenarios` — **golden diff expected per-entry FP-edge ≤5/N**. Field rename `raw_factors` → `factors` is visible. Any per-entry numeric drift >5e-6 → investigate sum order.
- `cargo run --bin replay_ai_log -- <fresh v29 corpus>` reads the new format successfully.
- v28 logs trigger `LogError::UnsupportedSchema { found: 28, required: 29 }`.

---

## Commit 3 — Migrate `intent.rs` callers + delete legacy types

**Цель.** `compute_factors`, `PlanFactors` struct, `*_IDX` constants, `SIGNED_FACTOR`, `NUM_FACTORS`, and `IntentWeights::dot(&PlanFactors)` are all gone. `factors/mod.rs` reduced to module declarations + `ScoredStep` + re-exports. The 8.A migration is complete.

**Файлы (изменить).**

- `src/combat/ai/intent.rs:4` — drop `compute_factors, PlanFactors` from imports; keep `aoe_area, aoe_hits`.
- `src/combat/ai/intent.rs:820–842` (`IntentWeights`) — keep struct + builder methods. Drop `dot(&PlanFactors)` (`:835–841`). Add new narrow API:
  ```rust
  /// Score the offensive value of `step` from the perspective of `focus_target`.
  /// Returns 0 if `step` has no target (Move) or targets a non-focus entity
  /// (and isn't an AoE that covers focus). Used by FocusTarget/ApplyCC.
  pub(crate) fn intent_offensive_value_on_target(
      focus: Entity,
      step: &ScoredStep,
      ctx: &ScoringCtx,
      outcome: &ActionOutcomeEstimate,
      weights: &IntentWeights,
      content: &ContentView,
  ) -> f32 { ... }
  ```
  Body folds the existing `filter_offensive_for_target` (`:855–900`) **with** the dot-product into one self-contained function. Inside:
  - For Move: return 0.
  - For Cast on focus directly: return `weights.damage * StepFactor::Damage.compute(...) + weights.kill_now * StepFactor::KillNow.compute(...) + weights.kill_promised * StepFactor::KillPromised.compute(...) + weights.cc * StepFactor::Cc.compute(...)`.
  - For Cast AoE covering focus: same sum × 0.6.
  - Otherwise: 0.
- `src/combat/ai/intent.rs:855–900` (`filter_offensive_for_target`) — delete; logic absorbed into `intent_offensive_value_on_target`.
- `src/combat/ai/intent.rs:949–957` (FocusTarget Cast branch in `intent_score`) — replace:
  ```rust
  let raw = compute_factors(step_ctx, step, outcome);
  let filtered = filter_offensive_for_target(raw, *focus, step, snap, content);
  let weights = IntentWeights::default().kill_now(2.0).kill_promised(0.3).damage(1.0).cc(0.5);
  weights.dot(&filtered)
  ```
  with:
  ```rust
  let weights = IntentWeights::default().kill_now(2.0).kill_promised(0.3).damage(1.0).cc(0.5);
  intent_offensive_value_on_target(*focus, step, step_ctx, outcome, &weights, content)
  ```
- `src/combat/ai/intent.rs:971–977` (ApplyCC Cast branch) — same swap with `IntentWeights::default().cc(1.5).damage(0.3)`.
- `src/combat/ai/factors/mod.rs:142–272` — delete:
  - `pub const NUM_FACTORS: usize = 10;` (`:145`)
  - All `*_IDX` constants (`:152–161`)
  - `pub const SIGNED_FACTOR: [bool; NUM_FACTORS]` (`:167–169`)
  - `pub struct PlanFactors { ... }` (`:181–192`) and its `impl` (`:194–216`)
  - `pub fn compute_factors(...)` (`:241–272`)
  - Doc-block at `:142–179` updated to describe the new registry-based architecture.
- `src/combat/ai/factors/mod.rs:30–35` (re-exports) — review:
  - `pub use offensive::aoe_area;` keep (used by `intent.rs`, `factors::scarcity`).
  - `pub use saturation::buff_saturation_penalty;` keep until external callers migrate.
  - `pub use survival::compute_plan_self_survival;` evaluate if scorer still uses; if only PlanFactor::SelfSurvival uses, remove this re-export.
  - `pub use tempo::compute_plan_tempo_gain;` same logic.
  - `pub use aoe_hits::{aoe_hits, AoeHits};` keep.
  - `pub use adjustments::crit_fail_adjusted;` keep.
  - Add: `pub use registry::{BatchStats, NeedAxis, default_norm}; pub use step::StepFactor; pub use plan::PlanFactor; pub use terminal::{TerminalFactor, TerminalScore}; pub use crate::combat::ai::factors::PlanFactorValues;` (the wrapper).
- `src/combat/ai/planning/terminal.rs:36–86` — delete the legacy `TerminalScore` struct entirely. The free functions `compute_*` (`:93–319`) become `pub(crate)` (still used by the per-factor leaves). Or move them inline into the leaves and delete `terminal.rs` as a public surface module. Recommended: keep file as a thin shim that re-exports `TerminalFactor` from `factors::terminal`.
- `src/combat/ai/log.rs:77–91` — schema-evolution comment block. Update with v28→v29 break note.
- `src/bin/replay_ai_log.rs` — update `read_v28_events` → `read_v29_events`. The `LoggedPlan.annotation.factors` and `.terminal` fields deserialise via `PlanAnnotation`'s custom serde automatically; no manual JSON traversal needed. Update header comment "schema v28" → "schema v29". Update tests at `:855–880` to use v28 sample as the rejection test, v29 as the accept test.
- `src/bin/mine_ai_logs.rs` — same surface update (schema label only). Deeper mining changes are 8.C; 8.A only requires it builds and accepts v29 input.

**Подшаги (порядок исполнения).**

1. Add `intent_offensive_value_on_target` in `intent.rs` (next to `IntentWeights`). Test against legacy via `intent_score_via_narrow_offensive_api_matches_legacy` — for both `FocusTarget` Cast-on-focus, Cast-AoE-covering-focus, and Cast-on-non-focus cases.
2. Swap the two callers in `intent.rs:950, 972` from `compute_factors + filter + dot` → `intent_offensive_value_on_target`. Run `cargo test`. Pin: `intent_score` outputs identical for FocusTarget and ApplyCC fixtures.
3. Delete `filter_offensive_for_target` (`intent.rs:855–900`).
4. Delete `IntentWeights::dot(&PlanFactors)` (`intent.rs:835–841`).
5. Run `cargo check`. Anywhere `PlanFactors` is still referenced → refactor to `PlanFactorValues`. Final outliers from the grep: test fixtures, doc comments. Each becomes a one-line change.
6. Delete `compute_factors`, `PlanFactors` struct, `*_IDX` constants, `SIGNED_FACTOR`, `NUM_FACTORS` from `factors/mod.rs`.
7. Delete legacy `TerminalScore` struct from `planning/terminal.rs`. Move free functions into per-leaf `compute(...)` bodies if they aren't already (check during commit 1 — recommended path was thin wrappers, so the free functions are still in `terminal.rs`; convert wrappers to inline body in the leaves and then delete `terminal.rs`'s free functions). Or keep `terminal.rs` as a doc/tests file with `pub(crate)` wrappers.
8. Update doc-comment headers: `factors/mod.rs:1–20`, `planning/scorer.rs:1–37`, `planning/terminal.rs:1–35`, `pipeline/mod.rs:1–10`, `combat/ai/mod.rs` (if it has a registry doc) — describe new architecture.
9. Update `bin/replay_ai_log.rs` schema label and tests; verify `cargo run --bin replay_ai_log -- <v29 sample>` works on a freshly produced v29 log.
10. Final pass: `cargo test --all-targets`, `cargo clippy --all-targets`, `cargo run --bin ai_scenarios`.

**Ловушки и риски.**

- **`intent_offensive_value_on_target` AoE coverage check.** Legacy `filter_offensive_for_target` reads `aoe_area(def, target_pos, caster_tile)` from `factors::offensive::aoe_area` (`intent.rs:880`). Keep that call inside the new function — `aoe_area` re-export must stay (`factors/mod.rs:32`).
- **AoE 0.6 multiplier.** Legacy applies `*= 0.6` to all 4 offensive factors (`intent.rs:883–886`). The new function must apply the same scaling: `let scale = if direct { 1.0 } else if aoe_covers_focus { 0.6 } else { 0.0 }; sum_of_weighted_step_factors * scale`. Pin via `intent_score_via_narrow_offensive_api_matches_legacy` AoE case.
- **`factors::compute_plan_self_survival` re-export removal.** If commit 3 removes this re-export but external callers (test code) still reference `factors::compute_plan_self_survival`, the build breaks. Audit before removing:
  ```
  rg -n "compute_plan_self_survival|compute_plan_tempo_gain" src/
  ```
  Likely callers: tests in `factors/survival.rs` (own module, fine), maybe `planning/scorer.rs::compute_plan_factors` (already migrated). Remove only if no other consumers.
- **`TerminalScore` deletion ordering.** `outcome/mod.rs:159` was migrated in commit 2 to point to the new `factors::TerminalScore`. `planning::terminal::TerminalScore` is unused as of commit 2's end. Safe to delete in commit 3.
- **Doc-comment drift.** `planning/scorer.rs:1–37` describes the 10-factor pipeline with positional indices. Rewrite to reference `StepFactor` / `PlanFactor` enums and the registry contract. This is non-functional but high-leverage for future readers.
- **No new tests likely needed** beyond `intent_score_via_narrow_offensive_api_matches_legacy` — the registry-walk pin tests from commit 2 still cover the broader path.

**Gate-проверка (commit 3).**

- `cargo build --all-targets` зелёный.
- `cargo clippy --all-targets` зелёный.
- All commit-1 + commit-2 tests still green.
- New: `intent_score_via_narrow_offensive_api_matches_legacy` (FocusTarget direct hit, AoE coverage, miss; ApplyCC direct hit). Pin against pre-commit-3 outputs (saved as fixture).
- `cargo run --bin ai_scenarios` — **golden diff zero** (no behavior change vs. commit 2; only dead-code removal).
- `cargo run --bin replay_ai_log -- <v29 corpus>` succeeds; `<v28 corpus>` returns `UnsupportedSchema { found: 28, required: 29 }`.
- Mining baseline (post-step-7 v28 → re-run `ai_scenarios` on post-8.A v29 → mine v29) reproduces post-step-7 metrics **bit-for-bit** modulo the FP-edge tolerance from commit 2.
- `rg "PlanFactors|NUM_FACTORS|SIGNED_FACTOR|compute_factors|DAMAGE_IDX|INTENT_IDX|raw_factors" src/` returns **zero hits** in `src/combat/ai/` (only doc-comment / changelog references in `log.rs:77–91` are allowed).

---

## Чек-лист тестов (полный 8.A)

**Расположение и имя — название → файл-владелец.**

| # | Test name | File |
|---|---|---|
| 1 | `factor_kind_macro_generates_correct_enum_metadata` | `factors/registry.rs` (or wherever macro lives) |
| 2 | `step_factor_compute_pure_for_known_outcome::damage` | `factors/step/damage.rs::tests` |
| 3 | `step_factor_compute_pure_for_known_outcome::kill_now` | `factors/step/kill_now.rs::tests` |
| 4 | `step_factor_compute_pure_for_known_outcome::kill_promised` | `factors/step/kill_promised.rs::tests` |
| 5 | `step_factor_compute_pure_for_known_outcome::cc` | `factors/step/cc.rs::tests` |
| 6 | `step_factor_compute_pure_for_known_outcome::heal` | `factors/step/heal.rs::tests` |
| 7 | `step_factor_compute_pure_for_known_outcome::scarcity` | `factors/step/scarcity.rs::tests` |
| 8 | `step_factor_compute_pure_for_known_outcome::saturation` | `factors/step/saturation.rs::tests` |
| 9 | `plan_factor_compute_matches_legacy::intent` | `factors/plan/intent.rs::tests` |
| 10 | `plan_factor_compute_matches_legacy::tempo_gain` | `factors/plan/tempo_gain.rs::tests` |
| 11 | `plan_factor_compute_matches_legacy::self_survival` | `factors/plan/self_survival.rs::tests` |
| 12 | `self_survival_ignores_intent_parameter` | `factors/plan/self_survival.rs::tests` |
| 13 | `terminal_factor_compute_matches_legacy::*` × 8 | `factors/terminal/{factor}.rs::tests` |
| 14 | `factor_values_serde_round_trip_named_map` | `factors/mod.rs::tests` |
| 15 | `terminal_score_serde_round_trip_named_map` | `factors/terminal/mod.rs::tests` |
| 16 | `compute_plan_factors_via_step_registry_matches_legacy` | `planning/scorer.rs::tests` |
| 17 | `terminal_aggregator_via_registry_matches_legacy_formula` | `planning/scorer.rs::tests` |
| 18 | `intent_score_via_narrow_offensive_api_matches_legacy::focus_direct` | `intent.rs::tests` |
| 19 | `intent_score_via_narrow_offensive_api_matches_legacy::focus_aoe_covers` | `intent.rs::tests` |
| 20 | `intent_score_via_narrow_offensive_api_matches_legacy::focus_aoe_misses` | `intent.rs::tests` |
| 21 | `intent_score_via_narrow_offensive_api_matches_legacy::apply_cc_direct` | `intent.rs::tests` |
| 22 | `actor_tick_v29_round_trip` | `log.rs::tests` |
| 23 | `actor_tick_v28_load_yields_unsupported_schema_error` | `log.rs::tests` |
| 24 | (legacy update) `actor_tick_v27_load_yields_unsupported_schema_error` — required = 29 | `log.rs::tests:1418` |
| 25 | (legacy update) `actor_tick_v26_load_yields_unsupported_schema_error` — required = 29 | `log.rs::tests:1429` |

**Existing tests that MUST still pass unchanged** (regression guard):
- `factors/scarcity.rs` — all 5 tests.
- `factors/saturation.rs` — all 4 tests.
- `factors/survival.rs` — all 7 tests (especially phantom-tail regressions).
- `factors/tempo.rs` — all 4 tests.
- `planning/scorer.rs` — `sum_factors_scale_by_step_weight`, `post_goal_leaves_step_weight_purely_geometric`, `rescore_matches_full_score_under_same_intent` (these pin the discounted-sum invariants the migration must preserve).
- `planning/terminal.rs` — all 25+ axis tests.
- `pipeline/stages/{viability,protect_self,killable_gate,adaptation,pick_best}.rs` — all migrated to `PlanFactorValues`-based fixtures, asserts unchanged.

---

## Финальный gate 8.A

After commit 3 lands:

1. `cargo test --all-targets` — все зелёные, including the 25+ new tests above.
2. `cargo clippy --all-targets -- -D warnings` — зелёный.
3. `cargo build --all-targets --release` — зелёный.
4. `cargo run --bin ai_scenarios` — выходной log file:
   - Schema v29.
   - `factors` named map в каждом plan annotation.
   - `terminal` named map в каждом plan annotation.
   - Per-entry numeric values: ≤5/N FP-edge differ от post-step-7 baseline. >5/N → расследовать summation order in registry walk.
5. v28 logs (saved corpus from post-step-7) дают `LogError::UnsupportedSchema { found: 28, required: 29 }` — pinned by test #24 above and integration smoke test via `replay_ai_log`.
6. v29 round-trip: `replay_ai_log` reads → re-emits → читает обратно → bit-for-bit identical.
7. **Mining baseline reproduction.** Take a v28 corpus from post-step-7. Re-run the same scenario set under post-8.A code → get a v29 corpus. Mine the v29 corpus → metrics (move-only-wasted, repeated-tile-plans, zero-net-move, etc.) reproduce v28 metrics bit-for-bit modulo ≤5/N FP-edge per entry. >5/N → расследовать.
8. Dead-code grep: `rg -n "PlanFactors|NUM_FACTORS|SIGNED_FACTOR|compute_factors|raw_factors|DAMAGE_IDX|KILL_NOW_IDX|KILL_PROMISED_IDX|CC_IDX|HEAL_IDX|INTENT_IDX|SCARCITY_IDX|TEMPO_IDX|SATURATION_IDX|SELF_SURVIVAL_IDX" src/` → only mentions in changelog comments (`log.rs:77–91`) survive.

---

### Critical Files for Implementation

- `/Users/splav/personal/storyforge/src/combat/ai/factors/mod.rs` — registry roots, typed wrapper, deletion targets all live here.
- `/Users/splav/personal/storyforge/src/combat/ai/planning/scorer.rs` — `finalize_scores`, `compute_plan_factors_sans_intent`, `compute_plan_intent_sum`; the heart of the refactor.
- `/Users/splav/personal/storyforge/src/combat/ai/planning/terminal.rs` — terminal axis bodies migrate into per-factor leaves; legacy struct deleted.
- `/Users/splav/personal/storyforge/src/combat/ai/intent.rs` — `compute_factors` callsites and `IntentWeights::dot` removal; new narrow `intent_offensive_value_on_target` API.
- `/Users/splav/personal/storyforge/src/combat/ai/outcome/mod.rs` — `PlanAnnotation.raw_factors` → `factors`, `terminal` typed-wrapper swap; the wire-format break point.
- `/Users/splav/personal/storyforge/src/combat/ai/log.rs` — `SCHEMA_VERSION = 29`, `PlanLogEntry.raw_factors` field deletion, plan-to-log builder signature change.
