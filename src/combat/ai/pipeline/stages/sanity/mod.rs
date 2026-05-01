//! Sanity stage + shared types, helpers, and residual rules.
//!
//! ## Layout
//! - `mod.rs` — `SanityStage` (pipeline stage), `SanityHit`, `SanityRule` types,
//!   `sanity_adjust_plans` orchestrator, shared helpers
//!   (`expected_aoo_damage`, `plan_is_defensive`, `apply_protect_self_mask`,
//!   `plan_has_self_aoe`).
//! - `healer_exposure.rs` — Rule 1: non-healer abandoning unguarded healer.
//! - `retreat_trap.rs` — Rule 2: final tile with < 2 open neighbours.
//! - `synergy_bonus.rs` — Rule 3: reposition to safer tile + useful cast.
//!
//! The three rules that were migrated to critics in steps 10.1–10.2
//! (`Survival`, `AoOBleed`, `LosBlindspot`, `SelfAoe`) are no longer present;
//! `SanityRule` has exactly three variants.

mod healer_exposure;
mod retreat_trap;
mod synergy_bonus;

use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::factors::{aoe_area, PlanFactor, PlanFactorValues};
use crate::combat::ai::pipeline::{PlanStage, ScoredPool, StageCtx};
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::scoring::position_eval::evaluate_position;
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::combat::effects_math::final_damage_f32;
use crate::content::abilities::AoEShape;
use crate::game::hex::Hex;
use std::collections::HashSet;

// ── Sanity rule observability ──────────────────────────────────────────────

/// Identifies one residual sanity rule. Variants that were migrated to
/// `PlanCritic` in steps 10.1–10.2 (`Survival`, `AoOBleed`, `LosBlindspot`,
/// `SelfAoe`) are removed in step 10.4. Stable codes consumed by offline
/// analyzers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanityRule {
    /// Non-healer abandoning the team's unguarded healer.
    HealerExposure,
    /// Final tile has fewer than 2 open neighbours (retreat trap).
    RetreatTrap,
    /// Plan repositions to a safer/better tile AND includes a useful cast (synergy bonus).
    SynergyBonus,
}

impl SanityRule {
    /// Short stable code for offline analyzer consumption.
    pub fn code(self) -> &'static str {
        match self {
            Self::HealerExposure => "healer_exposure",
            Self::RetreatTrap => "retreat_trap",
            Self::SynergyBonus => "synergy_bonus",
        }
    }
}

/// Records that one sanity rule fired on one plan and the factor it applied.
/// `multiplier < 1.0` for penalties; `> 1.0` for the synergy bonus.
/// The value is the **clamped** factor that was actually multiplied into the
/// plan score.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SanityHit {
    pub rule: SanityRule,
    pub multiplier: f32,
}

// ── Pipeline stage ─────────────────────────────────────────────────────────

pub struct SanityStage;

impl PlanStage for SanityStage {
    fn name(&self) -> &'static str {
        "sanity"
    }

    fn apply(&self, pool: &mut ScoredPool, ctx: &mut StageCtx) {
        use crate::combat::ai::pipeline::score_trace::{MultiplierHit, MultiplierKind};

        // Snapshot entry scores BEFORE sanity_adjust_plans mutates them —
        // needed to detect masked plans (NEG_INFINITY) for the invariant guard.
        let entry_scores: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();

        let mut scores: Vec<f32> = entry_scores.clone();
        let breakdown = sanity_adjust_plans(&mut scores, &pool.plans, ctx.scoring);

        for (i, (ann, (new_score, hits))) in pool
            .annotations
            .iter_mut()
            .zip(scores.into_iter().zip(breakdown.into_iter()))
            .enumerate()
        {
            ann.score = new_score;

            // P3a.6: bridging-reset removed. FinalizeStage (upstream) sets
            // trace.base; this stage pushes multiplier hits on top of the
            // accumulated trace.
            for hit in &hits {
                ann.score_trace.push_multiplier(MultiplierHit {
                    kind: MultiplierKind::Sanity,
                    value: hit.multiplier,
                });
            }
            ann.sanity = hits;

            // Invariant: ann.score == trace.compute() for finite plans.
            // Skip masked plans (entry_score = NEG_INFINITY) — sanity_adjust_plans
            // leaves them unmutated, and NEG_INFINITY corner cases can produce NaN.
            if entry_scores[i].is_finite() {
                debug_assert!(
                    (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                    "P3a.6 invariant violated: plan[{i}] ann.score={} vs compute()={}",
                    ann.score,
                    ann.score_trace.compute(),
                );
            }
        }
    }
}

