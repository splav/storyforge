//! ActionOutcomeEstimate — structured outcome vector shared across factors,
//! intent, critics, and terminal eval. Populated in SimState::apply_step
//! call chain; consumers migrate onto it incrementally (steps 4.1–4.5).
//!
//! Step 4.0 ships the type + PlanAnnotation container zero-filled — no
//! consumers yet. See docs/ai_rework_step4_plan.md.

use crate::combat::ai::factors::PlanFactors;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::types::PlanStep;
use crate::combat::ai::scoring::{status_applications, stun_denial_value};
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, TargetType};
use crate::content::content_view::ContentView;
use crate::content::races::CritFailEffect;
use serde::{Deserialize, Serialize};

/// Structured estimate of a single plan step's consequences.
/// Fields populated incrementally across steps 4.1–4.2.
///
/// Semantic note (docs/ai_rework.md §4):
/// - `expected_damage`: raw expected damage from this step (step 4.1, populated).
/// - `p_kill_now`: 1.0 if step kills a target in this turn, else 0.0.
/// - `p_kill_soon`: probability of killing a target within the damage horizon.
/// - `deny_value`: aggregated CC / armor-debuff / vuln "denial" value.
/// - `rescue_value`: heal value with urgency baked-in during wave 1;
///   step 3 (need layer) will split urgency into NeedSignals.rescue_ally.
/// - `board_pressure`: 0.0 placeholder, filled in step 5 (terminal eval).
/// - `exposure_delta`: Δdanger from step (worst_path_danger for Move, 0 for Cast).
///   Populated in the generator; currently no consumer reads it — terminal eval
///   uses `ctx.maps.danger.get(plan.final_pos)` directly. Kept as structured
///   telemetry; will feed step-level critics in step 10.
/// - `geometry_gain`: 0.0 placeholder, filled in step 17 (geometry awareness).
/// - `resource_swing`: signed resource cost (negative = spent).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ActionOutcomeEstimate {
    pub expected_damage: f32,
    pub p_kill_now: f32,
    pub p_kill_soon: f32,
    pub deny_value: f32,
    pub rescue_value: f32,
    pub board_pressure: f32,
    pub exposure_delta: f32,
    pub geometry_gain: f32,
    pub resource_swing: f32,
}

/// Result of the viability-gate pass for one plan (step 7.1).
///
/// `passed = true` means the intent signal for this plan met the threshold and
/// no swap was triggered (or no threshold applies). `adjusted_score` is the
/// final score after any intent-column rewrite that viability triggered; it
/// equals the pre-viability score when no swap occurred.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ViabilityResult {
    /// Whether the viability gate passed without triggering a fallback swap.
    pub passed: bool,
    /// Score after viability rewrite (equals pre-viability score when passed).
    pub adjusted_score: f32,
}

impl Default for ViabilityResult {
    fn default() -> Self {
        Self { passed: true, adjusted_score: 0.0 }
    }
}

/// Per-plan annotation bundle. Grows as pipeline stages accrue data
/// (outcome in wave 1; critics / band / agenda in later waves).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlanAnnotation {
    /// One ActionOutcomeEstimate per plan step, same length as TurnPlan.steps
    /// and TurnPlan.outcomes.
    #[serde(default)]
    pub outcomes: Vec<ActionOutcomeEstimate>,
    /// One-shot terminal-state evaluation. Populated by `terminal_state_score`
    /// in `finalize_scores`; consumed by aggregation in 5.4.
    /// Serialized into JSONL as of schema v23 (step 5.6). Old v22 logs
    /// deserialize via `#[serde(default)]` → zero-filled `TerminalScore`,
    /// preserving backward compatibility.
    #[serde(default)]
    pub terminal: crate::combat::ai::planning::terminal::TerminalScore,
    /// Step 6.2: repair affinity of this plan against the stored goal context.
    /// Populated in `pick_action` when `AiMemory.last_goal` is present.
    /// Default zero-filled when no stored goal exists. Consumed by the
    /// repair bonus aggregation in 6.3 (not read into score in 6.2).
    #[serde(default)]
    pub repair_affinity: crate::combat::ai::repair::RepairAffinity,
    /// Step 7.1: viability gate result for this plan.
    /// Default (passed=true, adjusted_score=0.0) when ViabilityStage has not
    /// run yet or the gate did not apply to this intent.
    #[serde(default)]
    pub viability: ViabilityResult,
    /// Step 7.1: sanity hits applied to this plan (rule + multiplier pairs).
    /// Empty until SanityStage runs or when no rules fired.
    #[serde(default)]
    pub sanity: Vec<crate::combat::ai::planning::sanity::SanityHit>,
    /// Step 7.2: adaptation decision for this plan (was PlanRanking.adaptation.reasons[i]).
    /// `None` when no adaptation trigger fired for this plan.
    #[serde(default)]
    pub adaptation: Option<AdaptationData>,
    /// Step 7.2: contract mask applied to this plan (ProtectSelf or KillableGate masking).
    /// `None` when no mask applied.
    #[serde(default)]
    pub contract: Option<ContractMaskHit>,
    /// Step 7.4: final aggregated score for this plan after all pipeline stages.
    /// Default 0.0. Written by scoring stages (replaces ScoredPool.scored).
    #[serde(default)]
    pub score: f32,
    /// Step 7.4: raw factor decomposition for this plan.
    /// Written by the initial scoring pass. Default PlanFactors::default().
    #[serde(default)]
    pub raw_factors: PlanFactors,
    /// Step 7.4: whether this plan was chosen as the winning plan.
    /// Set to `true` by `PickBestStage`. Default false.
    #[serde(default)]
    pub chosen: bool,
    /// Step 7.4: pick mechanics info for the chosen plan.
    /// `None` for non-chosen plans. Set by `PickBestStage`.
    #[serde(default)]
    pub pick: Option<PickInfo>,
}

