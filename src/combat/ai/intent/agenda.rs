//! Agenda — step 11.2.
//!
//! `Agenda` is the ordered list of tactical candidates the AI will pursue this
//! turn.  In 11.2 the agenda is **built but not used**: `build_agenda` is
//! called in `pick_action` and the result is immediately discarded.  Routing
//! lands in 11.4; per-item considerations scoring in 11.3.
//!
//! Per-band sizes (fixed, see §11 decisions §2):
//! - `ForcedTargeting`          → N=1
//! - `CriticalSelfPreservation` → N=2 (ProtectSelf + best Reposition)
//! - `HardRescueOpportunity`    → N=2 (ProtectAlly + FocusTarget on threat)
//! - `NormalTactical`           → N=1 in 11.2 (legacy `select_intent` winner).
//!   Full N=3 expansion deferred to 11.5 when `select_intent` is decomposed.

use bevy::prelude::Entity;
use serde::{Deserialize, Serialize};

use crate::combat::ai::appraisal::{ally_threat_proxy, NeedSignals};
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::intent::{
    select_intent_normal, AiMemory, BandReason, IntentKind, IntentReason, PriorityBand, TacticalIntent,
};
use crate::combat::ai::intent::considerations::{compute_considerations, IntentConsiderations};
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::scoring::target_selection::target_selection_score;
use crate::combat::ai::config::tuning::AiTuning;

// ── AgendaItem ────────────────────────────────────────────────────────────────

/// One candidate tactical intent with its raw score and diagnostic reason.
///
/// `kind` and `target` together identify what the AI wants to do; `raw_score`
/// determines ordering within the agenda; `reason` is passed through to logs
/// and to the considerations scorer in 11.3.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgendaItem {
    /// Tactical intent kind (without entity payload).
    pub kind: IntentKind,
    /// Optional target entity — populated for FocusTarget / ApplyCC / ProtectAlly.
    pub target: Option<Entity>,
    /// Score from the legacy or band-specific heuristic.
    pub raw_score: f32,
    /// Why this item was added — for logs and considerations.
    pub reason: IntentReason,
    /// Structured 6-axis considerations computed in 11.3.
    /// Populated by `build_agenda`; not used for routing until 11.4.
    pub considerations: IntentConsiderations,
}

impl AgendaItem {
    /// Convert this item into a `TacticalIntent` suitable for scoring functions
    /// (`compute_plan_intent_sum`, `compute_plan_tempo_gain`).
    ///
    /// Target-bearing intents (`FocusTarget`, `ApplyCC`, `ProtectAlly`) use
    /// the stored `self.target`.  When `target` is `None` (e.g. the item was
    /// built without a target — shouldn't happen for these kinds, but handled
    /// defensively), the intent degrades to `Reposition`.
    pub fn intent_for_scoring(&self) -> TacticalIntent {
        match self.kind {
            IntentKind::FocusTarget => self
                .target
                .map(|t| TacticalIntent::FocusTarget { target: t })
                .unwrap_or(TacticalIntent::Reposition),
            IntentKind::ApplyCC => self
                .target
                .map(|t| TacticalIntent::ApplyCC { target: t })
                .unwrap_or(TacticalIntent::Reposition),
            IntentKind::ProtectAlly => self
                .target
                .map(|ally| TacticalIntent::ProtectAlly { ally })
                .unwrap_or(TacticalIntent::Reposition),
            IntentKind::Reposition => TacticalIntent::Reposition,
            IntentKind::ProtectSelf => TacticalIntent::ProtectSelf,
            IntentKind::SetupAOE => TacticalIntent::SetupAOE,
            // LastStand is an EvaluationMode marker, not a TacticalIntent.
            // intent_for_scoring() is overridden by EvaluationMode::LastStand
            // in the scorer — this fallback value is never used for scoring.
            IntentKind::LastStand => TacticalIntent::Reposition,
        }
    }
}

// ── Agenda ────────────────────────────────────────────────────────────────────