// ── Orchestrator ───────────────────────────────────────────────────────────

/// Apply residual sanity multipliers to `scores` in place and return a
/// per-plan breakdown of which rules fired and with what multiplier.
///
/// The outer `Vec` is parallel to `plans`/`scores`. Each inner `Vec` lists
/// the `SanityHit`s for that plan in the order they fired; an empty inner
/// vec means no rules fired for that plan and the score was unchanged.
///
/// **Early-return case (`scores.len() <= 1`):** returns a `Vec` of empty
/// inner vecs sized to `scores.len()` (0 or 1 entries). The single-plan
/// edge case does not run any rule, so the breakdown is empty — but the
/// outer length still matches `scores.len()` so callers can index it safely
/// without a special-case check.
pub fn sanity_adjust_plans(
    scores: &mut [f32],
    plans: &[TurnPlan],
    ctx: &ScoringCtx,
) -> Vec<Vec<SanityHit>> {
    // Pre-allocate one empty inner vec per plan.
    let mut breakdown: Vec<Vec<SanityHit>> = (0..scores.len()).map(|_| Vec::new()).collect();

    if scores.len() <= 1 {
        return breakdown;
    }

    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, ctx.world.tuning, maps);
    let current_danger = maps.danger.get(active.pos);

    for (idx, (plan, score)) in plans.iter().zip(scores.iter_mut()).enumerate() {
        if !score.is_finite() {
            continue;
        }
        let mut penalty = 1.0f32;
        let final_pos = plan.final_pos;
        let hits = &mut breakdown[idx];

        // 1. Healer exposure: a non-healer abandoning the team's healer.
        for hit in healer_exposure::evaluate(active, final_pos, &allies) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        // 2. Retreat trap: final tile with fewer than 2 open neighbours
        // (flankable, no room to move next turn).
        if let Some(hit) = retreat_trap::evaluate(final_pos, &ally_positions) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        // 3. Synergy bonus: the plan repositions to a safer/better tile AND
        // includes a useful cast. Encourages retreat-and-help combos.
        if let Some(hit) = synergy_bonus::evaluate(
            active,
            plan,
            final_pos,
            current_danger,
            current_pos_eval,
            ctx,
        ) {
            penalty *= hit.multiplier;
            hits.push(hit);
        }

        *score *= penalty;
    }

    breakdown
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Sum of expected AoO damage the plan would take across all provoking
/// transitions. For each melee enemy with reactions and a damage estimate,
/// scan the plan's movement path for the first `was_adj && !still_adj`
/// transition (one AoO per enemy per round) and accrue its expected damage
/// against the actor's armor + vulnerability. Returns 0.0 if no provokers
/// are triggered — fast path for typical non-adjacent moves.
/// Visible to the adaptation layer — the `ExpectedSelfLethal` trigger
/// compares this against `active.hp`. Kept here because the
/// non-lethal multiplicative penalty (inside `sanity_adjust_plans`) uses
/// the same number.
pub(crate) fn expected_aoo_damage(
    active: &UnitSnapshot,
    plan: &TurnPlan,
    enemies: &[&UnitSnapshot],
) -> f32 {
    let mut total = 0.0f32;
    let mitigation = (active.armor + active.armor_bonus) as f32;
    let vuln = active.damage_taken_bonus as f32;
    for e in enemies {
        if e.reactions_left <= 0 {
            continue;
        }
        let Some(raw) = e.aoo_expected_damage else { continue };
        // Scan: does the path ever leave adjacency with this enemy?
        let mut prev = active.pos;
        let mut triggered = false;
        for step in &plan.steps {
            let PlanStep::Move { path } = step else { continue };
            for &h in path {
                if prev.unsigned_distance_to(e.pos) == 1
                    && h.unsigned_distance_to(e.pos) != 1
                {
                    triggered = true;
                    break;
                }
                prev = h;
            }
            if triggered {
                break;
            }
        }
        if triggered {
            total += final_damage_f32(raw, mitigation, vuln, /* pierces_armor */ false);
        }
    }
    total
}

