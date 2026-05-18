//! Appraisal / Need layer (step 3 of ai-rework).
//!
//! Aggregates raw tactical facts (`BattleSnapshot` + `InfluenceMaps` + `AiMemory`)
//! into normalised "urgency" signals consumed by `select_intent` and downstream
//! scoring layers. `compute_need_signals` accepts `&AppraisalCtx` and populates
//! 7 signals: `self_preserve`, `continue_commitment`, `finish_target`,
//! `reposition`, `conserve_resource`, `rescue_ally`, `apply_cc`.
//! `setup_aoe` stays at 0.0 — no Setup mechanic exists in shape (see plan §9.B scope).
//!
//! Spec: `docs/ai_need_signals.md` (mining-driven taxonomy + curve params).
//! Decomposition: `docs/ai_rework_step3_plan.md`.

mod self_preserve;
mod continue_commitment;
mod finish_target;
mod reposition;
mod conserve_resource;
mod rescue_ally;
mod apply_cc;

pub(crate) use rescue_ally::ally_threat_proxy;

use serde::{Deserialize, Serialize};

use crate::combat::ai::memory::AiMemory;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::world::influence::InfluenceMaps;
use crate::combat::ai::world::tags::{AbilityTagCache, StatusTagCache};
use crate::combat::ai::config::tuning::AiTuning;
use crate::content::content_view::ContentView;

/// Grouping struct for all read-only inputs to `compute_need_signals`.
///
/// Provides tag-cache access for `rescue_ally` / `apply_cc` producers alongside
/// snapshot, influence maps, memory, tuning, and content view.
pub struct AppraisalCtx<'a> {
    pub active:       &'a UnitSnapshot,
    pub snap:         &'a BattleSnapshot,
    pub maps:         &'a InfluenceMaps,
    pub memory:       &'a AiMemory,
    pub tuning:       &'a AiTuning,
    pub ability_tags: &'a AbilityTagCache,
    pub status_tags:  &'a StatusTagCache,
    pub content:      &'a ContentView,
}

/// Normalised need-signal vector. Each field in [0, 1] semantically; producers
/// clamp. Seven signals are populated via `compute_need_signals`: `self_preserve`,
/// `continue_commitment`, `finish_target`, `reposition`, `conserve_resource`,
/// `rescue_ally`, `apply_cc`. `setup_aoe` stays at 0.0 — no Setup mechanic in shape.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NeedSignals {
    pub self_preserve: f32,
    pub rescue_ally: f32,
    pub finish_target: f32,
    pub apply_cc: f32,
    pub setup_aoe: f32,
    pub reposition: f32,
    pub conserve_resource: f32,
    pub continue_commitment: f32,
}