/// Top-N candidates for this actor's turn, ordered by `raw_score` descending.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agenda {
    /// Which priority band this agenda was built for.
    pub band: PriorityBand,
    /// Top-N items, ordered by `raw_score` descending.
    /// N varies per band (see module-level doc).
    pub items: Vec<AgendaItem>,
}

// ── build_agenda ─────────────────────────────────────────────────────────────

/// Build the agenda for `active` given the current battle state.
///
/// Dispatches to a per-band builder based on `band`.  The result is sorted by
/// `raw_score` descending before being returned.
///
/// `memory` is forwarded to `build_normal_tactical` so that stickiness bonuses
/// apply within the normal-tactical intent selection, matching the prior
/// `select_intent` behaviour (step 11.5).
///
/// **11.3 contract**: considerations are computed for every item but are NOT used
/// for routing — that lands in 11.4.  `repair` is `None` in 11.3; per-plan repair
/// affinity arrives in 11.4 alongside the plan overlay.
#[allow(clippy::too_many_arguments)]
pub fn build_agenda(
    band: PriorityBand,
    band_reason: &BandReason,
    active: UnitView<'_>,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    needs: &NeedSignals,
    difficulty: &DifficultyProfile,
    tuning: &AiTuning,
    memory: &AiMemory,
    status_tags: &crate::combat::ai::world::tags::StatusTagCache,
) -> Agenda {
    let mut items = match band {
        PriorityBand::ForcedTargeting => {
            build_forced_targeting(band_reason, active, snap)
        }
        PriorityBand::CriticalSelfPreservation => {
            build_critical_self_preservation(band_reason, active, snap, maps, needs)
        }
        PriorityBand::HardRescueOpportunity => {
            build_hard_rescue_opportunity(active, snap, needs)
        }
        PriorityBand::NormalTactical => {
            build_normal_tactical(active, snap, maps, needs, difficulty, tuning, memory, status_tags)
        }
    };

    // ── Step 11.3: compute considerations per item ────────────────────────────
    // `repair = None` — per-plan RepairAffinity arrives in 11.4.
    // `role` taken from active.role (AxisProfile on UnitView via Deref).
    let role: &AxisProfile = &active.cache.role;
    for item in items.iter_mut() {
        item.considerations = compute_considerations(item, needs, role, None);
    }

    // Ensure ordering contract: highest raw_score first.
    items.sort_by(|a, b| b.raw_score.partial_cmp(&a.raw_score).unwrap_or(std::cmp::Ordering::Equal));

    Agenda { band, items }
}

// ── Per-band builders ─────────────────────────────────────────────────────────

/// ForcedTargeting: N=1 — the taunter is the only valid target.
fn build_forced_targeting(
    band_reason: &BandReason,
    active: UnitView<'_>,
    snap: &BattleSnapshot,
) -> Vec<AgendaItem> {
    let BandReason::TauntForced { taunter } = band_reason else {
        // Defensive: band/reason mismatch is a programming error.
        return Vec::new();
    };
    let taunter = *taunter;

    // Score: use target_priority for ordering consistency with legacy path;
    // falls back to 1.0 if the taunter is no longer in the snapshot.
    let raw_score = snap
        .unit(taunter)
        .map(|t| target_selection_score(active, t, snap))
        .unwrap_or(1.0);

    vec![AgendaItem {
        kind: IntentKind::FocusTarget,
        target: Some(taunter),
        raw_score,
        reason: IntentReason::TauntForced,
        considerations: IntentConsiderations::default(),
    }]
}

