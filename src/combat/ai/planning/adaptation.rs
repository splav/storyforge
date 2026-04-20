//! Value-function **adaptation** layer.
//!
//! Pipeline position: between `sanity_adjust_plans` (plan-level cost
//! correction, soft multipliers) and contract masks (intent↔plan coherence
//! enforcement). Adaptation answers the question:
//!
//! > "Facts discovered after measurement make the current value function
//! >  inadequate for some plans. Which plans, and what's the right
//! >  evaluation regime for them instead?"
//!
//! Example: `expected_aoo_damage >= actor_hp` for a plan means the actor
//! does not continue to exist after this turn — `continue-to-exist value =
//! 0` — so scoring the plan under `FocusTarget`/`ApplyCC`/... is semantically
//! wrong. The only evaluation regime that stays meaningful is **LastStand**:
//! "what useful thing do I achieve before going down".
//!
//! # Invariants
//!
//! The layer is intentionally narrow. These are load-bearing:
//!
//! 1. **ONE PASS.** `apply_adaptation` runs once per `pick_action`, after
//!    sanity, before contract masks. No internal loops, no re-entry.
//! 2. **FACTS ONLY.** Triggers are snapshot facts
//!    (`expected_aoo_damage >= hp`, `plan_is_defensive`, `global_intent`).
//!    Never post-score comparisons — that would create circular meaning.
//! 3. **NO PENALTIES / NO MASKS.** The layer only maps
//!    `(plan → EvaluationMode)` and triggers intent-column rescore for the
//!    affected rows. It does not multiply scores and does not write `-∞`.
//!    That territory belongs to sanity (multipliers) and contract (masks).
//! 4. **IDEMPOTENT.** Applying adaptation a second time is a no-op.
//!    `EvaluationMode` changes at most once per plan.
//! 5. **CONTRACT-NEUTRAL.** Adaptation does not know about contract masks.
//!    Contract runs AFTER adaptation and masks only plans with
//!    `mode = Default` — plans with `mode != Default` have already opted
//!    out of the original intent's contract by virtue of the regime switch.
//!
//! Adding a new `AdaptationReason`: only if the new case satisfies all five
//! invariants. A "I want to penalise X a bit more" rule belongs in sanity,
//! not here.

use crate::combat::ai::factors::PlanFactors;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::sanity::expected_aoo_damage;
use crate::combat::ai::planning::scorer::rescore_with_per_plan_modes;
use crate::combat::ai::planning::{plan_is_defensive, TurnPlan};
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::combat::ai::utility::ScoringCtx;

/// Evaluation regime used when scoring the intent-column of a plan.
///
/// `Default` = score under the global `TacticalIntent` selected by
/// `select_intent`. `LastStand` = score as if the actor is committed to a
/// "final useful action" — the `TacticalIntent::LastStand` scoring table in
/// `intent_score()` is reused so no new scoring code is needed; this enum
/// only selects *which* existing table to apply, per plan.
///
/// Populated by `apply_adaptation`; consumed by the scorer's per-plan
/// intent rescore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    /// Score under the global tactical intent.
    #[default]
    Default,
    /// Score under the LastStand regime — "final useful action" weighting.
    /// Used when the plan either kills the actor (per-plan) or the global
    /// intent cannot be satisfied (global ProtectSelf → no defensive).
    LastStand,
}

impl EvaluationMode {
    /// Returns the effective intent to use for scoring this plan's
    /// intent-column. `Default` defers to the caller's global intent;
    /// `LastStand` always overrides to `TacticalIntent::LastStand` regardless
    /// of what the caller passes.
    ///
    /// Consolidates the "which intent drives scoring?" decision in one
    /// place so callers don't have to know the mapping.
    pub fn effective_intent(self, global: TacticalIntent) -> TacticalIntent {
        match self {
            EvaluationMode::Default => global,
            EvaluationMode::LastStand => TacticalIntent::LastStand,
        }
    }
}

/// Fact-based reason an individual plan's evaluation regime was switched.
/// Carries enough numeric context for debug/log to explain the switch —
/// no post-score values, only snapshot facts (see invariant #2).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdaptationReason {
    /// Plan's expected AoO damage on its move transitions reaches or
    /// exceeds the actor's current HP → continue-to-exist value = 0 →
    /// evaluate under LastStand. Per-plan override.
    ///
    /// "Expected" because `expected_aoo_damage` is an EV aggregate
    /// (crit-fail is disabled in sim); in a live turn the plan may or
    /// may not kill the actor. The adaptation threshold is conservative:
    /// if EV says ≥ HP, treat it as self-terminating.
    ExpectedSelfLethal { aoo_dmg: f32, actor_hp: i32 },
    /// Global intent is `ProtectSelf` but **no** plan in the pool is
    /// defensive (by `plan_is_defensive`). The ProtectSelf contract
    /// cannot be satisfied, so every plan is evaluated under LastStand.
    /// Global override (applied to all plans).
    ProtectSelfNoDefensive,
}

