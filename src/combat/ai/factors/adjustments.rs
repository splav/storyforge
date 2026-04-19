//! Reservation-based coordination adjustments + crit-fail expected-value.

use super::{OffensiveFactors, ScoredStep};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::BattleSnapshot;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::AbilityDef;
use crate::content::races::CritFailEffect;
use crate::core::ResourceKind;

/// Absolute penalty applied to `position` when another unit already reserved
/// the tile we'd end on. Subtractive — multiplicative scaling is wrong on a
/// **signed** factor: `pos *= 0.5` on a negative position (already-bad tile)
/// moves the value closer to zero, making the reserved tile look *better*
/// than the same tile unreserved. The constant matches the old multiplicative
/// effect at `position ≈ 1.0` (where `×0.5` subtracted 0.5) while staying
/// correct across the sign boundary.
const RESERVED_TILE_PENALTY: f32 = 0.5;

/// Coordination knob: overkill penalty + focus-fire bonus + duplicate-CC + tile collision.
pub(super) fn apply_reservation_adjustments(
    step: &ScoredStep,
    off: &mut OffensiveFactors,
    focus: &mut f32,
    position: &mut f32,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
    reservations: &Reservations,
) {
    if let Some(target_ent) = step.target() {
        let reserved_dmg = reservations.reserved_damage(target_ent);
        if reserved_dmg > 0.0 {
            if let Some(target_unit) = snap.unit(target_ent) {
                let hp_left = target_unit.hp as f32 - reserved_dmg;
                if hp_left <= 0.0 {
                    // Team-mates already reserved lethal damage. Our hit is
                    // waste (apart from a crit-fail hedge). Scale damage AND
                    // kill together — previously `kill = 0.0` was absolute
                    // while damage leaked `mult` through, leaving overkill
                    // plans attractive whenever raw damage was high.
                    let mult = ctx.world.difficulty.overkill_multiplier();
                    off.damage *= mult;
                    off.kill *= mult;
                } else {
                    *focus *= 1.0 + ctx.world.difficulty.focus_fire_bonus();
                }
            }
        }
        if reservations.has_reserved_cc(target_ent) {
            off.cc *= 0.15;
        }
    }
    if reservations.is_tile_reserved(step.caster_tile()) {
        *position -= RESERVED_TILE_PENALTY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::utility::{ActorCtx, AiWorld, UtilityContext};
    use crate::content::abilities::CasterContext;
    use crate::content::content_view::ContentView;
    use crate::content::races::CritFailEffect;
    use crate::game::components::Abilities;
    use crate::game::hex::hex_from_offset;

    fn make_ctx<'a>(
        content: &'a ContentView,
        difficulty: &'a DifficultyProfile,
        caster: &'a CasterContext,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        UtilityContext {
            world: AiWorld { content, difficulty },
            actor: ActorCtx {
                caster,
                abilities,
                crit_fail_effect: CritFailEffect::Miss,
                crit_fail_chance: 0.0,
            },
        }
    }

    /// Regression: reserved-tile penalty must always push `position` down,
    /// regardless of sign. Old code did `*= 0.5`, which flipped the effect on
    /// negative positions — a bad tile's reservation made it look better.
    #[test]
    fn reserved_tile_penalises_both_signs() {
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(Vec::new());
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let tile = hex_from_offset(3, 3);
        let step = ScoredStep::Move { caster_tile: tile };
        let snap = BattleSnapshot { units: Vec::new(), round: 1 };

        let mut reservations = Reservations::default();
        reservations.reserve_tile(tile);

        // Positive position: penalty must lower it.
        let mut off = OffensiveFactors::default();
        let mut focus = 0.0;
        let mut position = 1.0f32;
        apply_reservation_adjustments(&step, &mut off, &mut focus, &mut position, &snap, &ctx, &reservations);
        assert!(position < 1.0, "positive position must be reduced, got {position}");

        // Negative position: penalty must make it MORE negative, not less.
        let mut position = -0.5f32;
        apply_reservation_adjustments(&step, &mut off, &mut focus, &mut position, &snap, &ctx, &reservations);
        assert!(
            position < -0.5,
            "negative position must be pushed further from zero, got {position}",
        );
    }

    /// Overkill penalty must scale `damage` AND `kill` together. Previously
    /// `kill` was absolute-zeroed while `damage` retained the difficulty
    /// multiplier's share — leaving overkill plans attractive whenever raw
    /// damage was high.
    #[test]
    fn overkill_scales_damage_and_kill_uniformly() {
        use crate::combat::ai::role::{AiRole, AxisProfile};
        use crate::combat::ai::snapshot::{AiTags, UnitSnapshot};
        use crate::game::components::Team;
        use bevy::prelude::Entity;
        use crate::core::AbilityId;

        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(Vec::new());
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);
        let mult = difficulty.overkill_multiplier();
        assert!(mult > 0.0 && mult < 1.0, "precondition: non-trivial multiplier");

        let target_ent = Entity::from_raw_u32(99).expect("valid");
        let target = UnitSnapshot {
            entity: target_ent,
            team: Team::Player,
            role: AxisProfile::from(AiRole::Bruiser),
            pos: hex_from_offset(1, 0),
            hp: 5,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 1,
            max_ap: 1,
            movement_points: 0,
            speed: 0,
            mana: None,
            rage: None,
            energy: None,
            abilities: Vec::new(),
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        };
        let snap = BattleSnapshot { units: vec![target.clone()], round: 1 };
        let mut reservations = Reservations::default();
        // Reserve 10 HP of incoming damage against a 5-HP target — lethal.
        reservations.reserve_damage(target_ent, 10.0);

        let ability = AbilityId::from("melee_attack");
        let step = ScoredStep::Cast {
            ability: &ability,
            target: target_ent,
            target_pos: target.pos,
            caster_tile: hex_from_offset(0, 0),
        };
        let mut off = OffensiveFactors {
            damage: 8.0,
            heal: 0.0,
            kill: 1.0,
            cc: 0.0,
        };
        let mut focus = 0.7;
        let mut position = 0.0;
        apply_reservation_adjustments(&step, &mut off, &mut focus, &mut position, &snap, &ctx, &reservations);

        assert!(
            (off.damage - 8.0 * mult).abs() < 1e-5,
            "damage must be scaled by overkill multiplier, got {}",
            off.damage,
        );
        assert!(
            (off.kill - mult).abs() < 1e-5,
            "kill must be scaled by the SAME multiplier (not zeroed), got {}",
            off.kill,
        );
    }

    /// Without a reservation on the tile, position is untouched regardless
    /// of sign — only the reservation triggers the penalty.
    #[test]
    fn unreserved_tile_leaves_position_untouched() {
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(Vec::new());
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let step = ScoredStep::Move { caster_tile: hex_from_offset(0, 0) };
        let snap = BattleSnapshot { units: Vec::new(), round: 1 };
        let reservations = Reservations::default();

        let mut off = OffensiveFactors::default();
        let mut focus = 0.0;
        let mut position = -0.5f32;
        apply_reservation_adjustments(&step, &mut off, &mut focus, &mut position, &snap, &ctx, &reservations);
        assert_eq!(position, -0.5);
    }
}

pub fn crit_fail_adjusted(
    score: f32,
    def: &AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
    match effect {
        CritFailEffect::ManaOverload => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score - chance * mana_cost
        }
        CritFailEffect::CircuitBreach => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score * (1.0 - chance) - chance * mana_cost * 0.5
        }
        _ => score * (1.0 - chance),
    }
}
