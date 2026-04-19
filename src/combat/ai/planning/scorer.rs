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
//! - **Post-goal behavior**: once a step kills the current
//!   `FocusTarget`/`ApplyCC` target, the intent is satisfied. Subsequent
//!   steps skip the **intent** aggregation — they aren't aligned or
//!   misaligned, they're orthogonal to a now-solved goal. All other
//!   factors (damage, heal, cc, kill, focus, scarcity) continue at their
//!   normal geometric `base^k` decay. No extra multiplier — post-goal
//!   actions are scored on their own merit, neither penalised as
//!   "bonuses" nor inflated as "peers".
//! - `kill`: **discounted sum** of `raw_kill × step_weight` across Cast
//!   steps. Accumulates count of planned kills (each `raw_kill` is
//!   binary 0/1 from `single_target_kill`) with geometric decay — a
//!   plan killing two enemies outscores one killing one.
//! - `focus`: **discounted sum** of `target_priority × step_weight`
//!   across Cast steps. Two casts on priority targets outscore one;
//!   double-tapping the same target accumulates appropriately.
//! - `intent`: **discounted sum** of `intent_score × step_weight`
//!   across all steps (Cast and Move). Captures alignment across the
//!   whole plan, including misalign penalties on tail steps that do
//!   drag the signal down. Skipped once the intent's goal is achieved
//!   (see post-goal above).
//!
//! All three factors now share the same aggregation shape as damage /
//! cc / heal / scarcity: plan-wide cumulative with `base^k` decay per
//! depth. Single rule across every Cast-accumulating factor.
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

