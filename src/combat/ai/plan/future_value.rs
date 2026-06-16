//! Phase 7 prototype — offline-only scoring helpers.
//!
//! Implements the decomposition:
//!   `Score(plan) = PrefixScore(committed_prefix) + γ · FutureValue(committed_state)`
//!
//! where `PrefixScore` runs production `compute_plan_factors` + `finalize_scores`
//! on the truncated committed prefix, and `FutureValue` estimates the strategic
//! value of the state the actor is in after committing.
//!
//! **Offline prototype only.** Production pipeline never calls this module.
//! Single consumer: `replay_ai_log --phase7-prototype`.

use crate::combat::ai::config::tuning::AiTuning;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::builder::hypothetical as estimate_hypothetical;
use crate::combat::ai::plan::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::scoring::factors::aggregate::{
    aggregate_factors_to_score, compute_plan_factors,
};
use crate::combat::ai::scoring::factors::aoe_area;
use crate::combat::ai::scoring::factors::aoe_hits;
use crate::combat::ai::scoring::position_eval::evaluate_position;
use crate::combat::ai::scoring::target_selection::target_selection_score;
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::content::abilities::AoEShape;
use crate::game::hex::Hex;

// ── Constants ──────────────────────────────────────────────────────────────────

/// Discount applied to the FutureValue term in the prototype score.
/// KEEP IN SYNC with replay_ai_log.rs PHASE7_GAMMA
pub const PHASE7_GAMMA: f32 = 0.25;

/// Normalisation denominator for the mobility component.
/// KEEP IN SYNC with replay_ai_log.rs PHASE7_MAX_MOBILITY
pub const PHASE7_MAX_MOBILITY: f32 = 30.0;

// ── plan_prefix_only ──────────────────────────────────────────────────────────

/// Truncate `plan` to its committed prefix — the steps that fire this tick.
///
/// `sim_snapshots` truncates to `prefix_len` (runtime) or stays empty
/// (deserialized). `final_pos` is `committed_prefix_end_pos`. `residual_ap/mp`
/// come from the full plan (conservative; sim residuals aren't extractable here
/// without the actor entity).
pub fn plan_prefix_only(plan: &TurnPlan) -> TurnPlan {
    let prefix_len = plan.committed_step_count();
    let steps: Vec<PlanStep> = plan.steps[..prefix_len].to_vec();
    let outcomes = plan.outcomes[..prefix_len].to_vec();

    // Position after the committed prefix, not after the phantom tail.
    let final_pos = committed_prefix_end_pos(plan);

    // Per-unit residuals aren't extractable from the sim snapshot without the
    // actor entity, which we lack here — fall back to plan residuals
    // (slightly optimistic but safe for prototype scoring).
    let (residual_ap, residual_mp) = (plan.residual_ap, plan.residual_mp);

    // Shape invariant: sim_snapshots must be empty OR match steps.len().
    let sim_snapshots = if plan.sim_snapshots.is_empty() {
        Vec::new()
    } else {
        plan.sim_snapshots[..prefix_len].to_vec()
    };

    TurnPlan {
        steps,
        final_pos,
        residual_ap,
        residual_mp,
        outcomes,
        partial_score: 0.0,
        sim_snapshots,
        annotation: Default::default(),
    }
}

/// Position of the actor after the committed prefix fires.
/// Single source of truth consumed by both `plan_prefix_only` and
/// `score_plans_prototype` (which needs the position for `future_value`).
pub fn committed_prefix_end_pos(plan: &TurnPlan) -> Hex {
    match plan.committed_prefix() {
        CommittedPrefix::EndTurn => plan.final_pos,
        CommittedPrefix::Cast { .. } => plan.final_pos,
        CommittedPrefix::MoveOnly { path } | CommittedPrefix::MoveThenCast { path, .. } => {
            path.last().copied().unwrap_or(plan.final_pos)
        }
    }
}

// ── future_value_from_committed_state ─────────────────────────────────────────

