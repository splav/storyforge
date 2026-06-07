//! Bridge smoke tests: projector isolation.
//!
//! Verifies `project_state_to_ecs` in isolation — without going through
//! `process_action_system`. Each test uses a `projector_only_app` (no mirror
//! system) so a direct mutation of `CombatStateRes` is not overwritten before
//! PostUpdate. Covers position, HP/MP, mana, status-effects, and aura-applied
//! status preservation.

use bevy::prelude::*;

use storyforge::combat::engine_bridge::{entity_to_uid, CombatStateRes, UnitIdMap};
use combat_engine::StatusId;
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::components::{
    ActionPoints, ActiveStatus, Reactions, StatusEffects,
    Team, Vital,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::HexPositions;

use super::common;


/// Projector-isolation test: direct engine mutation flows to ECS without
/// going through `process_action_system`.
///
/// Strategy: use a `projector_only_app` (no mirror) so a manual write to
/// `CombatStateRes` is not wiped before PostUpdate.
/// We seed the resource and `UnitIdMap` via `init_bridge_engine_state`,
/// then transplant those resources into the projector-only app for the assertion.
#[test]
fn projector_writes_engine_mutation_to_ecs() {
    // --- Phase A: seed engine state via the full bridge_app ---
    let start = hex_from_offset(0, 0);

    let mut seed_app = common::apps::bridge::bridge_app();

    common::apps::bridge::spawn_caster(&mut seed_app, start, vec![]);

    // Seed engine state from ECS (no mirror system in bridge_app).
    common::apps::bridge::bootstrap(&mut seed_app);

    // --- Phase B: set up projector-only app with the same entity / resources ---
    let mut app = common::apps::bridge::projector_only_app();

    // Spawn the same actor entity in the new world (entity id is stable across
    // App instances; we need the same Entity bits so UnitIdMap lookups work).
    let new_actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            common::apps::bridge::bridge_stats(),
            0,
            6,
            vec![],
            common::apps::bridge::default_equipment(),
        ))
        .id();

    // The entity id may differ between worlds; use a fresh uid derived from
    // new_actor's entity bits for the projector-only app's UnitIdMap.
    let new_actor_uid = entity_to_uid(new_actor);

    // Place actor at start in HexPositions.
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(new_actor, start);

    // Insert the UnitIdMap entry for the new entity.
    app.world_mut()
        .resource_mut::<UnitIdMap>()
        .insert(new_actor, new_actor_uid);

    // Seed CombatStateRes with one unit at start position.
    {
        use storyforge::combat_engine::state::{CombatState, RoundPhase};
        let unit = common::engine_unit::EngineUnitBuilder::new(new_actor_uid.0)
            .pos_hex(start)
            .build();
        let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        app.world_mut().resource_mut::<CombatStateRes>().0 = state;
    }

    // --- Phase C: directly mutate engine state (no mirror to undo it) ---
    let target_pos = hex_from_offset(3, 2);
    let target_hp = 12_i32;
    let target_mp = 3_i32;
    let target_reactions: i32 = 0;

    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = res.0.unit_mut(new_actor_uid).expect("unit must be in engine state");
        unit.pos = target_pos;
        // Mutate HP via the pool (pools[Hp] is the canonical source since Stage 3c).
        let max_hp = unit.max_hp();
        unit.pools[combat_engine::PoolKind::Hp] = Some((target_hp, max_hp));
        unit.pools[combat_engine::PoolKind::Mp] = Some((target_mp, target_mp));
        unit.reactions_left = target_reactions;
    }

    // --- Phase D: run projector ---
    app.update();

    // Assert all four fields were projected to ECS.
    let ecs_pos = app
        .world()
        .resource::<HexPositions>()
        .get(&new_actor)
        .expect("actor must have an ECS position");
    assert_eq!(ecs_pos, target_pos, "HexPositions must match engine pos after projection");

    let entity_ref = app.world().entity(new_actor);

    let ecs_hp = entity_ref.get::<Vital>().expect("actor must have Vital").hp;
    assert_eq!(ecs_hp, target_hp, "Vital.hp() must match engine hp after projection");

    let ecs_mp = entity_ref
        .get::<ActionPoints>()
        .expect("actor must have ActionPoints")
        .movement_points;
    assert_eq!(ecs_mp, target_mp, "ActionPoints.movement_points must match engine after projection");

    let ecs_reactions = entity_ref
        .get::<Reactions>()
        .expect("actor must have Reactions")
        .remaining;
    assert_eq!(
        ecs_reactions,
        target_reactions as u8,
        "Reactions.remaining must match engine reactions_left after projection"
    );
}