/// Top-level entry. Produces one composite score per plan plus the raw
/// pre-normalization factor matrix (so log writers / offline tools can
/// recalibrate weights without rerunning sim).
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
    let mut kill_sum = 0.0f32;
    let mut cc_sum = 0.0f32;
    let mut scarcity_sum = 0.0f32;
    let mut focus_sum = 0.0f32;
    let mut intent_sum = 0.0f32;
    let mut path_danger_max = maps.danger.get(active.pos);

    let base_discount = ctx.world.difficulty.plan_step_discount;
    let mut step_weight: f32 = 1.0;
    let mut goal_achieved = false;

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

        // Intent factor participates uniformly across Cast and Move steps,
        // accumulated with geometric decay. Skipped once the intent's goal
        // has been achieved earlier in the plan — further steps are
        // orthogonal to a solved intent.
        if !goal_achieved {
            let iv = intent_score(
                intent,
                &scored_step,
                &sim_actor,
                pre_snap,
                maps,
                ctx.world.content,
                ctx.world.difficulty,
            );
            intent_sum += iv * step_weight;
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
            // Every Cast-accumulating factor uses the same shape: discounted
            // sum with base^k decay. Deep Casts keep contributing but weigh
            // less, reflecting execution uncertainty over plan depth.
            damage_sum += raw[0] * step_weight;
            cc_sum += raw[2] * step_weight;
            heal_sum += raw[3] * step_weight;
            scarcity_sum += raw[8] * step_weight;
            kill_sum += raw[1] * step_weight;
            focus_sum += raw[6] * step_weight;
        }

        // Geometric per-step discount on the next step's contribution.
        step_weight *= base_discount;

        // Latch goal_achieved once a step's cached outcome kills the
        // intent's declared target (FocusTarget / ApplyCC). Only affects
        // intent aggregation (subsequent steps skip it); step_weight stays
        // purely geometric, so other factors score post-goal actions on
        // their own merit.
        if !goal_achieved {
            let killed = plan
                .outcomes
                .get(idx)
                .map(|o| o.killed.as_slice())
                .unwrap_or(&[]);
            if killed_intent_target(killed, intent) {
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
        focus_sum = snap
            .enemies_of(active.team)
            .map(|t| target_priority(active, t, snap))
            .fold(0.0f32, f32::max);
    }

    [
        damage_sum,
        kill_sum,
        cc_sum,
        heal_sum,
        position,
        risk,
        focus_sum,
        intent_sum,
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

    /// Under discounted-sum aggregation, a single Cast@focus at depth k
    /// contributes `intent_score × base^k` to intent_sum (and similarly
    /// `target_priority × base^k` to focus_sum). Move steps under
    /// FocusTarget intent score 0, so they don't accumulate intent.
    ///
    /// For a plan with exactly one Cast@focus, intent_sum equals the
    /// step_weight at the Cast step: 1.0 direct, 0.85 bundled, 0.72
    /// deep-3. This pins the aggregation shape.
    #[test]
    fn sum_factors_scale_by_step_weight() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let focus = unit(2, Team::Player, hex_from_offset(5, 0));
        let snap = BattleSnapshot {
            units: vec![actor.clone(), focus.clone()],
            round: 1,
        };
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let mut difficulty = DifficultyProfile::normal();
        difficulty.plan_step_discount = 0.85;
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: focus.entity };

        let cast_focus = || PlanStep::Cast {
            ability: "melee_attack".into(),
            target: focus.entity,
            target_pos: focus.pos,
        };
        let mov = |q, r| PlanStep::Move { path: vec![hex_from_offset(q, r)] };
        let build = |steps: Vec<PlanStep>| {
            let len = steps.len();
            TurnPlan {
                steps,
                final_pos: hex_from_offset(0, 0),
                residual_ap: 0,
                residual_mp: 0,
                outcomes: vec![StepOutcome::default(); len],
                partial_score: 0.0,
                sim_snapshots: vec![snap.clone(); len],
            }
        };

        // (description, plan, expected intent_sum). intent_score for
        // FocusTarget-match Cast = 1.0; Move = 0.0. Single Cast plan's
        // intent_sum equals step_weight at the Cast position.
        let cases: Vec<(&str, TurnPlan, f32)> = vec![
            ("direct cast — step 0, weight 1.0",
                build(vec![cast_focus()]),
                1.0),
            ("bundle cast — step 1, weight 0.85",
                build(vec![mov(1, 0), cast_focus()]),
                0.85),
            ("deep cast — step 2, weight 0.85²",
                build(vec![mov(0, 1), mov(0, 2), cast_focus()]),
                0.85 * 0.85),
        ];
        for (name, plan, expected_sum) in cases {
            let f = compute_plan_factors(
                &plan, &actor, &intent, &ctx, &snap, &maps, &reservations,
            );
            // factors: [0 dmg, 1 kill, 2 cc, 3 heal, 4 pos, 5 risk,
            //           6 focus, 7 intent, 8 scarcity]
            let intent_val = f[7];
            assert!(
                (intent_val - expected_sum).abs() < 0.005,
                "{name}: intent={intent_val}, expected≈{expected_sum}",
            );
            assert!(
                f[6] > 0.0,
                "{name}: focus_sum > 0 (Cast on priority target)",
            );
        }

        // Two-Cast plan accumulates: intent = 1.0 + 1.0×0.85 = 1.85.
        // Demonstrates that sum genuinely stacks signals, which max
        // used to collapse.
        let plan_double = build(vec![cast_focus(), cast_focus()]);
        let f = compute_plan_factors(
            &plan_double, &actor, &intent, &ctx, &snap, &maps, &reservations,
        );
        assert!(
            (f[7] - 1.85).abs() < 0.005,
            "double Cast@focus: intent_sum expected ≈ 1.85, got {}",
            f[7],
        );
    }

    /// Post-goal must not penalise further useful actions. Two identical
    /// two-Cast plans scored the same — one has step-0's cached `killed`
    /// listing the intent target (goal achieved), the other doesn't.
    /// Their `damage_sum` must match: step_weight stays pure geometric,
    /// without the old ×0.5 post-goal bump that used to halve subsequent
    /// step contributions.
    #[test]
    fn post_goal_leaves_step_weight_purely_geometric() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let target = unit(2, Team::Player, hex_from_offset(1, 0));
        let other = unit(3, Team::Player, hex_from_offset(2, 0));
        let snap = BattleSnapshot {
            units: vec![actor.clone(), target.clone(), other.clone()],
            round: 1,
        };
        let content =
            crate::content::content_view::ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::normal();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };

        let steps = vec![
            PlanStep::Cast {
                ability: "melee_attack".into(),
                target: target.entity,
                target_pos: target.pos,
            },
            PlanStep::Cast {
                ability: "melee_attack".into(),
                target: other.entity,
                target_pos: other.pos,
            },
        ];
        let mk = |outcomes: Vec<StepOutcome>| TurnPlan {
            steps: steps.clone(),
            final_pos: actor.pos,
            residual_ap: 0,
            residual_mp: 3,
            outcomes,
            partial_score: 0.0,
            sim_snapshots: vec![snap.clone(), snap.clone()],
        };

        let goal_achieved = mk(vec![
            StepOutcome { killed: vec![target.entity], ..Default::default() },
            StepOutcome::default(),
        ]);
        let goal_missed = mk(vec![
            StepOutcome::default(),
            StepOutcome::default(),
        ]);

        let f_goal =
            compute_plan_factors(&goal_achieved, &actor, &intent, &ctx, &snap, &maps, &reservations);
        let f_miss =
            compute_plan_factors(&goal_missed, &actor, &intent, &ctx, &snap, &maps, &reservations);

        // step_weight stays purely geometric — every Cast-accumulating
        // factor should be equal between the two plans regardless of
        // whether step 0's outcome killed the intent target. Intent
        // itself does differ (post-goal skips it), not asserted here.
        for (i, name) in [
            (0, "damage"),
            (1, "kill"),
            (2, "cc"),
            (3, "heal"),
            (6, "focus"),
            (8, "scarcity"),
        ] {
            assert_eq!(
                f_goal[i], f_miss[i],
                "{name}_sum must not depend on intent-kill status (step_weight stays geometric)",
            );
        }
    }
}
