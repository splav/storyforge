#![allow(clippy::too_many_arguments)]

pub mod combat_scene;
pub mod input;

use crate::app_state::{AppState, CombatPhase};
use crate::content::content_view::ActiveContent;
use crate::content::encounters::OnDefeat;
use crate::content::scenarios::SceneDef;
use crate::content::settings::GameSettings;
use crate::game::components::{Combatant, Faction, Team, Vital};
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::persistence::save_repo;
use crate::persistence::PersistencePaths;
use bevy::prelude::*;

#[derive(Message)]
pub struct AdvanceScenario;

pub fn start_scenario(mut commands: Commands, mut next_state: ResMut<NextState<AppState>>) {
    commands.spawn(Camera2d);
    next_state.set(AppState::MainMenu);
}

/// Initialize ScenarioState for the given scenario id at scene 0.
pub fn enter_scenario(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
) {
    enter_scenario_at(commands, db, next_state, scenario_id, 0);
}

pub fn enter_scenario_at(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
    scene_index: usize,
) {
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found"));
    assert!(
        scene_index < scen.scenes.len(),
        "scene_index {scene_index} out of range for scenario '{scenario_id}'"
    );
    let scene_index = skip_invisible(scen, scene_index).unwrap_or_else(|| {
        panic!("scenario '{scenario_id}' ends with only invisible scenes from index {scene_index}")
    });

    commands.insert_resource(ScenarioState {
        scenario_id: scenario_id.into(),
        scene_index,
    });
    // Publish the scenario's merged content view so combat systems see the
    // correct (possibly-overridden) abilities/statuses/weapons/etc.
    commands.insert_resource(ActiveContent(scen.content.clone()));

    // Build AI tag caches from the content view and insert them as Resources.
    // StatusTagCache is built first (no deps); AbilityTagCache uses it for
    // Defensive/ApplyCC/Peel classification.
    let (status_tags, ability_tags) =
        crate::combat::ai::world::tags::cache::build_caches(&scen.content);
    commands.insert_resource(status_tags);
    commands.insert_resource(ability_tags);

    match &scen.scenes[scene_index] {
        SceneDef::Story { .. } => next_state.set(AppState::Story),
        SceneDef::Combat { .. } => next_state.set(AppState::Combat),
    }
}

/// Walk forward past any invisible scenes (story with `lines = []`, used as a
/// pure party-change beat). Returns `None` if we ran off the end.
fn skip_invisible(scen: &crate::content::scenarios::ScenarioDef, mut idx: usize) -> Option<usize> {
    while idx < scen.scenes.len() {
        if scen.scenes[idx].is_invisible() {
            idx += 1;
            continue;
        }
        return Some(idx);
    }
    None
}

pub fn advance_scenario_system(
    mut commands: Commands,
    mut events: MessageReader<AdvanceScenario>,
    scenario: Option<ResMut<ScenarioState>>,
    campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
) {
    let Some(mut scenario) = scenario else { return };

    if events.read().next().is_none() {
        return;
    }
    events.clear();
    {
        scenario.scene_index += 1;

        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        // Skip invisible story beats (lines = [], used for party changes between visible scenes).
        match skip_invisible(scen, scenario.scene_index) {
            Some(idx) => scenario.scene_index = idx,
            None => {
                scenario.scene_index = scen.scenes.len();
            }
        }
        if scenario.scene_index >= scen.scenes.len() {
            // Scenario finished. Advance within campaign or end it.
            if let Some(mut camp_state) = campaign {
                camp_state.scenario_index += 1;
                let camp = db.campaigns.get(&camp_state.campaign_id).unwrap();
                if camp_state.scenario_index < camp.scenario_ids.len() {
                    let next_id = camp.scenario_ids[camp_state.scenario_index].clone();
                    enter_scenario(&mut commands, &db, &mut next_state, &next_id);
                    write_autosave(
                        paths.as_deref(),
                        settings.current_slot,
                        &camp_state,
                        &next_id,
                        0,
                    );
                    return;
                }
                // Campaign finished.
                let finished_id = camp_state.campaign_id.clone();
                commands.remove_resource::<CampaignState>();
                if let Some(p) = paths.as_deref() {
                    if let Err(e) =
                        save_repo::clear_campaign(&p.0, settings.current_slot, &finished_id)
                    {
                        warn!("failed to clear completed campaign from slot: {e}");
                    }
                }
            }
            next_state.set(AppState::MainMenu);
            return;
        }

        match &scen.scenes[scenario.scene_index] {
            SceneDef::Story { .. } => next_state.set(AppState::Story),
            SceneDef::Combat { .. } => next_state.set(AppState::Combat),
        }

        if let Some(camp) = campaign.as_deref() {
            write_autosave(
                paths.as_deref(),
                settings.current_slot,
                camp,
                &scenario.scenario_id,
                scenario.scene_index,
            );
        }
    }
}

