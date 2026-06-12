//! E2E tests for Wave-2 §8: phase victory override + round-based turn_limit.
//!
//! Tests cover:
//! - `apply_phase_overrides_system`: override applied, VictoryTarget marker attached,
//!   deadline set, UI dirty flag raised.
//! - `check_phase_deadline_system`: deadline expired + target alive → Defeat;
//!   deadline expired + target dead → Victory; deadline not yet reached → no transition.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use storyforge::app_state::CombatPhase;
use storyforge::combat::advance_turn::check_phase_deadline_system;
use storyforge::combat::bridge::{apply_phase_overrides_system, BridgeQueues, PhaseOverrideIntent};
use storyforge::content::encounters::VictoryCondition;
use storyforge::game::components::{Combatant, Dead, Faction, Team, VictoryTarget, Vital};
use storyforge::game::resources::{
    CombatContext, CombatObjective, PhaseDeadline, PhaseDeadlineState, UiDirty, UiDirtyFlags,
};

#[path = "common/mod.rs"]
mod common;

use common::apps::engine::movement_app;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Extend `movement_app` with resources required by the new systems that are
/// not yet in the base harness (they are owned by `CombatPipelinePlugin` in prod).
fn deadline_app() -> App {
    let mut app = movement_app();
    app.init_resource::<PhaseDeadline>();
    app.init_resource::<UiDirty>();
    app
}

/// Spawn a minimal enemy entity with a `Name` component.
fn spawn_named_enemy(app: &mut App, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            Name::new(name.to_string()),
            Combatant,
            Faction(Team::Enemy),
            Vital {
                hp: 50,
                max_hp: 100,
                armor: 0,
                magic_resist: 0,
            },
        ))
        .id()
}

/// Spawn an enemy with `VictoryTarget` and optionally with `Dead` / hp=0.
fn spawn_boss(app: &mut App, name: &str, alive: bool) -> Entity {
    let mut ent = app.world_mut().spawn((
        Name::new(name.to_string()),
        Combatant,
        Faction(Team::Enemy),
        Vital {
            hp: if alive { 50 } else { 0 },
            max_hp: 100,
            armor: 0,
            magic_resist: 0,
        },
        VictoryTarget {
            marker_color: [1.0, 0.0, 0.0],
        },
    ));
    if !alive {
        ent.insert(Dead);
    }
    ent.id()
}

/// Spawn a living hero (player-team combatant) so `players_alive` is true.
fn spawn_hero(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Name::new("Hero"),
            Combatant,
            Faction(Team::Player),
            Vital {
                hp: 30,
                max_hp: 30,
                armor: 0,
                magic_resist: 0,
            },
        ))
        .id()
}

// ── apply_phase_overrides_system ──────────────────────────────────────────────

/// A `PhaseOverrideIntent` with both `victory_override` and `turn_limit`:
/// - `CombatObjective` changes to `KillTarget`,
/// - `PhaseDeadline` is set with `phase_started_round` == `ctx.round`,
/// - `VictoryTarget` marker is attached to the phasing entity,
/// - `UiDirty` has `PHASE_HINT` raised.
#[test]
fn apply_phase_overrides_sets_objective_deadline_and_marker() {
    let mut app = deadline_app();

    // ctx.round = 2 — the round at which the phase fired.
    app.world_mut().resource_mut::<CombatContext>().round = 2;

    // Spawn the phasing enemy.
    let boss = spawn_named_enemy(&mut app, "Boss Phase Two");

    // Push the intent.
    app.world_mut()
        .resource_mut::<BridgeQueues>()
        .phase_overrides
        .push(PhaseOverrideIntent {
            entity: boss,
            victory_override: Some(VictoryCondition::KillTarget {
                enemy_name: "Boss Phase Two".to_string(),
                marker_color: [1.0, 0.0, 0.0],
                description: None,
            }),
            turn_limit: Some(3),
        });

    app.world_mut()
        .run_system_once(apply_phase_overrides_system)
        .expect("apply_phase_overrides_system failed");

    // 1. CombatObjective must be KillTarget("Boss Phase Two").
    let objective = app.world().resource::<CombatObjective>();
    match &objective.0 {
        VictoryCondition::KillTarget { enemy_name, .. } => {
            assert_eq!(enemy_name, "Boss Phase Two");
        }
        other => panic!("expected KillTarget objective, got {other:?}"),
    }

    // 2. PhaseDeadline must be set with correct round / limit.
    let dl = app.world().resource::<PhaseDeadline>();
    let state =
        dl.0.as_ref()
            .expect("PhaseDeadline must be Some after override");
    assert_eq!(state.phase_started_round, 2);
    assert_eq!(state.limit, 3);

    // 3. VictoryTarget marker must be attached to the boss entity.
    // (Commands deferred — need one app.update() to flush.)
    app.update();
    assert!(
        app.world().entity(boss).get::<VictoryTarget>().is_some(),
        "VictoryTarget must be attached to phasing entity"
    );

    // 4. UiDirty must have PHASE_HINT set.
    let dirty = app.world().resource::<UiDirty>();
    assert!(
        dirty.0.contains(UiDirtyFlags::PHASE_HINT),
        "UiDirty must have PHASE_HINT set after victory override"
    );
}

