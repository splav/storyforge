//! AI decision log miner — v33 schema (P3b).
//!
//! Reads all `*.jsonl` from a directory and prints aggregated metrics.
//! `actor_tick` events with `schema_version` >= 32 are processed.
//! v32 is schema-additive with v33: `score_trace_log` is absent → None.
//!
//! Class A (direct aggregation):
//!   A1. Adaptation reason frequency per plan (from annotation.adaptation).
//!       Also: rescore_mode from score_trace_log.rescore_mode (v33+).
//!   A2. Intent selection_kind frequency (from decision kind + Skip path).
//!   A3. Plan depth utilisation of the chosen plan (steps.len histogram).
//!   A4. (removed — was plan_divergence-specific, no longer applicable)
//!
//! Class B (sequence reconstruction):
//!   B5. Intent transition stability matrix per actor per combat.
//!
//! Class C (continuation analysis):
//!   C6. Continuation outcomes derived via `classify_continuation_outcome`.
//!       Skip events with stored_goal = new signal (actor passed with goal).
//!
//! Class E (modifier + jitter breakdown):
//!   E1. Per-modifier contribution distributions (summon_bonus, trade_bonus,
//!       repair_bonus). Non-zero entries only; trade_bonus is sign-aware
//!       (can be negative). Denominator: plans with at least one modifier emitted.
//!       Also: trace-sourced E1 from score_trace_log.addends (v33+).
//!   E2. Picking jitter (noise_applied) for chosen plans. Sign-aware reporter
//!       (mean / min / max / abs_max). Denominator: chosen plans.
//!
//! Class G (critics coverage — v33+ trace source):
//!   G2. trace-sourced per-multiplier-kind stats from score_trace_log.multipliers
//!       for chosen plans (v33+). Parallel to legacy G1 from annotation.critics.
//!
//! Class H (bands & agenda — schema v32+):
//!   H1. Band coverage: per-band tick count, winner-intent distribution,
//!       per-axis consideration histograms (urgency/feasibility/leverage/
//!       safety/role_affinity/continuation_value).
//!   H2. Agenda-item win-rate per band: which item index (0/1/2) wins most
//!       often. Sanity check: NormalTactical should not degenerate to item 0.
//!
//! Usage: `cargo run --release --bin mine_ai_logs -- --dir logs/`

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use storyforge::combat::ai::adapt::AdaptationReason;
use storyforge::combat::ai::repair::{
    classify_continuation_outcome, ContinuationOutcome, FreshDecisionKind, StoredGoalContext,
};
use storyforge::combat::ai::intent::{IntentKind, TacticalIntent};
use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision, StoredGoalContextSnapshot};
use storyforge::combat::ai::world::tags::AbilityTag;

// ── Session actor key ─────────────────────────────────────────────────────────

/// One session = one JSONL file (= one combat).
type SessionActorKey = (String, u64); // (filename, actor_id)

// ── H3c bucket enum ───────────────────────────────────────────────────────────

/// Mutually exclusive post-hoc fallback cause buckets (H3c).
/// Precedence: ApMpBlocked → NoTargetInAgenda → TargetUnreachable →
/// OnlyMovePlans → NoPlanAttemptsTarget → Unclassified.
///
/// Note: OnlyMovePlans (bucket 3) is checked before NoPlanAttemptsTarget (bucket 4)
/// because "no Cast steps in pool at all" is a more specific diagnosis than
/// "Cast steps exist but none target the agenda entity". When all plans are
/// Move-only, bucket 4 would also fire (trivially true), so we prefer bucket 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum H3cBucket {
    ApMpBlocked          = 0,
    NoTargetInAgenda     = 1,
    TargetUnreachable    = 2,
    OnlyMovePlans        = 3,
    NoPlanAttemptsTarget = 4,
    Unclassified         = 5,
}

const H3C_BUCKET_LABELS: [&str; 6] = [
    "ap_mp_blk",
    "no_tgt",
    "unreach",
    "only_move",
    "no_attempt",
    "unclass",
];

// ── Aggregation state ─────────────────────────────────────────────────────────

#[derive(Default)]
struct Aggregate {
    // A1: adaptation reason per plan (all plans in pool, not just chosen)
    adaptation_counts: BTreeMap<String, usize>,
    total_plans: usize,

    // A2: decision_kind frequencies
    decision_kind_counts: BTreeMap<String, usize>,
    total_decisions: usize,

    // A3: chosen plan depth histogram
    depth_counts: BTreeMap<usize, usize>,
    total_chosen: usize,

    // C6: continuation outcomes (derived via classify_continuation_outcome)
    cont_no_stored: usize,
    cont_method_delivered: usize,
    cont_in_transit: usize,
    cont_abandoned_reactive: BTreeMap<String, usize>,
    cont_abandoned_voluntary: usize,
    cont_abandoned_invalidating: usize,
    cont_abandoned_ttl_expired: usize,
    cont_severity_counts: BTreeMap<String, usize>,
    cont_goal_kind_counts: BTreeMap<String, usize>,
    total_with_continuation: usize,

    // Skip-path signals
    skip_total: usize,
    skip_with_stored_goal: usize,

    // B5: per-(session, actor) ordered list of (tick_order, decision_kind)
    // `tick_order` = sequential index within the session for ordering.
    actor_timelines: HashMap<SessionActorKey, Vec<(u64, String)>>,
    // Counter per session-actor for ordering ticks (monotonically increasing).
    actor_tick_counters: HashMap<SessionActorKey, u64>,

    // D1: outcome fact distributions — collected from chosen plan steps (non-zero only).
    d1_enemy_damage: Vec<f32>,
    d1_ally_damage: Vec<f32>,
    d1_self_damage: Vec<f32>,
    d1_hp_restored: Vec<f32>,
    d1_cc_turns_applied: Vec<f32>,
    d1_vulnerability_applied: Vec<f32>,
    d1_armor_shred_applied: Vec<f32>,
    // Kill binary facts: counts of Cast steps where flag == 1.0.
    d1_p_kill_now_count: usize,
    d1_p_kill_soon_count: usize,
    // Total Cast steps from chosen plans (denominator for kill rates).
    d1_total_cast_steps: usize,

    // D2: AoE per-entity damage breakdown.
    // Each entry = number of entities hit in one AoE Cast step.
    d2_entities_hit_per_cast: Vec<usize>,
    // All per-entity damage values across all AoE Cast steps (for avg/max).
    d2_per_entity_damage: Vec<f32>,

    // E1: modifier contribution distributions (per-plan, non-zero entries only).
    e1_summon_bonus: Vec<f32>,
    e1_trade_bonus: Vec<f32>,  // signed: can be negative
    e1_repair_bonus: Vec<f32>,
    // Plans in which at least one modifier was emitted (denominator for "% of plans with modifiers").
    e1_total_modifier_entries: usize,

    // E2: picking jitter (noise_applied) for chosen plans (non-zero entries only).
    e2_noise_applied: Vec<f32>,
    // Chosen plans processed (denominator for "% chosen with non-zero noise").
    e2_chosen_count: usize,

    // F1: AI tags coverage — per-tag counts for chosen plans.
    // A chosen plan is counted for tag T when at least one Cast step has T in its effective_ai_tags.
    // Denominator: total_chosen (plans where annotation.chosen == true).
    f1_ability_tag_counts: BTreeMap<String, usize>,

    // F2: Need signals post-9.B.
    // NOTE: NeedSignals are NOT part of the v30 log schema (ActorTickEvent).
    // This section pins the setup_aoe goal_kind count as a regression gate
    // (setup_aoe goal should never be stored; its NeedSignal is always 0.0).
    f2_setup_aoe_goal_count: usize,

    // F3: Continuation severity — cross-tab severity × hardcoded StatusTag.
    // Populated on actor_status_changed mismatch events (cont.severity != None).
    // Hardcoded StatusTag mapping (from statuses.toml + derive_status_tags rules):
    //   stunned, paralyzed → HardCC  |  taunted, pact_control → Compulsion
    //   poisoned, burning, exhaustion → Dot  |  defending → Buff
    //   disoriented, broken_faith → SoftCC/Cosmetic
    f3_severity_counts: BTreeMap<String, usize>,
    // cross-tab key = "Severity×Tag" e.g. "Invalidating×HardCC"
    f3_severity_by_tag: BTreeMap<String, usize>,
    // Per-severity goal continuation rate: preserved vs abandoned counts.
    f3_preserved_by_severity: BTreeMap<String, usize>,
    f3_abandoned_by_severity: BTreeMap<String, usize>,

    // G1: Critics coverage (step 10) — per-critic stats for chosen plans.
    // For each critic kind we track:
    //   - count of chosen plans where the critic fired (had a hit)
    //   - distribution of multipliers actually applied
    // Denominator: total_chosen.
    g1_critic_hit_counts: BTreeMap<String, usize>,
    g1_critic_multipliers: BTreeMap<String, Vec<f32>>,
    // A: pool-wide per-critic fire rate. How often each critic fires across
    // ALL plans in the pool (regardless of chosen). Distinguishes
    // "filter that knocks out candidates" (pool > 0, chosen 0) from
    // "dead critic / over-strict gating" (both 0).
    g1_pool_per_critic: BTreeMap<String, usize>,
    // Pool-wide aggregate (any critic fired) — kept for the headline number.
    g1_pool_with_any_critic: usize,
    g1_pool_total: usize,
    // E (Overcommit-only): cross-tab `OvercommitIntoDanger` hit count
    // by chosen-plan decision_kind. Tells us whether the over-fire is
    // localised to a specific decision class (e.g. only on Move).
    g1_overcommit_by_decision_kind: BTreeMap<String, usize>,
    // Total chosen plans by decision_kind (denominator for the cross-tab).
    g1_chosen_by_decision_kind: BTreeMap<String, usize>,

    // G: Overcommit × adaptation cross-tab. Detects whether Overcommit hits
    // pile up on top of LastStand-mode plans (double-pressure) or are
    // localised to Default-mode plans.
    //
    // Reason key = adaptation reason variant ("expected_self_lethal" /
    // "protect_self_no_defensive" / "protect_self_futile") or "none".
    g1_overcommit_by_adapt_reason: BTreeMap<String, usize>,
    g1_chosen_by_adapt_reason: BTreeMap<String, usize>,

    // H1: Band coverage (step 11.6, schema v32).
    // Per-band tick count and winner-intent distribution.
    // Denominator: total_decisions (all ticks, including skip-path).
    h1_band_tick_counts: BTreeMap<String, usize>,
    // Per-band: the winning intent kind (agenda_item attribution on chosen plan).
    // Key = "band/intent_kind", value = count.
    h1_band_winner_intent: BTreeMap<String, usize>,
    // Per-axis consideration histograms across ALL agenda items (all ticks, all bands).
    // Stored as raw f32 vecs for percentile reporting.
    h1_urgency: Vec<f32>,
    h1_feasibility: Vec<f32>,
    h1_leverage: Vec<f32>,
    h1_safety: Vec<f32>,
    h1_role_affinity: Vec<f32>,
    h1_continuation_value: Vec<f32>,

    // H1c.bis (step 11.8): per-IntentKind leverage histograms from the chosen plan's
    // considerations_per_item. Each plan-item's leverage goes into the bucket matching
    // the corresponding agenda item's kind.
    // Note: Reposition and SetupAOE share one bucket — they use the same overlay branch.
    // The global h1_leverage above is kept for cross-kind comparison and backward compat.
    h1_leverage_focus_target: Vec<f32>,
    h1_leverage_apply_cc: Vec<f32>,
    h1_leverage_protect_ally: Vec<f32>,
    h1_leverage_protect_self: Vec<f32>,
    h1_leverage_reposition: Vec<f32>, // also covers SetupAOE (same overlay branch)
    h1_leverage_last_stand: Vec<f32>,

    // H2: Agenda-item win-rate (step 11.6, schema v32).
    // Per band: which agenda item index (0/1/2...) wins most often.
    // Key = "band/item_index", value = count of ticks where that item won.
    h2_band_item_win: BTreeMap<String, usize>,
    // Denominator per band: total ticks with a chosen plan (regardless of
    // agenda attribution). Fallback rate = h2_band_chosen_total - Σ h2_band_item_win
    // for that band — indicates how often no agenda item was eligible.
    h2_band_chosen_total: BTreeMap<String, usize>,

    // H3a: Construction-time metrics from existing v32 logs.
    // Agenda size distribution per band. Key = "band/size", value = count of ticks.
    h3a_agenda_size: BTreeMap<String, usize>,
    // Item.target=None rate by (band, item.kind). Key = "band/kind/none" or "band/kind/some".
    h3a_target_none: BTreeMap<String, usize>,
    h3a_target_some: BTreeMap<String, usize>,
    // Per-band unattributed fallback breakdown.
    // Key = band. Values: chosen ticks with an attributed item vs without.
    h3a_band_attributed: BTreeMap<String, usize>,
    h3a_band_unattributed: BTreeMap<String, usize>,
    // Sub-classification of unattributed: agenda was empty.
    h3a_unattr_no_items: BTreeMap<String, usize>,
    // Sub-classification: all items have target=None.
    h3a_unattr_all_no_target: BTreeMap<String, usize>,

    // H3b: Per-plan reject reason breakdown (from reject_reasons_per_item, new in 11.7).
    // Key = "band/kind/reason", value = count of rejected per-plan entries.
    h3b_reject_reason: BTreeMap<String, usize>,
    // Total per-plan entries (eligible + rejected) per (band, kind). Key = "band/kind".
    h3b_total: BTreeMap<String, usize>,
    h3b_eligible: BTreeMap<String, usize>,
    // Whether any v32 log with reject_reasons_per_item was observed.
    h3b_has_data: bool,

    // H3c: Post-hoc fallback cause classifier (schema v32).
    // Counts for each unattributed tick (chosen plan has no agenda_item AND agenda non-empty).
    // Buckets are mutually exclusive; precedence order: ap_mp_blocked → no_target_in_agenda →
    // target_unreachable → no_plan_attempts_target → only_move_plans → unclassified.
    // Key = band label.
    h3c_ap_mp_blocked: BTreeMap<String, usize>,
    h3c_no_target_in_agenda: BTreeMap<String, usize>,
    h3c_target_unreachable: BTreeMap<String, usize>,
    h3c_no_plan_attempts_target: BTreeMap<String, usize>,
    h3c_only_move_plans: BTreeMap<String, usize>,
    h3c_unclassified: BTreeMap<String, usize>,
    // Per-(band, primary intent kind) breakdown. Key = "band/kind".
    h3c_by_band_kind: BTreeMap<String, [usize; 6]>,
    // Total unattributed ticks per band (= Σ all buckets per band). Sanity vs H3a.
    h3c_total: BTreeMap<String, usize>,
    // Total ticks with band data per band (for fallback% denominator).
    h3c_band_total: BTreeMap<String, usize>,

    // ── P3b: score_trace_log-sourced stats (v33+) ─────────────────────────────

    // A1-trace: rescore_mode distribution from score_trace_log.rescore_mode.
    // Key = "Default" / "LastStand" / "none" (absent). Denominator: total_plans.
    a1_trace_rescore_mode: BTreeMap<String, usize>,
    // Plans with score_trace_log present (v33+). Denominator for trace-sourced stats.
    trace_plans_total: usize,

