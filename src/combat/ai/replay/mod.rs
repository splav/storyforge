//! Replay assertion umbrella — overlay-based assertion DSL (this module) used by
//! integration tests and the `replay_ai_log` binary. The assertion pipeline
//! (JSONL → rebuilt decisions → overlay comparison) lives in [`pipeline`].
//!
//! # Overlay format (`*.expected.toml`)
//!
//! ```toml
//! [scope]
//! plan_id = 5   # optional; default = first entry in JSONL
//!
//! [[expectations]]
//! decision_kind = ["CastInPlace"]       # list ⇒ any-of
//! cast_ability  = ["fireball"]
//! intent_kind   = ["FocusTarget"]
//! primary_effect = ["Damage"]
//!
//! [[expectations]]
//! decision_kind = ["Move"]
//! intent_kind   = ["ProtectSelf"]
//! ```
//!
//! An assertion passes iff **at least one** variant in `[[expectations]]`
//! matches fully. An empty `[[expectations]]` list always passes.
//!
//! ## `decision_kind` mapping
//!
//! Four canonical strings: `"CastInPlace"` (cast, no prior move), `"MoveAndCast"`,
//! `"Move"` (both `MoveOnlyRetreat` and `MoveCloser` map here), `"EndTurn"`.
//!
//! ## `primary_effect` values
//!
//! Inferred from the first `Cast` step via `AbilityDef.effect`: `"Damage"`,
//! `"Heal"`, `"GrantMovement"`, `"RestoreResources"`, `"Summon"`, `"None"`.
//! Asserting it on a Move/EndTurn decision fails.

use serde::Deserialize;

use crate::combat::ai::plan::PlanStep;
use crate::content::abilities::EffectDef;
use crate::content::content_view::ActiveContentData;
use combat_engine::AbilityId;

// ── Overlay types ─────────────────────────────────────────────────────────────

/// Top-level overlay file.
#[derive(Debug, Deserialize, Default)]
pub struct Overlay {
    pub scope: Option<OverlayScope>,
    /// Optional AiMemory state to inject before pick_action runs.
    /// When present, sets `AiMemory.last_goal` from the overlay fields.
    /// When absent, `AiMemory` starts as default (no stored goal).
    pub ai_memory: Option<AiMemoryOverlay>,
    #[serde(default)]
    pub expectations: Vec<Expectation>,
}

/// Flat representation of `AiMemory` fields for TOML overlay injection. All
/// fields optional; `build_stored_goal` constructs `StoredGoalContext` when any
/// goal-kind-specific field is set. Flat (un-nested) for readable TOML tables.
#[derive(Debug, Deserialize, Default)]
pub struct AiMemoryOverlay {
    /// Goal kind: `"Finish"`, `"Pressure"`, `"DisableEnemy"`, `"HealAlly"`,
    /// `"Retreat"`, `"SetupAOE"`, or `"Reposition"`.
    pub last_goal_kind: Option<String>,
    /// Target entity bits (for targeted goals: Finish/Pressure/DisableEnemy/HealAlly).
    pub last_goal_target: Option<u64>,
    /// Hex anchor `[col, row]` for positional goals (Retreat/SetupAOE/Reposition).
    pub last_goal_region_anchor: Option<[i32; 2]>,
    /// Planned ability id for SetupAOE.
    pub last_goal_planned_ability: Option<String>,
    /// TTL remaining (default: 2).
    pub last_goal_ttl: Option<u8>,
    /// Confidence score at store time (default: 1.0).
    pub last_goal_confidence: Option<f32>,
    /// Round when goal was created (default: 0, meaning "long ago").
    pub last_goal_created_round: Option<u32>,
    /// Expected actor position at goal time `[col, row]`.
    pub last_goal_expected_actor_pos: Option<[i32; 2]>,
    /// Actor HP at store time (default: 0 — no hp-drop mismatch unless actor took damage).
    pub last_goal_actor_hp_at_store: Option<i32>,
    /// Actor rage at store time (default: 0).
    pub last_goal_actor_rage_at_store: Option<i32>,
    /// Actor status hash at store time (default: 0). String form (decimal or
    /// `0x`-prefixed hex) because TOML ints cap at i64 but this is u64.
    pub last_goal_actor_status_hash: Option<String>,
    /// Target HP at store time (default: 0).
    pub last_goal_target_hp_at_store: Option<i32>,
    /// Target position at store time `[col, row]` (default: [0, 0]).
    pub last_goal_target_pos_at_store: Option<[i32; 2]>,
    /// Region radius for repair affinity (default: 2).
    pub last_goal_region_radius: Option<u32>,
}

