#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::content::scenarios::SceneDef;
use crate::game::bundles::{enemy_bundle, hero_bundle};
use crate::game::components::{CombatPath, Combatant, Energy, Equipment, Initiative, Mana, Rage, StartingHexPos, UnitToken};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::messages::RestartCombat;
use crate::game::resources::{
    CombatContext, GameDb, HexPositions, PresetInitiative, ScenarioState, SelectionState, TurnQueue,
};
use crate::combat::enemy_popup::PopupCursor;
use crate::ui::animation::AnimationQueue;
use crate::ui::console_log::ConsoleCursor;
use bevy::prelude::*;

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Спаунит героев и врагов по текущему сценарию/энкаунтеру. Только Commands.
fn spawn_combatants(commands: &mut Commands, db: &GameDb, scenario: &ScenarioState) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let encounter_id = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { encounter_id } => encounter_id,
        _ => return,
    };
    let enc = db.encounters.get(encounter_id.as_str()).unwrap_or_else(|| {
        panic!("Encounter '{encounter_id}' not found in encounters.toml")
    });

    for member in &scen.party {
        let cls = db.classes.get(&member.class_id).unwrap_or_else(|| {
            panic!("Class '{}' not found in classes.toml", member.class_id)
        });
        let equipment = Equipment {
            main_hand: Some(cls.main_hand.clone()),
            off_hand: cls.off_hand.clone(),
            chest: cls.chest.clone(),
            legs: cls.legs.clone(),
            feet: cls.feet.clone(),
        };
        let effective = db.effective_stats(&cls.stats, &equipment);
        let armor = db.equipment_armor(&equipment);
        let mut ec = commands.spawn((
            Name::new(member.name.clone()),
            hero_bundle(effective, armor, cls.speed, cls.abilities.clone(), equipment),
            StartingHexPos(member.hex_pos.0, member.hex_pos.1),
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
        let effective = db.effective_stats(&enemy.stats, &equipment);
        let armor = db.equipment_armor(&equipment);
        let race_name = db.races.get(&enemy.race).map_or("", |r| r.name.as_str());
        let display_name = format!("{} {}", race_name, &enemy.name);
        let mut ec = commands.spawn((
            Name::new(display_name),
            enemy_bundle(effective, armor, enemy.speed, enemy.ability_ids.clone(), equipment),
            StartingHexPos(enemy.hex_pos.0, enemy.hex_pos.1),
        ));
        if enemy.rage_max > 0 { ec.insert(Rage::new(enemy.rage_max)); }
        if enemy.mana_max > 0 { ec.insert(Mana::new(enemy.mana_max)); }
        if enemy.energy_max > 0 { ec.insert(Energy::new(enemy.energy_max)); }
        if let Some(ref p) = enemy.path { ec.insert(CombatPath(p.clone())); }
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
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut cursor: ResMut<ConsoleCursor>,
    mut popup_cursor: ResMut<PopupCursor>,
    mut anim_queue: ResMut<AnimationQueue>,
) {
    spawn_combatants(&mut commands, &db, &scenario);
    reset_combat_state(&mut ctx, &mut log, &mut cursor, &mut popup_cursor, &mut anim_queue);
}

pub fn despawn_combatants(
    mut commands: Commands,
    combatants: Query<Entity, With<Combatant>>,
    tokens: Query<Entity, With<UnitToken>>,
    mut positions: ResMut<HexPositions>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut sel: ResMut<SelectionState>,
    mut anim_queue: ResMut<AnimationQueue>,
    popups: Query<Entity, With<crate::ui::animation::EnemyActionPopup>>,
) {
    for entity in combatants.iter().chain(tokens.iter()).chain(popups.iter()) {
        commands.entity(entity).despawn();
    }
    positions.clear();
    queue.order.clear();
    queue.index = 0;
    ctx.encounter = None;
    sel.clear();
    anim_queue.0.clear();
}

// ── Restart combat ──────────────────────────────────────────────────────────

/// Сохраняет инициативу в `PresetInitiative`, полностью пересоздаёт сцену.
/// `build_turn_order` подхватит сохранённые значения вместо бросков кубика.
pub fn restart_combat_system(
    mut reader: MessageReader<RestartCombat>,
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    combatants: Query<(Entity, &Name, &Initiative), With<Combatant>>,
    cleanup: Query<Entity, Or<(With<UnitToken>, With<crate::ui::animation::EnemyActionPopup>)>>,
    mut preset: ResMut<PresetInitiative>,
    mut positions: ResMut<HexPositions>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut log: ResMut<CombatLog>,
    mut cursor: ResMut<ConsoleCursor>,
    mut popup_cursor: ResMut<PopupCursor>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut sel: ResMut<SelectionState>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    if reader.read().next().is_none() {
        return;
    }

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

    // 3. Spawn fresh combatants + reset state.
    spawn_combatants(&mut commands, &db, &scenario);
    reset_combat_state(&mut ctx, &mut log, &mut cursor, &mut popup_cursor, &mut anim_queue);

    // 4. → StartRound, где assign_hex_positions создаст токены,
    //    а build_turn_order возьмёт инициативу из PresetInitiative.
    next_phase.set(CombatPhase::StartRound);
}
