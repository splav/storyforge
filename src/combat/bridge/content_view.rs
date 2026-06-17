//! `EcsContentView` — ECS-backed engine `ActiveContentData` adapter.

use bevy::prelude::*;

use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::game::components::Equipment;

use combat_engine::content::ContentView as EngineContentView;
use combat_engine::dice::DiceExpr as EngineDiceExpr;
use combat_engine::modifier;

// ── process_action_system ─────────────────────────────────────────────────────

/// ECS-backed `ActiveContentData` adapter for `process_action_system`.
///
/// Carries only static content (active_content); per-combat state (caster
/// contexts, auras, AoO dice, phase triggers) lives on engine `Unit` fields
/// and is populated once at combat init by `from_ecs` / `bootstrap_combat_state`.
pub struct EcsContentView<'a> {
    active_content: &'a ActiveContent,
}

impl<'a> EngineContentView for EcsContentView<'a> {
    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
        self.active_content.abilities.get(id).map(|a| &a.engine)
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> {
        self.active_content.statuses.get(id).map(|s| &s.engine)
    }

    fn unit_template(&self, id: &str) -> Option<combat_engine::UnitTemplate> {
        let tpl = self.active_content.unit_templates.get(id)?;
        Some(build_engine_template_from_def(tpl, self.active_content))
    }
}

/// Build a fully-populated engine `UnitTemplate` from a bridge `UnitTemplateDef`.
///
/// Mirrors `bootstrap_combat_state`'s caster_context/aoo_dice logic but from
/// content alone (no ECS queries), so summon `Effect::Spawn` gets correct stats.
/// `auras`/`enemy_phases` stay empty — those are encounter-level, not template.
fn build_engine_template_from_def(
    tpl: &crate::content::unit_templates::UnitTemplateDef,
    active_content: &ActiveContent,
) -> combat_engine::UnitTemplate {
    let equipment = Equipment {
        main_hand: Some(tpl.equipment.main_hand.clone()),
        off_hand: tpl.equipment.off_hand.clone(),
        chest: tpl.equipment.chest.clone(),
        legs: tpl.equipment.legs.clone(),
        feet: tpl.equipment.feet.clone(),
    };
    let effective = active_content.effective_stats(&tpl.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);

    let bevy_ctx = CasterContext::new(&tpl.stats, Some(&equipment), &active_content.weapons);
    // crit_fail_outcome: from the unit's combat path, default Miss.
    let crit_fail_effect = tpl
        .path
        .as_deref()
        .and_then(|p| active_content.paths.get(p))
        .map_or(crate::content::races::CritFailEffect::Miss, |p| {
            p.crit_fail_effect.clone()
        });
    let engine_ctx = combat_engine::CasterContext {
        str_mod: bevy_ctx.str_mod,
        int_mod: bevy_ctx.int_mod,
        spell_power: bevy_ctx.spell_power,
        weapon_dice: bevy_ctx.weapon_dice,
        ranged_dice: bevy_ctx.ranged_dice,
        crit_fail_outcome: crate::content::to_engine::crit_fail_outcome(&crit_fail_effect),
        dex_mod: modifier(tpl.stats.dexterity),
    };

    // AoO dice: unit needs a melee WeaponAttack ability (range.max == 1) + weapon dice.
    let has_melee = tpl.ability_ids.iter().any(|aid| {
        active_content.abilities.get(aid).is_some_and(|def| {
            matches!(def.effect, EffectDef::WeaponAttack { ranged: false, .. })
                && def.range.max == 1
        })
    });
    let aoo_dice = if has_melee {
        bevy_ctx.weapon_dice.map(|core_dice| {
            EngineDiceExpr::new(
                core_dice.count,
                core_dice.sides,
                core_dice.bonus + combat_engine::modifier(tpl.stats.strength),
            )
        })
    } else {
        None
    };

    combat_engine::UnitTemplate {
        max_hp: effective.max_hp,
        armor,
        magic_resist: 0, // bridge-spawned summons carry no magic resist by default
        base_speed: tpl.speed,
        max_ap: 1, // templates carry no max_ap; matches CombatantBundle hardcoded default
        mana_max: tpl.resources.mana_max,
        energy_max: tpl.resources.energy_max,
        rage_max: tpl.resources.rage_max,
        caster_context: engine_ctx,
        aoo_dice,
        auras: Vec::new(),
        enemy_phases: Vec::new(),
        regen_per_pool: combat_engine::enum_map::enum_map! {
            // Hp has no turn-start regen in gameplay.
            combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
        initial_statuses: tpl
            .initial_statuses
            .iter()
            .map(|s| combat_engine::StatusId::from(s.as_str()))
            .collect(),
        initial_pools: {
            let map = &tpl.initial_pools;
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => map.get("hp").copied(),
                combat_engine::PoolKind::Mana   => map.get("mana").copied(),
                combat_engine::PoolKind::Rage   => map.get("rage").copied(),
                combat_engine::PoolKind::Energy => map.get("energy").copied(),
                combat_engine::PoolKind::Ap     => map.get("ap").copied(),
                combat_engine::PoolKind::Mp     => map.get("mp").copied(),
            }
        },
        tags: Default::default(),
    }
}

