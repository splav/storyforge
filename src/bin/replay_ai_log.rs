//! Replay an AI decision log (JSONL) and show what the **current** sanity
//! pipeline does to each entry's ranking. For every log line the tool:
//!
//! Optional `--phase7-prototype` flag runs a dual pipeline: baseline scoring
//! plus the Phase 7 decomposition prototype (`score_plans_prototype` from
//! `future_value.rs`). Prints extra metrics blocks when combined with
//! `--metrics-summary`. No effect on output when flag is absent (invariance).
//!
//! 1. Parses the entry (snapshot, intent, plan pool with raw factors).
//! 2. Rebuilds `InfluenceMaps` deterministically from the snapshot.
//! 3. Feeds the logged raw factors through the live `finalize_scores` so
//!    scores match production bit-for-bit (summon_bonus, trade_bonus,
//!    hash-based noise, batch normalisation).
//! 4. Runs `sanity_adjust_plans` on that score vector.
//! 5. Picks the post-sanity winner via the live `pick_best_plan` (mercy
//!    + top-K RNG tiebreak). Pre-sanity top uses argmax as a diagnostic
//!      reference.
//! 6. Prints the original top plan and the post-sanity top plan side-by-side,
//!    flagging entries where the choice changed.
//!
//! Usage: `cargo run --bin replay_ai_log -- logs/<file>.jsonl [--verbose]`.

use std::cmp::Reverse;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use bevy::prelude::Entity;
use serde::Deserialize;

use storyforge::combat::ai::difficulty::DifficultyProfile;
use storyforge::combat::ai::factors::{PlanFactors, DAMAGE_IDX, KILL_NOW_IDX, SELF_SURVIVAL_IDX};
use storyforge::combat::ai::influence::{build_influence_maps, InfluenceConfig};
use storyforge::combat::ai::planning::{
    apply_protect_self_mask, finalize_scores, pick_best_plan,
    sanity_adjust_plans, CommittedPrefix, PlanStep, StepOutcome, TurnPlan,
};
use storyforge::combat::ai::planning::future_value::{plan_prefix_only, score_plans_prototype};
use storyforge::combat::ai::planning::sanity::SELF_SURVIVAL_EPSILON;
use storyforge::combat::ai::SanityHit;
use storyforge::combat::ai::role::AxisProfile;
use storyforge::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use storyforge::combat::ai::reservations::Reservations;
use storyforge::combat::ai::utility::{AiWorld, ScoringCtx};
use storyforge::content::content_view::ContentView;
use storyforge::core::DiceRng;
use storyforge::game::hex::Hex;

/// Mirror of `log::AiLogEntry` with owned fields so we can deserialize.
#[derive(Deserialize)]
struct LogEntry {
    schema_version: u32,
    #[allow(dead_code)]
    plan_id: u64,
    #[allow(dead_code)]
    timestamp_ms: u128,
    decision_time_ms: u64,
    round: u32,
    actor_id: u64,
    actor_name: String,
    actor_ap: i32,
    actor_max_ap: i32,
    actor_mp: i32,
    #[allow(dead_code)]
    actor_max_mp: i32,
    plans_evaluated: usize,
    #[allow(dead_code)]
    plans_shown: usize,
    snapshot: BattleSnapshot,
    intent: IntentBlock,
    plans: Vec<PlanLog>,
    committed_decision: serde_json::Value,
    /// v15+: killable gate telemetry. v14 and earlier default to false/0.
    #[serde(default)]
    #[allow(dead_code)]
    gate_applied: bool,
    #[serde(default)]
    #[allow(dead_code)]
    gate_pruned_count: usize,
    #[serde(default)]
    #[allow(dead_code)]
    survival_mode_active: bool,
    #[serde(default)]
    #[allow(dead_code)]
    last_stand_active: bool,
}

#[derive(Deserialize)]
struct IntentBlock {
    intent: storyforge::combat::ai::intent::TacticalIntent,
    selection_kind: String,
    // reason_text is present in the log schema but unused here; serde
    // tolerates it via #[serde(default)] on a dropped field.
    #[serde(default, rename = "reason_text")]
    _reason_text: String,
}

#[derive(Deserialize)]
struct PlanLog {
    rank: usize,
    chosen: bool,
    steps: Vec<PlanStep>,
    outcomes: Vec<StepOutcome>,
    final_pos: [i32; 2],
    residual_ap: i32,
    residual_mp: i32,
    /// v1-v9: 9 elements; v10: 10 (tempo_gain); v11: 11 (kill split); v12: 12 (saturation);
    /// v13: 13 (self_survival); v14+: 10 elements (position/risk/focus removed,
    /// indices renumbered — not backward-compatible with v1–v13 raw factor layout).
    /// Using `Vec<f32>` so serde handles all lengths; callers pad/truncate to NUM_FACTORS.
    raw_factors: Vec<f32>,
    /// `None` when the game pruned the plan before scoring (e.g. beam-search
    /// early rejection). Such plans still appear in the log so we can see
    /// what was considered, but they have no numeric score to compare
    /// against. Replay treats them as NEG_INFINITY — identical to a plan
    /// masked by sanity — so `argmax` naturally ignores them.
    score: Option<f32>,
    /// v6+: pre-adaptation score. Older logs default to `None` (no
    /// adaptation concept existed). Reserved for future `--show-adapt`
    /// diff mode; currently the replayer recomputes its own base via
    /// `raw_factors` so the logged number isn't used in rendering, but
    /// it's kept on `PlanLog` so offline analyzers can round-trip it.
    #[serde(default)]
    #[allow(dead_code)]
    base_score: Option<f32>,
    /// v6+: which evaluation regime scored this plan. Older logs default
    /// to `Default`.
    #[serde(default)]
    evaluation_mode: LoggedEvaluationMode,
    /// v6+: fact that triggered a mode switch; `None` when
    /// `evaluation_mode = Default`.
    #[serde(default)]
    adaptation_reason: Option<LoggedAdaptationReason>,
    /// v7+: per-plan trade breakdown + post-tanh score contribution.
    /// Older logs default to an all-zero block — render suppresses the
    /// line when `delta == 0 && !self_lethal`.
    #[serde(default)]
    trade: LoggedTradeBlock,
    /// v16+: per-rule sanity breakdown. v15 and earlier logs default to
    /// an empty vec via `#[serde(default)]`.
    #[serde(default)]
    #[allow(dead_code)]
    sanity_breakdown: Vec<SanityHit>,
}

/// Mirrors `log::TradeBlock`. Verbose-only rendering; not consumed by
/// the scoring reconstruction.
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[allow(dead_code)]
struct LoggedTradeBlock {
    #[serde(default)]
    delta: f32,
    #[serde(default)]
    killed: f32,
    #[serde(default)]
    lost: f32,
    #[serde(default)]
    self_lost: f32,
    #[serde(default)]
    self_lethal: bool,
    #[serde(default)]
    score: f32,
}

/// Mirrors `planning::adaptation::EvaluationMode` for deserialization.
/// Keep in sync with the enum's serde rename when variants change.
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LoggedEvaluationMode {
    #[default]
    Default,
    LastStand,
}

impl LoggedEvaluationMode {
    fn is_adapted(self) -> bool {
        !matches!(self, LoggedEvaluationMode::Default)
    }
}

/// Mirrors `planning::adaptation::AdaptationReason` for deserialization.
/// We don't need the numeric payloads during replay — just the kind —
/// so the variants are kept unit and tagged like the game enum.
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LoggedAdaptationReason {
    ExpectedSelfLethal {
        #[serde(default)]
        #[allow(dead_code)]
        aoo_dmg: f32,
        #[serde(default)]
        #[allow(dead_code)]
        actor_hp: i32,
    },
    ProtectSelfNoDefensive,
    ProtectSelfFutile {
        #[serde(default)]
        #[allow(dead_code)]
        pending_dot: i32,
        #[serde(default)]
        #[allow(dead_code)]
        actor_hp: i32,
    },
}

impl LoggedAdaptationReason {
    fn code(&self) -> &'static str {
        match self {
            Self::ExpectedSelfLethal { .. } => "expected_self_lethal",
            Self::ProtectSelfNoDefensive => "protect_self_no_defensive",
            Self::ProtectSelfFutile { .. } => "protect_self_futile",
        }
    }
}

// ── Regression metrics ──────────────────────────────────────────────────────

/// Cumulative regression counters across all processed log entries.
/// See `docs/ai-replay.md §Regression metrics` for definitions.
#[derive(Default)]
struct Metrics {
    /// Committed-prefix is `MoveOnly`.
    move_only_total: usize,
    /// … and destination == actor's starting position (displacement = 0).
    move_only_wasted: usize,
    /// Intent == ProtectSelf AND chosen plan's evaluation_mode == Default
    /// (mask was applied; LastStand entries are excluded — their non-defensive
    /// commit is by design, not a leak).
    panic_total: usize,
    /// … and committed action is non-defensive (attack / move-closer).
    panic_leaked: usize,
    /// Entry's `selection_kind == "killable"`.
    killable_total: usize,
    /// … and chosen plan's `raw_factors[KILL_NOW_IDX] > 0`.
    killable_closed: usize,
    /// Chosen plans that contain ≥1 Move step.
    plans_with_moves: usize,
    /// … and ≥1 tile is visited more than once across all Move paths (including start).
    repeated_tile_plans: usize,
    /// … (among plans_with_moves) and final_pos == actor start pos.
    zero_net_move_plans: usize,
    /// Chosen plans that have a Cast step followed by ≥1 Move step.
    plans_with_post_cast_move: usize,
    /// … and the post-cast move revisits a pre-cast tile AND net displacement ≤ pre-cast.
    post_cast_retreat_plans: usize,

    // ── Killable kill-line metrics (step-2 checkpoint) ───────────────────────
    /// Entries where intent=FocusTarget(killable) AND ≥1 plan in the pool has a
    /// "real kill-line": `offensive_vs_target && (committed_prefix_kills_target
    /// OR damage ≥ hp·α)`. Mirrors production gate's intent-coherent definition
    /// (see `killable_gate.rs::apply_killable_gate`). Denominator for
    /// killable_non_offensive_rate / wrong_target_rate / kill_conversion_rate.
    /// See `docs/ai_rework.md §5.2` for rationale.
    killable_with_kill_line_total: usize,
    /// … and chosen plan is non-offensive (no Cast vs intent target).
    killable_non_offensive: usize,
    /// … and chosen plan is offensive (has Cast) but no Cast targets intent.target.
    killable_wrong_target: usize,
    /// … and chosen plan's `raw_factors[KILL_NOW_IDX] >= 1.0` (target actually killed).
    killable_kill_converted: usize,

    // ── Phantom-tail metrics ──────────────────────────────────────────────────
    /// Chosen plans that contain ≥1 Cast step (denominator for phantom_tail_chosen_rate).
    chosen_with_cast_total: usize,
    /// … and have a post-cast Move step (phantom tail — not committed this tick).
    phantom_tail_chosen: usize,
    /// Of phantom-tail choices: best tailless alt has a DIFFERENT committed action key.
    phantom_tail_flips_committed: usize,

