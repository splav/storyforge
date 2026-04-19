//! Plan scoring: replay each plan on a sim, aggregate 9 factors, normalize and
//! weight the same way single-candidate scoring does.
//!
//! Aggregation rules per factor:
//! - `damage`, `heal`, `cc`, `scarcity`: **discounted sum** across cast steps.
//!   step[k] contributes its per-step factor value weighted by
//!   `base_discount^k`, where `base_discount` is a difficulty knob (0.75 easy
//!   / 0.85 normal / 0.90 hard). Rationale: future steps carry execution
//!   uncertainty — each depth multiplies the chance of state drift between
//!   plan and reality. The discount also prevents "cheap-filler" extensions
//!   from winning the damage normalization race against genuinely strong
//!   short plans.
//! - **Post-goal aggressive discount**: once a step kills the current
//!   `FocusTarget`/`ApplyCC` target, remaining steps are additionally scaled
//!   by ×0.5. Post-kill heal/move actions still contribute (preserves info
//!   that Plan B does more than Plan A), but they're properly treated as
//!   bonuses rather than peers of the goal-achieving step.
//! - `kill`: max across steps (binary "did this plan kill anyone?"), not
//!   discounted — a goal outcome is valued at achievement magnitude.
//! - `focus`: max target_priority across casts, not discounted.
//! - `intent`: max intent_score across the **committed prefix**
//!   (`steps[..committed_step_count]`), not across the whole plan. Deep
//!   uncommitted steps don't execute this tick — aggregating their intent
//!   would reward plans whose alignment sits in the discarded tail
//!   (e.g. `[Move-random, Move-random, Cast@focus]` getting intent=1.0
//!   while committing a useless first Move). Moves inside the committed
//!   prefix still participate so Reposition intent lands on the move step.
//! - `position`: `evaluate_position(final_pos)` — terminal.
//! - `risk`: `1 − max_danger_along_path` — worst tile the actor traverses or
//!   casts from.

#![allow(clippy::too_many_arguments)]

use crate::combat::ai::factors::{self, ScoredStep, NUM_FACTORS, SIGNED_FACTOR};
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{intent_score, TacticalIntent};
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::estimate_st_damage;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{CasterContext, EffectDef};
use crate::core::modifier;
use crate::core::DiceRng;
use crate::game::components::Abilities;
use bevy::prelude::Entity;

