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
    flags: Option<&std::collections::BTreeSet<String>>,
) {
    enter_scenario_at(commands, db, next_state, scenario_id, 0, flags);
}

pub fn enter_scenario_at(
    commands: &mut Commands,
    db: &GameDb,
    next_state: &mut NextState<AppState>,
    scenario_id: &str,
    scene_index: usize,
    flags: Option<&std::collections::BTreeSet<String>>,
) {
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found"));
    assert!(
        scene_index < scen.scenes.len(),
        "scene_index {scene_index} out of range for scenario '{scenario_id}'"
    );

    let empty = std::collections::BTreeSet::new();
    let flags = flags.unwrap_or(&empty);

    // Resolve the first non-skipped scene from `scene_index` onward.
    // If all remaining scenes are gated/invisible (e.g. a save that lands on an
    // all-gated tail, or a non-campaign scenario with flag-gated scenes and no
    // active flags), finish gracefully — transition to MainMenu — rather than
    // panicking. The caller is responsible for having already inserted
    // CampaignState if needed; we do not advance the campaign index here.
    let resolved = skip_skipped(scen, scene_index, flags);
    let Some(scene_index) = resolved else {
        next_state.set(AppState::MainMenu);
        return;
    };

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
        SceneDef::Choice { .. } => next_state.set(AppState::Story),
    }
}

/// Returns true if the scene should be auto-skipped:
/// - invisible story beat (lines = [], used for party changes between visible scenes), OR
/// - flag-gated scene whose required flag is absent from `flags`.
fn should_skip(scene: &crate::content::scenarios::SceneDef, flags: &std::collections::BTreeSet<String>) -> bool {
    scene.is_invisible()
        || scene
            .requires_flag()
            .is_some_and(|f| !flags.contains(f))
}

