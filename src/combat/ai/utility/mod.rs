//! Top-level AI decision pipeline.
//!
//! Layout:
//! - `pick` — final top-K + mercy candidate selection, post-pick reservations.
//! - `sanity` — multiplicative penalties for dangerous/bad candidates + defensive classification.
//! - `fallback` — moves used when no cast candidates survive.

#![allow(clippy::too_many_arguments)]

mod fallback;
mod pick;
mod sanity;

pub use pick::PickMechanics;

pub use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};

use crate::content::content_view::ContentView;
use crate::combat::ai::debug::{build_debug_snapshot, build_fallback_debug, AiDebugSnapshot};
use crate::combat::ai::difficulty::DifficultyProfile;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    default_focus_target, intent_viability_threshold, select_intent, update_memory,
    AiMemory, TacticalIntent,
};
use crate::combat::ai::log::{self, AiLogger, IntentBlock};
use crate::combat::ai::planning::{
    apply_protect_self_mask, decision_from_plan, generate_plans, pick_best_plan,
    plan_to_candidate, record_plan_reservation, sanity_adjust_plans, score_plans_with_raw,
    PlanStep, TurnPlan,
};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::content::abilities::CasterContext;
use crate::content::races::CritFailEffect;
use crate::core::{AbilityId, DiceRng};
use crate::game::components::{Abilities, Team};
use crate::game::hex::Hex;
use crate::game::pathfinding::ReachableMap;
use bevy::prelude::*;
use std::collections::HashMap;

// ── Public types ────────────────────────────────────────────────────────────