/// CriticalSelfPreservation: N=2 — ProtectSelf + best Reposition-away.
fn build_critical_self_preservation(
    band_reason: &BandReason,
    active: UnitView<'_>,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    needs: &NeedSignals,
) -> Vec<AgendaItem> {
    let (self_preserve, danger) = match band_reason {
        BandReason::PanicOverride { self_preserve, danger } => (*self_preserve, *danger),
        _ => (needs.self_preserve, maps.danger.get(active.pos)),
    };

    // Item 1: ProtectSelf — urgency = self_preserve × danger (mirrors select_intent logic).
    let protect_self_score = (self_preserve * danger).clamp(0.0, 1.0);
    let protect_self = AgendaItem {
        kind: IntentKind::ProtectSelf,
        target: None,
        raw_score: protect_self_score,
        reason: IntentReason::PanicOverride {
            self_preserve,
            self_preserve_threshold: 0.0, // exact threshold not available here; 11.3 will fill in
            danger,
            danger_threshold: 0.0,
        },
        considerations: IntentConsiderations::default(),
    };

    // Item 2: Reposition away — score based on reposition need signal.
    // In 11.2 the tile selection is deferred to plan generation; here we
    // only establish the intent with the reposition signal as the score.
    let reposition_score = (needs.reposition * 0.7 + 0.3).clamp(0.0, 1.0);
    let reposition = AgendaItem {
        kind: IntentKind::Reposition,
        target: None,
        raw_score: reposition_score,
        reason: IntentReason::Reposition {
            reposition: needs.reposition,
            floor: 0.0, // floor not available here; plan-level viability will gate
        },
        considerations: IntentConsiderations::default(),
    };

    // ProtectSelf is always the primary item (higher urgency); Reposition
    // is secondary.  Sort by raw_score is applied by build_agenda.
    let mut items = vec![protect_self, reposition];
    // Ensure ProtectSelf wins the primary slot even if reposition scored higher
    // by clamping: ProtectSelf raw_score must be ≥ Reposition raw_score.
    if items[1].raw_score > items[0].raw_score {
        items[0].raw_score = items[1].raw_score + f32::EPSILON;
    }

    // Check for enemy in snap (snapshot may be empty in edge-case tests).
    // If the snap has no enemies, the reposition intent still makes sense
    // as the only action available; keep it.
    let _ = snap; // used via snap.unit above; kept for clarity
    items
}