/// Adaptation reason + original (pre-adaptation) score for a single plan.
/// Written by `AdaptationStage`; consumed by the finalizer to build
/// `IntentReason::Adapted` for the winning plan.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdaptationData {
    pub reason: crate::combat::ai::planning::AdaptationReason,
    /// Score this plan had immediately before adaptation rescored it.
    pub original_score: f32,
}

/// Record of a contract mask hit (ProtectSelf or KillableGate).
/// Written by `ProtectSelfMaskStage` / `KillableGateStage`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContractMaskHit {
    /// Which mask applied: `"protect_self"` or `"killable_gate"`.
    pub mask: String,
    /// Score this plan had immediately before the mask set it to -∞.
    pub original_score: f32,
}

/// Pick diagnostics for the winning plan.
/// Written by `PickBestStage`; `None` on all non-chosen plans.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PickInfo {
    /// Top-K window, mercy flag, chosen position in the ranked pool.
    pub mechanics: crate::combat::ai::planning::PickMechanics,
}

// ---------------------------------------------------------------------------
// Extraction helpers (step 4.2)
// ---------------------------------------------------------------------------

/// `p_kill_soon` component of `ActionOutcomeEstimate`.
///
/// Returns `1.0` if `def`'s direct damage won't kill `target` now but the
/// accumulated DoT (pending on target + newly applied by this ability) will.
/// Returns `0.0` otherwise (including when direct damage already kills — that
/// case is covered by `p_kill_now` via sim's `StepOutcome.killed`).
pub fn estimate_kill_soon(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
) -> f32 {
    let Some(calc) = def.effect.calc(caster) else { return 0.0 };
    let armor = if calc.pierces_armor {
        0.0
    } else {
        (target.armor + target.armor_bonus) as f32
    };
    let net = calc.expected().round() - armor + target.damage_taken_bonus as f32;
    // kill_now case — no kill_soon when net already kills
    if net >= target.hp as f32 {
        return 0.0;
    }
    let pending_dot = already_pending_dot(target);
    let new_dot = dot_tick_sum_for_ability(def, target, content);
    if net + pending_dot + new_dot >= target.hp as f32 { 1.0 } else { 0.0 }
}

/// Denial value from CC statuses applied by `def` against `target`.
///
/// Extracted from `factors::offensive::status_cc_value` — formula is 1:1.
/// Includes stun denial (via `stun_denial_value`) plus vulnerability and
/// armor-shred contributions.
pub fn estimate_deny_value(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    let stun = stun_denial_value(def, target, content);
    let other: f32 = status_applications(def, content)
        .map(|(sd, d)| {
            let mut val = 0.0f32;
            if sd.damage_taken_bonus > 0 {
                val += sd.damage_taken_bonus as f32 * d;
            }
            if sd.armor_bonus > 0 {
                val += sd.armor_bonus as f32 * d;
            }
            val
        })
        .sum();
    stun + other
}

