//! Bridge smoke tests: phase transitions, turn-lifecycle handoff, and DoT-on-handoff semantics.
//!
//! Covers boss phase transition triggered by cast damage (ECS write + CombatLog),
//! the bridge's auto-end-turn contract when cast exhausts AP+MP (gate B-α), and
//! DoT applier semantics during turn handoff — hero's poison ticks at hero's turn
//! start, not at the handoff to the enemy's turn.

use bevy::prelude::*;

use combat_engine::{AbilityId, DiceExpr, StatusId};
use storyforge::combat::bridge::{entity_to_uid, CombatStateRes};
use storyforge::content::abilities::{AbilityRange, AoEShape, EffectDef};
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{CombatStats, Team, Vital};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::HexPositions;

use super::common;

#[test]
fn phase_transition_via_cast_writes_ecs_and_emits_log_entry() {
    use bevy::prelude::Name as BevyName;
    use storyforge::content::abilities::TargetType;
    use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
    use storyforge::game::components::EnemyPhases;

    let caster_pos = hex_from_offset(0, 0);
    let boss_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("phase_nuke");
    // 0d1+60 → constant 60 damage, strength=0 so str_mod=0, boss armor=0.
    let ability_def = common::apps::bridge::bevy_ability(
        "phase_nuke",
        "Phase Nuke",
        combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::Damage {
                dice: DiceExpr::new(0, 1, 60),
            },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    );

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

    // Caster: str=0 so str_mod=0, damage is purely from the +60 bonus.
    let zero_str_stats = CombatStats {
        max_hp: 20,
        strength: 0,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        zero_str_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );

    // Boss: max_hp=100, armor=0. Pending phase at 50% threshold.
    let boss_stats = CombatStats {
        max_hp: 100,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let phase = PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(50),
        name: Some("Phase Two".into()),
        stats: None,
        ability_ids: None,
        heal_to_full: true,
        flavor: Some("Boss enters phase two!".into()),
        victory_override: None,
        turn_limit: None,
        ai_behavior: None,
        tags: None,
        equipment: None,
        base_speed: None,
    };
    let boss = app
        .world_mut()
        .spawn((
            CombatantBundle::new(
                Team::Enemy,
                boss_stats,
                0,
                0,
                6,
                vec![],
                common::apps::bridge::no_equipment(),
            ),
            EnemyPhases {
                pending: vec![phase],
            },
            BevyName::new("Boss"),
        ))
        .id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(boss, boss_hex);

    common::apps::bridge::bootstrap(&mut app);

    // Script d20 to 11 so crit-fail doesn't fire.
    common::apps::bridge::script_no_crit_fail(&mut app);

    common::apps::bridge::write_cast(&mut app, caster, ability_id, boss, boss_hex);

    app.update();

    // --- Assertions ---

    // 1. EnemyPhases.pending is empty (pop happened).
    let phases = app
        .world()
        .entity(boss)
        .get::<EnemyPhases>()
        .expect("boss must retain EnemyPhases component");
    assert!(
        phases.pending.is_empty(),
        "EnemyPhases.pending must be empty after phase transition; got: {:?}",
        phases.pending,
    );

    // 2. Boss Name == "Phase Two" (ECS-only delta was written).
    let name = app
        .world()
        .entity(boss)
        .get::<BevyName>()
        .expect("boss must have Name");
    assert_eq!(
        name.as_str(),
        "Phase Two",
        "boss name must update to new phase name"
    );

    // 3. Boss is alive (heal_to_full: engine revived, Dead was not inserted).
    let vital = app
        .world()
        .entity(boss)
        .get::<Vital>()
        .expect("boss must have Vital");
    assert!(
        vital.is_alive(),
        "boss must be alive after phase transition (heal_to_full=true)"
    );
    assert_eq!(
        vital.hp, vital.max_hp,
        "boss must be healed to full after phase transition"
    );

    // 4. CombatLog contains PhaseEntered with correct prev/next name.
    let log = app.world().resource::<CombatLog>();
    let phase_entry = log.0.iter().find_map(|e| {
        if let CombatEvent::PhaseEntered {
            actor,
            prev_name,
            next_name,
            flavor,
        } = e
        {
            Some((*actor, prev_name.clone(), next_name.clone(), flavor.clone()))
        } else {
            None
        }
    });
    let (pe_actor, pe_prev, pe_next, pe_flavor) =
        phase_entry.expect("CombatLog must contain PhaseEntered; full log: {log:?}");
    assert_eq!(pe_actor, boss, "PhaseEntered.actor must be the boss entity");
    assert_eq!(
        pe_prev, "Boss",
        "PhaseEntered.prev_name must be original boss name"
    );
    assert_eq!(
        pe_next, "Phase Two",
        "PhaseEntered.next_name must be new phase name"
    );
    assert_eq!(
        pe_flavor,
        Some("Boss enters phase two!".into()),
        "PhaseEntered.flavor must match"
    );
}

