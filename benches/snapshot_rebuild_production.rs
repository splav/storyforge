//! Bench: production `build_snapshot()` (AI snapshot rebuild from ECS).
//!
//! ## Purpose
//!
//! The original `snapshot_rebuild.rs` bench measured `snapshot_from()` — a
//! **test helper** that takes a `Vec<UnitSnapshot>` and reconstructs a
//! `BattleSnapshot`. The production call site is different: `build_snapshot()`
//! in `src/combat/ai/world/snapshot.rs` reads live ECS components
//! (`AiCombatantQ`, `HexMap`, `StatusEffects`, etc.) to produce the snapshot.
//! This bench measures THAT function.
//!
//! ## Call count per AI turn
//!
//! `build_snapshot` is called **exactly once per AI actor turn** at the top of
//! `run_ai_turn()` in `src/combat/ai/system.rs:140`. There is no beam-search
//! or inner-loop re-invocation — the snapshot is built once and then passed
//! down to `pick_action`, `goal_lifecycle`, and logging helpers. Consequently,
//! the per-turn cost is:
//!
//!   per_turn_cost = median_wall_time × 1
//!
//! See `docs/combat/perf-baseline.md` §"Production target re-measurement" for
//! the corrected verdict (HOT / COLD / negligible).
//!
//! ## Bench design
//!
//! - Bevy `World` + resources are built **once**, outside `b.iter()`.
//! - `SystemState` is built **once** — amortises Bevy's query-validation
//!   overhead; only the actual ECS fetch + `build_snapshot` logic runs inside
//!   the hot loop.
//! - Scenario: identical 6-unit mid-encounter as `snapshot_rebuild.rs`
//!   (2 player + 4 enemy, one corpse, no statuses) so the two numbers are
//!   directly comparable.
//! - Units are spawned with a real `melee_attack` ability loaded from
//!   `assets/data/abilities.toml` so ability-iteration paths inside
//!   `build_snapshot` exercise realistic content.
//!
//! ## Running
//!
//! ```bash
//! cargo bench --bench snapshot_rebuild_production
//! ```
//!
//! Build WITHOUT `--features dev` (release profile, static link) — see CLAUDE.md.

use bevy::ecs::system::{RunSystemOnce, SystemState};
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

use storyforge::app_state::{AppState, CombatPhase};
use storyforge::combat::ai::config::difficulty::DifficultyProfile;
use storyforge::combat::ai::config::role::AxisProfile;
use storyforge::combat::ai::world::snapshot::build_snapshot;
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::engine_bridge::{
    apply_bridge_queues_post_projection, apply_bridge_queues_pre_projection,
    bootstrap_combat_state, process_action_system, project_state_to_ecs, BridgeQueues,
    CombatStateRes, UnitIdMap,
};
use storyforge::combat::turn_order::build_turn_order;
use storyforge::combat::DiceRngRes;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::settings::GameSettings;
use storyforge::game::bundles::{enemy_bundle, hero_bundle};
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{
    AiCombatantQ, CombatStats, Combatant, Equipment, StatusEffects,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::hex_map::HexMap;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatObjective, GameDb, HexCorpses, HexPositions,
    PresetInitiative, SelectionState, TurnQueue,
};
use storyforge::ui::animation::AnimationQueue;
use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

// ── Scenario constants ────────────────────────────────────────────────────────

const MELEE_ATTACK: &str = "melee_attack";

fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 25,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

fn test_equipment() -> Equipment {
    Equipment {
        main_hand: Some("short_sword".into()),
        off_hand: None,
        chest: "mage_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

// ── World construction ────────────────────────────────────────────────────────

/// Build a minimal Bevy `App` with the resources `build_snapshot` needs.
/// Mirrors `tests/common/apps/engine.rs::movement_app()`.
fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin))
        .init_state::<AppState>()
        .add_sub_state::<CombatPhase>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatObjective>()
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatLog>()
        .init_resource::<GameDb>()
        .insert_resource(ActiveContent({
            let global = std::path::Path::new("assets/data");
            storyforge::content::content_view::ContentView::load_layered(global, global)
        }))
        .init_resource::<GameSettings>()
        .init_resource::<SelectionState>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<DiceRngRes>()
        .init_resource::<AnimationQueue>()
        .init_resource::<storyforge::combat::ai::world::reservations::Reservations>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<storyforge::combat::ai::log::engine_trace::EngineTraceWriter>()
        .init_resource::<PresetInitiative>()
        .insert_resource(HexGridOffset(bevy::math::Vec2::ZERO))
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<BridgeQueues>()
        .insert_resource(AbilityTagCache::default())
        .insert_resource(DifficultyProfile::default())
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
            build_turn_order.run_if(in_state(CombatPhase::StartRound)),
        );

    // Transition to AwaitCommand (mirrors enter_await_command).
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Combat);
    app.update();
    app.world_mut()
        .resource_mut::<NextState<CombatPhase>>()
        .set(CombatPhase::AwaitCommand);
    app.update();

    app
}