/// Heal value for a `SingleAlly` ability with urgency baked in.
///
/// Uses `compute_score_core` (the inlined `score_action` formula) and wraps it
/// with `crit_fail_adjusted` — exactly as `factors::offensive::compute_offensive`
/// does for the `heal` branch. Returns `0.0` for non-heal or non-SingleAlly
/// abilities.
pub fn estimate_rescue_value(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
    crit_fail_effect: &CritFailEffect,
    crit_fail_chance: f32,
) -> f32 {
    if def.target_type != TargetType::SingleAlly {
        return 0.0;
    }
    let raw = compute_score_core(def, target, caster, content, danger_at_target);
    crit_fail_adjusted_rescue(raw, def, crit_fail_effect, crit_fail_chance)
}

/// Scorer-compatible damage estimate for a single-target enemy cast.
///
/// Mirrors the damage path of `factors::offensive::compute_offensive`:
/// `compute_score_core + crit_fail_adjusted`. This is the value stored in
/// `ActionOutcomeEstimate::expected_damage` for single-target casts so that
/// the scorer can read it directly without re-running the score formula.
///
/// Returns `0.0` for non-SingleEnemy abilities (heal / AoE / status-only).
/// For AoE, the generator calls `compute_aoe_damage` directly and stores the
/// result, so this helper is not used there.
pub fn estimate_expected_damage(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
    crit_fail_effect: &CritFailEffect,
    crit_fail_chance: f32,
) -> f32 {
    if def.target_type != TargetType::SingleEnemy {
        return 0.0;
    }
    let raw = compute_score_core(def, target, caster, content, 0.0);
    crit_fail_adjusted_rescue(raw, def, crit_fail_effect, crit_fail_chance)
}

/// Hypothetical (without sim) outcome estimate for a single (ability, target) pair.
///
/// Used by `future_value::λ_attack` and `picker::record_committed_reservations`
/// where no sim step has been executed — we need an HP-equivalent value from
/// first principles.
///
/// `expected_damage` is set to the full `compute_score_core` result (damage +
/// status contribution), which makes `λ_attack = 0.5 * expected_damage` identical
/// to the legacy `0.5 * score_action(...)` in HP-equivalent units.
///
/// `danger_at_target` is passed straight to the heal-urgency formula;
/// callers that don't have a danger map pass `0.0` (as before).
pub fn estimate_hypothetical(
    def: &AbilityDef,
    target: &UnitSnapshot,
    caster: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
) -> ActionOutcomeEstimate {
    // Full HP-equivalent score — mirrors what score_action returned without
    // the crit_fail adjustment (future_value never had crit_fail).
    let score = compute_score_core(def, target, caster, content, danger_at_target);

    // Kill detection: if net damage (same formula as scoring) >= hp, kill_now.
    let p_kill_now = {
        let killed = if let Some(calc) = def.effect.calc(caster) {
            let armor = if calc.pierces_armor { 0.0 } else { (target.armor + target.armor_bonus) as f32 };
            let net = (calc.expected() - armor + target.damage_taken_bonus as f32).max(0.0);
            net >= target.hp as f32
        } else {
            false
        };
        if killed { 1.0f32 } else { 0.0f32 }
    };
    let p_kill_soon = if p_kill_now == 0.0 {
        estimate_kill_soon(def, target, caster, content)
    } else {
        0.0
    };
    let deny_value = estimate_deny_value(def, target, content);

    ActionOutcomeEstimate {
        expected_damage: score,
        p_kill_now,
        p_kill_soon,
        deny_value,
        ..Default::default()
    }
}

