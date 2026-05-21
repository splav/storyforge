#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::content::scenarios::{active_party, SceneDef};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::config::role::infer_profile;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::game::bundles::{enemy_bundle, hero_bundle};
use crate::game::components::{AuraSource, CombatPath, Combatant, Energy, EnemyPhases, Equipment, Initiative, Mana, Rage, StartingHexPos, UnitToken, VictoryTarget};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::messages::RestartCombat;
use crate::game::resources::{
    CombatContext, CombatObjective, GameDb, HexPositions, PresetInitiative, ScenarioState, SelectionState, TurnQueue,
};
use crate::combat::engine_bridge::{CombatStateRes, PendingPhaseTransitions, UnitIdMap};
use crate::combat::enemy_popup::PopupCursor;
use crate::ui::animation::AnimationQueue;
use crate::ui::console_log::ConsoleCursor;
use bevy::prelude::*;

#[derive(Component)]
pub struct BattleBackground;

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Спаунит героев и врагов по текущему сценарию/энкаунтеру. Только Commands.
fn spawn_combatants(
    commands: &mut Commands,
    db: &GameDb,
    scenario: &ScenarioState,
    objective: &mut CombatObjective,
    tag_cache: &AbilityTagCache,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let encounter_id = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { encounter_id, .. } => encounter_id,
        _ => return,
    };
    let enc = scen.encounters.get(encounter_id.as_str()).unwrap_or_else(|| {
        panic!(
            "Encounter '{encounter_id}' not found in scenario '{}'",
            scen.id
        )
    });

    objective.0 = enc.victory.clone();
    let content = &scen.content;

    let party = active_party(scen, scenario.scene_index);
    for member in &party {
        let cls = content.classes.get(&member.class_id).unwrap_or_else(|| {
            panic!("Class '{}' not found in classes.toml", member.class_id)
        });
        let equipment = Equipment {
            main_hand: Some(cls.main_hand.clone()),
            off_hand: cls.off_hand.clone(),
            chest: cls.chest.clone(),
            legs: cls.legs.clone(),
            feet: cls.feet.clone(),
        };
        let effective = content.effective_stats(&cls.stats, &equipment);
        let armor = content.equipment_armor(&equipment);
        let role = infer_profile(&cls.abilities, effective.max_hp, armor, content, tag_cache);
        let mut ec = commands.spawn((
            Name::new(member.name.clone()),
            hero_bundle(effective, armor, cls.speed, cls.abilities.clone(), equipment),
            StartingHexPos(member.hex_pos),
            role,
            AiMemory::default(),
        ));
        if cls.rage_max > 0 { ec.insert(Rage::new(cls.rage_max)); }
        if cls.mana_max > 0 { ec.insert(Mana::new(cls.mana_max)); }
        if cls.energy_max > 0 { ec.insert(Energy::new(cls.energy_max)); }
        if let Some(ref p) = member.path { ec.insert(CombatPath(p.clone())); }
    }

    for enemy in &enc.enemies {
        let equipment = Equipment {
            main_hand: Some(enemy.main_hand.clone()),
            off_hand: enemy.off_hand.clone(),
            chest: enemy.chest.clone(),
            legs: enemy.legs.clone(),
            feet: enemy.feet.clone(),
        };
        let effective = content.effective_stats(&enemy.stats, &equipment);
        let armor = content.equipment_armor(&equipment);
        let race_name = content.races.get(&enemy.race).map_or("", |r| r.name.as_str());
        let display_name = format!("{} {}", race_name, &enemy.name);
        let role = infer_profile(&enemy.ability_ids, effective.max_hp, armor, content, tag_cache);
        let mut ec = commands.spawn((
            Name::new(display_name),
            enemy_bundle(effective, armor, enemy.speed, enemy.ability_ids.clone(), equipment),
            StartingHexPos(enemy.hex_pos),
            role,
            AiMemory::default(),
        ));
        if enemy.rage_max > 0 { ec.insert(Rage::new(enemy.rage_max)); }
        if enemy.mana_max > 0 { ec.insert(Mana::new(enemy.mana_max)); }
        if enemy.energy_max > 0 { ec.insert(Energy::new(enemy.energy_max)); }
        if let Some(ref p) = enemy.path { ec.insert(CombatPath(p.clone())); }
        if let VictoryCondition::KillTarget { enemy_name, marker_color, .. } = &enc.victory {
            if &enemy.name == enemy_name {
                ec.insert(VictoryTarget { marker_color: *marker_color });
            }
        }
        if !enemy.phases.is_empty() {
            ec.insert(EnemyPhases { pending: enemy.phases.clone() });
        }
        if let Some(ref aura) = enemy.aura {
            ec.insert(AuraSource {
                status: aura.status.clone(),
                radius: aura.radius,
                affects: aura.affects,
            });
        }
    }
}

/// Сбрасывает все ресурсы боя в начальное состояние.
fn reset_combat_state(
    ctx: &mut CombatContext,
    log: &mut CombatLog,
    cursor: &mut ConsoleCursor,
    popup_cursor: &mut PopupCursor,
    anim_queue: &mut AnimationQueue,
) {
    ctx.round = 0;
    ctx.encounter = None;
    log.0.clear();
    log.push(CombatEvent::CombatStarted);
    cursor.0 = 0;
    popup_cursor.0 = 0;
    anim_queue.0.clear();
}

// ── Systems ─────────────────────────────────────────────────────────────────

