//! Minimal Bevy-free [`ContentView`] stub for engine and replay tests that need
//! a `ContentView` without booting a Bevy app. Only the empty form is used (see
//! [`TomlContentView::empty`]); content parsing lives solely in `src/content/*`.

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