/// Max danger value along the path tiles of a single Move step.
/// Returns `0.0` for Cast steps.
///
/// Shared helper for `exposure_delta` in the outcome estimate. Uses only the
/// current step's path (not the whole plan) so each step's annotation is
/// independent.
pub fn step_path_danger(step: &PlanStep, maps: &InfluenceMaps) -> f32 {
    let PlanStep::Move { path } = step else { return 0.0 };
    path.iter().map(|&h| maps.danger.get(h)).fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// Private helpers (mirrors of private fns in factors::offensive)
// ---------------------------------------------------------------------------

fn already_pending_dot(target: &UnitSnapshot) -> f32 {
    target
        .statuses
        .iter()
        .map(|s| s.dot_per_tick.max(0) as f32 * s.rounds_remaining as f32)
        .sum()
}

fn dot_tick_sum_for_ability(
    def: &AbilityDef,
    target: &UnitSnapshot,
    content: &ContentView,
) -> f32 {
    status_applications(def, content)
        .map(|(sd, dur)| {
            let per_tick = sd.dot_dice.as_ref().map(|d| d.expected()).unwrap_or(0.0)
                + sd.hp_percent_dot as f32 / 100.0 * target.max_hp as f32;
            per_tick * dur
        })
        .filter(|&v| v > 0.0)
        .sum()
}

/// Crit-fail expected-value adjustment for heal (`rescue_value`).
/// Mirrors `factors::adjustments::crit_fail_adjusted` — same formula.
fn crit_fail_adjusted_rescue(
    score: f32,
    def: &AbilityDef,
    effect: &CritFailEffect,
    chance: f32,
) -> f32 {
    use crate::core::ResourceKind;
    match effect {
        CritFailEffect::ManaOverload => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score - chance * mana_cost
        }
        CritFailEffect::CircuitBreach => {
            let mana_cost: f32 = def
                .costs
                .iter()
                .filter(|c| c.resource == ResourceKind::Mana)
                .map(|c| c.amount as f32)
                .sum();
            score * (1.0 - chance) - chance * mana_cost * 0.5
        }
        _ => score * (1.0 - chance),
    }
}

