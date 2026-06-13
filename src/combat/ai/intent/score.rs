use super::kinds::TacticalIntent;
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::scoring::factors::ScoredStep;
use crate::combat::ai::scoring::factors::{aoe_area, aoe_hits, StepFactor};
use crate::combat::ai::scoring::position_eval::evaluate_position;
use crate::combat::ai::world::snapshot::UnitView;
#[cfg(test)]
use crate::content::abilities::EffectCalcExt;
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ActiveContentData;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

// ── Pursuit (Move alignment under FocusTarget / ApplyCC) ───────────────────

/// Score a pure Move step by how much it closes the gap to the intent's
/// target, with an explicit reward for entering a "threat bubble" from
/// which the actor will be able to act on its target on the next
/// meaningful action.
///
/// # Signature
///
/// Takes `from_pos` / `to_pos` / `target_pos` explicitly rather than
/// reading `active.pos`. The scorer calls `intent_score` per step with
/// `active = sim_actor` (pre-step perspective), so reading `active.pos`
/// would work today — but the coupling is implicit and brittle. Explicit
/// positions make the helper self-contained and trivially unit-testable.
///
/// # Reach semantics
///
/// Caller picks `reach` to match the intent:
/// - `FocusTarget`: `active.speed + active.max_attack_range` — "will I be
///   able to hit on my next action window".
/// - `ApplyCC`: `active.speed + cc_reach(active, content)` — same shape
///   but measured against the longest-range CC-capable ability.
///
/// Using just `max_attack_range` (without `speed`) would miss the whole
/// point for melee pursuers: a warrior 2 tiles from the target after a
/// move that cuts 3 tiles of distance is semantically "about to engage",
/// and the signal must reflect that.
///
/// # Score shape
///
/// - `new_dist ≤ reach` → `0.8` — entered threat bubble. Strong but still
///   below a direct Cast (`1.0`), so Cast plans always win when castable.
/// - closing (`delta > 0`) → `0.3 × delta/reach`, capped at `0.3`. Mild
///   positive, can't spoof the viability threshold (`0.5` for
///   FocusTarget/ApplyCC) on its own.
/// - retreat (`delta < 0`) → `-0.1 × |delta|/reach`, capped at `0.1`.
///   Proportional and soft — a temporary step backward around a choke or
///   an obstacle barely registers, position/risk factors handle the rest.
/// - no change → `0.0`.
pub fn pursuit_move_score(from_pos: Hex, to_pos: Hex, target_pos: Hex, reach: u32) -> f32 {
    let new_dist = to_pos.unsigned_distance_to(target_pos);
    if new_dist <= reach {
        return 0.8;
    }
    let reach_f = reach.max(1) as f32;
    let cur_dist = from_pos.unsigned_distance_to(target_pos) as i32;
    let delta = cur_dist - new_dist as i32;
    if delta > 0 {
        (0.3 * delta as f32 / reach_f).min(0.3)
    } else if delta < 0 {
        -(0.1 * ((-delta) as f32 / reach_f).min(1.0))
    } else {
        0.0
    }
}

/// Longest CC-capable range in the actor's kit. Used by `ApplyCC`
/// pursuit scoring to define the "engagement horizon" — a Move that
/// brings the actor within `speed + cc_reach` of the CC target is
/// setting up a next-turn stun, which is the whole point of the intent.
///
/// Falls back to `max_attack_range` when the actor has no CC-tagged
/// ability (e.g. weapon-attached stun via status that doesn't fire
/// `applies_cc`). Conservative default — won't over-promise.
pub fn cc_reach(active: UnitView<'_>, content: &ActiveContentData) -> u32 {
    active
        .cache
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| applies_cc(def, content))
        .map(|def| def.range.max)
        .max()
        .unwrap_or(active.cache.max_attack_range)
}

// ── IntentWeights ────────────────────────────────────────────────────────────

/// Per-intent weight vector for the four offensive axes (damage, kill_now, kill_promised, cc).
///
/// Only the fields explicitly set matter; all others default to 0.0. Builder
/// methods mirror the field names for readable declarations:
/// `IntentWeights::default().kill_now(2.0).damage(1.0)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IntentWeights {
    pub damage: f32,
    pub kill_now: f32,
    pub kill_promised: f32,
    pub cc: f32,
}

