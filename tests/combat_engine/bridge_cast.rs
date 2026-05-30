//! Bridge smoke tests: cast routing, log emission, crit-fail, and summon.
//!
//! Covers `ActionInput::Cast` routing through `process_action_system`, CombatLog
//! emission for damage, status-applied, mana-changed, and critical-miss events,
//! crit-fail positive/negative cases, and synchronous ECS entity creation on
//! summon cast.

use bevy::prelude::*;

use storyforge::combat::engine_bridge::{entity_to_uid, CombatStateRes, UnitIdMap};
use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef};
use storyforge::content::content_view::ActiveContent;
use combat_engine::{AbilityId, DiceExpr, StatusId};
use storyforge::game::combat_log::{CombatEvent, CombatLog};
use storyforge::game::components::{
    CombatStats,
    Team,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::HexPositions;

use super::common;


// ── Phase 2 step 7b: Cast event → CombatLog translation tests ───────────────

/// Run a full cast round-trip and invoke `assert_log` on the resulting `CombatLog`.
///
/// `stats` is used for the caster. `setup_unit` is called after bootstrap to
/// mutate the engine unit (e.g. set mana). All other setup is identical across
/// the three "cast emits …" tests.
fn run_cast_log_test(
    ability: AbilityDef,
    caster_stats: CombatStats,
    setup_unit: impl FnOnce(&mut combat_engine::state::Unit),
    assert_log: impl FnOnce(&CombatLog),
) {
    let ability_id = ability.id.clone();
    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability);

    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        caster_stats,
        0,
        6,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let target = common::apps::bridge::spawn_unit(
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

    let caster_uid = entity_to_uid(caster);
    // AP is now in pools (canonical since Phase C-6); no legacy field write needed.
    setup_unit(
        app.world_mut()
            .resource_mut::<CombatStateRes>()
            .0
            .unit_mut(caster_uid)
            .unwrap(),
    );

    common::apps::bridge::script_no_crit_fail(&mut app);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, target, target_pos);
    app.update();

    assert_log(app.world().resource::<CombatLog>());

    let _ = target; // silence unused-variable warning
}



/// Cast with `EffectDef::Damage` emits `AbilityUsed` + `DamageResult` in CombatLog.
///
/// Caster has strength=0 (str_mod=0) so damage = dice bonus only (5).
/// Target has armor=0, so armor_reduced=0 and final_damage=5.
/// The crit-fail d20 is scripted to 11 (non-1) to ensure normal resolution.
#[test]
fn cast_emits_damage_result_log_entry() {
    use storyforge::content::abilities::TargetType;

    let zero_str_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };
    let ability_def = AbilityDef {
        id: AbilityId::from("dmg_ability"),
        name: "Fireball".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            effect: EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) },
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: None,
        },
    };

    run_cast_log_test(ability_def, zero_str_stats, |_| {}, |log| {
        let ability_used: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::AbilityUsed { actor: a, ability_name, target: t, is_aoe, .. } = e {
                Some((*a, ability_name.clone(), *t, *is_aoe))
            } else { None }
        }).collect();
        assert_eq!(ability_used.len(), 1, "expected exactly one AbilityUsed, got {:?}", ability_used);
        let (_, au_name, _, au_aoe) = &ability_used[0];
        assert_eq!(au_name, "Fireball");
        assert!(!au_aoe, "ability is not AoE");

        let dmg_results: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::DamageResult { target: t, final_damage, armor_reduced, .. } = e {
                Some((*t, *final_damage, *armor_reduced))
            } else { None }
        }).collect();
        assert_eq!(dmg_results.len(), 1, "expected exactly one DamageResult, got {:?}", dmg_results);
        let (_, dr_dmg, dr_armor) = dmg_results[0];
        assert_eq!(dr_dmg, 5, "final_damage must be 5 (0d1+5, str_mod=0, armor=0)");
        assert_eq!(dr_armor, 0, "armor_reduced must be 0");
    });
}