/// Selects which log entry to assert against.
#[derive(Debug, Deserialize)]
pub struct OverlayScope {
    /// `plan_id` from the JSONL entry. When absent, the first entry is used.
    pub plan_id: Option<u64>,
}

/// One assertion variant. All fields are optional (`#[serde(default)]` means
/// empty vec = "not specified, skip this field"). Assertion passes iff *all*
/// specified fields match.
///
/// All fields are lists. `[x]` means exact; `[x, y]` means any-of.
/// `not_*` fields are exclusion lists: passes iff actual ∉ list.
#[derive(Debug, Deserialize, Default)]
pub struct Expectation {
    /// Allowed decision kinds: `CastInPlace | MoveAndCast | Move | EndTurn`.
    #[serde(default)]
    pub decision_kind: Vec<String>,
    /// Allowed ability names (for Cast decisions).
    #[serde(default)]
    pub cast_ability: Vec<String>,
    /// Allowed target entity bits (for Cast decisions).
    #[serde(default)]
    pub cast_target: Vec<u64>,
    /// Allowed end positions as `[x, y]` pairs.
    #[serde(default)]
    pub end_position: Vec<[i32; 2]>,
    /// Allowed intent kind names.
    #[serde(default)]
    pub intent_kind: Vec<String>,
    /// Allowed primary effect kinds: `Damage | Heal | GrantMovement | RestoreResources | Summon | None`.
    #[serde(default)]
    pub primary_effect: Vec<String>,
    /// Excluded target entity bits — actual target must NOT be in this list.
    #[serde(default)]
    pub not_target: Vec<u64>,
    /// Excluded end positions — actual end_pos must NOT be in this list.
    #[serde(default)]
    pub not_end_position: Vec<[i32; 2]>,
    // ── v24 fields (step 6.5) — parsed but assertions deferred to 6.6 ────────
    /// Expected continuation_outcome code, e.g. `"goal_preserved_method_preserved"`.
    /// TODO(6.6): wire into `match_variant` once 5 new scenarios are added.
    #[serde(default)]
    pub continuation_outcome: Option<String>,
    /// Expected goal_kind code, e.g. `"finish"`, `"pressure"`.
    /// TODO(6.6): wire into `match_variant` once 5 new scenarios are added.
    #[serde(default)]
    pub goal_kind: Option<String>,
}

// ── Parsed actual decision ─────────────────────────────────────────────────────

/// Normalized representation of the chosen plan's committed action.
/// Built from the chosen `PlanLog` + intent + content lookup.
#[derive(Debug)]
pub struct ActualDecision {
    /// `"CastInPlace"`, `"MoveAndCast"`, `"Move"`, or `"EndTurn"`.
    pub decision_kind: String,
    /// Ability name — present only for Cast variants.
    pub cast_ability: Option<String>,
    /// Target entity bits — present only for Cast variants.
    pub cast_target: Option<u64>,
    /// Final position `[x, y]`.
    pub end_position: [i32; 2],
    /// Intent kind name.
    pub intent_kind: String,
    /// Primary effect kind — `None` when no Cast or unknown ability.
    pub primary_effect: Option<String>,
}

/// Extract decision_kind string from the first committed step(s) of the plan.
///
/// Follows the same classification logic as `committed_action_key`:
/// - empty steps → `"EndTurn"`
/// - first step Cast → `"CastInPlace"`
/// - first step Move, second step Cast → `"MoveAndCast"`
/// - first step Move, no Cast follows → `"Move"`
///
/// `MoveOnlyRetreat` and `MoveCloser` from `DecisionBlock` both map to `"Move"`.
fn decision_kind_from_steps(steps: &[PlanStep]) -> &'static str {
    match steps {
        [] => "EndTurn",
        [PlanStep::Cast { .. }, ..] => "CastInPlace",
        [PlanStep::Move { .. }, PlanStep::Cast { .. }, ..] => "MoveAndCast",
        _ => "Move",
    }
}

