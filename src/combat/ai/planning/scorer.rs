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
//! - `kill`: max across Cast steps **in the committed prefix** (binary "did
//!   this tick's commit kill anyone?"), not discounted. Gated for the same
//!   reason as `intent`: a tail Cast the plan never fires shouldn't claim
//!   credit for a kill.
//! - `focus`: max target_priority across Cast steps **in the committed
//!   prefix**, not discounted. Same rationale.
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
        // committed prefix (tail steps don't fire this tick) and skipped
        // once the intent's goal is already achieved (further steps are
        // orthogonal — neither aligned nor misaligned with a solved intent).
        if idx < committed && !goal_achieved {
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
            // Discounted cumulative factors — tail Casts keep contributing,
            // but the geometric discount reflects execution uncertainty.
            damage_sum += raw[0] * step_weight;
            cc_sum += raw[2] * step_weight;
            heal_sum += raw[3] * step_weight;
            scarcity_sum += raw[8] * step_weight;
            // Max outcome/priority signals — only from Casts that actually
            // commit this tick (see module doc). Otherwise a plan like
            // `[Move, Move, Cast-kill-X]` would claim kill=1 on a Cast
            // that next tick's re-plan will never fire as step-0.
            if idx < committed {
                if raw[1] > kill_max {
                    kill_max = raw[1];
                }
                if raw[6] > focus_max {
                    focus_max = raw[6];
                }
            }
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

    /// Max-aggregated factors (intent, kill, focus) must reflect what
    /// `commit_plan` actually fires this tick — not the uncommitted tail.
    /// Plans like `[Move-random, Move-random, Cast@focus]` used to steal
    /// intent=1 / focus=high from a Cast that will never fire as step-0.
    #[test]
    fn max_factors_respect_committed_prefix() {
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
        let intent = TacticalIntent::FocusTarget { target: focus_id };

        // Build a plan with the given leading steps; pads sim_snapshots with
        // the caller's snap — scorer only reads them for pre-step `sim_actor`
        // extraction, equal snapshots suffice for these tests.
        let build_plan = |steps: Vec<PlanStep>| {
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
        let cast_on_focus = || PlanStep::Cast {
            ability: "melee_attack".into(),
            target: focus_id,
            target_pos: focus.pos,
        };
        let mov = |q, r| PlanStep::Move { path: vec![hex_from_offset(q, r)] };

        // (plan description, plan, expected committed count, signal expected)
        enum Signal { Zero, Positive }
        let cases: Vec<(&str, TurnPlan, usize, Signal)> = vec![
            (
                "Move-Move-Cast@focus — tail Cast doesn't count",
                build_plan(vec![mov(0, 1), mov(0, 2), cast_on_focus()]),
                1,
                Signal::Zero,
            ),
            (
                "Move-Cast@focus bundle — Cast is in committed prefix",
                build_plan(vec![mov(1, 0), cast_on_focus()]),
                2,
                Signal::Positive,
            ),
        ];

        for (name, plan, want_commit, want_signal) in cases {
            assert_eq!(plan.committed_step_count(), want_commit, "{name}: commit count");
            let f = compute_plan_factors(
                &plan, &actor, &intent, &ctx, &snap, &maps, &reservations,
            );
            // factors: [0 dmg, 1 kill, 2 cc, 3 heal, 4 pos, 5 risk,
            //           6 focus, 7 intent, 8 scarcity]
            let (intent_val, focus_val) = (f[7], f[6]);
            match want_signal {
                Signal::Zero => {
                    assert_eq!(intent_val, 0.0, "{name}: intent");
                    assert_eq!(focus_val, 0.0, "{name}: focus");
                }
                Signal::Positive => {
                    assert!(intent_val > 0.0, "{name}: intent = {intent_val}");
                    assert!(focus_val > 0.0, "{name}: focus = {focus_val}");
                }
            }
        }
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
            active_unit: actor.entity,
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

        // damage_sum (index 0) pre-fix: goal-achieved plan halved step-1
        // contribution → strictly less than goal-missed. New semantics:
        // step_weight geometric only, so both plans score identically on
        // any Cast-accumulating factor.
        for (i, name) in [
            (0, "damage"), (2, "cc"), (3, "heal"), (8, "scarcity"),
        ] {
            assert_eq!(
                f_goal[i], f_miss[i],
                "{name}_sum must not depend on intent-kill status (step_weight stays geometric)",
            );
        }
    }
}