pub fn spawn_combat_scene(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    asset_server: Res<AssetServer>,
    windows: Query<&Window>,
    mut ctx: ResMut<CombatContext>,
    mut objective: ResMut<CombatObjective>,
    mut log: ResMut<CombatLog>,
    mut cursor: ResMut<ConsoleCursor>,
    mut popup_cursor: ResMut<PopupCursor>,
    mut anim_queue: ResMut<AnimationQueue>,
    tag_cache: Res<AbilityTagCache>,
) {
    spawn_combatants(&mut commands, &db, &scenario, &mut objective, &tag_cache);
    spawn_background(&mut commands, &db, &scenario, &asset_server, &windows);
    reset_combat_state(&mut ctx, &mut log, &mut cursor, &mut popup_cursor, &mut anim_queue);
}

fn spawn_background(
    commands: &mut Commands,
    db: &GameDb,
    scenario: &ScenarioState,
    asset_server: &AssetServer,
    windows: &Query<&Window>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let location = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { location: Some(loc), .. } => loc,
        _ => return,
    };
    let rel_path = format!("images/battle_backgrounds/{location}.png");
    if !std::path::Path::new("assets").join(&rel_path).exists() {
        warn!("battle background not found: {rel_path}");
        return;
    }
    let handle: Handle<Image> = asset_server.load(&rel_path);
    let size = windows.single().ok().map(|w| Vec2::new(w.width(), w.height()));
    commands.spawn((
        BattleBackground,
        Sprite {
            image: handle,
            custom_size: size,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));
}

pub fn despawn_combatants(
    mut commands: Commands,
    combatants: Query<Entity, With<Combatant>>,
    tokens: Query<Entity, With<UnitToken>>,
    backgrounds: Query<Entity, With<BattleBackground>>,
    mut positions: ResMut<HexPositions>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut sel: ResMut<SelectionState>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut combat_state: ResMut<CombatStateRes>,
    mut id_map: ResMut<UnitIdMap>,
    mut pending_phases: ResMut<PendingPhaseTransitions>,
    popups: Query<Entity, With<crate::ui::animation::EnemyActionPopup>>,
) {
    for entity in combatants.iter().chain(tokens.iter()).chain(popups.iter()).chain(backgrounds.iter()) {
        commands.entity(entity).despawn();
    }
    positions.clear();
    queue.order.clear();
    queue.index = 0;
    ctx.encounter = None;
    sel.clear();
    anim_queue.0.clear();
    // Engine state mirrors must be cleared too — otherwise the next combat's
    // StartRound system `project_state_to_ecs` writes stale positions from the
    // previous combat into HexPositions before init_state_from_ecs has a chance
    // to rebuild engine state from the new ECS combatants, causing position
    // collisions with the freshly spawned units.
    *combat_state = CombatStateRes::default();
    id_map.clear();
    pending_phases.0.clear();
}

// ── Restart combat ──────────────────────────────────────────────────────────

/// Сохраняет инициативу в `PresetInitiative`, полностью пересоздаёт сцену.
/// `build_turn_order` подхватит сохранённые значения вместо бросков кубика.
pub fn restart_combat_system(
    mut reader: MessageReader<RestartCombat>,
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    tag_cache: Res<AbilityTagCache>,
    combatants: Query<(Entity, &Name, &Initiative), With<Combatant>>,
    cleanup: Query<Entity, Or<(With<UnitToken>, With<crate::ui::animation::EnemyActionPopup>)>>,
    mut preset: ResMut<PresetInitiative>,
    mut positions: ResMut<HexPositions>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut objective: ResMut<CombatObjective>,
    mut reset_bundle: (
        ResMut<CombatLog>,
        ResMut<ConsoleCursor>,
        ResMut<PopupCursor>,
        ResMut<AnimationQueue>,
    ),
    mut sel: ResMut<SelectionState>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
    // Engine state mirrors bundled into one tuple param to stay under Bevy's
    // 16-system-param limit.
    mut engine_mirrors: (
        ResMut<CombatStateRes>,
        ResMut<UnitIdMap>,
        ResMut<PendingPhaseTransitions>,
    ),
) {
    let (combat_state, id_map, pending_phases) = &mut engine_mirrors;
    if reader.read().next().is_none() {
        return;
    }

    let (log, cursor, popup_cursor, anim_queue) = &mut reset_bundle;

    // 1. Save initiative by name.
    preset.0.clear();
    for (_, name, init) in &combatants {
        preset.0.insert(name.as_str().to_string(), init.0);
    }

    // 2. Despawn combatants, tokens, popups.
    for (entity, _, _) in &combatants {
        commands.entity(entity).despawn();
    }
    for entity in &cleanup {
        commands.entity(entity).despawn();
    }
    positions.clear();
    queue.order.clear();
    queue.index = 0;
    sel.clear();

    // Engine state mirrors — same rationale as in despawn_combatants: without
    // clearing here, the upcoming StartRound's project_state_to_ecs would
    // project stale combat-1 unit positions into the freshly cleared HexPositions
    // before init_state_from_ecs rebuilds engine state, colliding with the
    // newly-spawned units.
    **combat_state = CombatStateRes::default();
    id_map.clear();
    pending_phases.0.clear();

    // 3. Spawn fresh combatants + reset state.
    spawn_combatants(&mut commands, &db, &scenario, &mut objective, &tag_cache);
    reset_combat_state(&mut ctx, log, cursor, popup_cursor, anim_queue);

    // 4. → StartRound, где assign_hex_positions создаст токены,
    //    а build_turn_order возьмёт инициативу из PresetInitiative.
    next_phase.set(CombatPhase::StartRound);
}
