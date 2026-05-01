use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::world::tags::AbilityTag;
use super::AppraisalCtx;

pub(super) fn compute_rescue_ally(ctx: &AppraisalCtx<'_>) -> f32 {
    // Gate: actor has any ability with Rescue tag in effective kit.
    // `ctx.content.abilities.get(id)` existence check ensures the ability is
    // a known content entry; tag lookup is cache-only (no def access needed).
    let has_rescue_kit = ctx.active.abilities.iter().any(|id| {
        ctx.content.abilities.contains_key(id)
            && ctx.ability_tags.effective(id).contains_tag(AbilityTag::Rescue)
    });
    if !has_rescue_kit {
        return 0.0;
    }

    // Find most-endangered ally within reach budget.
    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.max_attack_range);
    let best_danger: f32 = ctx.snap.units.iter()
        .filter(|a| a.team == ctx.active.team && a.entity != ctx.active.entity)
        .filter(|a| ctx.active.pos.unsigned_distance_to(a.pos) <= reach)
        .map(|a| {
            let hp_low = (1.0 - a.hp_pct()).clamp(0.0, 1.0);
            let threat_to_ally = ally_threat_proxy(a, ctx.snap);
            hp_low * threat_to_ally
        })
        .fold(0.0_f32, f32::max);

    ctx.tuning.curves.rescue_ally.eval(best_danger)
}

/// Estimate the threat level to `ally` from nearby enemies: max DPR among
/// enemies in attack range of the ally, normalised to ≈ [0, 1] by dividing by
/// 10 (mid-game DPR ceiling). Reuses `scoring::horizon_avg` for consistency
/// with the scoring layer.
pub(crate) fn ally_threat_proxy(ally: &UnitSnapshot, snap: &BattleSnapshot) -> f32 {
    snap.units.iter()
        .filter(|e| e.team != ally.team)
        .filter(|e| e.pos.unsigned_distance_to(ally.pos) <= e.max_attack_range)
        .map(crate::combat::ai::scoring::horizon_avg)
        .fold(0.0_f32, f32::max)
        / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, UnitBuilder};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use crate::combat::ai::appraisal::tests::{default_memory, snap, make_ctx};
    use crate::combat::ai::appraisal::tests::{content_with_rescue_ability, minimal_ability_def_with_override};

    #[test]
    fn rescue_ally_zero_when_no_rescue_kit() {
        // Actor has no abilities → no Rescue tag → signal = 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos).full_hp(20).build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .hp(4).max_hp(20).build(); // 20% HP — in danger
        let enemy = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 3))
            .threat(8.0).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_rescue_ally(&ctx), 0.0, "no Rescue kit → signal must be 0");
    }

    #[test]
    fn rescue_ally_zero_when_no_allies_in_danger() {
        // Actor has Rescue kit, but ally is at full HP and no enemies threatening.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .full_hp(20).build(); // full HP — not in danger
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally]);
        let maps = empty_maps();
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal < 0.05, "full-HP ally → signal near 0, got {signal}");
    }

    #[test]
    fn rescue_ally_high_when_ally_low_hp_threatened() {
        // Actor has Rescue kit; ally is at 20% HP and an enemy is adjacent.
        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let enemy_pos = hex_from_offset(4, 3); // same tile — adjacent to ally
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build(); // 20% HP
        // Enemy adjacent to ally with high DPR.
        let enemy = UnitBuilder::new(3, Team::Player, enemy_pos)
            .threat(8.0)
            .damage_horizon(vec![8.0])
            .max_attack_range(1)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal > 0.6, "low HP ally + high-DPR adjacent enemy → signal > 0.6, got {signal}");
    }

    #[test]
    fn rescue_ally_uses_override_for_kit_check() {
        // Ability with override ["rescue"] — must pass Rescue gate even though
        // underlying EffectDef is not a heal.
        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        // Ally at low HP so signal is non-zero when gate passes.
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build();
        let enemy = UnitBuilder::new(3, Team::Player, ally_pos)
            .threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();
        // content_with_rescue_ability uses ai_tags_override → must work.
        let (content, at, st) = content_with_rescue_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx);
        assert!(signal > 0.0, "override rescue tag → gate passes, signal > 0, got {signal}");
    }

    #[test]
    fn rescue_ally_zero_when_override_empties_kit() {
        // Ability with override Some([]) → replace-not-append semantics → no tags → gate fails.
        use crate::combat::ai::world::tags::cache::build_caches;

        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build();
        let enemy = UnitBuilder::new(3, Team::Player, ally_pos)
            .threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), ally, enemy]);
        let maps = empty_maps();

        // "heal" ability with empty override → tags = AbilityTagSet::empty().
        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&[]); // empty override → no tags
        def.id = "heal".into();
        content.abilities.insert("heal".into(), def);
        let (st, at) = build_caches(&content);
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(compute_rescue_ally(&ctx), 0.0, "empty override → gate fails → signal = 0");
    }
}
