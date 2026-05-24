//! AbilityTagCache / StatusTagCache as Bevy Resources.
//!
//! Built once at scenario load via `build_caches(content)`, stored as
//! Resources beside `ActiveContent`. Lookup is HashMap O(1).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::content::content_view::ContentView;
use combat_engine::{AbilityId, StatusId};

use super::classify::{derive_ability_tags, derive_status_tags, StatusTagLookup};
use super::{AbilityTagSet, StatusTagSet};

// ── StatusTagCache ────────────────────────────────────────────────────────────

/// Numeric bonuses carried by a status definition. Stored alongside
/// `StatusTagSet` in `StatusTagCache` so `refresh_aggregates` can read both
/// tags and bonuses from a single cache lookup without needing `&ContentView`.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusBonuses {
    pub speed_bonus: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
}

#[derive(Resource, Default, Debug, Clone)]
pub struct StatusTagCache {
    pub map: HashMap<StatusId, StatusTagSet>,
    pub bonuses: HashMap<StatusId, StatusBonuses>,
}

impl StatusTagCache {
    pub fn get(&self, id: &StatusId) -> StatusTagSet {
        self.map.get(id).copied().unwrap_or_default()
    }

    /// Return the numeric bonuses for the given status id, or zero defaults
    /// when the id is not in the cache.
    pub fn bonuses(&self, id: &StatusId) -> StatusBonuses {
        self.bonuses.get(id).copied().unwrap_or_default()
    }
}

impl StatusTagLookup for StatusTagCache {
    fn get_tags(&self, id: &StatusId) -> StatusTagSet {
        self.get(id)
    }
}

// ── AbilityTagCache ───────────────────────────────────────────────────────────

#[derive(Resource, Default, Debug, Clone)]
pub struct AbilityTagCache {
    /// Derived tags (without override).
    pub map: HashMap<AbilityId, AbilityTagSet>,
    /// Explicit overrides parsed at load time (replace-not-append semantics).
    /// Only populated for abilities that have `ai_tags_override` set.
    pub override_map: HashMap<AbilityId, AbilityTagSet>,
}

impl AbilityTagCache {
    /// Raw derived tags (override ignored).
    pub fn get(&self, id: &AbilityId) -> AbilityTagSet {
        self.map.get(id).copied().unwrap_or_default()
    }