/// Projector writes `Mana.current` from engine state.
///
/// `CombatantBundle` does not include `Mana`, so it is inserted manually.
/// Engine state is seeded with `mana = Some((4, 10))`; after `app.update()`
/// the ECS `Mana.current` must equal 4.
#[test]
fn projector_writes_mana_from_engine_state() {
    use storyforge::game::components::Mana;
    use storyforge::combat_engine::state::{CombatState, RoundPhase};

    let start = hex_from_offset(0, 0);
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);

    // Add Mana component — not part of CombatantBundle by default.
    app.world_mut()
        .entity_mut(actor)
        .insert(Mana { current: 10, max: 10 });

    let actor_uid = entity_to_uid(actor);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);

    // Seed engine state with mana pool at full.
    let unit = common::engine_unit::EngineUnitBuilder::new(actor_uid.0)
        .pos_hex(start)
        .ap(1, 1)
        .mana(10, 10)
        .build();
    let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Mutate engine mana to simulate a cast that spent mana.
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.pools[combat_engine::PoolKind::Mana] = Some((4, 10));
    }

    app.update();

    let mana = app
        .world()
        .entity(actor)
        .get::<Mana>()
        .expect("Mana component must exist");
    assert_eq!(mana.current, 4, "Mana.current must equal engine mana after projection");
    assert_eq!(mana.max, 10, "Mana.max must remain unchanged");
}

/// Projector writes `StatusEffects` from engine state.
///
/// Engine state carries one status `(poison, actor_uid)`; after `app.update()`
/// the ECS `StatusEffects.0` must contain exactly that entry.
#[test]
fn projector_writes_statuses_from_engine_state() {
    use storyforge::combat_engine::state::{
        ActiveStatus as EngineActiveStatus, CombatState, RoundPhase,
    };

    let start = hex_from_offset(0, 0);
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);

    let actor_uid = entity_to_uid(actor);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);

    // Seed engine state with no statuses yet.
    let unit = common::engine_unit::EngineUnitBuilder::new(actor_uid.0)
        .pos_hex(start)
        .ap(1, 1)
        .build();
    let state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Push a status into engine state (simulates Cast having applied poison).
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.statuses.push(EngineActiveStatus {
            id: StatusId::from("poison"),
            rounds_remaining: 3,
            dot_per_tick: 2,
            applier: combat_engine::state::EffectSource::Unit(actor_uid),
        });
    }

    app.update();

    let status_effects = app
        .world()
        .entity(actor)
        .get::<StatusEffects>()
        .expect("actor must have StatusEffects");
    assert_eq!(status_effects.0.len(), 1, "exactly one status projected");
    let s = &status_effects.0[0];
    assert_eq!(s.id, StatusId::from("poison"), "status id must match");
    assert_eq!(s.rounds_remaining, 3, "rounds_remaining must match");
    assert_eq!(s.dot_per_tick, 2, "dot_per_tick must match");
    assert_eq!(s.applier, Some(actor), "applier entity must resolve to actor");
}

