use super::AppraisalCtx;

pub(super) fn compute_self_preserve(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let memory = ctx.memory;
    let tuning = ctx.tuning;
    let hp_pct = active.hp_pct();
    let urgency_hp = tuning.curves.self_preserve_hp.eval(1.0 - hp_pct);

    let recent_damage_taken = memory
        .hp_ratio_at_last_turn
        .map(|prev| (prev - hp_pct).max(0.0))
        .unwrap_or(0.0);
    let dmg_mult_raw = 1.0 + tuning.curves.self_preserve_dmg_alpha * recent_damage_taken;

    // Dampen urgency when the unit was already defensive last turn and
    // no fresh damage came in — prevents re-triggering ProtectSelf every
    // turn when the actor is simply "sitting low but unthreatened".
    let dmg_mult = if memory.last_turn_was_defensive && recent_damage_taken < 0.05 {
        dmg_mult_raw * 0.5
    } else {
        dmg_mult_raw
    };

    (urgency_hp * dmg_mult).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::tests::{default_memory, make_ctx, snap};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::memory::AiMemory;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn self_preserve_zero_at_full_hp() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .full_hp(20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_self_preserve(&ctx);
        // Logistic at (1 - 1.0) = 0.0 is well below 0.05.
        assert!(signal < 0.05, "expected near 0 at full HP, got {signal}");
    }

    #[test]
    fn self_preserve_high_at_low_hp() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(4)
            .max_hp(20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_self_preserve(&ctx);
        // hp_pct = 0.2, urgency_hp should be high.
        assert!(signal > 0.7, "expected > 0.7 at 20% HP, got {signal}");
    }

    #[test]
    fn self_preserve_amplified_by_recent_damage() {
        let hp_pct_now = 0.5;
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(10)
            .max_hp(20)
            .build();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();

        let memory_dmg = AiMemory {
            hp_ratio_at_last_turn: Some(0.9),
            ..default_memory()
        };
        let ctx_dmg = make_ctx(&active, &s, &memory_dmg, &tuning, &maps, &content, &at, &st);
        let signal_with_damage = compute_self_preserve(&ctx_dmg);

        let memory_no_damage = default_memory();
        let ctx_no = make_ctx(
            &active,
            &s,
            &memory_no_damage,
            &tuning,
            &maps,
            &content,
            &at,
            &st,
        );
        let signal_no_damage = compute_self_preserve(&ctx_no);

        assert!(
            signal_with_damage > signal_no_damage,
            "damage history ({:.3}) should amplify self_preserve vs baseline ({:.3}), hp_pct_now={hp_pct_now}",
            signal_with_damage,
            signal_no_damage,
        );
    }

    #[test]
    fn self_preserve_dampened_after_defensive() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(10)
            .max_hp(20)
            .build();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();

        let memory_defensive = AiMemory {
            last_turn_was_defensive: true,
            hp_ratio_at_last_turn: Some(0.5), // same HP → no fresh damage
            ..default_memory()
        };
        let memory_normal = AiMemory {
            last_turn_was_defensive: false,
            hp_ratio_at_last_turn: Some(0.5),
            ..default_memory()
        };
        let ctx_def = make_ctx(
            &active,
            &s,
            &memory_defensive,
            &tuning,
            &maps,
            &content,
            &at,
            &st,
        );
        let ctx_nor = make_ctx(
            &active,
            &s,
            &memory_normal,
            &tuning,
            &maps,
            &content,
            &at,
            &st,
        );
        let signal_defensive = compute_self_preserve(&ctx_def);
        let signal_normal = compute_self_preserve(&ctx_nor);

        assert!(
            signal_defensive < signal_normal,
            "defensive flag should dampen self_preserve ({:.3}) vs normal ({:.3})",
            signal_defensive,
            signal_normal,
        );
    }
}
