//! Pure classifier: shape → tag set.
//!
//! `derive_status_tags` is one-pass (no dependencies).
//! `derive_ability_tags` requires a status-tag lookup (for Defensive / ApplyCC / Peel
//! which depend on what statuses the ability applies). Caller builds the
//! StatusTagCache first, then passes it into ability classification.

use std::collections::HashMap;

use crate::content::abilities::{AbilityDef, EffectDef, StatusOn, TargetType};
use crate::content::statuses::StatusDef;
use combat_engine::StatusId;

use super::{AbilityTag, AbilityTagSet, StatusTag, StatusTagSet};

// ── StatusTagLookup ──────────────────────────────────────────────────────────

/// Trait for looking up pre-computed status tags by id.
/// Implemented by both `HashMap<StatusId, StatusTagSet>` (tests) and
/// `StatusTagCache` (production via `&cache.map`).
pub trait StatusTagLookup {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet;
}

impl StatusTagLookup for HashMap<StatusId, StatusTagSet> {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet {
        self.get(id).copied().unwrap_or_default()
    }
}

// ── Status classifier ────────────────────────────────────────────────────────

/// Derive AI semantic tags from a status definition. Pure — no side effects.
///
/// Rule summary:
/// - HardCC:     `skips_turn`
/// - SoftCC:     `causes_disadvantage || speed_bonus < 0`
/// - Dot:        `dot_dice.is_some() || hp_percent_dot > 0`
/// - Buff:       `buff_class.is_some() || armor_bonus > 0`
/// - Compulsion: `forces_targeting` — overrides targeting (taunt-like).
///   Set in parallel with other tags; suppresses the Cosmetic fallback.
/// - Cosmetic:   fallback when none of the above apply (including Compulsion)
pub fn derive_status_tags(def: &StatusDef) -> StatusTagSet {
    let mut s = StatusTagSet::empty();
    if def.skips_turn {
        s.insert_tag(StatusTag::HardCC);
    }
    if def.causes_disadvantage || def.bonuses.speed_bonus < 0 {
        s.insert_tag(StatusTag::SoftCC);
    }
    if def.dot_dice.is_some() || def.hp_percent_dot > 0 {
        s.insert_tag(StatusTag::Dot);
    }
    if def.buff_class.is_some() || def.bonuses.armor_bonus > 0 {
        s.insert_tag(StatusTag::Buff);
    }
    if def.forces_targeting {
        s.insert_tag(StatusTag::Compulsion);
    }
    // Cosmetic fallback: only when no other tag was set (Compulsion included).
    if s.is_empty() {
        s.insert_tag(StatusTag::Cosmetic);
    }
    s
}

// ── Ability classifier ───────────────────────────────────────────────────────

/// Derive AI semantic tags from an ability definition. Pure — no side effects.
///
/// Rule summary:
/// - Offensive: any direct damage effect (WeaponAttack / Damage / SpellDamage)
/// - Rescue:    Heal effect targeting SingleAlly
/// - Summon:    Summon effect
/// - Mobility:  GrantMovement effect
/// - Defensive: applies a Buff-class status to self or ally
/// - ApplyCC:   applies HardCC or SoftCC status to an enemy target
/// - Peel:      applies a `forces_targeting` status to self or ally (taunt-redirect)
///
/// `status_lookup` provides pre-computed `StatusTagSet` per status id.
/// `status_defs` is consulted for Peel classification only (checking `forces_targeting`
/// raw flag, which is not part of `StatusTagSet`).
pub fn derive_ability_tags<L: StatusTagLookup>(
    def: &AbilityDef,
    status_lookup: &L,
    status_defs: &HashMap<StatusId, StatusDef>,
) -> AbilityTagSet {
    let mut s = AbilityTagSet::empty();

    // Offensive: any direct damage effect.
    if matches!(
        def.effect,
        EffectDef::WeaponAttack { .. } | EffectDef::Damage { .. } | EffectDef::SpellDamage { .. }
    ) {
        s.insert_tag(AbilityTag::Offensive);
    }

    // Rescue: heal effect targeted at a single ally.
    if def.target_type == TargetType::SingleAlly && matches!(def.effect, EffectDef::Heal { .. }) {
        s.insert_tag(AbilityTag::Rescue);
    }

    // Summon.
    if matches!(def.effect, EffectDef::Summon { .. }) {
        s.insert_tag(AbilityTag::Summon);
    }

    // Mobility.
    if matches!(def.effect, EffectDef::GrantMovement { .. }) {
        s.insert_tag(AbilityTag::Mobility);
    }

    // Defensive: ability applies a Buff status to self or to an ally.
    let applies_buff_to_protected = def.statuses.iter().any(|sa| {
        let is_protected_target = sa.on == StatusOn::MySelf
            || (sa.on == StatusOn::Target && def.target_type == TargetType::SingleAlly)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::Myself);
        is_protected_target
            && status_lookup
                .get_tags(&sa.status)
                .contains_tag(StatusTag::Buff)
    });
    if applies_buff_to_protected {
        s.insert_tag(AbilityTag::Defensive);
    }

    // ApplyCC: applies HardCC or SoftCC to an enemy (SingleEnemy or Ground AoE).
    let applies_cc_to_enemy = def.statuses.iter().any(|sa| {
        if sa.on != StatusOn::Target {
            return false;
        }
        if !matches!(
            def.target_type,
            TargetType::SingleEnemy | TargetType::Ground
        ) {
            return false;
        }
        let tags = status_lookup.get_tags(&sa.status);
        tags.contains_tag(StatusTag::HardCC) || tags.contains_tag(StatusTag::SoftCC)
    });
    if applies_cc_to_enemy {
        s.insert_tag(AbilityTag::ApplyCC);
    }

    // Peel: applies a `forces_targeting = true` status (taunt-redirect) to self or ally.
    // Checked via raw `forces_targeting` flag in status_defs (not via StatusTagSet —
    // forces_targeting is intentionally not a StatusTag).
    let applies_taunt = def.statuses.iter().any(|sa| {
        let to_ally_or_self = sa.on == StatusOn::MySelf
            || (sa.on == StatusOn::Target && def.target_type == TargetType::SingleAlly)
            || (sa.on == StatusOn::Target && def.target_type == TargetType::Myself);
        to_ally_or_self
            && status_defs
                .get(&sa.status)
                .is_some_and(|sd| sd.forces_targeting)
    });
    if applies_taunt {
        s.insert_tag(AbilityTag::Peel);
    }

    s
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
