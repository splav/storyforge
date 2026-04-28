//! Pure classifier: shape → tag set.
//!
//! `derive_status_tags` is one-pass (no dependencies).
//! `derive_ability_tags` requires a status-tag lookup (for Defensive / ApplyCC / Peel
//! which depend on what statuses the ability applies). Caller builds the
//! StatusTagCache first, then passes it into ability classification.

use std::collections::HashMap;

use crate::content::abilities::{AbilityDef, EffectDef, StatusOn, TargetType};
use crate::content::statuses::StatusDef;
use crate::core::StatusId;

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
/// - HardCC:  `skips_turn`
/// - SoftCC:  `causes_disadvantage || speed_bonus < 0`
/// - Dot:     `dot_dice.is_some() || hp_percent_dot > 0`
/// - Buff:    `buff_class.is_some() || armor_bonus > 0`
/// - Cosmetic: fallback when none of the above apply
///
/// Note: `forces_targeting` is intentionally not mapped to any StatusTag —
/// it's a raw shape flag used by the ability-side Peel classifier via `status_defs`.
pub fn derive_status_tags(def: &StatusDef) -> StatusTagSet {
    let mut s = StatusTagSet::empty();
    if def.skips_turn {
        s.insert_tag(StatusTag::HardCC);
    }
    if def.causes_disadvantage || def.speed_bonus < 0 {
        s.insert_tag(StatusTag::SoftCC);
    }
    if def.dot_dice.is_some() || def.hp_percent_dot > 0 {
        s.insert_tag(StatusTag::Dot);
    }
    if def.buff_class.is_some() || def.armor_bonus > 0 {
        s.insert_tag(StatusTag::Buff);
    }
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
        EffectDef::WeaponAttack | EffectDef::Damage { .. } | EffectDef::SpellDamage { .. }
    ) {
        s.insert_tag(AbilityTag::Offensive);
    }

    // Rescue: heal effect targeted at a single ally.
    if def.target_type == TargetType::SingleAlly
        && matches!(def.effect, EffectDef::Heal { .. })
    {
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
        is_protected_target && status_lookup.get_tags(&sa.status).contains_tag(StatusTag::Buff)
    });
    if applies_buff_to_protected {
        s.insert_tag(AbilityTag::Defensive);
    }

    // ApplyCC: applies HardCC or SoftCC to an enemy (SingleEnemy or Ground AoE).
    let applies_cc_to_enemy = def.statuses.iter().any(|sa| {
        if sa.on != StatusOn::Target {
            return false;
        }
        if !matches!(def.target_type, TargetType::SingleEnemy | TargetType::Ground) {
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
mod tests {
    use super::*;
    use crate::core::AbilityId;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn load_content() -> crate::content::content_view::ContentView {
        crate::content::content_view::ContentView::load_global_for_tests()
    }

    fn build_status_lookup(
        content: &crate::content::content_view::ContentView,
    ) -> HashMap<StatusId, StatusTagSet> {
        content
            .statuses
            .iter()
            .map(|(id, def)| (id.clone(), derive_status_tags(def)))
            .collect()
    }

    fn ability(id: &str, content: &crate::content::content_view::ContentView) -> AbilityDef {
        content
            .abilities
            .get(&AbilityId::from(id))
            .unwrap_or_else(|| panic!("ability '{id}' not found in content"))
            .clone()
    }

    fn status_def(
        id: &str,
        content: &crate::content::content_view::ContentView,
    ) -> StatusDef {
        content
            .statuses
            .get(&StatusId::from(id))
            .unwrap_or_else(|| panic!("status '{id}' not found in content"))
            .clone()
    }

    // ── derive_status_tags pin tests ──────────────────────────────────────────

    #[test]
    fn derive_status_tags_for_defending() {
        let content = load_content();
        let def = status_def("defending", &content);
        // armor_bonus=4, buff_class=ArmorBuff → BUFF
        assert_eq!(derive_status_tags(&def), StatusTagSet::BUFF);
    }

    #[test]
    fn derive_status_tags_for_taunted() {
        let content = load_content();
        let def = status_def("taunted", &content);
        // forces_targeting=true — not a StatusTag; no other AI-side fields → COSMETIC
        assert_eq!(derive_status_tags(&def), StatusTagSet::COSMETIC);
    }

    #[test]
    fn derive_status_tags_for_stunned() {
        let content = load_content();
        let def = status_def("stunned", &content);
        // skips_turn=true → HARD_CC
        assert_eq!(derive_status_tags(&def), StatusTagSet::HARD_CC);
    }

    #[test]
    fn derive_status_tags_for_burning() {
        let content = load_content();
        let def = status_def("burning", &content);
        // damage_taken_bonus=1 — vulnerability, not dot_dice or hp_percent_dot → COSMETIC
        assert_eq!(derive_status_tags(&def), StatusTagSet::COSMETIC);
    }

    #[test]
    fn derive_status_tags_for_paralyzed() {
        let content = load_content();
        let def = status_def("paralyzed", &content);
        // skips_turn=true → HARD_CC
        assert_eq!(derive_status_tags(&def), StatusTagSet::HARD_CC);
    }

    #[test]
    fn derive_status_tags_for_poisoned() {
        let content = load_content();
        let def = status_def("poisoned", &content);
        // dot_count=1, dot_sides=4 → DOT
        assert_eq!(derive_status_tags(&def), StatusTagSet::DOT);
    }

    #[test]
    fn derive_status_tags_for_broken_faith() {
        let content = load_content();
        let def = status_def("broken_faith", &content);
        // blocks_mana_abilities=true — not in 5 status tags → COSMETIC
        assert_eq!(derive_status_tags(&def), StatusTagSet::COSMETIC);
    }

    #[test]
    fn derive_status_tags_for_exhaustion() {
        let content = load_content();
        let def = status_def("exhaustion", &content);
        // speed_bonus=-1 (SOFT_CC) + hp_percent_dot=5 (DOT)
        assert_eq!(
            derive_status_tags(&def),
            StatusTagSet::SOFT_CC | StatusTagSet::DOT
        );
    }

    #[test]
    fn derive_status_tags_for_pact_control() {
        let content = load_content();
        let def = status_def("pact_control", &content);
        // ai_controlled=true — not in 5 status tags → COSMETIC
        assert_eq!(derive_status_tags(&def), StatusTagSet::COSMETIC);
    }

    #[test]
    fn derive_status_tags_for_disoriented() {
        let content = load_content();
        let def = status_def("disoriented", &content);
        // causes_disadvantage=true → SOFT_CC
        assert_eq!(derive_status_tags(&def), StatusTagSet::SOFT_CC);
    }

    // ── derive_ability_tags pin tests ─────────────────────────────────────────

    #[test]
    fn derive_ability_tags_for_move() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("move", &content);
        // effect=ToggleMoveMode → empty
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::empty()
        );
    }

    #[test]
    fn derive_ability_tags_for_rest() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("rest", &content);
        // effect=RestoreResources → empty
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::empty()
        );
    }

    #[test]
    fn derive_ability_tags_for_melee_attack() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("melee_attack", &content);
        // effect=WeaponAttack, target=SingleEnemy → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_taunt() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("taunt", &content);
        // defending(on=target,target=myself) → DEFENSIVE (Buff via armor_bonus)
        // taunted(on=self) → PEEL (forces_targeting via status_defs)
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::DEFENSIVE | AbilityTagSet::PEEL
        );
    }

    #[test]
    fn derive_ability_tags_for_fireball() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("fireball", &content);
        // effect=SpellDamage, target=Ground → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_thunderstrike() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("thunderstrike", &content);
        // effect=SpellDamage, target=Ground → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_heal() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("heal", &content);
        // effect=Heal, target=SingleAlly → RESCUE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::RESCUE
        );
    }

    #[test]
    fn derive_ability_tags_for_flash() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("flash", &content);
        // effect=SpellDamage, target=SingleEnemy → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_burn() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("burn", &content);
        // statuses=[burning(Cosmetic) on=target]; no damage effect → empty
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::empty()
        );
    }

    #[test]
    fn derive_ability_tags_for_spark() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("spark", &content);
        // effect=SpellDamage, target=SingleEnemy → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_stun() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("stun", &content);
        // statuses=[stunned(HARD_CC) on=target, target=SingleEnemy] → APPLY_CC
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::APPLY_CC
        );
    }

    #[test]
    fn derive_ability_tags_for_backstab() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("backstab", &content);
        // effect=Damage; statuses=[poisoned(DOT) on=target] — DOT is not CC → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_rush() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("rush", &content);
        // effect=GrantMovement, target=Myself → MOBILITY
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::MOBILITY
        );
    }

    #[test]
    fn derive_ability_tags_for_field_medic() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("field_medic", &content);
        // effect=Heal, target=SingleAlly → RESCUE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::RESCUE
        );
    }

    #[test]
    fn derive_ability_tags_for_bow_shot() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("bow_shot", &content);
        // effect=Damage, target=SingleEnemy, range.min=2 → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_paralyzing_shot() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("paralyzing_shot", &content);
        // effect=Damage (OFFENSIVE) + paralyzed(HARD_CC) on=target, target=SingleEnemy → APPLY_CC
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE | AbilityTagSet::APPLY_CC
        );
    }

    #[test]
    fn derive_ability_tags_for_poison_shot() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("poison_shot", &content);
        // effect=Damage; statuses=[poisoned(DOT)] — DOT not CC → OFFENSIVE
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::OFFENSIVE
        );
    }

    #[test]
    fn derive_ability_tags_for_summon_storm_spirit() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("summon_storm_spirit", &content);
        // effect=Summon, target=Myself → SUMMON
        assert_eq!(
            derive_ability_tags(&def, &lookup, &content.statuses),
            AbilityTagSet::SUMMON
        );
    }

    // ── Special/regression tests ──────────────────────────────────────────────

    #[test]
    fn derive_ability_tags_taunt_has_peel_via_taunted_status() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("taunt", &content);

        // Verify Peel is present in the full result.
        let tags = derive_ability_tags(&def, &lookup, &content.statuses);
        assert!(tags.contains_tag(AbilityTag::Peel), "taunt must have PEEL");

        // Now test WITHOUT taunted.forces_targeting — Peel must disappear.
        let mut status_defs_modified = content.statuses.clone();
        if let Some(taunted) = status_defs_modified.get_mut(&StatusId::from("taunted")) {
            taunted.forces_targeting = false;
        }
        let tags_no_peel = derive_ability_tags(&def, &lookup, &status_defs_modified);
        assert!(
            !tags_no_peel.contains_tag(AbilityTag::Peel),
            "without forces_targeting, taunt must NOT have PEEL"
        );
        assert!(
            tags_no_peel.contains_tag(AbilityTag::Defensive),
            "Defensive must still be present (via defending status)"
        );
    }

    #[test]
    fn derive_ability_tags_paralyzing_shot_has_offensive_and_apply_cc() {
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("paralyzing_shot", &content);
        let tags = derive_ability_tags(&def, &lookup, &content.statuses);
        assert!(tags.contains_tag(AbilityTag::Offensive), "must have OFFENSIVE");
        assert!(tags.contains_tag(AbilityTag::ApplyCC), "must have APPLY_CC");
        // No other tags for paralyzing_shot
        assert_eq!(tags, AbilityTagSet::OFFENSIVE | AbilityTagSet::APPLY_CC);
    }

    #[test]
    fn classify_is_pure_no_io() {
        // Call classify twice on the same inputs — results must be identical.
        let content = load_content();
        let lookup = build_status_lookup(&content);
        let def = ability("taunt", &content);
        let r1 = derive_ability_tags(&def, &lookup, &content.statuses);
        let r2 = derive_ability_tags(&def, &lookup, &content.statuses);
        assert_eq!(r1, r2, "classifier must be deterministic / pure");
    }
}
