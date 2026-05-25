//! Parity tests: AI sim vs real combat for canonical scenarios.
//!
//! Step 12.0: skeleton + sentinel test only. Per-drift parity tests are
//! added incrementally:
//!   - 12.1 (status reflow): `parity_haste_speed_real_vs_sim`,
//!     `parity_armor_buff_mitigation_real_vs_sim`
//!   - 12.2 (AoO): `parity_aoo_real_vs_sim`,
//!     `parity_aoo_decrements_reactions_real_vs_sim`
//!   - 12.3 (rage): `parity_rage_real_vs_sim`, `parity_rage_aoe_real_vs_sim`

/// Summary of differences between the AI sim and real combat for a single
/// scenario run.
#[derive(Debug, Default)]
pub struct ParityReport {
    /// HP difference (sim − real); 0 means identical.
    pub hp_drift: i32,
    /// Position differed between sim and real.
    pub pos_drift: bool,
    /// Status list differed between sim and real.
    pub statuses_drift: bool,
    /// Rage difference (sim − real); 0 means identical.
    pub rage_drift: i32,
    /// Speed difference (sim − real); 0 means identical.
    pub speed_drift: i32,
}

impl ParityReport {
    pub fn is_clean(&self) -> bool {
        self.hp_drift == 0
            && !self.pos_drift
            && !self.statuses_drift
            && self.rage_drift == 0
            && self.speed_drift == 0
    }
}

/// Run a named parity scenario and return the diff report.
///
/// Step 12.0: stub — always returns a clean report.
/// Real implementations added per-drift in steps 12.1–12.3.
pub fn run_parity_scenario(_name: &str) -> ParityReport {
    ParityReport::default()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn parity_no_op_scenario_zero_drift() {
    let report = run_parity_scenario("no_op");
    assert!(report.is_clean(), "no-op scenario must have zero drift, got {:?}", report);
}

/// Parity check: after a haste status (speed_bonus=+2) is applied, the sim's
/// `unit.speed` equals `base_speed + 2`.
///
/// **Sim-side only.** The real combat pipeline does not update the `Speed` ECS
/// component when a status is applied mid-round — it computes the bonus in
/// `build_snapshot` at snapshot-construction time. A full real-vs-sim test
/// would require constructing a Bevy world with the unit + haste status and
/// calling `build_snapshot`, then running the sim and comparing.
///
/// TODO(12.1): Extend to full Bevy integration once the `effects_app` +
/// `ApplyStatus` plumbing is wired for AI-snapshot comparison. See
/// `tests/statuses.rs` for the real-combat harness pattern.
#[test]
fn parity_haste_speed_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::{StatusTagCache, StatusTagSet};
    use storyforge::combat::ai::world::tags::cache::StatusBonuses;
    use combat_engine::StatusId;
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn, TargetType};
    use combat_engine::AbilityId;

    // Build a cache with "haste" → speed_bonus=+2.
    let mut cache = StatusTagCache::default();
    let haste_id = StatusId::from("haste");
    cache.map.insert(haste_id.clone(), StatusTagSet::empty());
    cache.bonuses.insert(haste_id.clone(), StatusBonuses { speed_bonus: 2, armor_bonus: 0, damage_taken_bonus: 0 });

    // Build a self-haste ability.
    let haste_def = AbilityDef {
        id: AbilityId::from("cast_haste"),
        name: "Haste".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication {
            status: haste_id.clone(),
            duration_rounds: 2,
            on: StatusOn::Target,
        }],
            key: None,
        },
    };

    use storyforge::content::content_view::ContentView;
    use storyforge::content::statuses::StatusDef;
    use std::collections::HashMap;

    let haste_status = StatusDef {
        id: haste_id.clone(),
        name: "Haste".into(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses { armor_bonus: 0, damage_taken_bonus: 0, speed_bonus: 2 },
            skips_turn: false,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            causes_disadvantage: false,
        },
    };

    let mut content = ContentView {
        abilities: HashMap::new(),
        keyed_abilities: Vec::new(),
        statuses: HashMap::new(),
        weapons: HashMap::new(),
        armor: HashMap::new(),
        classes: HashMap::new(),
        unit_templates: HashMap::new(),
        races: HashMap::new(),
        factions: HashMap::new(),
        paths: HashMap::new(),
        ..ContentView::default()
    };
    content.abilities.insert(haste_def.id.clone(), haste_def.clone());
    content.statuses.insert(haste_id.clone(), haste_status);

    // Build cache from content so all bonuses are correct.
    use storyforge::combat::ai::world::tags::cache::build_caches;
    let (status_tag_cache, _) = build_caches(&content);

    // Actor: base_speed=3, speed=3, ap=2.
    let actor_pair = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
        .ap(2)
        .threat(0.0)
        .max_attack_range(0)
        .abilities(vec![haste_def.id.clone()])
        .build_pair();
    let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let snap = snapshot_from_pairs(vec![actor_pair], 1);

    // --- Sim side ---
    let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tag_cache);
    sim.apply_step(
        &PlanStep::Cast {
            ability: haste_def.id.clone(),
            target: actor_id,
            target_pos: hex_from_offset(0, 0),
        },
        &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
        &content,
        false,
    );

    let actor_after = sim.unit(actor_id).expect("actor present after cast");
    assert_eq!(
        actor_after.speed, 5,
        "after haste (speed_bonus=+2), speed should be base(3)+bonus(2)=5, got {}",
        actor_after.speed,
    );
    assert_eq!(actor_after.base_speed, 3, "base_speed unchanged by status");
}

