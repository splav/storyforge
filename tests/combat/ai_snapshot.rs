/// Regression test: `build_snapshot` must include dead units (hp=0 markers).
///
/// Before the `HexPositions` → `HexCorpses` split, dead entities were removed
/// from `HexPositions` by the projector, causing `build_snapshot`'s
/// `positions.get(&c.entity)?` guard to silently drop them. AI accessors like
/// `dead_units()` and `all_enemies_of()` rely on the dead-unit rows being present.
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use crate::common::{apps::engine::*, fixtures::*};
use storyforge::combat::ai::config::difficulty::DifficultyProfile;
use storyforge::combat::ai::config::role::AxisProfile;
use storyforge::combat::ai::world::snapshot::build_snapshot;
use storyforge::combat::bridge::{CombatStateRes, UnitIdMap};
use storyforge::game::components::{AiCombatantQ, Combatant, Dead, StatusEffects};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::hex_map::HexMap;
use storyforge::game::resources::{HexCorpses, HexPositions};

fn spawn_at(app: &mut App, pos: hexx::Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, pos);
    e
}

/// `build_snapshot` must include dead combatants so that death-aware AI
/// accessors (`dead_units`, `all_enemies_of`) can find them.
#[test]
fn build_snapshot_includes_dead_combatant() {
    let mut app = movement_app();

    // Spawn a living and a dead enemy.
    let living = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "Living",
    );
    let dead = spawn_at(
        &mut app,
        hex_from_offset(4, 3),
        test_enemy(base_stats()),
        "Dead",
    );

    // Mark `dead` as dead: insert Dead component, move to HexCorpses, clear from HexPositions.
    app.world_mut().entity_mut(dead).insert(Dead);
    app.world_mut()
        .get_mut::<storyforge::game::components::Vital>(dead)
        .unwrap()
        .hp = 0;
    {
        let pos = app.world().resource::<HexPositions>().get(&dead).unwrap();
        app.world_mut()
            .resource_mut::<HexCorpses>()
            .insert(dead, pos);
        app.world_mut().resource_mut::<HexPositions>().remove(&dead);
    }

    init_engine_state(&mut app);
    // DifficultyProfile is not in movement_app's default resources (it's injected
    // by the game plugin at startup). Insert a default for this test.
    app.insert_resource(DifficultyProfile::default());

    // Run build_snapshot as a one-shot system; return the entity set it produced.
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn snapshot_system(
        combatants: Query<AiCombatantQ, With<Combatant>>,
        statuses: Query<&StatusEffects>,
        hex_map: HexMap,
        roles: Query<&AxisProfile>,
        content: Res<storyforge::content::content_view::ActiveContent>,
        difficulty: Res<DifficultyProfile>,
        state_res: Res<CombatStateRes>,
        id_map: Res<UnitIdMap>,
    ) -> Vec<Entity> {
        let keep_alive_entities = std::collections::HashSet::new();
        let snap = build_snapshot(
            1,
            &combatants,
            &statuses,
            &hex_map,
            &roles,
            &content,
            &difficulty,
            state_res.0.clone(),
            &id_map,
            &keep_alive_entities,
            combat_engine::state::Team::Enemy,
        );
        snap.cache.units.iter().map(|u| u.entity).collect()
    }

    let entities_in_cache: Vec<Entity> = app
        .world_mut()
        .run_system_once(snapshot_system)
        .expect("snapshot_system failed");

    assert!(
        entities_in_cache.contains(&living),
        "living entity must be in AiCache",
    );
    assert!(
        entities_in_cache.contains(&dead),
        "dead entity must be in AiCache (hp=0 marker for death-aware accessors)",
    );
}

