#![allow(dead_code)]

use bevy::ecs::message::Messages;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::{
    advance_turn::{advance_turn_system, check_victory_system}, ai::log::debug::AiDebugState,
    ai::config::difficulty::DifficultyProfile, ai::world::influence::InfluenceConfig,
    ai::world::reservations::Reservations,
    apply_effects::apply_effects_system, ai::system::enemy_ai_system,
    engine_bridge::{mirror_state_from_ecs, process_action_system, project_state_to_ecs,
                    CombatStateRes, UnitIdMap},
    phases::phase_transition_system,
    resolution::resolve_action_system,
    skip_dead::skip_stunned_turn_system,
    status_tick::tick_status_effects_system,
    validation::validate_action_system,
};
use storyforge::combat::ai::world::tags::cache::build_caches;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::settings::GameSettings;
use storyforge::content::statuses::StatusDef;
use storyforge::core::{DiceExpr, DiceRng};
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{CombatStats, Equipment};
use storyforge::game::messages::{
    ActionInput, ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, SpawnUnit, UseAbility, ValidatedAction,
};
use storyforge::game::resources::{
    CombatContext, CombatObjective, GameDb, HexPositions, SelectionState, TurnQueue,
};

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

pub fn validation_app() -> App {
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
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<DiceRng>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<EndTurn>()
        .add_systems(
            Update,
            validate_action_system.run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

pub fn effects_app() -> App {
    let mut app = App::new();
    let content = storyforge::content::content_view::ContentView::load_global_for_tests();
    let (status_tags, ability_tags) = build_caches(&content);
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatObjective>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .insert_resource(ActiveContent(content))
        .insert_resource(status_tags)
        .insert_resource(ability_tags)
        .init_resource::<SelectionState>()
        .init_resource::<DiceRng>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_message::<SpawnUnit>()
        .add_systems(
            Update,
            (
                tick_status_effects_system,
                apply_effects_system,
                phase_transition_system,
                advance_turn_system,
                check_victory_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

pub fn resolve_app() -> App {
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
        .init_resource::<DiceRng>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_message::<SpawnUnit>()
        .add_systems(
            Update,
            (resolve_action_system, apply_effects_system, advance_turn_system, check_victory_system)
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

pub fn stun_app() -> App {
    let mut app = App::new();
    let content = storyforge::content::content_view::ContentView::load_global_for_tests();
    let (status_tags, ability_tags) = build_caches(&content);
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatObjective>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .insert_resource(ActiveContent(content))
        .insert_resource(status_tags)
        .insert_resource(ability_tags)
        .init_resource::<GameSettings>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<DiceRng>()
        .init_resource::<DifficultyProfile>()
        .init_resource::<AiDebugState>()
        .init_resource::<Reservations>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<InfluenceConfig>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_message::<SpawnUnit>()
        .add_message::<UseAbility>()
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (
                tick_status_effects_system,
                skip_stunned_turn_system,
                enemy_ai_system,
                apply_effects_system,
                advance_turn_system,
                check_victory_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
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
        .init_resource::<DiceRng>()
        .init_resource::<AnimationQueue>()
        .init_resource::<Reservations>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<PresetInitiative>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .add_message::<ActionInput>()
        .add_systems(
            PreUpdate,
            mirror_state_from_ecs.run_if(in_state(CombatPhase::AwaitCommand)),
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

pub fn pipeline_app() -> App {
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
        .init_resource::<DiceRng>()
        .add_message::<UseAbility>()
        .add_message::<ValidatedAction>()
        .add_message::<ApplyDamage>()
        .add_message::<ApplyHeal>()
        .add_message::<ApplyStatus>()
        .add_message::<EndTurn>()
        .add_message::<SpawnUnit>()
        .add_systems(
            Update,
            (
                validate_action_system,
                resolve_action_system,
                apply_effects_system,
                advance_turn_system,
                check_victory_system,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        );
    enter_await_command(&mut app);
    app
}

pub fn insert_taunt_status(app: &mut App) {
    app.world_mut().resource_mut::<ActiveContent>().0.statuses.insert(
        "taunt".into(),
        StatusDef {
            id: "taunt".into(),
            name: "Taunt".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: false,
            forces_targeting: true,
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

pub fn insert_burning_status(app: &mut App) {
    app.world_mut().resource_mut::<ActiveContent>().0.statuses.insert(
        "burning".into(),
        StatusDef {
            id: "burning".into(),
            name: "Burning".into(),
            armor_bonus: 0,
            damage_taken_bonus: 1,
            skips_turn: false,
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

pub fn insert_poison_status(app: &mut App) {
    app.world_mut().resource_mut::<ActiveContent>().0.statuses.insert(
        "poisoned".into(),
        StatusDef {
            id: "poisoned".into(),
            name: "Poisoned".into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: false,
            forces_targeting: false,
            dot_dice: Some(DiceExpr::new(1, 4, 0)),
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
            buff_class: None,
        },
    );
}
