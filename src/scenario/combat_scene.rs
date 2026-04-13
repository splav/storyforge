use crate::content::scenarios::SceneDef;
use crate::game::bundles::{enemy_bundle, hero_bundle};
use crate::game::components::{Combatant, Mana, Rage, StartingHexPos, UnitToken};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{
    CombatContext, GameDb, HexPositions, ScenarioState, SelectionState, TurnQueue,
};
use crate::combat::enemy_popup::PopupCursor;
use crate::ui::animation::AnimationQueue;
use crate::ui::console_log::ConsoleCursor;
use bevy::prelude::*;

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
        let mut ec = commands.spawn((
            Name::new(member.name.clone()),
            hero_bundle(
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

    ctx.round = 0;
    ctx.active = None;
    ctx.last_active = None;
    ctx.encounter = None;
    ctx.turn_ending = false;
    log.0.clear();
    log.push(CombatEvent::CombatStarted);
    cursor.0 = 0;
    popup_cursor.0 = 0;
    anim_queue.0.clear();
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
    for entity in &combatants {
        commands.entity(entity).despawn();
    }
    for entity in &tokens {
        commands.entity(entity).despawn();
    }
    positions.clear();
    queue.order.clear();
    queue.index = 0;
    ctx.active = None;
    ctx.last_active = None;
    ctx.encounter = None;
    sel.clear();
    anim_queue.0.clear();
    for entity in &popups {
        commands.entity(entity).despawn();
    }
}
