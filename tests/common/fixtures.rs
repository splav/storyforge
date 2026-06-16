//! Cross-layer fixtures: stats, equipment, hero/enemy bundle helpers, and a
//! few Bevy message conveniences. Used by both engine-layer and bridge-layer
//! integration tests.

#![allow(dead_code)]

use bevy::ecs::message::Messages;
use bevy::prelude::*;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::components::{CombatStats, Equipment};

pub const MELEE_ATTACK: &str = "melee_attack";

pub fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 10,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

pub fn test_equipment() -> Equipment {
    Equipment {
        main_hand: Some("short_sword".into()),
        off_hand: None,
        chest: "mage_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

pub fn test_hero(stats: CombatStats) -> impl Bundle {
    hero_bundle(stats, 0, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
}

pub fn test_enemy(stats: CombatStats) -> impl Bundle {
    enemy_bundle(stats, 0, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
}

pub fn enter_await_command(app: &mut App) {
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Combat);
    app.update();
    app.world_mut()
        .resource_mut::<NextState<CombatPhase>>()
        .set(CombatPhase::AwaitCommand);
    app.update();
}

pub fn write_message<M: Message>(app: &mut App, msg: M) {
    app.world_mut().resource_mut::<Messages<M>>().write(msg);
}

pub fn message_count<M: Message>(app: &App) -> usize {
    app.world()
        .resource::<Messages<M>>()
        .iter_current_update_messages()
        .count()
}
