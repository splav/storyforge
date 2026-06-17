//! BuffIntoVoid critic: penalises a status cast onto a target that already
//! carries that status — either active at plan start, or applied by an earlier
//! step in the same plan ("buff into void").
//!
//! Returns one `CriticHit` for the first wasted cast; multiplier **0.6**.

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use bevy::prelude::Entity;
use combat_engine::StatusId;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Score multiplier when a buff/status cast is wasted on a target that already
/// has the effect active. Moderate penalty — not a fatal mistake.
const BUFF_INTO_VOID_MULTIPLIER: f32 = 0.6;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — no configuration; all logic comes from the snapshot and plan.
pub struct BuffIntoVoid;

impl PlanCritic for BuffIntoVoid {
    fn name(&self) -> &'static str {
        "buff_into_void"
    }

    fn evaluate(&self, plan: &TurnPlan, ctx: &ScoringCtx) -> Option<CriticHit> {
        // Track statuses applied by earlier plan steps, keyed by target Entity.
        // Entity is stable identity even if the target moves between steps —
        // a Hex-keyed proxy would miss intra-plan re-buff after target movement.
        let mut plan_applied: Vec<(Entity, StatusId)> = Vec::new();

        for step in &plan.steps {
            let PlanStep::Cast {
                ability, target, ..
            } = step
            else {
                continue;
            };

            let Some(def) = ctx.world.content.abilities.get(ability) else {
                continue;
            };

            if def.statuses.is_empty() {
                continue;
            }

            let Some(target_unit) = ctx.snap.unit(*target) else {
                for sa in &def.statuses {
                    plan_applied.push((*target, sa.status.clone()));
                }
                continue;
            };

            for sa in &def.statuses {
                // 1. Already active on the target from the original snapshot?
                let already_on_unit = target_unit.statuses.iter().any(|s| s.id == sa.status);

                // 2. Applied earlier in this plan to the same target entity?
                let applied_by_plan = plan_applied
                    .iter()
                    .any(|(ent, sid)| *ent == *target && *sid == sa.status);

                if already_on_unit || applied_by_plan {
                    return Some(CriticHit {
                        critic: CriticKind::BuffIntoVoid,
                        multiplier: BUFF_INTO_VOID_MULTIPLIER,
                        reason: CriticReason::BuffIntoVoid {
                            ability: ability.to_string(),
                            target_already_buffed: already_on_unit,
                        },
                    });
                }
            }

            for sa in &def.statuses {
                plan_applied.push((*target, sa.status.clone()));
            }
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, CriticReason};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::test_helpers::{
        assert_critic_fires, assert_critic_passes, run_critic, status_view, CriticScenarioBuilder,
        UnitBuilder,
    };
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn, TargetType,
    };
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;
    use combat_engine::{AbilityId, StatusId};

    /// Build a simple status-applying ability def.
    fn buff_ability(id: &str, status_id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleAlly,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::None,
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                    status: StatusId::from(status_id),
                    on: StatusOn::Target,
                    duration_rounds: 2,
                }],
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        }
    }

    fn cast_plan(
        ability: &str,
        target_entity: Entity,
        target_pos: crate::game::hex::Hex,
    ) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from(ability),
                target: target_entity,
                target_pos,
            }],
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default()],
            ..TurnPlan::default()
        }
    }

    // ── fires on canonical case (target already has status) ───────────────────

    #[test]
    fn buff_into_void_fires_on_canonical_case() {
        // Target already has "shield" status active → critic must fire.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let mut target = UnitBuilder::new(2, Team::Enemy, target_pos).build();
        target.statuses = vec![status_view("shield", 2, 0)];

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
            .build();
        let plan = cast_plan("buff_shield", target_entity, target_pos);

        assert_critic_fires(
            &BuffIntoVoid,
            &plan,
            &scn,
            CriticKind::BuffIntoVoid,
            BUFF_INTO_VOID_MULTIPLIER,
            |reason| {
                let CriticReason::BuffIntoVoid {
                    target_already_buffed,
                    ..
                } = reason
                else {
                    panic!("expected BuffIntoVoid reason, got {reason:?}");
                };
                assert!(*target_already_buffed, "target_already_buffed must be true");
            },
        );
    }

    // ── passes on clean plan (target has no status) ───────────────────────────

    #[test]
    fn buff_into_void_passes_on_clean_plan() {
        // Target has no statuses — applying "shield" is useful.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
            .build();
        let plan = cast_plan("buff_shield", target_entity, target_pos);

        assert_critic_passes(&BuffIntoVoid, &plan, &scn);
    }

    // ── fires on redundant cast within plan (second step duplicates first) ────

    #[test]
    fn buff_into_void_severity_scales_with_input() {
        // Single cast with no existing status → None.
        // Double cast of same status to same target → Some (redundant within plan).
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
            .build();

        let single_plan = cast_plan("buff_shield", target_entity, target_pos);
        let double_plan = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: AbilityId::from("buff_shield"),
                    target: target_entity,
                    target_pos,
                },
                PlanStep::Cast {
                    ability: AbilityId::from("buff_shield"),
                    target: target_entity,
                    target_pos,
                },
            ],
            final_pos: caster_pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default(), Default::default()],
            ..TurnPlan::default()
        };
        // Single cast — must not fire.
        assert_critic_passes(&BuffIntoVoid, &single_plan, &scn);

        // Double cast — second step is redundant, must fire.
        let hit = run_critic(&BuffIntoVoid, &double_plan, &scn)
            .expect("second cast of same status must trigger buff_into_void critic");
        let CriticReason::BuffIntoVoid {
            target_already_buffed,
            ..
        } = hit.reason
        else {
            panic!("expected BuffIntoVoid reason");
        };
        assert!(
            !target_already_buffed,
            "target_already_buffed must be false (redundant within plan, not pre-existing)",
        );
    }

    // ── name() is stable (catches name-mutation mutants) ──────────────────────

    #[test]
    fn name_is_stable() {
        assert_eq!(BuffIntoVoid.name(), "buff_into_void");
    }

    // ── `&&` vs `||` discriminators in plan_applied.any() (line 84) ──────────

    /// 2-step plan applying DIFFERENT statuses to the SAME target.
    /// With `&&` (correct): `plan_applied` match requires both entity AND status —
    /// neither step duplicates → critic must NOT fire.
    /// With `||` (mutated to OR): entity match alone fires → critic would
    /// incorrectly mark second step as buff-into-void.
    #[test]
    fn different_statuses_to_same_target_does_not_fire() {
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target])
            .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
            .with_ability("buff_haste", buff_ability("buff_haste", "haste"))
            .build();

        let plan = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: AbilityId::from("buff_shield"),
                    target: target_entity,
                    target_pos,
                },
                PlanStep::Cast {
                    ability: AbilityId::from("buff_haste"),
                    target: target_entity,
                    target_pos,
                },
            ],
            final_pos: caster_pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default(), Default::default()],
            ..TurnPlan::default()
        };
        assert_critic_passes(&BuffIntoVoid, &plan, &scn);
    }

    /// 2-step plan applying the SAME status to DIFFERENT targets.
    /// With `&&` (correct): entity differs → no match → critic must NOT fire.
    /// With `||` (mutated): status match alone fires → critic would incorrectly
    /// flag the second step.
    #[test]
    fn same_status_to_different_targets_does_not_fire() {
        let caster_pos = hex_from_offset(0, 0);
        let target_a_pos = hex_from_offset(2, 0);
        let target_b_pos = hex_from_offset(3, 0);
        let target_a_entity = Entity::from_raw_u32(2).expect("valid");
        let target_b_entity = Entity::from_raw_u32(3).expect("valid");

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target_a = UnitBuilder::new(2, Team::Enemy, target_a_pos).build();
        let target_b = UnitBuilder::new(3, Team::Enemy, target_b_pos).build();

        let scn = CriticScenarioBuilder::new(caster)
            .with_units(vec![target_a, target_b])
            .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
            .build();

        let plan = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: AbilityId::from("buff_shield"),
                    target: target_a_entity,
                    target_pos: target_a_pos,
                },
                PlanStep::Cast {
                    ability: AbilityId::from("buff_shield"),
                    target: target_b_entity,
                    target_pos: target_b_pos,
                },
            ],
            final_pos: caster_pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default(), Default::default()],
            ..TurnPlan::default()
        };
        assert_critic_passes(&BuffIntoVoid, &plan, &scn);
    }
}
