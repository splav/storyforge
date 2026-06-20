use crate::content::campaigns::{load_campaigns, CampaignDef};
use crate::content::content_view::ActiveContentData;
use crate::content::scenarios::ScenarioDef;
use bevy::prelude::*;
use combat_engine::StatusId;
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

/// Static obstacle hexes for the currently active combat encounter.
/// Populated from `EncounterDef.obstacles` in `spawn_combatants` and
/// pushed into `CombatState.blocked_hexes` during `bootstrap_combat_state`.
/// Cleared on restart/exit together with the engine mirrors.
#[derive(Resource, Default, Clone)]
pub struct CombatBlockedHexes(pub Vec<hexx::Hex>);

/// Environmental hazard objects for the currently active combat encounter.
/// Populated from `EncounterDef.environment` in `spawn_combatants` and
/// pushed into `CombatState.environment` during `bootstrap_combat_state`.
/// Cleared on restart/exit together with the engine mirrors.
#[derive(Resource, Default, Clone)]
pub struct CombatEnvironment(pub Vec<combat_engine::state::EnvObject>);

/// Active round-based phase deadline. Set when a boss phase carrying a
/// `turn_limit` activates; checked each Finalize by `check_phase_deadline_system`.
/// `None` when no timed phase is active. Reset to `None` on combat (re)start.
#[derive(Resource, Default)]
pub struct PhaseDeadline(pub Option<PhaseDeadlineState>);

#[derive(Clone, Debug)]
pub struct PhaseDeadlineState {
    /// `CombatContext.round` value at the moment the phase activated.
    pub phase_started_round: u32,
    /// Number of rounds the player has to satisfy the new objective.
    pub limit: u32,
}

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
        let mut q = TurnQueue {
            order: vec![dummy(0), dummy(1)],
            index: 1,
        };
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
        let q = TurnQueue {
            order: vec![e],
            index: 0,
        };
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
        let campaign_order: Vec<String> = loaded.campaigns.iter().map(|c| c.id.clone()).collect();
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