// Projector preserves aura-applied ECS statuses that the engine doesn't know about.
///
/// The aura entry (written by `apply_auras_system` in TurnStart, after
/// `init_state_from_ecs`) has a different applier entity than the
/// ability-applied entry in engine state.  After projection:
/// - "aura_buff" (aura_source applier, not in engine) → preserved.
/// - "burning"   (actor_uid applier, only in engine)  → projected in.
#[test]
fn projector_preserves_aura_applied_status_during_cast_projection() {
    use storyforge::combat_engine::state::{
        ActiveStatus as EngineActiveStatus, CombatState, RoundPhase, Team as EngineTeam,
    };

    let start = hex_from_offset(0, 0);
    let start2 = hex_from_offset(1, 0);
    let mut app = common::apps::bridge::projector_only_app();

    let actor = common::apps::bridge::spawn_caster(&mut app, start, vec![]);
    let aura_source = common::apps::bridge::spawn_caster(&mut app, start2, vec![]);

    let actor_uid = entity_to_uid(actor);
    let aura_source_uid = entity_to_uid(aura_source);
    app.world_mut().resource_mut::<UnitIdMap>().insert(actor, actor_uid);
    app.world_mut().resource_mut::<UnitIdMap>().insert(aura_source, aura_source_uid);

    // Pre-seed ECS: actor already has an aura-applied status (aura_buff from aura_source).
    app.world_mut()
        .entity_mut(actor)
        .get_mut::<StatusEffects>()
        .expect("actor must have StatusEffects")
        .0
        .push(ActiveStatus {
            id: StatusId::from("aura_buff"),
            rounds_remaining: 1,
            dot_per_tick: 0,
            applier: Some(aura_source),
        });

    // Seed engine state: actor has no statuses.
    let make_unit = |id: storyforge::combat_engine::state::UnitId, _team: EngineTeam, pos| {
        common::engine_unit::EngineUnitBuilder::new(id.0)
            .pos_hex(pos)
            .ap(1, 1)
            .build()
    };
    let state = CombatState::new(
        vec![
            make_unit(actor_uid, EngineTeam::Player, start),
            make_unit(aura_source_uid, EngineTeam::Player, start2),
        ],
        1, RoundPhase::ActorTurn, 0,
    );
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Engine: Cast has applied "burning" to actor (self-applied).
    {
        let mut res = app.world_mut().resource_mut::<CombatStateRes>();
        let u = res.0.unit_mut(actor_uid).expect("unit in state");
        u.statuses.push(EngineActiveStatus {
            id: StatusId::from("burning"),
            rounds_remaining: 2,
            dot_per_tick: 1,
            applier: combat_engine::state::EffectSource::Unit(actor_uid),
        });
    }

    app.update();

    let status_effects = app
        .world()
        .entity(actor)
        .get::<StatusEffects>()
        .expect("actor must have StatusEffects");
    assert_eq!(
        status_effects.0.len(),
        2,
        "both aura_buff (preserved) and burning (projected) must be present"
    );

    let ids: Vec<&str> = status_effects.0.iter().map(|s| s.id.0.as_str()).collect();
    assert!(ids.contains(&"aura_buff"), "aura_buff must be preserved");
    assert!(ids.contains(&"burning"), "burning must be projected from engine");

    let aura_entry = status_effects
        .0
        .iter()
        .find(|s| s.id.0 == "aura_buff")
        .unwrap();
    assert_eq!(aura_entry.applier, Some(aura_source), "aura_buff applier must be aura_source entity");

    let burning_entry = status_effects
        .0
        .iter()
        .find(|s| s.id.0 == "burning")
        .unwrap();
    assert_eq!(burning_entry.applier, Some(actor), "burning applier maps from actor_uid to actor entity");
}

/// `from_ecs` (bootstrap round 1) aggregates `armor_bonus` and `speed_bonus` from
/// a pre-seeded `StatusEffects` component on a party hero.
///
/// Scenario: hero spawns with an "injured" status (armor_bonus=-1, speed_bonus=-1).
/// After `bootstrap_combat_state` the engine unit's `agg.armor_bonus` and
/// `agg.speed_bonus` must both be -1.
#[test]
fn from_ecs_round1_aggregates_preseeded_status_bonuses() {
    use storyforge::combat::engine_bridge::{CombatStateRes, entity_to_uid};
    use storyforge::content::content_view::ActiveContent;
    use storyforge::content::statuses::StatusDef;
    use storyforge::game::components::{ActiveStatus, StatusEffects};
    use combat_engine::{StatusId, StatusDef as EngineStatusDef, StatusBonuses, PERMANENT_DURATION};

    let start = hex_from_offset(0, 0);
    let mut app = common::apps::bridge::bridge_app();

    let hero = common::apps::bridge::spawn_caster(&mut app, start, vec![]);

    // Pre-seed StatusEffects with "injured" (armor_bonus=-1, speed_bonus=-1).
    let hero_id = hero;
    app.world_mut()
        .entity_mut(hero)
        .get_mut::<StatusEffects>()
        .expect("StatusEffects should be on hero after bundle spawn")
        .0
        .push(ActiveStatus {
            id: StatusId::from("injured"),
            rounds_remaining: PERMANENT_DURATION,
            applier: Some(hero_id),
            dot_per_tick: 0,
        });

    // Register the "injured" status in ActiveContent so from_ecs can look up bonuses.
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .statuses
        .insert(
            StatusId::from("injured"),
            StatusDef {
                id: StatusId::from("injured"),
                name: "Injured".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: EngineStatusDef {
                    causes_disadvantage: false,
                    blocks_mana_abilities: false,
                    forces_targeting: false,
                    skips_turn: false,
                    bonuses: StatusBonuses {
                        armor_bonus: -1,
                        speed_bonus: -1,
                        damage_taken_bonus: 0,
                    },
                    hp_percent_dot: 0,
                },
            },
        );

    common::apps::bridge::bootstrap(&mut app);

    let uid = entity_to_uid(hero);
    let state = app.world().resource::<CombatStateRes>();
    let unit = state.0.unit(uid).expect("hero must be in engine state after bootstrap");

    // from_ecs seeds speed = base_speed + speed_bonus from statuses.
    // Hero was spawned with speed=6 (bridge_stats default). injured adds -1 → effective speed=5.
    assert_eq!(unit.armor_bonus, -1, "armor_bonus must be -1 from injured status");
    assert_eq!(unit.speed, 6 - 1, "effective speed must be base(6) + injured speed_bonus(-1)");
}