impl IntentWeights {
    pub fn damage(mut self, w: f32) -> Self {
        self.damage = w;
        self
    }
    pub fn kill_now(mut self, w: f32) -> Self {
        self.kill_now = w;
        self
    }
    pub fn kill_promised(mut self, w: f32) -> Self {
        self.kill_promised = w;
        self
    }
    pub fn cc(mut self, w: f32) -> Self {
        self.cc = w;
        self
    }
}

// ── Narrow offensive API ─────────────────────────────────────────────────────

/// Score the offensive value of `step` from the perspective of `focus`.
///
/// Returns 0 if `step` is a Move, or if it targets a non-focus entity and
/// is not an AoE that covers the focus tile.
///
/// Used by `FocusTarget` and `ApplyCC` intent branches to compute
/// the weighted offensive score for a single step with focus-target filtering.
pub(crate) fn intent_offensive_value_on_target(
    focus: Entity,
    step: &ScoredStep,
    ctx: &ScoringCtx,
    outcome: &ActionOutcomeEstimate,
    weights: &IntentWeights,
    content: &ActiveContentData,
) -> f32 {
    let snap = ctx.snap;
    let needs = ctx.need_signals;

    let scale = match step {
        ScoredStep::Move { .. } => return 0.0,
        ScoredStep::Cast {
            ability,
            target,
            target_pos,
            caster_tile,
        } => {
            if *target == focus {
                1.0
            } else if let Some(def) = content.abilities.get(*ability) {
                if def.aoe != AoEShape::None {
                    if let Some(focus_unit) = snap.unit(focus) {
                        let area = aoe_area(def, *target_pos, *caster_tile);
                        if area.contains(&focus_unit.pos) {
                            0.6
                        } else {
                            return 0.0;
                        }
                    } else {
                        return 0.0;
                    }
                } else {
                    return 0.0;
                }
            } else {
                return 0.0;
            }
        }
    };

    let damage = StepFactor::Damage.compute(ctx, step, outcome, &needs);
    let kill_now = StepFactor::KillNow.compute(ctx, step, outcome, &needs);
    let kill_prom = StepFactor::KillPromised.compute(ctx, step, outcome, &needs);
    let cc = StepFactor::Cc.compute(ctx, step, outcome, &needs);

    (weights.damage * damage
        + weights.kill_now * kill_now
        + weights.kill_promised * kill_prom
        + weights.cc * cc)
        * scale
}

// ── Intent → utility score (factor[7]) ──────────────────────────────────────

// Compute how well a scored step aligns with the current intent.
// Positive = aligned, zero = neutral, negative = misaligned (soft penalty).
//
// Uses a dot-product of per-step impact factors against intent-specific weight
// vectors (via `IntentWeights`) for `FocusTarget` and `ApplyCC`. This makes
// alignment proportional to actual impact magnitude — a hit doing 10 damage
// outscores a hit doing 1 damage, fixing S5 (low-value armor hits getting full
// intent credit under the old hardcoded 1.0 return).
//
// `ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand` preserve their
// existing formulas (ported to the new signature).
// ── LastStand step scorer ──────────────────────────────────────────────────

