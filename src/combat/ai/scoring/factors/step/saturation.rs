//! `StepFactor::Saturation` — buff-redundancy penalty for a Cast step.
//!
//! Returns -0.4 per buff_class already present on the target recipient.
//! Move steps and non-Cast steps return 0.0.
//!
//! # Contract on `ctx`
//! `ctx.snap` must be the **pre-step** snapshot. The caller (scorer's step loop)
//! shifts perspective via `ctx.with_perspective(&sim_actor, pre_snap)` before
//! calling this factor, so `ctx.snap` reflects the state before this step fires.

pub const NAME: &str = "saturation";
pub const SIGNED: bool = true;

use crate::combat::ai::appraisal::NeedSignals;
use crate::combat::ai::scoring::factors::ScoredStep;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::content::abilities::StatusOn;
use crate::content::content_view::ContentView;
use crate::core::AbilityId;
use bevy::prelude::Entity;

const BUFF_REDUNDANCY_PENALTY: f32 = -0.4;

pub fn compute(
    ctx: &ScoringCtx,
    step: &ScoredStep,
    _outcome: &ActionOutcomeEstimate,
    _needs: &NeedSignals,
) -> f32 {
    match step {
        ScoredStep::Cast { ability, target, .. } => {
            let caster = ctx.active.entity();
            let pre_snap = ctx.snap; // caller must have applied with_perspective
            buff_saturation_penalty(ability, *target, caster, pre_snap, ctx.world.content)
        }
        ScoredStep::Move { .. } => 0.0,
    }
}