fn validate_content(scen_id: &str, c: &ActiveContentData) {
    use crate::content::weapons::HandType;

    // Abilities → statuses.
    for (id, def) in &c.abilities {
        for sa in &def.statuses {
            assert!(
                c.statuses.contains_key(&sa.status),
                "scenario '{scen_id}' ability '{}' references unknown status '{}'",
                id,
                sa.status,
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
        assert!(
            c.armor.contains_key(&cls.chest),
            "scenario '{scen_id}' class '{id}' unknown chest '{}'",
            cls.chest
        );
        assert!(
            c.armor.contains_key(&cls.legs),
            "scenario '{scen_id}' class '{id}' unknown legs '{}'",
            cls.legs
        );
        assert!(
            c.armor.contains_key(&cls.feet),
            "scenario '{scen_id}' class '{id}' unknown feet '{}'",
            cls.feet
        );
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
        assert!(
            c.races.contains_key(&t.race),
            "{label}: unknown race '{}'",
            t.race
        );
        if let Some(ref f) = t.faction {
            assert!(c.factions.contains_key(f), "{label}: unknown faction '{f}'");
        }
        if let Some(ref p) = t.path {
            assert!(c.paths.contains_key(p), "{label}: unknown path '{p}'");
        }
        assert!(
            c.weapons.contains_key(&t.equipment.main_hand),
            "{label}: unknown weapon '{}'",
            t.equipment.main_hand
        );
        if let Some(ref oh) = t.equipment.off_hand {
            assert!(
                c.weapons.contains_key(oh),
                "{label}: unknown off-hand weapon '{oh}'"
            );
        }
        assert!(
            c.armor.contains_key(&t.equipment.chest),
            "{label}: unknown chest '{}'",
            t.equipment.chest
        );
        assert!(
            c.armor.contains_key(&t.equipment.legs),
            "{label}: unknown legs '{}'",
            t.equipment.legs
        );
        assert!(
            c.armor.contains_key(&t.equipment.feet),
            "{label}: unknown feet '{}'",
            t.equipment.feet
        );
        for aid in &t.ability_ids {
            assert!(
                c.abilities.contains_key(aid),
                "{label}: unknown ability '{aid}'"
            );
        }
    }
}

/// Recursively validate every name reference in a `VictoryCondition` tree
/// (`KillTarget.enemy_name`, `KeepAlive.target_name`) against the encounter's
/// enemies / active party. Fails fast at load time so a typo can't cause silent
/// instant-defeat (F10) or instant-victory (F17).
fn validate_victory_names(
    cond: &crate::content::encounters::VictoryCondition,
    enc: &crate::content::encounters::EncounterDef,
    party_names: &std::collections::HashSet<&str>,
    label: &str,
) {
    use crate::content::encounters::VictoryCondition;
    match cond {
        VictoryCondition::AllEnemiesDead => {}
        VictoryCondition::KillTarget { enemy_name, .. } => {
            let matches = enc.enemies.iter().filter(|e| &e.name == enemy_name).count();
            assert!(
                matches == 1,
                "{label} victory KillTarget references '{enemy_name}' \
                 (matches in encounter enemies: {matches} — must be exactly one)",
            );
        }
        VictoryCondition::KeepAlive { target_name, .. } => {
            let in_enemies = enc.enemies.iter().any(|e| &e.name == target_name);
            let in_party = party_names.contains(target_name.as_str());
            assert!(
                in_enemies || in_party,
                "{label} victory KeepAlive references '{target_name}' — \
                 must match an enemy or an active-party member; \
                 enemy names: {:?}; party names: {:?}",
                enc.enemies
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>(),
                party_names,
            );
        }
        VictoryCondition::AllOf(conds) => {
            for c in conds {
                validate_victory_names(c, enc, party_names, label);
            }
        }
    }
}

fn validate_scenario(scen_id: &str, scen: &ScenarioDef) {
    let c = &scen.content;

    // Encounters — enemies, phases, auras, victory.
    for (enc_id, enc) in &scen.encounters {
        let label = format!("scenario '{scen_id}' encounter '{enc_id}'");
        for enemy in &enc.enemies {
            assert!(
                crate::game::hex::in_bounds(enemy.hex_pos),
                "{label} enemy '{}': spawn hex {:?} is outside the battlefield \
                 (even rows 0..6 → cols 0..6, odd rows → cols 0..7)",
                enemy.name,
                crate::game::hex::hex_to_offset(enemy.hex_pos),
            );
            assert!(
                c.races.contains_key(&enemy.race),
                "{label} enemy '{}': unknown race",
                enemy.name
            );
            if let Some(ref fac) = enemy.faction {
                assert!(
                    c.factions.contains_key(fac),
                    "{label} enemy '{}': unknown faction '{fac}'",
                    enemy.name
                );
            }
            if let Some(ref p) = enemy.path {
                assert!(
                    c.paths.contains_key(p),
                    "{label} enemy '{}': unknown path '{p}'",
                    enemy.name
                );
            }
            assert!(
                c.weapons.contains_key(&enemy.main_hand),
                "{label} enemy '{}': unknown weapon '{}'",
                enemy.name,
                enemy.main_hand
            );
            if let Some(ref oh) = enemy.off_hand {
                assert!(
                    c.weapons.contains_key(oh),
                    "{label} enemy '{}': unknown off-hand weapon '{oh}'",
                    enemy.name
                );
            }
            if let Some(w) = c.weapons.get(&enemy.main_hand) {
                if w.hand == crate::content::weapons::HandType::TwoHanded {
                    assert!(
                        enemy.off_hand.is_none(),
                        "{label} enemy '{}': weapon '{}' is two-handed but off_hand is set",
                        enemy.name,
                        enemy.main_hand,
                    );
                }
            }
            assert!(
                c.armor.contains_key(&enemy.chest),
                "{label} enemy '{}': unknown chest",
                enemy.name
            );
            assert!(
                c.armor.contains_key(&enemy.legs),
                "{label} enemy '{}': unknown legs",
                enemy.name
            );
            assert!(
                c.armor.contains_key(&enemy.feet),
                "{label} enemy '{}': unknown feet",
                enemy.name
            );
            for aid in &enemy.ability_ids {
                assert!(
                    c.abilities.contains_key(aid),
                    "{label} enemy '{}': unknown ability '{aid}'",
                    enemy.name
                );
            }
            for (i, ph) in enemy.phases.iter().enumerate() {
                if let Some(ability_ids) = &ph.ability_ids {
                    for aid in ability_ids {
                        assert!(
                            c.abilities.contains_key(aid),
                            "{label} enemy '{}' phase {i}: unknown ability '{aid}'",
                            enemy.name
                        );
                    }
                }
            }
            if let Some(aura) = &enemy.aura {
                assert!(
                    c.statuses.contains_key(&aura.status),
                    "{label} enemy '{}': aura references unknown status '{}'",
                    enemy.name,
                    aura.status,
                );
            }
        }

        // Static field objects must also sit inside the battlefield.
        for obs in &enc.obstacles {
            assert!(
                crate::game::hex::in_bounds(*obs),
                "{label}: obstacle at {:?} is outside the battlefield",
                crate::game::hex::hex_to_offset(*obs),
            );
        }
        for env in &enc.environment {
            assert!(
                crate::game::hex::in_bounds(env.hex),
                "{label}: environment object at {:?} is outside the battlefield",
                crate::game::hex::hex_to_offset(env.hex),
            );
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

        // Victory name references (KillTarget / KeepAlive / nested AllOf) are
        // validated below in the combat-scene loop where active party is known.
    }

    // Party members (starting + party_add from story scenes).
    let all_members = scen
        .party
        .iter()
        .chain(scen.scenes.iter().flat_map(|s| match s {
            crate::content::scenarios::SceneDef::Story { party_add, .. } => {
                party_add.iter().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        }));
    for member in all_members {
        assert!(
            crate::game::hex::in_bounds(member.hex_pos),
            "scenario '{scen_id}' party '{}': spawn hex {:?} is outside the battlefield",
            member.name,
            crate::game::hex::hex_to_offset(member.hex_pos),
        );
        assert!(
            c.races.contains_key(&member.race),
            "scenario '{scen_id}' party '{}': unknown race",
            member.name
        );
        if let Some(ref fac) = member.faction {
            assert!(
                c.factions.contains_key(fac),
                "scenario '{scen_id}' party '{}': unknown faction",
                member.name
            );
        }
        if let Some(ref p) = member.path {
            assert!(
                c.paths.contains_key(p),
                "scenario '{scen_id}' party '{}': unknown path",
                member.name
            );
        }
        if let Some(ref tpl) = member.template {
            // Template-based party member (e.g. non-acting NPC ally): stats /
            // class / equipment come from unit_templates, `class_id` is unused.
            assert!(
                c.unit_templates.contains_key(tpl),
                "scenario '{scen_id}' party '{}': unknown template '{tpl}'",
                member.name,
            );
        } else {
            assert!(
                c.classes.contains_key(&member.class_id),
                "scenario '{scen_id}' party '{}': unknown class '{}'",
                member.name,
                member.class_id,
            );
        }
    }

    // status_ops validation in story scenes.
    for (scene_idx, scene) in scen.scenes.iter().enumerate() {
        if let crate::content::scenarios::SceneDef::Story { status_ops, .. } = scene {
            // Statuses take effect on advance → the unit must be present in the party
            // AFTER this scene is processed (i.e. scene_idx + 1).
            let post_advance_party = crate::content::scenarios::active_party(scen, scene_idx + 1);
            let post_names: std::collections::HashSet<&str> =
                post_advance_party.iter().map(|m| m.name.as_str()).collect();

            for op in status_ops {
                assert!(
                    post_names.contains(op.unit_name()),
                    "scenario '{scen_id}' scene {scene_idx}: \
                     status op unit '{}' is not in the active party after this scene; \
                     party: {:?}",
                    op.unit_name(),
                    post_names,
                );
                assert!(
                    c.statuses.contains_key(op.status_id()),
                    "scenario '{scen_id}' scene {scene_idx}: \
                     status op references unknown status '{}'",
                    op.status_id(),
                );
            }
        }
    }

    // Choice scene validation.
    for (scene_idx, scene) in scen.scenes.iter().enumerate() {
        if let crate::content::scenarios::SceneDef::Choice { options, .. } = scene {
            assert!(
                !options.is_empty(),
                "scenario '{scen_id}' scene {scene_idx}: choice has no options",
            );
            for opt in options {
                assert!(
                    !opt.set_flag.is_empty(),
                    "scenario '{scen_id}' scene {scene_idx}: choice option '{}' has empty set_flag",
                    opt.label,
                );
            }
        }
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

                // Victory name references — checked here so active_party is in scope.
                let party_names: std::collections::HashSet<&str> =
                    party.iter().map(|m| m.name.as_str()).collect();
                let vic_label =
                    format!("scenario '{scen_id}' scene {scene_idx} encounter '{encounter_id}'");
                validate_victory_names(&enc.victory, enc, &party_names, &vic_label);

                // Phase victory_override validation: names + KillTarget self-reference guard.
                for enemy in &enc.enemies {
                    for (i, ph) in enemy.phases.iter().enumerate() {
                        if let Some(ov) = &ph.victory_override {
                            validate_victory_names(ov, enc, &party_names, &vic_label);
                            // Marker-attachment guard: an overriding KillTarget MUST name
                            // the phasing enemy itself (its post-phase name), otherwise the
                            // VictoryTarget marker won't attach and the combat is unwinnable.
                            if let crate::content::encounters::VictoryCondition::KillTarget {
                                enemy_name,
                                ..
                            } = ov
                            {
                                let phase_name = ph.name.as_deref().unwrap_or(enemy.name.as_str());
                                assert!(
                                    enemy_name == phase_name,
                                    "{vic_label} enemy '{}' phase {i}: victory_override KillTarget targets \
                                     '{enemy_name}' but must target the phasing enemy itself ('{phase_name}')",
                                    enemy.name,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Validation tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod validate_party_status_tests {
    use crate::content::content_view::ActiveContentData;
    use crate::content::scenarios::{PartyMemberDef, PartyStatusOp, ScenarioDef, SceneDef};
    use crate::game::resources::GameDb;
    use std::collections::HashMap;

    fn member(name: &str) -> PartyMemberDef {
        PartyMemberDef {
            id: name.to_ascii_lowercase(),
            name: name.into(),
            race: "human".into(),
            faction: None,
            path: None,
            class_id: "warrior".into(),
            hex_pos: hexx::Hex::ZERO,
            template: None,
            sprite: None,
            gender: Default::default(),
        }
    }

    /// Real global content (races / classes / weapons / armor / statuses, incl.
    /// `injured`) so party-member validation passes *before* status-op validation
    /// runs. Hand-building a consistent class equipment chain is brittle; load the
    /// actual `assets/data` view instead.
    fn valid_content() -> ActiveContentData {
        ActiveContentData::load_global_for_tests()
    }

    fn scenario_with_status_ops(
        status_ops: Vec<PartyStatusOp>,
        party: Vec<PartyMemberDef>,
    ) -> GameDb {
        let scen = ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party,
            scenes: vec![SceneDef::Story {
                lines: vec![],
                party_add: vec![],
                party_remove: vec![],
                status_ops,
                requires_flag: None,
                no_camp: false,
            }],
            content: valid_content(),
            encounters: HashMap::new(),
        };
        let mut db = GameDb {
            scenarios: HashMap::new(),
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };
        db.scenarios.insert("s1".into(), scen);
        db
    }

    /// `validate_scenario` panics when a `status_add` entry names a unit not in the
    /// post-advance party.
    #[test]
    #[should_panic(expected = "not in the active party")]
    fn validate_status_add_unknown_unit_panics() {
        let db = scenario_with_status_ops(
            vec![PartyStatusOp::Add {
                unit_name: "Nobody".into(),
                status_id: "injured".into(),
            }],
            vec![member("Alice")],
        );
        db.validate();
    }

    /// `validate_scenario` panics when a `Remove` op names a unit not in
    /// the post-advance party.
    #[test]
    #[should_panic(expected = "not in the active party")]
    fn validate_status_remove_unknown_unit_panics() {
        let db = scenario_with_status_ops(
            vec![PartyStatusOp::Remove {
                unit_name: "Nobody".into(),
                status_id: "injured".into(),
            }],
            vec![member("Alice")],
        );
        db.validate();
    }

    /// `validate_scenario` panics when an op references a status id
    /// that doesn't exist in content.
    #[test]
    #[should_panic(expected = "unknown status")]
    fn validate_status_add_unknown_status_id_panics() {
        let db = scenario_with_status_ops(
            vec![PartyStatusOp::Add {
                unit_name: "Alice".into(),
                status_id: "no_such_status".into(),
            }],
            vec![member("Alice")],
        );
        db.validate();
    }

    /// `validate_scenario` passes for a valid `Add` op naming a known party member
    /// with a status present in content.
    #[test]
    fn validate_status_valid_passes() {
        let db = scenario_with_status_ops(
            vec![PartyStatusOp::Add {
                unit_name: "Alice".into(),
                status_id: "injured".into(),
            }],
            vec![member("Alice")],
        );
        db.validate(); // must not panic
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
    /// Persistent flags accumulated across combat victories (and future story choices).
    /// Written on `OnEnter(CombatPhase::Victory)`, before autosave.
    #[serde(default)]
    pub flags: std::collections::BTreeSet<String>,
    /// Flat party-wide item stash (gear not currently equipped).
    #[serde(default)]
    pub stash: Vec<crate::content::item_ref::ItemRef>,
    /// Per-hero equipment overrides keyed by the hero's stable slug id.
    /// Overrides the class default on combat spawn. Missing entry → class default.
    #[serde(default)]
    pub loadouts: std::collections::HashMap<String, crate::content::item_ref::EquipmentSave>,
}

// ── Hex positions ────────────────────────────────────────────────────────────

/// Spatial index for the **occupancy layer** — alive units only.
///
/// One-per-hex invariant; dead units live in [`HexCorpses`]. Pathfinder /
/// legality / occupancy queries read this.
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

/// Spatial index for the **corpse layer** — dead units.
///
/// Multiple corpses per hex are allowed (unlike [`HexPositions`]). Occupancy
/// queries read [`HexPositions`]; rendering and corpse-targeting effects (future
/// resurrect / cleave / loot) read this index.
#[derive(Resource, Default)]
pub struct HexCorpses {
    by_entity: HashMap<Entity, hexx::Hex>,
    by_pos: HashMap<hexx::Hex, Vec<Entity>>,
    pub generation: u64,
}

impl HexCorpses {
    pub fn insert(&mut self, entity: Entity, pos: hexx::Hex) {
        if let Some(&old_pos) = self.by_entity.get(&entity) {
            if old_pos == pos {
                return;
            }
            // Corpses are stationary by design. If this fires, either engine
            // semantics changed (push/drag-body) or a writer mis-routed an alive
            // entity here. Release builds still re-link safely.
            debug_assert!(
                false,
                "HexCorpses: entity {entity:?} moved from {old_pos:?} to {pos:?} — \
                 corpses are stationary by design",
            );
            self.by_pos
                .entry(old_pos)
                .and_modify(|v| v.retain(|&e| e != entity));
        }
        self.by_entity.insert(entity, pos);
        self.by_pos.entry(pos).or_default().push(entity);
        self.generation += 1;
    }

    pub fn remove(&mut self, entity: &Entity) {
        if let Some(pos) = self.by_entity.remove(entity) {
            self.by_pos
                .entry(pos)
                .and_modify(|v| v.retain(|&e| &e != entity));
        }
        self.generation += 1;
    }

    /// Returns all corpses at the given hex.
    pub fn at(&self, pos: &hexx::Hex) -> &[Entity] {
        self.by_pos.get(pos).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Reverse lookup: which hex does this corpse occupy?
    pub fn get(&self, entity: &Entity) -> Option<hexx::Hex> {
        self.by_entity.get(entity).copied()
    }

    pub fn clear(&mut self) {
        self.by_entity.clear();
        self.by_pos.clear();
        self.generation += 1;
    }
}

// ── UI selection ─────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct SelectionState {
    pub selected_actor: Option<Entity>,
    pub selected_ability: Option<combat_engine::AbilityId>,
    pub selected_target: Option<Entity>,
    pub move_mode: bool,
    /// Unit currently shown in the inspection panel.
    /// Completely disjoint from the command-flow fields above —
    /// NOT diffed by `DirtyBridgePrev`; changes are signalled via `UiDirtyFlags::INSPECT`.
    pub inspected: Option<Entity>,
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
        const FORECAST      = 0b10_0000_0000;
        const STATUS_BADGES = 0b100_0000_0000;
        const INSPECT       = 0b1000_0000_0000;
    }
}

#[derive(Resource, Default)]
pub struct UiDirty(pub UiDirtyFlags);

// ── Action Forecast ─────────────────────────────────────────────────────────

/// What kind of outcome a forecast entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForecastKind {
    Damage,
    Heal,
}

/// One affected unit's expected outcome for the pending ability cast.
#[derive(Debug, Clone)]
pub struct ForecastEntry {
    /// Bevy entity this forecast applies to.
    pub entity: Entity,
    pub kind: ForecastKind,
    /// Expected delta (damage dealt or HP restored), always positive.
    pub amount: i32,
    pub hp_before: i32,
    /// `max(0, hp_before - amount)` for damage; `min(max_hp, hp_before + amount)` for heal.
    pub hp_after: i32,
    /// `true` if `UnitDied` was present for this unit in the preview events.
    pub lethal: bool,
    /// Statuses that will be applied to this unit.
    pub statuses: Vec<StatusId>,
}

/// Expected combat outcomes for the currently selected ability + hovered target.
///
/// Populated by `compute_forecast` (gated on `UiDirtyFlags::FORECAST`).
/// Cleared when no valid (actor, ability, target) triple is present.
#[derive(Resource, Default)]
pub struct ActionForecast {
    pub entries: Vec<ForecastEntry>,
    /// Flat per-cast crit-fail chance in percent (5 for a d20 roll).
    pub crit_fail_pct: u8,
}

impl ActionForecast {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.crit_fail_pct = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod validate_choice_tests {
    use crate::content::content_view::ActiveContentData;
    use crate::content::scenarios::{ChoiceOption, ScenarioDef, SceneDef};
    use crate::game::resources::GameDb;
    use std::collections::HashMap;

    fn choice_scenario(options: Vec<ChoiceOption>) -> (GameDb, String) {
        let scen = ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party: vec![],
            scenes: vec![SceneDef::Choice {
                prompt: vec![],
                options,
                requires_flag: None,
            }],
            content: ActiveContentData::default(),
            encounters: HashMap::new(),
        };
        let mut db = GameDb {
            scenarios: HashMap::new(),
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };
        let id = scen.id.clone();
        db.scenarios.insert(id.clone(), scen);
        (db, id)
    }

    /// `validate_scenario` panics for a `Choice` scene with no options.
    #[test]
    #[should_panic(expected = "choice has no options")]
    fn validate_choice_empty_options_panics() {
        let (db, _) = choice_scenario(vec![]);
        db.validate();
    }

    /// `validate_scenario` panics when a choice option has an empty `set_flag`.
    #[test]
    #[should_panic(expected = "has empty set_flag")]
    fn validate_choice_empty_set_flag_panics() {
        let (db, _) = choice_scenario(vec![ChoiceOption {
            label: "Go".into(),
            set_flag: "".into(),
        }]);
        db.validate();
    }

    /// `validate_scenario` passes for a well-formed choice scene.
    #[test]
    fn validate_choice_valid_passes() {
        let (db, _) = choice_scenario(vec![
            ChoiceOption {
                label: "Help".into(),
                set_flag: "helped".into(),
            },
            ChoiceOption {
                label: "Ignore".into(),
                set_flag: "ignored".into(),
            },
        ]);
        db.validate(); // must not panic
    }
}