impl AdaptationReason {
    /// Stable snake_case code for analyzers / JSONL `adaptation_reason`
    /// field. Keep in sync with schema_version in `log.rs` when renaming.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
        }
    }
}

/// Output of the adaptation pass. Parallel vectors aligned with the plan
/// pool: `modes[i]` is the evaluation regime for `plans[i]`, and
/// `reasons[i]` is `Some(_)` iff `modes[i] != Default`.
///
/// Consumed by (a) `pick_action` when wrapping the committed plan's
/// `IntentReason` as `Adapted { prior, reason }`, and (b) the contract
/// mask (`apply_protect_self_mask`) to skip plans that opted out of the
/// current intent's contract via a mode switch.
pub struct Adaptation {
    pub modes: Vec<EvaluationMode>,
    pub reasons: Vec<Option<AdaptationReason>>,
}

impl Adaptation {
    /// Empty adaptation for a pool of size `n` — every plan at Default,
    /// no reasons recorded. Used as the initial state before
    /// `apply_adaptation` runs, and as a safe fallback in tests.
    pub fn empty(n: usize) -> Self {
        Self {
            modes: vec![EvaluationMode::Default; n],
            reasons: vec![None; n],
        }
    }

    /// Did any plan end up in a non-Default mode?
    pub fn any_adapted(&self) -> bool {
        self.modes.iter().any(|m| !matches!(m, EvaluationMode::Default))
    }
}