/// Compute need signals from raw tactical state via `AppraisalCtx`.
///
/// Populates all 7 active signals; `rescue_ally` and `apply_cc` read tag-caches.
/// `setup_aoe` stays at 0.0 — no Setup mechanic in shape (see inline comment).
pub fn compute_need_signals(ctx: &AppraisalCtx<'_>) -> NeedSignals {
    NeedSignals {
        self_preserve:       self_preserve::compute_self_preserve(ctx),
        continue_commitment: continue_commitment::compute_continue_commitment(ctx),
        finish_target:       finish_target::compute_finish_target(ctx),
        reposition:          reposition::compute_reposition(ctx),
        conserve_resource:   conserve_resource::compute_conserve_resource(ctx),
        rescue_ally:         rescue_ally::compute_rescue_ally(ctx),
        apply_cc:            apply_cc::compute_apply_cc(ctx),
        // Setup mechanic absent from shape — activates when channel/marker
        // effects are introduced. Explicit 0.0 pin (not a stub).
        setup_aoe:           0.0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::combat::ai::world::snapshot::BattleSnapshot;
    use crate::combat::ai::test_helpers::{empty_content, empty_maps, UnitBuilder};
    use crate::combat::ai::config::tuning::AiTuning;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    pub fn default_memory() -> AiMemory {
        AiMemory {
            last_intent: None,
            last_target: None,
            turns_committed: 0,
            last_goal: None,
            hp_ratio_at_last_turn: None,
            last_turn_was_defensive: false,
            turns_in_low_hp: 0,
        }
    }

    pub fn snap(units: Vec<crate::combat::ai::world::snapshot::UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    /// Convenience helper: build an `AppraisalCtx` for unit tests that call
    /// individual producer functions. Uses empty caches and empty maps by default.
    #[allow(clippy::too_many_arguments)]
    pub fn make_ctx<'a>(
        active: &'a UnitSnapshot,
        battle_snap: &'a BattleSnapshot,
        memory: &'a AiMemory,
        tuning: &'a AiTuning,
        maps: &'a crate::combat::ai::world::influence::InfluenceMaps,
        content: &'a crate::content::content_view::ContentView,
        ability_tags: &'a crate::combat::ai::world::tags::AbilityTagCache,
        status_tags: &'a crate::combat::ai::world::tags::StatusTagCache,
    ) -> AppraisalCtx<'a> {
        AppraisalCtx { active, snap: battle_snap, maps, memory, tuning, ability_tags, status_tags, content }
    }

    /// Minimal `AbilityDef` for tests — only sets the override, everything else default.
    pub fn minimal_ability_def_with_override(tags: &[&str]) -> crate::content::abilities::AbilityDef {
        use crate::content::abilities::{AbilityDef, AbilityRange, EffectDef, TargetType, AoEShape};
        AbilityDef {
            id: "test".into(),
            name: "test".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange::MELEE,
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            magic_domains: vec![],
            magic_method: String::new(),
            key: None,
            ai_tags_override: Some(tags.iter().map(|s| s.to_string()).collect()),
            is_move_toggle: false,
        }
    }

    /// Build a content + ability_tag_cache with a "heal" ability tagged Rescue.
    pub fn content_with_rescue_ability() -> (crate::content::content_view::ContentView, crate::combat::ai::world::tags::AbilityTagCache, crate::combat::ai::world::tags::StatusTagCache) {
        use crate::combat::ai::world::tags::cache::build_caches;

        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&["rescue"]);
        def.id = "heal".into();
        content.abilities.insert("heal".into(), def);
        let (st, at) = build_caches(&content);
        (content, at, st)
    }

    /// Build content + cache with a "stun" ability tagged ApplyCC.
    pub fn content_with_apply_cc_ability() -> (crate::content::content_view::ContentView, crate::combat::ai::world::tags::AbilityTagCache, crate::combat::ai::world::tags::StatusTagCache) {
        use crate::combat::ai::world::tags::cache::build_caches;

        let mut content = empty_content();
        let mut def = minimal_ability_def_with_override(&["apply_cc"]);
        def.id = "stun".into();
        content.abilities.insert("stun".into(), def);
        let (st, at) = build_caches(&content);
        (content, at, st)
    }

    // ── integration / setup_aoe pin ───────────────────────────────────────

    #[test]
    fn default_need_signals_are_zero() {
        let n = NeedSignals::default();
        assert_eq!(n.self_preserve, 0.0);
        assert_eq!(n.rescue_ally, 0.0);
        assert_eq!(n.finish_target, 0.0);
        assert_eq!(n.apply_cc, 0.0);
        assert_eq!(n.setup_aoe, 0.0);
        assert_eq!(n.reposition, 0.0);
        assert_eq!(n.conserve_resource, 0.0);
        assert_eq!(n.continue_commitment, 0.0);
    }

    #[test]
    fn compute_need_signals_setup_aoe_remains_zero_after_9b() {
        // Explicit pin: setup_aoe stays 0.0 regardless of kit/aoe-shape.
        // No Setup mechanic in shape — see plan §9.B scope.
        let actor_pos = hex_from_offset(3, 3);
        let active = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .full_hp(20)
            .ap(1)
            .mana(20, 20)
            .build();
        let memory = default_memory();
        let tuning = AiTuning::default();
        let maps = empty_maps();
        let s = snap(vec![active.clone()]);
        let content = empty_content();
        let (st, at) = crate::combat::ai::test_helpers::empty_caches();
        let ctx = make_ctx(&active, &s, &memory, &tuning, &maps, &content, &at, &st);
        let signals = compute_need_signals(&ctx);
        assert_eq!(signals.setup_aoe, 0.0, "setup_aoe must remain 0.0 — no Setup mechanic in shape");
        assert!(signals.self_preserve < 0.05, "self_preserve near 0 at full HP");
        assert!(signals.conserve_resource < 0.1, "conserve_resource near 0 at full mana");
    }
}