pub enum AiDecision {
    CastInPlace {
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveAndCast {
        path: Vec<Hex>,
        ability: AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveCloser {
        path: Vec<Hex>,
    },
    MoveOnlyRetreat {
        path: Vec<Hex>,
    },
    EndTurn,
}

// ── Context ─────────────────────────────────────────────────────────────────

pub struct UtilityContext<'a> {
    pub content: &'a ContentView,
    pub difficulty: &'a DifficultyProfile,
    pub caster: &'a CasterContext,
    pub abilities: &'a Abilities,
    pub opponent_team: Team,
    pub crit_fail_effect: CritFailEffect,
    pub crit_fail_chance: f32,
    /// Every tile currently occupied by any entity tracked in `HexPositions`
    /// — **including dead units** (whose entries persist intentionally so
    /// corpses still block movement). Pathfinding-level check; the snapshot
    /// layer filters dead units out of `units` for scoring/targeting, and
    /// this set patches the gap so the planner doesn't route through a tile
    /// that's physically blocked.
    pub blocked_tiles: &'a std::collections::HashSet<crate::game::hex::Hex>,
}

/// Shared empty set for tests and scopes where no tile is considered blocked.
/// Safe to borrow at any lifetime thanks to the `'static` backing.
pub fn empty_blocked_tiles() -> &'static std::collections::HashSet<crate::game::hex::Hex> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static EMPTY: OnceLock<HashSet<crate::game::hex::Hex>> = OnceLock::new();
    EMPTY.get_or_init(HashSet::new)
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Top-level decision function. Replaces evaluate_targets + plan_movement.
/// When `debug` is true, returns an `AiDebugSnapshot` alongside the decision.
pub fn pick_action(
    actor: Entity,
    actor_pos: Hex,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
    rng: &mut DiceRng,
    memory: &mut AiMemory,
    reservations: &mut Reservations,
    logger: &mut AiLogger,
    debug: bool,
    debug_names: &HashMap<Entity, String>,
) -> (AiDecision, Option<AiDebugSnapshot>, Option<TurnPlan>) {
    let log_on = logger.is_enabled();
    let t0 = if log_on { Some(std::time::Instant::now()) } else { None };

    let Some(active) = snap.unit(actor) else {
        return (AiDecision::EndTurn, None, None);
    };

    // ── Select tactical intent ──────────────────────────────────────────
    let choice = select_intent(active, snap, maps, memory, ctx.difficulty);
    update_memory(memory, &choice.intent);
    let mut intent = choice.intent;
    let mut intent_reason = choice.reason;

    // ── Generate plans (beam search over depths) ───────────────────────
    let plans = generate_plans(actor, ctx, snap, maps);

    if plans.is_empty() {
        let decision = fallback::fallback_move(actor_pos, active, ctx, snap, reach, maps);
        let ds = if debug {
            Some(build_fallback_debug(
                active, actor_pos, &intent, &intent_reason, &decision,
                "no plans generated", ctx, snap, maps, debug_names,
            ))
        } else { None };
        return (decision, ds, None);
    }

    // ── Score plans under the chosen intent ────────────────────────────
    // Keep the raw-factor matrix for logging even after rescoring under a
    // fallback intent below; it's cheap to recompute and represents what the
    // winning decision was actually scored against.
    let (mut scored, mut raw_factors) =
        score_plans_with_raw(&plans, active, &intent, ctx, snap, maps, reservations, rng);

    // ── Intent viability guard ─────────────────────────────────────────
    // If no plan achieves the intent's signal, fall back. Two tiers:
    //   - midpanic: HP below midpanic_hp_threshold AND standing in danger →
    //     `ProtectSelf`. The actor can't execute the original intent *and*
    //     is too exposed to blindly push toward a fallback focus target.
    //   - default: reachable `FocusTarget` over a live enemy, same as before.
    // Plan generation is intent-agnostic, so rescoring against the same pool
    // is enough.
    if let Some(threshold) = intent_viability_threshold(&intent) {
        let max_align = max_intent_signal(&plans, &intent, active, ctx, snap, maps);
        if max_align < threshold {
            let hp_pct = active.hp_pct();
            let actor_danger = maps.danger.get(active.pos);
            let midpanic_hp = ctx.difficulty.midpanic_hp_threshold();
            let panic_danger = ctx.difficulty.awareness_danger_threshold();
            let midpanic = hp_pct < midpanic_hp && actor_danger > panic_danger;

            let new_intent = if midpanic {
                intent_reason = format!(
                    "midpanic_fallback: hp%={:.0}%<{:.0}% AND danger={:.2}>{:.2} (max_align={:.2}<{:.2})",
                    hp_pct * 100.0, midpanic_hp * 100.0,
                    actor_danger, panic_danger,
                    max_align, threshold,
                );
                Some(TacticalIntent::ProtectSelf)
            } else {
                let exclude = match &intent {
                    TacticalIntent::FocusTarget { target } => Some(*target),
                    _ => None,
                };
                let cand_stubs: Vec<_> = plans
                    .iter()
                    .map(|p| plan_to_candidate(p, actor_pos))
                    .collect();
                default_focus_target(active, snap, &cand_stubs, exclude).map(|t| {
                    let original_label = format!("{:?}", intent.kind());
                    intent_reason = format!(
                        "fallback from {}: max_align={:.2}<threshold={:.2}",
                        original_label, max_align, threshold,
                    );
                    TacticalIntent::FocusTarget { target: t }
                })
            };

            if let Some(new) = new_intent {
                if intent.kind() != new.kind() || intent.target() != new.target() {
                    intent = new;
                    let (new_scored, new_raw) = score_plans_with_raw(
                        &plans, active, &intent, ctx, snap, maps, reservations, rng,
                    );
                    scored = new_scored;
                    raw_factors = new_raw;
                }
            }
        }
    }

    // Sanity adjust: multiplicative penalties for situations the 9-factor
    // score can't catch (low-HP through AoO corridors, self-AoE, LOS
    // blindspots, retreat traps). Runs on all plans so low-ranked terrible
    // ones can't sneak up via noise.
    sanity_adjust_plans(&mut scored, &plans, active, snap, maps, ctx);

    // ProtectSelf mask: when intent is (or fell to) ProtectSelf, mask any
    // plan whose first step isn't defensive to -∞. This is where the intent
    // gets real teeth — without it, "I want to protect myself" is just a
    // +1.0 intent factor on a few candidates, easily out-scored by high-
    // damage offensive plans. If no plan is defensive (surrounded, no safe
    // move), LastStand rescoring takes over so the actor at least lands a
    // final useful hit.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let margin = ctx.difficulty.defensive_tile_margin();
        let any_defensive = apply_protect_self_mask(
            &mut scored, &plans, active, ctx.content, maps, margin,
        );
        if !any_defensive {
            let last_stand = TacticalIntent::LastStand;
            let (ls_scored, ls_raw) = score_plans_with_raw(
                &plans, active, &last_stand, ctx, snap, maps, reservations, rng,
            );
            scored = ls_scored;
            raw_factors = ls_raw;
            intent_reason = format!("{intent_reason} → LastStand (no defensive plan)");
        }
    }

