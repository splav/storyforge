//! Tests for `sim.rs` — split from the source file via `#[path]` in
//! `sim.rs` (see end of that file). Production code stays in `sim.rs`;
//! this file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../docs/testing.md):
//! `sim.rs` grew to 1676 LOC with tests dominating the lower half.
//!
//! `super::*` here resolves to `sim.rs` (since this file is included
//! as `mod tests` inside sim.rs).

use super::*;
use crate::combat::ai::world::snapshot::{ActiveStatusView, UnitSnapshot};
use crate::combat::ai::test_helpers::{empty_content, empty_status_tag_cache, snapshot_from, UnitBuilder};
use crate::content::abilities::{
    AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn, TargetType,
};
use combat_engine::{AbilityId, DiceExpr, ResourceKind, StatusId};
use crate::game::components::Team;
use crate::game::hex::hex_from_offset;

/// Sim-suite defaults: mana 5/10 (enough for simple casts), armor as
/// override. `hp` also explicit because armor+hp tests are the whole
/// point of this module.
fn unit(id: u32, team: Team, pos: Hex, hp: i32, armor: i32) -> UnitSnapshot {
    UnitBuilder::new(id, team, pos)
        .hp(hp)
        .armor(armor)
        .mana(5, 10)
        .build()
}

fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
    snapshot_from(units, 1)
}

fn ctx(str_mod: i32, int_mod: i32) -> CasterContext {
    CasterContext { str_mod, int_mod, spell_power: 0, weapon_dice: None }
}

fn ability(
    id: &str,
    effect: EffectDef,
    target_type: TargetType,
    range: u32,
) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            effect,
            target_type,
            range: AbilityRange { min: 0, max: range },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
        },
    }
}

// ── damage / armor / kill ───────────────────────────────────────────────

#[test]
fn damage_subtracts_armor_and_decrements_hp() {
    // Engine reads caster_ctx from the unit snapshot; set str_mod=4 there.
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20).armor(0).mana(5, 10)
        .caster_ctx(ctx(4, 0))
        .build();
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 2);
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    // 1d6 (EV 3.5 → rounded via `DiceSource` to 4) + str_mod(4) = 8 raw.
    // armor 2 → dealt 6.
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
    let step = PlanStep::Cast {
        ability: def.id.clone(),
        target: target_id,
        target_pos: hex_from_offset(1, 0),
    };
    let outcome = sim.apply_step(&step, &ctx(4, 0), &content, false);

    let t = sim.unit(target_id).unwrap();
    assert_eq!(t.hp, 14, "20 - 6 dealt = 14, got hp={}", t.hp);
    assert!((outcome.damage - 6.0).abs() < 0.01, "raw damage {}", outcome.damage);
    assert_eq!(outcome.hits, 1);
    assert!(outcome.killed.is_empty());
}

// Regression: heavy armor used to make sim predict 0 damage (`.max(0.0)`),
// but the live pipeline floors at `max(1)`. Both now agree on the floor —
// see `combat::effects_math::final_damage_f32`.
#[test]
fn damage_respects_min_one_floor_against_heavy_armor() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 10);
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    // 1d6 (EV 3.5) + str_mod(0) = 3.5 vs armor 10 → raw would underflow;
    // floor → 1.0.
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
    let step = PlanStep::Cast {
        ability: def.id.clone(),
        target: target_id,
        target_pos: hex_from_offset(1, 0),
    };
    let outcome = sim.apply_step(&step, &ctx(0, 0), &content, false);

    let t = sim.unit(target_id).unwrap();
    assert_eq!(t.hp, 19, "expected 1-damage floor to land, got hp={}", t.hp);
    assert!(
        (outcome.damage - 1.0).abs() < 0.01,
        "expected damage floor 1.0, got {}",
        outcome.damage,
    );
}

#[test]
fn lethal_damage_removes_unit_and_records_kill() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20).armor(0).mana(5, 10)
        .caster_ctx(ctx(4, 0))
        .build();
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 3, 0);
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    // 1d6 + str_mod(4) = 8 raw vs 3 hp → lethal.
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
    let step = PlanStep::Cast {
        ability: def.id.clone(),
        target: target_id,
        target_pos: hex_from_offset(1, 0),
    };
    let outcome = sim.apply_step(&step, &ctx(4, 0), &content, false);

    assert_eq!(outcome.killed, vec![target_id]);
    // Corpse stays in the snapshot with hp=0 (lift-prune: snapshot is the
    // single source of truth, including dead units). Downstream
    // `enemies_of` / `actor_unit` filter by `is_alive`, so plan-walking
    // code still sees the target as "gone" without a retain'd vec.
    let corpse = sim.unit(target_id).expect("corpse retained in snapshot");
    assert_eq!(corpse.hp, 0);
    assert!(!corpse.is_alive());
    assert_eq!(
        sim.enemies_of(Team::Enemy).count(), 0,
        "default enemies_of hides the corpse",
    );
}

