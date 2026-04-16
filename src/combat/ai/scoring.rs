use crate::combat::ai::difficulty::DifficultyProfile;
use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, TargetType};
use crate::game::components::Abilities;
use crate::game::resources::GameDb;
use bevy::prelude::Entity;

/// Snapshot of a potential target with status-derived bonuses and threat estimate.
pub struct TargetInfo {
    pub entity: Entity,
    pub pos: hexx::Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    /// Max expected damage this unit deals per turn (for stun/kill scoring).
    pub threat: f32,
}

/// Score a single (ability, target) pair in HP-equivalent units.
/// Higher = more desirable. Returns 0.0 for options that should be skipped.
pub fn score_action(
    def: &AbilityDef,
    target: &TargetInfo,
    ctx: &CasterContext,
    db: &GameDb,
    profile: &DifficultyProfile,
) -> f32 {
    let Some(calc) = def.effect.calc(ctx) else {
        return if matches!(def.effect, EffectDef::GrantMovement { .. }) {
            0.0
        } else {
            status_score(def, target, db, profile)
        };
    };

    let expected = calc.expected();

    let dmg_score = if calc.is_heal {
        let missing = (target.max_hp - target.hp) as f32;
        if missing <= 0.0 {
            return 0.0;
        }
        let effective = expected.min(missing);
        let hp_pct = target.hp as f32 / target.max_hp.max(1) as f32;
        let urgency = if hp_pct < profile.heal_urgency_threshold {
            profile.heal_urgency_multiplier
        } else {
            1.0
        };
        effective * urgency
    } else {
        let mitigation = if calc.pierces_armor {
            0.0
        } else {
            (target.armor + target.armor_bonus) as f32 * profile.armor_awareness
        };
        let dmg = (expected - mitigation + target.damage_taken_bonus as f32).max(1.0);
        kill_adjusted(dmg, target, profile)
    };

    dmg_score + status_score(def, target, db, profile)
}

/// Estimate the maximum expected single-action damage output for a unit.
/// Used to value stuns and kills: stunning a high-threat target is worth more.
pub fn estimate_threat(ctx: &CasterContext, abilities: &Abilities, db: &GameDb) -> f32 {
    abilities
        .0
        .iter()
        .filter_map(|id| db.abilities.get(id))
        .filter(|def| matches!(def.target_type, TargetType::SingleEnemy))
        .filter_map(|def| def.effect.calc(ctx))
        .map(|calc| calc.expected().max(1.0))
        .fold(0.0f32, f32::max)
}

// ── Internals ──────────────────────────────────────────────────────────────

fn kill_adjusted(expected_dmg: f32, target: &TargetInfo, profile: &DifficultyProfile) -> f32 {
    if expected_dmg >= target.hp as f32 {
        expected_dmg * profile.kill_multiplier
    } else {
        expected_dmg
    }
}

fn status_score(
    def: &AbilityDef,
    target: &TargetInfo,
    db: &GameDb,
    profile: &DifficultyProfile,
) -> f32 {
    let mut total = 0.0f32;
    for sa in &def.statuses {
        let Some(sd) = db.statuses.get(&sa.status) else {
            continue;
        };
        let d = sa.duration_rounds as f32;
        if sd.skips_turn {
            total += target.threat * d;
        }
        if sd.damage_taken_bonus > 0 {
            total += sd.damage_taken_bonus as f32 * d;
        }
        if sd.armor_bonus > 0 {
            total += sd.armor_bonus as f32 * d;
        }
    }
    total * profile.status_value_scale
}