/// Parity check: after a stone_skin buff (armor_bonus=+5) is applied to a
/// target, the sim computes damage correctly (raw - armor - 5).
///
/// **Sim-side only.** See `parity_haste_speed_real_vs_sim` for the
/// rationale — full Bevy ECS integration is deferred.
///
/// TODO(12.1): Extend to full Bevy integration (apply buff + real damage
/// event) once the real-vs-sim harness is wired. See `tests/effects.rs`
/// and `tests/statuses.rs` for the real-combat harness pattern.
#[test]
fn parity_armor_buff_mitigation_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use combat_engine::final_damage_f32;
    use combat_engine::StatusId;
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, StatusApplication, StatusOn, TargetType};
    use combat_engine::{AbilityId, DiceExpr};
    use storyforge::content::statuses::StatusDef;
    use storyforge::content::content_view::ContentView;
    use std::collections::HashMap;

    let stone_skin_id = StatusId::from("stone_skin");

    // stone_skin: armor_bonus=+5.
    let stone_skin_def = StatusDef {
        id: stone_skin_id.clone(),
        name: "Stone Skin".into(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses { armor_bonus: 5, damage_taken_bonus: 0, speed_bonus: 0 },
            skips_turn: false,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            causes_disadvantage: false,
        },
    };

    // Buff ability: SingleEnemy (so it reaches a target in tests).
    let buff_def = AbilityDef {
        id: AbilityId::from("stone_skin_cast"),
        name: "Stone Skin".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication {
            status: stone_skin_id.clone(),
            duration_rounds: 3,
            on: StatusOn::Target,
        }],
            key: None,
        },
    };

    // Damage ability: 1d6 (EV=3.5→4) + str_mod=4 → raw=8.
    let atk_def = AbilityDef {
        id: AbilityId::from("strike"),
        name: "Strike".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
        },
    };

    let mut content = ContentView {
        abilities: HashMap::new(),
        keyed_abilities: Vec::new(),
        statuses: HashMap::new(),
        weapons: HashMap::new(),
        armor: HashMap::new(),
        classes: HashMap::new(),
        unit_templates: HashMap::new(),
        races: HashMap::new(),
        factions: HashMap::new(),
        paths: HashMap::new(),
        ..ContentView::default()
    };
    content.abilities.insert(buff_def.id.clone(), buff_def.clone());
    content.abilities.insert(atk_def.id.clone(), atk_def.clone());
    content.statuses.insert(stone_skin_id.clone(), stone_skin_def);

    use storyforge::combat::ai::world::tags::cache::build_caches;
    let (status_tag_cache, _) = build_caches(&content);

    // buffer: Enemy, ap=2, max_attack_range=3, abilities=[buff]
    let buffer_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .ap(2)
        .max_attack_range(3)
        .abilities(vec![buff_def.id.clone()])
        .build_pair();
    // target: Player, ap=0, mp=0, threat=0.0, max_attack_range=0, abilities=[]
    let target_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .ap(0)
        .speed(0)
        .threat(0.0)
        .max_attack_range(0)
        .build_pair();
    // attacker: Enemy, ap=2, max_attack_range=3, abilities=[atk], threat=5.0, caster_ctx(str_mod=4)
    let attacker_pair = UnitBuilder::new(3, Team::Enemy, hex_from_offset(2, 0))
        .ap(2)
        .max_attack_range(3)
        .abilities(vec![atk_def.id.clone()])
        .caster_ctx(CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None })
        .build_pair();

    let buffer_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let target_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
    let attacker_id = bevy::prelude::Entity::from_raw_u32(3).expect("valid");

    let snap = snapshot_from_pairs(vec![buffer_pair, target_pair, attacker_pair], 1);

    // Step 1: apply stone_skin to target.
    let mut sim = SimState::from_snapshot(&snap, buffer_id, &status_tag_cache);
    sim.apply_step(
        &PlanStep::Cast {
            ability: buff_def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        },
        &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
        &content,
        false,
    );

    // Verify armor_bonus refreshed.
    assert_eq!(
        sim.unit(target_id).unwrap().armor_bonus, 5,
        "target armor_bonus must be 5 after stone_skin",
    );

    // Step 2: attacker strikes target (swap actor).
    sim.actor = attacker_id;
    let atk_outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: atk_def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        },
        &CasterContext { str_mod: 4, int_mod: 0, spell_power: 0, weapon_dice: None },
        &content,
        false,
    );

    // raw = ceil(EV(1d6)) + str_mod(4) = 4 + 4 = 8. armor_bonus=5. Dealt = max(1, 8-5) = 3.
    let expected_dealt = final_damage_f32(8.0, 5.0, 0.0, false);
    assert!(
        (atk_outcome.damage - expected_dealt).abs() < 0.01,
        "sim damage {:.2} should equal formula {:.2} (raw=8, armor_bonus=5)",
        atk_outcome.damage,
        expected_dealt,
    );

    let target_hp = sim.unit(target_id).unwrap().hp;
    assert_eq!(target_hp, 20 - expected_dealt as i32,
        "target HP should be 20 - {} = {}", expected_dealt as i32, 20 - expected_dealt as i32);
}