// ── Phase B-α: lock-in tests (pre-S6 bridge contract) ─────────────────────────

/// Locks in the observable bridge contract for Cast-that-exhausts-AP/MP.
///
/// Today the bridge has a synchronous auto-end block that fires step(EndTurn)
/// when the caster's AP+MP hit 0 after a cast.  After B-γ (S6) the engine will
/// self-end its turn, but the bridge output observable to the UI must be
/// identical in both cases.  This test pins that contract.
///
/// Setup: hero casts an ability with cost_ap=1.  Hero starts with AP=1, MP=0
/// so after the cast AP=0, MP=0 → auto-end fires.
///
/// Assertions (must pass pre- AND post-S6):
///  - CombatLog contains, in order: AbilityUsed(hero), TurnEnded(hero), TurnStarted(enemy).
///  - ActiveCombatant migrated from hero to enemy.
#[test]
fn cast_via_bridge_exhausting_ap_mp_emits_turn_lifecycle_in_log() {
    use storyforge::content::abilities::TargetType;
    use storyforge::game::components::ActiveCombatant;

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();

    // Register a minimal cast ability (no damage needed — we just need AP cost).
    let ability_id = AbilityId::from("exhausting_zap");
    let ability_def = common::apps::bridge::bevy_ability(
        "exhausting_zap",
        "Exhausting Zap",
        combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    );
    common::apps::bridge::insert_ability(&mut app, ability_def);

    let hero = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let enemy = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    // Set engine turn queue: hero is index=0 (current), enemy is index=1.
    // bootstrap() doesn't set the engine turn queue when the ECS TurnQueue is
    // empty (bridge tests don't run build_turn_order). Without this, step(EndTurn)
    // inside the auto-end block fails with NotCurrent and is silently swallowed.
    let hero_uid = entity_to_uid(hero);
    let enemy_uid = entity_to_uid(enemy);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        state.0.set_turn_queue(vec![hero_uid, enemy_uid], 0);
    }

    // Set hero: AP=1 (default is 1 from CombatantBundle), MP=0.
    // Bridge auto-end fires when AP<=0 && MP<=0 after cast.
    common::apps::bridge::with_engine_unit(&mut app, hero, |u| {
        u.pools[combat_engine::PoolKind::Ap] = Some((1, 1));
        u.pools[combat_engine::PoolKind::Mp] = Some((0, 6));
    });

    // Insert ActiveCombatant on hero to simulate it being the active combatant.
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, hero, ability_id, enemy, target_pos);
    app.update();

    // ── Assert CombatLog order ────────────────────────────────────────────────
    let log = app.world().resource::<CombatLog>();

    // Find indices of the relevant events.
    let ability_used_idx = log
        .0
        .iter()
        .position(|e| matches!(e, CombatEvent::AbilityUsed { .. }));
    let turn_ended_idx = log
        .0
        .iter()
        .position(|e| matches!(e, CombatEvent::TurnEnded { actor: a, .. } if *a == hero));
    let turn_started_enemy_idx = log
        .0
        .iter()
        .position(|e| matches!(e, CombatEvent::TurnStarted { actor: a } if *a == enemy));

    assert!(
        ability_used_idx.is_some(),
        "CombatLog must contain AbilityUsed; log: {:?}",
        log.0,
    );
    assert!(
        turn_ended_idx.is_some(),
        "CombatLog must contain TurnEnded(hero); log: {:?}",
        log.0,
    );
    assert!(
        turn_started_enemy_idx.is_some(),
        "CombatLog must contain TurnStarted(enemy); log: {:?}",
        log.0,
    );

    // Order: AbilityUsed → TurnEnded(hero) → TurnStarted(enemy).
    let au = ability_used_idx.unwrap();
    let te = turn_ended_idx.unwrap();
    let ts = turn_started_enemy_idx.unwrap();
    assert!(
        au < te && te < ts,
        "expected AbilityUsed[{au}] < TurnEnded[{te}] < TurnStarted[{ts}]; log: {:?}",
        log.0,
    );

    // ── Assert ActiveCombatant migrated hero → enemy ──────────────────────────
    // apply_pending_turn_lifecycle_system runs after process_action_system via
    // PendingTurnLifecycle.remove_active / insert_active queues.
    // After the full app.update(), ActiveCombatant should be on enemy, not hero.
    assert!(
        app.world().get::<ActiveCombatant>(enemy).is_some(),
        "enemy must have ActiveCombatant after turn handoff",
    );
    assert!(
        app.world().get::<ActiveCombatant>(hero).is_none(),
        "hero must NOT have ActiveCombatant after turn handoff",
    );
}