fn write_autosave(
    paths: Option<&PersistencePaths>,
    slot: u8,
    campaign: &CampaignState,
    scenario_id: &str,
    scene_index: usize,
) {
    let Some(p) = paths else { return };
    if let Err(e) = save_repo::record_progress(&p.0, slot, campaign, scenario_id, scene_index) {
        warn!("autosave failed: {e}");
    }
}

/// On entering `CombatPhase::Victory`, collect `on_victory_flags` from the just-won
/// combat scene and insert them into `CampaignState.flags`.
///
/// Runs on `OnEnter(CombatPhase::Victory)` — fires *before* the player presses Space
/// to advance, so flags are already in `CampaignState` when `write_autosave` is called
/// from `advance_scenario_system`.
pub fn write_victory_flags(
    scenario: Option<Res<ScenarioState>>,
    mut campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
) {
    let (Some(scenario), Some(campaign)) = (scenario, campaign.as_mut()) else {
        return;
    };
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    if let SceneDef::Combat { on_victory_flags, .. } = &scen.scenes[scenario.scene_index] {
        for flag in on_victory_flags {
            campaign.flags.insert(flag.clone());
        }
    }
}

/// On combat end (victory OR proceed-defeat), evaluate this encounter's secondary
/// `objectives` against the final ECS state and record each MET objective's `id`
/// into `CampaignState.flags`.
///
/// Registered on BOTH `OnEnter(CombatPhase::Victory)` and `OnEnter(CombatPhase::Defeat)`.
/// On defeat it records only when `on_defeat == Proceed` (a `Retry` defeat restarts
/// combat → no progression). Runs the same frame combat ends — strictly before
/// `advance_scenario_system` autosaves (which only fires on a later `AdvanceScenario`).
pub fn write_objective_flags(
    scenario: Option<Res<ScenarioState>>,
    mut campaign: Option<ResMut<CampaignState>>,
    db: Res<GameDb>,
    phase: Res<State<CombatPhase>>,
    combatants: Query<(&Vital, &Faction), With<Combatant>>,
    named_vitals: Query<(&Name, &Vital)>,
) {
    let (Some(scenario), Some(campaign)) = (scenario, campaign.as_mut()) else {
        return;
    };
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let SceneDef::Combat { encounter_id, .. } = &scen.scenes[scenario.scene_index] else {
        return;
    };
    let Some(enc) = scen.encounters.get(encounter_id.as_str()) else {
        return;
    };
    if enc.objectives.is_empty() {
        return;
    }
    // Retry-defeat → combat restarts → record nothing.
    if *phase.get() == CombatPhase::Defeat && enc.on_defeat == OnDefeat::Retry {
        return;
    }

    let enemies_alive = combatants
        .iter()
        .any(|(v, f)| v.is_alive() && f.0 == Team::Enemy);
    let is_named_alive = |name: &str| -> bool {
        named_vitals.iter().any(|(n, v)| n.as_str() == name && v.is_alive())
    };
    for obj in &enc.objectives {
        if crate::combat::advance_turn::objective_met(&obj.condition, enemies_alive, &is_named_alive) {
            campaign.flags.insert(obj.id.clone());
        }
    }
}