/// Regression: an `AiBehaviorOverride { Flee }` ECS component must surface as
/// `UnitAiCache.forced_mode == Some(EvaluationMode::Flee)` through the real
/// `build_snapshot` query path (the unit-test parity check uses `snapshot_from`,
/// which bypasses the ECS component → forced_mode mapping).
#[test]
fn build_snapshot_maps_ai_behavior_override_to_forced_mode() {
    use storyforge::combat::ai::adapt::EvaluationMode;
    use storyforge::content::encounters::AiBehaviorKind;
    use storyforge::game::components::AiBehaviorOverride;

    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());

    let plain = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "Plain",
    );
    let fleeing = spawn_at(
        &mut app,
        hex_from_offset(4, 3),
        test_enemy(base_stats()),
        "Fleeing",
    );
    app.world_mut()
        .entity_mut(fleeing)
        .insert(AiBehaviorOverride {
            kind: AiBehaviorKind::Flee,
        });

    init_engine_state(&mut app);

    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn snapshot_system(
        combatants: Query<AiCombatantQ, With<Combatant>>,
        statuses: Query<&StatusEffects>,
        hex_map: HexMap,
        roles: Query<&AxisProfile>,
        content: Res<storyforge::content::content_view::ActiveContent>,
        difficulty: Res<DifficultyProfile>,
        state_res: Res<CombatStateRes>,
        id_map: Res<UnitIdMap>,
    ) -> Vec<(Entity, Option<EvaluationMode>)> {
        let keep_alive_entities = std::collections::HashSet::new();
        let snap = build_snapshot(
            1,
            &combatants,
            &statuses,
            &hex_map,
            &roles,
            &content,
            &difficulty,
            state_res.0.clone(),
            &id_map,
            &keep_alive_entities,
            combat_engine::state::Team::Enemy,
        );
        snap.cache
            .units
            .iter()
            .map(|u| (u.entity, u.forced_mode))
            .collect()
    }

    let modes: Vec<(Entity, Option<EvaluationMode>)> = app
        .world_mut()
        .run_system_once(snapshot_system)
        .expect("snapshot_system failed");

    let plain_mode = modes
        .iter()
        .find(|(e, _)| *e == plain)
        .expect("plain in cache")
        .1;
    let fleeing_mode = modes
        .iter()
        .find(|(e, _)| *e == fleeing)
        .expect("fleeing in cache")
        .1;

    assert_eq!(
        plain_mode, None,
        "unit without AiBehaviorOverride must have forced_mode == None"
    );
    assert_eq!(
        fleeing_mode,
        Some(EvaluationMode::Flee),
        "unit with AiBehaviorOverride{{Flee}} must map to forced_mode == Some(Flee)",
    );
}

/// Hidden traps must be absent from the AI snapshot; revealed traps must appear.
///
/// This enforces the commit-C invariant: `build_snapshot` strips `!revealed`
/// env objects so the planner cannot "cheat" by simulating outcomes on hexes
/// it has no knowledge of.
#[test]
fn ai_snapshot_excludes_hidden_traps_includes_revealed() {
    use combat_engine::state::{EnvId, EnvKind, EnvObject};
    use storyforge::game::hex::hex_from_offset;

    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());

    spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "A",
    );
    spawn_at(
        &mut app,
        hex_from_offset(4, 3),
        test_enemy(base_stats()),
        "B",
    );
    init_engine_state(&mut app);

    // Seed engine state with one hidden (enemy-owned, not revealed to enemy) and
    // one visible (revealed_to contains Enemy) trap.
    {
        use combat_engine::state::{Team, TeamSet};
        let mut cs = app.world_mut().resource_mut::<CombatStateRes>();
        cs.0.environment = vec![
            EnvObject {
                id: EnvId(0),
                hex: hex_from_offset(2, 3),
                kind: EnvKind::Hazard,
                ability: combat_engine::AbilityId::from("spike_trap"),
                owner: None,
                revealed_to: TeamSet::EMPTY, // hidden — must NOT appear in snapshot
            },
            EnvObject {
                id: EnvId(1),
                hex: hex_from_offset(5, 3),
                kind: EnvKind::Hazard,
                ability: combat_engine::AbilityId::from("spike_trap"),
                owner: None,
                revealed_to: {
                    let mut ts = TeamSet::EMPTY;
                    ts.insert(Team::Enemy); // visible to enemy — must appear in snapshot
                    ts
                },
            },
        ];
    }

    #[allow(clippy::too_many_arguments)]
    fn snapshot_system(
        combatants: Query<AiCombatantQ, With<Combatant>>,
        statuses: Query<&StatusEffects>,
        hex_map: HexMap,
        roles: Query<&AxisProfile>,
        content: Res<storyforge::content::content_view::ActiveContent>,
        difficulty: Res<DifficultyProfile>,
        state_res: Res<CombatStateRes>,
        id_map: Res<UnitIdMap>,
    ) -> usize {
        use combat_engine::state::Team;
        let keep_alive_entities = std::collections::HashSet::new();
        let snap = build_snapshot(
            1,
            &combatants,
            &statuses,
            &hex_map,
            &roles,
            &content,
            &difficulty,
            state_res.0.clone(),
            &id_map,
            &keep_alive_entities,
            Team::Enemy,
        );
        snap.state.environment.len()
    }

    let env_count: usize = app
        .world_mut()
        .run_system_once(snapshot_system)
        .expect("snapshot_system failed");

    assert_eq!(
        env_count, 1,
        "snapshot must contain exactly the visible trap (hidden one must be stripped)",
    );
}