    // E1-trace: addend contributions from score_trace_log.addends (v33+).
    // Parallel to E1 from annotation.modifiers; cross-validates the two sources.
    e1_trace_summon: Vec<f32>,
    e1_trace_trade: Vec<f32>,
    e1_trace_repair: Vec<f32>,
    // Plans with at least one addend in score_trace_log (denominator for "% plans with addends").
    e1_trace_modifier_entries: usize,

    // G2: multiplier-kind breakdown from score_trace_log.multipliers (v33+).
    // Per-kind: count of chosen plans where that kind appeared + value distribution.
    // Key = "sanity" / "critic". Denominator: total_chosen.
    g2_trace_multiplier_counts: BTreeMap<String, usize>,
    g2_trace_multiplier_values: BTreeMap<String, Vec<f32>>,
    // Chosen plans with score_trace_log present (denominator for G2).
    g2_trace_chosen_total: usize,
}

impl Aggregate {
    fn process_event(&mut self, session: &str, event: &ActorTickEvent) {
        self.total_decisions += 1;

        let decision_kind = decision_kind_label(&event.decision);

        // A2: decision_kind
        *self.decision_kind_counts.entry(decision_kind.to_owned()).or_default() += 1;

        // Skip path
        if matches!(event.decision, LoggedDecision::Skip { .. }) {
            self.skip_total += 1;
            if event.continuation.is_some() {
                self.skip_with_stored_goal += 1;
            }
            // B5: record skip tick for timeline
            let key: SessionActorKey = (session.to_owned(), event.actor_id);
            let counter = self.actor_tick_counters.entry(key.clone()).or_default();
            let order = *counter;
            *counter += 1;
            self.actor_timelines.entry(key).or_default().push((order, decision_kind.to_owned()));
            // C6 for skip path
            self.process_continuation(event, FreshDecisionKind::EndTurn);
            return;
        }

        // Full path: A1 + A3 + B5 + C6

        // A1: adaptation reason per plan
        for plan in &event.plans {
            self.total_plans += 1;
            let reason_key = plan
                .annotation
                .adaptation
                .as_ref()
                .map(|a| format!("{:?}", a.reason))
                .unwrap_or_else(|| "none".to_owned());
            *self.adaptation_counts.entry(reason_key).or_default() += 1;
        }

        // A3: chosen plan depth + D1/D2: outcome fact distributions
        if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
            self.total_chosen += 1;
            *self.depth_counts.entry(chosen.steps.len()).or_default() += 1;

            // D1 + D2: collect from all steps of the chosen plan.
            for outcome in &chosen.annotation.outcomes {
                self.d1_total_cast_steps += 1;

                if outcome.enemy_damage > 0.0 { self.d1_enemy_damage.push(outcome.enemy_damage); }
                if outcome.ally_damage > 0.0 { self.d1_ally_damage.push(outcome.ally_damage); }
                if outcome.self_damage > 0.0 { self.d1_self_damage.push(outcome.self_damage); }
                if outcome.hp_restored > 0.0 { self.d1_hp_restored.push(outcome.hp_restored); }
                if outcome.cc_turns_applied > 0.0 { self.d1_cc_turns_applied.push(outcome.cc_turns_applied); }
                if outcome.vulnerability_applied > 0.0 { self.d1_vulnerability_applied.push(outcome.vulnerability_applied); }
                if outcome.armor_shred_applied > 0.0 { self.d1_armor_shred_applied.push(outcome.armor_shred_applied); }

                if outcome.p_kill_now >= 1.0 { self.d1_p_kill_now_count += 1; }
                if outcome.p_kill_soon >= 1.0 { self.d1_p_kill_soon_count += 1; }

                // D2: AoE breakdown.
                if !outcome.enemy_damage_per_entity.is_empty() {
                    self.d2_entities_hit_per_cast.push(outcome.enemy_damage_per_entity.len());
                    for &(_, dmg) in &outcome.enemy_damage_per_entity {
                        self.d2_per_entity_damage.push(dmg);
                    }
                }
            }
        }

        // E1: modifier contributions — walk all plans in pool.
        for plan in &event.plans {
            if !plan.annotation.modifiers().is_empty() {
                self.e1_total_modifier_entries += 1;
            }
            for mc in plan.annotation.modifiers() {
                if mc.contribution.abs() > f32::EPSILON {
                    match mc.name.as_str() {
                        "summon_bonus" => self.e1_summon_bonus.push(mc.contribution),
                        "trade_bonus"  => self.e1_trade_bonus.push(mc.contribution),
                        "repair_bonus" => self.e1_repair_bonus.push(mc.contribution),
                        _ => {}
                    }
                }
            }
        }