/// Walk forward past any scenes that should be skipped (invisible story beats
/// or flag-gated scenes whose required flag is absent). Returns `None` if we
/// ran off the end.
fn skip_skipped(
    scen: &crate::content::scenarios::ScenarioDef,
    mut idx: usize,
    flags: &std::collections::BTreeSet<String>,
) -> Option<usize> {
    while idx < scen.scenes.len() {
        if should_skip(&scen.scenes[idx], flags) {
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
        let empty = std::collections::BTreeSet::new();
        let flags = campaign.as_deref().map(|c| &c.flags).unwrap_or(&empty);

        // Skip invisible story beats and flag-gated scenes whose required flag is absent.
        match skip_skipped(scen, scenario.scene_index, flags) {
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
                    enter_scenario(&mut commands, &db, &mut next_state, &next_id, Some(&camp_state.flags));
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
            SceneDef::Choice { .. } => next_state.set(AppState::Story),
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
                requires_flag: None,
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
                requires_flag: None,
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
            status_ops: vec![],
            requires_flag: None,
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
                excludes_flag: None,
            }],
            party_add: vec![],
            party_remove: vec![],
            status_ops: vec![],
            requires_flag: None,
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

    // ── requires_flag scene-level gating ─────────────────────────────────────

    /// Helper: build a SceneDef::Story with given requires_flag (no lines → invisible
    /// unless requires_flag is None, but that's intentional for some tests).
    fn gated_story(flag: Option<&str>) -> SceneDef {
        SceneDef::Story {
            lines: vec![crate::content::scenarios::DialogueLine {
                speaker: "X".into(),
                text: "scene text".into(),
                requires_flag: None,
                excludes_flag: None,
            }],
            party_add: vec![],
            party_remove: vec![],
            status_ops: vec![],
            requires_flag: flag.map(str::to_string),
        }
    }

    fn gated_combat(flag: Option<&str>, victory_flags: Vec<&str>) -> SceneDef {
        SceneDef::Combat {
            encounter_id: "enc".into(),
            location: None,
            on_victory_flags: victory_flags.into_iter().map(str::to_string).collect(),
            requires_flag: flag.map(str::to_string),
        }
    }

    fn flags(keys: &[&str]) -> std::collections::BTreeSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    fn scenario_from_scenes(scenes: Vec<SceneDef>) -> ScenarioDef {
        ScenarioDef {
            id: "s1".into(),
            name: "s1".into(),
            party: vec![],
            scenes,
            content: crate::content::content_view::ContentView::default(),
            encounters: HashMap::new(),
        }
    }

    // ── should_skip unit tests ────────────────────────────────────────────────

    /// A scene with requires_flag=None is never skipped due to flag gating.
    #[test]
    fn should_skip_none_flag_never_skips() {
        let scene = gated_story(None);
        assert!(!should_skip(&scene, &flags(&[])));
        assert!(!should_skip(&scene, &flags(&["x"])));
    }

    /// A scene with requires_flag=Some("x") is skipped when "x" absent, played when present.
    #[test]
    fn should_skip_flag_absent_skips_flag_present_plays() {
        let scene = gated_story(Some("x"));
        assert!(should_skip(&scene, &flags(&[])), "flag absent → skip");
        assert!(should_skip(&scene, &flags(&["y"])), "wrong flag → skip");
        assert!(!should_skip(&scene, &flags(&["x"])), "flag present → play");
    }

    /// is_invisible (lines=[]) still skips regardless of flag gate.
    #[test]
    fn should_skip_invisible_story_always_skips() {
        let invisible = SceneDef::Story {
            lines: vec![],
            party_add: vec![],
            party_remove: vec![],
            status_ops: vec![],
            requires_flag: None,
        };
        assert!(should_skip(&invisible, &flags(&[])));
        assert!(should_skip(&invisible, &flags(&["x"])));
    }

    // ── skip_skipped integration ──────────────────────────────────────────────

    /// skip_skipped returns the first non-gated, non-invisible index.
    /// Regression: existing linear behavior (no requires_flag) unchanged.
    #[test]
    fn skip_skipped_linear_no_flags_unchanged() {
        let scen = scenario_from_scenes(vec![gated_story(None), gated_story(None)]);
        assert_eq!(skip_skipped(&scen, 0, &flags(&[])), Some(0));
        assert_eq!(skip_skipped(&scen, 1, &flags(&[])), Some(1));
    }

    /// skip_skipped skips a gated scene when flag absent; stops at the ungated one.
    #[test]
    fn skip_skipped_hops_over_gated_scene() {
        let scen = scenario_from_scenes(vec![
            gated_story(Some("secret")), // index 0: gated
            gated_story(None),            // index 1: always visible
        ]);
        // Without flag: index 0 skipped, lands on 1.
        assert_eq!(skip_skipped(&scen, 0, &flags(&[])), Some(1));
        // With flag: index 0 plays.
        assert_eq!(skip_skipped(&scen, 0, &flags(&["secret"])), Some(0));
    }

    /// skip_skipped returns None when ALL remaining scenes are gated and flag absent.
    #[test]
    fn skip_skipped_all_gated_returns_none() {
        let scen = scenario_from_scenes(vec![
            gated_story(Some("x")),
            gated_story(Some("x")),
        ]);
        assert_eq!(skip_skipped(&scen, 0, &flags(&[])), None);
        // With flag: first scene is reachable.
        assert_eq!(skip_skipped(&scen, 0, &flags(&["x"])), Some(0));
    }

    // ── branch: choice sets flag, downstream scene gated ─────────────────────

    /// A Combat scene gated on "fight" is skipped when the chosen flag was
    /// something else; the alternate gated story scene plays instead.
    /// Simulate by manipulating the ScenarioDef + flags directly (no Bevy app).
    #[test]
    fn branch_choice_flag_gates_combat_and_story() {
        let scen = scenario_from_scenes(vec![
            // scene 0: choice (no flag gate itself)
            SceneDef::Choice {
                prompt: vec![],
                options: vec![
                    crate::content::scenarios::ChoiceOption {
                        label: "Fight".into(),
                        set_flag: "fight".into(),
                    },
                    crate::content::scenarios::ChoiceOption {
                        label: "Flee".into(),
                        set_flag: "flee".into(),
                    },
                ],
                requires_flag: None,
            },
            // scene 1: combat, only shown if "fight"
            gated_combat(Some("fight"), vec!["victory_marker"]),
            // scene 2: story, only shown if "flee"
            gated_story(Some("flee")),
        ]);

        // Player chose "fight": combat plays (scene 1), story skipped.
        let f_fight = flags(&["fight"]);
        assert_eq!(skip_skipped(&scen, 1, &f_fight), Some(1));
        assert_eq!(skip_skipped(&scen, 2, &f_fight), None); // gated-flee tail

        // Player chose "flee": combat skipped, story plays (scene 2).
        let f_flee = flags(&["flee"]);
        assert_eq!(skip_skipped(&scen, 1, &f_flee), Some(2));
    }

    // ── skipped Combat drops its on_victory_flags ─────────────────────────────

    /// Skipping a Combat scene means its on_victory_flags never enter
    /// CampaignState.flags (by design: the gate decides who gets the flags).
    #[test]
    fn skipped_combat_does_not_write_victory_flags() {
        let scen = scenario_from_scenes(vec![
            // scene 0: combat gated on "fight" — player chose "flee", so this is skipped
            gated_combat(Some("fight"), vec!["victory_marker"]),
            // scene 1: story with no gate, reachable
            gated_story(None),
        ]);
        let db = make_db(scen);

        let mut app = App::new();
        app.add_message::<AdvanceScenario>();
        app.insert_resource(db);
        // Start at index -1 conceptually; we place scene_index at 0 and use the
        // write_victory_flags system to verify it doesn't fire when gated.
        // Instead: advance from a pre-scene into scene 0 (gated-combat) and check
        // that after skip_skipped the system ends at scene 1 (story), not scene 0.
        // We verify by running advance from a notional scene "-1" via index wrapping:
        // easier approach — just assert skip_skipped logic directly.
        let f_flee = flags(&["flee"]); // "fight" flag NOT present
        let scen2 = app.world().resource::<GameDb>().scenarios.get("s1").unwrap();
        // From index 0 with flee flag: should land on scene 1 (story), skipping combat.
        assert_eq!(skip_skipped(scen2, 0, &f_flee), Some(1));

        // Also verify write_victory_flags won't run on a skipped scene by ensuring
        // the scenario state would be set to scene 1, not scene 0.
        // (write_victory_flags reads scen.scenes[scene_index].on_victory_flags; if
        // scene_index == 1 it finds no on_victory_flags, so flags stay empty.)
        app.insert_resource(ScenarioState { scenario_id: "s1".into(), scene_index: 1 });
        app.insert_resource(CampaignState {
            campaign_id: "c".into(),
            scenario_index: 0,
            flags: f_flee.clone(),
        });
        app.add_systems(Update, write_victory_flags);
        app.update();

        let camp_flags = &app.world().resource::<CampaignState>().flags;
        assert!(
            !camp_flags.contains("victory_marker"),
            "victory_marker must NOT appear: the combat scene was skipped"
        );
    }

    // ── all-gated tail: clean termination ────────────────────────────────────

    /// advance_scenario_system reaching an all-gated tail terminates cleanly
    /// (no panic, transitions to MainMenu).
    #[test]
    fn advance_reaches_all_gated_tail_terminates() {
        use crate::content::settings::GameSettings;

        // Two scenes: index 0 is visible (starting point), index 1 is gated-absent.
        let scen = scenario_from_scenes(vec![
            gated_story(None),         // index 0: visible
            gated_story(Some("gone")), // index 1: gated, flag will be absent
        ]);
        let db = make_db(scen);

        let mut app = App::new();
        app.add_message::<AdvanceScenario>();
        app.insert_resource(db);
        app.insert_resource(ScenarioState { scenario_id: "s1".into(), scene_index: 0 });
        app.insert_resource(GameSettings::default());
        app.insert_resource(NextState::<AppState>::default());
        // No CampaignState → non-campaign scenario, flags = empty.
        app.add_systems(Update, (
            |mut w: MessageWriter<AdvanceScenario>| { w.write(AdvanceScenario); },
            advance_scenario_system,
        ).chain());
        app.update();

        // scene_index was 0; after advance: 0+1=1, gated → skip → None → finish.
        // ScenarioState.scene_index is set to scenes.len() == 2 on finish.
        let idx = app.world().resource::<ScenarioState>().scene_index;
        assert_eq!(idx, 2, "scene_index should be past end (= scenes.len()) after finish");
    }

    // ── enter_scenario_at: gated tail is graceful (no panic) ─────────────────

    /// enter_scenario_at with a gated tail and no flags must NOT panic —
    /// it gracefully transitions to MainMenu.
    #[test]
    fn enter_scenario_at_gated_tail_no_panic() {
        let scen = scenario_from_scenes(vec![
            gated_story(Some("secret")), // index 0: gated, flag absent
        ]);
        let db = make_db(scen);
        // Also need at least one encounter-free scenario so enter_scenario_at doesn't
        // fail on missing content — our scenario has no encounters, that's fine.

        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let world = bevy::ecs::world::World::new();
        let mut commands = Commands::new(&mut commands_queue, &world);
        let mut next_state = NextState::<AppState>::default();

        // No flags → all-gated tail → graceful MainMenu transition, no panic.
        enter_scenario_at(
            &mut commands,
            &db,
            &mut next_state,
            "s1",
            0,
            None, // no flags
        );
        // If we reach here without panic, the test passes.
        // next_state should have been set to MainMenu.
        assert!(
            matches!(next_state, NextState::Pending(AppState::MainMenu)),
            "expected MainMenu transition"
        );
    }

    // ── save-load reentry with flags ──────────────────────────────────────────

    /// enter_scenario_at with populated flags resolves to the same scene advance would.
    /// With gate flag present: gated scene plays. Without: skipped.
    #[test]
    fn enter_scenario_at_flag_resolves_correctly() {
        let scen = scenario_from_scenes(vec![
            gated_story(Some("secret")), // index 0: gated
            gated_story(None),            // index 1: always visible
        ]);
        let db = make_db(scen);

        // Save-load lands at index 0 with "secret" flag → scene 0 plays.
        {
            let mut commands_queue = bevy::ecs::world::CommandQueue::default();
            let world = bevy::ecs::world::World::new();
            let mut commands = Commands::new(&mut commands_queue, &world);
            let mut next_state = NextState::<AppState>::default();
            enter_scenario_at(&mut commands, &db, &mut next_state, "s1", 0, Some(&flags(&["secret"])));
            // If ScenarioState would be inserted with scene_index=0 we're correct;
            // we can verify by applying the queue.
            let mut world2 = bevy::ecs::world::World::new();
            commands_queue.apply(&mut world2);
            let state = world2.resource::<ScenarioState>();
            assert_eq!(state.scene_index, 0, "with flag: gated scene 0 should play");
        }

        // Save-load lands at index 0 WITHOUT "secret" flag → skip to scene 1.
        {
            let mut commands_queue = bevy::ecs::world::CommandQueue::default();
            let world = bevy::ecs::world::World::new();
            let mut commands = Commands::new(&mut commands_queue, &world);
            let mut next_state = NextState::<AppState>::default();
            enter_scenario_at(&mut commands, &db, &mut next_state, "s1", 0, None);
            let mut world2 = bevy::ecs::world::World::new();
            commands_queue.apply(&mut world2);
            let state = world2.resource::<ScenarioState>();
            assert_eq!(state.scene_index, 1, "without flag: skip to scene 1");
        }
    }

    // ── None-campaign: no panic, flags treated as empty ──────────────────────

    /// Entering a scenario with no CampaignState treats flags as empty:
    /// gated scenes skip, no panic.
    #[test]
    fn none_campaign_gated_scenes_skip_no_panic() {
        use crate::content::settings::GameSettings;

        let scen = scenario_from_scenes(vec![
            gated_story(Some("x")), // index 0: gated, will be skipped
            gated_story(None),       // index 1: always visible
        ]);
        let db = make_db(scen);

        let mut app = App::new();
        app.add_message::<AdvanceScenario>();
        app.insert_resource(db);
        // Place at index 0; no CampaignState inserted.
        app.insert_resource(ScenarioState { scenario_id: "s1".into(), scene_index: 0 });
        app.insert_resource(GameSettings::default());
        app.insert_resource(NextState::<AppState>::default());

        // Manually test skip_skipped with empty flags (simulates None-campaign path).
        let world = app.world();
        let db2 = world.resource::<GameDb>();
        let scen2 = db2.scenarios.get("s1").unwrap();
        let empty = std::collections::BTreeSet::new();
        // From index 0 with empty flags: scene 0 (gated) skipped, scene 1 plays.
        assert_eq!(skip_skipped(scen2, 0, &empty), Some(1));
        // No panic reached → test passes.
    }
}