/// Locks in the DoT applier semantics during Cast-that-exhausts-AP/MP.
///
/// Hero has a poison status applied to the enemy (applier=hero_uid).
/// `tick_actor_statuses` fires for statuses where `applier == starting_actor`,
/// so hero's poison-on-enemy ticks when HERO's turn starts (round 2+), NOT
/// when the turn hands off to the enemy.
///
/// This test pins the semantic so B-γ cannot accidentally invert it.
/// Must pass both pre- and post-S6.
#[test]
fn cast_with_dot_status_ticks_next_actor_dot_on_handoff() {
    use storyforge::combat::bridge::entity_to_uid;
    use storyforge::combat_engine::state::ActiveStatus as EngineActiveStatus;
    use storyforge::content::abilities::TargetType;

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();

    // Register ability (AP=1, no damage, just to trigger exhaustion).
    let ability_id = AbilityId::from("final_strike");
    let ability_def = common::apps::bridge::bevy_ability(
        "final_strike",
        "Final Strike",
        combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    );
    common::apps::bridge::insert_ability(&mut app, ability_def);

    // Register a poison StatusDef with hp_percent_dot=10 so ticking it would
    // deal damage.  If the DoT fires on handoff, enemy HP drops — and we assert
    // it does NOT drop.
    let poison_id = StatusId::from("hero_poison");
    app.world_mut()
        .resource_mut::<storyforge::content::content_view::ActiveContent>()
        .0
        .statuses
        .insert(
            poison_id.clone(),
            common::apps::bridge::bevy_status(
                "hero_poison",
                combat_engine::StatusDef {
                    hp_percent_dot: 10,
                    heal_per_tick: 0,
                    ..Default::default()
                },
            ),
        );

    let hero = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let enemy = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec![],
        common::apps::bridge::no_equipment(),
        target_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    let hero_uid = entity_to_uid(hero);
    let enemy_uid = entity_to_uid(enemy);

    // Set engine turn queue: hero is index=0 (current), enemy is index=1.
    // Required for step(EndTurn) in the bridge auto-end block to succeed.
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        state.0.set_turn_queue(vec![hero_uid, enemy_uid], 0);
    }

    // Record enemy HP before the cast.
    let enemy_hp_before = app
        .world()
        .resource::<CombatStateRes>()
        .0
        .unit(enemy_uid)
        .unwrap()
        .hp();

    // Inject hero's poison onto the enemy engine unit directly.
    // applier=hero_uid means it ticks at hero's turn start, NOT enemy's.
    common::apps::bridge::with_engine_unit(&mut app, enemy, |u| {
        u.statuses.push(EngineActiveStatus {
            id: poison_id.clone(),
            rounds_remaining: 3,
            dot_per_tick: 0, // flat tick = 0; damage comes from hp_percent_dot in StatusDef
            applier: combat_engine::state::EffectSource::Unit(hero_uid),
        });
    });

    // Hero: AP=1, MP=0 → cast exhausts AP.
    common::apps::bridge::with_engine_unit(&mut app, hero, |u| {
        u.pools[combat_engine::PoolKind::Ap] = Some((1, 1));
        u.pools[combat_engine::PoolKind::Mp] = Some((0, 6));
    });

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, hero, ability_id, enemy, target_pos);
    app.update();

    // Enemy HP must NOT have changed — hero's poison ticks at HERO's round-2
    // turn start, not at the handoff to enemy's turn.
    let enemy_hp_after = app
        .world()
        .resource::<CombatStateRes>()
        .0
        .unit(enemy_uid)
        .unwrap()
        .hp();

    assert_eq!(
        enemy_hp_after, enemy_hp_before,
        "enemy HP must be unchanged immediately after cast-and-handoff; \
         hero's poison (applier=hero) ticks at hero's turn start (round 2+), \
         not at enemy's turn start. before={enemy_hp_before}, after={enemy_hp_after}",
    );
}