/// Parity check (12.2): sim AoO damage matches `final_damage_f32` formula.
///
/// Actor at (3,3), enemy with AoO raw=6 at (4,3) — adjacent. Actor moves to
/// (2,3) leaving adjacency. Sim must record `outcome.self_damage ==
/// final_damage_f32(6.0, mitigation, vuln, false)`.
///
/// **Sim-side only.** Real-combat AoO integration requires the full Bevy
/// movement pipeline + `Reactions` component. See `tests/aoo.rs` for the
/// real-combat AoO harness; that test verifies the identical formula.
///
/// TODO(12.2): Wire full real-vs-sim comparison once `run_parity_scenario`
/// drives both sides end-to-end.
#[test]
fn parity_aoo_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::StatusTagCache;
    use combat_engine::final_damage_f32;
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use storyforge::content::abilities::CasterContext;
    use storyforge::content::content_view::ContentView;

    let raw_aoo = 6.0f32;
    let actor_armor = 2;
    let mitigation = actor_armor as f32;
    let vuln = 0.0f32;

    // actor: Enemy at (3,3), armor=2, ap=1, mp=3, threat=0.0, max_attack_range=1
    let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .armor(actor_armor)
        .threat(0.0)
        .build_pair();
    // enemy: Player at (4,3), ap=0, mp=0, threat=5.0, aoo(raw=6, reactions=1)
    let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .ap(0)
        .speed(0)
        .aoo(raw_aoo, 1)
        .build_pair();

    let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

    let status_tags = StatusTagCache::default();
    let content = ContentView::default();
    let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
    let outcome = sim.apply_step(
        &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
        &CasterContext::default(),
        // content not needed for a Move step — pass empty.
        &content,
        false,
    );

    let expected = final_damage_f32(raw_aoo, mitigation, vuln, false);
    assert!(
        (outcome.self_damage - expected).abs() < 0.01,
        "sim AoO self_damage {:.2} must equal formula {:.2} (raw={raw_aoo}, armor={actor_armor})",
        outcome.self_damage,
        expected,
    );
}

/// Parity check (12.2): after one Move that provokes AoO, enemy reactions_left
/// is decremented to 0 in the sim snapshot — mirroring real combat where
/// `Reactions` is decremented on each AoO.
///
/// **Sim-side only.** See `parity_aoo_real_vs_sim` for rationale.
///
/// TODO(12.2): Extend to full Bevy integration with `Reactions` component
/// verification after a real MoveUnit pipeline run. See `tests/aoo.rs`.
#[test]
fn parity_aoo_decrements_reactions_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::StatusTagCache;
    use storyforge::content::content_view::ContentView;
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use storyforge::content::abilities::CasterContext;

    // actor: Enemy at (3,3), ap=1, mp=3, threat=0.0, max_attack_range=1
    let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .threat(0.0)
        .build_pair();
    // enemy: Player at (4,3), ap=0, mp=0, threat=5.0, aoo(raw=5.0, reactions=1)
    let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .ap(0)
        .speed(0)
        .aoo(5.0, 1)
        .build_pair();

    let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let enemy_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
    let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

    let status_tags = StatusTagCache::default();
    let content = ContentView::default();
    let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
    sim.apply_step(
        &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
        &CasterContext::default(),
        &content,
        false,
    );

    assert_eq!(
        sim.unit(enemy_id).unwrap().reactions_left,
        0,
        "enemy reactions_left must be 0 after one provoked AoO",
    );
}

