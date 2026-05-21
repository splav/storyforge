use crate::combat::ai::intent::IntentKind;
use super::AppraisalCtx;

pub(super) fn compute_continue_commitment(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let snap = ctx.snap;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
    // Use an Option-returning closure so `?` works for early exits.
    let inner = || -> Option<f32> {
        // Only sticky for target-oriented intents.
        let kind = memory.last_intent?;
        if !matches!(kind, IntentKind::FocusTarget | IntentKind::ApplyCC) {
            return None;
        }
        let last_target_id = memory.last_target?;
        let last_target = snap.unit(last_target_id)?;

        // If target is already in the finisher zone, let finish_target take over.
        let last_target_hp = last_target.hp_pct();
        if last_target_hp <= 0.25 {
            return None;
        }

        // Reachability check: can we reach the target within speed + attack range?
        let reach_budget = (active.speed.max(0) as u32)
            .saturating_add(active.cache.max_attack_range);
        let dist = active.pos.unsigned_distance_to(last_target.pos);
        if dist > reach_budget {
            return None;
        }

        Some(tuning.curves.continue_commitment_hp.eval(last_target_hp))
    };

    inner().unwrap_or(0.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::memory::AiMemory;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, ent, UnitBuilder};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::combat::ai::appraisal::tests::{default_memory, snap, make_ctx};

    #[test]
    fn continue_commitment_zero_when_no_last_intent() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0);
    }

    #[test]
    fn continue_commitment_zero_when_target_not_in_snap() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(99)), // not in snap
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0);
    }

    #[test]
    fn continue_commitment_zero_when_target_low_hp() {
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_attack_range(2)
            .build();
        // Target at 20% HP → finisher zone.
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(2)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0, "finisher zone should return 0");
    }

    #[test]
    fn continue_commitment_zero_when_unreachable() {
        let actor_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(9, 9);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .speed(1)
            .max_attack_range(1)
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(8)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_continue_commitment(&ctx), 0.0, "unreachable target should return 0");
    }

    #[test]
    fn continue_commitment_high_when_alive_50pct_reachable() {
        // With default curve (Logistic { mid: 0.4, k: 10 }):
        //   eval(0.5) = 1/(1+exp(-10*(0.5-0.4))) ≈ 0.73.
        // Ascending logistic: high while target is healthy, drops near finisher zone.
        let actor_pos = hex_from_offset(3, 3);
        let target_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .max_attack_range(2)
            .speed(3)
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos)
            .hp(5)
            .max_hp(10)
            .build();
        let memory = AiMemory {
            last_intent: Some(IntentKind::FocusTarget),
            last_target: Some(ent(2)),
            ..default_memory()
        };
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), target]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_continue_commitment(&ctx);
        assert!(signal > 0.6, "should be > 0.6 for reachable 50% HP target, got {signal}");
    }
}