    // Pick best plan via mercy + top-K window (same math as single-candidate pick).
    let (best_idx, pick_mech) = pick_best_plan(
        &scored, &plans, active, &intent, ctx, snap, maps, reservations, rng,
    );

    let best_plan = &plans[best_idx];
    let decision = decision_from_plan(best_plan, actor, actor_pos);

    // Build debug snapshot — reuse the existing formatter with synthesized
    // candidates (each plan → its committed first-tick action).
    let debug_snapshot = if debug {
        let cand_view: Vec<_> = plans.iter().map(|p| plan_to_candidate(p, actor_pos)).collect();
        Some(build_debug_snapshot(
            active, actor_pos, &intent, &intent_reason, &cand_view, &scored, &decision,
            ctx, snap, maps, reservations, debug_names, Some(&pick_mech),
        ))
    } else {
        None
    };

    // Record reservations for every cast in the winning plan.
    record_plan_reservation(best_plan, active, ctx, snap, reservations, actor_pos);

    // ── AI log: write structured entry for offline analysis ────────────
    if log_on {
        let decision_time_ms = t0.map_or(0, |t| t.elapsed().as_millis() as u64);
        let plan_id = logger.next_plan_id();

        // Rank plans by final score, keep top-10 for size budget.
        let mut indexed: Vec<(usize, f32)> =
            scored.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let shown = indexed.len().min(10);
        let plan_entries: Vec<_> = indexed
            .iter()
            .take(shown)
            .enumerate()
            .map(|(rank, &(idx, score))| {
                log::plan_to_log_entry(
                    &plans[idx],
                    rank + 1,
                    idx == best_idx,
                    raw_factors[idx],
                    score,
                )
            })
            .collect();

        let actor_name = debug_names
            .get(&actor)
            .map(|s| s.as_str())
            .unwrap_or("<unknown>");
        let intent_block = IntentBlock {
            intent: &intent,
            selection_kind: log::classify_selection(&intent_reason),
            reason_text: &intent_reason,
        };
        let entry = log::build_entry(
            plan_id, decision_time_ms, active, actor_name, snap, intent_block,
            plans.len(), shown, plan_entries, &decision,
        );
        if let Err(e) = logger.write_entry(&entry) {
            warn!("AI log write failed: {}", e);
        }
    }

    (decision, debug_snapshot, Some(best_plan.clone()))
}

/// Maximum per-step intent score across the plan pool. Used by the viability
/// guard to decide if the chosen intent can actually be executed.
fn max_intent_signal(
    plans: &[TurnPlan],
    intent: &TacticalIntent,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
) -> f32 {
    use crate::combat::ai::intent::intent_score;
    use crate::combat::ai::planning::SimState;

    let mut best = f32::NEG_INFINITY;
    for plan in plans {
        let mut sim = SimState::from_snapshot(snap, active.entity);
        for step in &plan.steps {
            let sim_actor = match sim.actor_unit() {
                Some(u) => u.clone(),
                None => break,
            };
            let cand = plan_step_to_candidate(step, &sim_actor.pos);
            let v = intent_score(intent, &cand, &sim_actor, &sim.snapshot, maps, ctx.content, ctx.difficulty);
            if v > best { best = v; }
            sim.apply_step(step, ctx.caster, ctx.content);
        }
    }
    if best.is_finite() { best } else { 0.0 }
}

fn plan_step_to_candidate(step: &PlanStep, from: &Hex) -> crate::combat::ai::candidates::ActionCandidate {
    use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
    match step {
        PlanStep::Cast { ability, target, target_pos } => ActionCandidate {
            tile: *from,
            path: Vec::new(),
            kind: CandidateKind::Cast {
                ability: ability.clone(),
                target_pos: *target_pos,
                target: Some(*target),
            },
        },
        PlanStep::Move { path } => ActionCandidate {
            tile: *path.last().unwrap_or(from),
            path: path.clone(),
            kind: CandidateKind::MoveOnly,
        },
    }
}