/// Top-level entry. Produces one composite score per plan using the same
/// normalization+weight+noise pipeline as `score_candidates`.
pub fn score_plans(
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> Vec<f32> {
    score_plans_with_raw(plans, active, intent, ctx, snap, maps, reservations, rng).0
}

/// Same computation as `score_plans`, but also returns the **pre-normalization**
/// raw factor matrix so log writers / offline tools can recalibrate weights
/// without rerunning sim.
pub fn score_plans_with_raw(
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> (Vec<f32>, Vec<[f32; NUM_FACTORS]>) {
    if plans.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let raw: Vec<[f32; NUM_FACTORS]> = plans
        .iter()
        .map(|p| compute_plan_factors(p, active, intent, ctx, snap, maps, reservations))
        .collect();

    // Per-factor min/max for batch-relative normalization.
    let mut maxes = [0.0f32; NUM_FACTORS];
    let mut mins = [0.0f32; NUM_FACTORS];
    for factors in &raw {
        for (i, &v) in factors.iter().enumerate() {
            if v > maxes[i] {
                maxes[i] = v;
            }
            if v < mins[i] {
                mins[i] = v;
            }
        }
    }
    let mut denom = [0.0f32; NUM_FACTORS];
    for i in 0..NUM_FACTORS {
        denom[i] = if SIGNED_FACTOR[i] {
            mins[i].abs().max(maxes[i].abs())
        } else {
            maxes[i]
        };
    }

    let mut weights = active.role.factor_weights();
    weights[7] *= ctx.world.difficulty.intent_commitment;
    weights[8] *= ctx.world.difficulty.resource_discipline;
    let noise_amp = ctx.world.difficulty.score_noise();

    let scores: Vec<f32> = raw
        .iter()
        .zip(plans.iter())
        .map(|(factors, plan)| {
            let mut score = 0.0f32;
            for i in 0..NUM_FACTORS {
                let normalized = if denom[i] > f32::EPSILON {
                    factors[i] / denom[i]
                } else {
                    0.0
                };
                score += normalized * weights[i];
            }
            // Summon bonus bypasses normalisation: the factor pipeline can't
            // see the strategic value of creating an ally, and for hybrid
            // roles the damage-axis weight is too low to lift a raw summon
            // score on its own.
            score += plan_summon_bonus(plan, active, ctx, snap);
            if noise_amp > 0.0 {
                let noise = (rng.roll_d(1000) as f32 / 500.0 - 1.0) * noise_amp;
                score += noise;
            }
            score
        })
        .collect();
    (scores, raw)
}

/// Additive post-normalisation bonus for every `Summon` cast in the plan.
/// Each summon contributes `summon_dpr × decay`, where `decay = 1 − count/cap`
/// is recomputed against a **running** summon count so a multi-summon plan
/// doesn't get linear credit as the roster fills. Zero for plans without any
/// summon casts.
fn plan_summon_bonus(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
) -> f32 {
    let mut count = snap
        .units
        .iter()
        .filter(|u| u.summoner == Some(active.entity))
        .count() as f32;

    let mut total = 0.0f32;
    for step in &plan.steps {
        let PlanStep::Cast { ability, .. } = step else { continue };
        let Some(def) = ctx.world.content.abilities.get(ability) else { continue };
        let EffectDef::Summon { template, max_active } = &def.effect else { continue };

        let cap = max_active.unwrap_or(3).max(1) as f32;
        let decay = (1.0 - (count / cap)).max(0.0);
        if decay <= 0.0 {
            continue;
        }

        let Some(tpl) = ctx.world.content.unit_templates.get(template) else { continue };
        let weapon = ctx.world.content.weapons.get(&tpl.equipment.main_hand);
        let caster_ctx = CasterContext {
            str_mod: modifier(tpl.stats.strength),
            int_mod: modifier(tpl.stats.intelligence),
            spell_power: weapon.map_or(0, |wd| wd.spell_power),
            weapon_dice: weapon.map(|wd| wd.dice.clone()),
        };
        let abilities = Abilities(tpl.ability_ids.clone());
        let dpr = estimate_st_damage(&caster_ctx, &abilities, ctx.world.content);
        total += dpr * decay;
        count += 1.0;
    }
    total
}

/// Extra multiplicative discount on step_weight applied **after** a step that
/// kills the current intent's target. Expresses the scoring intuition that
/// post-goal actions are genuine bonuses but shouldn't be weighed as peers of
/// the goal-achieving step.
const POST_GOAL_DISCOUNT: f32 = 0.5;

/// Compute the 9 raw utility factors for a single plan. Empty plan (seed)
/// yields zeros for cumulative factors and baselines on position/risk at the
/// actor's current tile. See module docs for per-factor aggregation rules.
pub fn compute_plan_factors(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> [f32; NUM_FACTORS] {
    // No sim is run here: the generator already produced the sim state after
    // every step and cached it on the plan. For step k we read the
    // **pre-step-k** snapshot from `plan.sim_snapshots[k-1]` (or the original
    // `snap` for k=0). Invariant: `sim_snapshots.len() == steps.len()`, enforced
    // at generation time.
    debug_assert_eq!(
        plan.sim_snapshots.len(),
        plan.steps.len(),
        "TurnPlan sim_snapshots must align with steps",
    );

    let mut damage_sum = 0.0f32;
    let mut heal_sum = 0.0f32;
    let mut kill_max = 0.0f32;
    let mut cc_sum = 0.0f32;
    let mut scarcity_sum = 0.0f32;
    let mut focus_max = 0.0f32;
    let mut intent_max = f32::NEG_INFINITY;
    let mut path_danger_max = maps.danger.get(active.pos);

    let base_discount = ctx.world.difficulty.plan_step_discount;
    let mut step_weight: f32 = 1.0;
    let mut goal_achieved = false;
    // Intent aggregation is bounded to the committed prefix: deep steps
    // don't fire this tick and their intent signal is spurious (see module doc).
    let committed = plan.committed_step_count();

    for (idx, step) in plan.steps.iter().enumerate() {
        // Pre-step snapshot: cached post-state of the previous step, or the
        // caller's original snapshot for the first step.
        let pre_snap: &BattleSnapshot = if idx == 0 {
            snap
        } else {
            &plan.sim_snapshots[idx - 1]
        };
        let Some(sim_actor) = pre_snap.unit(active.entity).cloned() else {
            break;
        };

        if let PlanStep::Move { path } = step {
            // Track worst-tile danger across the path before the view is built.
            for &h in path {
                let d = maps.danger.get(h);
                if d > path_danger_max {
                    path_danger_max = d;
                }
            }
        }

        let scored_step = ScoredStep::from_plan_step(step, sim_actor.pos);

        // Intent factor participates uniformly across Cast and Move steps —
        // taken as the max, so it's not scaled by step_weight. Gated to the
        // committed prefix: tail steps don't fire this tick.
        if idx < committed {
            let iv = intent_score(
                intent,
                &scored_step,
                &sim_actor,
                pre_snap,
                maps,
                ctx.world.content,
                ctx.world.difficulty,
            );
            if iv > intent_max {
                intent_max = iv;
            }
        }

        if let PlanStep::Cast { .. } = step {
            let raw = factors::compute_factors(
                &scored_step,
                &sim_actor,
                intent,
                ctx,
                pre_snap,
                maps,
                reservations,
            );
            // Discounted cumulative factors.
            damage_sum += raw[0] * step_weight;
            cc_sum += raw[2] * step_weight;
            heal_sum += raw[3] * step_weight;
            scarcity_sum += raw[8] * step_weight;
            // Un-discounted outcome/priority signals.
            if raw[1] > kill_max {
                kill_max = raw[1];
            }
            if raw[6] > focus_max {
                focus_max = raw[6];
            }
        }

        // Geometric per-step discount on the next step's contribution.
        step_weight *= base_discount;

        // Post-goal aggressive discount fires at most once, when this step
        // killed the current intent's declared target. The kill signal comes
        // from the cached outcomes — AoE that incidentally kills the intent
        // target triggers the bump just like a direct cast.
        if !goal_achieved {
            let killed = plan
                .outcomes
                .get(idx)
                .map(|o| o.killed.as_slice())
                .unwrap_or(&[]);
            if killed_intent_target(killed, intent) {
                step_weight *= POST_GOAL_DISCOUNT;
                goal_achieved = true;
            }
        }
    }

    let position = evaluate_position(plan.final_pos, &active.role, maps);
    let final_danger = path_danger_max.max(maps.danger.get(plan.final_pos));
    let risk = 1.0 - final_danger;

    // Focus floor for empty plans: use the best priority target on current
    // snapshot so "do nothing" doesn't misleadingly score with focus=0.
    if plan.steps.is_empty() {
        focus_max = snap
            .enemies_of(active.team)
            .map(|t| target_priority(active, t, snap))
            .fold(0.0f32, f32::max);
    }

    let intent_val = if intent_max.is_finite() { intent_max } else { 0.0 };

    [
        damage_sum,
        kill_max,
        cc_sum,
        heal_sum,
        position,
        risk,
        focus_max,
        intent_val,
        scarcity_sum,
    ]
}

/// True iff the sim's step kills contain the intent's declared target. Only
/// `FocusTarget` and `ApplyCC` carry an explicit kill/CC goal; other intents
/// return false (they don't have a single "achievement" target).
fn killed_intent_target(killed: &[Entity], intent: &TacticalIntent) -> bool {
    let target = match intent {
        TacticalIntent::FocusTarget { target } => *target,
        TacticalIntent::ApplyCC { target } => *target,
        _ => return false,
    };
    killed.contains(&target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
    use crate::combat::ai::planning::types::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::AiTags;
    use crate::combat::ai::utility::{ActorCtx, AiWorld};
    use crate::content::races::CritFailEffect;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use bevy::prelude::Entity;

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(id).expect("valid"),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 2,
            max_ap: 2,
            movement_points: 3,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec!["melee_attack".into()],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        }
    }

    fn empty_maps() -> InfluenceMaps {
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    fn test_ctx<'a>(
        content: &'a crate::content::content_view::ContentView,
        difficulty: &'a DifficultyProfile,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        UtilityContext {
            world: AiWorld { content, difficulty },
            actor: ActorCtx {
                caster: &CasterContext {
                    str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None,
                },
                abilities,
                crit_fail_effect: CritFailEffect::Miss,
                crit_fail_chance: 0.0,
            },
        }
    }

    /// `[Move, Move, Cast@focus]` → committed_step_count == 1. The Cast on
    /// step-2 targets the FocusTarget intent (intent_score = 1.0), but never
    /// fires this tick. Intent factor must be 0.0 — otherwise plans with
    /// "great intent alignment buried in the uncommitted tail" would
    /// out-score honest plans that commit to an intent-aligned first step.
    #[test]
    fn intent_factor_ignores_uncommitted_tail_cast() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let focus = unit(2, Team::Player, hex_from_offset(5, 0));
        let actor_id = actor.entity;
        let focus_id = focus.entity;

        let snap = BattleSnapshot {
            units: vec![actor.clone(), focus.clone()],
            active_unit: actor_id,
            round: 1,
        };
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let maps = empty_maps();
        let reservations = Reservations::default();

        // Same snap in every sim_snapshots slot — the test checks intent
        // aggregation, not sim accuracy. compute_plan_factors only reads
        // sim_snapshots[idx-1] to pull `sim_actor`; equal snapshots suffice.
        let plan = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(0, 1)] },
                PlanStep::Move { path: vec![hex_from_offset(0, 2)] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: focus_id,
                    target_pos: focus.pos,
                },
            ],
            final_pos: hex_from_offset(0, 2),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![
                StepOutcome::default(),
                StepOutcome::default(),
                StepOutcome::default(),
            ],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone(), snap.clone()],
        };
        assert_eq!(plan.committed_step_count(), 1, "solo Move commits 1 step");

        let intent = TacticalIntent::FocusTarget { target: focus_id };
        let factors = compute_plan_factors(
            &plan, &actor, &intent, &ctx, &snap, &maps, &reservations,
        );
        // Index 7 is the intent factor. Committed step 0 is a Move → 0.0;
        // step 2 Cast on focus (intent_score=1.0) is beyond the committed
        // prefix and must not lift the factor.
        assert_eq!(
            factors[7], 0.0,
            "uncommitted tail Cast must not contribute (got {})",
            factors[7],
        );
    }

    /// `[Move, Cast@focus]` → committed_step_count == 2 (Move+Cast bundle
    /// fires atomically this tick). The Cast on focus IS within the
    /// committed prefix, so intent factor should be 1.0.
    #[test]
    fn intent_factor_includes_bundled_cast() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let focus = unit(2, Team::Player, hex_from_offset(2, 0));
        let actor_id = actor.entity;
        let focus_id = focus.entity;

        let snap = BattleSnapshot {
            units: vec![actor.clone(), focus.clone()],
            active_unit: actor_id,
            round: 1,
        };
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let maps = empty_maps();
        let reservations = Reservations::default();

        let plan = TurnPlan {
            steps: vec![
                PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
                PlanStep::Cast {
                    ability: "melee_attack".into(),
                    target: focus_id,
                    target_pos: focus.pos,
                },
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 1,
            residual_mp: 2,
            outcomes: vec![StepOutcome::default(), StepOutcome::default()],
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };
        assert_eq!(plan.committed_step_count(), 2, "Move+Cast bundle commits 2");

        let intent = TacticalIntent::FocusTarget { target: focus_id };
        let factors = compute_plan_factors(
            &plan, &actor, &intent, &ctx, &snap, &maps, &reservations,
        );
        assert!(
            (factors[7] - 1.0).abs() < 0.001,
            "bundled Cast on focus should yield intent factor 1.0 (got {})",
            factors[7],
        );
    }
}
