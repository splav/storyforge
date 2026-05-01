//! HealWithoutRescueValue critic — step 10.3.
//!
//! Fires when a plan contains a heal cast aimed at an ally with low *rescue
//! need*. Reuses `tuning.curves.rescue_ally` (the same curve used by the
//! appraisal layer for the `rescue_ally` need signal) so the critic and the
//! appraisal layer agree on what "ally needs rescue" means.
//!
//! Fire condition (two justifications block the critic — either is sufficient):
//!   - **Rescue need:** `rescue_need = curves.rescue_ally.eval((1 - hp_pct) *
//!     ally_threat_proxy) >= FIRE_THRESHOLD` — heal is rescue-justified.
//!   - **HP-need gate:** `target_hp_pct < HP_NEED_GATE` — wounded ally is worth
//!     topping up regardless of immediate threat.
//!
//! When neither holds, the heal lands on a healthy non-threatened ally and the
//! critic fires.
//!
//! Multiplier: continuous in `(FIRE_THRESHOLD - rescue_need)`. At
//! `rescue_need = 0` (target fully safe, no threat) the multiplier is the
//! legacy floor `1 - MAX_PENALTY = 0.4`; at the fire threshold boundary it
//! approaches `1.0` so the penalty doesn't snap.
//!
//! Replaces the legacy hard-pair `hp_pct > 0.7 && danger < 0.3` thresholds —
//! continuous gradation is consistent with how `appraisal::rescue_ally` already
//! grades ally need.

use crate::combat::ai::appraisal::ally_threat_proxy;
use super::{CriticHit, CriticKind, CriticReason, PlanCritic};
use crate::combat::ai::outcome::PlanAnnotation;
use crate::combat::ai::plan::types::{PlanStep, TurnPlan};
use crate::combat::ai::orchestration::ScoringCtx;
use crate::content::abilities::EffectDef;

// ── Constants ─────────────────────────────────────────────────────────────────

/// `rescue_need` below this threshold triggers the critic. Above it the heal
/// is rescue-justified.
const FIRE_THRESHOLD: f32 = 0.3;

/// HP-need fallback. If the target is below this HP fraction, healing is
/// considered justified regardless of threat — topping up wounded allies has
/// intrinsic value outside imminent rescue.
const HP_NEED_GATE: f32 = 0.6;

/// Maximum penalty depth (multiplier at `rescue_need = 0`). Mirrors the legacy
/// hard-coded multiplier so the worst-case penalty matches pre-curve behaviour.
const MAX_PENALTY: f32 = 0.6;

// ── Critic impl ───────────────────────────────────────────────────────────────

/// Unit struct — thresholds are baked as module constants.
pub struct HealWithoutRescueValue;