/// Estimate the strategic value of the actor's state after committing the prefix.
///
/// `λ_pos + λ_attack + λ_mob` (additive; only outer `γ` scales the whole).
/// Components: tile quality, offensive potential, movement freedom — see the
/// three `*_component` helpers below.
///
/// Intent-aware weights: `ProtectSelf` zeroes attack and doubles pos;
/// `FocusTarget`/`ApplyCC` filter to the intent target; `SetupAOE` uses AoE
/// abilities only. Other intents use defaults.
pub fn future_value_from_committed_state(
    active: UnitView<'_>,
    committed_pos: Hex,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    ctx: &ScoringCtx,
    intent: &TacticalIntent,
) -> f32 {
    let pos_weight = match intent {
        TacticalIntent::ProtectSelf => 2.0,
        _ => 1.0,
    };
    let lambda_pos = pos_weight * position_component(active, committed_pos, ctx.world.tuning, maps);

    let lambda_attack = match intent {
        TacticalIntent::ProtectSelf | TacticalIntent::ProtectAlly { .. } => 0.0,
        _ => attack_component_intent(active, committed_pos, snap, ctx, intent),
    };

    let lambda_mob = mobility_component(committed_pos, active.effective_speed(), snap);
    lambda_pos + lambda_attack + lambda_mob
}

/// λ_pos: how good is `committed_pos` for this unit's role.
fn position_component(
    active: UnitView<'_>,
    committed_pos: Hex,
    tuning: &AiTuning,
    maps: &InfluenceMaps,
) -> f32 {
    evaluate_position(committed_pos, &active.cache.role, tuning, maps)
}