// ── heal ───────────────────────────────────────────────────────────────

#[test]
fn heal_caps_at_missing_hp() {
    let actor = unit(1, Team::Player, hex_from_offset(0, 0), 20, 0);
    let ally = unit(2, Team::Player, hex_from_offset(1, 0), 15, 0);
    let actor_id = actor.entity;
    let ally_id = ally.entity;

    let mut content = empty_content();
    // Heal 3d6 (expected 10.5) but target is missing only 5.
    let def = ability(
        "cure",
        EffectDef::Heal { dice: DiceExpr::new(3, 6, 0) },
        TargetType::SingleAlly,
        2,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor, ally]), actor_id, empty_status_tag_cache());
    let step = PlanStep::Cast {
        ability: def.id.clone(),
        target: ally_id,
        target_pos: hex_from_offset(1, 0),
    };
    let outcome = sim.apply_step(&step, &ctx(0, 2), &content, false);

    let a = sim.unit(ally_id).unwrap();
    assert_eq!(a.hp, 20, "heal must clamp to max_hp");
    assert!((outcome.heal - 5.0).abs() < 0.01, "effective heal {}", outcome.heal);
}

// ── resource / AP / MP accounting ───────────────────────────────────────

#[test]
fn cast_decrements_ap_and_pays_mana() {
    let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    actor.action_points = 2;
    actor.max_ap = 2;
    let actor_id = actor.entity;
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
    let target_id = target.entity;

    let mut content = empty_content();
    let mut def = ability(
        "bolt",
        EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
        TargetType::SingleEnemy,
        3,
    );
    def.cost_ap = 1;
    def.costs = vec![crate::content::abilities::ResourceCost {
        resource: ResourceKind::Mana,
        amount: 3,
    }];
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
    sim.apply_step(
        &PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        },
        &ctx(0, 2),
        &content,
        false,
    );

    let a = sim.unit(actor_id).unwrap();
    assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 1, "AP drops from 2 to 1");
    assert_eq!(a.pools[combat_engine::PoolKind::Mana], Some((2, 10)), "mana 5 - 3 = 2");
}

#[test]
fn move_step_updates_pos_and_drains_mp() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    let actor_id = actor.entity;
    let target = hex_from_offset(2, 0);

    let content = empty_content();
    let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
    let outcome = sim.apply_step(
        &PlanStep::Move { path: vec![hex_from_offset(1, 0), target] },
        &ctx(0, 0),
        &content,
        false,
    );

    assert!(outcome.moved);
    let a = sim.unit(actor_id).unwrap();
    assert_eq!(a.pos, target);
    assert_eq!(a.pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 1, "speed 3 - path 2 = 1");
}

// ── stun status ─────────────────────────────────────────────────────────

#[test]
fn stun_status_is_recorded_in_outcome_and_tags() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();

    use crate::content::statuses::StatusDef;
    let stun_def = StatusDef {
        id: StatusId::from("stunned"),
        name: "Stunned".to_string(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses::default(),
            skips_turn: true,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            causes_disadvantage: false,
        },
    };
    content.statuses.insert(StatusId::from("stunned"), stun_def);

    let mut def = ability(
        "shock",
        EffectDef::None,
        TargetType::SingleEnemy,
        2,
    );
    def.statuses = vec![StatusApplication {
        status: StatusId::from("stunned"),
        duration_rounds: 1,
        on: StatusOn::Target,
    }];
    content.abilities.insert(def.id.clone(), def.clone());

    use crate::combat::ai::world::tags::cache::build_caches;
    let (status_tag_cache, _) = build_caches(&content);
    let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, &status_tag_cache);
    let outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(outcome.stunned, vec![target_id]);
    // After U4, snapshot.units is frozen; read via snapshot.unit() which
    // resolves through the live snapshot.state.
    let t = sim.unit(target_id).unwrap();
    assert!(t.is_stunned(&status_tag_cache));
}