/// Infer `primary_effect` string from the first `Cast` step in the plan.
///
/// Looks up the ability in `content`. Returns `None` if no Cast step exists
/// or the ability is not found in content.
pub fn primary_effect_from_steps(
    steps: &[PlanStep],
    content: &ActiveContentData,
) -> Option<String> {
    let ability_id = steps.iter().find_map(|s| match s {
        PlanStep::Cast { ability, .. } => Some(ability.clone()),
        _ => None,
    })?;
    let def = content.abilities.get(&ability_id)?;
    let label = match &def.effect {
        EffectDef::Damage { .. }
        | EffectDef::SpellDamage { .. }
        | EffectDef::WeaponAttack { .. } => "Damage",
        EffectDef::Heal { .. } => "Heal",
        EffectDef::GrantMovement { .. } => "GrantMovement",
        EffectDef::RestoreResources => "RestoreResources",
        EffectDef::Summon { .. } => "Summon",
        EffectDef::RevealEnvInRange { .. } => "RevealEnvInRange",
        EffectDef::None => "None",
    };
    Some(label.to_string())
}

/// Build the ability id from the first Cast step of a plan, if any.
fn first_cast_ability(steps: &[PlanStep]) -> Option<&AbilityId> {
    steps.iter().find_map(|s| match s {
        PlanStep::Cast { ability, .. } => Some(ability),
        _ => None,
    })
}

/// Build the target entity bits from the first Cast step of a plan, if any.
fn first_cast_target_bits(steps: &[PlanStep]) -> Option<u64> {
    steps.iter().find_map(|s| match s {
        PlanStep::Cast { target, .. } => Some(target.to_bits()),
        _ => None,
    })
}

// ── Matching logic ──────────────────────────────────────────────────────────

/// Result of matching a single `Expectation` variant against `ActualDecision`.
#[derive(Debug)]
pub struct VariantMatchResult {
    /// Index (0-based) of the variant in `expectations`.
    pub variant_idx: usize,
    /// Field failures: `(field_name, description)`.
    pub failures: Vec<(String, String)>,
}