impl PlanCritic for HealWithoutRescueValue {
    fn name(&self) -> &'static str {
        "heal_without_rescue_value"
    }

    fn evaluate(
        &self,
        plan: &TurnPlan,
        ann: &PlanAnnotation,
        ctx: &ScoringCtx,
    ) -> Option<CriticHit> {
        for (step_idx, step) in plan.steps.iter().enumerate() {
            let PlanStep::Cast { ability, target_pos, .. } = step else {
                continue;
            };

            let Some(def) = ctx.world.content.abilities.get(ability) else {
                continue;
            };

            // Determine whether this is a heal cast:
            // - effect is EffectDef::Heal, OR
            // - outcome records hp_restored > 0 (covers RestoreResources etc.)
            let is_heal_by_effect = matches!(def.effect, EffectDef::Heal { .. });
            let is_heal_by_outcome = ann
                .outcomes
                .get(step_idx)
                .is_some_and(|o| o.hp_restored > 0.0);

            if !is_heal_by_effect && !is_heal_by_outcome {
                continue;
            }

            // Look up target ally in snapshot.
            let Some(target) = ctx.snap.unit_at(*target_pos) else {
                continue;
            };

            let target_hp_pct = target.hp_pct();

            // HP-need fallback: a meaningfully wounded ally justifies the heal
            // even outside imminent rescue.
            if target_hp_pct < HP_NEED_GATE {
                continue;
            }

            // Same input as appraisal::compute_rescue_ally — keeps the critic
            // and the need-signal layer consistent on what "ally in trouble"
            // means.
            let raw = (1.0 - target_hp_pct).clamp(0.0, 1.0)
                * ally_threat_proxy(target, ctx.snap);
            let rescue_need = ctx.world.tuning.curves.rescue_ally.eval(raw);

            if rescue_need >= FIRE_THRESHOLD {
                continue;
            }

            // Linear: rescue_need=0 → multiplier=(1-MAX_PENALTY); at the fire
            // threshold boundary multiplier=1.0 so penalty fades smoothly.
            let waste = (FIRE_THRESHOLD - rescue_need) / FIRE_THRESHOLD; // (0, 1]
            let multiplier = 1.0 - MAX_PENALTY * waste;

            return Some(CriticHit {
                critic: CriticKind::HealWithoutRescueValue,
                multiplier,
                reason: CriticReason::HealWithoutRescueValue {
                    rescue_need,
                    target_hp_pct,
                },
            });
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::pipeline::stages::critics::{CriticKind, PlanCritic};
    use crate::combat::ai::outcome::{ActionOutcomeEstimate, PlanAnnotation};
    use crate::combat::ai::plan::types::TurnPlan;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
    use crate::core::{AbilityId, DiceExpr};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    fn heal_ability(id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::Heal { dice: DiceExpr::new(2, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn cast_heal_plan(
        ability: &str,
        target_entity: Entity,
        target_pos: crate::game::hex::Hex,
        caster_pos: crate::game::hex::Hex,
        hp_restored: f32,
    ) -> (TurnPlan, PlanAnnotation) {
        let plan = TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from(ability),
                target: target_entity,
                target_pos,
            }],
            final_pos: caster_pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes: vec![Default::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let mut ann = PlanAnnotation::default();
        ann.outcomes.push(ActionOutcomeEstimate {
            hp_restored,
            ..Default::default()
        });
        (plan, ann)
    }

    // ── fires on canonical case ───────────────────────────────────────────────

    #[test]
    fn heal_without_rescue_fires_on_canonical_case() {
        // Target: hp=28/30 (93% — above HP_NEED_GATE), no enemies → threat_proxy=0
        // → rescue_need ≈ 0 → fires with worst-case multiplier (1 - MAX_PENALTY = 0.4).
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);

        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let target = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(28)
            .max_hp(30)
            .build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));

        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![caster.clone(), target], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);

        let target_entity = Entity::from_raw_u32(2).expect("valid");
        let (plan, ann) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 3.0);

        let hit = HealWithoutRescueValue
            .evaluate(&plan, &ann, &ctx)
            .expect("must fire: healthy ally + no threat");
        assert_eq!(hit.critic, CriticKind::HealWithoutRescueValue);
        // Penalty must be in [floor, 1.0): no threat → low rescue_need → harsh.
        assert!(
            hit.multiplier < 1.0 && hit.multiplier >= 1.0 - MAX_PENALTY - 1e-6,
            "multiplier {} must be in [{}, 1.0)",
            hit.multiplier,
            1.0 - MAX_PENALTY,
        );
        if let CriticReason::HealWithoutRescueValue { rescue_need, target_hp_pct } = hit.reason {
            assert!(rescue_need < FIRE_THRESHOLD, "rescue_need {rescue_need}");
            assert!(target_hp_pct >= HP_NEED_GATE, "target_hp_pct {target_hp_pct}");
        } else {
            panic!("expected HealWithoutRescueValue reason, got {:?}", hit.reason);
        }
    }

    // ── passes when target is wounded enough for HP-need gate ─────────────────

    #[test]
    fn heal_without_rescue_passes_on_clean_plan() {
        // Target hp=15/30=50% (< HP_NEED_GATE=0.6) → wounded ally, heal justified.
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();
        let low_hp_target = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(15)
            .max_hp(30)
            .build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        let snap = BattleSnapshot::new(vec![caster.clone(), low_hp_target], 1);
        let maps = empty_maps();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &caster);
        let (plan, ann) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 10.0);
        assert!(
            HealWithoutRescueValue.evaluate(&plan, &ann, &ctx).is_none(),
            "must not fire: HP 50% < HP_NEED_GATE = {HP_NEED_GATE}",
        );
    }

    // ── HP-need gate boundary ─────────────────────────────────────────────────

    #[test]
    fn heal_without_rescue_severity_scales_with_input() {
        // The HP-need gate at 0.6: just above → fires; just below → passes.
        // Multiplier with empty_maps is near-floor (no enemies → threat_proxy=0).
        let caster_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(2, 0);
        let caster = UnitBuilder::new(1, Team::Enemy, caster_pos).build();

        let mut content = empty_content();
        content.abilities.insert(AbilityId::from("heal"), heal_ability("heal"));
        let difficulty = crate::combat::ai::config::difficulty::DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let target_entity = Entity::from_raw_u32(2).expect("valid");

        // hp_pct ≈ 0.767 (23/30, > HP_NEED_GATE) → fires.
        let target_fires = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(23)
            .max_hp(30)
            .build();
        let snap_f = BattleSnapshot::new(vec![caster.clone(), target_fires], 1);
        let maps_f = empty_maps();
        let ctx_f = make_scoring_ctx(&world, &snap_f, &maps_f, &reservations, &caster);
        let (plan_f, ann_f) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 3.0);
        let hit_f = HealWithoutRescueValue.evaluate(&plan_f, &ann_f, &ctx_f);
        assert!(hit_f.is_some(), "hp_pct ≈ 0.77 must fire");
        let m_f = hit_f.unwrap().multiplier;
        assert!((1.0 - MAX_PENALTY..1.0).contains(&m_f),
            "multiplier {m_f} should be in [{}, 1.0)", 1.0 - MAX_PENALTY);

        // hp_pct = 0.5 (15/30, < HP_NEED_GATE) → does not fire.
        let target_passes = UnitBuilder::new(2, Team::Enemy, target_pos)
            .hp(15)
            .max_hp(30)
            .build();
        let snap_p = BattleSnapshot::new(vec![caster.clone(), target_passes], 1);
        let maps_p = empty_maps();
        let ctx_p = make_scoring_ctx(&world, &snap_p, &maps_p, &reservations, &caster);
        let (plan_p, ann_p) = cast_heal_plan("heal", target_entity, target_pos, caster_pos, 8.0);
        assert!(
            HealWithoutRescueValue.evaluate(&plan_p, &ann_p, &ctx_p).is_none(),
            "hp_pct = 0.5 (< HP_NEED_GATE) must not fire",
        );
    }
}
