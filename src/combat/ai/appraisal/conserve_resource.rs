use super::AppraisalCtx;
use combat_engine::PoolKind;

pub(super) fn compute_conserve_resource(ctx: &AppraisalCtx<'_>) -> f32 {
    let active = ctx.active;
    let tuning = ctx.tuning;
    // mana pool is Option<(current, max)>; units without a mana bar have no
    // resource pressure (ratio = 1.0 → low signal on the descending logistic).
    let mana_ratio = match active.pools[PoolKind::Mana] {
        Some((current, max)) if max > 0 => current as f32 / max as f32,
        _ => 1.0,
    };

    tuning.curves.conserve_resource.eval(mana_ratio)
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
    fn conserve_resource_high_at_low_mana() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .mana(1, 10) // 10% mana
            .build();
        let tuning = AiTuning::default();
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(signal > 0.6, "expected > 0.6 at 10% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_low_at_full_mana() {
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .mana(19, 20) // 95% mana
            .build();
        let tuning = AiTuning::default();
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(signal < 0.1, "expected < 0.1 at 95% mana, got {signal}");
    }

    #[test]
    fn conserve_resource_no_pressure_when_no_mana_bar() {
        // No mana field → ratio = 1.0 → logistic(k<0) gives near 0.
        let active = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).build();
        let tuning = AiTuning::default();
        let memory = default_memory();
        let s = snap(vec![active.clone()]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_conserve_resource(&ctx);
        assert!(
            signal < 0.1,
            "expected near 0 when no mana bar, got {signal}"
        );
    }
}
