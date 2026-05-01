use bevy::prelude::Entity;
use crate::content::content_view::ContentView;
use crate::combat::ai::adapt::EvaluationMode;
use crate::combat::ai::scoring::factors::{aoe_area, aoe_hits, StepFactor};
use crate::combat::ai::outcome::ActionOutcomeEstimate;
use crate::combat::ai::scoring::position_eval::evaluate_position;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::world::snapshot::{AiTags, UnitSnapshot};
use crate::combat::ai::scoring::factors::ScoredStep;
use crate::combat::ai::orchestration::ScoringCtx;
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::Hex;
use super::kinds::TacticalIntent;

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
pub fn cc_reach(active: &UnitSnapshot, content: &ContentView) -> u32 {
    active
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| applies_cc(def, content))
        .map(|def| def.range.max)
        .max()
        .unwrap_or(active.max_attack_range)
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
    pub fn damage(mut self, w: f32) -> Self { self.damage = w; self }
    pub fn kill_now(mut self, w: f32) -> Self { self.kill_now = w; self }
    pub fn kill_promised(mut self, w: f32) -> Self { self.kill_promised = w; self }
    pub fn cc(mut self, w: f32) -> Self { self.cc = w; self }
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
    content: &ContentView,
) -> f32 {
    let snap = ctx.snap;
    let needs = ctx.need_signals;

    let scale = match step {
        ScoredStep::Move { .. } => return 0.0,
        ScoredStep::Cast { ability, target, target_pos, caster_tile } => {
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

    let damage     = StepFactor::Damage.compute(ctx, step, outcome, &needs);
    let kill_now   = StepFactor::KillNow.compute(ctx, step, outcome, &needs);
    let kill_prom  = StepFactor::KillPromised.compute(ctx, step, outcome, &needs);
    let cc         = StepFactor::Cc.compute(ctx, step, outcome, &needs);

    (weights.damage * damage
        + weights.kill_now * kill_now
        + weights.kill_promised * kill_prom
        + weights.cc * cc)
        * scale
}

// ── Intent → utility score (factor[7]) ──────────────────────────────────────

/// Compute how well a scored step aligns with the current intent.
/// Positive = aligned, zero = neutral, negative = misaligned (soft penalty).
///
/// Uses a dot-product of per-step impact factors against intent-specific weight
/// vectors (via `IntentWeights`) for `FocusTarget` and `ApplyCC`. This makes
/// alignment proportional to actual impact magnitude — a hit doing 10 damage
/// outscores a hit doing 1 damage, fixing S5 (low-value armor hits getting full
/// intent credit under the old hardcoded 1.0 return).
///
/// `ProtectSelf`, `ProtectAlly`, `SetupAOE`, `LastStand` preserve their
/// existing formulas (ported to the new signature).
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
        ScoredStep::Cast { ability, target_pos, target, .. } => {
            Some((*ability, *target_pos, *target))
        }
        ScoredStep::Move { .. } => None,
    };

    let Some((ability, _, target)) = cast else {
        // LastStand wants last useful action, not running.
        return -0.3;
    };
    let Some(def) = content.abilities.get(ability) else { return 0.0 };
    let mut score = 0.0f32;

    // "Direct offensive action" bonus in LastStand: covers both
    // entity-targeted (SingleEnemy) and cell-targeted (Ground)
    // attacks. AoE footprint gets an additional +0.3 below.
    if matches!(def.target_type, TargetType::SingleEnemy | TargetType::Ground) {
        score += 0.5;
    }
    if let Some(target_unit) = snap.unit(target) {
        if applies_cc(def, content) && !target_unit.tags.contains(AiTags::IS_STUNNED) {
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
    let active = step_ctx.active;
    let snap = step_ctx.snap;
    let maps = step_ctx.maps;
    let content = step_ctx.world.content;
    let difficulty = step_ctx.world.difficulty;
    let mild_penalty = step_ctx.world.tuning.thresholds.mild_penalty;

    // Move steps: scored only on position-related intent axes.
    let cast = match step {
        ScoredStep::Cast { ability, target_pos, target, .. } => {
            Some((*ability, *target_pos, *target))
        }
        ScoredStep::Move { .. } => None,
    };

    match intent {
        TacticalIntent::FocusTarget { target: focus } => {
            if cast.is_none() {
                // Pure move: pursuit geometry hook.
                return match snap.unit(*focus) {
                    Some(t) => {
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(active.max_attack_range);
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
                        let reach = (active.speed.max(0) as u32)
                            .saturating_add(cc_reach(active, content));
                        pursuit_move_score(active.pos, step.caster_tile(), t.pos, reach)
                    }
                    None => 0.0,
                };
            }
            // Cast: score offensive value via narrow API (CC-target filtered).
            let weights = IntentWeights::default()
                .cc(1.5)
                .damage(0.3);
            intent_offensive_value_on_target(*cc_target, step, step_ctx, outcome, &weights, content)
        }
        TacticalIntent::Reposition => {
            // Tiered: strong improvement rewarded, any improvement neutral,
            // no improvement penalized — mildly if casting, hard if just moving.
            let current = evaluate_position(active.pos, &active.role, step_ctx.world.tuning, maps);
            let new = evaluate_position(step.caster_tile(), &active.role, step_ctx.world.tuning, maps);
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
                if target == active.entity {
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
                let Some(def) = content.abilities.get(ability) else { return 0.0 };
                if def.target_type == TargetType::SingleAlly {
                    if target == *ally { 1.0 } else { mild_penalty }
                } else if snap.unit(*ally).is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1) {
                    0.5
                } else {
                    0.0
                }
            }
            // Move adjacent to the wounded ally = mild support (bodyguard).
            None => {
                if snap.unit(*ally).is_some_and(|a| step.caster_tile().unsigned_distance_to(a.pos) <= 1) {
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
            let Some(def) = content.abilities.get(ability) else { return 0.0 };
            if def.aoe == AoEShape::None {
                return mild_penalty;
            }
            let area = aoe_area(def, target_pos, step.caster_tile());
            let total = snap.enemies_of(active.team).count() as f32;
            let hit = aoe_hits(&area, active, snap).enemies.len() as f32;
            if total > 0.0 { hit / total } else { 0.0 }
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::combat::ai::adapt::EvaluationMode;
    use crate::combat::ai::outcome::ActionOutcomeEstimate;
    use crate::combat::ai::world::reservations::Reservations;
    use crate::combat::ai::world::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::combat::ai::test_helpers::{
        empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::combat::ai::config::difficulty::DifficultyProfile;
    use crate::core::AbilityId;
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};
    use crate::content::content_view::ContentView;

    /// Danger-only maps for intent-scoring tests; other three maps stay
    /// empty. Reposition scoring keys off `evaluate_position`, which reads
    /// danger with the Bruiser weight of -1.2 (so eval = -1.2 × danger).
    fn maps_with_dangers(tiles: &[(Hex, f32)]) -> crate::combat::ai::world::influence::InfluenceMaps {
        let mut m = empty_maps();
        for &(hex, val) in tiles {
            m.danger.add(hex, val);
        }
        m
    }

    fn dummy_unit(pos: Hex) -> UnitSnapshot {
        UnitBuilder::new(0, Team::Enemy, pos)
            .tags(AiTags::MELEE_ONLY)
            .build()
    }

    /// Caller owns the `AbilityId` so the `ScoredStep` ref stays valid for
    /// the scope of the test.
    fn dummy_step<'a>(tile: Hex, ability: &'a AbilityId) -> ScoredStep<'a> {
        ScoredStep::Cast {
            ability,
            target: Entity::from_raw_u32(1).expect("valid"),
            target_pos: tile,
            caster_tile: tile,
        }
    }

    #[test]
    fn reposition_penalizes_worse_tile() {
        // Current pos: eval = -1.2 * 1.5 = -1.8
        // Better tile:  eval = -1.2 * (7/6) ≈ -1.4  (improvement 0.4)
        // Worse tile:   eval = -1.2 * (19/12) ≈ -1.9 (improvement -0.1)
        let current = hex_from_offset(3, 3);
        let better = hex_from_offset(4, 3);
        let worse = hex_from_offset(2, 3);

        let maps = maps_with_dangers(&[
            (current, 1.5),
            (better, 7.0 / 6.0),
            (worse, 19.0 / 12.0),
        ]);

        let active = dummy_unit(current);
        let enemy = UnitSnapshot {
            entity: Entity::from_raw_u32(1).expect("valid"),
            team: Team::Player,
            ..dummy_unit(hex_from_offset(0, 0))
        };
        let snap = BattleSnapshot::new(vec![active.clone(), enemy], 1);
        let content = ContentView::load_global_for_tests();
        let intent = TacticalIntent::Reposition;
        let difficulty = DifficultyProfile::default();

        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ab = AbilityId::from("melee_attack");

        let ctx_worse = make_scoring_ctx(&world, &snap, &maps, &reservations, &active);
        let score_worse = intent_score(&intent, &dummy_step(worse, &ab), &ctx_worse, &ActionOutcomeEstimate::default(), EvaluationMode::Default);

        let ctx_better = make_scoring_ctx(&world, &snap, &maps, &reservations, &active);
        let score_better = intent_score(&intent, &dummy_step(better, &ab), &ctx_better, &ActionOutcomeEstimate::default(), EvaluationMode::Default);

        assert!(
            score_worse < 0.0,
            "worse tile should be penalized, got {score_worse}"
        );
        assert!(
            score_better > 0.0,
            "better tile should score positively, got {score_better}"
        );
    }

    // ── pursuit_move_score: pure helper ─────────────────────────────────

    /// Enter-reach gives the strong signal (0.8). Same bonus whether we
    /// land adjacent or at the reach boundary — caller's position/risk
    /// factors differentiate within the bubble.
    #[test]
    fn pursuit_entering_reach_scores_full_bonus() {
        let from = hex_from_offset(0, 0);
        let target = hex_from_offset(6, 0);
        // reach = 4: new tile at dist=4 from target qualifies.
        let landing = hex_from_offset(2, 0); // dist=4 from target
        let score = pursuit_move_score(from, landing, target, 4);
        assert!((score - 0.8).abs() < 1e-5, "enter-reach expected 0.8, got {score}");

        // Also enters when landing adjacent (dist=1 ≤ 4).
        let adj = hex_from_offset(5, 0); // dist=1
        let score_adj = pursuit_move_score(from, adj, target, 4);
        assert!((score_adj - 0.8).abs() < 1e-5);
    }

    /// Closing (outside reach) is mild positive, capped at 0.3 — can't
    /// spoof the 0.5 viability threshold on its own.
    #[test]
    fn pursuit_closing_is_mild_positive() {
        // from dist=10, to dist=7 — delta=3, reach=4, expected 0.3*3/4=0.225
        let from = hex_from_offset(10, 0);
        let to = hex_from_offset(7, 0);
        let target = hex_from_offset(0, 0);
        let score = pursuit_move_score(from, to, target, 4);
        assert!((score - 0.225).abs() < 1e-5, "closing: {score}");
        assert!(score < 0.5, "closing alone must stay below viability threshold");
        assert!(score > 0.0);
    }

    /// Retreat is softly negative and proportional — a single-tile back-
    /// step at reach=4 barely registers, so hex-grid detours around
    /// chokes or obstacles aren't punished.
    #[test]
    fn pursuit_retreat_is_soft_negative() {
        // from dist=5, to dist=6 — delta=-1, reach=4, expected -0.1*1/4=-0.025
        let from = hex_from_offset(5, 0);
        let to = hex_from_offset(6, 0);
        let target = hex_from_offset(0, 0);
        let score = pursuit_move_score(from, to, target, 4);
        assert!((score + 0.025).abs() < 1e-5, "retreat: {score}");
        assert!(score > -0.1, "retreat capped at -0.1, got {score}");
    }

    /// No change in hex distance (e.g. circling around an equidistant
    /// arc on hex-grid) scores 0 — neutral, not punished.
    #[test]
    fn pursuit_no_distance_change_is_zero() {
        // Target far (dist=10), reach=2: any equidistant neighbor stays
        // outside the bubble, so the test exercises the delta==0 branch
        // rather than accidentally tripping the enter-reach early return.
        let from = hex_from_offset(10, 0);
        let target = hex_from_offset(0, 0);
        let cur_d = from.unsigned_distance_to(target);
        let equidistant = from
            .all_neighbors()
            .into_iter()
            .find(|&n| n.unsigned_distance_to(target) == cur_d)
            .expect("even-r hex should admit an equidistant neighbor on a straight axis");
        let score = pursuit_move_score(from, equidistant, target, 2);
        assert_eq!(score, 0.0);
    }

    // ── cc_reach: content-aware reach computation ───────────────────────

    /// Actor has a ranged stun (range=3) and a melee weapon_attack
    /// (range=1). `cc_reach` must pick the stun's range — that's the
    /// intent-relevant engagement horizon.
    #[test]
    fn cc_reach_prefers_cc_ability_range() {
        use crate::content::abilities::{
            AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn,
            TargetType,
        };
        use crate::content::statuses::StatusDef;
        use crate::core::{DiceExpr, StatusId};

        let mut content = crate::combat::ai::test_helpers::empty_content();
        let stun_status_id = StatusId::from("stun");
        content.statuses.insert(
            stun_status_id.clone(),
            StatusDef {
                id: stun_status_id.clone(),
                name: "stun".into(),
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: true,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: false,
                buff_class: None,
            },
        );
        let stun_shot = AbilityDef {
            id: AbilityId::from("stun_shot"),
            name: "stun_shot".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication {
                status: stun_status_id,
                duration_rounds: 1,
                on: StatusOn::Target,
            }],
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        };
        let melee = AbilityDef {
            id: AbilityId::from("melee_attack"),
            name: "melee_attack".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        };
        content.abilities.insert(stun_shot.id.clone(), stun_shot.clone());
        content.abilities.insert(melee.id.clone(), melee.clone());

        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ability_names(&["stun_shot", "melee_attack"])
            .max_attack_range(3)
            .build();
        assert_eq!(cc_reach(&actor, &content), 3);

        // Actor without any CC ability falls back to max_attack_range.
        let brawler = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .ability_names(&["melee_attack"])
            .max_attack_range(1)
            .build();
        assert_eq!(cc_reach(&brawler, &content), 1);
    }

    // ── intent_score wiring: FocusTarget Move uses pursuit ──────────────

    /// Regression test for logs #1/#3/#7: a melee pursuer whose Move
    /// enters the (speed + range) bubble must score at/above the
    /// FocusTarget viability threshold (0.5). Before Fix B Move scored
    /// 0.0, so viability_fallback ran every turn even when the warrior
    /// was actively closing.
    #[test]
    fn focus_target_pursuit_enters_bubble_above_viability() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .speed(3)
            .max_attack_range(1)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(5, 0))
            .build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let intent = TacticalIntent::FocusTarget { target: target.entity };

        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // Move to (4,0) — dist=1 to target, reach=3+1=4, 1<=4 → 0.8.
        let move_into_reach = ScoredStep::Move { caster_tile: hex_from_offset(4, 0) };
        let score = intent_score(&intent, &move_into_reach, &ctx, &ActionOutcomeEstimate::default(), EvaluationMode::Default);
        assert!(
            score >= 0.5,
            "enter-reach Move must pass viability (0.5), got {score}",
        );
    }

    // ── intent_score: FocusTarget proportional scoring ──────────────────

    /// FocusTarget intent score must be proportional to actual damage dealt:
    /// hitting the focus target for 10 damage must outscore hitting it for 1.
    /// This pins the S5 fix — armor hits that do minimal damage no longer
    /// receive the same credit as impactful blows.
    #[test]
    fn focus_target_scores_proportional_to_damage() {
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
        use crate::core::DiceExpr;

        let target_pos = hex_from_offset(1, 0);
        let target = UnitBuilder::new(2, Team::Player, target_pos).hp(20).build();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), target.clone()], 1);
        let maps = empty_maps();
        let difficulty = DifficultyProfile::default();

        // Two abilities: one deals 10 damage, the other 1 damage.
        let mut content = crate::combat::ai::test_helpers::empty_content();
        let strong = AbilityDef {
            id: AbilityId::from("strong_hit"),
            name: "strong_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 10, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        };
        let weak = AbilityDef {
            id: AbilityId::from("weak_hit"),
            name: "weak_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 1, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        };
        content.abilities.insert(strong.id.clone(), strong.clone());
        content.abilities.insert(weak.id.clone(), weak.clone());

        let intent = TacticalIntent::FocusTarget { target: target.entity };
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let strong_id = AbilityId::from("strong_hit");
        let weak_id = AbilityId::from("weak_hit");
        let step_strong = ScoredStep::Cast {
            ability: &strong_id,
            target: target.entity,
            target_pos,
            caster_tile: actor.pos,
        };
        let step_weak = ScoredStep::Cast {
            ability: &weak_id,
            target: target.entity,
            target_pos,
            caster_tile: actor.pos,
        };

        // Build outcomes with raw fact fields — enemy_damage is the raw post-armor
        // damage (no policy weighting). compute_offensive reads this field.
        use crate::content::abilities::CasterContext;
        let caster_ctx = CasterContext::default();

        let raw_damage = |def: &AbilityDef| -> f32 {
            let Some(calc) = def.effect.calc(&caster_ctx) else { return 0.0; };
            if calc.is_heal { return 0.0; }
            let mitigation = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
            (calc.expected() - mitigation + target.damage_taken_bonus as f32).max(0.0)
        };

        let outcome_strong = ActionOutcomeEstimate {
            enemy_damage: raw_damage(&strong),
            ..Default::default()
        };
        let outcome_weak = ActionOutcomeEstimate {
            enemy_damage: raw_damage(&weak),
            ..Default::default()
        };

        let score_strong = intent_score(&intent, &step_strong, &ctx, &outcome_strong, EvaluationMode::Default);
        let score_weak = intent_score(&intent, &step_weak, &ctx, &outcome_weak, EvaluationMode::Default);

        assert!(
            score_strong > score_weak,
            "high-damage hit ({score_strong}) must outscore low-damage hit ({score_weak})",
        );
        assert!(score_strong > 0.0, "strong hit must score positively: {score_strong}");
        assert!(score_weak >= 0.0, "weak hit must not score negatively: {score_weak}");
    }

    /// Hitting a non-focus target with a single-target attack should yield
    /// near-zero intent score for FocusTarget intent (no offensive credit for
    /// the focus entity).
    #[test]
    fn focus_target_wrong_target_scores_near_zero() {
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
        use crate::core::DiceExpr;

        let focus_pos = hex_from_offset(1, 0);
        let other_pos = hex_from_offset(2, 0);
        let focus = UnitBuilder::new(2, Team::Player, focus_pos).hp(20).build();
        let other = UnitBuilder::new(3, Team::Player, other_pos).hp(20).build();
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone(), other.clone()], 1);
        let maps = empty_maps();
        let difficulty = DifficultyProfile::default();

        let mut content = crate::combat::ai::test_helpers::empty_content();
        let hit = AbilityDef {
            id: AbilityId::from("melee_hit"),
            name: "melee_hit".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(2, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        };
        content.abilities.insert(hit.id.clone(), hit);

        let intent = TacticalIntent::FocusTarget { target: focus.entity };
        let world = make_test_ctx(&content, &difficulty);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let ability_id = AbilityId::from("melee_hit");
        // Cast targeting `other` (not the focus entity).
        let step_wrong = ScoredStep::Cast {
            ability: &ability_id,
            target: other.entity,
            target_pos: other_pos,
            caster_tile: actor.pos,
        };

        let score = intent_score(&intent, &step_wrong, &ctx, &ActionOutcomeEstimate::default(), EvaluationMode::Default);
        assert!(
            score <= 0.0,
            "hitting non-focus target must yield ≤ 0 intent score, got {score}",
        );
    }

    // ── intent_offensive_value_on_target ────────────────────────────────────────

    mod intent_score_via_narrow_offensive_api_matches_legacy {
        use super::*;
        use crate::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType};
        use crate::core::DiceExpr;
        use crate::combat::ai::test_helpers::empty_content;

        fn make_hit_ability(id: &str) -> AbilityDef {
            AbilityDef {
                id: AbilityId::from(id),
                name: id.into(),
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
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

        fn make_aoe_ability(id: &str, radius: u32) -> AbilityDef {
            AbilityDef {
                id: AbilityId::from(id),
                name: id.into(),
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 5 },
                effect: EffectDef::Damage { dice: DiceExpr::new(2, 6, 0) },
                costs: Vec::new(),
                cost_ap: 2,
                aoe: AoEShape::Circle { radius },
                friendly_fire: false,
                statuses: Vec::new(),
                magic_domains: Vec::new(),
                magic_method: String::new(),
                key: None,
                ai_tags_override: None,
            }
        }

        /// Direct cast on the focus entity: score must be > 0 (damage weight 1.0).
        #[test]
        fn focus_direct() {
            let actor_pos = hex_from_offset(0, 0);
            let focus_pos = hex_from_offset(2, 0);

            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
            let focus = UnitBuilder::new(2, Team::Player, focus_pos).hp(50).max_hp(100).build();
            let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone()], 1);
            let maps = empty_maps();
            let difficulty = DifficultyProfile::default();
            let reservations = Reservations::default();

            let mut content = empty_content();
            let ab = make_hit_ability("hit");
            content.abilities.insert(ab.id.clone(), ab);

            let world = make_test_ctx(&content, &difficulty);
            let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            let ab_id = AbilityId::from("hit");
            let step = ScoredStep::Cast {
                ability: &ab_id,
                target: focus.entity,
                target_pos: focus_pos,
                caster_tile: actor_pos,
            };
            let outcome = ActionOutcomeEstimate {
                enemy_damage: 10.0,
                ..Default::default()
            };
            let weights = IntentWeights::default().kill_now(2.0).kill_promised(0.3).damage(1.0).cc(0.5);
            let score = intent_offensive_value_on_target(focus.entity, &step, &ctx, &outcome, &weights, &content);
            assert!(score > 0.0, "direct hit on focus must score > 0, got {score}");
        }

        /// Cast AoE whose area covers the focus tile: score must equal direct × 0.6.
        #[test]
        fn focus_aoe_covers() {
            let actor_pos = hex_from_offset(0, 0);
            // Target AoE at (1,0), focus at (2,0) — radius 2 covers focus.
            let aoe_target_pos = hex_from_offset(1, 0);
            let focus_pos = hex_from_offset(2, 0);

            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
            let focus = UnitBuilder::new(2, Team::Player, focus_pos).hp(50).max_hp(100).build();
            let direct_target = UnitBuilder::new(3, Team::Player, aoe_target_pos).hp(50).max_hp(100).build();
            let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone(), direct_target.clone()], 1);
            let maps = empty_maps();
            let difficulty = DifficultyProfile::default();
            let reservations = Reservations::default();

            let mut content = empty_content();
            let ab = make_aoe_ability("aoe_hit", 2);
            content.abilities.insert(ab.id.clone(), ab);

            let world = make_test_ctx(&content, &difficulty);
            let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            let ab_id = AbilityId::from("aoe_hit");
            // Step targets direct_target (not focus), but radius 2 covers focus_pos.
            let step = ScoredStep::Cast {
                ability: &ab_id,
                target: direct_target.entity,
                target_pos: aoe_target_pos,
                caster_tile: actor_pos,
            };
            let outcome = ActionOutcomeEstimate {
                enemy_damage: 10.0,
                enemy_damage_per_entity: vec![(direct_target.entity, 10.0), (focus.entity, 8.0)],
                ..Default::default()
            };
            let weights = IntentWeights::default().kill_now(2.0).kill_promised(0.3).damage(1.0).cc(0.5);

            // Score with AoE covering focus: should be 0.6 × direct-equivalent.
            let score_aoe = intent_offensive_value_on_target(focus.entity, &step, &ctx, &outcome, &weights, &content);
            // Direct equivalent: same step but targeting focus directly.
            let step_direct = ScoredStep::Cast {
                ability: &ab_id,
                target: focus.entity,
                target_pos: focus_pos,
                caster_tile: actor_pos,
            };
            let score_direct = intent_offensive_value_on_target(focus.entity, &step_direct, &ctx, &outcome, &weights, &content);

            assert!(score_aoe > 0.0, "AoE covering focus must score > 0, got {score_aoe}");
            let expected = score_direct * 0.6;
            assert!(
                (score_aoe - expected).abs() < 1e-4,
                "AoE score {score_aoe} must equal direct*0.6={expected}",
            );
        }

        /// Cast AoE whose area does NOT cover focus tile: score must be 0.
        #[test]
        fn focus_aoe_misses() {
            let actor_pos = hex_from_offset(0, 0);
            // Target AoE far away from focus.
            let aoe_target_pos = hex_from_offset(8, 0);
            let focus_pos = hex_from_offset(2, 0);

            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
            let focus = UnitBuilder::new(2, Team::Player, focus_pos).hp(50).max_hp(100).build();
            let other = UnitBuilder::new(3, Team::Player, aoe_target_pos).hp(50).max_hp(100).build();
            let snap = BattleSnapshot::new(vec![actor.clone(), focus.clone(), other.clone()], 1);
            let maps = empty_maps();
            let difficulty = DifficultyProfile::default();
            let reservations = Reservations::default();

            let mut content = empty_content();
            let ab = make_aoe_ability("aoe_miss", 1);
            content.abilities.insert(ab.id.clone(), ab);

            let world = make_test_ctx(&content, &difficulty);
            let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            let ab_id = AbilityId::from("aoe_miss");
            let step = ScoredStep::Cast {
                ability: &ab_id,
                target: other.entity,
                target_pos: aoe_target_pos,
                caster_tile: actor_pos,
            };
            let outcome = ActionOutcomeEstimate {
                enemy_damage: 10.0,
                ..Default::default()
            };
            let weights = IntentWeights::default().kill_now(2.0).kill_promised(0.3).damage(1.0).cc(0.5);
            let score = intent_offensive_value_on_target(focus.entity, &step, &ctx, &outcome, &weights, &content);
            assert_eq!(score, 0.0, "AoE not covering focus must score 0, got {score}");
        }

        /// ApplyCC: direct cast on cc_target with cc weight 1.5. Score > 0 when cc applied.
        #[test]
        fn apply_cc_direct() {
            let actor_pos = hex_from_offset(0, 0);
            let target_pos = hex_from_offset(2, 0);

            let actor = UnitBuilder::new(1, Team::Enemy, actor_pos).build();
            let cc_target = UnitBuilder::new(2, Team::Player, target_pos).hp(50).max_hp(100).build();
            let snap = BattleSnapshot::new(vec![actor.clone(), cc_target.clone()], 1);
            let maps = empty_maps();
            let difficulty = DifficultyProfile::default();
            let reservations = Reservations::default();

            let mut content = empty_content();
            let ab = make_hit_ability("stun_hit");
            content.abilities.insert(ab.id.clone(), ab);

            let world = make_test_ctx(&content, &difficulty);
            let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);
            let ab_id = AbilityId::from("stun_hit");
            let step = ScoredStep::Cast {
                ability: &ab_id,
                target: cc_target.entity,
                target_pos,
                caster_tile: actor_pos,
            };
            let outcome = ActionOutcomeEstimate {
                enemy_damage: 5.0,
                cc_turns_applied: 2.0,
                ..Default::default()
            };
            // ApplyCC weights: cc=1.5, damage=0.3
            let weights = IntentWeights::default().cc(1.5).damage(0.3);
            let score = intent_offensive_value_on_target(cc_target.entity, &step, &ctx, &outcome, &weights, &content);
            assert!(score > 0.0, "direct hit on cc_target must score > 0, got {score}");
        }
    }
}