    // ── Phase 7 prototype (only populated when --phase7-prototype is set) ─────
    proto: Option<Box<PrototypeMetrics>>,
    // ── P2 bucketed ranking (only populated when --phase7-p2 is set) ──────────
    p2: Option<Box<P2Metrics>>,
}

/// Metrics collected from the Phase 7 prototype pipeline.
/// Mirrors the acceptance-metric set from the baseline pipeline so delta-pp can
/// be computed in `print_summary`.
#[derive(Default)]
struct PrototypeMetrics {
    /// Total entries processed under the prototype pipeline (== Metrics total entries).
    total_entries: usize,

    // ── ranking_change_rate ──────────────────────────────────────────────────
    /// Entries where `committed_action_key` differs between baseline and prototype winner.
    ranking_changed: usize,

    // ── plateau_tie_rate ─────────────────────────────────────────────────────
    /// Baseline: entries classified as plateau (≥3 plans within 0.05 of top).
    plateau_ties_baseline: usize,
    /// Prototype: same formula on prototype scores.
    plateau_ties_prototype: usize,

    // ── Mirrored acceptance metrics (prototype pick) ─────────────────────────
    move_only_total: usize,
    move_only_wasted: usize,
    panic_total: usize,
    panic_leaked: usize,
    killable_with_kill_line_total: usize,
    killable_non_offensive: usize,
    killable_wrong_target: usize,
    killable_kill_converted: usize,
    plans_with_moves: usize,
    repeated_tile_plans: usize,
    zero_net_move_plans: usize,
    plans_with_post_cast_move: usize,
    post_cast_retreat_plans: usize,
    chosen_with_cast_total: usize,
    phantom_tail_chosen: usize,
    phantom_tail_flips_committed: usize,
}

/// Metrics collected from the P2 bucketed-ranking pipeline.
/// Same acceptance-metric set as `PrototypeMetrics` — parallel to baseline for delta-pp.
#[derive(Default)]
struct P2Metrics {
    total_entries: usize,

    ranking_changed: usize,

    plateau_ties_baseline: usize,
    plateau_ties_p2: usize,

    move_only_total: usize,
    move_only_wasted: usize,
    panic_total: usize,
    panic_leaked: usize,
    killable_with_kill_line_total: usize,
    killable_non_offensive: usize,
    killable_wrong_target: usize,
    killable_kill_converted: usize,
    plans_with_moves: usize,
    repeated_tile_plans: usize,
    zero_net_move_plans: usize,
    plans_with_post_cast_move: usize,
    post_cast_retreat_plans: usize,
    chosen_with_cast_total: usize,
    phantom_tail_chosen: usize,
    phantom_tail_flips_committed: usize,
}

impl Metrics {
    fn print_summary(&self) {
        println!("\n=== Regression Metrics ===");

        let wasted = if self.move_only_total > 0 {
            self.move_only_wasted as f64 / self.move_only_total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "wasted_mp_ratio:      {:5.1}%  ({}/{} MoveOnly commits with displacement=0)",
            wasted, self.move_only_wasted, self.move_only_total,
        );

        let leak = if self.panic_total > 0 {
            self.panic_leaked as f64 / self.panic_total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "panic_leak_rate:      {:5.1}%  ({}/{} ProtectSelf+Default-mode entries → non-defensive commit)",
            leak, self.panic_leaked, self.panic_total,
        );

        let closure = if self.killable_total > 0 {
            self.killable_closed as f64 / self.killable_total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "killable_closure_rate:{:5.1}%  ({}/{} killable intents → kill factor > 0)",
            closure, self.killable_closed, self.killable_total,
        );

        let repeated = if self.plans_with_moves > 0 {
            self.repeated_tile_plans as f64 / self.plans_with_moves as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "repeated_tile_rate:   {:5.1}%  ({}/{} plans-with-moves revisit ≥1 tile)  [target <5%]",
            repeated, self.repeated_tile_plans, self.plans_with_moves,
        );

        let zero_net = if self.plans_with_moves > 0 {
            self.zero_net_move_plans as f64 / self.plans_with_moves as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "zero_net_move_rate:   {:5.1}%  ({}/{} plans-with-moves end at start pos)  [target <1%]",
            zero_net, self.zero_net_move_plans, self.plans_with_moves,
        );

        let retreat = if self.plans_with_post_cast_move > 0 {
            self.post_cast_retreat_plans as f64 / self.plans_with_post_cast_move as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "post_cast_retreat_rate:{:4.1}%  ({}/{} post-cast-move plans retreat & revisit tile)  [target ↓≥70%]",
            retreat, self.post_cast_retreat_plans, self.plans_with_post_cast_move,
        );

        // ── Killable kill-line metrics (step-2 checkpoint) ───────────────────
        let killable_denom = self.killable_with_kill_line_total;
        let non_off = pct(self.killable_non_offensive, killable_denom);
        println!(
            "killable_non_offensive_rate:  {:5.1}%  ({}/{} killable+kill_line → chosen not offensive vs target)  [target <2%]",
            non_off, self.killable_non_offensive, killable_denom,
        );
        let wrong_tgt = pct(self.killable_wrong_target, killable_denom);
        println!(
            "killable_wrong_target_rate:   {:5.1}%  ({}/{} killable+kill_line → offensive but wrong target)  [target <5%]",
            wrong_tgt, self.killable_wrong_target, killable_denom,
        );
        let conv = pct(self.killable_kill_converted, killable_denom);
        println!(
            "kill_conversion_rate:         {:5.1}%  ({}/{} killable+kill_line → committed prefix kills target)  [target >85%]",
            conv, self.killable_kill_converted, killable_denom,
        );

        // ── Phantom-tail metrics ──────────────────────────────────────────────
        let phantom_rate = pct(self.phantom_tail_chosen, self.chosen_with_cast_total);
        println!(
            "phantom_tail_chosen_rate:     {:5.1}%  ({}/{} chosen-with-cast plans have post-cast Move tail)",
            phantom_rate, self.phantom_tail_chosen, self.chosen_with_cast_total,
        );
        let flip_rate = pct(self.phantom_tail_flips_committed, self.phantom_tail_chosen);
        println!(
            "phantom_tail_flips_committed: {:5.1}%  ({}/{} phantom-tail choices → best tailless alt has different committed action)",
            flip_rate, self.phantom_tail_flips_committed, self.phantom_tail_chosen,
        );

        if let Some(p) = &self.proto {
            self.print_prototype_summary(p);
        }
        if let Some(p) = &self.p2 {
            self.print_p2_summary(p);
        }
    }

    fn print_prototype_summary(&self, p: &PrototypeMetrics) {
        println!("\n=== Phase 7 Prototype Metrics ===");
        let total = p.total_entries;

        let change_rate = pct(p.ranking_changed, total);
        println!(
            "ranking_change_rate:          {:5.1}%  ({}/{} entries where prototype picks different committed action)",
            change_rate, p.ranking_changed, total,
        );

        let plateau_base = pct(p.plateau_ties_baseline, total);
        let plateau_proto = pct(p.plateau_ties_prototype, total);
        let plateau_delta = plateau_proto - plateau_base;
        println!(
            "plateau_tie_rate:  baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp  [target prototype < 10%]",
            plateau_base, plateau_proto, plateau_delta,
        );

        // ── Mirrored acceptance metrics ───────────────────────────────────────
        println!("\n  -- Acceptance metrics (prototype pick) --");

        let wasted_b = pct(self.move_only_wasted, self.move_only_total);
        let wasted_p = pct(p.move_only_wasted, p.move_only_total);
        println!(
            "  wasted_mp_ratio:      baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            wasted_b, wasted_p, wasted_p - wasted_b,
        );

        let leak_b = pct(self.panic_leaked, self.panic_total);
        let leak_p = pct(p.panic_leaked, p.panic_total);
        println!(
            "  panic_leak_rate:      baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            leak_b, leak_p, leak_p - leak_b,
        );

        let kll_denom_b = self.killable_with_kill_line_total;
        let kll_denom_p = p.killable_with_kill_line_total;
        let non_off_b = pct(self.killable_non_offensive, kll_denom_b);
        let non_off_p = pct(p.killable_non_offensive, kll_denom_p);
        println!(
            "  killable_non_offensive_rate: baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            non_off_b, non_off_p, non_off_p - non_off_b,
        );
        let conv_b = pct(self.killable_kill_converted, kll_denom_b);
        let conv_p = pct(p.killable_kill_converted, kll_denom_p);
        println!(
            "  kill_conversion_rate:        baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            conv_b, conv_p, conv_p - conv_b,
        );

        let rep_b = pct(self.repeated_tile_plans, self.plans_with_moves);
        let rep_p = pct(p.repeated_tile_plans, p.plans_with_moves);
        println!(
            "  repeated_tile_rate:   baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            rep_b, rep_p, rep_p - rep_b,
        );
        let zero_b = pct(self.zero_net_move_plans, self.plans_with_moves);
        let zero_p = pct(p.zero_net_move_plans, p.plans_with_moves);
        println!(
            "  zero_net_move_rate:   baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            zero_b, zero_p, zero_p - zero_b,
        );
        let post_b = pct(self.post_cast_retreat_plans, self.plans_with_post_cast_move);
        let post_p = pct(p.post_cast_retreat_plans, p.plans_with_post_cast_move);
        println!(
            "  post_cast_retreat_rate: baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            post_b, post_p, post_p - post_b,
        );
        let phantom_b = pct(self.phantom_tail_chosen, self.chosen_with_cast_total);
        let phantom_p = pct(p.phantom_tail_chosen, p.chosen_with_cast_total);
        println!(
            "  phantom_tail_chosen_rate: baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp",
            phantom_b, phantom_p, phantom_p - phantom_b,
        );
        let flip_b = pct(self.phantom_tail_flips_committed, self.phantom_tail_chosen);
        let flip_p = pct(p.phantom_tail_flips_committed, p.phantom_tail_chosen);
        println!(
            "  phantom_tail_flips_committed: baseline={:.1}%  prototype={:.1}%  Δ={:+.1} pp  [target prototype ≥40% drop]",
            flip_b, flip_p, flip_p - flip_b,
        );
    }