/// Cast with status-only ability emits `AbilityUsed` + `StatusApplied` in CombatLog.
///
/// The ability has `EffectDef::None` and one status on the target.
#[test]
fn cast_emits_status_applied_log_entry() {
    use storyforge::content::abilities::{StatusApplication, StatusOn, TargetType};

    let status_id = StatusId::from("burning");
    let ability_def = AbilityDef {
        id: AbilityId::from("burning_touch"),
        name: "Burning Touch".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication { status: status_id.clone(), duration_rounds: 2, on: StatusOn::Target }],
            key: None,
            requires_los: false,
            passive: None,
        },
    };

    run_cast_log_test(ability_def, common::apps::bridge::bridge_stats(), |_| {}, |log| {
        let status_events: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::StatusApplied { target: t, status } = e {
                Some((*t, status.clone()))
            } else { None }
        }).collect();
        assert_eq!(status_events.len(), 1, "expected exactly one StatusApplied, got {:?}", status_events);
        let (_, ev_status) = &status_events[0];
        assert_eq!(*ev_status, status_id, "StatusApplied status must be 'burning'");
    });
}

/// Cast with mana cost emits `ManaChanged` in CombatLog.
///
/// Ability costs 3 mana; caster starts with mana=(10,10).
/// After cast: mana=(7,10) → bridge diff emits ManaChanged.
#[test]
fn cast_emits_mana_changed_log_entry() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let ability_def = AbilityDef {
        id: AbilityId::from("mana_blast"),
        name: "Mana Blast".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
            cost_ap: 0,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: None,
        },
    };

    run_cast_log_test(ability_def, common::apps::bridge::bridge_stats(), |unit| {
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
    }, |log| {
        let mana_events: Vec<_> = log.0.iter().filter_map(|e| {
            if let CombatEvent::PoolChanged {
                actor: a, pool: combat_engine::PoolKind::Mana,
                current, max, cause: combat_engine::PoolChangeCause::Spent
            } = e {
                Some((*a, *current, *max))
            } else { None }
        }).collect();
        assert_eq!(mana_events.len(), 1, "expected exactly one PoolChanged{{Spent,Mana}}, got {:?}", mana_events);
        let (_, mc_current, mc_max) = mana_events[0];
        assert_eq!(mc_current, 7, "mana after cast must be 10 - 3 = 7");
        assert_eq!(mc_max, 10, "mana max must be 10");
    });
}

// ── Phase 2 step 7a: ActionInput::Cast routing smoke test ────────────────────

#[test]
fn process_action_system_routes_cast_into_engine() {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let mut app = common::apps::bridge::bridge_app();

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let caster = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Player,
        common::apps::bridge::bridge_stats(),
        0,
        6,
        vec!["zap".into()],
        common::apps::bridge::no_equipment(),
        caster_pos,
    );
    let target = common::apps::bridge::spawn_target(&mut app, target_pos);

    // Register a Cast-able ability with a mana cost in ActiveContent.
    let zap_id = AbilityId::from("zap");
    let zap_def = AbilityDef {
        id: zap_id.clone(),
        name: "zap".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::None,
            // No damage in 7a — just verify cost flows through
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: None,
        },
    };
    common::apps::bridge::insert_ability(&mut app, zap_def);

    common::apps::bridge::bootstrap(&mut app);

    // CombatantBundle default AP=1; bump to 2 so post-cast AP=1 is observable.
    // Mana isn't a default Bevy component on CombatantBundle — set on engine
    // state directly so PayCost has a pool to deduct from.
    let caster_uid = entity_to_uid(caster);
    common::apps::bridge::with_engine_unit(&mut app, caster, |unit| {
        unit.pools[combat_engine::PoolKind::Ap] = Some((2, 2));
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
    });

    common::apps::bridge::write_cast(&mut app, caster, zap_id, target, target_pos);

    app.update();

    // Engine state: caster's AP and mana paid.
    let state = app.world().resource::<CombatStateRes>();
    let caster_unit = state.0.unit(caster_uid).expect("caster still in state");
    assert_eq!(
        caster_unit.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0),
        1,
        "AP cost paid"
    );
    assert_eq!(
        caster_unit.pools[combat_engine::PoolKind::Mana],
        Some((7, 10)),
        "Mana cost paid (10 - 3)"
    );
}

