use crate::content::campaigns::{load_campaigns, CampaignDef};
use crate::content::content_view::ContentView;
use crate::content::scenarios::ScenarioDef;
use bevy::prelude::*;
use std::collections::HashMap;

// ── Initiative preset (carry initiative across a combat restart) ─────────────

/// Populated before a combat restart. Maps combatant name → saved initiative value.
/// `build_turn_order` reads this on round 1 instead of rolling, then clears it.
#[derive(Resource, Default)]
pub struct PresetInitiative(pub HashMap<String, i32>);

// ── Combat runtime ───────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct CombatContext {
    pub round: u32,
    pub encounter: Option<Entity>,
}

/// Victory condition for the currently active combat. Reset on scene enter/restart.
#[derive(Resource, Default, Clone)]
pub struct CombatObjective(pub crate::content::encounters::VictoryCondition);

#[derive(Resource, Default)]
pub struct TurnQueue {
    pub order: Vec<Entity>,
    pub index: usize,
}

impl TurnQueue {
    pub fn current(&self) -> Option<Entity> {
        self.order.get(self.index).copied()
    }

    pub fn advance(&mut self) {
        self.index = (self.index + 1) % self.order.len().max(1);
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use bevy::prelude::Entity;

    fn dummy(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    #[test]
    fn current_none_on_empty_queue() {
        assert!(TurnQueue::default().current().is_none());
    }

    #[test]
    fn advance_wraps_around() {
        let mut q = TurnQueue { order: vec![dummy(0), dummy(1)], index: 1 };
        q.advance();
        assert_eq!(q.index, 0);
    }

    #[test]
    fn advance_on_empty_stays_zero() {
        let mut q = TurnQueue::default();
        q.advance();
        assert_eq!(q.index, 0);
    }

    #[test]
    fn current_returns_active_entity() {
        let e = dummy(42);
        let q = TurnQueue { order: vec![e], index: 0 };
        assert_eq!(q.current(), Some(e));
    }
}

// ── Game rules database ──────────────────────────────────────────────────────

/// Metadata-only: campaigns + scenarios. All rule content (abilities/statuses/
/// weapons/armor/classes/unit_templates/races/factions/paths) lives in each
/// scenario's `content: ContentView` and is surfaced at runtime via
/// `ActiveContent`, which is populated on scenario entry.
#[derive(Resource)]
pub struct GameDb {
    pub scenarios: HashMap<String, ScenarioDef>,
    pub campaigns: HashMap<String, CampaignDef>,
    /// Campaigns in deterministic order (folder name, alphabetic) for menu rendering.
    pub campaign_order: Vec<String>,
}

impl Default for GameDb {
    fn default() -> Self {
        let loaded = load_campaigns();
        let campaign_order: Vec<String> =
            loaded.campaigns.iter().map(|c| c.id.clone()).collect();
        let db = Self {
            scenarios: loaded.scenarios,
            campaigns: loaded
                .campaigns
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect(),
            campaign_order,
        };
        db.validate();
        db
    }
}

impl GameDb {
    /// Runs per-scenario content validation + scene/encounter references + hex collisions.
    fn validate(&self) {
        for (camp_id, camp) in &self.campaigns {
            assert!(
                !camp.scenario_ids.is_empty(),
                "campaign '{camp_id}' has no scenarios",
            );
            for sid in &camp.scenario_ids {
                assert!(
                    self.scenarios.contains_key(sid),
                    "campaign '{camp_id}' references unknown scenario '{sid}'",
                );
            }
        }

        for (scen_id, scen) in &self.scenarios {
            validate_content(scen_id, &scen.content);
            validate_scenario(scen_id, scen);
        }
    }
}

fn validate_content(scen_id: &str, c: &ContentView) {
    use crate::content::weapons::HandType;

    // Abilities → statuses.
    for (id, def) in &c.abilities {
        for sa in &def.statuses {
            assert!(
                c.statuses.contains_key(&sa.status),
                "scenario '{scen_id}' ability '{}' references unknown status '{}'",
                id, sa.status,
            );
        }
    }

    // Classes → weapons, armor, abilities.
    for (id, cls) in &c.classes {
        assert!(
            c.weapons.contains_key(&cls.main_hand),
            "scenario '{scen_id}' class '{id}' references unknown weapon '{}'",
            cls.main_hand,
        );
        if let Some(ref oh) = cls.off_hand {
            assert!(
                c.weapons.contains_key(oh),
                "scenario '{scen_id}' class '{id}' references unknown off-hand weapon '{oh}'",
            );
        }
        if let Some(w) = c.weapons.get(&cls.main_hand) {
            if w.hand == HandType::TwoHanded {
                assert!(
                    cls.off_hand.is_none(),
                    "scenario '{scen_id}' class '{id}': weapon '{}' is two-handed but off_hand is set",
                    cls.main_hand,
                );
            }
        }
        assert!(c.armor.contains_key(&cls.chest), "scenario '{scen_id}' class '{id}' unknown chest '{}'", cls.chest);
        assert!(c.armor.contains_key(&cls.legs), "scenario '{scen_id}' class '{id}' unknown legs '{}'", cls.legs);
        assert!(c.armor.contains_key(&cls.feet), "scenario '{scen_id}' class '{id}' unknown feet '{}'", cls.feet);
        for aid in &cls.abilities {
            assert!(
                c.abilities.contains_key(aid),
                "scenario '{scen_id}' class '{id}' references unknown ability '{aid}'",
            );
        }
    }

    // Unit templates.
    for (id, t) in &c.unit_templates {
        let label = format!("scenario '{scen_id}' unit_template '{id}'");
        assert!(c.races.contains_key(&t.race), "{label}: unknown race '{}'", t.race);
        if let Some(ref f) = t.faction {
            assert!(c.factions.contains_key(f), "{label}: unknown faction '{f}'");
        }
        if let Some(ref p) = t.path {
            assert!(c.paths.contains_key(p), "{label}: unknown path '{p}'");
        }
        assert!(c.weapons.contains_key(&t.equipment.main_hand), "{label}: unknown weapon '{}'", t.equipment.main_hand);
        if let Some(ref oh) = t.equipment.off_hand {
            assert!(c.weapons.contains_key(oh), "{label}: unknown off-hand weapon '{oh}'");
        }
        assert!(c.armor.contains_key(&t.equipment.chest), "{label}: unknown chest '{}'", t.equipment.chest);
        assert!(c.armor.contains_key(&t.equipment.legs), "{label}: unknown legs '{}'", t.equipment.legs);
        assert!(c.armor.contains_key(&t.equipment.feet), "{label}: unknown feet '{}'", t.equipment.feet);
        for aid in &t.ability_ids {
            assert!(c.abilities.contains_key(aid), "{label}: unknown ability '{aid}'");
        }
    }
}

fn validate_scenario(scen_id: &str, scen: &ScenarioDef) {
    let c = &scen.content;

    // Encounters — enemies, phases, auras, victory.
    for (enc_id, enc) in &scen.encounters {
        let label = format!("scenario '{scen_id}' encounter '{enc_id}'");
        for enemy in &enc.enemies {
            assert!(c.races.contains_key(&enemy.race), "{label} enemy '{}': unknown race", enemy.name);
            if let Some(ref fac) = enemy.faction {
                assert!(c.factions.contains_key(fac), "{label} enemy '{}': unknown faction '{fac}'", enemy.name);
            }
            if let Some(ref p) = enemy.path {
                assert!(c.paths.contains_key(p), "{label} enemy '{}': unknown path '{p}'", enemy.name);
            }
            assert!(c.weapons.contains_key(&enemy.main_hand), "{label} enemy '{}': unknown weapon '{}'", enemy.name, enemy.main_hand);
            if let Some(ref oh) = enemy.off_hand {
                assert!(c.weapons.contains_key(oh), "{label} enemy '{}': unknown off-hand weapon '{oh}'", enemy.name);
            }
            if let Some(w) = c.weapons.get(&enemy.main_hand) {
                if w.hand == crate::content::weapons::HandType::TwoHanded {
                    assert!(
                        enemy.off_hand.is_none(),
                        "{label} enemy '{}': weapon '{}' is two-handed but off_hand is set",
                        enemy.name, enemy.main_hand,
                    );
                }
            }
            assert!(c.armor.contains_key(&enemy.chest), "{label} enemy '{}': unknown chest", enemy.name);
            assert!(c.armor.contains_key(&enemy.legs), "{label} enemy '{}': unknown legs", enemy.name);
            assert!(c.armor.contains_key(&enemy.feet), "{label} enemy '{}': unknown feet", enemy.name);
            for aid in &enemy.ability_ids {
                assert!(c.abilities.contains_key(aid), "{label} enemy '{}': unknown ability '{aid}'", enemy.name);
            }
            for (i, ph) in enemy.phases.iter().enumerate() {
                if let Some(ability_ids) = &ph.ability_ids {
                    for aid in ability_ids {
                        assert!(c.abilities.contains_key(aid), "{label} enemy '{}' phase {i}: unknown ability '{aid}'", enemy.name);
                    }
                }
            }
            if let Some(aura) = &enemy.aura {
                assert!(
                    c.statuses.contains_key(&aura.status),
                    "{label} enemy '{}': aura references unknown status '{}'",
                    enemy.name, aura.status,
                );
            }
        }

        // Hex collisions within encounter.
        {
            let mut seen = std::collections::HashSet::new();
            for enemy in &enc.enemies {
                assert!(
                    seen.insert(enemy.hex_pos),
                    "{label}: enemies share hex position {:?}",
                    enemy.hex_pos,
                );
            }
        }

        // Victory kill_target.
        if let crate::content::encounters::VictoryCondition::KillTarget { enemy_name, .. } =
            &enc.victory
        {
            let matches = enc.enemies.iter().filter(|e| &e.name == enemy_name).count();
            assert!(
                matches == 1,
                "{label} victory=kill_target references '{enemy_name}' (matches: {matches} — must be exactly one)",
            );
        }
    }

    // Party members (starting + party_add from story scenes).
    let all_members = scen.party.iter().chain(scen.scenes.iter().flat_map(|s| match s {
        crate::content::scenarios::SceneDef::Story { party_add, .. } => party_add.iter().collect::<Vec<_>>(),
        _ => Vec::new(),
    }));
    for member in all_members {
        assert!(c.races.contains_key(&member.race), "scenario '{scen_id}' party '{}': unknown race", member.name);
        if let Some(ref fac) = member.faction {
            assert!(c.factions.contains_key(fac), "scenario '{scen_id}' party '{}': unknown faction", member.name);
        }
        if let Some(ref p) = member.path {
            assert!(c.paths.contains_key(p), "scenario '{scen_id}' party '{}': unknown path", member.name);
        }
        assert!(c.classes.contains_key(&member.class_id), "scenario '{scen_id}' party '{}': unknown class '{}'", member.name, member.class_id);
    }

    // Scene encounter refs + party-vs-enemy hex collisions.
    for (scene_idx, scene) in scen.scenes.iter().enumerate() {
        if let crate::content::scenarios::SceneDef::Combat { encounter_id, .. } = scene {
            assert!(
                scen.encounters.contains_key(encounter_id.as_str()),
                "scenario '{scen_id}' scene {scene_idx}: unknown encounter '{encounter_id}'",
            );
            let party = crate::content::scenarios::active_party(scen, scene_idx);
            let mut positions = std::collections::HashSet::new();
            for member in &party {
                assert!(
                    positions.insert(member.hex_pos),
                    "scenario '{scen_id}' scene {scene_idx}: party share hex {:?}",
                    member.hex_pos,
                );
            }
            if let Some(enc) = scen.encounters.get(encounter_id.as_str()) {
                for enemy in &enc.enemies {
                    assert!(
                        !positions.contains(&enemy.hex_pos),
                        "scenario '{scen_id}' encounter '{encounter_id}': enemy '{}' at {:?} overlaps party",
                        enemy.name, enemy.hex_pos,
                    );
                }
            }
        }
    }
}

// ── Scenario state ──────────────────────────────────────────────────────────

#[derive(Resource, serde::Serialize, serde::Deserialize, Clone)]
pub struct ScenarioState {
    pub scenario_id: String,
    pub scene_index: usize,
}

#[derive(Resource, serde::Serialize, serde::Deserialize, Clone)]
pub struct CampaignState {
    pub campaign_id: String,
    pub scenario_index: usize,
}

// ── Hex positions ────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct HexPositions {
    by_entity: HashMap<Entity, hexx::Hex>,
    by_pos: HashMap<hexx::Hex, Entity>,
    pub generation: u64,
}

impl HexPositions {
    pub fn insert(&mut self, entity: Entity, pos: hexx::Hex) {
        debug_assert!(
            self.by_pos.get(&pos).is_none_or(|&e| e == entity),
            "HexPositions: position {pos:?} already occupied by another entity",
        );
        if let Some(&old_pos) = self.by_entity.get(&entity) {
            self.by_pos.remove(&old_pos);
        }
        self.by_entity.insert(entity, pos);
        self.by_pos.insert(pos, entity);
        self.generation += 1;
    }