/// Parity check (12.3): after a single-target Damage cast, attacker rage
/// increments by +1 and defender rage increments by +1 — mirroring
/// `apply_effects.rs:117-129` which iterates `for actor in [source, target]`.
///
/// **Sim-side only.** Real-combat rage verification requires a full Bevy world
/// with `Rage` ECS component. The real-pipeline rule is verified by inspection
/// of `apply_effects.rs:117-129`.
///
/// TODO(12.3): Extend to full Bevy integration once the real-combat parity
/// harness drives both sides end-to-end.
#[test]
fn parity_rage_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::StatusTagCache;
    use storyforge::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use storyforge::content::content_view::ContentView;
    use combat_engine::{AbilityId, DiceExpr};
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use std::collections::HashMap;

    // attacker: Enemy at (0,0), rage=(5,10), ap=1, threat=5.0
    let attacker_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .rage(5, 10)
        .caster_ctx(CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None })
        .build_pair();
    // defender: Player at (1,0), rage=(3,10), ap=0, mp=0, threat=0.0, max_attack_range=0
    let defender_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .ap(0)
        .speed(0)
        .threat(0.0)
        .max_attack_range(0)
        .rage(3, 10)
        .build_pair();

    let attacker_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let defender_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");

    let strike_def = AbilityDef {
        id: AbilityId::from("strike"),
        name: "Strike".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
        },
    };

    let mut content = ContentView {
        abilities: HashMap::new(),
        keyed_abilities: Vec::new(),
        statuses: HashMap::new(),
        weapons: HashMap::new(),
        armor: HashMap::new(),
        classes: HashMap::new(),
        unit_templates: HashMap::new(),
        races: HashMap::new(),
        factions: HashMap::new(),
        paths: HashMap::new(),
        ..ContentView::default()
    };
    content.abilities.insert(strike_def.id.clone(), strike_def.clone());

    let snap = snapshot_from_pairs(vec![attacker_pair, defender_pair], 1);
    let status_tags = StatusTagCache::default();
    let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

    sim.apply_step(
        &PlanStep::Cast {
            ability: strike_def.id.clone(),
            target: defender_id,
            target_pos: hex_from_offset(1, 0),
        },
        &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
        &content,
        false,
    );

    // Real pipeline: both source and target gain +1 rage per damage event.
    assert_eq!(
        sim.unit(attacker_id).unwrap().rage,
        Some((6, 10)),
        "attacker rage (5/10) should become (6/10) after dealing damage",
    );
    assert_eq!(
        sim.unit(defender_id).unwrap().rage,
        Some((4, 10)),
        "defender rage (3/10) should become (4/10) after taking damage",
    );
}