/// Build `EcsContentView` (wraps `ActiveContent` only; per-combat state lives on
/// engine `Unit` fields, populated at init by `from_ecs`).
///
/// Called from `bootstrap_combat_state`, `process_action_system`,
/// `advance_turn_system` (dead-actor DoT ticks), and `replay_engine_trace`
/// (content view from layered content without the full ECS).
pub fn build_ecs_content_view<'a>(content: &'a ActiveContent) -> EcsContentView<'a> {
    EcsContentView {
        active_content: content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ActiveContentData;
    use combat_engine::content::ContentView as EngineContentView;
    use combat_engine::StatusId;

    /// Regression for "провокация не даёт прирост брони": `status_bonuses` was a
    /// stub returning `StatusBonuses::default()` (always 0), so RefreshAggregates
    /// silently dropped armor bonuses while `forces_targeting` (read via
    /// `status_def`) still worked. Asserts the real `defending` (armor_bonus=4)
    /// now reports correctly.
    #[test]
    fn ecs_content_view_status_bonuses_reads_real_armor_bonus() {
        let active = ActiveContent(ActiveContentData::load_global_for_tests());
        let view = build_ecs_content_view(&active);

        let defending = view.status_bonuses(&StatusId::from("defending"));
        assert_eq!(
            defending.runtime.0.armor, 4,
            "defending must report armor (via runtime.0.armor)=4 from statuses.toml, not the stub default",
        );

        // Sanity: a status without armor stays at 0 (no false positives).
        let taunted = view.status_bonuses(&StatusId::from("taunted"));
        assert_eq!(taunted.runtime.0.armor, 0);
        assert_eq!(taunted.runtime.0.base_speed, 0);

        // Sanity: unknown status id falls back to default.
        let unknown = view.status_bonuses(&StatusId::from("__nonexistent__"));
        assert_eq!(unknown.runtime.0.armor, 0);
        assert_eq!(unknown.runtime.0.base_speed, 0);
    }

    /// End-to-end sanity: after `Effect::ApplyStatus(defending)` runs through
    /// the same `EcsContentView` path that production uses, the target unit's
    /// `armor_bonus` aggregate reflects the status (was 0 under the stub).
    #[test]
    fn refresh_aggregates_via_ecs_content_view_picks_up_defending_armor() {
        use combat_engine::effect::{apply_effect, Effect};
        use combat_engine::state::{CombatState, RoundPhase, Team, Unit, UnitId};
        use hexx::Hex;

        let active = ActiveContent(ActiveContentData::load_global_for_tests());
        let view = build_ecs_content_view(&active);

        let unit = Unit::new(
            UnitId(1),
            Team::Player,
            Hex::ZERO,
            combat_engine::RuntimeStats {
                armor: 3,
                magic_resist: 0,
                base_speed: 3,
            },
            combat_engine::RuntimeStatsDelta::default(),
            1, // reactions_left
            1, // reactions_max
            Vec::new(),
            None,
            None, // initiative: not yet rolled
            combat_engine::CasterContext::default(),
            None,
            Vec::new(),
            Vec::new(),
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => Some((20, 20)),
                combat_engine::PoolKind::Mana   => None,
                combat_engine::PoolKind::Rage   => None,
                combat_engine::PoolKind::Energy => None,
                combat_engine::PoolKind::Ap     => Some((1, 1)),
                combat_engine::PoolKind::Mp     => Some((3, 3)),
            },
            combat_engine::enum_map::enum_map! {
                combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
                combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
                combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
                combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
            },
            None,
        );
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);

        // Mirror the production path: ApplyStatus derives RefreshAggregates.
        let (derived, _) = apply_effect(
            &mut state,
            &Effect::ApplyStatus {
                target: UnitId(1),
                status: StatusId::from("defending"),
                rounds: 1,
                dot_per_tick: 0,
                applier: combat_engine::state::EffectSource::Unit(UnitId(1)),
            },
            &view,
        );
        // Process derived RefreshAggregates.
        for d in derived {
            apply_effect(&mut state, &d, &view);
        }

        let u = state.unit(UnitId(1)).unwrap();
        assert_eq!(
            u.runtime_bonus.0.armor, 4,
            "defending must contribute +4 runtime_bonus.armor"
        );
        // Effective armor = base armor + bonus = 3 + 4 = 7.
        assert_eq!(u.effective_armor(), 7);
    }
}
