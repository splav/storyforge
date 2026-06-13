//! Engine-layer App builder: `movement_app` + `init_engine_state`.
//!
//! `movement_app` constructs a Bevy `App` with state machine (`AppState`,
//! `CombatPhase`), bridge schedule, and content view loaded — sufficient for
//! engine-bridge integration tests that need the full per-turn pipeline.
//!
//! Split from `common/mod.rs` in Phase H3 of `docs/refactor/helpers-normalization-plan.md`.

#![allow(dead_code)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::DiceRngRes;
use storyforge::combat::{
    ai::world::reservations::Reservations,
    bridge::{
        apply_bridge_queues_post_projection, apply_bridge_queues_pre_projection,
        bootstrap_combat_state, process_action_system, project_state_to_ecs, BridgeQueues,
        CombatStateRes, UnitIdMap,
    },
};
use storyforge::content::content_view::ActiveContent;
use storyforge::content::settings::GameSettings;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective, GameDb, HexCorpses,
    HexPositions, SelectionState, TurnQueue, UiDirty,
};
use storyforge::ui::hex_grid::{HexMaterials, TokenMesh};

use super::super::fixtures::enter_await_command;

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
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<CombatEnvironment>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .insert_resource(ActiveContent(
            storyforge::content::content_view::ActiveContentData::load_global_for_tests(),
        ))
        .init_resource::<GameSettings>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<DiceRngRes>()
        .init_resource::<AnimationQueue>()
        .init_resource::<Reservations>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<storyforge::combat::ai::log::engine_trace::EngineTraceWriter>()
        .init_resource::<PresetInitiative>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<BridgeQueues>()
        .init_resource::<UiDirty>()
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials::default())
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (
                process_action_system,
                apply_bridge_queues_pre_projection,
                project_state_to_ecs,
                apply_bridge_queues_post_projection,
            )
                .chain()
                .run_if(in_state(CombatPhase::AwaitCommand)),
        )
        .add_systems(
            Update,
            (build_turn_order, bootstrap_combat_state)
                .chain()
                .run_if(in_state(CombatPhase::StartRound)),
        );
    enter_await_command(&mut app);
    app
}

/// Re-run the engine bootstrap system manually after spawning combatants.
///
/// `movement_app()` transitions to `AwaitCommand` at builder time (before any
/// units are spawned), so bootstrap does not fire on entry.  Call this
/// after your spawn block and any direct ECS mutations, but before the first
/// `write_message`.
///
/// After bootstrap, `insert_active` holds the engine-settled actor.  We drain
/// it immediately via `apply_bridge_queues_pre_projection` so that
/// `ActiveCombatant` is set before any action is processed in the test.
pub fn init_engine_state(app: &mut App) {
    use bevy::ecs::system::RunSystemOnce;
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues_pre_projection failed");
}