    fn print_p2_summary(&self, p: &P2Metrics) {
        println!("\n=== Phase 7 P2 Bucketed Ranking Metrics ===");
        let total = p.total_entries;

        let change_rate = pct(p.ranking_changed, total);
        println!(
            "ranking_change_rate:          {:5.1}%  ({}/{} entries where P2 picks different committed action)",
            change_rate, p.ranking_changed, total,
        );

        let plateau_base = pct(p.plateau_ties_baseline, total);
        let plateau_p2 = pct(p.plateau_ties_p2, total);
        let plateau_delta = plateau_p2 - plateau_base;
        println!(
            "plateau_tie_rate:  baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp  [target ≤ baseline+5 pp]",
            plateau_base, plateau_p2, plateau_delta,
        );

        println!("\n  -- Acceptance metrics (P2 pick) --");

        let leak_b = pct(self.panic_leaked, self.panic_total);
        let leak_p = pct(p.panic_leaked, p.panic_total);
        println!(
            "  panic_leak_rate:      baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp  [target ≤ baseline+5 pp]",
            leak_b, leak_p, leak_p - leak_b,
        );

        let kll_denom_b = self.killable_with_kill_line_total;
        let kll_denom_p = p.killable_with_kill_line_total;
        let non_off_b = pct(self.killable_non_offensive, kll_denom_b);
        let non_off_p = pct(p.killable_non_offensive, kll_denom_p);
        println!(
            "  killable_non_offensive_rate: baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp  [target ≤ 2%]",
            non_off_b, non_off_p, non_off_p - non_off_b,
        );
        let conv_b = pct(self.killable_kill_converted, kll_denom_b);
        let conv_p = pct(p.killable_kill_converted, kll_denom_p);
        println!(
            "  kill_conversion_rate:        baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp  [target ≥ 80%]",
            conv_b, conv_p, conv_p - conv_b,
        );

        let rep_b = pct(self.repeated_tile_plans, self.plans_with_moves);
        let rep_p = pct(p.repeated_tile_plans, p.plans_with_moves);
        println!(
            "  repeated_tile_rate:   baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp",
            rep_b, rep_p, rep_p - rep_b,
        );
        let zero_b = pct(self.zero_net_move_plans, self.plans_with_moves);
        let zero_p = pct(p.zero_net_move_plans, p.plans_with_moves);
        println!(
            "  zero_net_move_rate:   baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp",
            zero_b, zero_p, zero_p - zero_b,
        );
        let post_b = pct(self.post_cast_retreat_plans, self.plans_with_post_cast_move);
        let post_p = pct(p.post_cast_retreat_plans, p.plans_with_post_cast_move);
        println!(
            "  post_cast_retreat_rate: baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp",
            post_b, post_p, post_p - post_b,
        );
        let phantom_b = pct(self.phantom_tail_chosen, self.chosen_with_cast_total);
        let phantom_p = pct(p.phantom_tail_chosen, p.chosen_with_cast_total);
        println!(
            "  phantom_tail_chosen_rate: baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp",
            phantom_b, phantom_p, phantom_p - phantom_b,
        );
        let flip_b = pct(self.phantom_tail_flips_committed, self.phantom_tail_chosen);
        let flip_p = pct(p.phantom_tail_flips_committed, p.phantom_tail_chosen);
        println!(
            "  phantom_tail_flips_committed: baseline={:.1}%  p2={:.1}%  Δ={:+.1} pp  [not a P2 target]",
            flip_b, flip_p, flip_p - flip_b,
        );
    }
}

fn pct(num: usize, denom: usize) -> f64 {
    if denom > 0 { num as f64 / denom as f64 * 100.0 } else { 0.0 }
}

// ── P2 bucketed ranking ──────────────────────────────────────────────────────

/// Rank offset large enough to make any rank difference dominate the scalar
/// score differential. Scores from `finalize_scores` stay in ~[-5, +5] after
/// sanity; 100 clears that comfortably.
const P2_RANK_OFFSET: f32 = 100.0;

/// Classify the bucket rank for a `FocusTarget{T}` plan.
///
/// Evaluates the **committed prefix only** (steps/outcomes up to `prefix_len`).
/// Returns:
/// - `2` if the prefix kills T (outcome.killed contains T at any Cast-vs-T step).
/// - `1` if offensive vs T AND prefix damage on T ≥ α * target_hp.
/// - `0` if offensive vs T (weak offense, damage below threshold).
/// - `-1` otherwise (off-intent: no Cast targeting T in prefix).
///
/// `target_hp` is the snapshot HP before this tick (not modified by this plan's prefix).
/// α = `KILLABLE_ALPHA` (0.3), kept in sync with `killable_gate.rs`.
pub fn classify_focus_target_bucket(
    steps: &[storyforge::combat::ai::planning::PlanStep],
    outcomes: &[storyforge::combat::ai::planning::StepOutcome],
    target: bevy::prelude::Entity,
    target_hp: i32,
) -> i32 {
    use storyforge::combat::ai::planning::PlanStep;

    // Committed prefix length: same rule as `plan_prefix_only` / `committed_step_count`.
    let prefix_len = match steps.first() {
        None => 0,
        Some(PlanStep::Cast { .. }) => 1,
        Some(PlanStep::Move { .. }) => {
            if matches!(steps.get(1), Some(PlanStep::Cast { .. })) { 2 } else { 1 }
        }
    };

    let mut prefix_kills = false;
    let mut prefix_damage_on_t: f32 = 0.0;
    let mut offensive_vs_t = false;

    for (step, outcome) in steps
        .iter()
        .take(prefix_len)
        .zip(outcomes.iter())
    {
        if let PlanStep::Cast { target: t, .. } = step {
            if *t == target {
                offensive_vs_t = true;
                prefix_damage_on_t += outcome.damage;
                if outcome.killed.contains(&target) {
                    prefix_kills = true;
                }
            }
        }
    }

    if prefix_kills {
        return 2;
    }
    let hp_f = target_hp.max(0) as f32;
    if offensive_vs_t && prefix_damage_on_t >= hp_f * KILLABLE_ALPHA {
        return 1;
    }
    if offensive_vs_t {
        return 0;
    }
    -1
}

/// Classify the bucket rank for a `ProtectSelf` plan.
///
/// Uses `self_survival` from plan factors (computed on committed prefix in P2 pipeline).
/// Returns:
/// - `1` if `self_survival >= SELF_SURVIVAL_EPSILON` (0.15).
/// - `0` otherwise.
pub fn classify_protect_self_bucket(self_survival: f32) -> i32 {
    if self_survival >= SELF_SURVIVAL_EPSILON { 1 } else { 0 }
}

/// Apply P2 bucket reranking to an already-scored (and sanity/mask-adjusted) score vector.
///
/// For `FocusTarget{T}`: classifies each plan's committed prefix, assigns a bucket rank,
/// and shifts the score by `rank * P2_RANK_OFFSET` so lexicographic (rank, scalar) ordering
/// is represented as a single float comparison.
///
/// For `ProtectSelf`: uses `raw_factors[SELF_SURVIVAL_IDX]` on the committed prefix plan.
///
/// Other intents: no-op (scores unchanged, invariance preserved).
///
/// Plans already at `-∞` (masked out) are skipped — their rank can't lift them.
fn apply_p2_bucket_rerank(
    scores: &mut [f32],
    plans: &[TurnPlan],
    intent: &storyforge::combat::ai::intent::TacticalIntent,
    snap: &BattleSnapshot,
    ctx: &storyforge::combat::ai::utility::ScoringCtx,
) {
    use storyforge::combat::ai::intent::TacticalIntent;
    use storyforge::combat::ai::planning::scorer::compute_plan_factors;

    match intent {
        TacticalIntent::FocusTarget { target } => {
            let Some(target_snap) = snap.unit(*target) else { return };
            let target_hp = target_snap.hp;
            for (plan, score) in plans.iter().zip(scores.iter_mut()) {
                if !score.is_finite() { continue; }
                let rank = classify_focus_target_bucket(
                    &plan.steps,
                    &plan.outcomes,
                    *target,
                    target_hp,
                );
                *score += rank as f32 * P2_RANK_OFFSET;
            }
        }
        TacticalIntent::ProtectSelf => {
            for (plan, score) in plans.iter().zip(scores.iter_mut()) {
                if !score.is_finite() { continue; }
                // Recompute factors on committed prefix for committed-prefix discipline.
                let prefix = plan_prefix_only(plan);
                let prefix_factors = compute_plan_factors(&prefix, intent, ctx);
                let self_survival = prefix_factors.as_array()[SELF_SURVIVAL_IDX];
                let rank = classify_protect_self_bucket(self_survival);
                *score += rank as f32 * P2_RANK_OFFSET;
            }
        }
        // All other intents: baseline scalar ordering unchanged.
        _ => {}
    }
}

// ── Phantom-tail helpers ─────────────────────────────────────────────────────

/// True if `plan` has at least one Move step AFTER the first Cast step.
fn has_post_cast_tail(plan: &PlanLog) -> bool {
    let Some(cast_idx) = plan.steps.iter().position(|s| matches!(s, PlanStep::Cast { .. })) else {
        return false;
    };
    plan.steps[cast_idx + 1..].iter().any(|s| matches!(s, PlanStep::Move { .. }))
}

/// Comparable key for the *committed prefix* of a plan.
///
/// Uses the production `TurnPlan::committed_prefix()` as the single source of
/// truth (see `src/combat/ai/planning/types.rs`). Two plans with the same key
/// execute the same action this tick, so differences in their phantom tails are
/// purely cosmetic.
///
/// NOTE: `CommittedPrefix` is lifetime-bound and has no `PartialEq`/`Eq`, so
/// we convert it to an owned, comparable key here — without touching production
/// types.
#[derive(PartialEq, Eq, Debug)]
enum CommittedActionKey {
    EndTurn,
    MoveOnly { dest: Hex },
    CastInPlace {
        ability: storyforge::core::AbilityId,
        target: Entity,
        target_pos: Hex,
    },
    MoveThenCast {
        dest: Hex,
        ability: storyforge::core::AbilityId,
        target: Entity,
        target_pos: Hex,
    },
}

impl CommittedActionKey {
    fn from_prefix(p: CommittedPrefix<'_>) -> Self {
        match p {
            CommittedPrefix::EndTurn => Self::EndTurn,
            CommittedPrefix::MoveOnly { path } => Self::MoveOnly {
                dest: path.last().copied().unwrap_or(Hex::ZERO),
            },
            CommittedPrefix::Cast { ability, target, target_pos } => Self::CastInPlace {
                ability: ability.clone(),
                target,
                target_pos,
            },
            CommittedPrefix::MoveThenCast { path, ability, target, target_pos } => {
                Self::MoveThenCast {
                    dest: path.last().copied().unwrap_or(Hex::ZERO),
                    ability: ability.clone(),
                    target,
                    target_pos,
                }
            }
        }
    }
}

/// Build a lightweight `TurnPlan` from a `PlanLog` and extract its
/// `CommittedActionKey`. The plan's `partial_score` and `sim_snapshots`
/// are irrelevant for `committed_prefix()`, so we use neutral defaults.
fn committed_action_key(plan: &PlanLog) -> CommittedActionKey {
    let tp = TurnPlan {
        steps: plan.steps.clone(),
        final_pos: Hex::new(plan.final_pos[0], plan.final_pos[1]),
        residual_ap: plan.residual_ap,
        residual_mp: plan.residual_mp,
        outcomes: plan.outcomes.clone(),
        partial_score: 0.0,
        sim_snapshots: Vec::new(),
    };
    CommittedActionKey::from_prefix(tp.committed_prefix())
}

/// Returns `Some(true)` when the committed decision is a `MoveOnly` with
/// displacement=0 (actor ends on its starting tile), `Some(false)` when it's a
/// `MoveOnly` with real displacement, and `None` for all other decision kinds.
fn is_wasted_move(committed: &serde_json::Value, actor_pos: Hex) -> Option<bool> {
    let kind = committed.get("kind")?.as_str()?;
    if kind != "MoveOnlyRetreat" && kind != "MoveCloser" {
        return None;
    }
    let path = committed.get("path")?.as_array()?;
    let last = path.last()?.as_array()?;
    if last.len() < 2 {
        return None;
    }
    let x = last[0].as_i64()? as i32;
    let y = last[1].as_i64()? as i32;
    Some(x == actor_pos.x && y == actor_pos.y)
}