/// HardRescueOpportunity: N=2 — ProtectAlly + FocusTarget on the threat to that ally.
fn build_hard_rescue_opportunity(
    active: UnitView<'_>,
    snap: &BattleSnapshot,
    needs: &NeedSignals,
) -> Vec<AgendaItem> {
    // Find most-endangered ally: highest (1 - hp_pct) × threat_proxy score.
    let ally = snap
        .allies_of(active.team)
        .filter(|a| a.entity() != active.entity())
        .max_by(|a, b| {
            let score_a = (1.0 - a.hp_pct()) * ally_threat_proxy(*a, snap);
            let score_b = (1.0 - b.hp_pct()) * ally_threat_proxy(*b, snap);
            score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
        });

    let Some(endangered_ally) = ally else {
        // No ally found — return a fallback ProtectSelf item so the agenda
        // is never empty (build_agenda guarantees items.len() >= 1).
        return vec![AgendaItem {
            kind: IntentKind::ProtectSelf,
            target: None,
            raw_score: needs.rescue_ally,
            reason: IntentReason::Urgency {
                self_preserve: needs.self_preserve,
                danger: 0.0,
            },
            considerations: IntentConsiderations::default(),
        }];
    };

    let ally_entity = endangered_ally.entity();
    let rescue_score = needs.rescue_ally;

    // Item 1: ProtectAlly.
    let protect_ally = AgendaItem {
        kind: IntentKind::ProtectAlly,
        target: Some(ally_entity),
        raw_score: rescue_score,
        reason: IntentReason::ProtectAlly {
            ally_hp_pct: endangered_ally.hp_pct(),
            threshold: 0.5, // default threshold; exact value from tuning in 11.3
            heal_identity: active.cache.role.support.min(1.0),
        },
        considerations: IntentConsiderations::default(),
    };

    // Item 2 (optional): FocusTarget — the biggest threat to the endangered ally.
    // Only emit when an actual threat is identified. Step 11.7 mining showed that
    // emitting FocusTarget with `target=None` produced 23.5% of HardRescue
    // FocusTarget items as untargeted — semantic noise. Honest N=1 ProtectAlly
    // is preferable to an artificial FocusTarget without a target.
    let threat = snap
        .enemies_of(active.team)
        .filter(|e| e.pos.unsigned_distance_to(endangered_ally.pos) <= e.cache.max_attack_range)
        .max_by(|a, b| {
            crate::combat::ai::scoring::horizon_avg(*a)
                .partial_cmp(&crate::combat::ai::scoring::horizon_avg(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    let Some(threat) = threat else {
        // No threat to the endangered ally — return N=1 ProtectAlly only.
        return vec![protect_ally];
    };

    let focus_score = rescue_score * 0.8; // slightly lower than ProtectAlly
    let focus_item = AgendaItem {
        kind: IntentKind::FocusTarget,
        target: Some(threat.entity()),
        raw_score: focus_score,
        reason: IntentReason::BestPriority { priority: focus_score },
        considerations: IntentConsiderations::default(),
    };

    vec![protect_ally, focus_item]
}

/// NormalTactical: N=1 — winner of `select_intent_normal`.
///
/// Step 11.5: replaces the previous `select_intent` (full ladder) call with
/// `select_intent_normal` (FocusTarget / ApplyCC / SetupAOE / Reposition only).
/// Panic / taunt / rescue branches are handled by their own band builders.
///
/// `memory` is forwarded to preserve stickiness bonuses within normal-tactical
/// intent selection, matching prior behaviour in `pick_action`.
fn build_normal_tactical(
    active: UnitView<'_>,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    needs: &NeedSignals,
    difficulty: &DifficultyProfile,
    tuning: &AiTuning,
    memory: &AiMemory,
    status_tags: &crate::combat::ai::world::tags::StatusTagCache,
) -> Vec<AgendaItem> {
    let choice = select_intent_normal(active, snap, maps, memory, difficulty, tuning, needs, status_tags);

    let kind = choice.intent.kind();
    let target = choice.intent.target();

    // raw_score: 1.0 placeholder — actual winner is determined by per-item plan
    // scoring in PickBestStage (step 11.4).  Full multi-candidate expansion
    // (N=3) is deferred to a future step when mining confirms its benefit.
    let raw_score = 1.0_f32;

    vec![AgendaItem {
        kind,
        target,
        raw_score,
        reason: choice.reason,
        considerations: IntentConsiderations::default(),
    }]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::appraisal::NeedSignals;
    use crate::combat::ai::config::difficulty::DifficultyProfile;

    use crate::combat::ai::world::tags::AiTags;
    use crate::combat::ai::world::tags::StatusTagCache;
    use crate::combat::ai::world::tags::cache::build_caches;
    use crate::combat::ai::test_helpers::{empty_maps, empty_content, UnitBuilder};
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::content::statuses::StatusDef;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn origin() -> crate::game::hex::Hex {
        hex_from_offset(0, 0)
    }
    fn default_tuning() -> AiTuning { AiTuning::default() }
    fn default_difficulty() -> DifficultyProfile { DifficultyProfile::default() }

    fn taunt_status_tags() -> StatusTagCache {
        let mut content = empty_content();
        let status_def = StatusDef {
            id: "taunt".into(),
            name: "Taunt".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                skips_turn: false,
                bonuses: combat_engine::StatusBonuses::default(),
                forces_targeting: true,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        };
        content.statuses.insert("taunt".into(), status_def);
        let (status_tags, _ability_tags) = build_caches(&content);
        status_tags
    }

    fn unit_with_taunt(id: u32, team: Team, pos: crate::game::hex::Hex)
        -> crate::combat::ai::world::snapshot::UnitSnapshot
    {
        let mut unit = UnitBuilder::new(id, team, pos).build();
        unit.statuses.push(crate::combat::ai::world::snapshot::ActiveStatusView {
            id: "taunt".into(),
            rounds_remaining: 1,
            dot_per_tick: 0,
        });
        unit
    }

    // ── 1. ForcedTargeting emits exactly one FocusTarget item ─────────────

    #[test]
    fn agenda_forced_targeting_emits_single_item() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).build();
        let taunter = unit_with_taunt(2, Team::Player, hex_from_offset(1, 0));
        let taunter_entity = taunter.entity;
        let snap = snapshot_from(vec![active.clone(), taunter], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();
        let band_reason = BandReason::TauntForced { taunter: taunter_entity };
        let status_tags = taunt_status_tags();

        let agenda = build_agenda(
            PriorityBand::ForcedTargeting,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &NeedSignals::default(),
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &status_tags,
        );

        assert_eq!(agenda.items.len(), 1, "ForcedTargeting must emit exactly 1 item");
        let item = &agenda.items[0];
        assert_eq!(item.kind, IntentKind::FocusTarget);
        assert_eq!(item.target, Some(taunter_entity));
    }

    // ── 2. CriticalSelfPreservation emits two items: ProtectSelf + Reposition

    #[test]
    fn agenda_critical_self_preservation_emits_two_items() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).hp(2).max_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0)).build();
        let snap = snapshot_from(vec![active.clone(), enemy], 1);

        let tuning = default_tuning();
        let difficulty = default_difficulty();
        let danger_panic = difficulty.awareness_danger_threshold(&tuning);
        let mut maps = empty_maps();
        maps.danger.add(origin(), danger_panic + 0.1);

        let needs = NeedSignals {
            self_preserve: tuning.thresholds.panic_self_preserve_threshold + 0.01,
            reposition:    0.6,
            ..NeedSignals::default()
        };
        let band_reason = BandReason::PanicOverride {
            self_preserve: needs.self_preserve,
            danger: danger_panic + 0.1,
        };

        let agenda = build_agenda(
            PriorityBand::CriticalSelfPreservation,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &needs,
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &StatusTagCache::default(),
        );

        assert_eq!(agenda.items.len(), 2, "CriticalSelf must emit exactly 2 items");
        // Sorted descending — first must be ProtectSelf.
        assert_eq!(agenda.items[0].kind, IntentKind::ProtectSelf, "item[0] must be ProtectSelf");
        assert_eq!(agenda.items[1].kind, IntentKind::Reposition, "item[1] must be Reposition");
    }

    // ── 3. HardRescueOpportunity emits two items: ProtectAlly + FocusTarget ─

    #[test]
    fn agenda_hard_rescue_emits_two_items() {
        let active = UnitBuilder::new(1, Team::Enemy, origin())
            .tags(AiTags::CAN_HEAL)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .hp(2)
            .max_hp(20)
            .build();
        // An enemy that threatens the ally.
        let threat = UnitBuilder::new(3, Team::Player, hex_from_offset(2, 0))
            .threat(8.0)
            .max_attack_range(2)
            .build();
        let snap = snapshot_from(vec![active.clone(), ally.clone(), threat], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();

        let needs = NeedSignals {
            rescue_ally: tuning.thresholds.hard_rescue_threshold + 0.05,
            ..NeedSignals::default()
        };
        let band_reason = BandReason::HardRescueNeed { rescue_need: needs.rescue_ally };

        let agenda = build_agenda(
            PriorityBand::HardRescueOpportunity,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &needs,
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &StatusTagCache::default(),
        );

        assert_eq!(agenda.items.len(), 2, "HardRescue must emit exactly 2 items");
        // First item must be ProtectAlly (highest raw_score).
        assert_eq!(agenda.items[0].kind, IntentKind::ProtectAlly, "item[0] must be ProtectAlly");
        assert_eq!(agenda.items[0].target, Some(ally.entity), "ProtectAlly target must be endangered ally");
        // Second item must be FocusTarget on the actual threat.
        assert_eq!(agenda.items[1].kind, IntentKind::FocusTarget, "item[1] must be FocusTarget");
        assert!(agenda.items[1].target.is_some(), "FocusTarget must carry a threat target");
    }

    /// Step 11.7 follow-up: when no enemy threatens the endangered ally,
    /// HardRescue must emit only N=1 ProtectAlly — never a FocusTarget with
    /// `target=None`. Mining showed 23.5% of legacy HardRescue/FocusTarget items
    /// were untargeted; this test pins the corrected semantic.
    #[test]
    fn agenda_hard_rescue_skips_focus_target_when_no_threat() {
        let active = UnitBuilder::new(1, Team::Enemy, origin())
            .tags(AiTags::CAN_HEAL)
            .build();
        let ally = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .hp(2)
            .max_hp(20)
            .build();
        // No enemy in the snapshot — no threat to find.
        let snap = snapshot_from(vec![active.clone(), ally.clone()], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();

        let needs = NeedSignals {
            rescue_ally: tuning.thresholds.hard_rescue_threshold + 0.05,
            ..NeedSignals::default()
        };
        let band_reason = BandReason::HardRescueNeed { rescue_need: needs.rescue_ally };

        let agenda = build_agenda(
            PriorityBand::HardRescueOpportunity,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &needs,
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &StatusTagCache::default(),
        );

        assert_eq!(
            agenda.items.len(),
            1,
            "HardRescue without a threat must collapse to N=1 ProtectAlly only"
        );
        assert_eq!(agenda.items[0].kind, IntentKind::ProtectAlly);
        assert!(
            !agenda.items.iter().any(|item| item.kind == IntentKind::FocusTarget && item.target.is_none()),
            "must never emit FocusTarget with target=None"
        );
    }

    // ── 4. NormalTactical emits at least one item ─────────────────────────

    #[test]
    fn agenda_normal_tactical_emits_at_least_one() {
        let active = UnitBuilder::new(1, Team::Enemy, origin()).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).build();
        let snap = snapshot_from(vec![active.clone(), enemy], 1);
        let maps = empty_maps();
        let tuning = default_tuning();
        let difficulty = default_difficulty();
        let band_reason = BandReason::Normal;

        let agenda = build_agenda(
            PriorityBand::NormalTactical,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &NeedSignals::default(),
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &StatusTagCache::default(),
        );

        assert!(
            !agenda.items.is_empty(),
            "NormalTactical must emit at least 1 item"
        );
    }

    // ── 5. Items are ordered by raw_score descending ──────────────────────

    #[test]
    fn agenda_items_ordered_by_raw_score_desc() {
        // Use CriticalSelf — guaranteed 2 items with known ordering invariant.
        let active = UnitBuilder::new(1, Team::Enemy, origin()).hp(2).max_hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0)).build();
        let snap = snapshot_from(vec![active.clone(), enemy], 1);

        let tuning = default_tuning();
        let difficulty = default_difficulty();
        let danger = difficulty.awareness_danger_threshold(&tuning) + 0.5;
        let mut maps = empty_maps();
        maps.danger.add(origin(), danger);

        let needs = NeedSignals {
            self_preserve: tuning.thresholds.panic_self_preserve_threshold + 0.3,
            reposition: 0.8,
            ..NeedSignals::default()
        };
        let band_reason = BandReason::PanicOverride {
            self_preserve: needs.self_preserve,
            danger,
        };

        let agenda = build_agenda(
            PriorityBand::CriticalSelfPreservation,
            &band_reason,
            snap.unit(active.entity).expect("active in snap"),
            &snap,
            &maps,
            &needs,
            &difficulty,
            &tuning,
            &AiMemory::default(),
            &StatusTagCache::default(),
        );

        // Verify strict descending order.
        for window in agenda.items.windows(2) {
            assert!(
                window[0].raw_score >= window[1].raw_score,
                "Items must be ordered by raw_score descending: {:.4} < {:.4}",
                window[0].raw_score,
                window[1].raw_score,
            );
        }
    }
}
