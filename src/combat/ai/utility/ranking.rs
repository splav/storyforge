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

        let new_intent = if midpanic {
            self.intent_reason = IntentReason::MidpanicFallback {
                hp_pct,
                midpanic_hp,
                danger: actor_danger,
                panic_danger,
                max_align,
                threshold,
            };
            Some(TacticalIntent::ProtectSelf)
        } else {
            let exclude = match &self.intent {
                TacticalIntent::FocusTarget { target } => Some(*target),
                _ => None,
            };
            let from_kind = self.intent.kind();
            default_focus_target(ctx.active, ctx.snap, plans, actor_pos, exclude).map(|t| {
                self.intent_reason = IntentReason::ViabilityFallback {
                    from: from_kind,
                    max_align,
                    threshold,
                };
                TacticalIntent::FocusTarget { target: t }
            })
        };

        if let Some(new) = new_intent {
            if self.intent.kind() != new.kind() || self.intent.target() != new.target() {
                self.intent = new;
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