/// Regression: a minimal combatant spawned via `npc_bundle` (no Abilities /
/// CombatStats / Equipment) must appear in `build_snapshot` — it was silently
/// dropped before AiCombatantQ fields were made Optional.
///
/// The NPC entry is expected to have threat ≈ 0 and an empty abilities list,
/// matching the default-fallback semantics introduced by this change.
#[test]
fn build_snapshot_includes_minimal_npc() {
    use storyforge::game::bundles::npc_bundle;
    use storyforge::game::components::{Team, Vital};

    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());

    // A normal enemy so the scenario is non-trivial.
    let normal = spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "Normal",
    );

    // A minimal NPC: Faction + Vital only (no Abilities / CombatStats / Equipment).
    let vital = Vital::new(
        &storyforge::game::components::CombatStats {
            max_hp: 5,
            strength: 5,
            dexterity: 5,
            constitution: 5,
            intelligence: 0,
            wisdom: 5,
            charisma: 5,
        },
        0,
        0,
    );
    let npc = spawn_at(
        &mut app,
        hex_from_offset(4, 3),
        npc_bundle(Team::Player, vital),
        "MinimalNpc",
    );

    init_engine_state(&mut app);

    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn snapshot_system(
        combatants: Query<AiCombatantQ, With<Combatant>>,
        statuses: Query<&StatusEffects>,
        hex_map: HexMap,
        roles: Query<&AxisProfile>,
        content: Res<storyforge::content::content_view::ActiveContent>,
        difficulty: Res<DifficultyProfile>,
        state_res: Res<CombatStateRes>,
        id_map: Res<UnitIdMap>,
    ) -> Vec<(Entity, f32, Vec<combat_engine::AbilityId>)> {
        let keep_alive_entities = std::collections::HashSet::new();
        let snap = build_snapshot(
            1,
            &combatants,
            &statuses,
            &hex_map,
            &roles,
            &content,
            &difficulty,
            state_res.0.clone(),
            &id_map,
            &keep_alive_entities,
            combat_engine::state::Team::Enemy,
        );
        snap.cache
            .units
            .iter()
            .map(|u| (u.entity, u.threat, u.abilities.clone()))
            .collect()
    }

    let entries: Vec<(Entity, f32, Vec<combat_engine::AbilityId>)> = app
        .world_mut()
        .run_system_once(snapshot_system)
        .expect("snapshot_system failed");

    let entities: Vec<Entity> = entries.iter().map(|(e, _, _)| *e).collect();

    assert!(
        entities.contains(&normal),
        "normal enemy must be in snapshot"
    );
    assert!(
        entities.contains(&npc),
        "minimal NPC must be in snapshot (was silently dropped before this fix)"
    );

    // NPC has no abilities — threat must be 0 and abilities list empty.
    let npc_entry = entries.iter().find(|(e, _, _)| *e == npc).unwrap();
    assert_eq!(npc_entry.1, 0.0, "minimal NPC threat must be 0");
    assert!(
        npc_entry.2.is_empty(),
        "minimal NPC abilities must be empty"
    );
}

// ── T3: build_snapshot ai_team visibility filter ─────────────────────────────

fn seed_env(app: &mut App, envs: Vec<combat_engine::state::EnvObject>) {
    app.world_mut()
        .resource_mut::<CombatStateRes>()
        .0
        .environment = envs;
}