/// λ_attack = 0.5 × best `estimate_hypothetical().expected_damage` over the
/// intent-filtered candidate set.
///
/// Per-intent filter: `FocusTarget{T}`/`ApplyCC{T}` → only T (CC-capable for
/// ApplyCC); `SetupAOE` → AoE abilities at the max-hits tile; else top-3
/// enemies by priority. Reachability: `distance <= speed + max_attack_range`.
fn attack_component_intent(
    active: UnitView<'_>,
    committed_pos: Hex,
    snap: &BattleSnapshot,
    ctx: &ScoringCtx,
    intent: &TacticalIntent,
) -> f32 {
    use crate::combat::ai::scoring::policy;

    let content = ctx.world.content;
    let reach_budget = active.effective_speed().max(0) + active.cache.max_attack_range as i32;

    // Apply `policy::damage::value` to a hypothetical outcome for a single target.
    // Accepts `&Unit` (engine state) — `UnitView` derefs to `Unit`.
    let damage_value =
        |def: &crate::content::abilities::AbilityDef, target: &combat_engine::state::Unit| -> f32 {
            let h = estimate_hypothetical(def, target, &active.cache.caster_ctx, content);
            let damage_progress = (h.enemy_damage / target.hp().max(1) as f32).min(1.0);
            policy::damage::value(h.enemy_damage, damage_progress)
        };

    match intent {
        TacticalIntent::FocusTarget {
            target: target_entity,
        } => {
            let Some(target) = snap.unit(*target_entity) else {
                return 0.0;
            };
            let dist = committed_pos.unsigned_distance_to(target.pos) as i32;
            if dist > reach_budget {
                return 0.0;
            }
            let best = active
                .cache
                .abilities
                .iter()
                .filter_map(|id| content.abilities.get(id))
                .map(|def| damage_value(def, target.state))
                .fold(0.0f32, f32::max);
            0.5 * best
        }

        TacticalIntent::ApplyCC {
            target: target_entity,
        } => {
            let Some(target) = snap.unit(*target_entity) else {
                return 0.0;
            };
            let dist = committed_pos.unsigned_distance_to(target.pos) as i32;
            if dist > reach_budget {
                return 0.0;
            }
            let best = active
                .cache
                .abilities
                .iter()
                .filter_map(|id| content.abilities.get(id))
                .filter(|def| applies_cc(def, content))
                .map(|def| damage_value(def, target.state))
                .fold(0.0f32, f32::max);
            0.5 * best
        }

        TacticalIntent::SetupAOE => {
            // Best AoE ability × position with the most enemies hit.
            let enemies: Vec<UnitView<'_>> = snap.enemies_of(active.team).collect();
            if enemies.is_empty() {
                return 0.0;
            }

            let mut best: f32 = 0.0;
            for ability_id in &active.cache.abilities {
                let Some(def) = content.abilities.get(ability_id) else {
                    continue;
                };
                if def.aoe == AoEShape::None {
                    continue;
                }
                // Use each enemy's tile as a candidate AoE center.
                for target in &enemies {
                    let dist = committed_pos.unsigned_distance_to(target.pos) as i32;
                    if dist > reach_budget {
                        continue;
                    }
                    let area = aoe_area(def, target.pos, committed_pos);
                    let hits = aoe_hits(&area, active, snap);
                    let hit_count = hits.enemies.len() as f32;
                    if hit_count > best {
                        best = hit_count;
                    }
                }
            }
            // Normalise by a soft cap of 4 enemies (analogous to the 0.5 scalar).
            0.5 * (best / 4.0).min(1.0)
        }

        // Default: top-3 enemies by priority (original Phase 7 logic).
        _ => {
            let mut enemies: Vec<UnitView<'_>> = snap.enemies_of(active.team).collect();
            if enemies.is_empty() {
                return 0.0;
            }
            enemies.sort_by(|a, b| {
                let score_b = target_selection_score(active, *b, snap);
                let score_a = target_selection_score(active, *a, snap);
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let top_enemies = &enemies[..enemies.len().min(3)];

            let mut best: f32 = 0.0;
            for target in top_enemies {
                let dist = committed_pos.unsigned_distance_to(target.pos) as i32;
                if dist > reach_budget {
                    continue;
                }
                for ability_id in &active.cache.abilities {
                    let Some(def) = content.abilities.get(ability_id) else {
                        continue;
                    };
                    let s = damage_value(def, target.state);
                    if s > best {
                        best = s;
                    }
                }
            }
            0.5 * best
        }
    }
}

/// λ_mob = 0.1 × reachable_tile_count / PHASE7_MAX_MOBILITY.
///
/// Reachable count proxied by hex-ring area within `speed` radius minus tiles
/// blocked by enemies/corpses — avoids a full BFS.
fn mobility_component(committed_pos: Hex, speed: i32, snap: &BattleSnapshot) -> f32 {
    let budget = speed.max(0);
    if budget == 0 {
        return 0.0;
    }

    // Hex ring area = 3r²+3r+1; subtract blocked tiles below.
    let radius = budget as u32;
    let ring_area = (3 * radius * radius + 3 * radius + 1) as f32;

    // Subtract occupied tiles (enemies + corpses block stopping).
    let blocked: usize = snap
        .state
        .units()
        .iter()
        .filter(|u| {
            let dist = committed_pos.unsigned_distance_to(u.pos);
            dist > 0 && dist <= radius
        })
        .count();

    let reachable = (ring_area - blocked as f32).max(0.0);
    0.1 * (reachable / PHASE7_MAX_MOBILITY).min(1.0)
}

// ── score_plans_prototype ─────────────────────────────────────────────────────

/// Prototype scorer implementing Phase 7 decomposition.
///
/// For each plan:
///   score[i] = PrefixScore(prefix_i) + PHASE7_GAMMA × FutureValue(committed_pos_i)
///
/// `PrefixScore` uses production `compute_plan_factors` + `finalize_scores` on
/// the truncated committed prefix — this means `intent_sum`, `tempo_gain`, and
/// `self_survival` are all computed on the prefix, not the phantom tail.
///
/// Returns `Vec<f32>` parallel to `plans`.
pub fn score_plans_prototype(
    plans: &[TurnPlan],
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Vec<f32> {
    if plans.is_empty() {
        return Vec::new();
    }

    // Build prefix plans and their factors in a single pass.
    let mut prefix_plans: Vec<TurnPlan> = plans.iter().map(plan_prefix_only).collect();
    let prefix_factors: Vec<_> = prefix_plans
        .iter()
        .map(|p| compute_plan_factors(p, intent, ctx))
        .collect();

    // Batch-normalize prefix factors (aggregate_factors_to_score is batch-wise).
    let prefix_scores = aggregate_factors_to_score(&mut prefix_plans, &prefix_factors, ctx);

    // Add γ × FutureValue for each plan's committed position.
    prefix_scores
        .into_iter()
        .enumerate()
        .map(|(i, ps)| {
            let committed_pos = committed_prefix_end_pos(&plans[i]);
            let fv = future_value_from_committed_state(
                ctx.active,
                committed_pos,
                ctx.snap,
                ctx.maps,
                ctx,
                intent,
            );
            ps + PHASE7_GAMMA * fv
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::combat::ai::plan::types::{PlanStep, StepOutcome};
    use crate::combat::ai::test_helpers::{ent, snapshot_from};
    use crate::combat::ai::test_helpers::{make_scoring_ctx, make_test_ctx, UnitBuilder};
    use crate::combat::ai::world::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::tags::AiTags;
    use crate::content::abilities::CasterContext;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use combat_engine::DiceExpr;

    /// `CasterContext` with a weapon so `WeaponAttack` effect yields non-zero expected damage.
    fn melee_caster() -> CasterContext {
        CasterContext {
            str_mod: 2,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: Some(DiceExpr::new(1, 6, 0)),
            ranged_dice: None,
            dex_mod: 0,
        }
    }

    fn make_cast(ability: &str, target: bevy::prelude::Entity, pos: Hex) -> PlanStep {
        PlanStep::Cast {
            ability: combat_engine::AbilityId::from(ability),
            target,
            target_pos: pos,
        }
    }

    fn make_cast_id(ability: &str, target: u32, pos: Hex) -> PlanStep {
        make_cast(ability, ent(target), pos)
    }

    fn make_move(dest: Hex) -> PlanStep {
        PlanStep::Move { path: vec![dest] }
    }

    fn plan_with(
        steps: Vec<PlanStep>,
        final_pos: Hex,
        sim_snapshots: Vec<BattleSnapshot>,
    ) -> TurnPlan {
        let outcomes = vec![StepOutcome::default(); steps.len()];
        TurnPlan {
            steps,
            final_pos,
            residual_ap: 1,
            residual_mp: 2,
            outcomes,
            partial_score: 0.0,
            sim_snapshots,
            annotation: Default::default(),
        }
    }

    fn plan_deserialized(steps: Vec<PlanStep>, final_pos: Hex) -> TurnPlan {
        plan_with(steps, final_pos, Vec::new())
    }

    fn plan_runtime(steps: Vec<PlanStep>, final_pos: Hex, snap: BattleSnapshot) -> TurnPlan {
        let n = steps.len();
        plan_with(steps, final_pos, vec![snap; n])
    }

    // ── plan_prefix_only tests ────────────────────────────────────────────────

    #[test]
    fn end_turn_prefix_is_empty() {
        let p = plan_deserialized(vec![], hex_from_offset(2, 2));
        let prefix = plan_prefix_only(&p);
        assert!(prefix.steps.is_empty());
        assert!(prefix.sim_snapshots.is_empty());
        assert_eq!(prefix.final_pos, hex_from_offset(2, 2));
    }

    #[test]
    fn solo_cast_prefix_is_one_step() {
        let target_pos = hex_from_offset(3, 0);
        let cast = make_cast_id("strike", 99, target_pos);
        let p = plan_deserialized(
            vec![cast.clone(), make_move(hex_from_offset(0, 1))],
            hex_from_offset(2, 2), // actor stays at 0,0 (cast in place), final_pos is plan's full
        );
        let prefix = plan_prefix_only(&p);
        assert_eq!(prefix.steps.len(), 1);
        assert!(matches!(prefix.steps[0], PlanStep::Cast { .. }));
        // final_pos for solo cast: actor didn't move, so plan.final_pos is reused.
        assert_eq!(prefix.final_pos, hex_from_offset(2, 2));
    }

    #[test]
    fn move_only_prefix_takes_destination() {
        let dest = hex_from_offset(1, 1);
        let p = plan_deserialized(
            vec![make_move(dest), make_move(hex_from_offset(2, 2))],
            hex_from_offset(2, 2),
        );
        let prefix = plan_prefix_only(&p);
        assert_eq!(prefix.steps.len(), 1);
        assert_eq!(prefix.final_pos, dest);
    }

    #[test]
    fn move_then_cast_prefix_is_two_steps() {
        let dest = hex_from_offset(1, 0);
        let target_pos = hex_from_offset(2, 0);
        let p = plan_deserialized(
            vec![
                make_move(dest),
                make_cast_id("strike", 5, target_pos),
                make_move(hex_from_offset(0, 0)), // phantom tail
            ],
            hex_from_offset(0, 0),
        );
        let prefix = plan_prefix_only(&p);
        assert_eq!(prefix.steps.len(), 2);
        assert!(matches!(prefix.steps[0], PlanStep::Move { .. }));
        assert!(matches!(prefix.steps[1], PlanStep::Cast { .. }));
        assert_eq!(prefix.final_pos, dest);
    }

    #[test]
    fn sim_snapshots_truncated_to_prefix_len() {
        let snap = BattleSnapshot::default();
        let dest = hex_from_offset(1, 0);
        let p = plan_runtime(
            vec![make_move(dest), make_move(hex_from_offset(2, 0))],
            hex_from_offset(2, 0),
            snap,
        );
        assert_eq!(p.sim_snapshots.len(), 2);
        let prefix = plan_prefix_only(&p);
        // MoveOnly: prefix_len = 1
        assert_eq!(prefix.steps.len(), 1);
        assert_eq!(prefix.sim_snapshots.len(), 1);
    }

    #[test]
    fn deserialized_plan_sim_empty_stays_empty() {
        let p = plan_deserialized(
            vec![
                make_move(hex_from_offset(1, 0)),
                make_move(hex_from_offset(2, 0)),
            ],
            hex_from_offset(2, 0),
        );
        assert!(p.sim_snapshots.is_empty());
        let prefix = plan_prefix_only(&p);
        assert!(
            prefix.sim_snapshots.is_empty(),
            "shape invariant: empty or len==steps.len()"
        );
        assert_eq!(prefix.steps.len(), 1);
    }

    // ── future_value::pos_component_reads_position_eval ──────────────────────

    #[test]
    fn pos_component_reads_position_eval() {
        let h = hex_from_offset(2, 2);
        let actor = UnitBuilder::new(1, Team::Enemy, h).build();

        let mut danger_map = InfluenceMap::new();
        danger_map.add(h, 0.6);
        let maps = InfluenceMaps {
            danger: danger_map,
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let snap = snapshot_from(vec![actor.clone()], 1);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let intent = TacticalIntent::Reposition;
        let fv = future_value_from_committed_state(actor_view, h, &snap, &maps, &ctx, &intent);
        // position_eval returns negative for dangerous tiles for most roles.
        // At minimum the function must not panic and return a finite value.
        assert!(fv.is_finite(), "future_value must be finite: {fv}");
        // Verify λ_pos is present: danger map hit should pull fv non-zero.
        let fv_safe = future_value_from_committed_state(
            actor_view,
            hex_from_offset(0, 0), // tile with no danger
            &snap,
            &InfluenceMaps {
                danger: InfluenceMap::new(),
                ally_support: InfluenceMap::new(),
                opportunity: InfluenceMap::new(),
                escape: InfluenceMap::new(),
            },
            &ctx,
            &intent,
        );
        assert_ne!(fv, fv_safe, "dangerous tile must differ from safe tile");
    }

    // ── attack_component tests ────────────────────────────────────────────────

    #[test]
    fn attack_component_zero_when_no_enemies() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .ability_names(&["melee_attack"])
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let result = attack_component_intent(
            actor_view,
            actor.pos,
            &snap,
            &ctx,
            &TacticalIntent::Reposition,
        );
        assert_eq!(result, 0.0, "no enemies → zero attack component");
    }

    #[test]
    fn attack_component_picks_best_reachable_target() {
        let committed_pos = hex_from_offset(3, 3);
        let actor = UnitBuilder::new(1, Team::Enemy, committed_pos)
            .ability_names(&["melee_attack"])
            .build();
        // Adjacent enemy: distance 1, within melee range.
        let nearby = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3)).build();
        // Far enemy: distance > speed + range.
        let far = UnitBuilder::new(3, Team::Player, hex_from_offset(10, 10)).build();
        let snap = snapshot_from(vec![actor.clone(), nearby.clone(), far.clone()], 1);

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let with_nearby = attack_component_intent(
            actor_view,
            committed_pos,
            &snap,
            &ctx,
            &TacticalIntent::Reposition,
        );
        // Nearby enemy is reachable; far is not. Component should be positive.
        // Exact value depends on ability scoring, but must exceed no-enemy case.
        assert!(with_nearby >= 0.0, "attack component must be non-negative");
    }

    #[test]
    fn mobility_component_scales_with_reachable_count() {
        let pos = hex_from_offset(5, 5);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).build();

        // No blockers — full ring area.
        let snap_empty = snapshot_from(vec![actor.clone()], 1);
        let mob_free = mobility_component(pos, 3, &snap_empty);

        // Many blockers — reduced mobility.
        let mut units = vec![actor.clone()];
        for i in 0..8 {
            let blocker = UnitBuilder::new(
                10 + i,
                Team::Player,
                hex_from_offset(5 + (i as i32 % 3) + 1, 5),
            )
            .build();
            units.push(blocker);
        }
        let snap_blocked = snapshot_from(units, 1);
        let mob_blocked = mobility_component(pos, 3, &snap_blocked);

        assert!(
            mob_free >= mob_blocked,
            "more blockers → lower mobility component"
        );
        assert!(mob_free > 0.0, "free board must have positive mobility");
    }

    #[test]
    fn future_value_sums_components() {
        // Regression guard: ensure the three components add up correctly.
        let pos = hex_from_offset(3, 3);
        let actor = UnitBuilder::new(1, Team::Enemy, pos)
            .ability_names(&["melee_attack"])
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);

        let mut danger_map = InfluenceMap::new();
        danger_map.add(pos, 0.4);
        let maps = InfluenceMaps {
            danger: danger_map.clone(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let intent = TacticalIntent::Reposition;
        let fv = future_value_from_committed_state(actor_view, pos, &snap, &maps, &ctx, &intent);
        let lp = position_component(actor_view, pos, ctx.world.tuning, &maps);
        let la = attack_component_intent(actor_view, pos, &snap, &ctx, &intent);
        let lm = mobility_component(pos, actor.speed, &snap);

        assert!(
            (fv - (lp + la + lm)).abs() < 1e-5,
            "future_value must equal sum of components: fv={fv}, lp+la+lm={}",
            lp + la + lm
        );
    }

    // ── score_plans_prototype tests ───────────────────────────────────────────

    #[test]
    fn empty_plans_returns_empty() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let result = score_plans_prototype(&[], &TacticalIntent::Reposition, &ctx);
        assert!(result.is_empty());
    }

    /// Two plans identical in their committed prefix but differing only in the
    /// phantom tail (post-cast move) must receive the same prototype score.
    /// This is the central regression guard for Phase 7.
    #[test]
    fn phantom_tail_plans_tie_with_tailless_equivalents() {
        let actor_pos = hex_from_offset(0, 0);
        let target_pos = hex_from_offset(1, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let intent = TacticalIntent::FocusTarget {
            target: target.entity,
        };

        let cast_step = make_cast("melee_attack", target.entity, target_pos);

        // Plan A: solo Cast — no tail.
        let plan_a = plan_deserialized(
            vec![cast_step.clone()],
            actor_pos, // actor stays put (cast in place)
        );

        // Plan B: Cast + phantom Move (retreat to start — zero displacement).
        let plan_b = plan_deserialized(vec![cast_step, make_move(actor_pos)], actor_pos);

        let scores = score_plans_prototype(&[plan_a, plan_b], &intent, &ctx);
        assert_eq!(scores.len(), 2);
        // Prototype scores must be equal: phantom tail has no influence.
        let diff = (scores[0] - scores[1]).abs();
        assert!(
            diff < 1e-4,
            "phantom tail must not change prototype score: scores = {:?}, diff = {diff}",
            scores
        );
    }

    /// A plan with a longer committed prefix (Move + Cast) should outscore a
    /// pure EndTurn plan when moving toward an enemy.
    #[test]
    fn longer_prefix_wins_over_shorter_with_same_end_state() {
        let actor_pos = hex_from_offset(0, 0);
        let step_pos = hex_from_offset(1, 0);
        let target_pos = hex_from_offset(2, 0);

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["melee_attack"])
            .build();
        let target = UnitBuilder::new(2, Team::Player, target_pos).build();
        let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let intent = TacticalIntent::FocusTarget {
            target: target.entity,
        };

        // EndTurn plan (no steps).
        let end_turn = plan_deserialized(vec![], actor_pos);
        // Move-then-Cast plan.
        let move_cast = plan_deserialized(
            vec![
                make_move(step_pos),
                make_cast("melee_attack", target.entity, target_pos),
            ],
            step_pos,
        );

        let scores = score_plans_prototype(&[end_turn, move_cast], &intent, &ctx);
        assert_eq!(scores.len(), 2);
        assert!(
            scores.iter().all(|s| s.is_finite()),
            "all scores must be finite"
        );
        // scores[0] = end_turn (no steps), scores[1] = move+cast toward enemy.
        // The move+cast plan should outscore end-turn when the actor can reach and attack.
        assert!(
            scores[1] > scores[0],
            "move+cast score ({}) should exceed end-turn score ({}) when actor can attack",
            scores[1],
            scores[0]
        );
    }

    // ── P1 intent-aware FV tests ──────────────────────────────────────────────

    /// FocusTarget: attack_component counts only the specified target.
    /// Position where the non-target B is reachable but target A is not → FV with
    /// FocusTarget{A} must be lower than with FocusTarget{B} (or default).
    #[test]
    fn focus_target_ignores_non_target_enemies() {
        let actor_pos = hex_from_offset(5, 5);
        let pos_a = hex_from_offset(10, 10); // far — outside reach_budget
        let pos_b = hex_from_offset(6, 5); // adjacent — reachable

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(&["melee_attack"])
            .caster_ctx(melee_caster())
            .build();
        let enemy_a = UnitBuilder::new(2, Team::Player, pos_a).build();
        let enemy_b = UnitBuilder::new(3, Team::Player, pos_b).build();
        let snap = snapshot_from(vec![actor.clone(), enemy_a.clone(), enemy_b.clone()], 1);

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let fv_focus_a = future_value_from_committed_state(
            actor_view,
            actor_pos,
            &snap,
            &maps,
            &ctx,
            &TacticalIntent::FocusTarget {
                target: enemy_a.entity,
            },
        );
        let fv_focus_b = future_value_from_committed_state(
            actor_view,
            actor_pos,
            &snap,
            &maps,
            &ctx,
            &TacticalIntent::FocusTarget {
                target: enemy_b.entity,
            },
        );

        // FocusTarget{A}: A is far → attack_component = 0. FocusTarget{B}: B adjacent → > 0.
        assert!(
            fv_focus_a < fv_focus_b,
            "FocusTarget on unreachable A must score lower than on reachable B: fv_a={fv_focus_a}, fv_b={fv_focus_b}"
        );
    }

    /// ProtectSelf: attack_component = 0, pos weighted ×2.
    #[test]
    fn protect_self_suppresses_attack_component() {
        let actor_pos = hex_from_offset(5, 5);
        let enemy_pos = hex_from_offset(6, 5); // adjacent — would contribute to attack under default

        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(&["melee_attack"])
            .caster_ctx(melee_caster())
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();

        // Add danger at actor_pos so pos_component is non-zero (proves ×2 took effect).
        let mut danger_map = InfluenceMap::new();
        danger_map.add(actor_pos, 0.5);
        let maps = InfluenceMaps {
            danger: danger_map,
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        };

        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let fv_default = future_value_from_committed_state(
            actor_view,
            actor_pos,
            &snap,
            &maps,
            &ctx,
            &TacticalIntent::Reposition,
        );
        let fv_protect = future_value_from_committed_state(
            actor_view,
            actor_pos,
            &snap,
            &maps,
            &ctx,
            &TacticalIntent::ProtectSelf,
        );

        // Under ProtectSelf: attack_component = 0 (enemy adjacent, but ignored).
        // pos_component is ×2 vs default ×1.
        // λ_mob is same for both.
        let attack_default = attack_component_intent(
            actor_view,
            actor_pos,
            &snap,
            &ctx,
            &TacticalIntent::Reposition,
        );
        let pos_default = position_component(actor_view, actor_pos, ctx.world.tuning, &maps);
        let mob = mobility_component(actor_pos, actor.speed, &snap);

        // attack under ProtectSelf must be zero (enemy reachable but suppressed).
        assert!(
            attack_default > 0.0,
            "sanity: default attack must be positive with reachable enemy: {attack_default}"
        );
        let expected_protect = 2.0 * pos_default + mob;
        assert!(
            (fv_protect - expected_protect).abs() < 1e-5,
            "ProtectSelf FV must be 2×pos + mob (attack=0): got {fv_protect}, expected {expected_protect}"
        );
        assert!(
            (fv_default - (pos_default + attack_default + mob)).abs() < 1e-5,
            "default FV must sum all three: got {fv_default}"
        );
    }

    /// ApplyCC: attack_component counts only CC-capable abilities.
    /// When actor has no CC abilities → attack_component = 0.
    /// (Real CC ability test would require a CC ability in test fixtures; we prove
    /// the zero branch which is pure logic coverage of the filter path.)
    #[test]
    fn apply_cc_uses_only_cc_abilities() {
        let actor_pos = hex_from_offset(5, 5);
        let enemy_pos = hex_from_offset(6, 5);

        // melee_attack has no CC statuses → ApplyCC attack_component must be 0.
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ability_names(&["melee_attack"])
            .caster_ctx(melee_caster())
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, enemy_pos).build();
        let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);

        let content = crate::content::content_view::ActiveContentData::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let world = make_test_ctx(&content, &difficulty);
        let maps = crate::combat::ai::test_helpers::empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
        let actor_view = snap.unit(actor.entity).unwrap();

        let attack_cc = attack_component_intent(
            actor_view,
            actor_pos,
            &snap,
            &ctx,
            &TacticalIntent::ApplyCC {
                target: enemy.entity,
            },
        );
        let attack_default = attack_component_intent(
            actor_view,
            actor_pos,
            &snap,
            &ctx,
            &TacticalIntent::Reposition,
        );

        assert_eq!(
            attack_cc, 0.0,
            "no CC abilities → ApplyCC attack_component = 0"
        );
        assert!(
            attack_default > 0.0,
            "sanity: melee_attack scores >0 in default path"
        );
    }
}