impl VariantMatchResult {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Match `actual` against a single `Expectation`.
fn match_variant(actual: &ActualDecision, exp: &Expectation, idx: usize) -> VariantMatchResult {
    let mut failures: Vec<(String, String)> = Vec::new();

    // decision_kind
    if !exp.decision_kind.is_empty()
        && !exp.decision_kind.iter().any(|k| k == &actual.decision_kind)
    {
        failures.push((
            "decision_kind".to_string(),
            format!(
                "expected ∈ {:?}, got {:?}",
                exp.decision_kind, actual.decision_kind
            ),
        ));
    }

    // intent_kind
    if !exp.intent_kind.is_empty() && !exp.intent_kind.iter().any(|k| k == &actual.intent_kind) {
        failures.push((
            "intent_kind".to_string(),
            format!(
                "expected ∈ {:?}, got {:?}",
                exp.intent_kind, actual.intent_kind
            ),
        ));
    }

    // end_position
    if !exp.end_position.is_empty() && !exp.end_position.contains(&actual.end_position) {
        failures.push((
            "end_position".to_string(),
            format!(
                "expected ∈ {:?}, got {:?}",
                exp.end_position, actual.end_position
            ),
        ));
    }

    // cast_ability
    if !exp.cast_ability.is_empty() {
        match &actual.cast_ability {
            Some(ab) if exp.cast_ability.iter().any(|k| k == ab) => {}
            Some(ab) => failures.push((
                "cast_ability".to_string(),
                format!("expected ∈ {:?}, got {:?}", exp.cast_ability, ab),
            )),
            None => failures.push((
                "cast_ability".to_string(),
                format!(
                    "expected ∈ {:?}, but decision is {:?} (no cast)",
                    exp.cast_ability, actual.decision_kind
                ),
            )),
        }
    }

    // cast_target
    if !exp.cast_target.is_empty() {
        match actual.cast_target {
            Some(t) if exp.cast_target.contains(&t) => {}
            Some(t) => failures.push((
                "cast_target".to_string(),
                format!("expected ∈ {:?}, got {}", exp.cast_target, t),
            )),
            None => failures.push((
                "cast_target".to_string(),
                format!(
                    "expected ∈ {:?}, but decision is {:?} (no cast)",
                    exp.cast_target, actual.decision_kind
                ),
            )),
        }
    }

    // primary_effect
    if !exp.primary_effect.is_empty() {
        match &actual.primary_effect {
            Some(pe) if exp.primary_effect.iter().any(|k| k == pe) => {}
            Some(pe) => failures.push((
                "primary_effect".to_string(),
                format!("expected ∈ {:?}, got {:?}", exp.primary_effect, pe),
            )),
            None => failures.push((
                "primary_effect".to_string(),
                format!(
                    "expected ∈ {:?}, but decision is {:?} (no cast or unknown ability)",
                    exp.primary_effect, actual.decision_kind
                ),
            )),
        }
    }

    // not_target
    if !exp.not_target.is_empty() {
        if let Some(t) = actual.cast_target {
            if exp.not_target.contains(&t) {
                failures.push((
                    "not_target".to_string(),
                    format!("target {} is in exclusion list {:?}", t, exp.not_target),
                ));
            }
        }
        // If no cast target, not_target is trivially satisfied.
    }

    // not_end_position
    if !exp.not_end_position.is_empty() && exp.not_end_position.contains(&actual.end_position) {
        failures.push((
            "not_end_position".to_string(),
            format!(
                "end_pos {:?} is in exclusion list {:?}",
                actual.end_position, exp.not_end_position
            ),
        ));
    }

    VariantMatchResult {
        variant_idx: idx,
        failures,
    }
}

// ── Public assertion API ─────────────────────────────────────────────────────

/// Result of running the full assertion.
#[derive(Debug)]
pub enum AssertResult {
    /// At least one variant matched.
    Pass,
    /// No variant matched. Contains per-variant failure reports.
    Fail(Vec<VariantMatchResult>),
}

/// Run the overlay assertion against `actual`.
///
/// Returns `Pass` when `expectations` is empty (vacuous pass).
pub fn run_assertion(actual: &ActualDecision, overlay: &Overlay) -> AssertResult {
    if overlay.expectations.is_empty() {
        return AssertResult::Pass;
    }
    let results: Vec<VariantMatchResult> = overlay
        .expectations
        .iter()
        .enumerate()
        .map(|(i, exp)| match_variant(actual, exp, i))
        .collect();

    if results.iter().any(|r| r.passed()) {
        AssertResult::Pass
    } else {
        AssertResult::Fail(results)
    }
}

/// Print a structured diff for a failed assertion to stderr.
pub fn print_assertion_failure(actual: &ActualDecision, results: &[VariantMatchResult]) {
    eprintln!("ASSERTION FAILED");
    eprintln!("  actual decision:");
    eprintln!("    decision_kind  = {:?}", actual.decision_kind);
    eprintln!("    intent_kind    = {:?}", actual.intent_kind);
    eprintln!("    cast_ability   = {:?}", actual.cast_ability);
    eprintln!("    cast_target    = {:?}", actual.cast_target);
    eprintln!("    end_position   = {:?}", actual.end_position);
    eprintln!("    primary_effect = {:?}", actual.primary_effect);
    eprintln!("  variants:");
    for r in results {
        eprintln!(
            "    variant [{}]: {} field(s) failed",
            r.variant_idx + 1,
            r.failures.len()
        );
        for (field, desc) in &r.failures {
            eprintln!("      {field}: {desc}");
        }
    }
}

/// Build `ActualDecision` from the chosen plan log entry and intent.
///
/// `steps` come from the chosen `PlanLog`;
/// `final_pos` is `[x, y]`; `intent_kind_str` is the intent kind name string
/// (e.g. `"FocusTarget"`); `content` is used for primary_effect lookup.
pub fn build_actual_decision(
    steps: &[PlanStep],
    final_pos: [i32; 2],
    intent_kind_str: &str,
    content: &ActiveContentData,
) -> ActualDecision {
    let decision_kind = decision_kind_from_steps(steps).to_string();
    let cast_ability = first_cast_ability(steps).map(|id| id.0.clone());
    let cast_target = first_cast_target_bits(steps);
    let primary_effect = primary_effect_from_steps(steps, content);

    ActualDecision {
        decision_kind,
        cast_ability,
        cast_target,
        end_position: final_pos,
        intent_kind: intent_kind_str.to_string(),
        primary_effect,
    }
}

// ── Pipeline sub-module (assertion pipeline: JSONL reader + GoldenRecord) ────

pub mod pipeline;

pub use pipeline::{
    assert_v28_log_file, default_overlay_path, load_overlay, AssertError, AssertOutcome,
    GoldenRecord,
};

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn actual(
        decision_kind: &str,
        intent_kind: &str,
        cast_ability: Option<&str>,
        cast_target: Option<u64>,
        end_position: [i32; 2],
        primary_effect: Option<&str>,
    ) -> ActualDecision {
        ActualDecision {
            decision_kind: decision_kind.to_string(),
            intent_kind: intent_kind.to_string(),
            cast_ability: cast_ability.map(|s| s.to_string()),
            cast_target,
            end_position,
            primary_effect: primary_effect.map(|s| s.to_string()),
        }
    }