// ── Phase 2 step 7d: crit-fail event → CombatLog translation tests ───────────

/// Run a cast round-trip with a scripted d20 value and assert whether `CriticalMiss`
/// appears in the log.
///
/// `expect_crit_fail=true`  → log must contain `CriticalMiss`, must NOT contain `DamageResult`.
/// `expect_crit_fail=false` → log must NOT contain `CriticalMiss` or `CritFailSideEffect`.
fn run_crit_fail_log_test(d20: i32, expect_crit_fail: bool) {
    use storyforge::content::abilities::{ResourceCost, TargetType};
    use combat_engine::ResourceKind;

    let ability_id = AbilityId::from("cf_test_ability");
    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "CF Test".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            // Use damage for d20≠1 path so the cast has a visible effect; use None for d20=1 (miss).
            effect: if expect_crit_fail {
                EffectDef::None
            } else {
                EffectDef::Damage { dice: DiceExpr::new(0, 1, 5) }
            },
            costs: if expect_crit_fail {
                vec![ResourceCost { resource: ResourceKind::Mana, amount: 3 }]
            } else {
                vec![]
            },
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: None,
        },
    };

    let caster_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);
    let zero_str_stats = CombatStats {
        max_hp: 20, strength: 0, dexterity: 5, constitution: 10,
        intelligence: 0, wisdom: 10, charisma: 10,
    };

    let mut app = common::apps::bridge::bridge_app();
    common::apps::bridge::insert_ability(&mut app, ability_def);

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
    let target = common::apps::bridge::spawn_unit(
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

    let caster_uid = entity_to_uid(caster);
    {
        let mut state = app.world_mut().resource_mut::<CombatStateRes>();
        let unit = state.0.unit_mut(caster_uid).unwrap();
        // Mana pool set via canonical pools field (Phase C-6).
        unit.pools[combat_engine::PoolKind::Mana] = Some((10, 10));
        // AP is already set by bootstrap; pool is canonical since Phase C-6.
    }

    common::apps::bridge::script_d20(&mut app, d20);
    common::apps::bridge::write_cast(&mut app, caster, ability_id, target, target_pos);
    app.update();

    let log = app.world().resource::<CombatLog>();

    if expect_crit_fail {
        let crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { actor: a } if *a == caster));
        assert!(crit_miss, "CombatLog must contain CriticalMiss for the caster; got: {:?}", log.0);

        let has_damage = log.0.iter().any(|e| matches!(e, CombatEvent::DamageResult { .. }));
        assert!(!has_damage, "CombatLog must NOT contain DamageResult on crit-fail miss; got: {:?}", log.0);
    } else {
        let has_crit_miss = log.0.iter().any(|e| matches!(e, CombatEvent::CriticalMiss { .. }));
        let has_crit_side = log.0.iter().any(|e| matches!(e, CombatEvent::CritFailSideEffect { .. }));
        assert!(!has_crit_miss, "CombatLog must NOT contain CriticalMiss when d20≠1; got: {:?}", log.0);
        assert!(!has_crit_side, "CombatLog must NOT contain CritFailSideEffect when d20≠1; got: {:?}", log.0);
    }

    let _ = target;
}

/// Bridge translates `Event::CritFailed { outcome: Miss }` → `CombatEvent::CriticalMiss`.
///
/// DiceRng scripted to 1 (crit-fail). After update: CombatLog must contain
/// CriticalMiss for the caster; must NOT contain DamageResult.
#[test]
fn cast_crit_fail_miss_emits_critical_miss_log_entry() {
    run_crit_fail_log_test(1, true);
}

/// When d20 ≠ 1, CombatLog has NO CriticalMiss and NO CritFailSideEffect.
#[test]
fn cast_no_crit_fail_no_crit_fail_log_when_d20_non_one() {
    run_crit_fail_log_test(11, false);
}

// TODO(unisim phase2 step 7-followup or step 9): once EcsContentView populates
// crit_fail_outcome from race content (currently defaults to Miss), add bridge_cast
// tests for CritFailSideEffect variants (DoubleCost, SelfDamage, ApplyStatus).
// Engine cast.rs tests already pin the per-outcome logic on the engine side.