fn env_count_for_team(app: &mut App, team: combat_engine::state::Team) -> usize {
    use combat_engine::state::Team;
    // Run a one-shot system capturing env count for the given team.
    // We use a Local to smuggle the team in (Bevy one-shot systems can't take
    // extra parameters), so we parameterize via two separate concrete functions.
    match team {
        Team::Player => {
            #[allow(clippy::too_many_arguments)]
            fn sys(
                combatants: Query<AiCombatantQ, With<Combatant>>,
                statuses: Query<&StatusEffects>,
                hex_map: HexMap,
                roles: Query<&AxisProfile>,
                content: Res<storyforge::content::content_view::ActiveContent>,
                difficulty: Res<DifficultyProfile>,
                state_res: Res<CombatStateRes>,
                id_map: Res<UnitIdMap>,
            ) -> usize {
                let snap = build_snapshot(
                    1,
                    &combatants,
                    &statuses,
                    &hex_map,
                    &roles,
                    &content,
                    &difficulty,
                    state_res.0.clone(),
                    &id_map,
                    &std::collections::HashSet::new(),
                    combat_engine::state::Team::Player,
                );
                snap.state.environment.len()
            }
            app.world_mut().run_system_once(sys).unwrap()
        }
        Team::Enemy => {
            #[allow(clippy::too_many_arguments)]
            fn sys(
                combatants: Query<AiCombatantQ, With<Combatant>>,
                statuses: Query<&StatusEffects>,
                hex_map: HexMap,
                roles: Query<&AxisProfile>,
                content: Res<storyforge::content::content_view::ActiveContent>,
                difficulty: Res<DifficultyProfile>,
                state_res: Res<CombatStateRes>,
                id_map: Res<UnitIdMap>,
            ) -> usize {
                let snap = build_snapshot(
                    1,
                    &combatants,
                    &statuses,
                    &hex_map,
                    &roles,
                    &content,
                    &difficulty,
                    state_res.0.clone(),
                    &id_map,
                    &std::collections::HashSet::new(),
                    combat_engine::state::Team::Enemy,
                );
                snap.state.environment.len()
            }
            app.world_mut().run_system_once(sys).unwrap()
        }
    }
}

/// Own-team-owned traps are always visible to their owner's AI snapshot.
#[test]
fn snapshot_includes_own_team_owned_traps() {
    use combat_engine::state::{EnvId, EnvKind, EnvObject, Team, TeamSet};
    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());
    spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "A",
    );
    init_engine_state(&mut app);

    seed_env(
        &mut app,
        vec![EnvObject {
            id: EnvId(0),
            hex: hex_from_offset(1, 1),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from("t"),
            owner: Some(Team::Enemy),
            revealed_to: TeamSet::EMPTY,
        }],
    );

    assert_eq!(
        env_count_for_team(&mut app, Team::Enemy),
        1,
        "enemy-owned trap must appear in enemy snapshot"
    );
}

/// Enemy-owned traps that the player hasn't discovered are absent from the
/// player AI's snapshot.
#[test]
fn snapshot_excludes_enemy_owned_unrevealed_traps() {
    use combat_engine::state::{EnvId, EnvKind, EnvObject, Team, TeamSet};
    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());
    spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "A",
    );
    init_engine_state(&mut app);

    seed_env(
        &mut app,
        vec![EnvObject {
            id: EnvId(0),
            hex: hex_from_offset(1, 1),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from("t"),
            owner: Some(Team::Enemy),
            revealed_to: TeamSet::EMPTY,
        }],
    );

    assert_eq!(
        env_count_for_team(&mut app, Team::Player),
        0,
        "enemy-owned unrevealed trap must be absent from player snapshot"
    );
}

/// A trap revealed to the AI's team appears in the snapshot.
#[test]
fn snapshot_includes_trap_revealed_to_ai_team() {
    use combat_engine::state::{EnvId, EnvKind, EnvObject, Team, TeamSet};
    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());
    spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "A",
    );
    init_engine_state(&mut app);

    let mut revealed_to = TeamSet::EMPTY;
    revealed_to.insert(Team::Enemy);
    seed_env(
        &mut app,
        vec![EnvObject {
            id: EnvId(0),
            hex: hex_from_offset(1, 1),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from("t"),
            owner: None,
            revealed_to,
        }],
    );

    assert_eq!(
        env_count_for_team(&mut app, Team::Enemy),
        1,
        "trap revealed to enemy must appear in enemy snapshot"
    );
}

/// A neutral unrevealed trap is absent from both teams' snapshots.
#[test]
fn snapshot_neutral_unrevealed_absent_for_both_teams() {
    use combat_engine::state::{EnvId, EnvKind, EnvObject, Team, TeamSet};
    let mut app = movement_app();
    app.insert_resource(DifficultyProfile::default());
    spawn_at(
        &mut app,
        hex_from_offset(3, 3),
        test_enemy(base_stats()),
        "A",
    );
    init_engine_state(&mut app);

    seed_env(
        &mut app,
        vec![EnvObject {
            id: EnvId(0),
            hex: hex_from_offset(1, 1),
            kind: EnvKind::Hazard,
            ability: combat_engine::AbilityId::from("t"),
            owner: None,
            revealed_to: TeamSet::EMPTY,
        }],
    );

    assert_eq!(
        env_count_for_team(&mut app, Team::Player),
        0,
        "neutral unrevealed trap absent from player snapshot"
    );
    assert_eq!(
        env_count_for_team(&mut app, Team::Enemy),
        0,
        "neutral unrevealed trap absent from enemy snapshot"
    );
}
