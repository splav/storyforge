//! Plan ranking state + phase methods.
//!
//! `PlanRanking` owns the four values that co-evolve through `pick_action`'s
//! scoring pipeline: the current intent, its reason, the final score column,
//! and the raw-factor matrix. Each phase (`apply_viability`, `apply_sanity`,
//! `apply_protect_self`) takes `&mut self` and mutates them coherently —
//! replacing four loose `&mut` args per call and keeping the invariant
//! `scored.len() == raw_factors.len() == plans.len()` in one place.
//!
//! `pick_action` reads as a linear sequence of phase calls; each phase is
//! unit-testable without reconstructing the full pipeline.

use crate::combat::ai::factors::PlanFactors;
use crate::combat::ai::intent::{
    default_focus_target, intent_viability_threshold, IntentReason, TacticalIntent,
};
use crate::combat::ai::planning::{
    apply_protect_self_mask, pick_best_plan, rescore_with_intent, sanity_adjust_plans,
    score_plans_with_raw, PickMechanics, TurnPlan,
};
use crate::core::DiceRng;
use crate::game::hex::Hex;

use super::{AiWorld, ScoringCtx};

/// Co-evolving ranking state: intent (may be swapped mid-pipeline by
/// viability-fallback / last-stand), the explanation for the current intent,
/// and the per-plan scores + raw factors that drive ranking.
///
/// Invariant: `scored.len() == raw_factors.len() == plans.len()` — every
/// method preserves it.
pub struct PlanRanking {
    pub intent: TacticalIntent,
    pub intent_reason: IntentReason,
    pub scored: Vec<f32>,
    pub raw_factors: Vec<PlanFactors>,
}

impl PlanRanking {
    /// Score the plan pool under `intent` and build the initial ranking.
    pub fn initial(
        plans: &[TurnPlan],
        intent: TacticalIntent,
        intent_reason: IntentReason,
        ctx: &ScoringCtx,
    ) -> Self {
        let (scored, raw_factors) = score_plans_with_raw(plans, &intent, ctx);
        Self { intent, intent_reason, scored, raw_factors }
    }

    /// Intent viability guard. If no plan achieves the current intent's
    /// signal (max intent-factor < threshold), swap to a fallback:
    ///   - midpanic: HP below midpanic threshold AND standing in danger →
    ///     `ProtectSelf`. The actor can't execute the original intent *and*
    ///     is too exposed to blindly push toward a fallback focus target.
    ///   - default: reachable `FocusTarget` over a live enemy.
    ///
    /// Plan generation is intent-agnostic, so rescoring against the same
    /// pool is enough. Only factor[7] (the intent column) is rewritten by
    /// `rescore_with_intent`; the other eight columns stay intact.
    pub fn apply_viability(
        &mut self,
        plans: &[TurnPlan],
        actor_pos: Hex,
        ctx: &ScoringCtx,
    ) {
        let Some(threshold) = intent_viability_threshold(&self.intent) else { return };

        let max_align = self
            .raw_factors
            .iter()
            .map(|f| f.intent)
            .fold(f32::NEG_INFINITY, f32::max);
        if max_align >= threshold {
            return;
        }

        let hp_pct = ctx.active.hp_pct();
        let actor_danger = ctx.maps.danger.get(ctx.active.pos);
        let midpanic_hp = ctx.world.difficulty.midpanic_hp_threshold();
        let panic_danger = ctx.world.difficulty.awareness_danger_threshold();
        let midpanic = hp_pct < midpanic_hp && actor_danger > panic_danger;

        // Compute (candidate intent, candidate reason) without mutating self.
        // The mutation commits only after the kind/target guard passes below —
        // otherwise reason could drift away from intent when the guard blocks
        // the swap (e.g., fallback returns the same FocusTarget target).
        let candidate: Option<(TacticalIntent, IntentReason)> = if midpanic {
            Some((
                TacticalIntent::ProtectSelf,
                IntentReason::MidpanicFallback {
                    hp_pct,
                    midpanic_hp,
                    danger: actor_danger,
                    panic_danger,
                    max_align,
                    threshold,
                },
            ))
        } else {
            let exclude = match &self.intent {
                TacticalIntent::FocusTarget { target } => Some(*target),
                _ => None,
            };
            let from_kind = self.intent.kind();
            default_focus_target(ctx.active, ctx.snap, plans, actor_pos, exclude).map(|t| {
                (
                    TacticalIntent::FocusTarget { target: t },
                    IntentReason::ViabilityFallback {
                        from: from_kind,
                        max_align,
                        threshold,
                    },
                )
            })
        };

        if let Some((new_intent, new_reason)) = candidate {
            if self.intent.kind() != new_intent.kind()
                || self.intent.target() != new_intent.target()
            {
                self.intent = new_intent;
                self.intent_reason = new_reason;
                self.scored = rescore_with_intent(
                    plans, &mut self.raw_factors, &self.intent, ctx,
                );
            }
        }
    }