/// Run the ADAPTATION pass over the plan pool. Returns an `Adaptation`
/// reflecting per-plan mode decisions; mutates `raw` (intent-column for
/// switched plans) and `scored` (full batch rescored so normalisation
/// stays consistent).
///
/// See the module docstring for the five invariants this pass upholds.
/// The code structure below maps 1:1 to them:
/// - single function body, no recursion → **ONE PASS**
/// - triggers read `active.hp`, `expected_aoo_damage`, `plan_is_defensive`,
///   `intent` — all snapshot/input facts → **FACTS ONLY**
/// - no score multiplication, no masking; only mode map + rescore →
///   **NO PENALTIES / NO MASKS**
/// - rescore overwrites `raw[i].intent`; calling again produces the same
///   value → **IDEMPOTENT**
/// - does not consult contract masks and does not prevent them from
///   running afterwards → **CONTRACT-NEUTRAL**
pub fn apply_adaptation(
    plans: &[TurnPlan],
    raw: &mut [PlanFactors],
    scored: &mut Vec<f32>,
    intent: &TacticalIntent,
    ctx: &ScoringCtx,
) -> Adaptation {
    debug_assert_eq!(plans.len(), raw.len());
    debug_assert_eq!(plans.len(), scored.len());

    let mut adaptation = Adaptation::empty(plans.len());
    if plans.is_empty() {
        return adaptation;
    }

    let active = ctx.active;
    let content = ctx.world.content;
    let maps = ctx.maps;
    let margin = ctx.world.difficulty.defensive_tile_margin();

    // ── Global rule: ProtectSelf has no defensive option ──────────────────
    // Applied first because it's global — any per-plan ExpectedSelfLethal
    // rule would be shadowed by the global switch anyway.
    if matches!(intent, TacticalIntent::ProtectSelf) {
        let any_defensive = plans
            .iter()
            .any(|p| plan_is_defensive(p, active, content, maps, margin));
        if !any_defensive {
            for i in 0..plans.len() {
                adaptation.modes[i] = EvaluationMode::LastStand;
                adaptation.reasons[i] = Some(AdaptationReason::ProtectSelfNoDefensive);
            }
            *scored = rescore_with_per_plan_modes(plans, raw, &adaptation.modes, intent, ctx);
            return adaptation;
        }
        // ProtectSelf with defensive options present: contract still
        // holds. ExpectedSelfLethal per-plan adaptation is gated off —
        // the actor is committed to self-preservation, self-lethal plans
        // are contract violations and should be masked, not rescored.
        return adaptation;
    }

    // ── Per-plan rule: ExpectedSelfLethal ─────────────────────────────────
    // Only under non-ProtectSelf intents. Under FocusTarget/ApplyCC/...,
    // a plan whose EV-AoO cost exceeds the actor's HP represents a trade
    // that the actor's current value function cannot express; LastStand's
    // "final useful action" table evaluates it on its own terms (kill >
    // cc > damage), so the plan competes honestly against defensive
    // alternatives.
    let enemies: Vec<&UnitSnapshot> = ctx.snap.enemies_of(active.team).collect();
    let hp_cutoff = active.hp as f32;
    let mut any_switched = false;
    for (i, plan) in plans.iter().enumerate() {
        if active.hp <= 0 {
            break; // Dead actor has no plans to adapt — guard against weird snapshots.
        }
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        if aoo_dmg >= hp_cutoff {
            adaptation.modes[i] = EvaluationMode::LastStand;
            adaptation.reasons[i] = Some(AdaptationReason::ExpectedSelfLethal {
                aoo_dmg,
                actor_hp: active.hp,
            });
            any_switched = true;
        }
    }

    if any_switched {
        *scored = rescore_with_per_plan_modes(plans, raw, &adaptation.modes, intent, ctx);
    }

    adaptation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::planning::{sanity_adjust_plans, PlanStep};
    use crate::combat::ai::reservations::Reservations;
    use crate::combat::ai::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{
        empty_content, empty_maps, make_scoring_ctx, make_test_ctx, UnitBuilder,
    };
    use crate::game::components::Team;
    use crate::game::hex::{hex_from_offset, Hex};

    // ── effective_intent ─────────────────────────────────────────────────

    #[test]
    fn default_mode_defers_to_global_intent() {
        let global = TacticalIntent::Reposition;
        let got = EvaluationMode::Default.effective_intent(global);
        assert!(matches!(got, TacticalIntent::Reposition));
    }

    #[test]
    fn last_stand_mode_overrides_global() {
        // Even if the caller passes something unrelated, LastStand pins the
        // scoring regime — this is the whole point of the per-plan override.
        let global = TacticalIntent::Reposition;
        let got = EvaluationMode::LastStand.effective_intent(global);
        assert!(matches!(got, TacticalIntent::LastStand));
    }

    // ── apply_adaptation ─────────────────────────────────────────────────
    //
    // Scaffolding: each test builds a minimal actor + adjacent melee
    // enemy + plan(s), runs `apply_adaptation`, inspects `modes` /
    // `reasons` / side effects on `scored`. `expected_aoo_damage`
    // lights up when a Move step leaves adjacency (see sanity.rs tests
    // for the full matrix) — we use the simplest topology: actor at
    // origin, enemy at (1,0), Move to (-1,0) triggers an AoO.

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        }
    }

    #[test]
    fn detect_expected_self_lethal_sets_last_stand_mode() {
        // 3-HP actor leaves adjacency with a 5-expected-dmg melee enemy:
        // AoO EV is well above HP → plan gets mode=LastStand under
        // non-ProtectSelf intent.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .aoo(0.0, 0) // self has no AoO; irrelevant here
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let plans = vec![move_plan(vec![hex_from_offset(-1, 0)])];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactors::default()];
        let mut scored = vec![0.5];
        let intent = TacticalIntent::Reposition;
        let adaptation = apply_adaptation(&plans, &mut raw, &mut scored, &intent, &ctx);

        assert!(matches!(adaptation.modes[0], EvaluationMode::LastStand));
        assert!(matches!(
            adaptation.reasons[0],
            Some(AdaptationReason::ExpectedSelfLethal { .. })
        ));
    }

    #[test]
    fn expected_self_lethal_gated_off_under_protect_self() {
        // Intent=ProtectSelf with at least one defensive option in the pool
        // (empty plan = defensive by convention, see sanity::plan_is_defensive).
        // Contract takes priority over trade: the self-lethal Move plan is a
        // contract violation, not a trade — adaptation leaves its mode at
        // Default so the contract mask masks it downstream.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let empty_defensive = TurnPlan {
            steps: Vec::new(),
            final_pos: hex_from_offset(0, 0),
            residual_ap: 1,
            residual_mp: 3,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
        };
        let lethal_move = move_plan(vec![hex_from_offset(-1, 0)]);
        let plans = vec![empty_defensive, lethal_move];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactors::default(), PlanFactors::default()];
        let mut scored = vec![0.5, 0.5];
        let adaptation = apply_adaptation(
            &plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(
            matches!(adaptation.modes[0], EvaluationMode::Default)
                && matches!(adaptation.modes[1], EvaluationMode::Default),
            "ExpectedSelfLethal must not fire under ProtectSelf when defensive options exist",
        );
        assert!(adaptation.reasons[0].is_none() && adaptation.reasons[1].is_none());
    }

    #[test]
    fn protect_self_no_defensive_switches_all_plans_globally() {
        // ProtectSelf intent, but every plan is non-defensive — here a
        // Move into a strictly more dangerous tile (danger map peaks at
        // the destination). `plan_is_defensive` returns false because
        // the move destination is not safer than the actor's current
        // tile. All plans flip to LastStand mode with the global reason.
        let pos = hex_from_offset(0, 0);
        let danger_tile = hex_from_offset(3, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, pos).hp(5).build();
        let plans = vec![move_plan(vec![danger_tile])];
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let mut maps = empty_maps();
        maps.danger.add(danger_tile, 2.0);
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactors::default()];
        let mut scored = vec![0.5];
        let adaptation = apply_adaptation(
            &plans, &mut raw, &mut scored, &TacticalIntent::ProtectSelf, &ctx,
        );

        assert!(matches!(adaptation.modes[0], EvaluationMode::LastStand));
        assert!(matches!(
            adaptation.reasons[0],
            Some(AdaptationReason::ProtectSelfNoDefensive)
        ));
    }

    #[test]
    fn adaptation_is_idempotent_on_second_call() {
        // Running apply_adaptation twice in sequence must not churn state.
        // Second call re-detects the same facts, re-writes the same
        // mode/reason, triggers the same rescore that produces the same
        // `raw[i].intent` value. Final `scored` equal across the two.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let plans = vec![move_plan(vec![hex_from_offset(-1, 0)])];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactors::default()];
        let mut scored = vec![0.5];
        let intent = TacticalIntent::Reposition;

        let _ = apply_adaptation(&plans, &mut raw, &mut scored, &intent, &ctx);
        let after_first = scored.clone();
        let raw_after_first = raw.clone();

        let _ = apply_adaptation(&plans, &mut raw, &mut scored, &intent, &ctx);

        assert_eq!(after_first, scored, "scored stable across a second call");
        assert_eq!(
            raw_after_first[0].intent, raw[0].intent,
            "intent-column stable across a second call",
        );
    }

    #[test]
    fn default_plans_untouched_when_no_trigger_fires() {
        // Actor at full HP, no adjacent enemy, intent=Reposition, plan
        // is a harmless move. No AdaptationReason applies — scored/raw
        // unchanged, mode stays Default.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20)
            .build();
        let plans = vec![move_plan(vec![hex_from_offset(1, 1)])];
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut raw = vec![PlanFactors::default()];
        let scored_before = vec![0.5];
        let mut scored = scored_before.clone();
        let adaptation = apply_adaptation(
            &plans, &mut raw, &mut scored, &TacticalIntent::Reposition, &ctx,
        );

        assert!(matches!(adaptation.modes[0], EvaluationMode::Default));
        assert!(adaptation.reasons[0].is_none());
        assert!(!adaptation.any_adapted());
        // Empty adaptation path skips rescore; scored must equal input.
        assert_eq!(scored, scored_before);
    }

    #[test]
    fn sanity_no_longer_masks_expected_lethal_aoo() {
        // Regression: before MVP1, `sanity_adjust_plans` wrote -∞ when
        // `aoo_dmg >= hp`. The adaptation layer now owns that case;
        // sanity must stay in its "soft multipliers only" lane.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let plans = vec![
            // First plan: a no-op so sanity has a 2nd entry to compare
            // against (sanity_adjust_plans short-circuits on len<=1).
            TurnPlan {
                steps: Vec::new(),
                final_pos: hex_from_offset(0, 0),
                residual_ap: 0,
                residual_mp: 0,
                outcomes: Vec::new(),
                partial_score: 0.0,
                sim_snapshots: Vec::new(),
            },
            move_plan(vec![hex_from_offset(-1, 0)]),
        ];
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let content = empty_content();
        let difficulty = DifficultyProfile::default();
        let world = make_test_ctx(&content, &difficulty);
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        let mut scored = vec![1.0, 1.0];
        sanity_adjust_plans(&mut scored, &plans, &ctx);

        assert!(
            scored[1].is_finite(),
            "sanity must not produce -inf for expected-lethal AoO (got {})",
            scored[1],
        );
        // The non-lethal bleed penalty still floors at 0.25 × input.
        assert!(scored[1] >= 0.25, "soft AoO bleed floor holds");
    }
}