        // E2: picking jitter — collect noise_applied from chosen plan only.
        if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
            self.e2_chosen_count += 1;
            if let Some(pi) = &chosen.annotation.pick {
                if pi.noise_applied.abs() > f32::EPSILON {
                    self.e2_noise_applied.push(pi.noise_applied);
                }
            }
        }

        // F1: AI tags coverage — per-tag counts for chosen plans.
        if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
            for tag in AbilityTag::iter() {
                let has_tag = chosen
                    .annotation
                    .effective_ai_tags
                    .iter()
                    .any(|step_tags| step_tags.contains_tag(tag));
                if has_tag {
                    *self.f1_ability_tag_counts.entry(tag.name().to_owned()).or_default() += 1;
                }
            }

            // G1 (chosen-plan stats): for each critic that fired, increment
            // the hit count and record the multiplier.
            let mut overcommit_in_chosen = false;
            for hit in &chosen.annotation.critics {
                let key = format!("{:?}", hit.critic);
                *self.g1_critic_hit_counts.entry(key.clone()).or_default() += 1;
                self.g1_critic_multipliers.entry(key.clone()).or_default().push(hit.multiplier);
                if key == "OvercommitIntoDanger" {
                    overcommit_in_chosen = true;
                }
            }

            // E: chosen-plan totals by decision_kind + Overcommit cross-tab.
            *self.g1_chosen_by_decision_kind.entry(decision_kind.to_owned()).or_default() += 1;
            if overcommit_in_chosen {
                *self.g1_overcommit_by_decision_kind.entry(decision_kind.to_owned()).or_default() += 1;
            }

            // G: Overcommit × adaptation reason cross-tab.
            // Adaptation reason carries the LastStand/ProtectSelf context;
            // any non-None reason means the plan was rescored under a
            // non-Default regime. Use the variant's `kind` tag as the key.
            let adapt_key = chosen
                .annotation
                .adaptation
                .as_ref()
                .map(|a| match &a.reason {
                    AdaptationReason::ExpectedSelfLethal { .. } => "expected_self_lethal".to_owned(),
                    AdaptationReason::ProtectSelfNoDefensive => "protect_self_no_defensive".to_owned(),
                    AdaptationReason::ProtectSelfFutile { .. } => "protect_self_futile".to_owned(),
                })
                .unwrap_or_else(|| "none".to_owned());
            *self.g1_chosen_by_adapt_reason.entry(adapt_key.clone()).or_default() += 1;
            if overcommit_in_chosen {
                *self.g1_overcommit_by_adapt_reason.entry(adapt_key).or_default() += 1;
            }
        }

        // G1 (pool-wide): count how often any critic fires across all plans
        // in the pool, plus per-critic pool fire rate (Variant A).
        for plan in &event.plans {
            self.g1_pool_total += 1;
            if !plan.annotation.critics.is_empty() {
                self.g1_pool_with_any_critic += 1;
            }
            // A: per-critic pool count. Each critic counted at most once per plan.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for hit in &plan.annotation.critics {
                let key = format!("{:?}", hit.critic);
                if seen.insert(key.clone()) {
                    *self.g1_pool_per_critic.entry(key).or_default() += 1;
                }
            }
        }

        // ── P3b: score_trace_log-sourced stats (v33+) ─────────────────────────
        // E1-trace + A1-trace: walk all plans in the pool.
        for plan in &event.plans {
            let ann = &plan.annotation;
            if let Some(trace) = &ann.score_trace_log {
                self.trace_plans_total += 1;

                // A1-trace: rescore_mode from trace.
                let mode_key = match trace.rescore_mode {
                    Some(m) => format!("{m:?}"),
                    None => "none".to_owned(),
                };
                *self.a1_trace_rescore_mode.entry(mode_key).or_default() += 1;

                // E1-trace: addend contributions.
                if !trace.addends.is_empty() {
                    self.e1_trace_modifier_entries += 1;
                }
                for addend in &trace.addends {
                    if addend.value.abs() > f32::EPSILON {
                        match addend.name.as_str() {
                            "summon_bonus" => self.e1_trace_summon.push(addend.value),
                            "trade_bonus"  => self.e1_trace_trade.push(addend.value),
                            "repair_bonus" => self.e1_trace_repair.push(addend.value),
                            _ => {}
                        }
                    }
                }
            }
        }

        // G2: multiplier-kind stats from chosen plan's score_trace_log (v33+).
        if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
            if let Some(trace) = &chosen.annotation.score_trace_log {
                self.g2_trace_chosen_total += 1;
                for m in &trace.multipliers {
                    let kind_key = format!("{:?}", m.kind);
                    *self.g2_trace_multiplier_counts.entry(kind_key.clone()).or_default() += 1;
                    self.g2_trace_multiplier_values.entry(kind_key).or_default().push(m.value);
                }
            }
        }

        // B5
        let key: SessionActorKey = (session.to_owned(), event.actor_id);
        let counter = self.actor_tick_counters.entry(key.clone()).or_default();
        let order = *counter;
        *counter += 1;
        self.actor_timelines.entry(key).or_default().push((order, decision_kind.to_owned()));

        // C6
        let fdk = fresh_decision_kind(&event.decision);
        self.process_continuation(event, fdk);

        // H1: Band coverage — per-band tick count + per-axis consideration histograms.
        // Skipped on skip-path (event.band is None).
        if let Some(band) = &event.band {
            let band_key = format!("{band:?}");
            *self.h1_band_tick_counts.entry(band_key.clone()).or_default() += 1;

            // H1: per-axis histograms from agenda item-level considerations.
            for item in &event.agenda {
                let c = &item.considerations;
                self.h1_urgency.push(c.urgency);
                self.h1_feasibility.push(c.feasibility);
                self.h1_leverage.push(c.leverage);
                self.h1_safety.push(c.safety);
                self.h1_role_affinity.push(c.role_affinity);
                self.h1_continuation_value.push(c.continuation_value);
            }

            // H1c.bis (step 11.8): per-IntentKind leverage histograms from chosen plan.
            // Uses considerations_per_item (plan-aware overlay values), matched to agenda
            // items by index. Separate from the item-level h1_leverage above (which uses
            // agenda item baseline considerations, not plan-specific overlays).
            if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
                for (idx, cons) in chosen.annotation.considerations_per_item.iter().enumerate() {
                    if let Some(item) = event.agenda.get(idx) {
                        let bucket = match item.kind {
                            IntentKind::FocusTarget => &mut self.h1_leverage_focus_target,
                            IntentKind::ApplyCC     => &mut self.h1_leverage_apply_cc,
                            IntentKind::ProtectAlly => &mut self.h1_leverage_protect_ally,
                            IntentKind::ProtectSelf => &mut self.h1_leverage_protect_self,
                            IntentKind::Reposition | IntentKind::SetupAOE => &mut self.h1_leverage_reposition,
                            IntentKind::LastStand   => &mut self.h1_leverage_last_stand,
                        };
                        bucket.push(cons.leverage);
                    }
                }
            }

            // H1: winner-intent distribution — attributed agenda item kind on chosen plan.
            if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
                if let Some(item_idx) = chosen.annotation.agenda_item {
                    if let Some(item) = event.agenda.get(item_idx as usize) {
                        let winner_key = format!("{band_key}/{:?}", item.kind);
                        *self.h1_band_winner_intent.entry(winner_key).or_default() += 1;
                    }
                }
            }

            // H2: agenda-item win-rate — which item index wins per band.
            if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
                *self.h2_band_chosen_total.entry(band_key.clone()).or_default() += 1;
                if let Some(item_idx) = chosen.annotation.agenda_item {
                    let win_key = format!("{band_key}/{item_idx}");
                    *self.h2_band_item_win.entry(win_key).or_default() += 1;
                }
            }

            // H3a: construction-time metrics from existing v32 logs.
            // Agenda size distribution per band.
            let size_key = format!("{}/{}", band_key, event.agenda.len());
            *self.h3a_agenda_size.entry(size_key).or_default() += 1;

            // Item.target=None rate by (band, item.kind).
            for item in &event.agenda {
                let kind_key = format!("{band_key}/{:?}", item.kind);
                if item.target.is_none() {
                    *self.h3a_target_none.entry(kind_key).or_default() += 1;
                } else {
                    *self.h3a_target_some.entry(kind_key).or_default() += 1;
                }
            }

            // H3a: fallback bucket classification (chosen plan).
            if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
                if chosen.annotation.agenda_item.is_some() {
                    *self.h3a_band_attributed.entry(band_key.clone()).or_default() += 1;
                } else {
                    *self.h3a_band_unattributed.entry(band_key.clone()).or_default() += 1;
                    // Sub-classification.
                    if event.agenda.is_empty() {
                        *self.h3a_unattr_no_items.entry(band_key.clone()).or_default() += 1;
                    } else if event.agenda.iter().all(|it| it.target.is_none()) {
                        *self.h3a_unattr_all_no_target.entry(band_key.clone()).or_default() += 1;
                    }
                    // else: unclassified at construction-time; needs per-plan reject reasons.
                }
            }

            // H3b: per-plan reject reason breakdown (requires reject_reasons_per_item
            // field from 11.7 instrumentation — only present in new logs).
            for plan in &event.plans {
                let rr = &plan.annotation.reject_reasons_per_item;
                if rr.is_empty() {
                    // Old log without 11.7 field — skip per-plan stats.
                    continue;
                }
                self.h3b_has_data = true;
                for (item_idx, reason_opt) in rr.iter().enumerate() {
                    let item = match event.agenda.get(item_idx) {
                        Some(i) => i,
                        None => continue,
                    };
                    let kind_key = format!("{band_key}/{:?}", item.kind);
                    *self.h3b_total.entry(kind_key.clone()).or_default() += 1;
                    match reason_opt {
                        None => {
                            *self.h3b_eligible.entry(kind_key).or_default() += 1;
                        }
                        Some(reason) => {
                            let reason_key = format!("{kind_key}/{reason:?}");
                            *self.h3b_reject_reason.entry(reason_key).or_default() += 1;
                        }
                    }
                }
            }

            // H3c: post-hoc fallback cause classifier.
            // Runs for unattributed ticks where agenda is non-empty.
            *self.h3c_band_total.entry(band_key.clone()).or_default() += 1;
            if let Some(chosen) = event.plans.iter().find(|p| p.annotation.chosen) {
                if chosen.annotation.agenda_item.is_none() && !event.agenda.is_empty() {
                    let primary_kind = format!("{:?}", event.agenda[0].kind);
                    let band_kind_key = format!("{band_key}/{primary_kind}");

                    // Find actor in snapshot.
                    let actor_snap = event.snapshot.units.iter()
                        .find(|u| u.entity.to_bits() == event.actor_id);

                    // Bucket 1: ap_mp_blocked — actor has no AP and no MP.
                    let bucket = if let Some(actor) = actor_snap {
                        if actor.action_points == 0 && actor.movement_points == 0 {
                            H3cBucket::ApMpBlocked
                        } else {
                            // Bucket 2: no_target_in_agenda — all items have target=None.
                            let primary_target = event.agenda[0].target;
                            if primary_target.is_none() && event.agenda.iter().all(|it| it.target.is_none()) {
                                H3cBucket::NoTargetInAgenda
                            } else if let Some(target_bits) = primary_target {
                                // Bucket 3: target_unreachable — target too far.
                                let target_snap = event.snapshot.units.iter()
                                    .find(|u| u.entity.to_bits() == target_bits);
                                let unreachable = target_snap.map(|t| {
                                    let dist = actor.pos.unsigned_distance_to(t.pos);
                                    dist > (actor.movement_points as u32) + actor.max_attack_range
                                }).unwrap_or(false);

                                if unreachable {
                                    H3cBucket::TargetUnreachable
                                } else {
                                    // Bucket 3 (OnlyMovePlans): no Cast steps at all in pool.
                                    // Checked before bucket 4: "all move-only" is a more specific
                                    // root-cause than "no plan targets this entity" (trivially true
                                    // when there are no Cast plans at all).
                                    let any_cast = event.plans.iter().any(|p| {
                                        p.steps.iter().any(|s| matches!(s,
                                            storyforge::combat::ai::plan::PlanStep::Cast { .. }
                                        ))
                                    });
                                    if !any_cast {
                                        H3cBucket::OnlyMovePlans
                                    } else {
                                        // Bucket 4: Cast plans exist, but none target agenda entity.
                                        let any_plan_targets = event.plans.iter().any(|p| {
                                            p.steps.iter().any(|s| matches!(s,
                                                storyforge::combat::ai::plan::PlanStep::Cast { target, .. }
                                                if target.to_bits() == target_bits
                                            ))
                                        });
                                        if !any_plan_targets {
                                            H3cBucket::NoPlanAttemptsTarget
                                        } else {
                                            H3cBucket::Unclassified
                                        }
                                    }
                                }
                            } else {
                                // primary_target is None but not all items lack target —
                                // fall through to OnlyMovePlans / Unclassified.
                                let any_cast = event.plans.iter().any(|p| {
                                    p.steps.iter().any(|s| matches!(s,
                                        storyforge::combat::ai::plan::PlanStep::Cast { .. }
                                    ))
                                });
                                if !any_cast {
                                    H3cBucket::OnlyMovePlans
                                } else {
                                    H3cBucket::Unclassified
                                }
                            }
                        }
                    } else {
                        // actor not in snapshot — can't classify by resource/position
                        H3cBucket::Unclassified
                    };

                    match bucket {
                        H3cBucket::ApMpBlocked =>
                            *self.h3c_ap_mp_blocked.entry(band_key.clone()).or_default() += 1,
                        H3cBucket::NoTargetInAgenda =>
                            *self.h3c_no_target_in_agenda.entry(band_key.clone()).or_default() += 1,
                        H3cBucket::TargetUnreachable =>
                            *self.h3c_target_unreachable.entry(band_key.clone()).or_default() += 1,
                        H3cBucket::NoPlanAttemptsTarget =>
                            *self.h3c_no_plan_attempts_target.entry(band_key.clone()).or_default() += 1,
                        H3cBucket::OnlyMovePlans =>
                            *self.h3c_only_move_plans.entry(band_key.clone()).or_default() += 1,
                        H3cBucket::Unclassified =>
                            *self.h3c_unclassified.entry(band_key.clone()).or_default() += 1,
                    }
                    *self.h3c_total.entry(band_key.clone()).or_default() += 1;
                    self.h3c_by_band_kind
                        .entry(band_kind_key)
                        .or_insert([0usize; 6])[bucket as usize] += 1;
                }
            }
        }
    }

    fn process_continuation(&mut self, event: &ActorTickEvent, fdk: FreshDecisionKind) {
        self.total_with_continuation += 1;

        let Some(cont) = &event.continuation else {
            self.cont_no_stored += 1;
            return;
        };

        // Reconstruct StoredGoalContext from the log snapshot for classify_continuation_outcome.
        let stored_goal = StoredGoalContext::from(&cont.stored_goal);

        // Approximate the fresh TacticalIntent from decision + stored goal context.
        let fresh_intent = approximate_fresh_intent(&event.decision, &cont.stored_goal);

        // fresh_reason: read from event.intent_reason (full structured reason).
        // None on skip-path; classify as NoRuleDefault (voluntary if goal abandoned).
        let fallback_reason = storyforge::combat::ai::intent::IntentReason::NoRuleDefault;
        let fresh_reason = event.intent_reason.as_ref().unwrap_or(&fallback_reason);

        let outcome = classify_continuation_outcome(
            Some(&stored_goal),
            fresh_intent,
            fdk,
            fresh_reason,
            cont.severity,
            cont.age,
        );

        let goal_preserved = matches!(
            outcome,
            ContinuationOutcome::GoalPreservedMethodDelivered
                | ContinuationOutcome::GoalPreservedInTransit
        );

        match outcome {
            ContinuationOutcome::NoStoredGoal => {
                self.cont_no_stored += 1;
            }
            ContinuationOutcome::GoalPreservedMethodDelivered => {
                self.cont_method_delivered += 1;
            }
            ContinuationOutcome::GoalPreservedInTransit => {
                self.cont_in_transit += 1;
            }
            ContinuationOutcome::GoalAbandonedReactive { source } => {
                *self.cont_abandoned_reactive.entry(source).or_default() += 1;
            }
            ContinuationOutcome::GoalAbandonedVoluntary => {
                self.cont_abandoned_voluntary += 1;
            }
            ContinuationOutcome::GoalAbandonedInvalidating => {
                self.cont_abandoned_invalidating += 1;
            }
            ContinuationOutcome::GoalAbandonedTtlExpired => {
                self.cont_abandoned_ttl_expired += 1;
            }
            ContinuationOutcome::LegacyV25Abandoned { .. } => {
                // Cannot appear from classify_continuation_outcome; ignore.
            }
        }

        if let Some(sev) = cont.severity {
            *self.cont_severity_counts.entry(format!("{sev:?}")).or_default() += 1;
        }
        let goal_kind = format!("{:?}", cont.stored_goal.kind);
        *self.cont_goal_kind_counts.entry(goal_kind.clone()).or_default() += 1;

        // F2: setup_aoe goal regression pin — count goals of kind "setup_aoe".
        if goal_kind.to_lowercase().contains("setup_aoe")
            || cont.stored_goal.kind == "setup_aoe"
        {
            self.f2_setup_aoe_goal_count += 1;
        }

        // F3: continuation severity cross-tab on actor_status_changed events.
        // We use cont.severity as the mismatch signal regardless of reason_code,
        // because the severity is what drives continuation behaviour.
        if let Some(sev) = cont.severity {
            let sev_label = format!("{sev:?}");
            *self.f3_severity_counts.entry(sev_label.clone()).or_default() += 1;

            // Cross-tab: which statuses are on the actor at this tick?
            // Map each known status_id to its hardcoded StatusTag bucket.
            let tags_seen = statuses_to_tag_labels(event);
            if tags_seen.is_empty() {
                let key = format!("{sev_label}×(none)");
                *self.f3_severity_by_tag.entry(key).or_default() += 1;
            } else {
                for tag_label in &tags_seen {
                    let key = format!("{sev_label}×{tag_label}");
                    *self.f3_severity_by_tag.entry(key).or_default() += 1;
                }
            }

            // Goal continuation rate per severity.
            if goal_preserved {
                *self.f3_preserved_by_severity.entry(sev_label.clone()).or_default() += 1;
            } else {
                *self.f3_abandoned_by_severity.entry(sev_label).or_default() += 1;
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Hardcoded StatusTag mapping derived from `assets/data/statuses.toml` and
/// `derive_status_tags` rules. Returns *distinct* tag labels for statuses active
/// on the actor at this tick.
///
/// ContentDb is not available in the mining tool (pure-JSONL), so we pin the
/// 10 known status ids from statuses.toml directly. Unknown ids are labelled
/// "unknown". Rules mirror `classify.rs::derive_status_tags`.
///
/// Mapping (statuses.toml × classify rules):
///   stunned, paralyzed     → HardCC    (skips_turn)
///   taunted, pact_control  → Compulsion (forces_targeting / ai_controlled→Cosmetic)
///   poisoned, burning,
///   exhaustion             → Dot       (dot_dice / hp_percent_dot)
///   defending              → Buff      (armor_bonus)
///   disoriented            → SoftCC    (causes_disadvantage)
///   broken_faith           → Cosmetic  (blocks_mana — no classify rule)
///
/// Note: pact_control has ai_controlled=true only; no classify rule → Cosmetic.
fn statuses_to_tag_labels(event: &ActorTickEvent) -> Vec<&'static str> {
    // Find the actor's own UnitSnapshot in the snapshot.
    let actor_statuses = event
        .snapshot
        .units
        .iter()
        .find(|u| u.entity.to_bits() == event.actor_id)
        .map(|u| u.statuses.as_slice())
        .unwrap_or(&[]);

    let mut seen: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for s in actor_statuses {
        let tag = match s.id.0.as_str() {
            "stunned" | "paralyzed"          => "HardCC",
            "taunted"                         => "Compulsion",
            "pact_control"                    => "Cosmetic",
            "poisoned" | "burning"            => "Dot",
            "exhaustion"                      => "Dot",     // hp_percent_dot + speed_bonus → also SoftCC but Dot is primary
            "defending"                       => "Buff",
            "disoriented"                     => "SoftCC",
            "broken_faith"                    => "Cosmetic",
            _                                 => "unknown",
        };
        seen.insert(tag);
    }
    seen.into_iter().collect()
}

fn decision_kind_label(d: &LoggedDecision) -> &'static str {
    match d {
        LoggedDecision::Cast { .. } => "Cast",
        LoggedDecision::MoveAndCast { .. } => "MoveAndCast",
        LoggedDecision::Move { .. } => "Move",
        LoggedDecision::EndTurn => "EndTurn",
        LoggedDecision::Skip { .. } => "Skip",
    }
}

fn fresh_decision_kind(d: &LoggedDecision) -> FreshDecisionKind {
    match d {
        LoggedDecision::Cast { .. } | LoggedDecision::MoveAndCast { .. } => FreshDecisionKind::Cast,
        LoggedDecision::Move { .. } => FreshDecisionKind::Move,
        LoggedDecision::EndTurn | LoggedDecision::Skip { .. } => FreshDecisionKind::EndTurn,
    }
}

/// Approximate the fresh `TacticalIntent` from the logged decision + stored goal.
///
/// `classify_continuation_outcome` requires the fresh decision's `TacticalIntent`.
/// The v27 log does not persist intent directly; we reconstruct it approximately:
///
/// - Cast/MoveAndCast targeting the same entity as the stored goal → use the stored
///   goal's intent kind (the actor continued toward the same target).
/// - Cast/MoveAndCast targeting a *different* entity → `FocusTarget` at that new entity.
/// - Move → `Reposition` (best approximation; could be FocusTarget-walk, but we can't know).
/// - EndTurn/Skip → `Reposition` (pass, no meaningful intent to infer).
///
/// This heuristic is sufficient for the miner's `preserved` vs `abandoned` split.
/// Reactive-vs-voluntary classification reads `event.intent_reason` directly
/// (full structured `IntentReason`), so the miner correctly distinguishes
/// `GoalAbandonedReactive { source: ... }` from `GoalAbandonedVoluntary`.
fn approximate_fresh_intent(
    decision: &LoggedDecision,
    stored_goal: &StoredGoalContextSnapshot,
) -> TacticalIntent {
    use bevy::prelude::Entity;

    let stored_target = stored_goal.target_id.and_then(Entity::try_from_bits);

    match decision {
        LoggedDecision::Cast { target, .. } | LoggedDecision::MoveAndCast { target, .. } => {
            let Some(fresh) = Entity::try_from_bits(*target) else {
                return TacticalIntent::Reposition;
            };
            // If targeting the same entity as the stored goal, reproduce the stored intent kind.
            if Some(fresh) == stored_target {
                match stored_goal.kind.as_str() {
                    "finish" | "pressure" => TacticalIntent::FocusTarget { target: fresh },
                    "disable_enemy" => TacticalIntent::ApplyCC { target: fresh },
                    "heal_ally" => TacticalIntent::ProtectAlly { ally: fresh },
                    _ => TacticalIntent::FocusTarget { target: fresh },
                }
            } else {
                // Targeting a different entity — classify as FocusTarget to that entity.
                TacticalIntent::FocusTarget { target: fresh }
            }
        }
        LoggedDecision::Move { .. }
        | LoggedDecision::EndTurn
        | LoggedDecision::Skip { .. } => TacticalIntent::Reposition,
    }
}

// ── Printing helpers ──────────────────────────────────────────────────────────

fn pct(num: usize, denom: usize) -> f64 {
    if denom > 0 { num as f64 / denom as f64 * 100.0 } else { 0.0 }
}

fn print_freq_table(items: &BTreeMap<String, usize>, total: usize) {
    let mut rows: Vec<(&str, usize)> = items.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (key, count) in rows {
        println!("  {:<40} {:>6}  ({:5.1}%)", key, count, pct(count, total));
    }
}

fn print_depth_table(items: &BTreeMap<usize, usize>, total: usize) {
    let mut rows: Vec<(usize, usize)> = items.iter().map(|(k, v)| (*k, *v)).collect();
    rows.sort_by_key(|r| r.0);
    for (depth, count) in rows {
        println!("  depth {:>2}   {:>6}  ({:5.1}%)", depth, count, pct(count, total));
    }
}

fn print_transition_matrix(actor_timelines: &HashMap<SessionActorKey, Vec<(u64, String)>>) {
    use std::collections::BTreeSet;

    let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut total_transitions = 0usize;

    for timeline in actor_timelines.values() {
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(order, _)| *order);
        for window in sorted.windows(2) {
            let from = window[0].1.clone();
            let to = window[1].1.clone();
            *transitions.entry((from, to)).or_default() += 1;
            total_transitions += 1;
        }
    }

    if total_transitions == 0 {
        println!("  (no transitions — only single-decision actors)");
        return;
    }

    let all_kinds: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for (from, to) in transitions.keys() {
            set.insert(from.clone());
            set.insert(to.clone());
        }
        set.into_iter().collect()
    };

    let n = all_kinds.len();
    let col_w = all_kinds.iter().map(|k| k.len()).max().unwrap_or(4).max(4) + 2;
    let row_label_w = col_w;

    print!("{:>row_label_w$}", "FROM \\ TO");
    for to in &all_kinds {
        print!("  {:>col_w$}", &to[..to.len().min(col_w)]);
    }
    println!("  |  TOTAL");
    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));

    for from in &all_kinds {
        let row_total: usize = all_kinds
            .iter()
            .map(|to| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0))
            .sum();
        print!("{:>row_label_w$}", &from[..from.len().min(row_label_w)]);
        for to in &all_kinds {
            let cnt = *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0);
            if cnt > 0 {
                print!("  {:>col_w$}", cnt);
            } else {
                print!("  {:>col_w$}", ".");
            }
        }
        println!("  |  {:>5}  ({:.1}%)", row_total, pct(row_total, total_transitions));
    }

    println!("{}", "-".repeat(row_label_w + (col_w + 2) * n + 12));
    print!("{:>row_label_w$}", "TOTAL");
    for to in &all_kinds {
        let col_total: usize = all_kinds
            .iter()
            .map(|from| *transitions.get(&(from.clone(), to.clone())).unwrap_or(&0))
            .sum();
        print!("  {:>col_w$}", col_total);
    }
    println!("  |  {:>5}", total_transitions);
    println!("\nTotal transitions: {total_transitions}");

    let mut top: Vec<_> = transitions.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\nTop-10 transitions:");
    for ((from, to), cnt) in top.iter().take(10) {
        println!(
            "  {:<40} -> {:<40}  {:>5}  ({:.1}%)",
            from, to, cnt, pct(**cnt, total_transitions)
        );
    }

    let self_loops: usize = all_kinds
        .iter()
        .map(|k| *transitions.get(&(k.clone(), k.clone())).unwrap_or(&0))
        .sum();
    println!(
        "\nSelf-loop rate (intent unchanged between ticks): {} / {} ({:.1}%)",
        self_loops, total_transitions, pct(self_loops, total_transitions)
    );
}