/// The `on_defeat` policy of the encounter in the current combat scene.
/// Returns `Retry` when the current scene isn't combat or the encounter is absent.
pub fn current_on_defeat(db: &GameDb, scenario: &ScenarioState) -> OnDefeat {
    let Some(scen) = db.scenarios.get(&scenario.scenario_id) else {
        return OnDefeat::Retry;
    };
    let SceneDef::Combat { encounter_id, .. } = &scen.scenes[scenario.scene_index] else {
        return OnDefeat::Retry;
    };
    scen.encounters
        .get(encounter_id.as_str())
        .map(|e| e.on_defeat)
        .unwrap_or(OnDefeat::Retry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;
    use crate::content::encounters::{EncounterDef, ObjectiveDef, VictoryCondition, DEFAULT_TARGET_MARKER};
    use crate::content::scenarios::ScenarioDef;
    use crate::game::components::Vital;
    use std::collections::HashMap;

    fn minimal_scenario(id: &str, flags: Vec<&str>) -> ScenarioDef {
        ScenarioDef {
            id: id.into(),
            name: id.into(),
            party: vec![],
            scenes: vec![SceneDef::Combat {
                encounter_id: "enc".into(),
                location: None,
                on_victory_flags: flags.into_iter().map(str::to_string).collect(),
            }],
            content: ContentView::default(),
            encounters: HashMap::new(),
        }
    }

    /// Build a scenario with one encounter carrying one `KeepAlive` objective.
    fn scenario_with_objective(on_defeat: OnDefeat) -> ScenarioDef {
        let obj = ObjectiveDef {
            id: "boat_saved".into(),
            condition: VictoryCondition::KeepAlive {
                target_name: "Лодка".into(),
                marker_color: DEFAULT_TARGET_MARKER,
            },
            hidden: true,
        };
        let enc = EncounterDef {
            id: "enc".into(),
            name: "enc".into(),
            enemies: vec![],
            victory: VictoryCondition::AllEnemiesDead,
            obstacles: vec![],
            environment: vec![],
            on_defeat,
            objectives: vec![obj],
        };
        let mut encounters = HashMap::new();
        encounters.insert("enc".into(), enc);
        ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party: vec![],
            scenes: vec![SceneDef::Combat {
                encounter_id: "enc".into(),
                location: None,
                on_victory_flags: vec![],
            }],
            content: ContentView::default(),
            encounters,
        }
    }

    fn alive_vital() -> Vital {
        Vital { hp: 10, max_hp: 10, armor: 0 }
    }

    fn dead_vital() -> Vital {
        Vital { hp: 0, max_hp: 10, armor: 0 }
    }

    fn base_app(phase: CombatPhase, on_defeat: OnDefeat) -> App {
        let scenario = scenario_with_objective(on_defeat);
        let db = make_db(scenario);
        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.insert_resource(State::new(phase));
        app.add_systems(Update, write_objective_flags);
        app
    }

    fn make_db(scenario: ScenarioDef) -> GameDb {
        let mut db = GameDb {
            scenarios: HashMap::new(),
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };
        db.scenarios.insert(scenario.id.clone(), scenario);
        db
    }

    /// `write_victory_flags` inserts all `on_victory_flags` of the current
    /// combat scene into `CampaignState.flags`.
    #[test]
    fn victory_flags_written_to_campaign_state() {
        let scenario = minimal_scenario("s1", vec!["found_token", "kael_found"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.add_systems(Update, write_victory_flags);
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.contains("found_token"), "found_token should be in flags");
        assert!(flags.contains("kael_found"), "kael_found should be in flags");
        assert_eq!(flags.len(), 2);
    }

    /// `write_victory_flags` is idempotent — running twice yields the same set.
    #[test]
    fn victory_flags_idempotent() {
        let scenario = minimal_scenario("s1", vec!["flag_x"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.add_systems(Update, write_victory_flags);
        app.update();
        app.update(); // second call — must not duplicate

        let flags = &app.world().resource::<CampaignState>().flags;
        assert_eq!(flags.len(), 1);
    }

    /// `write_victory_flags` is a no-op when `CampaignState` is absent
    /// (e.g., standalone scenario run without a campaign wrapper).
    #[test]
    fn victory_flags_no_campaign_state_is_noop() {
        let scenario = minimal_scenario("s1", vec!["flag_a"]);
        let db = make_db(scenario);

        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState {
            scenario_id: "s1".into(),
            scene_index: 0,
        });
        // No CampaignState inserted — must not panic.
        app.add_systems(Update, write_victory_flags);
        app.update();
    }

    // ── write_objective_flags tests ──────────────────────────────────────────

    /// Victory + boat alive → flag recorded.
    #[test]
    fn objective_flags_victory_met() {
        let mut app = base_app(CombatPhase::Victory, OnDefeat::Retry);
        app.world_mut().spawn((
            Name::new("Лодка"),
            alive_vital(),
            Faction(Team::Player),
            Combatant,
        ));
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.contains("boat_saved"), "boat_saved should be set");
        assert_eq!(flags.len(), 1);
    }

    /// Victory + boat dead → flag NOT recorded.
    #[test]
    fn objective_flags_victory_not_met() {
        let mut app = base_app(CombatPhase::Victory, OnDefeat::Retry);
        app.world_mut().spawn((
            Name::new("Лодка"),
            dead_vital(),
            Faction(Team::Player),
            Combatant,
        ));
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(!flags.contains("boat_saved"));
    }

    /// Defeat + Proceed + boat alive → flag recorded.
    #[test]
    fn objective_flags_defeat_proceed_met() {
        let mut app = base_app(CombatPhase::Defeat, OnDefeat::Proceed);
        app.world_mut().spawn((
            Name::new("Лодка"),
            alive_vital(),
            Faction(Team::Player),
            Combatant,
        ));
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.contains("boat_saved"), "proceed-defeat should record met objective");
    }

    /// Defeat + Retry + boat alive → flag NOT recorded (retry = combat restarts).
    #[test]
    fn objective_flags_defeat_retry_records_nothing() {
        let mut app = base_app(CombatPhase::Defeat, OnDefeat::Retry);
        app.world_mut().spawn((
            Name::new("Лодка"),
            alive_vital(),
            Faction(Team::Player),
            Combatant,
        ));
        app.update();

        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.is_empty(), "retry-defeat must not record flags");
    }

    /// Encounter with no objectives → system is a no-op, no panic.
    #[test]
    fn objective_flags_no_objectives_is_noop() {
        // Use minimal_scenario which has an empty objectives vec and encounters map.
        let scenario = minimal_scenario("s1", vec![]);
        let db = make_db(scenario);
        let mut app = App::new();
        app.insert_resource(db);
        app.insert_resource(ScenarioState { scenario_id: "s1".into(), scene_index: 0 });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: std::collections::BTreeSet::new(),
        });
        app.insert_resource(State::new(CombatPhase::Victory));
        app.add_systems(Update, write_objective_flags);
        app.update();
        // No panic, no flags.
        let flags = &app.world().resource::<CampaignState>().flags;
        assert!(flags.is_empty());
    }

    // ── current_on_defeat ─────────────────────────────────────────────────────

    /// Returns `Proceed` for an encounter declared with `on_defeat: Proceed`.
    #[test]
    fn current_on_defeat_returns_proceed() {
        let scenario = scenario_with_objective(OnDefeat::Proceed);
        let db = make_db(scenario);
        let state = ScenarioState { scenario_id: "s1".into(), scene_index: 0 };
        assert_eq!(current_on_defeat(&db, &state), OnDefeat::Proceed);
    }

    /// Returns `Retry` for an encounter declared with the default `on_defeat`.
    #[test]
    fn current_on_defeat_returns_retry_for_default() {
        let scenario = scenario_with_objective(OnDefeat::Retry);
        let db = make_db(scenario);
        let state = ScenarioState { scenario_id: "s1".into(), scene_index: 0 };
        assert_eq!(current_on_defeat(&db, &state), OnDefeat::Retry);
    }

    /// Returns `Retry` when the scenario id is unknown.
    #[test]
    fn current_on_defeat_unknown_scenario_returns_retry() {
        let scenario = scenario_with_objective(OnDefeat::Proceed);
        let db = make_db(scenario);
        let state = ScenarioState { scenario_id: "no_such_scenario".into(), scene_index: 0 };
        assert_eq!(current_on_defeat(&db, &state), OnDefeat::Retry);
    }

    /// Returns `Retry` when the scene is Story (not Combat).
    #[test]
    fn current_on_defeat_story_scene_returns_retry() {
        let mut scenario = scenario_with_objective(OnDefeat::Proceed);
        // Replace the single combat scene with a story scene.
        scenario.scenes = vec![SceneDef::Story {
            lines: vec![],
            party_add: vec![],
            party_remove: vec![],
        }];
        let db = make_db(scenario);
        let state = ScenarioState { scenario_id: "s1".into(), scene_index: 0 };
        assert_eq!(current_on_defeat(&db, &state), OnDefeat::Retry);
    }

    /// Returns `Retry` when the encounter_id is not in the encounters map.
    #[test]
    fn current_on_defeat_missing_encounter_returns_retry() {
        let mut scenario = scenario_with_objective(OnDefeat::Proceed);
        scenario.encounters.clear();
        let db = make_db(scenario);
        let state = ScenarioState { scenario_id: "s1".into(), scene_index: 0 };
        assert_eq!(current_on_defeat(&db, &state), OnDefeat::Retry);
    }

    // ── advance_scenario_system ───────────────────────────────────────────────

    /// `AdvanceScenario` message increments `scene_index` regardless of phase.
    #[test]
    fn advance_scenario_increments_scene_index() {
        use crate::content::scenarios::DialogueLine;
        use crate::content::settings::GameSettings;
        // Build a scenario with two scenes so scene_index 0 → 1 is valid.
        // The second scene must be non-empty (visible) so skip_invisible stops there.
        let mut scenario = scenario_with_objective(OnDefeat::Proceed);
        scenario.scenes.push(SceneDef::Story {
            lines: vec![DialogueLine {
                speaker: "x".into(),
                text: "y".into(),
                requires_flag: None,
            }],
            party_add: vec![],
            party_remove: vec![],
        });
        let db = make_db(scenario);

        let mut app = App::new();
        app.add_message::<AdvanceScenario>();
        app.insert_resource(db);
        app.insert_resource(ScenarioState { scenario_id: "s1".into(), scene_index: 0 });
        app.insert_resource(GameSettings::default());
        // advance_scenario_system needs NextState<AppState>; insert directly to avoid
        // requiring StatesPlugin (which needs DefaultPlugins).
        app.insert_resource(NextState::<AppState>::default());
        // Writer system runs before advance; fires one message each update.
        app.add_systems(Update, (
            |mut w: MessageWriter<AdvanceScenario>| { w.write(AdvanceScenario); },
            advance_scenario_system,
        ).chain());
        app.update();

        let idx = app.world().resource::<ScenarioState>().scene_index;
        assert_eq!(idx, 1, "scene_index should advance from 0 to 1");
    }
}