    /// Multiplicative penalties for situations the 9-factor score can't
    /// catch (low-HP through AoO corridors, self-AoE, LOS blindspots,
    /// retreat traps). Runs on all plans so low-ranked terrible ones can't
    /// sneak up via noise.
    pub fn apply_sanity(&mut self, plans: &[TurnPlan], ctx: &ScoringCtx) {
        sanity_adjust_plans(&mut self.scored, plans, ctx);
    }

    /// ProtectSelf mask. Mask any plan whose first step isn't defensive to
    /// -∞ — this is where the intent gets real teeth. Without it,
    /// "I want to protect myself" is just a +1.0 intent factor on a few
    /// candidates, easily out-scored by high-damage offensive plans.
    ///
    /// If no plan is defensive (surrounded, no safe move), rescore under
    /// `LastStand` so the actor at least lands a final useful hit; the
    /// reason is wrapped in `LastStandAfter { prior }` to preserve the
    /// explanation that led to ProtectSelf in the first place.
    ///
    /// Caller guards with `matches!(self.intent, ProtectSelf)`; calling
    /// this unconditionally is a no-op on non-ProtectSelf intents only
    /// incidentally (the mask would strip nothing), so the guard is load-
    /// bearing.
    pub fn apply_protect_self(&mut self, plans: &[TurnPlan], ctx: &ScoringCtx) {
        let margin = ctx.world.difficulty.defensive_tile_margin();
        let any_defensive = apply_protect_self_mask(
            &mut self.scored, plans, ctx.active, ctx.world.content, ctx.maps, margin,
        );
        if !any_defensive {
            // `intent` stays ProtectSelf — LastStand is only the rescore
            // lens so factor[7] reflects "last useful action" weighting;
            // debug/log still show the original ProtectSelf label with a
            // LastStandAfter wrapper in the reason chain.
            let last_stand = TacticalIntent::LastStand;
            self.scored = rescore_with_intent(
                plans, &mut self.raw_factors, &last_stand, ctx,
            );
            let prior = std::mem::replace(&mut self.intent_reason, IntentReason::NoRuleDefault);
            self.intent_reason = IntentReason::LastStandAfter { prior: Box::new(prior) };
        }
    }