/// Returns `true` when the committed action is defensive in the ProtectSelf
/// sense: retreat, self-cast, or cast targeting an ally.
/// Simple proxy used by `panic_leak_rate`: target_type Myself / SingleAlly
/// are approximated by comparing entity teams in the snapshot.
fn is_defensive_decision(
    committed: &serde_json::Value,
    actor_id: u64,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
) -> bool {
    let kind = committed.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "EndTurn" | "MoveOnlyRetreat" => true,
        "CastInPlace" | "MoveAndCast" => {
            let target_id = committed.get("target_id").and_then(|v| v.as_u64()).unwrap_or(0);
            // Self-cast → defensive.
            if target_id == actor_id {
                return true;
            }
            // Ally-cast → defensive.
            Entity::try_from_bits(target_id)
                .and_then(|te| snap.unit(te))
                .map(|t| t.team == active.team)
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Tile-path metrics for the chosen plan. Returns `(has_moves, has_repeated_tile, zero_net_move)`.
///
/// Visits every tile in every Move step (plus the actor's starting tile) and
/// checks for revisits. `zero_net_move` is true when the actor has ≥1 Move step
/// but ends exactly where it started.
fn plan_move_metrics(steps: &[PlanStep], final_pos: Hex, start: Hex) -> (bool, bool, bool) {
    let has_moves = steps.iter().any(|s| matches!(s, PlanStep::Move { .. }));
    if !has_moves {
        return (false, false, false);
    }
    let mut visited = std::collections::HashSet::new();
    visited.insert(start);
    let mut repeated = false;
    for step in steps {
        if let PlanStep::Move { path } = step {
            for tile in path {
                if !visited.insert(*tile) {
                    repeated = true;
                }
            }
        }
    }
    let zero_net = final_pos == start;
    (true, repeated, zero_net)
}

/// Post-cast retreat metrics. Returns `(has_post_cast_move, is_retreat)`.
///
/// A plan "has post-cast move" when a Cast step is followed by ≥1 Move step.
/// "retreat" = the post-cast moves revisit ≥1 pre-cast tile AND the final
/// displacement from `start` is no greater than it was at cast time, i.e.
/// the actor didn't make net progress after the cast.
fn post_cast_metrics(steps: &[PlanStep], final_pos: Hex, start: Hex) -> (bool, bool) {
    // Find first Cast step index.
    let cast_idx = match steps.iter().position(|s| matches!(s, PlanStep::Cast { .. })) {
        Some(i) => i,
        None => return (false, false),
    };
    // Any Move step after the Cast?
    let has_post = steps[cast_idx + 1..].iter().any(|s| matches!(s, PlanStep::Move { .. }));
    if !has_post {
        return (false, false);
    }
    // Collect tiles visited up to (and including) cast position.
    let mut pre_cast_tiles = std::collections::HashSet::new();
    pre_cast_tiles.insert(start);
    let mut cast_pos = start;
    for step in &steps[..cast_idx] {
        if let PlanStep::Move { path } = step {
            for tile in path {
                pre_cast_tiles.insert(*tile);
            }
            if let Some(last) = path.last() {
                cast_pos = *last;
            }
        }
    }
    // Check post-cast moves for revisits.
    let mut post_repeated = false;
    for step in &steps[cast_idx + 1..] {
        if let PlanStep::Move { path } = step {
            for tile in path {
                if pre_cast_tiles.contains(tile) {
                    post_repeated = true;
                }
            }
        }
    }
    // "net ≤ 0": final distance from start ≤ cast distance from start.
    let cast_dist = cast_pos.unsigned_distance_to(start);
    let final_dist = final_pos.unsigned_distance_to(start);
    let net_regressed = final_dist <= cast_dist;
    let retreat = post_repeated && net_regressed;
    (true, retreat)
}

// ── Kill-line helpers ────────────────────────────────────────────────────────

/// α threshold for "real kill-line" via damage: damage ≥ target_hp × α is
/// considered meaningful kill pressure. See `docs/ai_rework.md §5.2`.
// KEEP IN SYNC with src/combat/ai/planning/killable_gate.rs::KILLABLE_ALPHA
const KILLABLE_ALPHA: f32 = 0.3;

// KEEP IN SYNC with src/combat/ai/planning/killable_gate.rs::apply_killable_gate
/// Does this plan's **committed prefix** actually kill `target`?
///
/// Committed prefix mirrors `commit_plan` rules (see `src/combat/ai/planning/picker.rs`):
/// - `[]`                    → no commit → false.
/// - `[Cast, ...]`           → 1-step solo cast prefix.
/// - `[Move, Cast, ...]`     → 2-step MoveAndCast bundle.
/// - `[Move, ...]` (no Cast) → 1-step move-only prefix, no cast → false.
///
/// For each step in the prefix that is `Cast { target: intent_target, .. }`,
/// check whether `outcomes[i].killed` contains `intent_target`. The AoO during
/// a Move step never kills intent target directly (AoO hits the mover), so we
/// only look at Cast steps.
///
/// Tail steps beyond the committed prefix are **phantom** — their outcomes
/// represent what the simulator projected, but `commit_plan` will never
/// actually execute them. Counting tail kills would reward phantom tails,
/// the same anti-pattern as pre-step-1c intent_sum accumulation.
fn plan_committed_prefix_kills_target(plan: &PlanLog, target: Entity) -> bool {
    let prefix_len = match plan.steps.first() {
        None => 0,
        Some(PlanStep::Cast { .. }) => 1,
        Some(PlanStep::Move { .. }) => {
            if matches!(plan.steps.get(1), Some(PlanStep::Cast { .. })) { 2 } else { 1 }
        }
    };
    for i in 0..prefix_len.min(plan.steps.len()) {
        if let PlanStep::Cast { target: t, .. } = &plan.steps[i] {
            if *t == target {
                if let Some(o) = plan.outcomes.get(i) {
                    if o.killed.contains(&target) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// KEEP IN SYNC with src/combat/ai/planning/killable_gate.rs::apply_killable_gate
/// True if at least one plan in the pool can finish or meaningfully damage
/// `target`. Intent-coherent definition: plan must be `offensive_vs_target`
/// AND either kill target in the committed prefix OR deal damage ≥ target_hp·α.
///
/// Matches production `apply_killable_gate` strength detection so the
/// denominator of kill-line metrics equals the gate's own firing condition.
fn has_real_kill_line(plans: &[PlanLog], target: Entity, target_hp: i32) -> bool {
    let hp_f = target_hp.max(0) as f32;
    plans.iter().any(|p| {
        if !plan_is_offensive_vs(p, target) {
            return false;
        }
        plan_committed_prefix_kills_target(p, target)
            || p.raw_factors.get(DAMAGE_IDX).copied().unwrap_or(0.0) >= hp_f * KILLABLE_ALPHA
    })
}

/// Is `plan` "offensive vs target" — i.e., casts at least one Cast step
/// directly at `target`. AoE casts aimed at another tile are NOT counted;
/// only plans that explicitly target the intent target qualify.
fn plan_is_offensive_vs(plan: &PlanLog, target: Entity) -> bool {
    plan.steps
        .iter()
        .any(|s| matches!(s, PlanStep::Cast { target: t, .. } if *t == target))
}

/// Does the plan have ≥1 Cast step at all (regardless of target)?
/// Used to distinguish "non-offensive / no cast" vs "offensive but wrong target".
fn plan_has_any_cast(plan: &PlanLog) -> bool {
    plan.steps.iter().any(|s| matches!(s, PlanStep::Cast { .. }))
}

/// Classify a score vector as a "plateau" for the plateau_tie_rate metric.
///
/// Algorithm: look at top-K = min(5, len) scores (assumed sorted descending).
/// If ≥ `min_count` scores satisfy `top - score < window`, it's a plateau.
/// Applied identically to baseline and prototype score vectors.
fn is_plateau(scores: &[f32], window: f32, min_count: usize) -> bool {
    if scores.is_empty() {
        return false;
    }
    let k = scores.len().min(5);
    let top = scores[0];
    let within = scores[..k].iter().filter(|&&s| top - s < window).count();
    within >= min_count
}

/// Infer `(campaign_dir, scenario_dir)` from a log filename by scanning known
/// campaign/scenario directories under `assets/data/campaigns/`.
///
/// Log filenames follow the pattern `<timestamp>_<campaign>_<scenario>_<encounter>.jsonl`
/// (all three IDs sanitized with underscores). We iterate over actual filesystem
/// dirs to find an unambiguous match without fragile string splitting.
fn infer_content_dirs(log_path: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let stem = log_path
        .file_name()?
        .to_str()?
        .trim_end_matches(".jsonl");
    // Strip timestamp prefix `YYYYMMDDTHHMMSS_`.
    let rest = {
        let mut parts = stem.splitn(2, '_');
        let _ts = parts.next()?;
        parts.next()?
    };

    let campaigns_base = std::path::Path::new("assets/data/campaigns");
    let Ok(entries) = std::fs::read_dir(campaigns_base) else {
        return None;
    };
    let mut campaign_ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    campaign_ids.sort_by_key(|s| Reverse(s.len())); // longest first → greedy match

    for campaign_id in &campaign_ids {
        let prefix = format!("{campaign_id}_");
        let Some(after_campaign) = rest.strip_prefix(prefix.as_str()) else {
            continue;
        };

        let campaign_dir = campaigns_base.join(campaign_id);
        let Ok(scen_entries) = std::fs::read_dir(&campaign_dir) else {
            continue;
        };
        let mut scenario_ids: Vec<String> = scen_entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        scenario_ids.sort_by_key(|s| Reverse(s.len()));

        for scenario_id in &scenario_ids {
            let scen_prefix = format!("{scenario_id}_");
            if after_campaign.starts_with(scen_prefix.as_str())
                || after_campaign == scenario_id.as_str()
            {
                let scenario_dir = campaign_dir.join(scenario_id);
                return Some((campaign_dir, scenario_dir));
            }
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut verbose = false;
    let mut simulate_ab = false;
    let mut metrics_summary = false;
    let mut phase7_prototype = false;
    let mut phase7_p2 = false;
    let mut campaign_override: Option<PathBuf> = None;
    let mut scenario_override: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut iter = args[1..].iter();
    while let Some(a) = iter.next() {
        if a == "--verbose" || a == "-v" {
            verbose = true;
        } else if a == "--simulate-ab" {
            simulate_ab = true;
        } else if a == "--metrics-summary" {
            metrics_summary = true;
        } else if a == "--phase7-prototype" {
            phase7_prototype = true;
        } else if a == "--phase7-p2" {
            phase7_p2 = true;
        } else if a == "--campaign" {
            campaign_override = iter.next().map(PathBuf::from);
        } else if a == "--scenario" {
            scenario_override = iter.next().map(PathBuf::from);
        } else if !a.starts_with('-') {
            paths.push(PathBuf::from(a));
        }
    }
    if paths.is_empty() {
        eprintln!(
            "usage: replay_ai_log <log.jsonl> [<log2.jsonl> ...] \
             [--verbose] [--simulate-ab] [--metrics-summary] [--phase7-prototype] \
             [--phase7-p2] [--campaign <dir>] [--scenario <dir>]"
        );
        std::process::exit(2);
    }

    // Resolve content dirs: explicit flags > filename inference > global fallback.
    let global = std::path::Path::new("assets/data");
    let (campaign_dir, scenario_dir) = if let (Some(c), Some(s)) = (&campaign_override, &scenario_override) {
        (c.clone(), s.clone())
    } else if let Some((c, s)) = paths.first().and_then(|p| infer_content_dirs(p)) {
        (c, s)
    } else {
        eprintln!(
            "warning: could not infer campaign/scenario from filename; \
             loading global content only (assets/data). \
             Pass --campaign <dir> --scenario <dir> to override."
        );
        (global.to_path_buf(), global.to_path_buf())
    };
    let content = ContentView::load_layered(&campaign_dir, &scenario_dir);
    let inf_cfg = InfluenceConfig::default();
    let difficulty = DifficultyProfile::normal();
    let mut rng = DiceRng::with_seed(0);
    // Separate rngs for experimental pipelines — baseline rng must not be advanced
    // by any experimental picks (invariance: without flags, output is identical).
    let mut rng_proto = DiceRng::with_seed(1);
    let mut rng_p2 = DiceRng::with_seed(2);
    let mut metrics = Metrics {
        proto: if phase7_prototype { Some(Box::new(PrototypeMetrics::default())) } else { None },
        p2: if phase7_p2 { Some(Box::new(P2Metrics::default())) } else { None },
        ..Metrics::default()
    };

    for path in &paths {
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        let reader = BufReader::new(file);

        let mut total = 0usize;
        let mut changed = 0usize;

        println!("\n=== Replay: {} ===\n", path.display());

        let mut divergence_total = 0usize;
        let mut divergence_used_cont = 0usize;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("read error: {e}");
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }

            // Route divergence events separately — they have a different schema
            // from regular pick_action entries and would fail to parse as LogEntry.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                if val.get("event_type").and_then(|v| v.as_str()) == Some("plan_divergence") {
                    divergence_total += 1;
                    if val.get("used_continuation").and_then(|v| v.as_bool()).unwrap_or(false) {
                        divergence_used_cont += 1;
                    }
                    if verbose {
                        let actor = val.get("actor_id").and_then(|v| v.as_u64()).unwrap_or(0);
                        let used = val.get("used_continuation").and_then(|v| v.as_bool()).unwrap_or(false);
                        let reason = val.get("replan_reason").and_then(|v| v.as_str()).unwrap_or("-");
                        let score_delta = val.get("score_delta").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let ability_changed = val.get("ability_changed").and_then(|v| v.as_bool()).unwrap_or(false);
                        let target_changed = val.get("target_changed").and_then(|v| v.as_bool()).unwrap_or(false);
                        let intent_changed = val.get("intent_changed").and_then(|v| v.as_bool()).unwrap_or(false);
                        println!(
                            "  [divergence] actor={actor} used_cont={used} reason={reason} \
                             score_delta={score_delta:+.2} intent_chg={intent_changed} \
                             ability_chg={ability_changed} target_chg={target_changed}"
                        );
                    }
                    continue;
                }
            }

            let entry: LogEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("parse error: {e}");
                    continue;
                }
            };
            // Older logs lack newer per-snapshot fields; `#[serde(default)]` on
            // each addition fills them with neutral defaults so replay
            // continues, just blind to those signals:
            // - v1 → v2: `reactions_left` (1) + `aoo_expected_damage` (None)
            // - v2 → v3: `caster_ctx` (zeros) + `crit_fail_effect` (Miss)
            // - v3 → v4: `damage_horizon` (empty) — CC/heal fall back to threat
            // - v4 → v5: `intent.reason` — structured reason payload; replay does
            //   not read it (classification still uses `selection_kind`).
            // - v5 → v6: per-plan ADAPTATION dump. Replay surfaces it in verbose
            //   output; older logs default to `evaluation_mode=default` and
            //   `adaptation_reason=None` so the renderer stays silent.
            // - v6 → v7: per-plan TRADE block (delta/killed/lost/self_lost/
            //   self_lethal/score). Replay surfaces the breakdown under
            //   `--verbose`; older logs drop to a default-filled block.
            if !(1..=15).contains(&entry.schema_version) {
                eprintln!("unsupported schema_version {}, skipping", entry.schema_version);
                continue;
            }
            if entry.schema_version < 14 {
                eprintln!(
                    "warning: schema_version {} < 14 — raw_factors indices differ from \
                     current layout (position/risk/focus removed in v14); replay scores \
                     may be inaccurate",
                    entry.schema_version
                );
            }
            total += 1;

            // Rebuild context.
            let actor = match Entity::try_from_bits(entry.actor_id) {
                Some(e) => e,
                None => {
                    eprintln!("invalid actor_id {}, skipping", entry.actor_id);
                    continue;
                }
            };
            let Some(active) = entry.snapshot.unit(actor).cloned() else {
                eprintln!("actor not found in snapshot, skipping");
                continue;
            };

            // ── Regression metrics (on raw logged data, before any re-scoring) ──
            if let Some(wasted) = is_wasted_move(&entry.committed_decision, active.pos) {
                metrics.move_only_total += 1;
                if wasted {
                    metrics.move_only_wasted += 1;
                }
            }
            // panic_leak_rate: ProtectSelf intent + Default evaluation_mode
            // (mask was active). LastStand entries are excluded — their
            // non-defensive commit is design, not a leak.
            if matches!(
                entry.intent.intent,
                storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
            ) {
                if let Some(chosen_plan) = entry.plans.iter().find(|p| p.chosen) {
                    if chosen_plan.evaluation_mode == LoggedEvaluationMode::Default {
                        metrics.panic_total += 1;
                        if !is_defensive_decision(
                            &entry.committed_decision,
                            entry.actor_id,
                            &active,
                            &entry.snapshot,
                        ) {
                            metrics.panic_leaked += 1;
                        }
                    }
                }
            }
            if entry.intent.selection_kind == "killable" {
                metrics.killable_total += 1;
                if entry
                    .plans
                    .iter()
                    .find(|p| p.chosen)
                    .is_some_and(|p| p.raw_factors.get(KILL_NOW_IDX).copied().unwrap_or(0.0) > 0.0)
                {
                    metrics.killable_closed += 1;
                }

                // Kill-line metrics: restrict to entries where the pool actually
                // contained a plan capable of finishing the target this turn.
                if let storyforge::combat::ai::intent::TacticalIntent::FocusTarget { target } =
                    entry.intent.intent
                {
                    if let Some(target_snap) = entry.snapshot.unit(target) {
                        if has_real_kill_line(&entry.plans, target, target_snap.hp) {
                            metrics.killable_with_kill_line_total += 1;
                            if let Some(chosen) = entry.plans.iter().find(|p| p.chosen) {
                                let offensive_vs_target = plan_is_offensive_vs(chosen, target);
                                if !offensive_vs_target {
                                    metrics.killable_non_offensive += 1;
                                    // Has casts but aimed at a different unit → wrong target.
                                    if plan_has_any_cast(chosen) {
                                        metrics.killable_wrong_target += 1;
                                    }
                                }
                                if plan_committed_prefix_kills_target(chosen, target) {
                                    metrics.killable_kill_converted += 1;
                                }
                            }
                        }
                    }
                }
            }
            // tempo metrics: repeated_tile_rate, zero_net_move_rate, post_cast_retreat_rate
            if let Some(chosen) = entry.plans.iter().find(|p| p.chosen) {
                let final_pos = Hex::new(chosen.final_pos[0], chosen.final_pos[1]);
                let (has_moves, repeated, zero_net) =
                    plan_move_metrics(&chosen.steps, final_pos, active.pos);
                if has_moves {
                    metrics.plans_with_moves += 1;
                    if repeated {
                        metrics.repeated_tile_plans += 1;
                    }
                    if zero_net {
                        metrics.zero_net_move_plans += 1;
                    }
                }
                let (has_post_cast, retreat) =
                    post_cast_metrics(&chosen.steps, final_pos, active.pos);
                if has_post_cast {
                    metrics.plans_with_post_cast_move += 1;
                    if retreat {
                        metrics.post_cast_retreat_plans += 1;
                    }
                }
            }
            // phantom-tail metrics
            if let Some(chosen) = entry.plans.iter().find(|p| p.chosen) {
                let has_cast = chosen.steps.iter().any(|s| matches!(s, PlanStep::Cast { .. }));
                if has_cast {
                    metrics.chosen_with_cast_total += 1;
                    if has_post_cast_tail(chosen) {
                        metrics.phantom_tail_chosen += 1;
                        let chosen_key = committed_action_key(chosen);
                        // Best tailless alt: not chosen, no post-cast tail, has a score.
                        let best_tailless_alt = entry
                            .plans
                            .iter()
                            .filter(|p| !p.chosen && !has_post_cast_tail(p))
                            .filter(|p| p.score.is_some())
                            .max_by(|a, b| {
                                a.score
                                    .unwrap_or(f32::NEG_INFINITY)
                                    .partial_cmp(&b.score.unwrap_or(f32::NEG_INFINITY))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                        if let Some(alt) = best_tailless_alt {
                            if committed_action_key(alt) != chosen_key {
                                metrics.phantom_tail_flips_committed += 1;
                            }
                        }
                    }
                }
            }

            let maps = build_influence_maps(&entry.snapshot, actor, active.team, &inf_cfg);

            let world = AiWorld {
                content: &content,
                difficulty: &difficulty,
                crit_fail_chance: 0.0,
            };

            // Reconstruct TurnPlan[] from log + raw factor matrix.
            let plans: Vec<TurnPlan> = entry
                .plans
                .iter()
                .map(|p| TurnPlan {
                    steps: p.steps.clone(),
                    final_pos: Hex::new(p.final_pos[0], p.final_pos[1]),
                    residual_ap: p.residual_ap,
                    residual_mp: p.residual_mp,
                    outcomes: p.outcomes.clone(),
                    partial_score: 0.0,
                    sim_snapshots: Vec::new(),
                })
                .collect();
            // Convert logged raw factor arrays back to structured PlanFactors so
            // the shared scoring pipeline can ingest them directly. Pad with
            // zeros for logs written before v10 (9-element arrays).
            let raw_factors: Vec<PlanFactors> = entry
                .plans
                .iter()
                .map(|p| {
                    use storyforge::combat::ai::factors::NUM_FACTORS;
                    let mut arr = [0.0f32; NUM_FACTORS];
                    for (i, &v) in p.raw_factors.iter().take(NUM_FACTORS).enumerate() {
                        arr[i] = v;
                    }
                    PlanFactors::from_array(arr)
                })
                .collect();

            // Reservations are empty during replay — each entry is scored in
            // isolation, without the round's coordination state from live play.
            let reservations = Reservations::default();
            let scoring_ctx = ScoringCtx {
                world: &world,
                maps: &maps,
                reservations: &reservations,
                snap: &entry.snapshot,
                active: &active,
            };

            // Reuse the production `finalize_scores` so summon_bonus, trade_bonus,
            // hash-based noise, and batch normalisation all match the live
            // pipeline bit-for-bit. Invariant: replay's pre-sanity score equals
            // what production produced given the same raw factors.
            let mut scores = finalize_scores(&plans, &raw_factors, &scoring_ctx);
            let pre_scores = scores.clone();

            let _ = sanity_adjust_plans(&mut scores, &plans, &scoring_ctx);

            // ProtectSelf mask — two paths:
            //   1. The logged intent is already ProtectSelf (fix A deployed at
            //      log time, or it was a hard panic override). Apply B directly.
            //   2. `--simulate-ab` + logged intent was a viability fallback AND
            //      midpanic conditions now hold → simulate the switch. Raw
            //      factors stay as-logged (they were computed under the old
            //      intent), so this under-counts ProtectSelf's intent-factor
            //      boost on defensive plans. Enough for directional verification.
            //
            // Reconstruct evaluation_mode from the logged plans (schema v6+).
            // Pre-v6 logs default every plan to `evaluation_mode=Default` via
            // #[serde(default)], so the mask still behaves as it did before;
            // the warning below flags those logs so callers know the result may
            // differ from the original live run.
            if entry.schema_version < 6 {
                eprintln!(
                    "warning: schema_version {} < 6 — evaluation_mode not available; \
                     replay applies mask as Default for all plans (results may differ \
                     from original live run)",
                    entry.schema_version
                );
            }
            let modes: Vec<storyforge::combat::ai::planning::EvaluationMode> = entry
                .plans
                .iter()
                .map(|p| match p.evaluation_mode {
                    LoggedEvaluationMode::Default => {
                        storyforge::combat::ai::planning::EvaluationMode::Default
                    }
                    LoggedEvaluationMode::LastStand => {
                        storyforge::combat::ai::planning::EvaluationMode::LastStand
                    }
                })
                .collect();
            let mut applied_mask = false;
            let mut simulated_switch = false;
            if matches!(
                entry.intent.intent,
                storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
            ) {
                apply_protect_self_mask(&mut scores, &raw_factors, &modes);
                applied_mask = true;
            } else if simulate_ab && entry.intent.selection_kind == "viability_fallback" {
                let hp_pct = active.hp_pct();
                let actor_danger = maps.danger.get(active.pos);
                let midpanic_hp = difficulty.midpanic_hp_threshold();
                let panic_danger = difficulty.awareness_danger_threshold();
                if hp_pct < midpanic_hp && actor_danger > panic_danger {
                    apply_protect_self_mask(&mut scores, &raw_factors, &modes);
                    applied_mask = true;
                    simulated_switch = true;
                }
            }
            let _ = applied_mask;

            // Compare rankings. Pre-sanity uses argmax as a simple reference
            // point ("what a perfect-information picker would take").
            // Post-sanity goes through the production `pick_best_plan` so
            // replay's final pick reflects mercy reordering and top-K
            // tie-breaking exactly as the live pipeline would. Replay's rng
            // is seeded independently of production's live state, so tie-breaks
            // on normal/easy difficulty (where top_k > 1 and multiple plans
            // fall within `window`) may diverge — that's RNG drift, not a
            // logic mismatch.
            let top_pre = argmax(&pre_scores);
            let (top_post, _pick_mech) = pick_best_plan(&scores, &raw_factors, &world, &mut rng);

            // ── Phase 7 prototype dual pipeline ──────────────────────────────────
            // Only runs when --phase7-prototype is set. Uses a separate rng so
            // the baseline rng is never advanced by prototype picks (invariance).
            if let Some(ref mut proto) = metrics.proto {
                proto.total_entries += 1;

                // Prototype scores: score_plans_prototype → sanity → mask → pick.
                let mut proto_scores = score_plans_prototype(
                    &plans,
                    &entry.intent.intent,
                    &scoring_ctx,
                );
                let _ = sanity_adjust_plans(&mut proto_scores, &plans, &scoring_ctx);
                if matches!(
                    entry.intent.intent,
                    storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
                ) {
                    apply_protect_self_mask(&mut proto_scores, &raw_factors, &modes);
                } else if simulate_ab && entry.intent.selection_kind == "viability_fallback" {
                    let hp_pct = active.hp_pct();
                    let actor_danger = maps.danger.get(active.pos);
                    let midpanic_hp = difficulty.midpanic_hp_threshold();
                    let panic_danger = difficulty.awareness_danger_threshold();
                    if hp_pct < midpanic_hp && actor_danger > panic_danger {
                        apply_protect_self_mask(&mut proto_scores, &raw_factors, &modes);
                    }
                }
                let (top_post_proto, _) =
                    pick_best_plan(&proto_scores, &raw_factors, &world, &mut rng_proto);

                // ranking_change_rate: committed action key differs.
                let baseline_key = committed_action_key(&entry.plans[top_post]);
                let proto_key = committed_action_key(&entry.plans[top_post_proto]);
                if baseline_key != proto_key {
                    proto.ranking_changed += 1;
                }

                // plateau_tie_rate — applied to both vectors (sorted descending).
                let mut base_sorted = scores.clone();
                base_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                if is_plateau(&base_sorted, 0.05, 3) {
                    proto.plateau_ties_baseline += 1;
                }
                let mut proto_sorted = proto_scores.clone();
                proto_sorted
                    .sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                if is_plateau(&proto_sorted, 0.05, 3) {
                    proto.plateau_ties_prototype += 1;
                }

                // ── Mirrored acceptance metrics on prototype pick ─────────────
                // Use the chosen plan from prototype pick (top_post_proto index).
                let proto_chosen_log = &entry.plans[top_post_proto];
                let proto_final_pos =
                    Hex::new(proto_chosen_log.final_pos[0], proto_chosen_log.final_pos[1]);

                // wasted_mp
                if let Some(wasted) = is_wasted_move(&entry.committed_decision, active.pos) {
                    // denominator is the same (entry type doesn't change); only
                    // check if the prototype pick changes the waste outcome for
                    // MoveOnly commits — but the committed_decision key is logged
                    // from production, not from prototype. For move metrics we
                    // use the prototype winner's plan shape directly.
                    let _ = wasted; // production-committed_decision stays as-is
                }
                // For move-shape metrics use the prototype winner's steps.
                let (has_moves_p, repeated_p, zero_net_p) = plan_move_metrics(
                    &proto_chosen_log.steps,
                    proto_final_pos,
                    active.pos,
                );
                if has_moves_p {
                    proto.plans_with_moves += 1;
                    if repeated_p { proto.repeated_tile_plans += 1; }
                    if zero_net_p { proto.zero_net_move_plans += 1; }
                }
                let (has_post_p, retreat_p) =
                    post_cast_metrics(&proto_chosen_log.steps, proto_final_pos, active.pos);
                if has_post_p {
                    proto.plans_with_post_cast_move += 1;
                    if retreat_p { proto.post_cast_retreat_plans += 1; }
                }
                // move_only_total/wasted: use the MoveOnly commit shape of proto winner.
                {
                    use storyforge::combat::ai::planning::CommittedPrefix;
                    let tp_proto = TurnPlan {
                        steps: proto_chosen_log.steps.clone(),
                        final_pos: proto_final_pos,
                        residual_ap: proto_chosen_log.residual_ap,
                        residual_mp: proto_chosen_log.residual_mp,
                        outcomes: proto_chosen_log.outcomes.clone(),
                        partial_score: 0.0,
                        sim_snapshots: Vec::new(),
                    };
                    if let CommittedPrefix::MoveOnly { path } = tp_proto.committed_prefix() {
                        proto.move_only_total += 1;
                        let dest = path.last().copied().unwrap_or(active.pos);
                        if dest == active.pos {
                            proto.move_only_wasted += 1;
                        }
                    }
                }
                // panic_leak (ProtectSelf + Default mode)
                if matches!(
                    entry.intent.intent,
                    storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
                ) && proto_chosen_log.evaluation_mode == LoggedEvaluationMode::Default
                {
                    proto.panic_total += 1;
                    if !is_defensive_decision(
                        &entry.committed_decision,
                        entry.actor_id,
                        &active,
                        &entry.snapshot,
                    ) {
                        proto.panic_leaked += 1;
                    }
                }
                // killable kill-line metrics
                if entry.intent.selection_kind == "killable" {
                    if let storyforge::combat::ai::intent::TacticalIntent::FocusTarget {
                        target,
                    } = entry.intent.intent
                    {
                        if let Some(target_snap) = entry.snapshot.unit(target) {
                            if has_real_kill_line(&entry.plans, target, target_snap.hp) {
                                proto.killable_with_kill_line_total += 1;
                                let offensive = plan_is_offensive_vs(proto_chosen_log, target);
                                if !offensive {
                                    proto.killable_non_offensive += 1;
                                    if plan_has_any_cast(proto_chosen_log) {
                                        proto.killable_wrong_target += 1;
                                    }
                                }
                                if plan_committed_prefix_kills_target(proto_chosen_log, target) {
                                    proto.killable_kill_converted += 1;
                                }
                            }
                        }
                    }
                }
                // phantom-tail metrics on proto winner
                let has_cast_p = proto_chosen_log
                    .steps
                    .iter()
                    .any(|s| matches!(s, PlanStep::Cast { .. }));
                if has_cast_p {
                    proto.chosen_with_cast_total += 1;
                    if has_post_cast_tail(proto_chosen_log) {
                        proto.phantom_tail_chosen += 1;
                        let proto_chosen_key = committed_action_key(proto_chosen_log);
                        // Best tailless alt by prototype score.
                        let best_tailless_proto = entry
                            .plans
                            .iter()
                            .enumerate()
                            .filter(|(i, p)| *i != top_post_proto && !has_post_cast_tail(p))
                            .filter(|(i, _)| proto_scores.get(*i).is_some())
                            .max_by(|(i, _), (j, _)| {
                                proto_scores[*i]
                                    .partial_cmp(&proto_scores[*j])
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(_, p)| p);
                        if let Some(alt) = best_tailless_proto {
                            if committed_action_key(alt) != proto_chosen_key {
                                proto.phantom_tail_flips_committed += 1;
                            }
                        }
                    }
                }
            }

            // ── Phase 7 P2 bucketed ranking pipeline ─────────────────────────────
            // Only runs when --phase7-p2 is set. Own rng_p2 for seed hygiene.
            if let Some(ref mut p2) = metrics.p2 {
                p2.total_entries += 1;

                // Scalar scores: same baseline pipeline as production.
                let mut p2_scores = finalize_scores(&plans, &raw_factors, &scoring_ctx);
                let _ = sanity_adjust_plans(&mut p2_scores, &plans, &scoring_ctx);
                if matches!(
                    entry.intent.intent,
                    storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
                ) {
                    apply_protect_self_mask(&mut p2_scores, &raw_factors, &modes);
                } else if simulate_ab && entry.intent.selection_kind == "viability_fallback" {
                    let hp_pct = active.hp_pct();
                    let actor_danger = maps.danger.get(active.pos);
                    let midpanic_hp = difficulty.midpanic_hp_threshold();
                    let panic_danger = difficulty.awareness_danger_threshold();
                    if hp_pct < midpanic_hp && actor_danger > panic_danger {
                        apply_protect_self_mask(&mut p2_scores, &raw_factors, &modes);
                    }
                }

                // Bucket rerank: overrides scalar ordering for FocusTarget and ProtectSelf.
                apply_p2_bucket_rerank(
                    &mut p2_scores,
                    &plans,
                    &entry.intent.intent,
                    &entry.snapshot,
                    &scoring_ctx,
                );

                let (top_post_p2, _) =
                    pick_best_plan(&p2_scores, &raw_factors, &world, &mut rng_p2);

                // ranking_change_rate vs baseline.
                let baseline_key = committed_action_key(&entry.plans[top_post]);
                let p2_key = committed_action_key(&entry.plans[top_post_p2]);
                if baseline_key != p2_key {
                    p2.ranking_changed += 1;
                }

                // plateau_tie_rate on scalar scores (before bucket shift) and p2 scores.
                let mut base_sorted = scores.clone();
                base_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                if is_plateau(&base_sorted, 0.05, 3) {
                    p2.plateau_ties_baseline += 1;
                }
                let mut p2_sorted = p2_scores.clone();
                p2_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                if is_plateau(&p2_sorted, 0.05, 3) {
                    p2.plateau_ties_p2 += 1;
                }

                // ── Mirrored acceptance metrics on P2 pick ────────────────────────
                let p2_chosen_log = &entry.plans[top_post_p2];
                let p2_final_pos =
                    Hex::new(p2_chosen_log.final_pos[0], p2_chosen_log.final_pos[1]);

                let (has_moves_p2, repeated_p2, zero_net_p2) = plan_move_metrics(
                    &p2_chosen_log.steps,
                    p2_final_pos,
                    active.pos,
                );
                if has_moves_p2 {
                    p2.plans_with_moves += 1;
                    if repeated_p2 { p2.repeated_tile_plans += 1; }
                    if zero_net_p2 { p2.zero_net_move_plans += 1; }
                }
                let (has_post_p2, retreat_p2) =
                    post_cast_metrics(&p2_chosen_log.steps, p2_final_pos, active.pos);
                if has_post_p2 {
                    p2.plans_with_post_cast_move += 1;
                    if retreat_p2 { p2.post_cast_retreat_plans += 1; }
                }
                {
                    let tp_p2 = TurnPlan {
                        steps: p2_chosen_log.steps.clone(),
                        final_pos: p2_final_pos,
                        residual_ap: p2_chosen_log.residual_ap,
                        residual_mp: p2_chosen_log.residual_mp,
                        outcomes: p2_chosen_log.outcomes.clone(),
                        partial_score: 0.0,
                        sim_snapshots: Vec::new(),
                    };
                    if let CommittedPrefix::MoveOnly { path } = tp_p2.committed_prefix() {
                        p2.move_only_total += 1;
                        let dest = path.last().copied().unwrap_or(active.pos);
                        if dest == active.pos {
                            p2.move_only_wasted += 1;
                        }
                    }
                }
                // panic_leak
                if matches!(
                    entry.intent.intent,
                    storyforge::combat::ai::intent::TacticalIntent::ProtectSelf
                ) && p2_chosen_log.evaluation_mode == LoggedEvaluationMode::Default
                {
                    p2.panic_total += 1;
                    if !is_defensive_decision(
                        &entry.committed_decision,
                        entry.actor_id,
                        &active,
                        &entry.snapshot,
                    ) {
                        p2.panic_leaked += 1;
                    }
                }
                // killable kill-line metrics
                if entry.intent.selection_kind == "killable" {
                    if let storyforge::combat::ai::intent::TacticalIntent::FocusTarget {
                        target,
                    } = entry.intent.intent
                    {
                        if let Some(target_snap) = entry.snapshot.unit(target) {
                            if has_real_kill_line(&entry.plans, target, target_snap.hp) {
                                p2.killable_with_kill_line_total += 1;
                                let offensive = plan_is_offensive_vs(p2_chosen_log, target);
                                if !offensive {
                                    p2.killable_non_offensive += 1;
                                    if plan_has_any_cast(p2_chosen_log) {
                                        p2.killable_wrong_target += 1;
                                    }
                                }
                                if plan_committed_prefix_kills_target(p2_chosen_log, target) {
                                    p2.killable_kill_converted += 1;
                                }
                            }
                        }
                    }
                }
                // phantom-tail metrics
                let has_cast_p2 = p2_chosen_log
                    .steps
                    .iter()
                    .any(|s| matches!(s, PlanStep::Cast { .. }));
                if has_cast_p2 {
                    p2.chosen_with_cast_total += 1;
                    if has_post_cast_tail(p2_chosen_log) {
                        p2.phantom_tail_chosen += 1;
                        let p2_chosen_key = committed_action_key(p2_chosen_log);
                        let best_tailless_p2 = entry
                            .plans
                            .iter()
                            .enumerate()
                            .filter(|(i, p)| *i != top_post_p2 && !has_post_cast_tail(p))
                            .filter(|(i, _)| p2_scores.get(*i).is_some())
                            .max_by(|(i, _), (j, _)| {
                                p2_scores[*i]
                                    .partial_cmp(&p2_scores[*j])
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(_, p)| p);
                        if let Some(alt) = best_tailless_p2 {
                            if committed_action_key(alt) != p2_chosen_key {
                                p2.phantom_tail_flips_committed += 1;
                            }
                        }
                    }
                }
            }

            let pre_was_chosen = entry.plans.iter().find(|p| p.chosen).map(|p| p.rank).unwrap_or(0);
            let hp = format!("{}/{}", active.hp, active.max_hp);

            let header = format!(
                "r{} {}: HP {} AP {}/{} MP {}, intent={} [{}], plans_eval={}, decision={}ms",
                entry.round,
                entry.actor_name,
                hp,
                entry.actor_ap,
                entry.actor_max_ap,
                entry.actor_mp,
                intent_kind(&entry.intent.intent),
                entry.intent.selection_kind,
                entry.plans_evaluated,
                entry.decision_time_ms,
            );

            if top_pre != top_post {
                changed += 1;
                let sim_tag = if simulated_switch { " (simulated A+B midpanic)" } else { "" };
                println!("🔁 {header}{sim_tag}", header = header);
                println!("   logged_chose=#{pre_was_chosen}, pre_sanity_top=#{} ({:+.2}), post_sanity_top=#{} ({:+.2})",
                    top_pre + 1, pre_scores[top_pre], top_post + 1, scores[top_post]);
                print_plan("   pre ", &entry.plans[top_pre], pre_scores[top_pre], scores[top_pre]);
                print_plan("   post", &entry.plans[top_post], pre_scores[top_post], scores[top_post]);
                let _ = entry.committed_decision;
            } else if verbose {
                println!("=  {header}");
                println!("   logged_chose=#{pre_was_chosen}, top=#{} ({:+.2} → {:+.2})",
                    top_pre + 1, pre_scores[top_pre], scores[top_pre]);
            }

            if verbose {
                println!("   — full ranking (pre → post) —");
                let mut indexed: Vec<(usize, f32, f32)> = (0..scores.len())
                    .map(|i| (i, pre_scores[i], scores[i]))
                    .collect();
                indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                for (i, pre, post) in indexed {
                    // Surface per-plan ADAPTATION metadata (v6+). Older logs
                    // default to Default/None → tag stays empty.
                    let adapt_tag = if entry.plans[i].evaluation_mode.is_adapted() {
                        match &entry.plans[i].adaptation_reason {
                            Some(r) => format!("  [adapted: last_stand ← {}]", r.code()),
                            None => "  [adapted: last_stand]".to_string(),
                        }
                    } else {
                        String::new()
                    };
                    // v7 trade block. Quiet when the plan didn't make a trade
                    // — no kills, no ally losses, no self-lethal exposure.
                    let trade = &entry.plans[i].trade;
                    let trade_tag = if trade.delta != 0.0
                        || trade.self_lethal
                        || trade.killed != 0.0
                        || trade.lost != 0.0
                    {
                        let self_tag = if trade.self_lethal { " SELF-LETHAL" } else { "" };
                        format!(
                            "  [trade: Δ={:+.1} (kill {:+.1} / lost {:+.1} / self {:+.1}) score={:+.2}{}]",
                            trade.delta, trade.killed, trade.lost, trade.self_lost,
                            trade.score, self_tag,
                        )
                    } else {
                        String::new()
                    };
                    println!(
                        "      #{}{}  pre={:+.2}  post={:+.2}  Δ={:+.2}  final=({},{})  {}{}{}",
                        entry.plans[i].rank,
                        if entry.plans[i].chosen { "★" } else { " " },
                        pre,
                        post,
                        post - pre,
                        entry.plans[i].final_pos[0],
                        entry.plans[i].final_pos[1],
                        plan_shape(&entry.plans[i]),
                        adapt_tag,
                        trade_tag,
                    );
                }
            }
        }

        println!("\n=== {} entries, {} ranking changes after sanity ===", total, changed);
        if divergence_total > 0 {
            println!(
                "=== {} divergence events: {} used continuation ({:.0}%), {} replanned ===",
                divergence_total,
                divergence_used_cont,
                divergence_used_cont as f64 / divergence_total as f64 * 100.0,
                divergence_total - divergence_used_cont,
            );
        }
    }

    if metrics_summary {
        metrics.print_summary();
    }
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    let mut best_score = f32::NEG_INFINITY;
    for (i, &s) in v.iter().enumerate() {
        if s.is_finite() && s > best_score {
            best_score = s;
            best = i;
        }
    }
    best
}

fn intent_kind(i: &storyforge::combat::ai::intent::TacticalIntent) -> &'static str {
    use storyforge::combat::ai::intent::TacticalIntent::*;
    match i {
        FocusTarget { .. } => "FocusTarget",
        ApplyCC { .. } => "ApplyCC",
        Reposition => "Reposition",
        ProtectSelf => "ProtectSelf",
        ProtectAlly { .. } => "ProtectAlly",
        SetupAOE => "SetupAOE",
        LastStand => "LastStand",
    }
}

fn plan_shape(p: &PlanLog) -> String {
    let mut out = Vec::new();
    for s in &p.steps {
        match s {
            PlanStep::Move { path } => {
                let last = path.last().copied().unwrap_or(Hex::ZERO);
                out.push(format!("Move→({},{})", last.x, last.y));
            }
            PlanStep::Cast { ability, target, .. } => {
                out.push(format!("Cast({}→{})", ability.0, target.to_bits()));
            }
        }
    }
    out.join(" · ")
}

fn print_plan(label: &str, p: &PlanLog, pre: f32, post: f32) {
    println!(
        "{label} #{} score {:+.2}→{:+.2}  {}  raw={:?}",
        p.rank,
        pre,
        post,
        plan_shape(p),
        p.raw_factors,
    );
    let _ = p.score; // logged score includes noise; we show recomputed.
                     // `None` = plan was pruned pre-scoring in the live run.
}

/// Silences dead_code lints on `AxisProfile::factor_weights` when only
/// referenced via deser chain.
#[allow(dead_code)]
fn _touch_axis(p: &AxisProfile) -> [f32; 10] {
    p.factor_weights()
}

#[cfg(test)]
mod tests {
    use super::*;
    use storyforge::combat::ai::planning::{PlanStep, StepOutcome};
    use storyforge::core::AbilityId;
    use storyforge::game::hex::Hex;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn cast_step(target: Entity) -> PlanStep {
        PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target,
            target_pos: Hex::ZERO,
        }
    }

    fn move_step() -> PlanStep {
        PlanStep::Move { path: vec![Hex::new(1, 0)] }
    }

    fn outcome_kills(target: Entity) -> StepOutcome {
        StepOutcome { killed: vec![target], ..StepOutcome::default() }
    }

    fn outcome_empty() -> StepOutcome {
        StepOutcome::default()
    }

    /// Build a minimal `PlanLog` for tests. Fields not relevant to the helper
    /// under test are set to harmless defaults.
    fn plan_log(steps: Vec<PlanStep>, outcomes: Vec<StepOutcome>) -> PlanLog {
        PlanLog {
            rank: 1,
            chosen: false,
            steps,
            outcomes,
            final_pos: [0, 0],
            residual_ap: 0,
            residual_mp: 0,
            raw_factors: vec![0.0; 10],
            score: None,
            base_score: None,
            evaluation_mode: LoggedEvaluationMode::Default,
            adaptation_reason: None,
            trade: LoggedTradeBlock::default(),
            sanity_breakdown: Vec::new(),
        }
    }

    // ── Test 1: solo Cast that kills target counts as conversion ─────────────

    #[test]
    fn committed_cast_kill_counts_as_conversion() {
        let tgt = ent(42);
        let plan = plan_log(
            vec![cast_step(tgt)],
            vec![outcome_kills(tgt)],
        );
        assert!(plan_committed_prefix_kills_target(&plan, tgt));
    }

    // ── Test 2: Move→Cast kill counts as conversion (Bug #1 regression guard) ─

    #[test]
    fn move_and_cast_kill_counts_as_conversion() {
        let tgt = ent(42);
        // Outcomes: step 0 (Move) → empty; step 1 (Cast) → kills target.
        let plan = plan_log(
            vec![move_step(), cast_step(tgt)],
            vec![outcome_empty(), outcome_kills(tgt)],
        );
        assert!(plan_committed_prefix_kills_target(&plan, tgt));
    }

    // ── Test 3: phantom tail Cast kill does NOT count ─────────────────────────

    #[test]
    fn tail_cast_kill_does_not_count() {
        let tgt = ent(42);
        // Shape: [Move, Move, Cast @ tgt] — Cast is at step index 2 (phantom tail).
        // Committed prefix = MoveOnly (step 0 only), so step 2 is never executed.
        let plan = plan_log(
            vec![move_step(), move_step(), cast_step(tgt)],
            vec![outcome_empty(), outcome_empty(), outcome_kills(tgt)],
        );
        assert!(!plan_committed_prefix_kills_target(&plan, tgt));
    }

    // ── Test 4: has_real_kill_line requires offensive_vs_target (Bug #2 guard) ─

    #[test]
    fn has_real_kill_line_requires_offensive_vs_target() {
        let intent_target = ent(1);
        let other_enemy = ent(2);

        // Plan A: Cast @ other_enemy with kn=1 — collateral kill, NOT vs intent target.
        let mut raw_a = vec![0.0f32; 10];
        raw_a[KILL_NOW_IDX] = 1.0;
        let plan_a = PlanLog {
            steps: vec![cast_step(other_enemy)],
            outcomes: vec![outcome_kills(other_enemy)],
            raw_factors: raw_a,
            ..plan_log(vec![], vec![])
        };

        // Plan B: heal / no cast at all.
        let plan_b = plan_log(vec![], vec![]);

        let pool = vec![plan_a, plan_b];
        // intent_target has HP = 20; neither plan is offensive_vs_target.
        assert!(!has_real_kill_line(&pool, intent_target, 20));
    }

    // ── is_plateau tests ──────────────────────────────────────────────────────

    #[test]
    fn plateau_three_within_window_is_plateau() {
        // top=1.0, then 0.98 and 0.97 are both within 0.05 → 3 total scores
        // satisfy top - s < 0.05.  Should be a plateau.
        let scores = vec![1.0f32, 0.98, 0.97, 0.5, 0.4];
        assert!(is_plateau(&scores, 0.05, 3), "3 scores within 0.05 → plateau");
    }

    #[test]
    fn plateau_only_two_within_window_is_not_plateau() {
        // top=1.0, only 0.95 within 0.05 (0.94 is 0.06 away → excluded) → 2 < 3.
        // Wait: 0.95: 1.0 - 0.95 = 0.05, NOT < 0.05. So only 1.0 itself.
        // Use 0.96: 1.0 - 0.96 = 0.04 < 0.05 → 2 total (1.0, 0.96).
        let scores = vec![1.0f32, 0.96, 0.5, 0.4];
        assert!(!is_plateau(&scores, 0.05, 3), "only 2 scores within 0.05 → not plateau");
    }

    #[test]
    fn plateau_clear_winner_is_not_plateau() {
        let scores = vec![1.0f32, 0.3, 0.2];
        assert!(!is_plateau(&scores, 0.05, 3), "spread > 0.05 → not plateau");
    }

    #[test]
    fn plateau_empty_is_not_plateau() {
        assert!(!is_plateau(&[], 0.05, 3));
    }
}

// ── P2 unit tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod p2_tests {
    use super::*;
    use storyforge::combat::ai::planning::{PlanStep, StepOutcome};
    use storyforge::core::AbilityId;
    use storyforge::game::hex::Hex;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    fn cast(target: Entity) -> PlanStep {
        PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target,
            target_pos: Hex::ZERO,
        }
    }

    fn cast_other() -> PlanStep {
        // Cast targeting entity 99 — not the intent target in tests.
        PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target: ent(99),
            target_pos: Hex::ZERO,
        }
    }

    fn outcome_kill(target: Entity) -> StepOutcome {
        StepOutcome { killed: vec![target], damage: 20.0, ..StepOutcome::default() }
    }

    fn outcome_damage(dmg: f32) -> StepOutcome {
        StepOutcome { damage: dmg, ..StepOutcome::default() }
    }

    // ── Test 1: rank=2 (lethal) beats rank=1 (pressure) ─────────────────────

    /// A plan that kills T in the committed prefix (rank=2) must sort above a
    /// plan that deals pressure damage (rank=1), even when the pressure plan
    /// has a higher scalar score.
    #[test]
    fn focus_target_lethal_beats_pressure() {
        let tgt = ent(1);
        let target_hp = 10;

        // Plan A: Cast at T, kills T (rank=2), scalar score 0.5 (low).
        let rank_a = classify_focus_target_bucket(
            &[cast(tgt)],
            &[outcome_kill(tgt)],
            tgt,
            target_hp,
        );
        // Plan B: Cast at T, deals 5 damage = 0.5 * 10 hp ≥ 0.3 * 10 (rank=1), scalar score 2.0 (high).
        let rank_b = classify_focus_target_bucket(
            &[cast(tgt)],
            &[outcome_damage(5.0)],
            tgt,
            target_hp,
        );

        assert_eq!(rank_a, 2, "lethal prefix → rank 2");
        assert_eq!(rank_b, 1, "pressure prefix → rank 1");

        // Lexicographic order: even with scalar A=0.5 and scalar B=2.0,
        // the final score A wins when rank gap dominates.
        let score_a = 0.5 + rank_a as f32 * P2_RANK_OFFSET;
        let score_b = 2.0 + rank_b as f32 * P2_RANK_OFFSET;
        assert!(score_a > score_b, "lethal plan wins despite lower scalar: {score_a} vs {score_b}");
    }

    // ── Test 2: rank=0 (weak offense) beats rank=-1 (off-intent) ────────────

    /// A plan that casts at T but deals sub-threshold damage (rank=0) must beat
    /// a plan that casts at a different target (rank=-1), regardless of scalar.
    #[test]
    fn focus_target_weak_offense_beats_off_intent() {
        let tgt = ent(2);
        let target_hp = 20;

        // Plan A: Cast at T, damage=1 < 0.3*20=6 (rank=0).
        let rank_a = classify_focus_target_bucket(
            &[cast(tgt)],
            &[outcome_damage(1.0)],
            tgt,
            target_hp,
        );
        // Plan B: Cast at other enemy (rank=-1), high scalar.
        let rank_b = classify_focus_target_bucket(
            &[cast_other()],
            &[outcome_damage(30.0)],
            tgt,
            target_hp,
        );

        assert_eq!(rank_a, 0, "weak offense at T → rank 0");
        assert_eq!(rank_b, -1, "no cast at T → rank -1");

        let score_a = 0.0 + rank_a as f32 * P2_RANK_OFFSET; // scalar=0
        let score_b = 3.0 + rank_b as f32 * P2_RANK_OFFSET; // scalar=3, but rank=-1
        assert!(score_a > score_b, "weak offense (rank 0) beats off-intent (rank -1): {score_a} vs {score_b}");
    }

    // ── Test 3: ProtectSelf survival rank=1 beats rank=0 ────────────────────

    /// A plan with self_survival=0.2 (rank=1) beats a plan with self_survival=0.1
    /// (rank=0), even if the offensive plan has a higher scalar score.
    #[test]
    fn protect_self_defensive_beats_offensive_under_mask() {
        let rank_defensive = classify_protect_self_bucket(0.20);
        let rank_offensive  = classify_protect_self_bucket(0.10);

        assert_eq!(rank_defensive, 1, "self_survival 0.20 ≥ ε_self → rank 1");
        assert_eq!(rank_offensive,  0, "self_survival 0.10 < ε_self → rank 0");

        // Even when offensive scalar is higher, defensive wins via rank.
        let score_defensive = 0.5 + rank_defensive as f32 * P2_RANK_OFFSET;
        let score_offensive  = 2.0 + rank_offensive  as f32 * P2_RANK_OFFSET;
        assert!(
            score_defensive > score_offensive,
            "defensive plan (rank 1) beats offensive (rank 0) despite lower scalar: {score_defensive} vs {score_offensive}"
        );
    }

    // ── Test 4: prefix discipline — only committed steps count ───────────────

    /// Key invariant test: `Cast T (lethal)` in plan A has prefix that kills T (rank=2).
    /// Plan B: `Cast other → Cast T (lethal)` — committed prefix is just `Cast other`
    /// (MoveOnly/Cast-only prefix = first step only when step[0] is Cast).
    /// B's prefix is Cast at 'other', not T → rank=-1.
    ///
    /// This ensures bucket classification uses committed-prefix discipline,
    /// not the full-plan outcome.
    #[test]
    fn bucket_uses_prefix_damage_not_tail() {
        let tgt = ent(3);
        let target_hp = 10;

        // Plan A: [Cast T (lethal)]. Committed prefix = [Cast T]. Kills T in prefix → rank 2.
        let rank_a = classify_focus_target_bucket(
            &[cast(tgt)],
            &[outcome_kill(tgt)],
            tgt,
            target_hp,
        );

        // Plan B: [Cast other, Cast T (lethal)].
        // Committed prefix: step[0] is Cast → prefix_len=1 → only [Cast other] is in prefix.
        // The Cast T is in the phantom tail — not committed. Prefix has no Cast at T → rank=-1.
        let rank_b = classify_focus_target_bucket(
            &[cast_other(), cast(tgt)],
            &[outcome_damage(0.0), outcome_kill(tgt)],
            tgt,
            target_hp,
        );

        assert_eq!(rank_a, 2, "solo Cast T kills in prefix → rank 2");
        assert_eq!(rank_b, -1, "Cast other is prefix, Cast T is tail → rank -1 (prefix does not cover T)");
    }
}
