use super::AppraisalCtx;

pub(super) fn compute_reposition(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;
    let tuning = ctx.tuning;
    let has_ap = active.action_points >= 1;
    let cur_pos_eval = crate::combat::ai::scoring::position_eval::evaluate_position(
        active.pos, &active.role, tuning, maps,
    );

    // BFS over reachable tiles (movement_points budget) to find the best
    // position improvement. Uses the same reach helper as the planner so
    // passability / stop rules are consistent.
    let reach = crate::combat::ai::plan::reach::reach_from(snap, active);
    let best_position_improvement = reach
        .destinations
        .iter()
        .map(|&tile| {
            let pe = crate::combat::ai::scoring::position_eval::evaluate_position(
                tile, &active.role, tuning, maps,
            );
            (pe - cur_pos_eval).max(0.0)
        })
        .fold(0.0_f32, f32::max);

    let engagement_gap = snap
        .enemies_of(active.team)
        .all(|e| active.pos.unsigned_distance_to(e.pos) > active.max_attack_range);

    let mut reposition = tuning.curves.reposition_pos_gain.eval(best_position_improvement);

    // Idle AP boost: no enemies in attack range, we have AP, AND there is a
    // real positional improvement to take. Without the improvement gate, the
    // boost forced reposition to fire even when no useful tile existed —
    // post-step-3 mining (3.6) showed this drove Reposition to 15% chosen
    // intent (target 3–5%) and inflated viability_fallback (5.1% → 16.8%)
    // because intent fired without a viable Move plan to back it. Tying the
    // boost to `best_position_improvement >= reposition_pos_gain.x_lo` keeps
    // the idle nudge but only when the curve already says there is somewhere
    // worth going.
    if engagement_gap && has_ap && best_position_improvement >= 0.05 {
        reposition = reposition.max(0.5);
    }

    reposition
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, UnitBuilder};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::combat::ai::appraisal::tests::{default_memory, snap, make_ctx};

    #[test]
    fn reposition_high_when_engagement_gap_with_real_improvement() {
        // No enemies + has AP + a reachable tile with meaningful pos_eval gain
        // → idle boost ≥ 0.5. Map is built so a neighbouring tile reads as
        // strictly better via the opportunity influence channel (Tank role
        // weights opportunity at +0.9 — see tuning.tables.axis_position_weights).
        let actor_pos = hex_from_offset(3, 3);
        let better_tile = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .build();
        let tuning = AiTuning::default();
        let mut maps = empty_maps();
        maps.opportunity.add(better_tile, 1.0);
        let s = snap(vec![active.clone()]);
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
        assert!(signal >= 0.5, "idle AP boost should push reposition ≥ 0.5, got {signal}");
    }

    #[test]
    fn reposition_no_boost_when_engagement_gap_but_no_improvement() {
        // No enemies + has AP but flat map (no tile is better than current).
        // Idle boost is gated on real best_position_improvement, so signal
        // collapses to curve.eval(0) ≈ 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .build();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone()]);
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
        assert!(signal < 0.1, "no improvement → no boost, got {signal}");
    }

    #[test]
    fn reposition_zero_when_engaged_no_position_gain() {
        // Enemy is adjacent (within max_attack_range=1) → no engagement gap.
        // Maps are all zeros → no position improvement.
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(1)
            .speed(3)
            .max_attack_range(1)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone(), enemy]);
        let memory = default_memory();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_reposition(&ctx);
        // No engagement gap, no position gain → only curve eval(0) which is ≈ 0.
        assert!(signal < 0.1, "expected near 0 when engaged with no position gain, got {signal}");
    }
}