    /// Final pick: mercy + top-K window. Returns the index into `plans` of
    /// the winning plan and the mechanics trace (top-K pool, similarity
    /// window, whether mercy reranked) for debug overlay.
    pub fn pick(&self, world: &AiWorld, rng: &mut DiceRng) -> (usize, PickMechanics) {
        pick_best_plan(&self.scored, &self.raw_factors, world, rng)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::intent::IntentKind;
    use crate::combat::ai::planning::PlanStep;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_test_ctx, UnitBuilder};
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Single-plan ranking at the given intent-factor value. Plans contain no
    /// steps, so any `rescore_with_intent` triggered by a fallback walks zero
    /// cast-steps → intent_sum stays 0, scores finalize to a stable batch.
    fn single_plan_ranking(
        intent: TacticalIntent,
        reason: IntentReason,
        intent_factor: f32,
    ) -> (Vec<TurnPlan>, PlanRanking) {
        let plan = TurnPlan {
            steps: Vec::new(),
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let factors = PlanFactors { intent: intent_factor, ..PlanFactors::default() };
        let ranking = PlanRanking {
            intent,
            intent_reason: reason,
            scored: vec![0.5],
            raw_factors: vec![factors],
        };
        (vec![plan], ranking)
    }

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        }
    }

    // ── apply_viability ────────────────────────────────────────────────────

    #[test]
    fn apply_viability_above_threshold_is_noop() {
        // Reposition threshold is 0.01. intent_factor=0.5 ≫ threshold → no
        // fallback path is taken; ranking stays untouched.
        let (plans, mut ranking) = single_plan_ranking(
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            0.5,
        );
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_viability(&plans, active.pos, &ctx);

        assert!(matches!(ranking.intent, TacticalIntent::Reposition));
        assert!(matches!(ranking.intent_reason, IntentReason::NoRuleDefault));
    }

    #[test]
    fn apply_viability_midpanic_swaps_to_protect_self() {
        // Low HP + high danger on the actor's tile. Intent factor 0.0 <
        // Reposition threshold 0.01 → fallback path enters midpanic branch.
        let (plans, mut ranking) = single_plan_ranking(
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            0.0,
        );
        let pos = hex_from_offset(0, 0);
        let active = UnitBuilder::new(1, Team::Enemy, pos)
            .hp(3)
            .max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(pos, 1.0);
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_viability(&plans, active.pos, &ctx);

        assert!(matches!(ranking.intent, TacticalIntent::ProtectSelf));
        assert!(
            matches!(ranking.intent_reason, IntentReason::MidpanicFallback { .. }),
            "expected MidpanicFallback, got {:?}", ranking.intent_reason,
        );
    }

    #[test]
    fn apply_viability_default_focus_switches_to_enemy() {
        // Healthy actor in safe tile; Reposition intent has zero alignment.
        // `default_focus_target` falls through to "any enemy by priority" and
        // returns the single live enemy.
        let (plans, mut ranking) = single_plan_ranking(
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            0.0,
        );
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).max_hp(20)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .hp(20).max_hp(20)
            .threat(3.0)
            .build();
        let enemy_id = enemy.entity;
        let snap = BattleSnapshot::new(vec![active.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_viability(&plans, active.pos, &ctx);

        match ranking.intent {
            TacticalIntent::FocusTarget { target } => assert_eq!(target, enemy_id),
            other => panic!("expected FocusTarget, got {:?}", other.kind()),
        }
        match ranking.intent_reason {
            IntentReason::ViabilityFallback { from, .. } => assert_eq!(from, IntentKind::Reposition),
            ref other => panic!("expected ViabilityFallback, got {:?}", other),
        }
    }

    #[test]
    fn apply_viability_no_enemies_keeps_intent() {
        // Low intent alignment but no live enemy for the fallback to pick —
        // ranking must stay put (no FocusTarget on a nonexistent target).
        let (plans, mut ranking) = single_plan_ranking(
            TacticalIntent::Reposition,
            IntentReason::NoRuleDefault,
            0.0,
        );
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).max_hp(20)
            .build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_viability(&plans, active.pos, &ctx);

        assert!(matches!(ranking.intent, TacticalIntent::Reposition));
        assert!(matches!(ranking.intent_reason, IntentReason::NoRuleDefault));
    }

    // ── apply_protect_self ─────────────────────────────────────────────────

    #[test]
    fn apply_protect_self_masks_non_defensive_and_preserves_reason() {
        // Two plans: empty-steps (defensive by convention) + move into
        // dangerous tile (non-defensive). Mask sends the second to -inf;
        // `any_defensive=true` so no LastStand rescore, reason untouched.
        let pos = hex_from_offset(0, 0);
        let bad = hex_from_offset(3, 0);
        let defensive = TurnPlan {
            steps: Vec::new(),
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let aggressive = move_plan(vec![bad]);
        let plans = vec![defensive, aggressive];
        let mut ranking = PlanRanking {
            intent: TacticalIntent::ProtectSelf,
            intent_reason: IntentReason::Urgency { hp_pct: 0.3, danger: 0.8 },
            scored: vec![0.5, 0.7],
            raw_factors: vec![PlanFactors::default(), PlanFactors::default()],
        };
        let active = UnitBuilder::new(1, Team::Enemy, pos).hp(5).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(bad, 2.0);
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_protect_self(&plans, &ctx);

        assert_eq!(ranking.scored[0], 0.5, "defensive plan score preserved");
        assert!(ranking.scored[1].is_infinite() && ranking.scored[1] < 0.0, "non-defensive masked to -inf");
        assert!(matches!(ranking.intent, TacticalIntent::ProtectSelf));
        assert!(
            matches!(ranking.intent_reason, IntentReason::Urgency { .. }),
            "reason untouched when defensive option exists",
        );
    }

    #[test]
    fn apply_protect_self_no_defensive_wraps_reason_in_last_stand() {
        // All plans non-defensive (cast at enemy ability that isn't
        // ally-targeted). Mask stores no `any_defensive=true` hit → rescore
        // under LastStand + wrap reason as LastStandAfter.
        let pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(1, 0);
        let aggressive = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: crate::combat::ai::test_helpers::ent(2),
                target_pos: enemy_pos,
            }],
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let plans = vec![aggressive];
        let prior_reason = IntentReason::Urgency { hp_pct: 0.2, danger: 0.9 };
        let mut ranking = PlanRanking {
            intent: TacticalIntent::ProtectSelf,
            intent_reason: prior_reason.clone(),
            scored: vec![0.7],
            raw_factors: vec![PlanFactors::default()],
        };
        let active = UnitBuilder::new(1, Team::Enemy, pos).hp(3).max_hp(20).build();
        let snap = BattleSnapshot::new(vec![active.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        // `apply_protect_self_mask` looks up the ability to decide defensive;
        // missing ability def returns `false` (non-defensive), which is what
        // this test wants — cheaper than configuring a real ability.
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = ScoringCtx { world: &world, maps: &maps, reservations: &reservations, snap: &snap, active: &active };

        ranking.apply_protect_self(&plans, &ctx);

        // Intent stays ProtectSelf — LastStand is only a rescore lens.
        assert!(matches!(ranking.intent, TacticalIntent::ProtectSelf));
        match ranking.intent_reason {
            IntentReason::LastStandAfter { prior } => {
                assert!(
                    matches!(*prior, IntentReason::Urgency { .. }),
                    "expected prior=Urgency, got {:?}", prior,
                );
            }
            ref other => panic!("expected LastStandAfter, got {:?}", other),
        }
    }
}