/// Score a single step under the **LastStand** evaluation regime.
///
/// Used when `EvaluationMode::LastStand` is active — the actor is committed to
/// a "final useful action" and survival considerations are secondary to impact.
///
/// Hierarchy: kill (CC bonus) > AoE > direct offensive > survival cast > running.
pub fn evaluate_last_stand_step(step: &ScoredStep, step_ctx: &ScoringCtx) -> f32 {
    let content = step_ctx.world.content;
    let snap = step_ctx.snap;
    let active = step_ctx.active;

    let cast = match step {
        ScoredStep::Cast {
            ability,
            target_pos,
            target,
            ..
        } => Some((*ability, *target_pos, *target)),
        ScoredStep::Move { .. } => None,
    };

    let Some((ability, _, target)) = cast else {
        // LastStand wants last useful action, not running.
        return -0.3;
    };
    let Some(def) = content.abilities.get(ability) else {
        return 0.0;
    };
    let mut score = 0.0f32;

    // "Direct offensive action" bonus in LastStand: covers both
    // entity-targeted (SingleEnemy) and cell-targeted (Ground)
    // attacks. AoE footprint gets an additional +0.3 below.
    if matches!(
        def.target_type,
        TargetType::SingleEnemy | TargetType::Ground
    ) {
        score += 0.5;
    }
    if let Some(target_unit) = snap.unit(target) {
        if applies_cc(def, content) && !target_unit.is_stunned(step_ctx.world.status_tags) {
            score += 0.8;
        }
    }
    if def.aoe != AoEShape::None {
        score += 0.3;
    }
    if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
        score += 0.1;
    }

    let _ = active; // active may be used for future extensions
    score
}

/// Score a single step under the **Flee** evaluation regime.
///
/// The fleeing unit maximises distance from the nearest enemy.
/// Offensive casts are suppressed (scored lowest); self-heal/self-buff allowed.
///
/// Score shape:
/// - `Move`: positive delta = moved farther away; non-improving ≤ 0; no enemies = 0.
/// - `Cast` offensive (`SingleEnemy | Ground`): `-1.0` (suppressed).
/// - `Cast` self-targeted (`Myself | SingleAlly` on self): `+0.3` (allowed).
/// - Other casts: `0.0` (neutral).
pub fn evaluate_flee_step(step: &ScoredStep, step_ctx: &ScoringCtx) -> f32 {
    let content = step_ctx.world.content;
    let active = step_ctx.active;

    // Collect enemies.
    let enemies: Vec<_> = step_ctx.snap.enemies_of(active.team).collect();

    // Distance to nearest enemy from a given tile (None if no enemies).
    let nearest = |tile: Hex| -> Option<u32> {
        enemies
            .iter()
            .map(|e| tile.unsigned_distance_to(e.pos))
            .min()
    };

    match step {
        ScoredStep::Move { caster_tile } => {
            // Return positive for moves that increase distance to nearest enemy.
            match (nearest(active.pos), nearest(*caster_tile)) {
                (Some(cur), Some(new)) => new as f32 - cur as f32,
                _ => 0.0, // No enemies present; no reason to flee.
            }
        }
        ScoredStep::Cast {
            ability, target, ..
        } => {
            if nearest(active.pos).is_none() {
                // No enemies present — no flee objective; neutral.
                return 0.0;
            }
            let Some(def) = content.abilities.get(*ability) else {
                return 0.0;
            };
            match def.target_type {
                // Offensive: suppressed — score lowest, will not be chosen.
                TargetType::SingleEnemy | TargetType::Ground => -1.0,
                // Self-targeted heal/buff: allowed.
                TargetType::Myself | TargetType::SingleAlly if *target == active.entity() => 0.3,
                // Other (ally-targeted non-self, etc.): neutral.
                _ => 0.0,
            }
        }
    }
}

