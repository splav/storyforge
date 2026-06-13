//! Minimal Bevy-free [`ContentView`] stub for engine and replay tests that need
//! a `ContentView` without booting a Bevy app.
//!
//! Historically this file ALSO parsed `assets/data/*.toml` directly — a
//! duplicate of the app-side parser in `src/content/*`. That duplication was
//! removed: the app parser is the single source of content parsing, and
//! `tests/content_parse_snapshot.rs` guards its output. What remains is the
//! empty view used by engine/replay tests via [`TomlContentView::empty`].

use std::collections::HashMap;

use crate::{
    content::{AbilityDef, ContentView, StatusDef, UnitTemplate},
    AbilityId, StatusId,
};

/// Bevy-free [`ContentView`] backed by in-memory maps. Only the empty form is
/// constructed (see [`TomlContentView::empty`]); it exists so engine and replay
/// tests can pass a `ContentView` without a Bevy app.
#[derive(Default)]
pub struct TomlContentView {
    abilities: HashMap<AbilityId, AbilityDef>,
    statuses: HashMap<StatusId, StatusDef>,
    unit_templates: HashMap<String, UnitTemplate>,
}

impl TomlContentView {
    /// Empty view — returns `None` / defaults for every query.
    pub fn empty() -> Self {
        Self::default()
    }
}

impl ContentView for TomlContentView {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.get(id)
    }

    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        self.statuses.get(id)
    }

    fn unit_template(&self, id: &str) -> Option<UnitTemplate> {
        self.unit_templates.get(id).cloned()
    }
}