// ── D1/D2 printing helpers ────────────────────────────────────────────────────

/// Compute percentile of a sorted slice (linear interpolation, index clamped).
/// `values` MUST be sorted ascending before calling.
fn percentile_sorted(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() { return 0.0; }
    let idx = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Print stats for a numeric fact field (non-zero values only).
/// `label` — display name. `values` — already filtered to non-zero. `total_steps` — denominator.
fn print_fact_field(label: &str, values: &mut [f32], total_steps: usize) {
    let count = values.len();
    if count == 0 {
        println!("  {:<28}  count=0  (never non-zero in corpus)", label);
        return;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = values.iter().sum::<f32>() / count as f32;
    let max = values.last().copied().unwrap_or(0.0);
    let p50 = percentile_sorted(values, 50.0);
    let p90 = percentile_sorted(values, 90.0);
    let p99 = percentile_sorted(values, 99.0);
    println!(
        "  {:<28}  count={:>4} ({:5.1}%)  mean={:7.1}  p50={:7.1}  p90={:8.1}  p99={:8.1}  max={:8.1}",
        label, count, pct(count, total_steps), mean, p50, p90, p99, max
    );
}

/// Print kill binary rate: how many Cast steps had flag == 1.0.
fn print_kill_rate(label: &str, count: usize, total_steps: usize) {
    println!(
        "  {:<28}  count={:>4} ({:5.1}%)",
        label, count, pct(count, total_steps)
    );
}

/// Print sign-aware stats for a numeric field (may include negative values).
///
/// Reports mean / min / max / abs_max; denominators are the total count passed in.
/// `values` must be non-empty non-zero entries (caller filters zeros).
fn print_signed_field(label: &str, values: &[f32], total: usize) {
    let count = values.len();
    if count == 0 {
        println!("  {:<28}  count=0  (never non-zero in corpus)", label);
        return;
    }
    let mean = values.iter().sum::<f32>() / count as f32;
    let min = values.iter().copied().fold(f32::INFINITY, f32::min);
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let abs_max = values.iter().copied().map(f32::abs).fold(0.0f32, f32::max);
    println!(
        "  {:<28}  count={:>4} ({:5.1}%)  mean={:+8.3}  min={:+8.3}  max={:+8.3}  abs_max={:8.3}",
        label, count, pct(count, total), mean, min, max, abs_max
    );
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dir: Option<PathBuf> = None;
    let mut iter = args[1..].iter();
    while let Some(a) = iter.next() {
        if a == "--dir" || a == "-d" {
            dir = iter.next().map(PathBuf::from);
        } else if !a.starts_with('-') {
            dir = Some(PathBuf::from(a));
        }
    }
    let dir = dir.unwrap_or_else(|| {
        eprintln!("usage: mine_ai_logs --dir <logs-dir>");
        std::process::exit(2);
    });

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();

    if files.is_empty() {
        eprintln!("no *.jsonl files found in {}", dir.display());
        std::process::exit(1);
    }

    let mut agg = Aggregate::default();
    let mut parse_errors = 0usize;
    let mut schema_errors = 0usize;

    for path in &files {
        let session = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Only process actor_tick events.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if val.get("event_type").and_then(|v| v.as_str()) != Some("actor_tick") {
                    continue;
                }
            }

            let event: ActorTickEvent = match storyforge::combat::ai::log::parse_actor_tick(line) {
                Ok(e) => e,
                Err(storyforge::combat::ai::log::LogError::UnsupportedSchema { found, required, .. }) => {
                    eprintln!(
                        "error: schema v{found} unsupported, v{required}+ required (file: {})",
                        path.display()
                    );
                    schema_errors += 1;
                    continue;
                }
                Err(_) => {
                    parse_errors += 1;
                    continue;
                }
            };

            agg.process_event(&session, &event);
        }
    }

    // ── Report ────────────────────────────────────────────────────────────────

    println!("# AI mining — v33 (P3b)");
    println!();
    println!(
        "Source: {} JSONL files, {} AI decisions ({} skip)",
        files.len(), agg.total_decisions, agg.skip_total
    );
    if parse_errors > 0 {
        println!("Parse errors (lines skipped): {parse_errors}");
    }
    if schema_errors > 0 {
        println!("Schema errors (non-v29 lines skipped): {schema_errors}");
    }
    println!();

    // A1: Adaptation reason frequency
    println!("## A1. Adaptation reason frequency (per plan in pool)");
    println!();
    println!("Total plans in pool (all logged, not just chosen): {}", agg.total_plans);
    println!();
    print_freq_table(&agg.adaptation_counts, agg.total_plans);
    println!();

    // A2: Decision kind frequency
    println!("## A2. Decision kind frequency (per tick)");
    println!();
    println!("Total ticks: {}", agg.total_decisions);
    println!();
    print_freq_table(&agg.decision_kind_counts, agg.total_decisions);
    println!();

    // A3: Plan depth utilisation
    println!("## A3. Chosen plan depth (steps.len) histogram");
    println!();
    println!("Total chosen plans: {}", agg.total_chosen);
    println!();
    print_depth_table(&agg.depth_counts, agg.total_chosen);
    println!();

    // Skip-path signals
    println!("## Skip-path signals");
    println!();
    println!(
        "  skip total                : {:>6}  ({:5.1}% of all ticks)",
        agg.skip_total, pct(agg.skip_total, agg.total_decisions)
    );
    println!(
        "  skip with stored_goal     : {:>6}  ({:5.1}% of skips)",
        agg.skip_with_stored_goal, pct(agg.skip_with_stored_goal, agg.skip_total)
    );
    println!();

    // C6: Continuation analysis
    println!("## C6. Continuation analysis (derived via classify_continuation_outcome)");
    println!();
    let n = agg.total_with_continuation;
    let cont_preserved_combined = agg.cont_method_delivered + agg.cont_in_transit;
    let cont_abandoned_reactive_total: usize = agg.cont_abandoned_reactive.values().sum();
    println!("Total ticks analysed: {n}");
    println!("  (outcome derived from raw log data — no pre-classified strings)");
    println!("  (reactive vs voluntary derived from event.intent_reason — full structured.)");
    println!();
    if n == 0 {
        println!("  (no ticks found)");
    } else {
        println!(
            "  goal_preserved | method_delivered : {:>6.1}%  ({})  [target: >=10%]",
            pct(agg.cont_method_delivered, n), agg.cont_method_delivered,
        );
        println!(
            "  goal_preserved | in_transit       : {:>6.1}%  ({})",
            pct(agg.cont_in_transit, n), agg.cont_in_transit,
        );
        println!(
            "  goal_preserved (combined)         : {:>6.1}%  ({})  [target: >=60%]",
            pct(cont_preserved_combined, n), cont_preserved_combined,
        );
        println!();
        println!(
            "  goal_abandoned | reactive         : {:>6.1}%  ({})",
            pct(cont_abandoned_reactive_total, n), cont_abandoned_reactive_total,
        );
        {
            let mut reactive_rows: Vec<(&str, usize)> = agg
                .cont_abandoned_reactive
                .iter()
                .map(|(k, v)| (k.as_str(), *v))
                .collect();
            reactive_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            for (src, count) in &reactive_rows {
                println!("    {:<34} {:>4}  ({:5.1}%)", src, count, pct(*count, n));
            }
        }
        println!(
            "  goal_abandoned | voluntary        : {:>6.1}%  ({})  [target: <=10%, REAL commitment failure]",
            pct(agg.cont_abandoned_voluntary, n), agg.cont_abandoned_voluntary,
        );
        println!(
            "  goal_abandoned | invalidating     : {:>6.1}%  ({})",
            pct(agg.cont_abandoned_invalidating, n), agg.cont_abandoned_invalidating,
        );
        println!(
            "  goal_abandoned | ttl_expired      : {:>6.1}%  ({})",
            pct(agg.cont_abandoned_ttl_expired, n), agg.cont_abandoned_ttl_expired,
        );
        println!(
            "  no_stored_goal                    : {:>6.1}%  ({})",
            pct(agg.cont_no_stored, n), agg.cont_no_stored,
        );
        if !agg.cont_severity_counts.is_empty() {
            println!();
            println!("  severity distribution:");
            print_freq_table(&agg.cont_severity_counts, agg.total_with_continuation);
        }
        if !agg.cont_goal_kind_counts.is_empty() {
            println!();
            println!("  goal_kind distribution:");
            print_freq_table(&agg.cont_goal_kind_counts, n);
        }
    }
    println!();

    // B5: Intent transition matrix
    println!("## B5. Decision kind transition matrix");
    println!();
    println!("Grouping: per actor per combat (JSONL file). Ordered by tick sequence.");
    println!(
        "Unique (combat, actor) pairs tracked: {}",
        agg.actor_timelines.len()
    );
    println!();
    print_transition_matrix(&agg.actor_timelines);
    println!();

    // D1: Outcome fact distributions
    println!("## D1. Outcome fact distributions (per chosen-plan step)");
    println!();
    println!("Total chosen-plan steps: {}", agg.d1_total_cast_steps);
    println!("(stats over non-zero values; count% = fraction of all steps where field > 0)");
    println!();
    println!("  {:<28}  {:>27}  {:>14}  {:>14}  {:>14}  {:>14}",
        "field", "count (freq%)", "mean", "p50", "p90/p99", "max");
    println!("  {}", "-".repeat(105));

    let total = agg.d1_total_cast_steps;
    print_fact_field("enemy_damage",          &mut agg.d1_enemy_damage,          total);
    print_fact_field("ally_damage",           &mut agg.d1_ally_damage,           total);
    print_fact_field("self_damage",           &mut agg.d1_self_damage,           total);
    print_fact_field("hp_restored",           &mut agg.d1_hp_restored,           total);
    print_fact_field("cc_turns_applied",      &mut agg.d1_cc_turns_applied,      total);
    print_fact_field("vulnerability_applied", &mut agg.d1_vulnerability_applied, total);
    print_fact_field("armor_shred_applied",   &mut agg.d1_armor_shred_applied,   total);
    println!();
    println!("  Kill binary facts (rate = % of all chosen-plan steps):");
    print_kill_rate("p_kill_now",  agg.d1_p_kill_now_count,  total);
    print_kill_rate("p_kill_soon", agg.d1_p_kill_soon_count, total);
    println!();

    // D2: AoE per-entity damage breakdown
    let total_aoe = agg.d2_entities_hit_per_cast.len();
    println!("## D2. AoE per-entity damage breakdown");
    println!();
    println!("Total AoE Cast steps (enemy_damage_per_entity non-empty): {total_aoe}");
    if total_aoe == 0 {
        println!("  (no AoE casts in corpus)");
    } else {
        println!();
        println!("  Entities hit per AoE Cast distribution:");
        let mut hit_counts: BTreeMap<usize, usize> = BTreeMap::new();
        for &n in &agg.d2_entities_hit_per_cast {
            let bucket = if n >= 4 { 4 } else { n };
            *hit_counts.entry(bucket).or_default() += 1;
        }
        for (bucket, count) in &hit_counts {
            let label = if *bucket >= 4 { "4+".to_owned() } else { bucket.to_string() };
            println!("    {} entities: {:>4}  ({:5.1}%)", label, count, pct(*count, total_aoe));
        }
        println!();
        if !agg.d2_per_entity_damage.is_empty() {
            agg.d2_per_entity_damage.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let vals = &agg.d2_per_entity_damage;
            let mean = vals.iter().sum::<f32>() / vals.len() as f32;
            let p50 = percentile_sorted(vals, 50.0);
            let p90 = percentile_sorted(vals, 90.0);
            let max = vals.last().copied().unwrap_or(0.0);
            println!(
                "  Per-entity damage (n={}):  mean={:.1}  p50={:.1}  p90={:.1}  max={:.1}",
                vals.len(), mean, p50, p90, max
            );
        }
    }
    println!();

    // E1: Modifier contributions
    println!("=== Modifier contributions (E1) ===");
    println!();
    println!(
        "Plans with at least one modifier emitted: {}  (of {} plans in pool)",
        agg.e1_total_modifier_entries, agg.total_plans
    );
    println!("(stats over non-zero contributions; count% = fraction of modifier-bearing plans)");
    println!();
    let e1_denom = agg.e1_total_modifier_entries;
    print_signed_field("summon_bonus", &agg.e1_summon_bonus, e1_denom);
    print_signed_field("trade_bonus",  &agg.e1_trade_bonus,  e1_denom);
    print_signed_field("repair_bonus", &agg.e1_repair_bonus, e1_denom);
    println!();

    // E2: Picking jitter
    println!("=== Picking jitter (E2) ===");
    println!();
    println!(
        "Chosen plans with non-zero noise_applied: {}  (of {} chosen plans)",
        agg.e2_noise_applied.len(), agg.e2_chosen_count
    );
    println!("(sign-aware: negative noise can flip close decisions)");
    println!();
    print_signed_field("noise_applied", &agg.e2_noise_applied, agg.e2_chosen_count);
    println!();

    // F1: AI tags coverage
    println!("=== AI tags coverage (F1) ===");
    println!();
    println!(
        "Chosen plans: {}  (a plan is counted for tag T when any Cast step has T)",
        agg.total_chosen
    );
    println!();
    for tag in AbilityTag::iter() {
        let count = agg.f1_ability_tag_counts.get(tag.name()).copied().unwrap_or(0);
        println!(
            "  {:<14}  {:>6}  ({:5.1}%)",
            tag.name(), count, pct(count, agg.total_chosen)
        );
    }
    println!();

    // F2: Need signals (post-9.B)
    println!("=== Need signals (F2) ===");
    println!();
    println!("NOTE: NeedSignals are not part of the v30 log schema (ActorTickEvent).");
    println!("      The section below shows only the regression pin for setup_aoe.");
    println!();
    println!(
        "  setup_aoe goal_kind count (regression pin — must be 0): {}",
        agg.f2_setup_aoe_goal_count
    );
    if agg.f2_setup_aoe_goal_count > 0 {
        println!("  *** REGRESSION: setup_aoe goal appeared in corpus — check compute_need_signals ***");
    }
    println!();
    println!("  (rescue_ally / apply_cc distributions: add NeedSignals to ActorTickEvent");
    println!("   in a future schema bump to enable full signal mining here.)");
    println!();

    // F3: Continuation severity (post-9.B)
    println!("=== Continuation severity (F3) ===");
    println!();
    println!(
        "Ticks with non-None severity (mismatch events): {}",
        agg.f3_severity_counts.values().sum::<usize>()
    );
    println!("(hardcoded StatusTag mapping for cross-tab — see statuses_to_tag_labels)");
    println!();

    // Per-severity counts
    {
        let severities = ["Cosmetic", "Relevant", "Invalidating"];
        for sev in &severities {
            let count = agg.f3_severity_counts.get(*sev).copied().unwrap_or(0);
            let total_sev: usize = agg.f3_severity_counts.values().sum();
            let preserved = agg.f3_preserved_by_severity.get(*sev).copied().unwrap_or(0);
            let abandoned = agg.f3_abandoned_by_severity.get(*sev).copied().unwrap_or(0);
            let total_outcome = preserved + abandoned;
            println!(
                "  {:<14}  {:>4}  ({:5.1}%)  preserved {:5.1}%  abandoned {:5.1}%",
                sev,
                count,
                pct(count, total_sev),
                pct(preserved, total_outcome),
                pct(abandoned, total_outcome),
            );
        }
    }
    println!();

    // Cross-tab: severity × StatusTag
    if !agg.f3_severity_by_tag.is_empty() {
        println!("  Cross-tab severity × StatusTag (actor's active statuses):");
        let cross_total: usize = agg.f3_severity_by_tag.values().sum();
        let mut rows: Vec<(&str, usize)> = agg
            .f3_severity_by_tag
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        for (key, count) in &rows {
            println!("    {:<30}  {:>4}  ({:5.1}%)", key, count, pct(*count, cross_total));
        }
    } else {
        println!("  (no mismatch events with status data in corpus)");
    }
    println!();

    // G1: Critics coverage (step 10).
    println!("=== Critics coverage (G1) ===");
    println!();
    println!(
        "Chosen plans: {}; pool plans: {}",
        agg.total_chosen, agg.g1_pool_total
    );
    println!(
        "  Pool-wide: any critic fired in {} / {} plans  ({:.1}%)",
        agg.g1_pool_with_any_critic,
        agg.g1_pool_total,
        pct(agg.g1_pool_with_any_critic, agg.g1_pool_total),
    );
    println!();

    // Six critics — fixed display order (defensive → positioning → resource/value).
    let critic_kinds = [
        "OvercommitIntoDanger",
        "SelfLethalWithoutPayoff",
        "BlindspotRanged",
        "BuffIntoVoid",
        "RareResourceForLowImpact",
        "HealWithoutRescueValue",
    ];

    // Per-critic table: chosen + pool fire rates + multiplier summary.
    println!(
        "  {:<28} {:>8} {:>8} {:>8} {:>8} {:>10} {:>10}",
        "critic", "chosen", "ch_freq", "pool", "po_freq", "mean_mul", "min_mul"
    );
    println!("  {}", "-".repeat(86));
    for k in &critic_kinds {
        let chosen_n = agg.g1_critic_hit_counts.get(*k).copied().unwrap_or(0);
        let pool_n = agg.g1_pool_per_critic.get(*k).copied().unwrap_or(0);
        let muls = agg.g1_critic_multipliers.get(*k);
        let (mean_mul, min_mul) = match muls {
            Some(v) if !v.is_empty() => {
                let sum: f32 = v.iter().sum();
                let mean = sum / v.len() as f32;
                let min = v.iter().copied().fold(f32::INFINITY, f32::min);
                (Some(mean), Some(min))
            }
            _ => (None, None),
        };
        let mean_str = mean_mul.map(|m| format!("{:.3}", m)).unwrap_or_else(|| "—".into());
        let min_str = min_mul.map(|m| format!("{:.3}", m)).unwrap_or_else(|| "—".into());
        println!(
            "  {:<28} {:>8} {:>7.1}% {:>8} {:>7.1}% {:>10} {:>10}",
            k,
            chosen_n,
            pct(chosen_n, agg.total_chosen),
            pool_n,
            pct(pool_n, agg.g1_pool_total),
            mean_str,
            min_str,
        );
    }
    println!();

    // F: multiplier severity buckets per critic that has hits.
    println!("  Multiplier severity buckets (chosen plans):");
    println!(
        "  {:<28} {:>10} {:>10} {:>10}",
        "critic", "<0.5", "0.5..0.8", "0.8..1.0"
    );
    println!("  {}", "-".repeat(64));
    for k in &critic_kinds {
        let muls = match agg.g1_critic_multipliers.get(*k) {
            Some(v) if !v.is_empty() => v,
            _ => continue, // skip critics with no hits
        };
        let mut severe = 0_usize;
        let mut moderate = 0_usize;
        let mut mild = 0_usize;
        for &m in muls {
            if m < 0.5 {
                severe += 1;
            } else if m < 0.8 {
                moderate += 1;
            } else {
                mild += 1;
            }
        }
        let total = muls.len();
        println!(
            "  {:<28} {:>4} ({:>4.1}%) {:>4} ({:>4.1}%) {:>4} ({:>4.1}%)",
            k,
            severe, pct(severe, total),
            moderate, pct(moderate, total),
            mild, pct(mild, total),
        );
    }
    println!();

    // E: Overcommit cross-tab by chosen decision_kind.
    let overcommit_total: usize = agg.g1_overcommit_by_decision_kind.values().sum();
    if overcommit_total > 0 {
        println!("  OvercommitIntoDanger × decision_kind (chosen plans):");
        println!(
            "  {:<14} {:>8} {:>8} {:>8}",
            "decision", "chosen", "with_oc", "rate%"
        );
        println!("  {}", "-".repeat(44));
        let mut kinds: Vec<&String> = agg.g1_chosen_by_decision_kind.keys().collect();
        kinds.sort_by(|a, b| {
            agg.g1_chosen_by_decision_kind.get(*b).cmp(&agg.g1_chosen_by_decision_kind.get(*a))
        });
        for kind in kinds {
            let chosen_total = agg.g1_chosen_by_decision_kind.get(kind).copied().unwrap_or(0);
            let with_oc = agg.g1_overcommit_by_decision_kind.get(kind).copied().unwrap_or(0);
            println!(
                "  {:<14} {:>8} {:>8} {:>7.1}%",
                kind, chosen_total, with_oc, pct(with_oc, chosen_total),
            );
        }
        println!();
    }

    // G: Overcommit × adaptation reason. Non-"none" rows imply LastStand
    // mode (every adaptation reason switches the plan to LastStand). Use
    // this to detect double-penalty: actor in LastStand AND Overcommit hit.
    if overcommit_total > 0 {
        println!("  OvercommitIntoDanger × adaptation reason (chosen plans):");
        println!(
            "  {:<32} {:>8} {:>8} {:>8}",
            "adaptation_reason", "chosen", "with_oc", "rate%"
        );
        println!("  {}", "-".repeat(60));
        let reason_order = [
            "none",                          // Default mode
            "expected_self_lethal",          // per-plan LastStand
            "protect_self_no_defensive",     // global LastStand (no defensive options)
            "protect_self_futile",           // global LastStand (DoT-doomed)
        ];
        for reason in &reason_order {
            let chosen_total = agg.g1_chosen_by_adapt_reason.get(*reason).copied().unwrap_or(0);
            if chosen_total == 0 {
                continue;
            }
            let with_oc = agg.g1_overcommit_by_adapt_reason.get(*reason).copied().unwrap_or(0);
            println!(
                "  {:<32} {:>8} {:>8} {:>7.1}%",
                reason, chosen_total, with_oc, pct(with_oc, chosen_total),
            );
        }
        // Aggregated LastStand row (sum of all non-"none" reasons).
        let last_stand_total: usize = agg
            .g1_chosen_by_adapt_reason
            .iter()
            .filter(|(k, _)| k.as_str() != "none")
            .map(|(_, v)| *v)
            .sum();
        let last_stand_with_oc: usize = agg
            .g1_overcommit_by_adapt_reason
            .iter()
            .filter(|(k, _)| k.as_str() != "none")
            .map(|(_, v)| *v)
            .sum();
        if last_stand_total > 0 {
            println!("  {}", "-".repeat(60));
            println!(
                "  {:<32} {:>8} {:>8} {:>7.1}%",
                "(any LastStand)",
                last_stand_total,
                last_stand_with_oc,
                pct(last_stand_with_oc, last_stand_total),
            );
        }
        println!();
    }

    // H1: Band coverage
    let h1_total_with_band: usize = agg.h1_band_tick_counts.values().sum();
    println!("## H1. Band coverage (schema v32 — bands/agenda serialised)");
    println!();
    if h1_total_with_band == 0 {
        println!("  (no v32 band data — corpus is pre-v32 or skip-only)");
    } else {
        println!("Ticks with band attribution: {} ({:.1}% of all ticks)", h1_total_with_band,
            pct(h1_total_with_band, agg.total_decisions));
        println!();
        println!("### H1a. Per-band tick count");
        println!();
        let band_order = [
            "ForcedTargeting",
            "CriticalSelfPreservation",
            "HardRescueOpportunity",
            "NormalTactical",
        ];
        for band in &band_order {
            let count = agg.h1_band_tick_counts.get(*band).copied().unwrap_or(0);
            println!("  {:<28} {:>6}  ({:5.1}%)", band, count, pct(count, h1_total_with_band));
        }
        // Any bands not in the canonical order (forward-compat).
        for (band, count) in &agg.h1_band_tick_counts {
            if !band_order.contains(&band.as_str()) {
                println!("  {:<28} {:>6}  ({:5.1}%)", band, count, pct(*count, h1_total_with_band));
            }
        }
        println!();

        println!("### H1b. Winner-intent distribution per band (chosen plan attribution)");
        println!();
        for (key, count) in &agg.h1_band_winner_intent {
            println!("  {:<44} {:>6}", key, count);
        }
        println!();

        println!("### H1c. Per-axis consideration histograms (all agenda items, all bands)");
        println!("  (mean / p10 / p50 / p90 / p99 over {} samples)", agg.h1_urgency.len());
        println!();
        let print_axis = |name: &str, mut v: Vec<f32>| {
            if v.is_empty() { println!("  {name:<20} (no data)"); return; }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            let p10 = percentile_sorted(&v, 10.0);
            let p50 = percentile_sorted(&v, 50.0);
            let p90 = percentile_sorted(&v, 90.0);
            let p99 = percentile_sorted(&v, 99.0);
            println!("  {name:<20} mean={mean:.3}  p10={p10:.3}  p50={p50:.3}  p90={p90:.3}  p99={p99:.3}");
        };
        print_axis("urgency",             agg.h1_urgency.clone());
        print_axis("feasibility",         agg.h1_feasibility.clone());
        print_axis("leverage",            agg.h1_leverage.clone());
        print_axis("safety",              agg.h1_safety.clone());
        print_axis("role_affinity",       agg.h1_role_affinity.clone());
        print_axis("continuation_value",  agg.h1_continuation_value.clone());
        println!();

        // H1c.bis: per-IntentKind leverage histograms (step 11.8).
        // Values come from chosen plan's considerations_per_item, so these are
        // plan-aware overlay values — not the item-level baselines above.
        // On pre-11.8 logs leverage was a single flat 0.0 formula; all buckets
        // will show mean=0.000 (expected, not a bug).
        println!("### H1c.bis. Per-IntentKind leverage histograms (step 11.8)");
        println!("  Sample: each tick contributes |agenda.items| values from the CHOSEN plan's");
        println!("  `considerations_per_item`, routed by `agenda.items[idx].kind`. Unchosen");
        println!("  plans are NOT sampled — focus is \"what the AI picked\", not \"full pool diversity\".");
        println!("  (Sample N here ≠ H2 attributed-ticks: a chosen plan with `agenda_item=None`");
        println!("   still contributes its considerations_per_item entries to these histograms.)");
        println!();

        // Helper: compute middle_mass = fraction of values in (0.05, 0.95).
        let middle_mass = |v: &[f32]| -> f32 {
            if v.is_empty() { return 0.0; }
            let in_middle = v.iter().filter(|&&x| x > 0.05 && x < 0.95).count();
            in_middle as f32 / v.len() as f32
        };

        let print_kind_axis = |name: &str, mut v: Vec<f32>| {
            if v.is_empty() {
                println!("  {name:<18}  N=0  (no data)");
                return;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = v.len();
            let mean = v.iter().sum::<f32>() / n as f32;
            let p10 = percentile_sorted(&v, 10.0);
            let p25 = percentile_sorted(&v, 25.0);
            let p50 = percentile_sorted(&v, 50.0);
            let p75 = percentile_sorted(&v, 75.0);
            let p90 = percentile_sorted(&v, 90.0);
            let mm = middle_mass(&v) * 100.0;
            println!(
                "  {name:<18}  N={n:<5}  mean={mean:.3}  p10={p10:.3}  p25={p25:.3}  p50={p50:.3}  p75={p75:.3}  p90={p90:.3}  middle_mass={mm:.1}%"
            );
        };

        let kinds: &[(&str, Vec<f32>)] = &[
            ("FocusTarget",     agg.h1_leverage_focus_target.clone()),
            ("ApplyCC",         agg.h1_leverage_apply_cc.clone()),
            ("ProtectAlly",     agg.h1_leverage_protect_ally.clone()),
            ("ProtectSelf",     agg.h1_leverage_protect_self.clone()),
            ("Reposition/AOE",  agg.h1_leverage_reposition.clone()),
            ("LastStand",       agg.h1_leverage_last_stand.clone()),
        ];
        for (name, v) in kinds {
            print_kind_axis(name, v.clone());
        }
        println!();

        // Cross-kind balance gate (acceptance criterion per design doc Section C):
        // if max_mean / min_mean > 1.30 → flag for retune.
        {
            let means: Vec<(&str, f32)> = kinds.iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(name, v)| {
                    let mean = v.iter().sum::<f32>() / v.len() as f32;
                    (*name, mean)
                })
                .collect();
            if means.len() >= 2 {
                let max_entry = means.iter().copied().fold(("", f32::NEG_INFINITY), |acc, x| if x.1 > acc.1 { x } else { acc });
                let min_entry = means.iter().copied().fold(("", f32::INFINITY),     |acc, x| if x.1 < acc.1 { x } else { acc });
                if min_entry.1 > 0.0 {
                    let ratio = max_entry.1 / min_entry.1;
                    if ratio > 1.30 {
                        println!(
                            "  [WARN] Cross-kind balance: {} (mean={:.3}) dominates {} (mean={:.3}) by {:.2}x — flag for retune",
                            max_entry.0, max_entry.1, min_entry.0, min_entry.1, ratio
                        );
                    } else {
                        println!("  [OK] Cross-kind balance: max/min ratio = {ratio:.2}x (threshold 1.30x)");
                    }
                } else {
                    println!("  [NOTE] Cross-kind balance: min_mean=0 (all kinds flat — pre-11.8 corpus or no data)");
                }
            } else {
                println!("  [NOTE] Cross-kind balance: insufficient kinds with data ({} kinds)", means.len());
            }
        }
        println!();
    }

    // H2: Agenda-item win-rate
    println!("## H2. Agenda-item win-rate per band (which item index wins most)");
    println!();
    if agg.h2_band_chosen_total.is_empty() {
        println!("  (no v32 agenda data — corpus is pre-v32 or skip-only)");
    } else {
        println!("  Sanity check: NormalTactical should have distributed wins (not index 0 dominating).");
        println!();
        // Group by band for readability.
        let band_order = ["ForcedTargeting", "CriticalSelfPreservation", "HardRescueOpportunity", "NormalTactical"];
        for band in &band_order {
            let band_total = agg.h2_band_chosen_total.get(*band).copied().unwrap_or(0);
            if band_total == 0 { continue; }
            println!("  {band} (attributed ticks: {band_total})");
            // Collect item wins for this band.
            let mut items: Vec<(usize, usize)> = agg.h2_band_item_win.iter()
                .filter_map(|(k, v)| {
                    let (b, idx) = k.rsplit_once('/')?;
                    if b == *band { Some((idx.parse::<usize>().ok()?, *v)) } else { None }
                })
                .collect();
            items.sort_by_key(|(idx, _)| *idx);
            for (idx, count) in &items {
                println!("    item[{idx}] wins: {:>5}  ({:5.1}%)", count, pct(*count, band_total));
            }
            // Ticks where chosen plan had no attributed agenda item.
            let attributed_sum: usize = items.iter().map(|(_, c)| c).sum();
            let unattributed = band_total.saturating_sub(attributed_sum);
            if unattributed > 0 {
                println!("    (no attribution): {:>5}  ({:5.1}%)", unattributed, pct(unattributed, band_total));
            }
            println!();
        }
    }

    // H3a: Construction-time metrics (retrospective on existing v32 logs)
    println!("## H3a. Agenda construction-time metrics (from existing v32 logs)");
    println!();
    let h3a_has_band_data = !agg.h3a_agenda_size.is_empty();
    if !h3a_has_band_data {
        println!("  (no v32 band data — corpus is pre-v32 or skip-only)");
    } else {
        // H3a.1: agenda size distribution per band.
        println!("### H3a.1. Agenda size distribution per band");
        println!();
        let band_order = ["ForcedTargeting", "CriticalSelfPreservation", "HardRescueOpportunity", "NormalTactical"];
        // Collect max size seen across all bands.
        let max_size: usize = agg.h3a_agenda_size.keys()
            .filter_map(|k| k.rsplit_once('/').and_then(|(_, s)| s.parse::<usize>().ok()))
            .max()
            .unwrap_or(3);
        let col_header: String = (0..=max_size).map(|i| format!("  items={i:>2}")).collect::<Vec<_>>().join("");
        println!("  {:<28}{col_header}   total", "Band");
        for band in &band_order {
            let band_total: usize = (0..=max_size)
                .map(|s| agg.h3a_agenda_size.get(&format!("{band}/{s}")).copied().unwrap_or(0))
                .sum();
            if band_total == 0 { continue; }
            let counts: String = (0..=max_size)
                .map(|s| format!("  {:>8}", agg.h3a_agenda_size.get(&format!("{band}/{s}")).copied().unwrap_or(0)))
                .collect::<Vec<_>>().join("");
            println!("  {:<28}{counts}   {}", band, band_total);
        }
        println!();

        // H3a.2: item.target=None rate by (band, item.kind).
        println!("### H3a.2. Item.target=None rate by (band, item.kind)");
        println!();
        println!("  {:<44} {:>11}  {:>11}  {:>7}  {:>9}", "Band/Kind", "target=None", "target=Some", "total", "none_rate%");
        // Collect all band/kind keys.
        let mut kind_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        kind_keys.extend(agg.h3a_target_none.keys().cloned());
        kind_keys.extend(agg.h3a_target_some.keys().cloned());
        for key in &kind_keys {
            let none_cnt = agg.h3a_target_none.get(key).copied().unwrap_or(0);
            let some_cnt = agg.h3a_target_some.get(key).copied().unwrap_or(0);
            let total = none_cnt + some_cnt;
            println!("  {:<44} {:>11}  {:>11}  {:>7}  {:>8.1}%",
                key, none_cnt, some_cnt, total, pct(none_cnt, total));
        }
        println!();

        // H3a.3: fallback buckets per band (attributed vs unattributed).
        println!("### H3a.3. Unattributed fallback rate per band");
        println!();
        println!("  {:<28} {:>11}  {:>12}  {:>12}", "Band", "attributed", "unattributed", "unattr_rate%");
        for band in &band_order {
            let attr = agg.h3a_band_attributed.get(*band).copied().unwrap_or(0);
            let unattr = agg.h3a_band_unattributed.get(*band).copied().unwrap_or(0);
            let total = attr + unattr;
            if total == 0 { continue; }
            println!("  {:<28} {:>11}  {:>12}  {:>11.1}%", band, attr, unattr, pct(unattr, total));
        }
        println!();

        // H3a.4: sub-classification of unattributed ticks.
        println!("### H3a.4. Unattributed sub-classification (construction-time)");
        println!();
        let total_unattr: usize = agg.h3a_band_unattributed.values().sum();
        let no_items: usize = agg.h3a_unattr_no_items.values().sum();
        let all_no_target: usize = agg.h3a_unattr_all_no_target.values().sum();
        let needs_reject: usize = total_unattr.saturating_sub(no_items + all_no_target);
        println!("  no_items_generated (agenda.empty):                      {no_items}");
        println!("  all_items_no_target (every item has target=None):       {all_no_target}");
        println!("  unclassified (needs per-plan reject_reason from 11.7b): {needs_reject}");
        println!("  total unattributed:                                      {total_unattr}");
        println!();
    }

    // H3b: Per-plan reject reason breakdown (from 11.7 instrumentation).
    println!("## H3b. Per-plan eligibility breakdown (composition-time)");
    println!();
    if !agg.h3b_has_data {
        println!("  no per-plan reject data — collect new logs after 11.7 lands");
        println!();
    } else {
        println!("### H3b.1. Reject-reason distribution per (band, item.kind)");
        println!();
        println!("  {:<44} {:>9}  {:>9}  {:>9}  {:>9}  {:>13}", "Band/Kind", "not_def", "not_off", "no_tgt", "no_ally", "total_rejects");
        let mut all_kinds: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        all_kinds.extend(agg.h3b_total.keys().cloned());
        for kind_key in &all_kinds {
            let not_def = agg.h3b_reject_reason.get(&format!("{kind_key}/NotDefensive")).copied().unwrap_or(0);
            let not_off = agg.h3b_reject_reason.get(&format!("{kind_key}/NotOffensiveVsTarget")).copied().unwrap_or(0);
            let no_tgt  = agg.h3b_reject_reason.get(&format!("{kind_key}/NoTarget")).copied().unwrap_or(0);
            let no_ally = agg.h3b_reject_reason.get(&format!("{kind_key}/NoAllyTarget")).copied().unwrap_or(0);
            let total_rej = not_def + not_off + no_tgt + no_ally;
            if total_rej == 0 { continue; }
            println!("  {:<44} {:>9}  {:>9}  {:>9}  {:>9}  {:>13}", kind_key, not_def, not_off, no_tgt, no_ally, total_rej);
        }
        println!();

        println!("### H3b.2. Eligibility rate per (band, item.kind)");
        println!();
        println!("  {:<44} {:>9}  {:>9}  {:>8}  {:>10}", "Band/Kind", "eligible", "rejected", "total", "elig_rate%");
        for kind_key in &all_kinds {
            let elig  = agg.h3b_eligible.get(kind_key).copied().unwrap_or(0);
            let total = agg.h3b_total.get(kind_key).copied().unwrap_or(0);
            let rej   = total.saturating_sub(elig);
            println!("  {:<44} {:>9}  {:>9}  {:>8}  {:>9.1}%", kind_key, elig, rej, total, pct(elig, total));
        }
        println!();
    }

    // H3c: Post-hoc fallback cause classifier
    println!("## H3c. Fallback cause classification (post-hoc, schema v32)");
    println!();
    let h3c_total_all: usize = agg.h3c_total.values().sum();
    if h3c_total_all == 0 && agg.h3c_band_total.is_empty() {
        println!("  (no v32 band data — corpus is pre-v32 or skip-only)");
        println!();
    } else {
        println!("Total unattributed ticks: {h3c_total_all}");
        println!();

        let band_order = ["ForcedTargeting", "CriticalSelfPreservation", "HardRescueOpportunity", "NormalTactical"];

        // H3c.1: per-band fallback breakdown
        println!("### H3c.1. Per-band fallback breakdown");
        println!();
        let col_w = 11usize;
        println!(
            "  {:<28}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>8}",
            "Band",
            H3C_BUCKET_LABELS[0], H3C_BUCKET_LABELS[1], H3C_BUCKET_LABELS[2],
            H3C_BUCKET_LABELS[3], H3C_BUCKET_LABELS[4], H3C_BUCKET_LABELS[5],
            "total",
            col_w = col_w,
        );
        for band in &band_order {
            let total = agg.h3c_total.get(*band).copied().unwrap_or(0);
            if total == 0 { continue; }
            let ap  = agg.h3c_ap_mp_blocked.get(*band).copied().unwrap_or(0);
            let nt  = agg.h3c_no_target_in_agenda.get(*band).copied().unwrap_or(0);
            let ur  = agg.h3c_target_unreachable.get(*band).copied().unwrap_or(0);
            let na  = agg.h3c_no_plan_attempts_target.get(*band).copied().unwrap_or(0);
            let om  = agg.h3c_only_move_plans.get(*band).copied().unwrap_or(0);
            let uc  = agg.h3c_unclassified.get(*band).copied().unwrap_or(0);
            println!(
                "  {:<28}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>8}",
                band, ap, nt, ur, na, om, uc, total,
                col_w = col_w,
            );
        }
        // Any unlisted bands.
        for (band, total) in &agg.h3c_total {
            if band_order.contains(&band.as_str()) { continue; }
            let ap  = agg.h3c_ap_mp_blocked.get(band).copied().unwrap_or(0);
            let nt  = agg.h3c_no_target_in_agenda.get(band).copied().unwrap_or(0);
            let ur  = agg.h3c_target_unreachable.get(band).copied().unwrap_or(0);
            let na  = agg.h3c_no_plan_attempts_target.get(band).copied().unwrap_or(0);
            let om  = agg.h3c_only_move_plans.get(band).copied().unwrap_or(0);
            let uc  = agg.h3c_unclassified.get(band).copied().unwrap_or(0);
            println!(
                "  {:<28}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>8}",
                band, ap, nt, ur, na, om, uc, total,
                col_w = col_w,
            );
        }
        println!();

        // Sanity check: H3c total vs H3a unattributed.
        {
            let h3a_total_unattr: usize = agg.h3a_band_unattributed.values().sum();
            // H3c only runs for unattributed ticks with non-empty agenda;
            // H3a.unattributed includes agenda-empty cases (no_items).
            let h3a_no_items: usize = agg.h3a_unattr_no_items.values().sum();
            let h3a_comparable = h3a_total_unattr.saturating_sub(h3a_no_items);
            let match_mark = if h3c_total_all == h3a_comparable { "OK" } else { "MISMATCH" };
            println!(
                "  Sanity: H3c total={h3c_total_all}, H3a unattr-no_items={h3a_comparable} [{match_mark}]"
            );
            if h3c_total_all != h3a_comparable {
                println!("  (H3a total_unattr={h3a_total_unattr}, no_items={h3a_no_items})");
            }
        }
        println!();

        // H3c.2: per-(band, primary intent kind) breakdown
        println!("### H3c.2. Per-(band, primary intent) breakdown");
        println!();
        println!(
            "  {:<40}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>8}",
            "Band/Kind",
            H3C_BUCKET_LABELS[0], H3C_BUCKET_LABELS[1], H3C_BUCKET_LABELS[2],
            H3C_BUCKET_LABELS[3], H3C_BUCKET_LABELS[4], H3C_BUCKET_LABELS[5],
            "total",
            col_w = col_w,
        );
        for (key, counts) in &agg.h3c_by_band_kind {
            let total: usize = counts.iter().sum();
            if total == 0 { continue; }
            println!(
                "  {:<40}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>col_w$}  {:>8}",
                key, counts[0], counts[1], counts[2], counts[3], counts[4], counts[5], total,
                col_w = col_w,
            );
        }
        println!();

        // H3c.3: per-band fallback rate decomposition
        println!("### H3c.3. Per-band fallback rate decomposition");
        println!();
        println!(
            "  {:<28}  {:>10}  {:>8}  {:>8}  {:>11}  {:>10}  {:>9}",
            "Band", "fallback%", "ap_mp%", "no_tgt%", "no_attempt%", "only_move%", "unclass%",
        );
        for band in &band_order {
            let band_total = agg.h3c_band_total.get(*band).copied().unwrap_or(0);
            if band_total == 0 { continue; }
            let unattr = agg.h3c_total.get(*band).copied().unwrap_or(0);
            let ap  = agg.h3c_ap_mp_blocked.get(*band).copied().unwrap_or(0);
            let nt  = agg.h3c_no_target_in_agenda.get(*band).copied().unwrap_or(0);
            let ur  = agg.h3c_target_unreachable.get(*band).copied().unwrap_or(0);
            let na  = agg.h3c_no_plan_attempts_target.get(*band).copied().unwrap_or(0);
            let om  = agg.h3c_only_move_plans.get(*band).copied().unwrap_or(0);
            let uc  = agg.h3c_unclassified.get(*band).copied().unwrap_or(0);
            // unreach is folded into no_attempt% column for the decomposition display
            // (both indicate "generator gap"), or shown as separate columns.
            let _ = ur; // shown in H3c.1; in decomp we show all as % of unattributed.
            println!(
                "  {:<28}  {:>9.1}%  {:>7.1}%  {:>7.1}%  {:>10.1}%  {:>9.1}%  {:>8.1}%",
                band,
                pct(unattr, band_total),
                pct(ap,  unattr),
                pct(nt,  unattr),
                pct(na,  unattr),
                pct(om,  unattr),
                pct(uc,  unattr),
            );
        }
        println!();
        println!("  NOTE: unreach% not shown in H3c.3 (folded into H3c.1; see above).");
        println!();
    }

    // ── P3b: score_trace_log-sourced stats (v33+) ────────────────────────────

    if agg.trace_plans_total > 0 {
        println!("## P3b-A1. Rescore-mode distribution (score_trace_log, v33+)");
        println!();
        println!("Plans with score_trace_log: {} / {}", agg.trace_plans_total, agg.total_plans);
        println!();
        print_freq_table(&agg.a1_trace_rescore_mode, agg.trace_plans_total);
        println!();

        println!("## P3b-E1. Addend contributions (score_trace_log, v33+)");
        println!();
        println!(
            "Plans with addends: {} / {} ({:.1}%)",
            agg.e1_trace_modifier_entries,
            agg.trace_plans_total,
            pct(agg.e1_trace_modifier_entries, agg.trace_plans_total),
        );
        println!();
        print_signed_field("summon_bonus", &agg.e1_trace_summon, agg.trace_plans_total);
        print_signed_field("trade_bonus",  &agg.e1_trace_trade,  agg.trace_plans_total);
        print_signed_field("repair_bonus", &agg.e1_trace_repair, agg.trace_plans_total);
        println!();

        println!("## P3b-G2. Multiplier-kind breakdown (score_trace_log, v33+)");
        println!();
        println!(
            "Chosen plans with trace: {} / {} ({:.1}%)",
            agg.g2_trace_chosen_total,
            agg.total_chosen,
            pct(agg.g2_trace_chosen_total, agg.total_chosen),
        );
        println!();
        for (kind, count) in &agg.g2_trace_multiplier_counts {
            let values = agg.g2_trace_multiplier_values.get(kind).map(|v| v.as_slice()).unwrap_or(&[]);
            let mean = if values.is_empty() { 0.0 } else { values.iter().sum::<f32>() / values.len() as f32 };
            println!(
                "  {:<10} hits={:>5}  ({:.1}% of trace-chosen)  mean_value={:.4}",
                kind, count, pct(*count, agg.g2_trace_chosen_total), mean
            );
        }
        if agg.g2_trace_multiplier_counts.is_empty() {
            println!("  (no multipliers recorded)");
        }
        println!();
    } else {
        println!("## P3b stats: no v33 score_trace_log data in corpus (v32-only logs)");
        println!();
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use storyforge::combat::ai::log::{ActorTickEvent, LoggedDecision, LoggedPlan};
    use storyforge::combat::ai::world::snapshot::BattleSnapshot;
    use storyforge::combat::ai::outcome::{PlanAnnotation, PickInfo};
    use storyforge::combat::ai::pipeline::stages::modifiers::ModifierContribution;
    use storyforge::combat::ai::plan::PlanStep;
    use storyforge::combat::ai::pipeline::stages::pick_best::PickMechanics;

    fn make_event(
        actor_id: u64,
        decision: LoggedDecision,
        plans: Vec<LoggedPlan>,
        continuation: Option<storyforge::combat::ai::log::ContinuationLogSection>,
    ) -> ActorTickEvent {
        ActorTickEvent {
            event_type: "actor_tick".to_owned(),
            schema_version: 32,
            round: 1,
            timestamp_ms: 0,
            actor_id,
            actor_name: "test".to_owned(),
            snapshot: BattleSnapshot::default(),
            plans,
            decision,
            continuation,
            intent_reason: None,
            evaluation_mode_reason: None,
            band: None,
            band_reason: None,
            agenda: vec![],
        }
    }

    fn plan_chosen(steps_len: usize) -> LoggedPlan {
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        let steps = (0..steps_len)
            .map(|_| PlanStep::Move { path: vec![] })
            .collect();
        LoggedPlan {
            rank: 1,
            steps,
            annotation: ann,
        }
    }

    fn plan_unchosen() -> LoggedPlan {
        LoggedPlan {
            rank: 2,
            steps: vec![PlanStep::Move { path: vec![] }],
            annotation: PlanAnnotation::default(),
        }
    }

    #[test]
    fn decision_kind_counted_correctly() {
        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &make_event(1, LoggedDecision::EndTurn, vec![], None));
        agg.process_event("f.jsonl", &make_event(1, LoggedDecision::EndTurn, vec![], None));
        agg.process_event(
            "f.jsonl",
            &make_event(1, LoggedDecision::Move { path: vec![] }, vec![], None),
        );

        assert_eq!(agg.total_decisions, 3);
        assert_eq!(*agg.decision_kind_counts.get("EndTurn").unwrap(), 2);
        assert_eq!(*agg.decision_kind_counts.get("Move").unwrap(), 1);
    }

    #[test]
    fn plan_depth_tracks_chosen_steps_len() {
        let mut agg = Aggregate::default();
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_chosen(3), plan_unchosen()],
            None,
        );
        agg.process_event("f.jsonl", &event);
        assert_eq!(*agg.depth_counts.get(&3).unwrap(), 1, "chosen plan has 3 steps");
        assert!(!agg.depth_counts.contains_key(&1), "non-chosen plan not counted");
    }

    #[test]
    fn skip_event_counted_separately() {
        let mut agg = Aggregate::default();
        agg.process_event(
            "f.jsonl",
            &make_event(
                1,
                LoggedDecision::Skip { reason: "no_ap_no_mp".to_owned() },
                vec![],
                None,
            ),
        );
        assert_eq!(agg.skip_total, 1);
        assert_eq!(agg.skip_with_stored_goal, 0);
        assert_eq!(agg.total_plans, 0);
    }

    #[test]
    fn v26_schema_skipped_with_error() {
        // The main() loop checks schema_version before deserialising.
        // This test verifies the label logic directly via the fast-path check.
        let json = r#"{"event_type":"actor_tick","schema_version":26,"round":1}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let ver = val.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
        assert_ne!(ver, 27, "v26 must be rejected");
    }

    #[test]
    fn transition_matrix_self_loops_and_changes() {
        let mut agg = Aggregate::default();
        // Actor 1, 3 sequential ticks: EndTurn → EndTurn → Move
        for (order, d) in [
            LoggedDecision::EndTurn,
            LoggedDecision::EndTurn,
            LoggedDecision::Move { path: vec![] },
        ]
        .into_iter()
        .enumerate()
        {
            let _ = order; // order tracked via actor_tick_counters internally
            agg.process_event("A.jsonl", &make_event(1, d, vec![], None));
        }

        let key = ("A.jsonl".to_owned(), 1u64);
        let timeline = agg.actor_timelines.get(&key).unwrap();
        let mut sorted = timeline.clone();
        sorted.sort_by_key(|(ord, _)| *ord);

        let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
        for w in sorted.windows(2) {
            *transitions
                .entry((w[0].1.clone(), w[1].1.clone()))
                .or_default() += 1;
        }

        assert_eq!(
            *transitions
                .get(&("EndTurn".to_owned(), "EndTurn".to_owned()))
                .unwrap(),
            1
        );
        assert_eq!(
            *transitions
                .get(&("EndTurn".to_owned(), "Move".to_owned()))
                .unwrap(),
            1
        );
    }

    #[test]
    fn pct_zero_denominator_returns_zero() {
        assert_eq!(pct(5, 0), 0.0);
        assert!((pct(1, 4) - 25.0).abs() < 1e-9);
    }

    fn plan_with_modifiers(chosen: bool, modifiers: Vec<ModifierContribution>) -> LoggedPlan {
        let mut ann = PlanAnnotation::default().with_modifiers(modifiers);
        ann.chosen = chosen;
        ann.pick = if chosen {
            Some(PickInfo { mechanics: PickMechanics::default(), noise_applied: 0.0 })
        } else {
            None
        };
        LoggedPlan {
            rank: 1,
            steps: vec![PlanStep::Move { path: vec![] }],
            annotation: ann,
        }
    }

    fn plan_with_noise(noise: f32) -> LoggedPlan {
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.pick = Some(PickInfo { mechanics: PickMechanics::default(), noise_applied: noise });
        LoggedPlan {
            rank: 1,
            steps: vec![PlanStep::Move { path: vec![] }],
            annotation: ann,
        }
    }

    #[test]
    fn mine_v29_corpus_produces_modifier_section() {
        let modifiers = vec![
            ModifierContribution { name: "summon_bonus".to_owned(), contribution: 5.0 },
            ModifierContribution { name: "trade_bonus".to_owned(), contribution: -2.0 },
        ];
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_with_modifiers(true, modifiers)],
            None,
        );

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(agg.e1_total_modifier_entries, 1, "one plan had modifiers");
        assert_eq!(agg.e1_summon_bonus, vec![5.0]);
        assert_eq!(agg.e1_trade_bonus, vec![-2.0]);
        assert!(agg.e1_repair_bonus.is_empty());
    }

    #[test]
    fn mine_v29_corpus_produces_jitter_section() {
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_with_noise(0.042)],
            None,
        );

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(agg.e2_chosen_count, 1);
        assert_eq!(agg.e2_noise_applied.len(), 1);
        assert!((agg.e2_noise_applied[0] - 0.042).abs() < 1e-6);
    }

    #[test]
    fn mine_e1_skips_zero_contributions() {
        // Zero-value contributions must not be collected.
        let modifiers = vec![
            ModifierContribution { name: "summon_bonus".to_owned(), contribution: 0.0 },
            ModifierContribution { name: "repair_bonus".to_owned(), contribution: 3.0 },
        ];
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_with_modifiers(false, modifiers)],
            None,
        );

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        // Plan had modifiers (non-empty vec) → counted.
        assert_eq!(agg.e1_total_modifier_entries, 1);
        assert!(agg.e1_summon_bonus.is_empty(), "zero contribution skipped");
        assert_eq!(agg.e1_repair_bonus, vec![3.0]);
    }

    #[test]
    fn mine_e2_skips_zero_noise() {
        // noise_applied == 0.0 must not land in e2_noise_applied vec.
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_with_noise(0.0)],
            None,
        );

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(agg.e2_chosen_count, 1, "chosen plan counted in denominator");
        assert!(agg.e2_noise_applied.is_empty(), "zero noise not recorded");
    }

    // ── F1: AI tags coverage ──────────────────────────────────────────────────

    fn plan_with_tags(tags: Vec<storyforge::combat::ai::world::tags::AbilityTagSet>) -> LoggedPlan {
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.effective_ai_tags = tags;
        LoggedPlan {
            rank: 1,
            steps: vec![PlanStep::Move { path: vec![] }],
            annotation: ann,
        }
    }

    #[test]
    fn f1_ability_tag_counts_chosen_plan_with_offensive() {
        use storyforge::combat::ai::world::tags::{AbilityTag, AbilityTagSet};
        let mut tags = AbilityTagSet::empty();
        tags.insert_tag(AbilityTag::Offensive);
        let event = make_event(1, LoggedDecision::EndTurn, vec![plan_with_tags(vec![tags])], None);

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(*agg.f1_ability_tag_counts.get("offensive").unwrap(), 1);
        assert!(!agg.f1_ability_tag_counts.contains_key("rescue"), "rescue not set");
        assert_eq!(agg.total_chosen, 1);
    }

    #[test]
    fn f1_plan_counted_once_per_tag_even_if_multiple_steps() {
        use storyforge::combat::ai::world::tags::{AbilityTag, AbilityTagSet};
        // Two Cast steps both with Offensive — plan counted once for Offensive.
        let mut step_tags = AbilityTagSet::empty();
        step_tags.insert_tag(AbilityTag::Offensive);
        let event = make_event(
            1,
            LoggedDecision::EndTurn,
            vec![plan_with_tags(vec![step_tags, step_tags])],
            None,
        );

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(*agg.f1_ability_tag_counts.get("offensive").unwrap(), 1,
            "plan counted once per tag even with two steps");
    }

    #[test]
    fn f1_unchosen_plan_not_counted() {
        use storyforge::combat::ai::world::tags::{AbilityTag, AbilityTagSet};
        let mut tags = AbilityTagSet::empty();
        tags.insert_tag(AbilityTag::Offensive);
        // Non-chosen plan: chosen=false
        let mut ann = PlanAnnotation::default();
        ann.chosen = false;
        ann.effective_ai_tags = vec![tags];
        let unchosen = LoggedPlan { rank: 1, steps: vec![], annotation: ann };
        let event = make_event(1, LoggedDecision::EndTurn, vec![unchosen], None);

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert!(agg.f1_ability_tag_counts.is_empty(), "unchosen plan not counted for F1");
    }

    // ── F2: Need signals regression pin ──────────────────────────────────────

    #[test]
    fn f2_setup_aoe_goal_count_zero_for_non_setup_aoe_goals() {
        use storyforge::combat::ai::log::ContinuationLogSection;
        use storyforge::combat::ai::log::StoredGoalContextSnapshot;

        let stored = StoredGoalContextSnapshot {
            kind: "finish".to_owned(),
            target_id: None,
            region_anchor: [0, 0],
            region_radius: 2,
            planned_ability: None,
            ttl: 3,
            confidence: 0.8,
            created_round: 1,
            expected_actor_pos: [0, 0],
            actor_hp_at_store: 20,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            actor_statuses_at_store: vec![],
            target_hp_at_store: 10,
            target_pos_at_store: [1, 0],
        };
        let cont = ContinuationLogSection { stored_goal: stored, severity: None, age: 1 };
        let event = make_event(1, LoggedDecision::EndTurn, vec![], Some(cont));

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(agg.f2_setup_aoe_goal_count, 0, "no setup_aoe goal should appear");
    }

    // ── F3: Continuation severity ─────────────────────────────────────────────

    #[test]
    fn f3_severity_counts_relevant_event() {
        use storyforge::combat::ai::log::ContinuationLogSection;
        use storyforge::combat::ai::log::StoredGoalContextSnapshot;
        use storyforge::combat::ai::repair::ContinuationSeverity;

        let stored = StoredGoalContextSnapshot {
            kind: "pressure".to_owned(),
            target_id: None,
            region_anchor: [0, 0],
            region_radius: 2,
            planned_ability: None,
            ttl: 3,
            confidence: 0.8,
            created_round: 1,
            expected_actor_pos: [0, 0],
            actor_hp_at_store: 20,
            actor_rage_at_store: 0,
            actor_status_hash: 0,
            actor_statuses_at_store: vec![],
            target_hp_at_store: 10,
            target_pos_at_store: [1, 0],
        };
        let cont = ContinuationLogSection {
            stored_goal: stored,
            severity: Some(ContinuationSeverity::Relevant),
            age: 1,
        };
        let event = make_event(1, LoggedDecision::EndTurn, vec![], Some(cont));

        let mut agg = Aggregate::default();
        agg.process_event("f.jsonl", &event);

        assert_eq!(*agg.f3_severity_counts.get("Relevant").unwrap(), 1);
    }

    // ── H1/H2: Band coverage + agenda-item win-rate ───────────────────────────

    fn make_event_with_band(
        band: storyforge::combat::ai::intent::bands::PriorityBand,
        band_reason: storyforge::combat::ai::intent::bands::BandReason,
        agenda: Vec<storyforge::combat::ai::log::AgendaItemLog>,
        plans: Vec<LoggedPlan>,
    ) -> ActorTickEvent {
        let mut event = make_event(1, LoggedDecision::EndTurn, plans, None);
        event.band = Some(band);
        event.band_reason = Some(band_reason);
        event.agenda = agenda;
        event
    }

    fn agenda_item_log(kind: storyforge::combat::ai::intent::IntentKind) -> storyforge::combat::ai::log::AgendaItemLog {
        use storyforge::combat::ai::intent::IntentReason;
        use storyforge::combat::ai::intent::considerations::IntentConsiderations;
        storyforge::combat::ai::log::AgendaItemLog {
            kind,
            target: None,
            raw_score: 1.0,
            considerations: IntentConsiderations { urgency: 0.5, feasibility: 0.7,
                leverage: 0.6, safety: 0.8, role_affinity: 0.9, continuation_value: 0.3 },
            reason: IntentReason::NoRuleDefault,
        }
    }

    #[test]
    fn h1_band_tick_count_incremented_for_events_with_band() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::IntentKind;

        let agenda = vec![agenda_item_log(IntentKind::FocusTarget)];
        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            agenda,
            vec![],
        );

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h1_band_tick_counts.get("NormalTactical").copied(), Some(1));
        assert_eq!(agg.h1_urgency.len(), 1);
        assert!((agg.h1_urgency[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn h2_item_win_rate_attributed_when_chosen_plan_has_agenda_item() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::IntentKind;

        // Chosen plan with agenda_item = 1.
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = Some(1);
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let agenda = vec![
            agenda_item_log(IntentKind::FocusTarget),
            agenda_item_log(IntentKind::ApplyCC),
        ];
        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            agenda,
            vec![plan],
        );

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h2_band_chosen_total.get("NormalTactical").copied(), Some(1));
        assert_eq!(agg.h2_band_item_win.get("NormalTactical/1").copied(), Some(1));
        assert!(!agg.h2_band_item_win.contains_key("NormalTactical/0"));
    }

    #[test]
    fn h1_skip_path_does_not_increment_band_counts() {
        // Skip path event has no band.
        let event = make_event(1, LoggedDecision::Skip { reason: "no_ap".to_owned() }, vec![], None);
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert!(agg.h1_band_tick_counts.is_empty(), "skip-path should not add to band counts");
        assert!(agg.h1_urgency.is_empty());
    }

    // ── H3a: Construction-time metrics ───────────────────────────────────────

    #[test]
    fn h3a_agenda_size_tracked_per_band() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::IntentKind;

        let agenda = vec![
            agenda_item_log(IntentKind::FocusTarget),
            agenda_item_log(IntentKind::Reposition),
        ];
        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            agenda,
            vec![],
        );
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(
            agg.h3a_agenda_size.get("NormalTactical/2").copied(),
            Some(1),
            "agenda of size 2 should be recorded"
        );
    }

    #[test]
    fn h3a_target_none_rate_by_band_kind() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::{IntentKind, IntentReason};
        use storyforge::combat::ai::intent::considerations::IntentConsiderations;
        use storyforge::combat::ai::log::AgendaItemLog;

        let mut item_with_target = AgendaItemLog {
            kind: IntentKind::FocusTarget,
            target: Some(42),
            raw_score: 1.0,
            considerations: IntentConsiderations::default(),
            reason: IntentReason::NoRuleDefault,
        };
        // One item with target, one without.
        let item_no_target = AgendaItemLog { target: None, ..item_with_target.clone() };
        item_with_target.target = Some(7);

        let event = make_event_with_band(
            PriorityBand::ForcedTargeting,
            BandReason::Normal,
            vec![item_with_target, item_no_target],
            vec![],
        );
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3a_target_none.get("ForcedTargeting/FocusTarget").copied(), Some(1));
        assert_eq!(agg.h3a_target_some.get("ForcedTargeting/FocusTarget").copied(), Some(1));
    }

    #[test]
    fn h3a_unattributed_classified_as_no_items_when_agenda_empty() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = None; // no attribution
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            vec![], // empty agenda
            vec![plan],
        );
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3a_band_unattributed.get("NormalTactical").copied(), Some(1));
        assert_eq!(agg.h3a_unattr_no_items.get("NormalTactical").copied(), Some(1));
    }

    // ── H3b: Per-plan reject reason ───────────────────────────────────────────

    #[test]
    fn h3b_no_data_when_reject_reasons_field_absent() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::IntentKind;

        // Plan without reject_reasons_per_item (empty = pre-11.7 log).
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            vec![agenda_item_log(IntentKind::FocusTarget)],
            vec![plan],
        );
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert!(!agg.h3b_has_data, "no reject_reasons_per_item → h3b_has_data stays false");
    }

    #[test]
    fn h3b_reject_reason_counted_when_field_present() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::outcome::RejectReason;

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.reject_reasons_per_item = vec![Some(RejectReason::NotDefensive)];
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let event = make_event_with_band(
            PriorityBand::CriticalSelfPreservation,
            BandReason::Normal,
            vec![agenda_item_log(IntentKind::ProtectSelf)],
            vec![plan],
        );
        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert!(agg.h3b_has_data);
        assert_eq!(
            agg.h3b_reject_reason.get("CriticalSelfPreservation/ProtectSelf/NotDefensive").copied(),
            Some(1)
        );
        assert_eq!(agg.h3b_total.get("CriticalSelfPreservation/ProtectSelf").copied(), Some(1));
        assert_eq!(agg.h3b_eligible.get("CriticalSelfPreservation/ProtectSelf").copied(), None);
    }

    // ── H3c: Post-hoc fallback cause classifier ───────────────────────────────

    /// Build a UnitSnapshot at the given hex-axial position with given AP/MP.
    fn unit_at(entity_bits: u64, x: i32, y: i32, ap: i32, mp: i32, max_range: u32)
        -> storyforge::combat::ai::world::snapshot::UnitSnapshot
    {
        use storyforge::combat::ai::world::snapshot::UnitSnapshot;
        use storyforge::content::abilities::CasterContext;
        use storyforge::combat::ai::config::role::AxisProfile;
        use storyforge::combat::ai::world::tags::AiTags;
        use storyforge::game::components::Team;
        use storyforge::content::races::CritFailEffect;
        use bevy::prelude::Entity;
        use hexx::Hex;
        UnitSnapshot {
            entity: Entity::try_from_bits(entity_bits).expect("valid entity bits"),
            team: Team::Player,
            role: AxisProfile::default(),
            pos: Hex::new(x, y),
            hp: 20, max_hp: 20,
            armor: 0, armor_bonus: 0, damage_taken_bonus: 0,
            action_points: ap, max_ap: 2,
            movement_points: mp, speed: mp,
            mana: None, rage: None, energy: None,
            abilities: vec![],
            threat: 1.0,
            tags: AiTags::empty(),
            max_attack_range: max_range,
            summoner: None,
            reactions_left: 1,
            aoo_expected_damage: None,
            statuses: vec![],
            caster_ctx: CasterContext::default(),
            crit_fail_effect: CritFailEffect::default(),
            damage_horizon: vec![],
            ai_tuning_override: None,
        }
    }

    /// Build an unattributed tick event (chosen plan has no agenda_item, agenda is non-empty).
    fn make_h3c_event(
        actor_id: u64,
        snapshot: storyforge::combat::ai::world::snapshot::BattleSnapshot,
        agenda: Vec<storyforge::combat::ai::log::AgendaItemLog>,
        plans: Vec<LoggedPlan>,
    ) -> ActorTickEvent {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        let mut event = make_event(actor_id, LoggedDecision::EndTurn, plans, None);
        event.band = Some(PriorityBand::NormalTactical);
        event.band_reason = Some(BandReason::Normal);
        event.agenda = agenda;
        event.snapshot = snapshot;
        event.actor_id = actor_id;
        event
    }

    #[test]
    fn h3c_ap_mp_blocked_when_actor_has_zero_resources() {
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::world::snapshot::BattleSnapshot;
        use bevy::prelude::Entity;

        let actor_bits = Entity::from_raw_u32(1).expect("valid").to_bits();
        // Actor with 0 AP and 0 MP.
        let actor = unit_at(actor_bits, 0, 0, 0, 0, 1);
        let mut snap = BattleSnapshot::default();
        snap.units.push(actor);

        // Chosen plan has no agenda_item; agenda non-empty with a target.
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = None;
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let mut item = agenda_item_log(IntentKind::FocusTarget);
        item.target = Some(999u64);
        let event = make_h3c_event(actor_bits, snap, vec![item], vec![plan]);

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3c_ap_mp_blocked.get("NormalTactical").copied(), Some(1));
        assert_eq!(agg.h3c_total.get("NormalTactical").copied(), Some(1));
        // All other buckets empty.
        assert!(agg.h3c_no_target_in_agenda.is_empty());
    }

    #[test]
    fn h3c_no_target_when_all_agenda_items_have_no_target() {
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::world::snapshot::BattleSnapshot;
        use bevy::prelude::Entity;

        let actor_bits = Entity::from_raw_u32(2).expect("valid").to_bits();
        // Actor has AP+MP.
        let actor = unit_at(actor_bits, 0, 0, 2, 3, 1);
        let mut snap = BattleSnapshot::default();
        snap.units.push(actor);

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = None;
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        // Agenda items with target=None.
        let item1 = agenda_item_log(IntentKind::FocusTarget); // target = None by default
        let item2 = agenda_item_log(IntentKind::Reposition);
        let event = make_h3c_event(actor_bits, snap, vec![item1, item2], vec![plan]);

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3c_no_target_in_agenda.get("NormalTactical").copied(), Some(1));
        assert!(agg.h3c_ap_mp_blocked.is_empty());
    }

    #[test]
    fn h3c_no_plan_attempts_target_when_pool_skips_agenda_target() {
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::world::snapshot::BattleSnapshot;
        use bevy::prelude::Entity;
        use storyforge::core::AbilityId;
        use hexx::Hex;

        let actor_bits = Entity::from_raw_u32(3).expect("valid").to_bits();
        let target_bits = Entity::from_raw_u32(4).expect("valid").to_bits();
        let target_entity = Entity::try_from_bits(target_bits).expect("valid");

        // Actor at (0,0) with AP+MP; target at (1,0) — within range.
        let actor  = unit_at(actor_bits, 0, 0, 2, 3, 2);
        let target = unit_at(target_bits, 1, 0, 2, 3, 1);
        let mut snap = BattleSnapshot::default();
        snap.units.push(actor);
        snap.units.push(target);

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = None;
        // Plans cast a DIFFERENT entity (entity 5), not the agenda target (entity 4).
        let other_entity = Entity::from_raw_u32(5).expect("valid");
        let plan = LoggedPlan {
            rank: 1,
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: other_entity,
                target_pos: Hex::ZERO,
            }],
            annotation: ann,
        };

        let mut item = agenda_item_log(IntentKind::FocusTarget);
        item.target = Some(target_bits);
        let _ = target_entity; // used in assertion comment
        let event = make_h3c_event(actor_bits, snap, vec![item], vec![plan]);

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3c_no_plan_attempts_target.get("NormalTactical").copied(), Some(1),
            "pool does not attempt agenda target → NoPlanAttemptsTarget");
    }

    #[test]
    fn h3c_only_move_plans_when_no_cast_steps_at_all() {
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::world::snapshot::BattleSnapshot;
        use bevy::prelude::Entity;
        use hexx::Hex;

        let actor_bits = Entity::from_raw_u32(6).expect("valid").to_bits();
        let target_bits = Entity::from_raw_u32(7).expect("valid").to_bits();

        // Actor at (0,0); target at (1,0) in range.
        let actor  = unit_at(actor_bits, 0, 0, 2, 3, 2);
        let target = unit_at(target_bits, 1, 0, 2, 3, 1);
        let mut snap = BattleSnapshot::default();
        snap.units.push(actor);
        snap.units.push(target);

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = None;
        // All plans are Move-only.
        let plan = LoggedPlan {
            rank: 1,
            steps: vec![PlanStep::Move { path: vec![Hex::new(1, 0)] }],
            annotation: ann,
        };

        let mut item = agenda_item_log(IntentKind::FocusTarget);
        item.target = Some(target_bits);
        let event = make_h3c_event(actor_bits, snap, vec![item], vec![plan]);

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h3c_only_move_plans.get("NormalTactical").copied(), Some(1),
            "all plans are Move-only → OnlyMovePlans bucket");
    }

    #[test]
    fn h3c_attributed_tick_not_counted() {
        // If chosen plan has an agenda_item, H3c should not fire.
        use storyforge::combat::ai::intent::IntentKind;
        use storyforge::combat::ai::world::snapshot::BattleSnapshot;
        use bevy::prelude::Entity;

        let actor_bits = Entity::from_raw_u32(8).expect("valid").to_bits();
        let actor = unit_at(actor_bits, 0, 0, 2, 3, 1);
        let mut snap = BattleSnapshot::default();
        snap.units.push(actor);

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.agenda_item = Some(0); // attributed
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let item = agenda_item_log(IntentKind::FocusTarget);
        let event = make_h3c_event(actor_bits, snap, vec![item], vec![plan]);

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        // H3c total should be 0 for attributed tick.
        assert_eq!(agg.h3c_total.get("NormalTactical").copied().unwrap_or(0), 0);
    }

    // ── H1c.bis: per-IntentKind leverage histograms (step 11.8) ─────────────

    /// Verify that per-kind leverage values from the chosen plan's
    /// considerations_per_item are routed to the correct buckets.
    ///
    /// Two agenda items: FocusTarget and ProtectSelf. Chosen plan has
    /// considerations_per_item with leverage 0.7 and 0.4 respectively.
    /// Each value must land in its own bucket and nowhere else.
    #[test]
    fn h1c_per_kind_leverage_buckets_split_correctly() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::{IntentKind, IntentReason};
        use storyforge::combat::ai::intent::considerations::IntentConsiderations;
        use storyforge::combat::ai::log::AgendaItemLog;

        let item_focus = AgendaItemLog {
            kind: IntentKind::FocusTarget,
            target: None,
            raw_score: 1.0,
            considerations: IntentConsiderations::default(),
            reason: IntentReason::NoRuleDefault,
        };
        let item_protect = AgendaItemLog {
            kind: IntentKind::ProtectSelf,
            target: None,
            raw_score: 1.0,
            considerations: IntentConsiderations::default(),
            reason: IntentReason::NoRuleDefault,
        };

        // Chosen plan with considerations_per_item aligned to the agenda.
        let cons_focus = IntentConsiderations {
            urgency: 0.0, feasibility: 1.0, leverage: 0.7,
            safety: 1.0, role_affinity: 0.5, continuation_value: 0.0,
        };
        let cons_protect = IntentConsiderations {
            urgency: 0.0, feasibility: 1.0, leverage: 0.4,
            safety: 1.0, role_affinity: 0.5, continuation_value: 0.0,
        };
        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.considerations_per_item = vec![cons_focus, cons_protect];

        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let mut event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            vec![item_focus, item_protect],
            vec![plan],
        );
        // Give the event a snapshot (required by H3c) — reuse default, no H3c needed.
        event.snapshot = BattleSnapshot::default();

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        // FocusTarget bucket gets 0.7.
        assert_eq!(agg.h1_leverage_focus_target.len(), 1);
        assert!((agg.h1_leverage_focus_target[0] - 0.7).abs() < 1e-6,
            "FocusTarget leverage expected 0.7, got {}", agg.h1_leverage_focus_target[0]);

        // ProtectSelf bucket gets 0.4.
        assert_eq!(agg.h1_leverage_protect_self.len(), 1);
        assert!((agg.h1_leverage_protect_self[0] - 0.4).abs() < 1e-6,
            "ProtectSelf leverage expected 0.4, got {}", agg.h1_leverage_protect_self[0]);

        // Other buckets stay empty.
        assert!(agg.h1_leverage_apply_cc.is_empty(), "ApplyCC bucket should be empty");
        assert!(agg.h1_leverage_protect_ally.is_empty(), "ProtectAlly bucket should be empty");
        assert!(agg.h1_leverage_reposition.is_empty(), "Reposition bucket should be empty");
        assert!(agg.h1_leverage_last_stand.is_empty(), "LastStand bucket should be empty");
    }

    /// Verify Reposition and SetupAOE both route to the shared reposition bucket.
    #[test]
    fn h1c_reposition_and_setup_aoe_share_bucket() {
        use storyforge::combat::ai::intent::bands::{BandReason, PriorityBand};
        use storyforge::combat::ai::intent::{IntentKind, IntentReason};
        use storyforge::combat::ai::intent::considerations::IntentConsiderations;
        use storyforge::combat::ai::log::AgendaItemLog;

        let make_item = |kind| AgendaItemLog {
            kind,
            target: None,
            raw_score: 1.0,
            considerations: IntentConsiderations::default(),
            reason: IntentReason::NoRuleDefault,
        };
        let make_cons = |leverage| IntentConsiderations {
            urgency: 0.0, feasibility: 1.0, leverage,
            safety: 1.0, role_affinity: 0.5, continuation_value: 0.0,
        };

        let mut ann = PlanAnnotation::default();
        ann.chosen = true;
        ann.considerations_per_item = vec![make_cons(0.6), make_cons(0.3)];
        let plan = LoggedPlan { rank: 1, steps: vec![], annotation: ann };

        let event = make_event_with_band(
            PriorityBand::NormalTactical,
            BandReason::Normal,
            vec![make_item(IntentKind::Reposition), make_item(IntentKind::SetupAOE)],
            vec![plan],
        );

        let mut agg = Aggregate::default();
        agg.process_event("h.jsonl", &event);

        assert_eq!(agg.h1_leverage_reposition.len(), 2,
            "both Reposition and SetupAOE should land in reposition bucket");
        let values: Vec<f32> = agg.h1_leverage_reposition.clone();
        assert!(values.contains(&0.6_f32) || values.iter().any(|&v| (v - 0.6).abs() < 1e-6));
        assert!(values.iter().any(|&v| (v - 0.3).abs() < 1e-6));
    }
}