    pub fn remove(&mut self, entity: &Entity) {
        if let Some(pos) = self.by_entity.remove(entity) {
            self.by_pos.remove(&pos);
        }
        self.generation += 1;
    }

    pub fn clear(&mut self) {
        self.by_entity.clear();
        self.by_pos.clear();
        self.generation += 1;
    }

    pub fn get(&self, entity: &Entity) -> Option<hexx::Hex> {
        self.by_entity.get(entity).copied()
    }

    pub fn entity_at(&self, pos: hexx::Hex) -> Option<Entity> {
        self.by_pos.get(&pos).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Entity, &hexx::Hex)> {
        self.by_entity.iter()
    }
}

// ── UI selection ─────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct SelectionState {
    pub selected_actor: Option<Entity>,
    pub selected_ability: Option<combat_engine::AbilityId>,
    pub selected_target: Option<Entity>,
    pub move_mode: bool,
}

impl SelectionState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

// ── UI dirty flags ──────────────────────────────────────────────────────────

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct UiDirtyFlags: u16 {
        const OVERLAY       = 0b0000_0001;
        const HEX_FILL      = 0b0000_0010;
        const LABELS        = 0b0000_0100;
        const ABILITY_PANEL = 0b0000_1000;
        const TURN_ORDER    = 0b0001_0000;
        const PHASE_HINT    = 0b0010_0000;
        const MOVE_BTN      = 0b0100_0000;
        const TOOLTIP       = 0b1000_0000;
        const TOKENS        = 0b1_0000_0000;
    }
}

#[derive(Resource, Default)]
pub struct UiDirty(pub UiDirtyFlags);