    fn cast_actual() -> ActualDecision {
        actual(
            "CastInPlace",
            "FocusTarget",
            Some("fireball"),
            Some(42),
            [3, 4],
            Some("Damage"),
        )
    }

    fn move_actual() -> ActualDecision {
        actual("Move", "ProtectSelf", None, None, [5, 5], None)
    }

    fn parse_overlay(toml: &str) -> Overlay {
        toml::from_str(toml).expect("parse overlay")
    }

    // (i) Empty overlay → pass
    #[test]
    fn empty_overlay_passes() {
        let overlay = parse_overlay("");
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // Also pass when expectations key exists but is empty
    #[test]
    fn empty_expectations_passes() {
        let overlay = parse_overlay("[scope]\nplan_id = 1\n");
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // (ii) Single variant, exact match → pass
    #[test]
    fn single_variant_exact_match_passes() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
decision_kind = ["CastInPlace"]
intent_kind   = ["FocusTarget"]
cast_ability  = ["fireball"]
primary_effect = ["Damage"]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // (iii) Single variant, any-of match → pass
    #[test]
    fn single_variant_any_of_match_passes() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
decision_kind  = ["CastInPlace", "MoveAndCast"]
intent_kind    = ["FocusTarget", "ApplyCC"]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // (iv) not_target: excluded target → fail; not-in-list → pass
    #[test]
    fn not_target_excludes_actual_fails() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
not_target = [42]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Fail(_)
        ));
    }

    #[test]
    fn not_target_not_in_list_passes() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
not_target = [99]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // (v) Two variants, first fails, second passes → pass (OR logic)
    #[test]
    fn two_variants_first_fails_second_passes() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
decision_kind = ["EndTurn"]

[[expectations]]
decision_kind = ["CastInPlace", "MoveAndCast"]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }

    // (vi) Two variants, both fail → Fail, diff mentions both
    #[test]
    fn two_variants_both_fail_mentions_both() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
decision_kind = ["EndTurn"]

[[expectations]]
decision_kind = ["Move"]
"#,
        );
        match run_assertion(&cast_actual(), &overlay) {
            AssertResult::Fail(results) => {
                assert_eq!(results.len(), 2);
                assert!(!results[0].passed());
                assert!(!results[1].passed());
            }
            AssertResult::Pass => panic!("expected Fail"),
        }
    }

    // (vii) primary_effect mismatch on Move-only decision → fail
    #[test]
    fn primary_effect_mismatch_on_move_fails() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
primary_effect = ["Damage"]
"#,
        );
        // move_actual has primary_effect = None
        assert!(matches!(
            run_assertion(&move_actual(), &overlay),
            AssertResult::Fail(_)
        ));
    }

    // decision_kind mismatch → fail
    #[test]
    fn decision_kind_mismatch_fails() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
decision_kind = ["Move"]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Fail(_)
        ));
    }

    // not_end_position: actual in list → fail
    #[test]
    fn not_end_position_in_list_fails() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
not_end_position = [[3, 4]]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Fail(_)
        ));
    }

    // not_end_position: actual not in list → pass
    #[test]
    fn not_end_position_not_in_list_passes() {
        let overlay = parse_overlay(
            r#"
[[expectations]]
not_end_position = [[9, 9]]
"#,
        );
        assert!(matches!(
            run_assertion(&cast_actual(), &overlay),
            AssertResult::Pass
        ));
    }
}