/// Returns the saturation penalty for casting `ability` targeting `target`
/// (ability target entity; for `on = MySelf` statuses, use `caster` instead).
///
/// Penalty per buff_class already present on the recipient: `-0.4`.
/// Zero if the ability applies no buff-class statuses, or no recipient has a
/// matching buff class already.
fn buff_saturation_penalty(
    ability: &AbilityId,
    target: Entity,
    caster: Entity,
    pre_snap: &BattleSnapshot,
    content: &ContentView,
) -> f32 {
    let Some(def) = content.abilities.get(ability) else { return 0.0 };
    let mut penalty = 0.0f32;

    for sa in &def.statuses {
        let Some(sd) = content.statuses.get(&sa.status) else { continue };
        let Some(bc) = &sd.buff_class else { continue };

        // Determine actual recipient entity based on StatusOn.
        let recipient = match sa.on {
            StatusOn::Target => target,
            StatusOn::MySelf => caster,
        };

        // Check if recipient already has a status of the same buff_class.
        let already_has = pre_snap
            .unit(recipient)
            .map(|u| {
                u.statuses.iter().any(|s| {
                    content
                        .statuses
                        .get(&s.id)
                        .and_then(|sd2| sd2.buff_class.as_ref())
                        .map(|c| c == bc)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        if already_has {
            penalty += BUFF_REDUNDANCY_PENALTY;
        }
    }

    penalty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::world::snapshot::{ActiveStatusView, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, TargetType};
    use crate::content::content_view::ContentView;
    use crate::content::statuses::{BuffClass, StatusDef};
    use crate::core::{AbilityId, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;


    fn make_unit(id: u32) -> UnitSnapshot {
        crate::combat::ai::test_helpers::UnitBuilder::new(id, Team::Enemy, hex_from_offset(0, 0)).build()
    }

    fn snap_with(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        snapshot_from(units, 1)
    }

    fn armor_buff_status(id: &str) -> StatusDef {
        StatusDef {
            id: StatusId::from(id),
            name: id.into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: Some(BuffClass::ArmorBuff),
            engine: combat_engine::StatusDef {
                armor_bonus: 4,
                damage_taken_bonus: 0,
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                causes_disadvantage: false,
            },
        }
    }

    fn ability_applying(id: &str, status_id: &str, on: StatusOn) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: vec![],
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                effect: EffectDef::None,
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                costs: vec![],
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                status: StatusId::from(status_id),
                duration_rounds: 2,
                on,
            }],
                key: None,
            },
        }
    }

    fn content_with(ability: AbilityDef, status: StatusDef) -> ContentView {
        let mut c = ContentView::default();
        c.abilities.insert(ability.id.clone(), ability);
        c.statuses.insert(status.id.clone(), status);
        c
    }

    /// Redundant same buff_class → -0.4.
    #[test]
    fn redundant_buff_penalized() {
        let mut target = make_unit(1);
        target.statuses.push(ActiveStatusView {
            id: StatusId::from("defending"),
            rounds_remaining: 1,
            dot_per_tick: 0,
        });
        let caster = make_unit(2);
        let snap = snap_with(vec![target.clone(), caster.clone()]);
        let ability = ability_applying("buff_armor", "defending", StatusOn::Target);
        let status = armor_buff_status("defending");
        let content = content_with(ability, status);

        let penalty = buff_saturation_penalty(
            &AbilityId::from("buff_armor"),
            target.entity,
            caster.entity,
            &snap,
            &content,
        );
        assert_eq!(penalty, -0.4, "same buff_class already present → -0.4");
    }

    /// Different buff_class → 0.0.
    #[test]
    fn different_buff_class_not_penalized() {
        let mut target = make_unit(1);
        target.statuses.push(ActiveStatusView {
            id: StatusId::from("haste_buff"),
            rounds_remaining: 1,
            dot_per_tick: 0,
        });
        let caster = make_unit(2);
        let snap = snap_with(vec![target.clone(), caster.clone()]);

        let ability = ability_applying("buff_armor", "defending", StatusOn::Target);
        let mut content = ContentView::default();
        content.abilities.insert(ability.id.clone(), ability);
        content.statuses.insert(
            StatusId::from("defending"),
            armor_buff_status("defending"),
        );
        content.statuses.insert(
            StatusId::from("haste_buff"),
            StatusDef {
                id: StatusId::from("haste_buff"),
                name: "haste_buff".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: Some(BuffClass::Haste),
                engine: combat_engine::StatusDef {
                    armor_bonus: 0,
                    damage_taken_bonus: 0,
                    skips_turn: false,
                    forces_targeting: false,
                    blocks_mana_abilities: false,
                    speed_bonus: 1,
                    hp_percent_dot: 0,
                    causes_disadvantage: false,
                },
            },
        );

        let penalty = buff_saturation_penalty(
            &AbilityId::from("buff_armor"),
            target.entity,
            caster.entity,
            &snap,
            &content,
        );
        assert_eq!(penalty, 0.0, "different buff_class → no penalty");
    }

    /// StatusOn::MySelf: buff on caster, not target. Caster already has ArmorBuff → -0.4.
    #[test]
    fn self_buff_penalized_when_caster_has_class() {
        let target = make_unit(1);
        let mut caster = make_unit(2);
        caster.statuses.push(ActiveStatusView {
            id: StatusId::from("defending"),
            rounds_remaining: 1,
            dot_per_tick: 0,
        });
        let snap = snap_with(vec![target.clone(), caster.clone()]);
        let ability = ability_applying("self_shield", "defending", StatusOn::MySelf);
        let status = armor_buff_status("defending");
        let content = content_with(ability, status);

        let penalty = buff_saturation_penalty(
            &AbilityId::from("self_shield"),
            target.entity,
            caster.entity,
            &snap,
            &content,
        );
        assert_eq!(penalty, -0.4, "self-buff on caster who already has class → -0.4");
    }

    /// No status on target → no penalty even if ability applies tracked class.
    #[test]
    fn fresh_target_no_penalty() {
        let target = make_unit(1);
        let caster = make_unit(2);
        let snap = snap_with(vec![target.clone(), caster.clone()]);
        let ability = ability_applying("buff_armor", "defending", StatusOn::Target);
        let status = armor_buff_status("defending");
        let content = content_with(ability, status);

        let penalty = buff_saturation_penalty(
            &AbilityId::from("buff_armor"),
            target.entity,
            caster.entity,
            &snap,
            &content,
        );
        assert_eq!(penalty, 0.0, "fresh target → no penalty");
    }
}