// Regression: drift #2 — heal must neutralise target DoT before restoring
// HP, matching `apply_effects_system`. Previously sim added the full heal
// to HP ignoring poison ticks.
#[test]
fn heal_cleanses_dot_before_restoring_hp() {
    // Engine reads caster_ctx from the unit snapshot; set int_mod=2 there.
    let healer = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
        .hp(20).armor(0).mana(5, 10)
        .caster_ctx(ctx(0, 2))
        .build();
    let mut ally = unit(2, Team::Player, hex_from_offset(1, 0), 10, 0);
    ally.statuses.push(ActiveStatusView {
        id: StatusId::from("poison"),
        rounds_remaining: 2,
        dot_per_tick: 3,
    });
    let healer_id = healer.entity;
    let ally_id = ally.entity;

    let mut content = empty_content();
    use crate::content::statuses::StatusDef;
    content.statuses.insert(
        StatusId::from("poison"),
        StatusDef {
            id: StatusId::from("poison"),
            name: "Poison".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses::default(),
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                causes_disadvantage: false,
            },
        },
    );
    // Heal: 1d4 (EV 2.5 → 3) + int_mod(2) = 5 raw.
    let def = ability(
        "cure",
        EffectDef::Heal { dice: DiceExpr::new(1, 4, 0) },
        TargetType::SingleAlly,
        2,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    use crate::combat::ai::world::tags::cache::build_caches;
    let (status_tag_cache, _) = build_caches(&content);
    let mut sim = SimState::from_snapshot(&snap(vec![healer, ally]), healer_id, &status_tag_cache);
    let outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: def.id.clone(),
            target: ally_id,
            target_pos: hex_from_offset(1, 0),
        },
        &ctx(0, 2),
        &content,
        false,
    );

    let t = sim.unit(ally_id).unwrap();
    // Heal 5: cleanse spends 3 on poison (status removed), 2 remain → HP 10+2=12.
    assert_eq!(t.hp, 12, "cleanse consumes 3, then +2 HP → 12, got {}", t.hp);
    assert!(
        t.statuses.iter().all(|s| s.id.0 != "poison"),
        "poison should be cleansed"
    );
    assert!(
        (outcome.heal - 2.0).abs() < 0.01,
        "reported heal is net HP restored (2), got {}",
        outcome.heal,
    );
}

// Regression: drift #5 — status applied in one step must update the
// target's armor aggregate so the next step's damage math sees the bonus.
#[test]
fn status_applied_this_step_armor_affects_next_step() {
    // Attacker uses str_mod=4 in step 2; set it on the snapshot so the engine sees it.
    let attacker = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20).armor(0).mana(5, 10)
        .caster_ctx(ctx(4, 0))
        .build();
    let buffer = unit(2, Team::Enemy, hex_from_offset(1, 0), 20, 0);
    // Target with HP 20, base armor 0.
    let mut target = unit(3, Team::Player, hex_from_offset(2, 0), 20, 0);
    // Buffer will apply `stone_skin` to target, granting +5 armor_bonus.
    // Attacker then hits; with aggregate refresh, damage is reduced by 5.
    target.action_points = 0;
    let attacker_id = attacker.entity;
    let buffer_id = buffer.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    use crate::content::statuses::StatusDef;
    content.statuses.insert(
        StatusId::from("stone_skin"),
        StatusDef {
            id: StatusId::from("stone_skin"),
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
        },
    );

    // Cross-team buff: SingleEnemy on target so the status actually lands
    // mid-sim without violating team-filtering in `compute_affected_targets`.
    let mut buff_def = ability(
        "stone_skin_cast",
        EffectDef::None,
        TargetType::SingleEnemy,
        3,
    );
    buff_def.statuses = vec![StatusApplication {
        status: StatusId::from("stone_skin"),
        duration_rounds: 3,
        on: StatusOn::Target,
    }];
    content.abilities.insert(buff_def.id.clone(), buff_def.clone());

    // Damage: 1d6 (EV 4) + str_mod(4) = 8 raw.
    let atk_def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        3,
    );
    content.abilities.insert(atk_def.id.clone(), atk_def.clone());

    // Build a real status tag cache so refresh_aggregates picks up
    // stone_skin's armor_bonus=5 (the whole point of this test).
    use crate::combat::ai::world::tags::cache::build_caches;
    let (status_tag_cache, _) = build_caches(&content);

    // Step 1: buffer (active actor for this cast) puts stone_skin on target.
    let mut sim = SimState::from_snapshot(
        &snap(vec![attacker, buffer, target]),
        buffer_id,
        &status_tag_cache,
    );
    sim.apply_step(
        &PlanStep::Cast {
            ability: buff_def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(2, 0),
        },
        &ctx(0, 0),
        &content,
        false,
    );

    let t_mid = sim.unit(target_id).unwrap();
    assert_eq!(
        t_mid.armor_bonus, 5,
        "aggregate should refresh after status apply, got {}",
        t_mid.armor_bonus,
    );

    // Step 2: attacker strikes target. Swap active actor.
    sim.actor = attacker_id;
    let atk_outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: atk_def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(2, 0),
        },
        &ctx(4, 0),
        &content,
        false,
    );

    let t_after = sim.unit(target_id).unwrap();
    // raw 8 − armor_bonus 5 = 3 dealt. HP: 20 − 3 = 17.
    assert_eq!(t_after.hp, 17, "armor should reduce damage from 8 to 3, got hp={}", t_after.hp);
    assert!(
        (atk_outcome.damage - 3.0).abs() < 0.01,
        "reported damage after mitigation {}",
        atk_outcome.damage,
    );
}

