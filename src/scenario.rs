use crate::app_state::AppState;
use crate::content::scenarios::SceneDef;
use crate::game::bundles::{enemy_bundle, warrior_bundle};
use crate::game::components::{Combatant, Mana, Rage, StartingHexPos};
use crate::game::resources::{
    CombatContext, CombatEvent, CombatLog, GameDb, HexPositions, ScenarioState, SelectionState,
    TurnQueue,
};
use crate::ui::console_log::ConsoleCursor;
use bevy::prelude::*;

#[derive(Message)]
pub struct AdvanceScenario;

// ── Startup ─────────────────────────────────────────────────────────────────

pub fn start_scenario(
    mut commands: Commands,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    commands.spawn(Camera2d);

    let scenario_id = "demo";
    let scen = db
        .scenarios
        .get(scenario_id)
        .unwrap_or_else(|| panic!("Scenario '{scenario_id}' not found in scenarios.toml"));

    commands.insert_resource(ScenarioState {
        scenario_id: scenario_id.into(),
        scene_index: 0,
    });

    match &scen.scenes[0] {
        SceneDef::Story { .. } => next_state.set(AppState::Story),
        SceneDef::Combat { .. } => next_state.set(AppState::Combat),
    }
}

// ── Advance scenario ────────────────────────────────────────────────────────

pub fn advance_scenario_system(
    mut events: MessageReader<AdvanceScenario>,
    scenario: Option<ResMut<ScenarioState>>,
    db: Res<GameDb>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    let Some(mut scenario) = scenario else { return };

    for _ in events.read() {
        scenario.scene_index += 1;

        let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
        if scenario.scene_index >= scen.scenes.len() {
            next_state.set(AppState::MainMenu);
            return;
        }

        match &scen.scenes[scenario.scene_index] {
            SceneDef::Story { .. } => next_state.set(AppState::Story),
            SceneDef::Combat { .. } => next_state.set(AppState::Combat),
        }
    }
}

// ── Combat scene spawn / despawn ────────────────────────────────────────────

pub fn spawn_combat_scene(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut cursor: ResMut<ConsoleCursor>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let encounter_id = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { encounter_id } => encounter_id,
        _ => return,
    };
    let enc = db.encounters.get(encounter_id.as_str()).unwrap_or_else(|| {
        panic!("Encounter '{encounter_id}' not found in encounters.toml")
    });

    // Spawn party.
    for member in &scen.party {
        let cls = db.classes.get(&member.class_id).unwrap_or_else(|| {
            panic!("Class '{}' not found in classes.toml", member.class_id)
        });
        let mut ec = commands.spawn((
            Name::new(member.name.clone()),
            warrior_bundle(
                cls.stats.clone(),
                cls.speed,
                cls.abilities.clone(),
                cls.weapon.clone(),
            ),
            StartingHexPos(member.hex_pos.0, member.hex_pos.1),
        ));
        if cls.rage_max > 0 {
            ec.insert(Rage::new(cls.rage_max));
        }
        if cls.mana_max > 0 {
            ec.insert(Mana::new(cls.mana_max));
        }
    }

    // Spawn enemies.
    for enemy in &enc.enemies {
        let mut ec = commands.spawn((
            Name::new(enemy.name.clone()),
            enemy_bundle(
                enemy.stats.clone(),
                enemy.speed,
                enemy.ability_ids.clone(),
                enemy.weapon_id.clone(),
            ),
            StartingHexPos(enemy.hex_pos.0, enemy.hex_pos.1),
        ));
        if enemy.rage_max > 0 {
            ec.insert(Rage::new(enemy.rage_max));
        }
        if enemy.mana_max > 0 {
            ec.insert(Mana::new(enemy.mana_max));
        }
    }

    // Reset combat state.
    ctx.round = 0;
    ctx.active = None;
    ctx.last_active = None;
    ctx.encounter = None;
    log.0.clear();
    log.push(CombatEvent::CombatStarted);
    cursor.0 = 0;
}

pub fn despawn_combatants(
    mut commands: Commands,
    combatants: Query<Entity, With<Combatant>>,
    mut positions: ResMut<HexPositions>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut sel: ResMut<SelectionState>,
) {
    for entity in &combatants {
        commands.entity(entity).despawn();
    }
    positions.0.clear();
    queue.order.clear();
    queue.index = 0;
    ctx.active = None;
    ctx.last_active = None;
    ctx.encounter = None;
    sel.clear();
}

// ── Victory / Defeat input ──────────────────────────────────────────────────

pub fn victory_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut writer: MessageWriter<AdvanceScenario>,
) {
    if keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) {
        writer.write(AdvanceScenario);
    }
}

pub fn defeat_input_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) {
        next_state.set(AppState::MainMenu);
    }
}
