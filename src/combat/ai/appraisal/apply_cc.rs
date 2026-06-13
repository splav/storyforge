use super::AppraisalCtx;
use crate::combat::ai::world::snapshot::UnitView;
use crate::combat::ai::world::tags::{AbilityTag, StatusTag, StatusTagCache};

pub(super) fn compute_apply_cc(ctx: &AppraisalCtx<'_>) -> f32 {
    // Gate: actor has any ability with ApplyCC tag in effective kit.
    let has_cc_kit = ctx.active.cache.abilities.iter().any(|id| {
        ctx.content.abilities.contains_key(id)
            && ctx
                .ability_tags
                .effective(id)
                .contains_tag(AbilityTag::ApplyCC)
    });
    if !has_cc_kit {
        return 0.0;
    }

    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.cache.max_attack_range);
    let best_threat: f32 = ctx
        .snap
        .enemies_of(ctx.active.team)
        .filter(|e| ctx.active.pos.unsigned_distance_to(e.pos) <= reach)
        .filter(|e| !target_already_hardcc(*e, ctx.status_tags))
        .map(|e| crate::combat::ai::scoring::horizon_avg(e))
        .fold(0.0_f32, f32::max);

    // LinearClamped — explicit DPR bounds [2, 10]; more robust than magic /10.
    ctx.tuning.curves.apply_cc.eval(best_threat)
}

/// Returns true if the unit already has a HardCC status applied.
fn target_already_hardcc(unit: UnitView<'_>, cache: &StatusTagCache) -> bool {
    unit.statuses
        .iter()
        .any(|st| cache.get(&st.id).contains_tag(StatusTag::HardCC))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::tests::{
        content_with_apply_cc_ability, minimal_ability_def_with_override,
    };
    use crate::combat::ai::appraisal::tests::{default_memory, make_ctx, snap};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, status_view, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    #[test]
    fn apply_cc_zero_when_no_cc_kit() {
        // Actor without stun-like ability → no ApplyCC tag → signal = 0.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .full_hp(20)
            .threat(5.0)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(
            compute_apply_cc(&ctx),
            0.0,
            "no ApplyCC kit → signal must be 0"
        );
    }

    #[test]
    fn apply_cc_zero_when_target_already_hardcc() {
        // Stun in kit, but the only enemy already has HardCC status → gate filters it → 0.
        use crate::combat::ai::world::tags::cache::build_caches;
        use crate::content::statuses::StatusDef;

        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(2)
            .build();

        // Build content with stun ability (ApplyCC) and a "stunned" status (HardCC).
        let mut content = empty_content();
        let mut ability_def = minimal_ability_def_with_override(&["apply_cc"]);
        ability_def.id = "stun".into();
        content.abilities.insert("stun".into(), ability_def);

        let status_def = StatusDef {
            id: "stunned".into(),
            name: "Stunned".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                skips_turn: true, // HardCC — derive_status_tags: skips_turn → HardCC
                bonuses: combat_engine::StatusBonuses::default(),
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        };
        content.statuses.insert("stunned".into(), status_def);

        let (st, at) = build_caches(&content);

        // Enemy already has the stunned status applied.
        let mut enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20)
            .threat(8.0)
            .damage_horizon(vec![8.0])
            .build();
        enemy.statuses.push(status_view("stunned", 1, 0));

        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_apply_cc(&ctx);
        assert!(
            signal < 0.05,
            "enemy already HardCC → filtered → signal near 0, got {signal}"
        );
    }

    #[test]
    fn apply_cc_high_when_unstunned_threat_in_reach() {
        // Actor has ApplyCC kit; enemy is a high-DPR unstunned threat in reach.
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(2)
            .speed(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20)
            .threat(9.0)
            .damage_horizon(vec![9.0])
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_apply_cc_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_apply_cc(&ctx);
        assert!(
            signal > 0.5,
            "unstunned high-DPR enemy in reach → signal > 0.5, got {signal}"
        );
    }

    #[test]
    fn apply_cc_zero_when_no_enemies_in_reach() {
        // Actor has ApplyCC kit but enemies are too far away.
        let actor_pos = hex_from_offset(0, 0);
        let enemy_pos = hex_from_offset(9, 9);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(1)
            .speed(1)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos)
            .full_hp(20)
            .threat(9.0)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let (content, at, st) = content_with_apply_cc_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(
            compute_apply_cc(&ctx),
            0.0,
            "enemies out of reach → signal = 0"
        );
    }

    #[test]
    fn apply_cc_zero_when_only_dead_enemies_in_reach() {
        // REGRESSION: до Phase D Pass 3 best_threat итерировался по snap.units без
        // alive-фильтра, и мёртвые враги с непустыми damage_horizon из cache ложно
        // стакали ApplyCC need signal. Проверяем, что трупы не дают CC-сигнала.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["stun"])
            .max_attack_range(2)
            .speed(3)
            .build();
        // Два мёртвых врага в reach с непустыми damage_horizon (как было бы в реальном логе).
        let dead_a = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .hp(0)
            .max_hp(20)
            .threat(9.0)
            .damage_horizon(vec![9.0])
            .build();
        let dead_b = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 3))
            .hp(0)
            .max_hp(20)
            .threat(7.0)
            .damage_horizon(vec![7.0])
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let s = snap(vec![active.clone(), dead_a, dead_b]);
        let maps = empty_maps();
        let (content, at, st) = content_with_apply_cc_ability();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        assert_eq!(
            compute_apply_cc(&ctx),
            0.0,
            "только мёртвые враги в reach → no CC signal (труп не нужно стакать)"
        );
    }
}