/// Spawn the 6-unit mid-encounter scenario into `app`.
///
/// Same layout as `snapshot_rebuild.rs` / `perf-baseline.md`:
///   P1 (2,3) 35HP, P2 (3,3) 18/30HP wounded,
///   E1 (7,3) 25HP, E2 (8,4) 25HP, E3 (5,2) corpse 0/20HP, E4 (9,5) 30HP.
fn spawn_scenario(app: &mut App) {
    let hero_stats = CombatStats {
        max_hp: 35,
        ..base_stats()
    };
    let hero2_stats = CombatStats {
        max_hp: 30,
        ..base_stats()
    };
    let enemy_stats = CombatStats {
        max_hp: 25,
        ..base_stats()
    };
    let enemy_stats2 = CombatStats {
        max_hp: 25,
        ..base_stats()
    };
    let enemy4_stats = CombatStats {
        max_hp: 30,
        ..base_stats()
    };

    // Hero P1 — full HP melee bruiser.
    let p1 = app
        .world_mut()
        .spawn(hero_bundle(
            hero_stats,
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();

    // Hero P2 — wounded ranged.
    let p2 = app
        .world_mut()
        .spawn(hero_bundle(
            hero2_stats,
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();
    // Reduce HP to simulate wound.
    app.world_mut()
        .get_mut::<storyforge::game::components::Vital>(p2)
        .unwrap()
        .hp = 18;

    // Enemy E1 — melee AoO-capable.
    let e1 = app
        .world_mut()
        .spawn(enemy_bundle(
            enemy_stats,
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();

    // Enemy E2 — melee AoO-capable.
    let e2 = app
        .world_mut()
        .spawn(enemy_bundle(
            enemy_stats2,
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();

    // Enemy E3 — corpse (hp=0); stays in HexCorpses so build_snapshot includes it.
    let e3 = app
        .world_mut()
        .spawn(enemy_bundle(
            CombatStats {
                max_hp: 20,
                ..base_stats()
            },
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();
    app.world_mut()
        .get_mut::<storyforge::game::components::Vital>(e3)
        .unwrap()
        .hp = 0;

    // Enemy E4 — ranged.
    let e4 = app
        .world_mut()
        .spawn(enemy_bundle(
            enemy4_stats,
            0,
            3,
            vec![MELEE_ATTACK.into()],
            test_equipment(),
        ))
        .id();

    // Insert positions.
    {
        let mut positions = app.world_mut().resource_mut::<HexPositions>();
        positions.insert(p1, hex_from_offset(2, 3));
        positions.insert(p2, hex_from_offset(3, 3));
        positions.insert(e1, hex_from_offset(7, 3));
        positions.insert(e2, hex_from_offset(8, 4));
        positions.insert(e4, hex_from_offset(9, 5));
    }
    // E3 is a corpse: lives in HexCorpses, not HexPositions.
    {
        app.world_mut()
            .entity_mut(e3)
            .insert(storyforge::game::components::Dead);
        let pos = hex_from_offset(5, 2);
        app.world_mut().resource_mut::<HexCorpses>().insert(e3, pos);
    }

    // Bootstrap engine state from the spawned ECS.
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
}

// ── Benchmark ────────────────────────────────────────────────────────────────

fn bench_snapshot_rebuild_production(c: &mut Criterion) {
    // Build the Bevy World ONCE — hoisted out of b.iter().
    let mut app = build_app();
    spawn_scenario(&mut app);

    // Build SystemState ONCE — amortises query-validation and param setup.
    // Only the ECS fetch + build_snapshot logic runs per iteration.
    type SnapSystemState<'w> = SystemState<(
        Query<'w, 'static, AiCombatantQ, With<Combatant>>,
        Query<'w, 'static, &'static StatusEffects>,
        HexMap<'w>,
        Query<'w, 'static, &'static AxisProfile>,
        Res<'w, ActiveContent>,
        Res<'w, DifficultyProfile>,
        Res<'w, CombatStateRes>,
        Res<'w, UnitIdMap>,
        Query<'w, 'static, Entity, With<storyforge::game::components::KeepAliveTarget>>,
    )>;

    let mut state: SnapSystemState = SystemState::new(app.world_mut());

    c.bench_function("snapshot_rebuild_production", |b| {
        b.iter(|| {
            let (
                combatants,
                statuses,
                hex_map,
                roles,
                content,
                difficulty,
                state_res,
                id_map,
                keep_alive_q,
            ) = state.get(app.world());

            let keep_alive_entities: std::collections::HashSet<bevy::prelude::Entity> =
                keep_alive_q.iter().collect();

            let snap = build_snapshot(
                black_box(1),
                &combatants,
                &statuses,
                black_box(&hex_map),
                &roles,
                black_box(&content),
                black_box(&difficulty),
                black_box(state_res.0.clone()),
                black_box(&id_map),
                black_box(&keep_alive_entities),
                combat_engine::state::Team::Enemy,
            );
            black_box(snap);
        });
    });
}

criterion_group!(benches, bench_snapshot_rebuild_production);
criterion_main!(benches);