pub(crate) fn plan_has_self_aoe(plan: &TurnPlan, ctx: &ScoringCtx) -> bool {
    let content = ctx.world.content;
    plan.walk_with_caster(ctx.active.pos).any(|(_, step, caster_pos)| {
        let PlanStep::Cast { ability, target_pos, .. } = step else { return false };
        let Some(def) = content.abilities.get(ability) else { return false };
        if !def.friendly_fire || def.aoe == AoEShape::None {
            return false;
        }
        // Route through the shared helper so a new `AoEShape` variant lands
        // here automatically — the inline match we used to have silently
        // missed anything beyond Circle/Line.
        aoe_area(def, *target_pos, caster_pos).contains(&caster_pos)
    })
}

// ── ProtectSelf mask ───────────────────────────────────────────────────────

/// A plan is **defensive** iff its `self_survival` factor is at or above
/// `epsilon`. The `self_survival` axis captures cumulative defensive value
/// across the plan (self-heal, armor-buff, and danger-exit), making the
/// threshold independent of step-level tile/target-type heuristics.
pub fn plan_is_defensive(self_survival: f32, epsilon: f32) -> bool {
    self_survival >= epsilon
}

/// Mask non-defensive plans to `-∞` under `ProtectSelf` intent — contract
/// enforcement. A plan opt-out from the ProtectSelf contract is expressed
/// via `EvaluationMode != Default` (set upstream in `apply_adaptation`
/// when the contract is globally unsatisfiable → `ProtectSelfNoDefensive`
/// switches every plan's mode to `LastStand`). Plans in non-Default mode
/// are left alone by this mask.
///
/// Returns true if at least one plan was observed to be defensive. The
/// "no defensive plan at all" case is now handled by ADAPTATION one step
/// upstream — by the time this function runs, that case has already
/// switched all plans to `LastStand` mode, so every plan will skip the
/// mask. The return value is retained for callers that want to observe
/// contract satisfiability, but no longer triggers a LastStand rescore
/// inside this function.
pub fn apply_protect_self_mask(
    scores: &mut [f32],
    raw: &[PlanFactorValues],
    modes: &[EvaluationMode],
    epsilon: f32,
) -> bool {
    debug_assert_eq!(raw.len(), modes.len());
    let mut any_defensive = false;
    for (i, f) in raw.iter().enumerate() {
        // Plans that adaptation moved to a non-Default mode have opted
        // out of the ProtectSelf contract; the mask does not apply to
        // them.
        if !matches!(modes.get(i), Some(EvaluationMode::Default)) {
            continue;
        }
        if plan_is_defensive(f.get_plan(PlanFactor::SelfSurvival), epsilon) {
            any_defensive = true;
        } else if i < scores.len() {
            scores[i] = f32::NEG_INFINITY;
        }
    }
    any_defensive
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::{IntentReason, TacticalIntent};
    use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
    use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
    use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use crate::core::DiceRng;

    fn make_move_plan(path: Vec<Hex>) -> TurnPlan {
        let final_pos = path.last().copied().unwrap_or_else(|| hex_from_offset(0, 0));
        TurnPlan {
            steps: vec![PlanStep::Move { path }],
            final_pos,
            ..TurnPlan::default()
        }
    }

    fn apply_sanity_to_two_plans(
        plans: Vec<TurnPlan>,
        scores: Vec<f32>,
        actor_hp: i32,
        actor_max_hp: i32,
        danger_on_final: Option<(Hex, f32)>,
    ) -> ScoredPool {
        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(actor_hp)
            .max_hp(actor_max_hp)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        if let Some((tile, val)) = danger_on_final {
            maps.danger.add(tile, val);
        }
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip(scores.into_iter()) {
            ann.score = score;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            // In production, FinalizeStage sets this; unit tests call the stage directly.
            ann.score_trace.base = score;
        }
        SanityStage.apply(&mut pool, &mut ctx);
        pool
    }

    // ── no hits on a clean plan ────────────────────────────────────────────

    #[test]
    fn sanity_stage_no_hits_leaves_annotation_empty() {
        // Two plans, full HP, no danger — no sanity rule fires.
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let pool = apply_sanity_to_two_plans(plans, vec![0.5, 0.4], 20, 20, None);

        for ann in &pool.annotations {
            assert!(
                ann.sanity.is_empty(),
                "expected no sanity hits for healthy actor in safe tile, got {:?}",
                ann.sanity,
            );
        }
    }

    // ── residual-only: low-HP actor on danger tile must not produce any hits ──
    // (Survival was migrated to critics in 10.1; SanityRule no longer has
    //  that variant after 10.4 cleanup.)

    #[test]
    fn sanity_stage_no_hits_for_low_hp_danger_tile() {
        // Low-HP actor on a danger tile: before step 10.1 this triggered
        // the Survival sanity rule. After 10.4, the enum no longer has
        // Survival — this test pins that the annotation stays empty.
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        // 2/20 HP = 10%; danger tile on destination of plan 0.
        let pool = apply_sanity_to_two_plans(
            plans, vec![0.5, 0.4], 2, 20, Some((dest_a, 1.0)),
        );

        // No sanity rule should fire — the only active rules are
        // HealerExposure, RetreatTrap, SynergyBonus, none of which
        // trigger in this solo-actor scenario.
        for ann in &pool.annotations {
            assert!(
                ann.sanity.is_empty(),
                "no sanity hits expected for low-HP actor in danger tile (Survival is now a critic), got {:?}",
                ann.sanity,
            );
        }
    }

    fn empty_content() -> crate::content::content_view::ContentView {
        crate::combat::ai::test_helpers::empty_content()
    }

    // ── sanity_survives_adaptation_path (B3 regression) ──────────────────
    //
    // Regression test for B3 fix (step 11.0): in the old pipeline order
    // SanityStage ran before FinalizeStage, which would rescore ann.score
    // from raw factors — wiping the Sanity multipliers. In the new order:
    //   ModeSelection → Finalize → Sanity → Critics → ...
    // Sanity runs AFTER Finalize, so its adjustments survive.
    //
    // This test runs: FinalizeStage (pre-populated adaptation) → SanityStage,
    // and verifies that the output of Sanity is a *modified version* of the
    // Finalize output (not the pre-finalize score).

    #[test]
    fn sanity_survives_adaptation_path() {
        use crate::combat::ai::factors::PlanFactorValues;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::planning::types::TurnPlan;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        // Two plans: one with LastStand adaptation injected, one Default.
        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let mut pool = ScoredPool::new(plans);
        let pre_scores = [0.8_f32, 0.6_f32];
        for (ann, (&score, adaptation)) in pool.annotations.iter_mut().zip(
            pre_scores.iter().zip([
                Some(AdaptationData {
                    reason: AdaptationReason::ProtectSelfNoDefensive,
                    original_score: 0.8,
                }),
                None,
            ])
        ) {
            ann.score = score;
            ann.factors = PlanFactorValues::default();
            ann.adaptation = adaptation;
        }

        // Run Finalize then Sanity (mirroring new pipeline order).
        FinalizeStage.apply(&mut pool, &mut ctx);
        let scores_after_finalize: Vec<f32> = pool.annotations.iter().map(|a| a.score).collect();
        SanityStage.apply(&mut pool, &mut ctx);

        // SanityStage either leaves scores unchanged (no rule fired) or
        // applies multipliers. Either way: scores must be ≤ finalized score
        // (sanity is non-additive, only penalty/bonus ≤1 or mild bonus).
        // The key invariant: scores after Sanity are based on finalized scores,
        // not on the raw pre_scores — meaning Finalize + Sanity compose correctly.
        for (i, ann) in pool.annotations.iter().enumerate() {
            // Sanity either preserves or reduces; cannot exceed finalized score
            // by more than a small bonus (SynergyBonus is +5% max).
            let finalized = scores_after_finalize[i];
            assert!(
                ann.score <= finalized * 1.1 + 1e-5,
                "plan[{i}]: sanity score {score} unexpectedly far above finalized {finalized}",
                score = ann.score,
                finalized = finalized,
            );
        }
    }

    // ── P3a.3 — ScoreTrace integration tests ─────────────────────────────────

    /// Clean plan (full HP, no danger, 2-plan pool): no rules fire.
    /// After apply: trace.base == entry_score, multipliers.len() == 0,
    /// trace.compute() == entry_score.
    #[test]
    fn p3a_sanity_no_hits_trace_has_only_base() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let entry_scores = vec![0.7_f32, 0.5_f32];
        let pool = apply_sanity_to_two_plans(plans, entry_scores.clone(), 20, 20, None);

        for (i, ann) in pool.annotations.iter().enumerate() {
            let entry = entry_scores[i];
            assert!(
                (ann.score_trace.base - entry).abs() < 1e-6,
                "plan[{i}]: trace.base must equal entry_score={entry}, got {}",
                ann.score_trace.base,
            );
            assert!(
                ann.score_trace.multipliers.is_empty(),
                "plan[{i}]: multipliers must be empty when no sanity rules fire, got {:?}",
                ann.score_trace.multipliers,
            );
            assert!(
                (ann.score_trace.compute() - entry).abs() < 1e-6,
                "plan[{i}]: trace.compute() must equal entry_score={entry}, got {}",
                ann.score_trace.compute(),
            );
        }
    }

    /// Reuse sanity_survives_adaptation_path setup to verify per-plan invariants
    /// introduced by P3a.3:
    ///   - multipliers.len() == sanity.len()  (1-to-1 mapping)
    ///   - each multiplier has kind=Sanity and value == sanity[j].multiplier
    ///   - compute() == ann.score (with ε=1e-5)
    ///
    /// Triggers may or may not fire (depends on rules); the invariants hold in
    /// both cases: no-hit → both vecs empty, hit → both vecs non-empty.
    #[test]
    fn p3a_sanity_with_hits_pushes_multipliers() {
        use crate::combat::ai::pipeline::score_trace::MultiplierKind;
        use crate::combat::ai::factors::PlanFactorValues;
        use crate::combat::ai::outcome::AdaptationData;
        use crate::combat::ai::adapt::AdaptationReason;
        use crate::combat::ai::pipeline::stages::finalize::FinalizeStage;
        use crate::combat::ai::pipeline::PlanStage;
        use crate::combat::ai::planning::types::TurnPlan;

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(10).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let plans = vec![TurnPlan::default(), TurnPlan::default()];
        let mut pool = ScoredPool::new(plans);
        for (ann, (&score, adaptation)) in pool.annotations.iter_mut().zip(
            [0.8_f32, 0.6_f32].iter().zip([
                Some(AdaptationData {
                    reason: AdaptationReason::ProtectSelfNoDefensive,
                    original_score: 0.8,
                }),
                None,
            ])
        ) {
            ann.score = score;
            ann.factors = PlanFactorValues::default();
            ann.adaptation = adaptation;
        }

        FinalizeStage.apply(&mut pool, &mut ctx);
        SanityStage.apply(&mut pool, &mut ctx);

        for (i, ann) in pool.annotations.iter().enumerate() {
            // 1-to-1: multipliers vec and sanity vec must have the same length.
            assert_eq!(
                ann.score_trace.multipliers.len(),
                ann.sanity.len(),
                "plan[{i}]: trace.multipliers.len()={} != sanity.len()={}",
                ann.score_trace.multipliers.len(),
                ann.sanity.len(),
            );
            // Each multiplier hit must have kind=Sanity and match the sanity value.
            for (j, (mhit, shit)) in ann
                .score_trace
                .multipliers
                .iter()
                .zip(ann.sanity.iter())
                .enumerate()
            {
                assert_eq!(
                    mhit.kind,
                    MultiplierKind::Sanity,
                    "plan[{i}] multiplier[{j}]: expected kind=Sanity",
                );
                assert!(
                    (mhit.value - shit.multiplier).abs() < 1e-6,
                    "plan[{i}] multiplier[{j}]: value={} != sanity.multiplier={}",
                    mhit.value,
                    shit.multiplier,
                );
            }
            // compute() == ann.score.
            assert!(
                (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                "plan[{i}]: compute()={} != ann.score={}",
                ann.score_trace.compute(),
                ann.score,
            );
        }
    }

    /// On a pool of 3 plans (all finite scores), after apply: for every plan
    /// (ann.score - trace.compute()).abs() < 1e-5.
    #[test]
    fn p3a_sanity_invariant_holds_for_all_finite_plans() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);
        let dest_c = hex_from_offset(3, 0);
        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
            make_move_plan(vec![dest_c]),
        ];

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(20)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let mut pool = ScoredPool::new(plans);
        for (ann, score) in pool.annotations.iter_mut().zip([0.9_f32, 0.7_f32, 0.5_f32]) {
            ann.score = score;
            // P3a.6: initialise trace.base so the stage runs without Finalize upstream.
            ann.score_trace.base = score;
        }
        SanityStage.apply(&mut pool, &mut ctx);

        for (i, ann) in pool.annotations.iter().enumerate() {
            assert!(
                ann.score.is_finite(),
                "plan[{i}]: score must remain finite, got {}", ann.score,
            );
            assert!(
                (ann.score - ann.score_trace.compute()).abs() < 1e-5,
                "plan[{i}]: invariant violated — ann.score={} vs compute()={}",
                ann.score,
                ann.score_trace.compute(),
            );
        }
    }

    /// A plan with ann.score = NEG_INFINITY (masked plan).
    /// After apply: ann.score stays NEG_INFINITY, invariant assert is skipped
    /// (entry.is_finite() == false), no panic. Sanity hits remain empty.
    /// P3a.6: trace.base is NOT set by this stage (Finalize would set it in
    /// production); in isolation the masked plan's trace.base stays as-is.
    #[test]
    fn p3a_sanity_masked_plan_trace_unchanged_or_only_base() {
        let dest_a = hex_from_offset(1, 0);
        let dest_b = hex_from_offset(2, 0);

        let pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(20)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let scoring = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            pos,
            &mut rng,
        );

        let plans = vec![
            make_move_plan(vec![dest_a]),
            make_move_plan(vec![dest_b]),
        ];
        let mut pool = ScoredPool::new(plans);
        // plan[0] is masked, plan[1] is normal.
        pool.annotations[0].score = f32::NEG_INFINITY;
        pool.annotations[1].score = 0.6;
        // P3a.6: initialise trace.base for the finite plan only.
        pool.annotations[1].score_trace.base = 0.6;

        // Must not panic.
        SanityStage.apply(&mut pool, &mut ctx);

        // plan[0]: score stays NEG_INFINITY;
        // sanity hits remain empty (sanity_adjust_plans skips non-finite).
        let masked = &pool.annotations[0];
        assert_eq!(
            masked.score,
            f32::NEG_INFINITY,
            "masked plan score must remain NEG_INFINITY",
        );
        assert!(
            masked.sanity.is_empty(),
            "masked plan must have no sanity hits, got {:?}", masked.sanity,
        );

        // plan[1]: finite plan invariant holds.
        let normal = &pool.annotations[1];
        assert!(
            normal.score.is_finite(),
            "normal plan score must remain finite",
        );
        assert!(
            (normal.score - normal.score_trace.compute()).abs() < 1e-5,
            "normal plan invariant violated: score={} vs compute()={}",
            normal.score,
            normal.score_trace.compute(),
        );
    }

    // ── planning/sanity.rs tests (migrated from planning layer) ──────────────

    /// Sanity-suite defaults: max_hp=30, speed=4, aoo=(5.0, 1 reaction).
    fn unit(id: u32, team: Team, pos: Hex, hp: i32) -> crate::combat::ai::world::snapshot::UnitSnapshot {
        UnitBuilder::new(id, team, pos)
            .hp(hp)
            .max_hp(30)
            .speed(4)
            .aoo(5.0, 1)
            .build()
    }

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // Even-r geometry reminder (verified empirically):
    //   Neighbors of (0,0): (1,0),(-1,0),(0,1),(0,-1),(1,1),(1,-1).
    //   (-1,0) is adjacent to (0,0) but NOT to (1,0) — "leaves adjacency".
    //   (1,1) is adjacent to BOTH (0,0) and (1,0) — "stays adjacent".

    /// Shape of the expected `expected_aoo_damage` result for a row.
    #[derive(Clone, Copy, Debug)]
    enum Aoo {
        /// Exactly zero — no provoker fired.
        Zero,
        /// Strictly positive — at least one provoker fired.
        Positive,
        /// Within an inclusive `[lo, hi]` band around a known EV.
        Near(f32, f32),
        /// Lethal — result must be ≥ the actor's own HP (pins the
        /// sanity-adjust mask precondition).
        AtLeastActorHp,
    }

    /// Table-driven AoO cases. Each row pins a distinct invariant; the
    /// `name` column is formatted into every failure message so a broken
    /// row stays as diagnostic as an individually-named test.
    #[test]
    fn expected_aoo_damage_matrix() {
        fn default_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            unit(2, Team::Player, hex_from_offset(1, 0), 20)
        }
        fn ranged_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            let mut e = default_enemy();
            e.max_attack_range = 5;
            e.aoo_expected_damage = None;
            e
        }
        fn hybrid_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            // Melee + ranged: max_attack_range>1 но melee-AoO есть.
            // Regression pin for the dropped `max_attack_range != 1` guard.
            let mut e = default_enemy();
            e.max_attack_range = 3;
            e
        }
        fn no_react_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            let mut e = default_enemy();
            e.reactions_left = 0;
            e
        }
        fn second_enemy() -> crate::combat::ai::world::snapshot::UnitSnapshot {
            unit(3, Team::Player, hex_from_offset(0, 1), 20)
        }

        struct Row {
            name: &'static str,
            actor_hp: i32,
            actor_armor: i32,
            enemies: Vec<crate::combat::ai::world::snapshot::UnitSnapshot>,
            path: Vec<Hex>,
            expected: Aoo,
        }
        let rows: Vec<Row> = vec![
            Row {
                name: "leaves adjacency provokes",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "stays adjacent does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(1, 1)],
                expected: Aoo::Zero,
            },
            Row {
                name: "ranged enemy does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![ranged_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                name: "hybrid melee+ranged provokes (regression: dropped max_range guard)",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![hybrid_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "enemy with 0 reactions does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![no_react_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                // Between two melees; (0,-1) leaves adjacency with both → 5 × 2.
                name: "multi-enemy damage sums",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy(), second_enemy()],
                path: vec![hex_from_offset(0, -1)],
                expected: Aoo::Near(9.5, 10.5),
            },
            Row {
                // Leaves → re-enters → leaves: one reaction per enemy, not three.
                name: "one AoO per enemy even with multiple transitions",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![
                    hex_from_offset(-1, 0),
                    hex_from_offset(0, 0),
                    hex_from_offset(-1, 0),
                ],
                expected: Aoo::Near(4.5, 5.5),
            },
            Row {
                // Armor 10 vs raw 5.0 — final_damage floors at 1.
                name: "armor clamps expected damage at the 1-HP floor",
                actor_hp: 20, actor_armor: 10,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Near(0.99, 1.01),
            },
            Row {
                // Precondition `expected_aoo_damage ≥ actor_hp` — the
                // input signal ADAPTATION uses to trigger
                // `ExpectedSelfLethal`. Sanity no longer reads this for
                // a hard mask (it stays in soft-bleed territory only),
                // but the helper itself must still report correctly so
                // adaptation can act on it.
                name: "expected-lethal AoO reaches actor HP threshold",
                actor_hp: 3, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::AtLeastActorHp,
            },
        ];

        for row in &rows {
            let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), row.actor_hp);
            actor.armor = row.actor_armor;
            let enemy_refs: Vec<&crate::combat::ai::world::snapshot::UnitSnapshot> = row.enemies.iter().collect();
            let plan = move_plan(row.path.clone());
            let dmg = expected_aoo_damage(&actor, &plan, &enemy_refs);
            let name = row.name;
            match row.expected {
                Aoo::Zero => assert_eq!(dmg, 0.0, "[{name}] expected 0, got {dmg}"),
                Aoo::Positive => assert!(dmg > 0.0, "[{name}] expected > 0, got {dmg}"),
                Aoo::Near(lo, hi) => assert!(
                    (lo..=hi).contains(&dmg),
                    "[{name}] expected in [{lo}, {hi}], got {dmg}",
                ),
                Aoo::AtLeastActorHp => assert!(
                    dmg >= row.actor_hp as f32,
                    "[{name}] expected ≥ hp({}), got {dmg}", row.actor_hp,
                ),
            }
        }
    }

    // ── plan_has_self_aoe: routed through shared `aoe_area` ─────────────
    //
    // Smoke test: a friendly-fire Circle AoE centred on the caster's tile
    // must be detected as self-AoE. The inline match that used to live here
    // covered Circle/Line only; `aoe_area` (shared with every other AoE
    // caller) automatically picks up new `AoEShape` variants, so adding
    // e.g. Cone later will be covered without a code change here.

    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr};

    fn fireball_def(radius: u32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("fireball"),
            name: "fireball".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius },
            friendly_fire: true,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    #[test]
    fn plan_has_self_aoe_detects_friendly_fire_circle_on_caster() {
        use crate::combat::ai::world::reservations::Reservations;
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};
        use crate::content::abilities::CasterContext;
        let actor_pos = hex_from_offset(0, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 20);
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = crate::game::components::Abilities(Vec::new());
        let mut content = empty_content();
        let def = fireball_def(1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let utility = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&utility, &snap, &maps, &reservations, &actor);

        // Single-cast fireball centred on `target_pos`, fired from `actor_pos`.
        let cast_fireball_at = |target_pos: Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("fireball"),
                target: crate::combat::ai::test_helpers::ent(99),
                target_pos,
            }],
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: vec![Default::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };

        assert!(
            plan_has_self_aoe(&cast_fireball_at(actor_pos), &ctx),
            "friendly-fire circle on caster tile must be flagged as self-AoE",
        );
        assert!(
            !plan_has_self_aoe(&cast_fireball_at(hex_from_offset(5, 5)), &ctx),
            "AoE centred far from the caster must not be flagged",
        );
    }

    /// Pin that `sanity_adjust_plans` only fires the three residual rules
    /// (`HealerExposure`, `RetreatTrap`, `SynergyBonus`) and never the four
    /// variants that were migrated to critics in steps 10.1–10.2.
    /// Uses the same scenario as the pre-10.1 test: low-HP actor in a
    /// danger tile, adjacent to an AoO-capable enemy.
    #[test]
    fn sanity_only_fires_residual_rules() {
        use crate::combat::ai::world::reservations::Reservations;
        use crate::combat::ai::world::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx};

        let actor_pos = hex_from_offset(3, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 5); // low HP (was survival trigger)
        let enemy_pos = hex_from_offset(4, 0);
        let enemy = unit(2, Team::Player, enemy_pos, 20); // AoO-capable (was bleed trigger)

        let content = empty_content();
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let mut maps = empty_maps();
        let dest = hex_from_offset(2, 0);
        maps.danger.add(dest, 0.9);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let noop_plan = TurnPlan {
            steps: Vec::new(),
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let aoo_plan = move_plan(vec![dest]);

        let plans = vec![noop_plan, aoo_plan];
        let mut scores = vec![1.0f32, 1.0f32];
        let breakdown = sanity_adjust_plans(&mut scores, &plans, &ctx);

        assert_eq!(breakdown.len(), 2);

        // Only residual rules are valid; the enum no longer has Survival /
        // AoOBleed / SelfAoe / LosBlindspot so the compiler ensures no such
        // hits can appear.
        for hits in &breakdown {
            for h in hits {
                assert!(
                    matches!(h.rule, SanityRule::HealerExposure | SanityRule::RetreatTrap | SanityRule::SynergyBonus),
                    "unexpected rule in sanity breakdown: {:?}", h.rule,
                );
            }
        }
    }
}