// ── AoE ─────────────────────────────────────────────────────────────────

#[test]
fn aoe_circle_hits_all_enemies_in_radius() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    let t1 = unit(2, Team::Player, hex_from_offset(3, 0), 20, 0);
    let t2 = unit(3, Team::Player, hex_from_offset(4, 0), 20, 0);
    let actor_id = actor.entity;
    let t1_id = t1.entity;
    let t2_id = t2.entity;

    let mut content = empty_content();
    let mut def = ability(
        "blast",
        EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
        TargetType::SingleEnemy,
        5,
    );
    def.aoe = AoEShape::Circle { radius: 1 };
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, t1, t2]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_step(
        &PlanStep::Cast {
            ability: def.id.clone(),
            target: t1_id,
            target_pos: hex_from_offset(3, 0),
        },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(outcome.hits, 2, "radius-1 centered at (3,0) covers both (3,0) and (4,0)");
    assert!(sim.unit(t1_id).unwrap().hp < 20);
    assert!(sim.unit(t2_id).unwrap().hp < 20);
}

// ── GrantMovement ───────────────────────────────────────────────────────

// NOTE: GrantMovement is deferred to Phase 3 in the engine — `effect_for_target`
// returns None for this variant so no MP is added.  The engine still pays AP.
#[test]
fn grant_movement_pays_ap_engine_defers_mp() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
    let actor_id = actor.entity;

    let mut content = empty_content();
    let def = ability(
        "rush",
        EffectDef::GrantMovement { distance: 4 },
        TargetType::Myself,
        0,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
    sim.apply_step(
        &PlanStep::Cast {
            ability: def.id.clone(),
            target: actor_id,
            target_pos: hex_from_offset(0, 0),
        },
        &ctx(0, 0),
        &content,
        false,
    );

    let a = sim.unit(actor_id).unwrap();
    // Engine pays AP (cost_ap=1), but GrantMovement effect fanout is Phase 3 —
    // MP stays at the initial value (3) since no GrantMovement Effect is emitted.
    assert_eq!(a.pools[combat_engine::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0), 3, "engine defers GrantMovement to Phase 3; MP unchanged");
    assert_eq!(a.pools[combat_engine::PoolKind::Ap].map(|(c, _)| c).unwrap_or(0), 0, "AP cost still paid by engine");
}

// ── AoO propagation (step 12.2) ─────────────────────────────────────────
//
// Positions from `tests/aoo.rs` (verified adjacent/non-adjacent):
//   actor_pos  = hex_from_offset(3, 3)  — hero start
//   enemy_pos  = hex_from_offset(4, 3)  — goblin; distance 1 from actor_pos
//   away_pos   = hex_from_offset(2, 3)  — distance 2 from enemy (verified in aoo.rs)
//   near_pos   = hex_from_offset(3, 4)  — distance 1 from actor_pos AND enemy_pos