/// `PhaseOverrideIntent` with `turn_limit` only (no `victory_override`):
/// deadline is set, objective stays default, no VictoryTarget attached.
#[test]
fn apply_phase_overrides_turn_limit_only() {
    let mut app = deadline_app();
    app.world_mut().resource_mut::<CombatContext>().round = 5;

    let enemy = spawn_named_enemy(&mut app, "Runner");

    app.world_mut()
        .resource_mut::<BridgeQueues>()
        .phase_overrides
        .push(PhaseOverrideIntent {
            entity: enemy,
            victory_override: None,
            turn_limit: Some(4),
        });

    app.world_mut()
        .run_system_once(apply_phase_overrides_system)
        .expect("apply_phase_overrides_system failed");

    let dl = app.world().resource::<PhaseDeadline>();
    let state = dl.0.as_ref().expect("PhaseDeadline must be Some");
    assert_eq!(state.phase_started_round, 5);
    assert_eq!(state.limit, 4);

    // Objective unchanged (still AllEnemiesDead default).
    let objective = app.world().resource::<CombatObjective>();
    assert!(matches!(objective.0, VictoryCondition::AllEnemiesDead));
}

// ── check_phase_deadline_system ───────────────────────────────────────────────

/// Deadline reached (round - started >= limit) + target still alive → Defeat.
#[test]
fn deadline_reached_target_alive_is_defeat() {
    let mut app = deadline_app();

    spawn_hero(&mut app);
    spawn_boss(&mut app, "Boss", /* alive */ true);

    app.world_mut().resource_mut::<CombatObjective>().0 = VictoryCondition::KillTarget {
        enemy_name: "Boss".to_string(),
        marker_color: [1.0, 0.0, 0.0],
        description: None,
    };
    *app.world_mut().resource_mut::<PhaseDeadline>() = PhaseDeadline(Some(PhaseDeadlineState {
        phase_started_round: 1,
        limit: 2,
    }));
    app.world_mut().resource_mut::<CombatContext>().round = 3; // 3 - 1 = 2 >= 2 → expired

    app.world_mut()
        .run_system_once(check_phase_deadline_system)
        .expect("check_phase_deadline_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::Defeat,
        "expired deadline + boss alive must be Defeat"
    );
}

/// Deadline reached + target dead (player killed in time) → Victory.
#[test]
fn deadline_reached_target_dead_is_victory() {
    let mut app = deadline_app();

    spawn_hero(&mut app);
    spawn_boss(&mut app, "Boss", /* alive */ false);

    app.world_mut().resource_mut::<CombatObjective>().0 = VictoryCondition::KillTarget {
        enemy_name: "Boss".to_string(),
        marker_color: [1.0, 0.0, 0.0],
        description: None,
    };
    *app.world_mut().resource_mut::<PhaseDeadline>() = PhaseDeadline(Some(PhaseDeadlineState {
        phase_started_round: 1,
        limit: 2,
    }));
    app.world_mut().resource_mut::<CombatContext>().round = 3;

    app.world_mut()
        .run_system_once(check_phase_deadline_system)
        .expect("check_phase_deadline_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::Victory,
        "expired deadline + boss dead must be Victory"
    );
}

/// Deadline NOT yet reached (round - started < limit) → no state transition.
#[test]
fn deadline_not_reached_no_transition() {
    let mut app = deadline_app();

    spawn_hero(&mut app);
    spawn_boss(&mut app, "Boss", /* alive */ true);

    app.world_mut().resource_mut::<CombatObjective>().0 = VictoryCondition::KillTarget {
        enemy_name: "Boss".to_string(),
        marker_color: [1.0, 0.0, 0.0],
        description: None,
    };
    *app.world_mut().resource_mut::<PhaseDeadline>() = PhaseDeadline(Some(PhaseDeadlineState {
        phase_started_round: 1,
        limit: 2,
    }));
    // round=2 → 2 - 1 = 1 < 2 → not expired yet
    app.world_mut().resource_mut::<CombatContext>().round = 2;

    app.world_mut()
        .run_system_once(check_phase_deadline_system)
        .expect("check_phase_deadline_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::AwaitCommand,
        "deadline not reached must leave phase unchanged"
    );
}

/// No active deadline (PhaseDeadline is None) → no transition.
#[test]
fn no_deadline_no_transition() {
    let mut app = deadline_app();
    spawn_hero(&mut app);
    // PhaseDeadline is None by default.
    app.world_mut().resource_mut::<CombatContext>().round = 999;

    app.world_mut()
        .run_system_once(check_phase_deadline_system)
        .expect("check_phase_deadline_system failed");
    app.update();

    let phase = app.world().resource::<State<CombatPhase>>().get().clone();
    assert_eq!(
        phase,
        CombatPhase::AwaitCommand,
        "no deadline must leave phase unchanged"
    );
}
