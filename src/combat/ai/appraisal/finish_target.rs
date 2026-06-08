use super::AppraisalCtx;
use combat_engine::PoolKind;

pub(super) fn compute_finish_target(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
    let reach_budget = (active.speed.max(0) as u32).saturating_add(active.cache.max_attack_range);

    // Best killability metric among reachable killable enemies.
    // None means no killable target exists → signal stays 0.
    let killable_low_hp: Option<f32> = snap
        .enemies_of(active.team)
        .filter(|_| active.pools[PoolKind::Ap].map(|(c, _)| c).unwrap_or(0) > 0)
        .filter(|e| active.cache.threat >= e.eff_hp() as f32)
        .filter(|e| active.pos.unsigned_distance_to(e.pos) <= reach_budget)
        .map(|e| 1.0 - e.hp_pct())
        .reduce(f32::max);

    // No killable candidate → strictly 0 regardless of curve baseline.
    let Some(best_damage_pct) = killable_low_hp else {
        return 0.0;
    };

    let mut finish_target = tuning.curves.finish_target_kill.eval(best_damage_pct);

    // Bonus if the last-committed target is killable and has taken damage
    // (eases the handoff from continue_commitment → finish_target).
    if let Some(last_id) = memory.last_target {
        if let Some(last) = snap.unit(last_id) {
            // Heuristic for "we dealt damage to this target" — without a shared
            // team blackboard (step 13) we use 1 - hp_pct as a proxy.
            let target_damage_proxy = 1.0 - last.hp_pct();
            if target_damage_proxy > 0.1 && active.cache.threat >= last.eff_hp() as f32 {
                finish_target = (finish_target + 0.2).min(1.0);
            }
        }
    }

    finish_target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::tests::{default_memory, make_ctx, snap};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn finish_target_zero_when_no_killable() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        // Actor threat (5.0) < enemy eff_hp (50) → not killable.
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .threat(5.0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .full_hp(50)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_finish_target(&ctx), 0.0);
    }

    #[test]
    fn finish_target_high_when_killable_low_hp() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        // Enemy at 20% HP (2 of 10), threat > eff_hp.
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .threat(15.0)
            .max_attack_range(2)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_finish_target(&ctx);
        assert!(
            signal > 0.7,
            "expected > 0.7 for killable low-HP enemy, got {signal}"
        );
    }

    #[test]
    fn finish_target_zero_when_actor_no_ap() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(0)
            .threat(15.0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(
            compute_finish_target(&ctx),
            0.0,
            "no AP should yield 0 (filter blocks killable iter)"
        );
    }
}
