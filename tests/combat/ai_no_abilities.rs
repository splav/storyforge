//! Regression: an alive enemy with no abilities must end its turn, not hang.
//!
//! The bug: `enemy_ai_system` returned early without writing
//! `ActionInput::EndTurn` when the active `Team::Enemy` combatant had an
//! absent/empty `Abilities` component, hanging the turn.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use crate::common::{apps::engine::*, fixtures::*};
use storyforge::combat::ai::system::enemy_ai_system;
use storyforge::game::components::ActiveCombatant;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{HexCorpses, HexPositions};

/// Build a `movement_app` extended with the resources that `enemy_ai_system`
/// requires but that `movement_app` does not initialise.  All added resources
/// implement `Default` and are only touched by the normal (non-early-exit)
/// code path, so empty defaults are fine for this test.
fn ai_app() -> App {
    use storyforge::combat::ai::config::difficulty::DifficultyProfile;
    use storyforge::combat::ai::log::debug::AiDebugState;
    use storyforge::combat::ai::log::PendingAiLogEntries;
    use storyforge::combat::ai::world::influence::InfluenceConfig;
    use storyforge::combat::ai::world::tags::cache::StatusTagCache;

    let mut app = movement_app();
    app.init_resource::<DifficultyProfile>()
        .init_resource::<InfluenceConfig>()
        .init_resource::<HexCorpses>()
        .init_resource::<StatusTagCache>()
        .init_resource::<AiDebugState>()
        .init_resource::<PendingAiLogEntries>();
    app
}

fn spawn_at(app: &mut App, bundle: impl Bundle, pos: hexx::Hex) -> Entity {
    let e = app.world_mut().spawn(bundle).id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos);
    e
}

/// Alive `Team::Enemy` with empty `Abilities` → `enemy_ai_system` must emit
/// exactly one `ActionInput::EndTurn` for that actor.
#[test]
fn alive_enemy_no_abilities_emits_end_turn() {
    use storyforge::game::bundles::enemy_bundle;
    use storyforge::game::components::Equipment;

    let mut app = ai_app();

    let equipment = Equipment {
        main_hand: None,
        off_hand: None,
        chest: "".into(),
        legs: "".into(),
        feet: "".into(),
    };
    // Spawn an enemy with a deliberately empty ability list — this models
    // non-acting NPC roster units (e.g. Тэо, Хорст, accumulator in ch3).
    let actor = spawn_at(
        &mut app,
        enemy_bundle(base_stats(), 0, 0, 3, vec![], equipment),
        hex_from_offset(3, 3),
    );

    // Need a second combatant so the engine can build a valid turn order
    // (bootstrap_combat_state requires at least one unit per side, or at least
    // a non-empty roster).  A minimal hero satisfies this.
    spawn_at(&mut app, test_hero(base_stats()), hex_from_offset(5, 3));

    init_engine_state(&mut app);

    // Verify the engine settled `actor` as the active combatant.  If it settled
    // the hero instead (e.g. initiative ordering changed), force our enemy.
    if app.world().get::<ActiveCombatant>(actor).is_none() {
        // Remove from whoever has it and give it to actor.
        let holders: Vec<Entity> = app
            .world_mut()
            .query::<(Entity, &ActiveCombatant)>()
            .iter(app.world())
            .map(|(e, _)| e)
            .collect();
        for h in holders {
            app.world_mut().entity_mut(h).remove::<ActiveCombatant>();
        }
        app.world_mut().entity_mut(actor).insert(ActiveCombatant);
    }

    // Run enemy_ai_system once; it should write EndTurn without touching the
    // normal AI decision loop (the actor has no abilities).
    app.world_mut()
        .run_system_once(enemy_ai_system)
        .expect("enemy_ai_system failed");

    // Inspect the ActionInput messages written this frame (no update() has
    // run, so iter_current_update_messages covers everything enemy_ai_system
    // wrote via MessageWriter).
    let msgs_res = app.world().resource::<Messages<ActionInput>>();
    let msg_count = msgs_res.iter_current_update_messages().count();
    assert_eq!(
        msg_count, 1,
        "expected exactly one ActionInput message, got {msg_count}"
    );

    let is_end_turn_for_actor = msgs_res
        .iter_current_update_messages()
        .any(|m| matches!(m, ActionInput::EndTurn { actor: a } if *a == actor));
    assert!(
        is_end_turn_for_actor,
        "expected ActionInput::EndTurn {{ actor: {actor:?} }}"
    );
}
