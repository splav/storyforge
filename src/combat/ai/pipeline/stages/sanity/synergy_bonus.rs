//! Rule 3: plan repositions to a safer/better tile AND includes a useful cast.
//!
//! Encourages retreat-and-help combos. Multiplicative so it does not flip sign.

use crate::combat::ai::pipeline::stages::sanity::{SanityHit, SanityRule};
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::game::hex::Hex;

/// Evaluate the SynergyBonus rule for one plan.
///
/// Returns `Some(SanityHit { rule: SynergyBonus, multiplier: 1.1 })` when the
/// plan moves to a safer or positionally better tile **and** includes a useful
/// cast. Returns `None` otherwise (including when the plan does not reposition).
pub(super) fn evaluate(
    active: &UnitSnapshot,
    plan: &TurnPlan,
    final_pos: Hex,
    current_danger: f32,
    current_pos_eval: f32,
    ctx: &ScoringCtx,
) -> Option<SanityHit> {
    use crate::combat::ai::scoring::position_eval::evaluate_position;

    if final_pos == active.pos {
        return None;
    }
    let safer_tile = ctx.maps.danger.get(final_pos) + 0.05 < current_danger;
    let better_pos =
        evaluate_position(final_pos, &active.role, ctx.world.tuning, ctx.maps) > current_pos_eval;
    if (safer_tile || better_pos) && plan_has_useful_cast(plan, ctx) {
        Some(SanityHit {
            rule: SanityRule::SynergyBonus,
            multiplier: 1.1,
        })
    } else {
        None
    }
}

fn plan_has_useful_cast(plan: &TurnPlan, ctx: &ScoringCtx) -> bool {
    let content = ctx.world.content;
    let caster = &ctx.active.caster_ctx;
    plan.steps.iter().any(|s| {
        if let PlanStep::Cast { ability, .. } = s {
            content.abilities.get(ability).is_some_and(|def| {
                def.effect.calc(caster).is_some() || !def.statuses.is_empty()
            })
        } else {
            false
        }
    })
}