pub fn intent_score(
    intent: &TacticalIntent,
    step: &ScoredStep,
    step_ctx: &ScoringCtx,
    outcome: &ActionOutcomeEstimate,
    mode: EvaluationMode,
) -> f32 {
    // LastStand evaluation regime: bypass intent-specific scoring.
    if mode == EvaluationMode::LastStand {
        return evaluate_last_stand_step(step, step_ctx);
    }
    // Flee evaluation regime: maximise distance from nearest enemy.
    if mode == EvaluationMode::Flee {
        return evaluate_flee_step(step, step_ctx);
    }
    let active = step_ctx.active;
    let snap = step_ctx.snap;
    let maps = step_ctx.maps;
    let content = step_ctx.world.content;
    let difficulty = step_ctx.world.difficulty;
    let mild_penalty = step_ctx.world.tuning.thresholds.mild_penalty;

    // Move steps: scored only on position-related intent axes.
    let cast = match step {
        ScoredStep::Cast {
            ability,
            target_pos,
            target,
            ..
        } => Some((*ability, *target_pos, *target)),
        ScoredStep::Move { .. } => None,
    };

    match intent {
        TacticalIntent::FocusTarget { target: focus } => {
            if cast.is_none() {
                // Pure move: pursuit geometry hook.
                return match snap.unit(*focus) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(active.cache.max_attack_range);
                        pursuit_move_score(active.pos, step.caster_tile(), t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Cast: score offensive value via narrow API (focus-target filtered).
            let weights = IntentWeights::default()
                .kill_now(2.0)
                .kill_promised(0.3)
                .damage(1.0)
                .cc(0.5);
            intent_offensive_value_on_target(*focus, step, step_ctx, outcome, &weights, content)
        }
        TacticalIntent::ApplyCC { target: cc_target } => {
            if cast.is_none() {
                // Pure move during ApplyCC: reach uses CC-capable range.
                return match snap.unit(*cc_target) {
                    Some(t) => {
                        let reach =
                            (active.speed.max(0) as u32).saturating_add(cc_reach(active, content));
                        pursuit_move_score(active.pos, step.caster_tile(), t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Cast: score offensive value via narrow API (CC-target filtered).
            let weights = IntentWeights::default().cc(1.5).damage(0.3);
            intent_offensive_value_on_target(*cc_target, step, step_ctx, outcome, &weights, content)
        }
        TacticalIntent::Reposition => {
            // Tiered: strong improvement rewarded, any improvement neutral,
            // no improvement penalized — mildly if casting, hard if just moving.
            let current =
                evaluate_position(active.pos, &active.cache.role, step_ctx.world.tuning, maps);
            let new = evaluate_position(
                step.caster_tile(),
                &active.cache.role,
                step_ctx.world.tuning,
                maps,
            );
            let improvement = new - current;
            let min_improv = difficulty.reposition_min_improvement(step_ctx.world.tuning);
            if improvement >= min_improv {
                improvement.min(2.0)
            } else if improvement > 0.0 {
                0.0
            } else if cast.is_some() {
                -0.3
            } else {
                -1.0
            }
        }
        TacticalIntent::ProtectSelf => {
            // Self-directed defensive casts (self-heal, self-buff on Myself or
            // SingleAlly aimed at caster) are full ProtectSelf alignment —
            // staying put to save yourself is protecting self, regardless of
            // tile danger. Otherwise use tile safety.
            if let Some((ability, _, target)) = cast {
                if target == active.entity() {
                    if let Some(def) = content.abilities.get(ability) {
                        if matches!(def.target_type, TargetType::SingleAlly | TargetType::Myself) {
                            return 1.0;
                        }
                    }
                }
            }
            1.0 - maps.danger.get(step.caster_tile())
        }
        TacticalIntent::ProtectAlly { ally } => match cast {
            Some((ability, _, target)) => {
                let Some(def) = content.abilities.get(ability) else {
                    return 0.0;
                };
                if def.target_type == TargetType::SingleAlly {
                    if target == *ally {
                        1.0
                    } else {
                        mild_penalty
                    }
                } else if snap
                    .unit(*ally)
                    .is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1)
                {
                    0.5
                } else {
                    0.0
                }
            }
            // Move adjacent to the wounded ally = mild support (bodyguard).
            None => {
                if snap
                    .unit(*ally)
                    .is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1)
                {
                    0.5
                } else {
                    0.0
                }
            }
        },
        TacticalIntent::SetupAOE => {
            let Some((ability, target_pos, _)) = cast else {
                // Pure movement can't set up AoE; neutral.
                return 0.0;
            };
            let Some(def) = content.abilities.get(ability) else {
                return 0.0;
            };
            if def.aoe == AoEShape::None {
                return mild_penalty;
            }
            let area = aoe_area(def, target_pos, step.caster_tile());
            let total = snap.enemies_of(active.team).count() as f32;
            let hit = aoe_hits(&area, step_ctx.active, snap).enemies.len() as f32;
            if total > 0.0 {
                hit / total
            } else {
                0.0
            }
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
#[path = "score_tests.rs"]
mod tests;