/// Moving out of adjacency with a reacting enemy records AoO self_damage
/// and applies it to actor hp.
#[test]
fn apply_move_records_aoo_self_damage() {
    // Actor at (3,3), enemy at (4,3) — adjacent (distance 1).
    // Move to (2,3) — distance 2 from enemy (leaves adjacency).
    // No armor → raw damage == dealt damage.
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(20)
        .armor(0)
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(5.0, 1)
        .build();
    let actor_id = actor.entity;

    // Pre-conditions (mirrors aoo.rs verified layout).
    let actor_pos = hex_from_offset(3, 3);
    let enemy_pos = hex_from_offset(4, 3);
    let away = hex_from_offset(2, 3);
    assert_eq!(actor_pos.unsigned_distance_to(enemy_pos), 1, "actor adj to enemy");
    assert_eq!(away.unsigned_distance_to(enemy_pos), 2, "away not adj to enemy");

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_move(&[away]);

    assert_eq!(outcome.self_damage, 5.0, "raw 5, no armor → self_damage 5");
    assert_eq!(sim.actor_unit().unwrap().hp, 15, "hp 20 − 5 AoO = 15");
}

/// After a provoked AoO, the triggering enemy's reactions_left is decremented.
#[test]
fn apply_move_decrements_enemy_reactions() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3)).aoo(5.0, 1).build();
    let enemy_id = enemy.entity;
    let actor_id = actor.entity;

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_move(&[hex_from_offset(2, 3)]);

    assert_eq!(
        sim.unit(enemy_id).unwrap().reactions_left,
        0,
        "enemy reaction consumed",
    );
}

/// Enemy with reactions_left = 0 does not trigger AoO even when adjacency is left.
#[test]
fn apply_move_no_aoo_when_already_used_reaction() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
    // reactions_left = 0 — reaction already spent this round.
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3)).aoo(5.0, 0).build();
    let actor_id = actor.entity;

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

    assert_eq!(outcome.self_damage, 0.0, "no reaction available → no AoO");
    assert_eq!(sim.actor_unit().unwrap().hp, 20, "hp unchanged");
}

/// A lethal AoO sets actor hp to 0; self_damage reports HP actually lost
/// (the HP delta, not raw dealt damage). With hp=1 and raw=10, HP delta = 1.
///
/// **Behaviour change from legacy sim (manifest):** the old `apply_move`
/// tracked `self_damage` as actual dealt damage post-armor (`final_damage_f32`),
/// which could exceed the actor's remaining HP (e.g., 10 dealt vs 1 HP).
/// The engine shim uses HP delta instead, which is the HP actually lost (1).
/// For safety scoring (`total_self_damage / actor_max_hp`) this is equivalent
/// in the lethal case: both produce a ratio that clamps to 1.0.
#[test]
fn apply_move_kills_actor_with_lethal_aoo() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(1)
        .armor(0)
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(10.0, 1)
        .build();
    let actor_id = actor.entity;

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

    // Engine path: self_damage = HP delta (1 hp lost, not 10 raw dealt).
    assert_eq!(outcome.self_damage, 1.0,
        "engine shim: self_damage is HP delta (1 hp lost), not raw dealt damage (10)");
    assert!(
        sim.actor_unit().is_none(),
        "actor hp=0 → is_alive()=false → actor_unit() returns None",
    );
    // hp clamped to 0, not negative.
    // After U4, snapshot.units is frozen at pre-step state; read via engine state.
    let dead = sim.unit(actor_id).expect("corpse retained in engine state");
    assert_eq!(dead.hp, 0, "hp clamped to 0");
}

/// Path that stays adjacent to the enemy does not trigger AoO.
#[test]
fn apply_move_no_aoo_when_path_stays_adjacent() {
    // Actor at (3,3), enemy at (4,3). Move to (3,4) which is adjacent to
    // both (verified: (3,4) is distance 1 from (3,3) per aoo.rs layout).
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(5.0, 1)
        .build();
    let actor_id = actor.entity;
    let dest = hex_from_offset(3, 4);

    // Pre-condition: dest must be adjacent to enemy (distance 1).
    assert_eq!(
        dest.unsigned_distance_to(hex_from_offset(4, 3)),
        1,
        "test precondition: (3,4) is adjacent to enemy at (4,3)",
    );

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_move(&[dest]);

    assert_eq!(outcome.self_damage, 0.0, "no adjacency-leave → no AoO");
    assert_eq!(sim.actor_unit().unwrap().hp, 20, "hp unchanged");
}