/// A `PhaseDef` with `tags = Some({...})` is projected into the engine
/// `PhaseEntry.tags` during bootstrap (`from_ecs` / `bootstrap_combat_state`).
///
/// This covers Step 4 of Slice C2: the `tags: phase.tags.clone()` path in the
/// `enemy_phases` projection loop.
#[test]
fn phase_def_tags_carried_into_engine_phase_entry() {
    use combat_engine::TagId;
    use std::collections::BTreeSet;
    use storyforge::combat::bridge::CombatStateRes;
    use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
    use storyforge::game::components::EnemyPhases;

    let boss_hex = hex_from_offset(1, 0);

    let boss_stats = CombatStats {
        max_hp: 100,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let phase_tags: BTreeSet<TagId> = ["aberration", "incorporeal"]
        .iter()
        .map(|s| TagId::from(*s))
        .collect();
    let phase = PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(50),
        name: None,
        stats: None,
        ability_ids: None,
        heal_to_full: false,
        flavor: None,
        victory_override: None,
        turn_limit: None,
        ai_behavior: None,
        tags: Some(phase_tags.clone()),
        equipment: None,
        base_speed: None,
    };

    let mut app = common::apps::bridge::bridge_app();
    let boss = app
        .world_mut()
        .spawn((
            storyforge::game::bundles::CombatantBundle::new(
                storyforge::game::components::Team::Enemy,
                boss_stats,
                0,
                0,
                6,
                vec![],
                common::apps::bridge::no_equipment(),
            ),
            EnemyPhases {
                pending: vec![phase],
            },
        ))
        .id();
    app.world_mut()
        .resource_mut::<storyforge::game::resources::HexPositions>()
        .insert(boss, boss_hex);

    common::apps::bridge::bootstrap(&mut app);

    // After bootstrap, the engine state should have the boss's phase with tags populated.
    let state = app.world().resource::<CombatStateRes>();
    let boss_uid = entity_to_uid(boss);
    let unit = state
        .0
        .unit(boss_uid)
        .expect("boss must be in engine state after bootstrap");
    assert_eq!(
        unit.enemy_phases.len(),
        1,
        "boss must have one engine phase entry"
    );
    let entry = &unit.enemy_phases[0];
    assert_eq!(
        entry.tags.as_ref(),
        Some(&phase_tags),
        "engine PhaseEntry.tags must match PhaseDef.tags after bootstrap carry; got: {:?}",
        entry.tags,
    );
}

/// After a phase transition (triggered by cast damage), the ECS `Tags` component
/// on the boss is replaced with the phase's tag set.
///
/// This covers Step 5 of Slice C2: the `commands.entity(ent).insert(Tags(...))` path
/// in `apply_phase_ecs_writes`.
#[test]
fn phase_transition_updates_ecs_tags_component() {
    use combat_engine::TagId;
    use std::collections::BTreeSet;
    use storyforge::content::abilities::TargetType;
    use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
    use storyforge::game::components::{EnemyPhases, Tags};

    let caster_pos = hex_from_offset(0, 0);
    let boss_hex = hex_from_offset(1, 0);

    let ability_id = AbilityId::from("tag_nuke");
    let ability_def = common::apps::bridge::bevy_ability(
        "tag_nuke",
        "Tag Nuke",
        combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::Damage {
                dice: DiceExpr::new(0, 1, 60),
            },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    );

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

    let zero_str_stats = CombatStats {
        max_hp: 20,
        strength: 0,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        storyforge::game::components::Team::Player,
        zero_str_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );

    let boss_stats = CombatStats {
        max_hp: 100,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let new_tags: BTreeSet<TagId> = ["aberration", "incorporeal"]
        .iter()
        .map(|s| TagId::from(*s))
        .collect();
    let phase = PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(50),
        name: None,
        stats: None,
        ability_ids: None,
        heal_to_full: false,
        flavor: None,
        victory_override: None,
        turn_limit: None,
        ai_behavior: None,
        tags: Some(new_tags.clone()),
        equipment: None,
        base_speed: None,
    };

    let boss = app
        .world_mut()
        .spawn((
            storyforge::game::bundles::CombatantBundle::new(
                storyforge::game::components::Team::Enemy,
                boss_stats,
                0,
                0,
                6,
                vec![],
                common::apps::bridge::no_equipment(),
            ),
            EnemyPhases {
                pending: vec![phase],
            },
            Name::new("TagBoss"),
            // Boss starts with no Tags component — the phase will insert one.
        ))
        .id();
    app.world_mut()
        .resource_mut::<storyforge::game::resources::HexPositions>()
        .insert(boss, boss_hex);

    common::apps::bridge::bootstrap(&mut app);
    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, boss, boss_hex);
    app.update();

    // After the phase transition, the ECS Tags component must hold the phase's tag set.
    let tags_component = app
        .world()
        .entity(boss)
        .get::<Tags>()
        .expect("boss must have Tags component after phase transition with tags");
    assert_eq!(
        tags_component.0, new_tags,
        "ECS Tags must be replaced with the phase's tag set after transition; got: {:?}",
        tags_component.0,
    );
}