// ── Phase 3.5c: Cast(Summon) creates ECS entity via bridge ───────────────────

#[test]
fn cast_summon_creates_ecs_entity_synchronously() {
    use storyforge::content::abilities::TargetType;
    use storyforge::content::unit_templates::{EquipmentBlock, ResourcesBlock, UnitTemplateDef};
    use storyforge::game::components::{Combatant, SummonedBy};

    let summoner_pos = hex_from_offset(0, 0);

    let ability_id = AbilityId::from("summon_imp");
    let template_id = "imp";

    let ability_def = AbilityDef {
        id: ability_id.clone(),
        name: "Призвать беса".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template_id: template_id.into(),
                max_active: None,
            },
            costs: vec![],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![],
            key: None,
            requires_los: false,
            passive: None,
        },
    };

    let template = UnitTemplateDef {
        id: template_id.into(),
        name: "Imp".into(),
        race: String::new(),
        faction: None,
        path: None,
        speed: 4,
        stats: CombatStats { max_hp: 8, strength: 2, dexterity: 5, constitution: 8, intelligence: 0, wisdom: 5, charisma: 5 },
        equipment: EquipmentBlock {
            main_hand: "unarmed".into(),
            off_hand: None,
            chest: "".into(),
            legs: "".into(),
            feet: "".into(),
        },
        resources: ResourcesBlock::default(),
        ability_ids: vec![],
        ai_tuning_override: None,
        initial_statuses: vec![],
        initial_pools: std::collections::HashMap::new(),
    };

    let mut app = common::apps::bridge::bridge_app();
    {
        let mut content = app.world_mut().resource_mut::<ActiveContent>();
        content.0.abilities.insert(ability_id.clone(), ability_def);
        content.0.unit_templates.insert(template_id.into(), template);
    }

    let summoner = common::apps::bridge::spawn_unit(
        &mut app,
        Team::Enemy,
        common::apps::bridge::bridge_stats(),
        0,
        4,
        vec![ability_id.clone()],
        common::apps::bridge::no_equipment(),
        summoner_pos,
    );

    common::apps::bridge::bootstrap(&mut app);

    // Ensure summoner has AP.
    common::apps::bridge::with_engine_unit(&mut app, summoner, |unit| {
        unit.pools[combat_engine::PoolKind::Ap] = Some((1, 1));
    });

    // Script crit-fail d20 to non-1 (summon has no damage roll after that).
    common::apps::bridge::script_no_crit_fail(&mut app);

    // Cast summon targeting self (summoner == target for Myself abilities).
    common::apps::bridge::write_cast(&mut app, summoner, ability_id, summoner, summoner_pos);

    app.update();

    // Assert: a new Combatant entity exists (besides the summoner).
    let combatants: Vec<Entity> = app.world_mut()
        .query::<(Entity, &Combatant)>()
        .iter(app.world())
        .map(|(e, _)| e)
        .filter(|&e| e != summoner)
        .collect();
    assert_eq!(combatants.len(), 1, "expected exactly one summoned entity, got {:?}", combatants);
    let summoned = combatants[0];

    // Assert: registered in UnitIdMap.
    let id_map = app.world().resource::<UnitIdMap>();
    assert!(id_map.get_id(summoned).is_some(), "summoned entity must be in UnitIdMap");

    // Assert: has a position adjacent to summoner.
    let positions = app.world().resource::<HexPositions>();
    let pos = positions.get(&summoned).expect("summoned entity must have a HexPositions entry");
    assert_ne!(pos, summoner_pos, "summoned entity must not share summoner's hex");

    // Assert: SummonedBy component set.
    let summoned_by = app.world().entity(summoned).get::<SummonedBy>()
        .expect("summoned entity must have SummonedBy component");
    assert_eq!(summoned_by.0, summoner);

    // Assert: CombatLog has Summoned entry.
    let log = app.world().resource::<CombatLog>();
    let has_summoned = log.0.iter().any(|e| matches!(e, CombatEvent::Summoned { summoner: s, .. } if *s == summoner));
    assert!(has_summoned, "CombatLog must contain Summoned entry; got: {:?}", log.0);
}
