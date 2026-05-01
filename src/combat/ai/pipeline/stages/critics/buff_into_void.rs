//! BuffIntoVoid critic — step 10.3.
//!
//! Fires when a plan contains a Cast step that applies a status effect to a
//! target who already has that same status active (or who received the same
//! status from an earlier step in the same plan). The cast is therefore wasted
//! ("buff into void").
//!
//! Fire condition:
//!   A `PlanStep::Cast { ability, target_pos, .. }` where `ability.statuses`
//!   is non-empty AND the primary target already carries at least one of those
//!   statuses (checked against `UnitSnapshot.statuses` at plan start, plus any
//!   statuses applied by earlier plan steps to the same target).
//!
//! One `CriticHit` is returned for the **first** wasted cast detected. If
//! multiple steps waste buffs the first one is sufficient to signal the problem.
//!
//! Multiplier: **0.6** (moderate — wasteful, but not catastrophic).

use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::utility::ScoringCtx;
use crate::core::StatusId;
use bevy::prelude::Entity;

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

    fn evaluate(
        &self,
        plan: &TurnPlan,
        _ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        // Track statuses applied by earlier plan steps, keyed by target Entity.
        // Entity is stable identity even if the target moves between steps —
        // a Hex-keyed proxy would miss intra-plan re-buff after target movement.
        let mut plan_applied: Vec<(Entity, StatusId)> = Vec::new();

        for step in &plan.steps {
            let PlanStep::Cast { ability, target, .. } = step else {
                continue;
            };

            let Some(def) = ctx.world.content.abilities.get(ability) else {
                continue;
            };

            if def.statuses.is_empty() {
                continue;
            }

            // Look up target unit by Entity (handles cross-step movement and
            // entity-keyed identity correctly).
            let Some(target_unit) = ctx.snap.unit(*target) else {
                // Register statuses this step would apply even if target not found,
                // then continue.
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
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, PlanCritic};
    use crate::combat::ai::outcome::PlanAnnotation;
    use crate::combat::ai::planning::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::{ActiveStatusView, BattleSnapshot};
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn, TargetType};
    use crate::core::{AbilityId, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    /// Build a simple status-applying ability def.
    fn buff_ability(id: &str, status_id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
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
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn cast_plan(ability: &str, target_entity: Entity, target_pos: crate::game::hex::Hex) -> TurnPlan {
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
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // ── fires on canonical case (target already has status) ───────────────────

    #[test]
    fn buff_into_void_fires_on_canonical_case() {
        // Target already has "shield" status active.
        // Caster casts an ability that applies "shield" again → critic must fire.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let mut target = UnitBuilder::new(2, Team::Enemy, target_pos).build();
        target.statuses = vec![ActiveStatusView {
            id: StatusId::from("shield"),
            rounds_remaining: 2,
            dot_per_tick: 0,
        }];

        let mut content = empty_content();
        content.abilities.insert(
            AbilityId::from("buff_shield"),
            buff_ability("buff_shield", "shield"),
        );

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        let target_entity = Entity::from_raw_u32(2).expect("valid");
        let plan = cast_plan("buff_shield", target_entity, target_pos);
        let ann = PlanAnnotation::default();

        let result = BuffIntoVoid.evaluate(&plan, &ann, &ctx);
        assert!(result.is_some(), "critic must fire when target already has the status");
        let hit = result.unwrap();
        assert_eq!(hit.critic, CriticKind::BuffIntoVoid);
        assert!(
            (hit.multiplier - BUFF_INTO_VOID_MULTIPLIER).abs() < 1e-6,
            "multiplier must be {BUFF_INTO_VOID_MULTIPLIER}, got {}", hit.multiplier
        );
        if let CriticReason::BuffIntoVoid { target_already_buffed, .. } = hit.reason {
            assert!(target_already_buffed, "target_already_buffed must be true");
        } else {
            panic!("expected BuffIntoVoid reason, got {:?}", hit.reason);
        }
    }

    // ── passes on clean plan (target has no status) ───────────────────────────

    #[test]
    fn buff_into_void_passes_on_clean_plan() {
        // Target has no statuses — applying "shield" is useful.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos).build(); // no statuses

        let mut content = empty_content();
        content.abilities.insert(
            AbilityId::from("buff_shield"),
            buff_ability("buff_shield", "shield"),
        );

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        let target_entity = Entity::from_raw_u32(2).expect("valid");
        let plan = cast_plan("buff_shield", target_entity, target_pos);
        let ann = PlanAnnotation::default();

        let result = BuffIntoVoid.evaluate(&plan, &ann, &ctx);
        assert!(result.is_none(), "critic must not fire when target has no such status");
    }

    // ── fires on redundant cast within plan (second step duplicates first) ────

    #[test]
    fn buff_into_void_severity_scales_with_input() {
        // Two cast steps in the plan applying the same status to the same target.
        // First cast: target has no status → passes.
        // Second cast: first cast already recorded status → critic fires.
        //
        // This also verifies the None boundary (single cast, no existing status)
        // vs Some boundary (second cast of same status in same plan).
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos).build();

        let mut content = empty_content();
        content.abilities.insert(
            AbilityId::from("buff_shield"),
            buff_ability("buff_shield", "shield"),
        );

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        let target_entity = Entity::from_raw_u32(2).expect("valid");

        // Single cast — no prior applications, no existing status → None.
        let single_plan = cast_plan("buff_shield", target_entity, target_pos);
        assert!(
            BuffIntoVoid.evaluate(&single_plan, &PlanAnnotation::default(), &ctx).is_none(),
            "single cast with no existing status must not fire",
        );

        // Double cast — second step duplicates the first → Some.
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
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let result = BuffIntoVoid.evaluate(&double_plan, &PlanAnnotation::default(), &ctx);
        assert!(
            result.is_some(),
            "second cast of same status must trigger buff_into_void critic",
        );
        if let Some(hit) = result {
            if let CriticReason::BuffIntoVoid { target_already_buffed, .. } = hit.reason {
                assert!(
                    !target_already_buffed,
                    "target_already_buffed must be false (redundant within plan, not pre-existing)",
                );
            } else {
                panic!("expected BuffIntoVoid reason");
            }
        }
    }
}