/// After a phase transition the bridge mirrors `engine Unit.runtime` → ECS
/// `RuntimeStatsMirror` as a single POD assignment.
///
///   `RuntimeStatsMirror.0` == `engine_unit.runtime`
///
/// This is the single-source-of-truth invariant guarded by `apply_phase_ecs_writes`.
/// The `PhaseEntry.runtime` is injected directly into the engine state after bootstrap
/// (bypassing the PhaseDef→PhaseEntry equipment-derivation path) so the test stays
/// content-free while still exercising the full bridge mirror path.
#[test]
fn phase_transition_mirrors_runtime_stats_into_ecs() {
    use combat_engine::RuntimeStats;
    use storyforge::combat::bridge::CombatStateRes;
    use storyforge::content::abilities::TargetType;
    use storyforge::content::encounters::{PhaseDef, PhaseTrigger};
    use storyforge::game::components::{EnemyPhases, RuntimeStatsMirror};

    let caster_pos = hex_from_offset(0, 0);
    let boss_hex = hex_from_offset(1, 0);

    // Damage ability: 0d1+60 → constant 60, pierces boss armor so the threshold
    // is crossed regardless of the boss's initial armor.
    let ability_id = AbilityId::from("runtime_nuke");
    let ability_def = common::apps::bridge::bevy_ability(
        "runtime_nuke",
        "Runtime Nuke",
        combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: combat_engine::AbilityRange { min: 0, max: 5 },
            effect: combat_engine::EffectDef::Damage {
                dice: DiceExpr::new(0, 1, 60),
            },
            costs: vec![],
            cost_ap: 1,
            aoe: combat_engine::AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    );

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

    // Caster: str=0 so str_mod=0, damage is purely from the +60 bonus.
    let zero_str_stats = CombatStats {
        max_hp: 20,
        strength: 0,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        zero_str_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );

    // Boss: max_hp=100, armor=0. Phase at 50% — no equipment so runtime stays
    // None at bootstrap; we'll patch it into the engine state below.
    let boss_stats = CombatStats {
        max_hp: 100,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    };
    let phase = PhaseDef {
        trigger: PhaseTrigger::HpBelowPct(50),
        name: None,
        stats: None,
        ability_ids: None,
        heal_to_full: true,
        flavor: None,
        victory_override: None,
        turn_limit: None,
        ai_behavior: None,
        tags: None,
        equipment: None, // runtime injected directly into engine state below
        base_speed: None,
    };
    let boss = app
        .world_mut()
        .spawn((
            CombatantBundle::new(
                Team::Enemy,
                boss_stats,
                0,
                0,
                6,
                vec![],
                common::apps::bridge::no_equipment(),
            ),
            EnemyPhases {
                pending: vec![phase],
            },
            Name::new("RuntimeBoss"),
        ))
        .id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(boss, boss_hex);

    common::apps::bridge::bootstrap(&mut app);

    // Patch the engine PhaseEntry to carry a known RuntimeStats.
    // This exercises the bridge mirror (apply_phase_ecs_writes reads engine_unit.runtime)
    // without needing real equipment content.
    let new_runtime = RuntimeStats {
        armor: 12,
        magic_resist: 6,
        base_speed: 3,
    };
    let boss_uid = entity_to_uid(boss);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        if let Some(u) = state.0.unit_mut(boss_uid) {
            if let Some(entry) = u.enemy_phases.get_mut(0) {
                entry.runtime = Some(new_runtime);
            }
        }
    }

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, boss, boss_hex);
    app.update();

    // The engine must have applied the runtime replacement.
    let state = app.world().resource::<CombatStateRes>();
    let engine_unit = state
        .0
        .unit(boss_uid)
        .expect("boss must be in engine state after phase transition");
    assert_eq!(
        engine_unit.runtime, new_runtime,
        "engine Unit.runtime must equal phase RuntimeStats after EnterPhase"
    );

    // The ECS RuntimeStatsMirror must equal the engine Unit.runtime POD.
    let mirror = app
        .world()
        .entity(boss)
        .get::<RuntimeStatsMirror>()
        .expect("boss must have RuntimeStatsMirror");
    assert_eq!(
        mirror.0, engine_unit.runtime,
        "RuntimeStatsMirror.0 must equal engine Unit.runtime after phase mirror"
    );
}
