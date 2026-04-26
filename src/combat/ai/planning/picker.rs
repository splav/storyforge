//! Final plan selection: mercy tie-breaker + top-K window + commitment to the
//! first step as an `AiDecision`.

#![allow(clippy::too_many_arguments)]

/// Raw mechanics output from `pick_best_plan`. The outer layer converts pool
/// indices into human-readable labels for debug output.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct PickMechanics {
    pub top_k: usize,
    pub window: f32,
    pub mercy_margin: f32,
    pub mercy_applied: bool,
    /// `(plan_index, final_score)` in pool order.
    pub pool: Vec<(usize, f32)>,
    pub chosen_pos: usize,
}

use crate::combat::ai::factors::{aoe_area, aoe_hits, PlanFactors};
use crate::combat::ai::planning::types::{CommittedPrefix, PlanStep, TurnPlan};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{AiDecision, AiWorld, MoveOrigin};
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::DiceRng;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

/// Commit the winning plan's first step (or first two, if they're a
/// Move→Cast bundle) as a single `AiDecision`, along with how many steps
/// of the plan the decision consumed. The remainder of the plan is
/// discarded — every AI tick re-plans from scratch.
///
/// Bundling rules (`consumed` follows the match arm):
/// - Empty plan → `EndTurn`, 0 steps.
/// - `[Cast, ..]` → `CastInPlace`, 1 step.
/// - `[Move, Cast, ..]` → `MoveAndCast` (or `CastInPlace` if the move path
///   is empty), 2 steps. One atomic tick preserves the engine contract
///   (one `UseAbility` per actor-turn pathfind).
/// - `[Move, ..]` → `Move { origin: BestPlan }` (or `EndTurn` when the path is
///   a no-op), 1 step.
pub fn commit_plan(plan: &TurnPlan, actor_pos: Hex) -> (AiDecision, usize) {
    // Structural decomposition lives on TurnPlan; we only decide how each
    // prefix shape maps to an `AiDecision` (with a few no-op short-circuits
    // for empty-path degenerate cases).
    let prefix = plan.committed_prefix();
    let consumed = prefix.step_count();
    let decision = match prefix {
        CommittedPrefix::EndTurn => AiDecision::EndTurn,
        CommittedPrefix::Cast { ability, target, target_pos } => AiDecision::CastInPlace {
            ability: ability.clone(),
            target,
            target_pos,
        },
        CommittedPrefix::MoveThenCast { path, ability, target, target_pos } => {
            // Degenerate bundle (empty move path) collapses to a bare cast.
            if path.is_empty() {
                AiDecision::CastInPlace {
                    ability: ability.clone(),
                    target,
                    target_pos,
                }
            } else {
                AiDecision::MoveAndCast {
                    path: path.to_vec(),
                    ability: ability.clone(),
                    target,
                    target_pos,
                }
            }
        }
        CommittedPrefix::MoveOnly { path } => {
            // Degenerate move (empty path or stays put) ends the turn instead.
            let dest = path.last().copied().unwrap_or(actor_pos);
            if path.is_empty() || dest == actor_pos {
                AiDecision::EndTurn
            } else {
                AiDecision::Move {
                    path: path.to_vec(),
                    origin: MoveOrigin::BestPlan,
                }
            }
        }
    };
    (decision, consumed)
}

/// Mercy cruelty for a plan: how harsh does it feel? Kill dominates; CC caps
/// at 0.5 regardless of magnitude. Reads the **precomputed** raw factor row
/// for `plan` — previously we re-ran `compute_plan_factors` per plan in the
/// mercy window, which was a full plan-walk + per-step factor recomputation
/// just to grab two numbers we already had.
fn mercy_cruelty(raw: &PlanFactors) -> f32 {
    raw.kill_now + raw.kill_promised * 0.5 + (raw.cc * 0.1).min(0.5)
}

/// Pick the winning plan. Mirrors `pick_best_candidate` — window-bounded top-K
/// sampling with a mercy tie-breaker applied only inside the near-best window.
///
/// Always returns the `PickMechanics` breakdown (top_k, window, mercy
/// bookkeeping, ranked pool). The pool is ≤ `top_k` elements (1-3 in practice),
/// so the allocation is ~24 bytes on the stack / small-Vec region — too cheap
/// to justify a dual streaming-vs-materialize path. Prod callers ignore the
/// mechanics; debug overlay reads it.
pub fn pick_best_plan(
    scored: &[f32],
    raw_factors: &[PlanFactors],
    ctx: &AiWorld,
    rng: &mut DiceRng,
) -> (usize, PickMechanics) {
    let top_k_req = ctx.difficulty.top_k_choice();
    let m = ctx.difficulty.mercy_margin();
    let window = (ctx.difficulty.score_noise() * 2.0).max(0.05);

    let make_mech = |top_k, mercy_applied, pool, chosen_pos| PickMechanics {
        top_k,
        window,
        mercy_margin: m,
        mercy_applied,
        pool,
        chosen_pos,
    };

    if scored.is_empty() {
        return (0, make_mech(top_k_req, false, vec![], 0));
    }

    let mut ranked: Vec<(usize, f32)> = scored.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let best_score = ranked[0].1;
    let mut mercy_applied = false;
    if m > 0.0 && best_score.is_finite() {
        let mercy_end = ranked
            .iter()
            .position(|(_, s)| !s.is_finite() || *s < best_score - m)
            .unwrap_or(ranked.len());
        if mercy_end > 1 {
            let mut windowed: Vec<(usize, f32)> = ranked[..mercy_end]
                .iter()
                .map(|&(i, s)| (i, s - m * mercy_cruelty(&raw_factors[i])))
                .collect();
            windowed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (slot, item) in windowed.into_iter().enumerate() {
                ranked[slot] = item;
            }
            mercy_applied = true;
        }
    }

    let k = top_k_req.max(1).min(ranked.len());
    let best_after = ranked[0].1;

    let pool: Vec<(usize, f32)> = ranked
        .iter()
        .take(k)
        .filter(|(_, s)| s.is_finite() && *s >= best_after - window)
        .map(|&(i, s)| (i, s))
        .collect();

    if pool.is_empty() {
        return (
            ranked[0].0,
            make_mech(k, mercy_applied, vec![(ranked[0].0, ranked[0].1)], 0),
        );
    }
    // `roll_d(N)` returns `1..=N`; shift to a 0-based index.
    let chosen_pos = (rng.roll_d(pool.len() as u32) - 1) as usize;
    let chosen_idx = pool[chosen_pos].0;
    (chosen_idx, make_mech(k, mercy_applied, pool, chosen_pos))
}

