use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::world::tags::AbilityTag;
use super::AppraisalCtx;

pub(super) fn compute_rescue_ally(ctx: &AppraisalCtx<'_>) -> f32 {
    // Gate: actor has any ability with Rescue tag in effective kit.
    // `ctx.content.abilities.get(id)` existence check ensures the ability is
    // a known content entry; tag lookup is cache-only (no def access needed).
    let has_rescue_kit = ctx.active.cache.abilities.iter().any(|id| {
        ctx.content.abilities.contains_key(id)
            && ctx.ability_tags.effective(id).contains_tag(AbilityTag::Rescue)
    });
    if !has_rescue_kit {
        return 0.0;
    }

    let actor_entity = ctx.active.entity();
    // Find most-endangered ally within reach budget.
    let reach = (ctx.active.speed.max(0) as u32).saturating_add(ctx.active.cache.max_attack_range);
    let best_danger: f32 = ctx.snap.allies_of(ctx.active.team)
        .filter(|a| a.entity() != actor_entity)
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
pub(crate) fn ally_threat_proxy(ally: UnitView<'_>, snap: &BattleSnapshot) -> f32 {
    snap.enemies_of(ally.team)
        .filter(|e| e.pos.unsigned_distance_to(ally.pos) <= e.cache.max_attack_range)
        .map(|e| crate::combat::ai::scoring::horizon_avg(e))
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

    #[test]
    fn rescue_ally_ignores_dead_allies_in_reach() {
        // REGRESSION: до Phase D Pass 3 best_danger итерировался по snap.units без
        // alive-фильтра, и трупы союзников с hp_pct=0 → hp_low=1.0 ложно
        // триггерили rescue need signal. Проверяем, что мёртвые ally не увеличивают
        // сигнал по сравнению с baseline (нет живых союзников).
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        // Живой высокоDPR-враг рядом, чтобы было бы что считать через ally_threat_proxy.
        let enemy = UnitBuilder::new(4, Team::Player, hex_from_offset(4, 3))
            .full_hp(20).threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let (content, at, st) = content_with_rescue_ability();
        let maps = empty_maps();

        // baseline: нет союзников вообще
        let s_no_allies = snap(vec![active.clone(), enemy.clone()]);
        let ctx_no_allies = make_ctx(&active, &s_no_allies, &memory, &tuning, &maps, &content, &at, &st);
        let baseline = compute_rescue_ally(&ctx_no_allies);

        // test: два мёртвых союзника в reach — не должны поднять сигнал выше baseline
        let dead_ally_a = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .hp(0).max_hp(20).build();
        let dead_ally_b = UnitBuilder::new(3, Team::Enemy, hex_from_offset(2, 3))
            .hp(0).max_hp(20).build();
        let s_with_dead = snap(vec![active.clone(), dead_ally_a, dead_ally_b, enemy]);
        let ctx_with_dead = make_ctx(&active, &s_with_dead, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx_with_dead);

        assert_eq!(signal, baseline,
            "мёртвые ally в reach = отсутствие живых ally (нет вклада в best_danger); \
             baseline={baseline}, with_dead={signal}");
    }

    #[test]
    fn ally_threat_proxy_ignores_dead_enemies() {
        // REGRESSION: до Phase D Pass 3 ally_threat_proxy итерировался по snap.units
        // без alive-фильтра, и мёртвые враги с непустыми damage_horizon (из cache)
        // ложно поднимали "угрозу союзнику". Проверяем, что мёртвый враг рядом с
        // живым союзником не поднимает rescue-сигнал выше baseline (нет врагов).
        let actor_pos = hex_from_offset(3, 3);
        let ally_pos = hex_from_offset(4, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ability_names(&["heal"])
            .max_attack_range(3)
            .speed(3)
            .build();
        // Живой союзник на низком HP → есть кого спасать.
        let ally = UnitBuilder::new(2, Team::Enemy, ally_pos)
            .hp(4).max_hp(20).build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let (content, at, st) = content_with_rescue_ability();
        let maps = empty_maps();

        // baseline: живой ally, НЕТ врагов → threat_to_ally = 0
        let s_no_enemies = snap(vec![active.clone(), ally.clone()]);
        let ctx_no_enemies = make_ctx(&active, &s_no_enemies, &memory, &tuning, &maps, &content, &at, &st);
        let baseline = compute_rescue_ally(&ctx_no_enemies);

        // test: тот же ally + один мёртвый враг рядом → threat не должен вырасти
        let dead_enemy = UnitBuilder::new(3, Team::Player, ally_pos)
            .hp(0).max_hp(20).threat(8.0).damage_horizon(vec![8.0]).max_attack_range(1).build();
        let s_with_dead = snap(vec![active.clone(), ally, dead_enemy]);
        let ctx_with_dead = make_ctx(&active, &s_with_dead, &memory, &tuning, &maps, &content, &at, &st);
        let signal = compute_rescue_ally(&ctx_with_dead);

        assert_eq!(signal, baseline,
            "live low-HP ally + мёртвый враг рядом = live low-HP ally + нет врагов \
             (мёртвый не добавляет threat); baseline={baseline}, with_dead={signal}");
    }
}