/// Core HP-equivalent score for a single (ability, target) pair.
///
/// Inlined from the former `scoring::score_action` (deleted in step 4.5).
/// All callers that previously used `score_action` now call this instead;
/// formulas are bit-identical, verified by the golden-replay gate.
///
/// `danger_at_target` is only consumed by the heal branch (urgency weighting);
/// callers on the damage path pass `0.0`.
pub(crate) fn compute_score_core(
    def: &AbilityDef,
    target: &UnitSnapshot,
    ctx: &CasterContext,
    content: &ContentView,
    danger_at_target: f32,
) -> f32 {
    use crate::combat::ai::scoring::status_score;
    let Some(calc) = def.effect.calc(ctx) else {
        return if matches!(def.effect, EffectDef::GrantMovement { .. }) {
            0.0
        } else {
            status_score(def, target, content)
        };
    };

    let expected = calc.expected();

    let dmg_score = if calc.is_heal {
        let missing = (target.max_hp - target.hp) as f32;
        if missing <= 0.0 {
            return 0.0;
        }
        let effective = expected.min(missing);
        let delta_pct = effective / target.max_hp.max(1) as f32;
        let horizon_sum: f32 = target.damage_horizon.iter().sum::<f32>().max(target.threat);
        let hp_missing = 1.0 - target.hp_pct();
        let incoming = (danger_at_target / target.hp.max(1) as f32).min(1.0);
        let urgency = 1.0 + hp_missing.max(incoming).min(1.0);
        delta_pct * horizon_sum * urgency
    } else {
        let mitigation = if calc.pierces_armor {
            0.0
        } else {
            (target.armor + target.armor_bonus) as f32
        };
        let raw = (expected - mitigation + target.damage_taken_bonus as f32).max(0.0);
        let progress = (raw / target.hp.max(1) as f32).min(1.0);
        raw * (0.5 + 0.5 * progress)
    };

    dmg_score + status_score(def, target, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::snapshot::ActiveStatusView;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::content_view::ContentView;
    use crate::core::{AbilityId, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn db() -> ContentView {
        ContentView::load_global_for_tests()
    }

    fn get_def<'a>(content: &'a ContentView, id: &str) -> &'a AbilityDef {
        content.abilities.get(&AbilityId::from(id)).expect("ability not found")
    }

    fn melee_caster(str_mod: i32) -> CasterContext {
        CasterContext { str_mod, ..Default::default() }
    }

    #[test]
    fn default_outcome_is_zero() {
        let o = ActionOutcomeEstimate::default();
        assert_eq!(o.expected_damage, 0.0);
        assert_eq!(o.p_kill_now, 0.0);
        assert_eq!(o.p_kill_soon, 0.0);
        assert_eq!(o.deny_value, 0.0);
        assert_eq!(o.rescue_value, 0.0);
        assert_eq!(o.board_pressure, 0.0);
        assert_eq!(o.exposure_delta, 0.0);
        assert_eq!(o.geometry_gain, 0.0);
        assert_eq!(o.resource_swing, 0.0);
    }

    #[test]
    fn default_annotation_is_empty() {
        let a = PlanAnnotation::default();
        assert!(a.outcomes.is_empty());
    }

    // --- estimate_kill_soon ---
    //
    // `p_kill_now` lives on `outcome.p_kill_now` via sim (`StepOutcome.killed`);
    // these tests target the "DoT will finish it" signal that powers
    // `outcome.p_kill_soon`.

    /// When direct damage already kills, kill_soon returns 0 — p_kill_now (via
    /// sim.killed) covers this case, and the two fields are mutually exclusive.
    /// melee_attack, str_mod=2 → direct=2, hp=1 → direct kills.
    #[test]
    fn estimate_kill_soon_is_zero_when_direct_damage_kills() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).hp(1).build();
        let ks = estimate_kill_soon(
            get_def(&content, "melee_attack"), &target, &melee_caster(2), &content,
        );
        assert_eq!(ks, 0.0, "kill_soon=0 when direct damage kills (p_kill_now covers it)");
    }

    /// melee_attack with str_mod=0 → direct=0; pending DoT (3/tick × 2 rounds = 6) ≥ hp=5
    #[test]
    fn estimate_kill_soon_fires_on_pending_dot() {
        let content = db();
        let mut target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(5).build();
        target.statuses = vec![ActiveStatusView {
            id: StatusId::from("poisoned"),
            rounds_remaining: 2,
            dot_per_tick: 3,
        }];
        let ks = estimate_kill_soon(
            get_def(&content, "melee_attack"), &target, &melee_caster(0), &content,
        );
        assert_eq!(ks, 1.0, "pending DoT 6 ≥ hp=5 → kill_soon");
    }

    /// poison_shot: direct 1d4 (expected 2.5) + poisoned×3 (2.5/tick × 3 = 7.5) = 10 ≥ hp=5
    #[test]
    fn estimate_kill_soon_fires_on_new_dot_from_ability() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(5).build();
        let c = CasterContext::default();
        let ks = estimate_kill_soon(get_def(&content, "poison_shot"), &target, &c, &content);
        assert_eq!(ks, 1.0, "direct 2.5 + new DoT 7.5 = 10 ≥ hp=5 → kill_soon");
    }

    /// melee_attack with str_mod=0, no pending DoT: direct=0, combined=0 < hp=100
    #[test]
    fn estimate_kill_soon_zero_when_combined_insufficient() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(100).build();
        let ks = estimate_kill_soon(
            get_def(&content, "melee_attack"), &target, &melee_caster(0), &content,
        );
        assert_eq!(ks, 0.0);
    }

    /// Boundary case: expected=5.5 rounds to 6, hp=6 → direct kills, kill_soon=0.
    /// Pins the `.round()` behaviour in `estimate_kill_soon` so it stays in sync
    /// with sim's damage resolution.
    #[test]
    fn estimate_kill_soon_rounds_expected_to_match_sim() {
        use crate::core::DiceExpr;
        let content = db();
        let caster = CasterContext {
            str_mod: 2,
            weapon_dice: Some(DiceExpr::new(1, 6, 0)),
            ..Default::default()
        };
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).hp(6).build();
        let ks = estimate_kill_soon(get_def(&content, "melee_attack"), &target, &caster, &content);
        assert_eq!(ks, 0.0, "expected=5.5 rounds to 6 ≥ hp=6 → direct kills, kill_soon=0");
    }

    // --- estimate_deny_value ---

    /// stun_denial_value test: ability with skips_turn status should produce > 0 deny.
    /// Uses poison_shot as a proxy for an ability that applies statuses.
    /// For a pure CC scenario, use stun ability when available in test content.
    #[test]
    fn estimate_deny_value_zero_for_no_cc_ability() {
        let content = db();
        // melee_attack has no status effects -> deny_value = 0
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_deny_value(get_def(&content, "melee_attack"), &target, &content);
        assert_eq!(val, 0.0, "melee_attack applies no CC -> deny=0");
    }

    /// poison_shot applies poisoned status with dot — has no skips_turn or
    /// damage_taken_bonus, so deny_value = 0 (cc-denial subset only).
    #[test]
    fn estimate_deny_value_zero_for_dot_only_status() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_deny_value(get_def(&content, "poison_shot"), &target, &content);
        assert_eq!(val, 0.0, "poison_shot has DoT but no skips_turn/damage_taken_bonus -> deny=0");
    }

    // --- estimate_rescue_value ---

    /// Non-heal ability -> rescue_value = 0.
    #[test]
    fn estimate_rescue_value_zero_for_damage_ability() {
        let content = db();
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(10).build();
        let val = estimate_rescue_value(
            get_def(&content, "melee_attack"),
            &target,
            &CasterContext::default(),
            &content,
            0.0,
            &CritFailEffect::Miss,
            0.0,
        );
        assert_eq!(val, 0.0, "melee_attack is not a heal -> rescue=0");
    }

    /// Full-HP target -> rescue_value = 0 (no missing HP to heal).
    #[test]
    fn estimate_rescue_value_zero_for_full_hp_target() {
        let content = db();
        // full_hp means hp == max_hp, missing = 0
        let target = UnitBuilder::new(1, Team::Player, hex_from_offset(1, 0)).full_hp(20).build();
        // Use heal ability from content if available, otherwise skip gracefully.
        if let Some(def) = content.abilities.get(&AbilityId::from("heal")) {
            let val = estimate_rescue_value(
                def, &target, &CasterContext::default(), &content, 0.0,
                &CritFailEffect::Miss, 0.0,
            );
            assert_eq!(val, 0.0, "full-HP target -> rescue=0");
        }
    }

    // --- step_path_danger ---

    fn empty_maps_local() -> crate::combat::ai::influence::InfluenceMaps {
        use crate::combat::ai::influence::{InfluenceMap, InfluenceMaps};
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    /// Cast step -> exposure_delta = 0.
    #[test]
    fn step_path_danger_zero_for_cast() {
        use bevy::prelude::Entity;
        let maps = empty_maps_local();
        let step = PlanStep::Cast {
            ability: crate::core::AbilityId::from("melee_attack"),
            target: Entity::from_bits(1),
            target_pos: hex_from_offset(0, 0),
        };
        assert_eq!(step_path_danger(&step, &maps), 0.0);
    }

    /// Move through tiles with known danger -> max is returned.
    #[test]
    fn step_path_danger_returns_max_along_path() {
        use crate::game::hex::hex_from_offset;
        let mut maps = empty_maps_local();
        let h1 = hex_from_offset(0, 1);
        let h2 = hex_from_offset(0, 2);
        maps.danger.add(h1, 3.0);
        maps.danger.add(h2, 7.0);
        let step = PlanStep::Move { path: vec![h1, h2] };
        assert_eq!(step_path_danger(&step, &maps), 7.0);
    }

    // --- estimate_hypothetical ---

    /// `estimate_hypothetical(...).expected_damage` equals `compute_score_core(...)`
    /// for a damage ability — pins the contract that the outcome's HP-equivalent
    /// value is produced by the same formula as the sim-derived `expected_damage`.
    /// `future_value::attack_component_intent` relies on this for λ_attack.
    #[test]
    fn estimate_hypothetical_expected_damage_matches_compute_score_core() {
        let content = db();
        let def = get_def(&content, "melee_attack");
        let caster = melee_caster(2);
        let target = UnitBuilder::new(1, Team::Enemy, hex_from_offset(1, 0)).full_hp(20).build();

        let expected = compute_score_core(def, &target, &caster, &content, 0.0);
        let est = estimate_hypothetical(def, &target, &caster, &content, 0.0);

        assert!(
            (est.expected_damage - expected).abs() < 1e-6,
            "expected_damage {:.6} should equal compute_score_core {:.6}",
            est.expected_damage, expected
        );
    }

    /// `p_kill_now = 1.0` when net damage >= target.hp.
    #[test]
    fn estimate_hypothetical_kill_now_when_damage_exceeds_hp() {
        let content = db();
        let def = get_def(&content, "melee_attack");
        let caster = melee_caster(5); // high str_mod for guaranteed kill
        let target = UnitBuilder::new(1, Team::Enemy, hex_from_offset(1, 0)).hp(1).build();

        let est = estimate_hypothetical(def, &target, &caster, &content, 0.0);
        assert_eq!(est.p_kill_now, 1.0, "should detect kill when net_dmg >= hp");
        assert_eq!(est.p_kill_soon, 0.0, "p_kill_soon must be 0 when p_kill_now=1");
    }

    /// `deny_value` for a no-CC damage ability is 0.
    #[test]
    fn estimate_hypothetical_deny_zero_for_melee_attack() {
        let content = db();
        let def = get_def(&content, "melee_attack");
        let caster = melee_caster(0);
        let target = UnitBuilder::new(1, Team::Enemy, hex_from_offset(1, 0)).full_hp(20).build();
        let est = estimate_hypothetical(def, &target, &caster, &content, 0.0);
        assert_eq!(est.deny_value, 0.0, "melee_attack has no CC -> deny_value=0");
    }
}