/// Record reservations for the **committed** prefix of the winning plan so
/// subsequent AI units this round coordinate (avoid overkill, duplicate CC,
/// tile collisions). Only the first `consumed` steps — the ones this tick
/// actually emits as an `AiDecision` — are recorded. Future plan steps stay
/// invisible to the reservation layer until they themselves commit on a later
/// tick; this trades a slightly weaker coordination signal for freedom from
/// ghost reservations when plans get invalidated mid-flight.
///
/// `consumed` comes from `steps_consumed_by_decision` and matches the match
/// arm in `decision_from_steps` (1 for a solo cast/move, 2 for a Move→Cast
/// bundle).
pub fn record_committed_reservations(
    plan: &TurnPlan,
    consumed: usize,
    active: &UnitSnapshot,
    ctx: &AiWorld,
    snap: &BattleSnapshot,
    reservations: &mut Reservations,
    actor_pos: Hex,
) {
    let mut resting_tile = actor_pos;
    for (idx, step, caster_tile) in plan.walk_with_caster(actor_pos).take(consumed) {
        // After a Move, `walk_with_caster` advances to the destination on
        // the *next* yield — track the post-step caster ourselves so the
        // final reservation uses the resting tile after the committed prefix.
        if let PlanStep::Move { path } = step {
            if let Some(&dest) = path.last() {
                resting_tile = dest;
            }
        }
        let PlanStep::Cast { ability, target, target_pos } = step else { continue };
        let Some(def) = ctx.content.abilities.get(ability) else { continue };
        let is_cc = applies_cc(def, ctx.content);
        let hits: Vec<Entity> = if def.aoe == AoEShape::None {
            vec![*target]
        } else {
            let area = aoe_area(def, *target_pos, caster_tile);
            aoe_hits(&area, active, snap)
                .enemies
                .iter()
                .map(|e| e.entity)
                .collect()
        };
        for ent in hits {
            if let Some(_target_unit) = snap.unit(ent) {
                if def.target_type != TargetType::SingleAlly {
                    // Use the sim-populated expected_damage from PlanAnnotation —
                    // it reflects the actual projected damage for this step and
                    // avoids re-deriving via compute_score_core for reservation bookkeeping.
                    let dmg = plan.annotation.outcomes.get(idx).map_or(0.0, |o| o.expected_damage);
                    if dmg > 0.0 {
                        reservations.reserve_damage(ent, dmg);
                    }
                }
                if is_cc {
                    reservations.reserve_cc(ent);
                }
            }
        }
    }

    // Reserve the tile we'll actually stop on this tick (end of the committed
    // prefix), not the plan's eventual `final_pos` — same no-ghost principle.
    if resting_tile != actor_pos {
        reservations.reserve_tile(resting_tile);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AbilityId;
    use crate::game::hex::hex_from_offset;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    // ── commit_plan: (decision, consumed) shape for each plan arm ──────────

    fn plan_from(steps: Vec<PlanStep>) -> TurnPlan {
        TurnPlan {
            steps,
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    #[test]
    fn commit_empty_plan_ends_turn() {
        let (decision, consumed) = commit_plan(&plan_from(vec![]), hex_from_offset(0, 0));
        assert!(matches!(decision, AiDecision::EndTurn));
        assert_eq!(consumed, 0);
    }

    #[test]
    fn commit_solo_cast_consumes_one() {
        let plan = plan_from(vec![PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target: ent(1),
            target_pos: hex_from_offset(0, 0),
        }]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert!(matches!(decision, AiDecision::CastInPlace { .. }));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn commit_move_cast_bundles_into_two() {
        let plan = plan_from(vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(2),
                target_pos: hex_from_offset(2, 0),
            },
        ]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        match decision {
            AiDecision::MoveAndCast { path, ability, target, .. } => {
                assert_eq!(path.len(), 1);
                assert_eq!(ability.0, "strike");
                assert_eq!(target, ent(2));
            }
            other => panic!("expected MoveAndCast, got {:?}", std::mem::discriminant(&other)),
        }
        assert_eq!(consumed, 2);
    }

    #[test]
    fn commit_solo_move_consumes_one() {
        let plan = plan_from(vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert!(matches!(
            decision,
            AiDecision::Move { origin: MoveOrigin::BestPlan, .. }
        ));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn commit_move_move_keeps_first_only_no_bundle() {
        let plan = plan_from(vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Move { path: vec![hex_from_offset(2, 0)] },
        ]);
        let (_, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert_eq!(consumed, 1, "Move→Move does not bundle");
    }
}