    /// Effective tags = override if present, else derived.
    /// Central place for replace-not-append semantics.
    pub fn effective(&self, id: &AbilityId) -> AbilityTagSet {
        self.override_map
            .get(id)
            .copied()
            .unwrap_or_else(|| self.get(id))
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build both caches from a content view.
///
/// Order matters: `StatusTagCache` is built first (no deps), then
/// `AbilityTagCache` uses it for Defensive / ApplyCC / Peel classification.
/// Override strings are parsed here (fail-fast on unknown tag names).
pub fn build_caches(content: &ContentView) -> (StatusTagCache, AbilityTagCache) {
    // Pass 1: classify all statuses — tags and numeric bonuses in one sweep.
    let mut status_map: HashMap<StatusId, StatusTagSet> = HashMap::new();
    let mut bonuses_map: HashMap<StatusId, StatusBonuses> = HashMap::new();
    for (id, def) in &content.statuses {
        status_map.insert(id.clone(), derive_status_tags(def));
        bonuses_map.insert(id.clone(), StatusBonuses {
            speed_bonus: def.speed_bonus,
            armor_bonus: def.armor_bonus,
            damage_taken_bonus: def.damage_taken_bonus,
        });
    }

    // Pass 2: classify all abilities using the status map just built.
    let mut ability_map: HashMap<AbilityId, AbilityTagSet> = HashMap::new();
    for (id, def) in &content.abilities {
        let tags = derive_ability_tags(def, &status_map, &content.statuses);
        ability_map.insert(id.clone(), tags);
    }

    // Pass 3: parse override strings → AbilityTagSet (fail-fast on unknown).
    let mut override_map: HashMap<AbilityId, AbilityTagSet> = HashMap::new();
    for (id, def) in &content.abilities {
        if let Some(names) = &def.ai_tags_override {
            let tags = parse_override(names, id);
            override_map.insert(id.clone(), tags);
        }
    }

    (
        StatusTagCache { map: status_map, bonuses: bonuses_map },
        AbilityTagCache { map: ability_map, override_map },
    )
}

/// Parse a list of tag-name strings into an `AbilityTagSet`.
/// Panics on unknown tag names — fail-fast for content authoring errors.
fn parse_override(names: &[String], ability_id: &AbilityId) -> AbilityTagSet {
    use super::AbilityTag;
    let mut s = AbilityTagSet::empty();
    for n in names {
        let t = AbilityTag::from_name(n).unwrap_or_else(|| {
            panic!(
                "ability '{}': unknown ai_tags_override entry '{}' \
                 (known: offensive defensive rescue summon mobility apply_cc peel)",
                ability_id, n
            )
        });
        s.insert_tag(t);
    }
    s
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;
    use combat_engine::AbilityId;

    fn load_content() -> ContentView {
        ContentView::load_global_for_tests()
    }

    // ── Commit 2 tests ────────────────────────────────────────────────────────

    #[test]
    fn build_caches_global_content_covers_all_abilities() {
        let content = load_content();
        let (sc, ac) = build_caches(&content);
        assert_eq!(
            ac.map.len(),
            content.abilities.len(),
            "ability cache must have one entry per ability"
        );
        assert_eq!(
            sc.map.len(),
            content.statuses.len(),
            "status cache must have one entry per status"
        );
    }

    #[test]
    fn build_caches_status_first_then_abilities_dependency_satisfied() {
        // `taunt` ability relies on `taunted.forces_targeting` and `defending.buff_class`
        // being present in the status lookup when ability classification runs.
        // If the order were reversed, `taunt` would not get PEEL.
        let content = load_content();
        let (_, ac) = build_caches(&content);
        let tags = ac.get(&AbilityId::from("taunt"));
        use crate::combat::ai::world::tags::AbilityTag;
        assert!(
            tags.contains_tag(AbilityTag::Peel),
            "taunt must have PEEL — status classification must run before ability"
        );
        assert!(
            tags.contains_tag(AbilityTag::Defensive),
            "taunt must have DEFENSIVE"
        );
    }

    #[test]
    fn build_caches_is_idempotent() {
        let content = load_content();
        let (sc1, ac1) = build_caches(&content);
        let (sc2, ac2) = build_caches(&content);
        assert_eq!(sc1.map, sc2.map, "status cache must be idempotent");
        assert_eq!(ac1.map, ac2.map, "ability cache must be idempotent");
        assert_eq!(ac1.override_map, ac2.override_map, "override_map must be idempotent");
    }

    // ── Commit 3 tests ────────────────────────────────────────────────────────

    #[test]
    fn override_replaces_derived_not_appends() {
        use crate::combat::ai::world::tags::AbilityTagSet;

        let mut content = load_content();
        // melee_attack derived = OFFENSIVE; override to DEFENSIVE only
        let ability_id = AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&ability_id) {
            def.ai_tags_override = Some(vec!["defensive".to_string()]);
        }
        let (_, ac) = build_caches(&content);
        let effective = ac.effective(&ability_id);
        assert_eq!(effective, AbilityTagSet::DEFENSIVE, "override must replace, not append");
        assert!(!effective.contains(AbilityTagSet::OFFENSIVE), "OFFENSIVE must be absent");
    }

    #[test]
    fn override_empty_vec_results_in_empty_tag_set() {
        let mut content = load_content();
        let ability_id = AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&ability_id) {
            def.ai_tags_override = Some(vec![]);
        }
        let (_, ac) = build_caches(&content);
        assert_eq!(
            ac.effective(&ability_id),
            AbilityTagSet::empty(),
            "empty override vec must yield empty tag set"
        );
    }

    #[test]
    fn override_none_uses_derived() {
        let content = load_content();
        let ability_id = AbilityId::from("melee_attack");
        // No override set in global content
        let (_, ac) = build_caches(&content);
        use crate::combat::ai::world::tags::AbilityTagSet;
        assert_eq!(
            ac.effective(&ability_id),
            AbilityTagSet::OFFENSIVE,
            "None override must use derived tags"
        );
    }

    #[test]
    #[should_panic(expected = "unknown ai_tags_override")]
    fn override_unknown_tag_panics() {
        let mut content = load_content();
        let ability_id = AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&ability_id) {
            def.ai_tags_override = Some(vec!["bogus_tag".to_string()]);
        }
        let _ = build_caches(&content);
    }

    #[test]
    fn override_multi_tag_combines() {
        let mut content = load_content();
        let ability_id = AbilityId::from("melee_attack");
        if let Some(def) = content.abilities.get_mut(&ability_id) {
            def.ai_tags_override = Some(vec!["offensive".to_string(), "peel".to_string()]);
        }
        let (_, ac) = build_caches(&content);
        use crate::combat::ai::world::tags::{AbilityTag, AbilityTagSet};
        let tags = ac.effective(&ability_id);
        assert!(tags.contains_tag(AbilityTag::Offensive));
        assert!(tags.contains_tag(AbilityTag::Peel));
        assert_eq!(tags, AbilityTagSet::OFFENSIVE | AbilityTagSet::PEEL);
    }
}