/// AoO fires at most once per enemy per step even if the path briefly
/// leaves and re-enters adjacency.
#[test]
fn apply_move_aoo_only_once_per_enemy_per_step() {
    // Actor at (3,3), enemy at (4,3).
    // Path: [(2,3), (3,4)] — first cell (2,3) is away (dist 2 from enemy),
    // second cell (3,4) is adjacent again (dist 1 from enemy).
    // AoO should trigger exactly once on the (3,3)→(2,3) transition.
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(20)
        .armor(0)
        .build();
    // reactions = 2 to prove the cap comes from scan logic, not reactions_left running out.
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(5.0, 2)
        .build();
    let enemy_id = enemy.entity;
    let actor_id = actor.entity;

    let enemy_pos = hex_from_offset(4, 3);
    // Verify: (2,3) is NOT adjacent to enemy; (3,4) IS adjacent to enemy.
    assert_eq!(hex_from_offset(2, 3).unsigned_distance_to(enemy_pos), 2, "(2,3) not adj");
    assert_eq!(hex_from_offset(3, 4).unsigned_distance_to(enemy_pos), 1, "(3,4) adj");

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    // Path: leave adjacency at step (3,3→2,3), then re-enter at (3,4).
    let outcome = sim.apply_move(&[hex_from_offset(2, 3), hex_from_offset(3, 4)]);

    assert_eq!(outcome.self_damage, 5.0, "exactly one AoO per step per enemy");
    assert_eq!(
        sim.unit(enemy_id).unwrap().reactions_left,
        1,
        "only one reaction consumed out of 2",
    );
}

/// AoO damage is mitigated by armor_bonus from status buffs (12.1 + 12.2 integration).
#[test]
fn apply_move_aoo_mitigated_by_armor_bonus() {
    // Actor armor=0, armor_bonus=5 (simulating a prior status apply).
    // Enemy AoO raw=8. Expected: final_damage_f32(8, 5, 0, false) = max(1, 8-5) = 3.
    // armor_bonus must be set before SimState::from_snapshot so that
    // both snapshot and combat_state see the same value.
    let mut actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(20)
        .armor(0)
        .build();
    actor.armor_bonus = 5;
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(8.0, 1)
        .build();
    let actor_id = actor.entity;

    let sim_snap = snap(vec![actor, enemy]);
    let mut sim = SimState::from_snapshot(&sim_snap, actor_id, empty_status_tag_cache());

    let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

    assert_eq!(outcome.self_damage, 3.0, "armor_bonus 5 reduces raw 8 AoO to 3");
    assert_eq!(sim.actor_unit().unwrap().hp, 17, "hp 20 − 3 = 17");
}

// ── Rage gain on damage (drift #3) ─────────────────────────────────────

/// Single-target hit: attacker has rage, defender does not.
/// Attacker rage increments by 1; defender rage stays None.
#[test]
fn apply_damage_grants_rage_to_attacker_per_hit() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .rage(5, 10)
        .build();
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .build(); // no rage
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((6, 10)), "attacker rage (5/10) → (6/10)");
    assert_eq!(sim.unit(target_id).unwrap().pools[combat_engine::PoolKind::Rage], None, "defender has no rage component");
}

/// Single-target hit: defender has rage, attacker does not.
/// Defender rage increments by 1; attacker rage stays None.
#[test]
fn apply_damage_grants_rage_to_defender_per_hit() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .build(); // no rage
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .rage(3, 10)
        .build();
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.unit(target_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((4, 10)), "defender rage (3/10) → (4/10)");
    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], None, "attacker has no rage component");
}

/// Single-target hit: both sides have rage. Each gains exactly +1.
#[test]
fn apply_damage_grants_rage_to_both_attacker_and_defender() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .rage(2, 10)
        .build();
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .rage(7, 10)
        .build();
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((3, 10)), "attacker (2/10) → (3/10)");
    assert_eq!(sim.unit(target_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((8, 10)), "defender (7/10) → (8/10)");
}