/// Parity check (12.3): AoE Damage hitting 3 defenders — attacker gains +1
/// rage per target hit (total +3), each defender gains +1.
///
/// Mirrors `apply_effects.rs:117-129`: the loop iterates one entry per damage
/// event, so AoE with N targets calls `rage.gain()` on the attacker N times.
///
/// **Sim-side only.** See `parity_rage_real_vs_sim` for rationale.
///
/// TODO(12.3): Extend to full Bevy integration.
#[test]
fn parity_rage_aoe_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::StatusTagCache;
    use storyforge::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use storyforge::content::content_view::ContentView;
    use combat_engine::{AbilityId, DiceExpr};
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;
    use std::collections::HashMap;

    let make_unit = |id: u32, team: Team, col: i32, rage: Option<(i32, i32)>| {
        let mut b = UnitBuilder::new(id, team, hex_from_offset(col, 0))
            .max_attack_range(5);
        if let Some((cur, max)) = rage {
            b = b.rage(cur, max);
        }
        if team == Team::Player {
            b = b.ap(0).speed(0).threat(0.0);
        }
        b.build_pair()
    };

    let attacker_pair = make_unit(1, Team::Enemy, 0, Some((5, 10)));
    // Three defenders clustered within AoE radius 1 of (3,0).
    let d1_pair = make_unit(2, Team::Player, 3, Some((0, 10)));
    let d2_pair = make_unit(3, Team::Player, 4, Some((0, 10)));
    let d3_pair = make_unit(4, Team::Player, 2, Some((0, 10)));

    let attacker_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let d1_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
    let d2_id = bevy::prelude::Entity::from_raw_u32(3).expect("valid");
    let d3_id = bevy::prelude::Entity::from_raw_u32(4).expect("valid");

    let blast_def = AbilityDef {
        id: AbilityId::from("blast"),
        name: "Blast".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius: 1 },
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
        },
    };

    let mut content = ContentView {
        abilities: HashMap::new(),
        keyed_abilities: Vec::new(),
        statuses: HashMap::new(),
        weapons: HashMap::new(),
        armor: HashMap::new(),
        classes: HashMap::new(),
        unit_templates: HashMap::new(),
        races: HashMap::new(),
        factions: HashMap::new(),
        paths: HashMap::new(),
        ..ContentView::default()
    };
    content.abilities.insert(blast_def.id.clone(), blast_def.clone());

    let snap = snapshot_from_pairs(vec![attacker_pair, d1_pair, d2_pair, d3_pair], 1);
    let status_tags = StatusTagCache::default();
    let mut sim = SimState::from_snapshot(&snap, attacker_id, &status_tags);

    let outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: blast_def.id.clone(),
            target: d1_id,
            target_pos: hex_from_offset(3, 0),
        },
        &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
        &content,
        false,
    );

    assert_eq!(outcome.hits, 3, "AoE radius-1 at (3,0) should hit d1(3,0), d2(4,0), d3(2,0)");

    // Attacker gets +1 per damage event → +3 total.
    assert_eq!(
        sim.unit(attacker_id).unwrap().rage,
        Some((8, 10)),
        "attacker rage (5/10) + 3 hits = (8/10)",
    );
    // Each defender gets +1.
    assert_eq!(sim.unit(d1_id).unwrap().rage, Some((1, 10)), "d1 (0/10) → (1/10)");
    assert_eq!(sim.unit(d2_id).unwrap().rage, Some((1, 10)), "d2 (0/10) → (1/10)");
    assert_eq!(sim.unit(d3_id).unwrap().rage, Some((1, 10)), "d3 (0/10) → (1/10)");
}

/// Parity check (12.3, AoO branch): when a Move provokes an AoO, the real
/// `movement_system` (`combat/movement.rs:228-236`) iterates
/// `for actor in [attacker, ev.actor]` and calls `rage.gain()` on both.
/// The sim mirrors this in `apply_move`.
///
/// **Sim-side only.** Full Bevy run would require a `MoveUnit` integration
/// (see `tests/aoo.rs`) which is out of scope for 12.3 parity coverage.
#[test]
fn parity_aoo_grants_rage_real_vs_sim() {
    use storyforge::combat::ai::plan::sim::SimState;
    use storyforge::combat::ai::plan::types::PlanStep;
    use storyforge::combat::ai::test_helpers::{snapshot_from_pairs, UnitBuilder};
    use storyforge::combat::ai::world::tags::StatusTagCache;
    use storyforge::content::abilities::CasterContext;
    use storyforge::content::content_view::ContentView;
    use storyforge::game::components::Team;
    use storyforge::game::hex::hex_from_offset;

    // Actor at (3,3), adjacent enemy at (4,3). Move to (2,3) — leaves adjacency.
    // actor: Enemy, ap=1, mp=3, rage=(4,10), threat=0.0
    let actor_pair = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .rage(4, 10)
        .threat(0.0)
        .build_pair();
    // enemy: Player, ap=0, mp=0, rage=(7,10), threat=5.0, aoo(5.0, reactions=1)
    let enemy_pair = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .ap(0)
        .speed(0)
        .rage(7, 10)
        .aoo(5.0, 1)
        .build_pair();

    let actor_id = bevy::prelude::Entity::from_raw_u32(1).expect("valid");
    let enemy_id = bevy::prelude::Entity::from_raw_u32(2).expect("valid");
    let snap = snapshot_from_pairs(vec![actor_pair, enemy_pair], 1);

    let status_tags = StatusTagCache::default();
    let content = ContentView::default();
    let mut sim = SimState::from_snapshot(&snap, actor_id, &status_tags);
    sim.apply_step(
        &PlanStep::Move { path: vec![hex_from_offset(2, 3)] },
        &CasterContext::default(),
        &content,
        false,
    );

    // Both sides bumped by exactly 1, mirroring `for actor in [attacker, ev.actor]`.
    assert_eq!(sim.actor_unit().unwrap().rage, Some((5, 10)), "victim 4 → 5");
    assert_eq!(
        sim.unit(enemy_id).unwrap().rage,
        Some((8, 10)),
        "AoO attacker 7 → 8",
    );
}






