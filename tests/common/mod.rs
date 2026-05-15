#![allow(dead_code)]

use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::{
    ai::world::reservations::Reservations,
    engine_bridge::{init_state_from_ecs, process_action_system, project_state_to_ecs,
                    CombatStateRes, UnitIdMap},
};
use storyforge::content::content_view::ActiveContent;
use storyforge::content::settings::GameSettings;
use storyforge::content::statuses::StatusDef;
use storyforge::combat::DiceRngRes;
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{CombatStats, Equipment};
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::game::messages::{ActionInput, EndTurn};
use storyforge::game::resources::{
    CombatContext, CombatObjective, GameDb, HexPositions, SelectionState, TurnQueue,
};
use storyforge::ui::hex_grid::{HexMaterials, TokenMesh};

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
    hero_bundle(stats, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
}

pub fn test_enemy(stats: CombatStats) -> impl Bundle {
    enemy_bundle(stats, 0, 3, vec![MELEE_ATTACK.into()], test_equipment())
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

pub fn movement_app() -> App {
    use bevy::math::Vec2;
    use storyforge::combat::turn_order::build_turn_order;
    use storyforge::game::resources::PresetInitiative;
    use storyforge::ui::animation::AnimationQueue;
    use storyforge::ui::hex_grid::HexGridOffset;

    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatObjective>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .insert_resource(ActiveContent(storyforge::content::content_view::ContentView::load_global_for_tests()))
        .init_resource::<GameSettings>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<DiceRngRes>()
        .init_resource::<AnimationQueue>()
        .init_resource::<Reservations>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<PresetInitiative>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials {
            empty: Handle::default(),
            player: Handle::default(),
            enemy: Handle::default(),
            dead: Handle::default(),
            in_range: Handle::default(),
            in_range_dim: Handle::default(),
            move_range: Handle::default(),
            border_active: Handle::default(),
            border_target: Handle::default(),
            border_in_range: Handle::default(),
            border_in_range_dim: Handle::default(),
            border_move: Handle::default(),
            aoe_preview: Handle::default(),
            border_aoe: Handle::default(),
            token_player: Handle::default(),
            token_enemy: Handle::default(),
            token_dead: Handle::default(),
        })
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .add_message::<ActionInput>()
        .add_message::<EndTurn>()
        .add_systems(
            OnEnter(CombatPhase::AwaitCommand),
            init_state_from_ecs,
        )
        .add_systems(
            Update,
            (process_action_system, project_state_to_ecs)
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        )
        .add_systems(Update, build_turn_order.run_if(in_state(CombatPhase::StartRound)));
    enter_await_command(&mut app);
    app
}

/// Re-run the engine init system manually after spawning combatants.
///
/// `movement_app()` transitions to `AwaitCommand` at builder time (before any
/// units are spawned), so `OnEnter` fires with an empty world.  Call this
/// after your spawn block and any direct ECS mutations, but before the first
/// `write_message`.
pub fn init_engine_state(app: &mut App) {
    use bevy::ecs::system::RunSystemOnce;
    app.world_mut()
        .run_system_once(init_state_from_ecs)
        .expect("init_state_from_ecs failed");
}

pub fn insert_stun_status(app: &mut App) {
    app.world_mut().resource_mut::<ActiveContent>().0.statuses.insert(
        "stun".into(),
        StatusDef {
            id: "stun".into(),
            name: "Stun".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
            buff_class: None,
        },
    );
}