/// AoE hitting 3 enemies: attacker rage gets +1 per target hit (total +3).
/// Each defender gets +1.
#[test]
fn aoe_damage_grants_rage_per_target_to_attacker() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .rage(5, 10)
        .build();
    // Three enemies clustered at (3,0), (4,0), (3,1) — within radius 1 of (3,0).
    let t1 = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).rage(0, 10).build();
    let t2 = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 0)).rage(0, 10).build();
    let t3 = UnitBuilder::new(4, Team::Player, hex_from_offset(3, 1)).rage(0, 10).build();
    let actor_id = actor.entity;
    let t1_id = t1.entity;
    let t2_id = t2.entity;
    let t3_id = t3.entity;

    let mut content = empty_content();
    let mut def = ability(
        "blast",
        EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
        TargetType::SingleEnemy,
        5,
    );
    def.aoe = AoEShape::Circle { radius: 1 };
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, t1, t2, t3]),
        actor_id,
        empty_status_tag_cache(),
    );
    let outcome = sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: t1_id, target_pos: hex_from_offset(3, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(outcome.hits, 3, "AoE should hit all 3 enemies");
    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((8, 10)), "attacker (5/10) + 3 hits = (8/10)");
    assert_eq!(sim.unit(t1_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "t1 (0/10) → (1/10)");
    assert_eq!(sim.unit(t2_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "t2 (0/10) → (1/10)");
    assert_eq!(sim.unit(t3_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "t3 (0/10) → (1/10)");
}

/// Rage clamps at max: attacker at max rage stays there after a hit.
#[test]
fn rage_caps_at_max_for_attacker() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .rage(10, 10)
        .build();
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((10, 10)), "attacker rage capped at max 10");
}

/// Rage clamps at max: defender at max rage stays there after taking a hit.
#[test]
fn rage_caps_at_max_for_defender() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
        .rage(10, 10)
        .build();
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.unit(target_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((10, 10)), "defender rage capped at max 10");
}

/// Units with no rage component (rage: None) are silently unaffected.
/// No panic, rage stays None on both sides.
#[test]
fn units_without_rage_component_are_unaffected() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build(); // rage: None
    let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build(); // rage: None
    let actor_id = actor.entity;
    let target_id = target.entity;

    let mut content = empty_content();
    let def = ability(
        "strike",
        EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
        TargetType::SingleEnemy,
        1,
    );
    content.abilities.insert(def.id.clone(), def.clone());

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, target]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_step(
        &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
        &ctx(0, 0),
        &content,
        false,
    );

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], None, "attacker has no rage component, stays None");
    assert_eq!(sim.unit(target_id).unwrap().pools[combat_engine::PoolKind::Rage], None, "defender has no rage component, stays None");
}

// ── AoO rage (drift #3, AoO branch) ─────────────────────────────────────

/// Mirrors `combat/movement.rs:228-236` real-pipeline rule: for every AoO
/// hit, BOTH the AoO attacker AND the moving victim gain +1 rage.
#[test]
fn apply_move_aoo_grants_rage_to_both_sides() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(20)
        .rage(0, 10)
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(5.0, 1)
        .rage(0, 10)
        .build();
    let actor_id = actor.entity;
    let enemy_id = enemy.entity;

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_move(&[hex_from_offset(2, 3)]);

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((1, 10)), "victim +1 rage");
    assert_eq!(
        sim.unit(enemy_id).unwrap().pools[combat_engine::PoolKind::Rage],
        Some((1, 10)),
        "AoO attacker +1 rage",
    );
}

/// AoO rage gain clamps to max on both sides.
#[test]
fn apply_move_aoo_rage_caps_at_max() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(20)
        .rage(10, 10)
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .aoo(5.0, 1)
        .rage(10, 10)
        .build();
    let actor_id = actor.entity;
    let enemy_id = enemy.entity;

    let mut sim = SimState::from_snapshot(
        &snap(vec![actor, enemy]),
        actor_id,
        empty_status_tag_cache(),
    );
    sim.apply_move(&[hex_from_offset(2, 3)]);

    assert_eq!(sim.actor_unit().unwrap().pools[combat_engine::PoolKind::Rage], Some((10, 10)));
    assert_eq!(sim.unit(enemy_id).unwrap().pools[combat_engine::PoolKind::Rage], Some((10, 10)));
}

// TODO(12.3): `self_damage_grants_two_rage_for_self_aoe` — actor is both
// source and defender in friendly-fire AoE. The real pipeline iterates
// `for actor in [source, target]` so the same unit's `rage.gain()` is
// called twice → total +2. Setting up a single-unit self-AoE scenario
// requires a friendly_fire=true AoE ability that targets the caster — the
// existing `ability()` helper only supports SingleEnemy target type, and
// `TargetType::Myself` with AoE is not exercised by current content.
// The structural correctness is verified by inspection: in
// `apply_primary`, defender rage is bumped inside the `unit_mut(ent)` borrow,
// then `actor_unit_mut()` (same entity) bumps it again — producing +2.
