//! Tests for `generator.rs` — split from the source file via `#[path]` in
//! `generator.rs` (see end of that file). Production code stays in
//! `generator.rs`; this file holds the test module body.
//!
//! Split per [docs/testing.md §2](../../../../docs/testing.md):
//! `generator.rs` grew to 2089 LOC with tests dominating the lower half.
//!
//! `super::*` here resolves to `generator.rs` (since this file is included
//! as `mod tests` inside generator.rs).

use super::*;
use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::test_helpers::snapshot_from;
use crate::combat::ai::test_helpers::UnitFixture;
use crate::combat::ai::test_helpers::{
    empty_content, empty_maps, empty_status_tag_cache, ent, UnitBuilder,
};
use crate::content::abilities::{
    AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
};
use crate::game::components::{Abilities, Team};
use crate::game::hex::hex_from_offset;
use combat_engine::{AbilityId, DiceExpr};

/// Generator-suite defaults: caller sets `hp` + `max_ap` (beam search
/// branching tests rely on these to tune pool shape). Ability list is a
/// test-wide superset of every id referenced across tests in this
/// module, so each test actor "knows" whatever a specific test wires
/// through `ctx.actor.abilities` without per-test ability setup.
/// Tests that specifically exercise unknown-ability rejection use
/// `UnitBuilder::ability_names(&[])` directly.
fn unit(id: u32, team: Team, pos: Hex, hp: i32, max_ap: i32) -> UnitFixture {
    UnitBuilder::new(id, team, pos)
        .hp(hp)
        .ap(max_ap)
        .ability_names(&[
            "strike",
            "melee_attack",
            "heal",
            "stun_bolt",
            "aoe_stun",
            "fireball",
            "mana_bolt",
            "melee",
        ])
        .build()
}

fn strike_def(id: &str, range: u32, cost_ap: i32) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::Damage {
                dice: DiceExpr::new(1, 6, 0),
            },
            costs: Vec::new(),
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    }
}

use crate::combat::ai::test_helpers::make_test_ctx as make_ctx;

// ── Depth-1 generation ──────────────────────────────────────────────────

#[test]
fn depth_1_plan_set_includes_empty_and_single_casts() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;

    let mut content = empty_content();
    let def = strike_def("strike", 1, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let _caster = CasterContext {
        str_mod: 4,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, target], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // At least one empty plan (seed) + one single-cast plan.
    assert!(
        plans.iter().any(|p| p.steps.is_empty()),
        "seed plan must exist"
    );
    assert!(
        plans
            .iter()
            .any(|p| p.steps.len() == 1 && matches!(&p.steps[0], PlanStep::Cast { .. })),
        "at least one single-step cast plan expected"
    );
    // Invariant: annotation.outcomes.len() == steps.len() for every plan.
    for plan in &plans {
        assert_eq!(
            plan.annotation.outcomes.len(),
            plan.steps.len(),
            "annotation.outcomes length must match steps length"
        );
    }
}

// ── Flee regime: offensive casts dropped at generation ─────────────────

/// Wave-2 §9: a unit in the Flee regime must NOT generate offensive Cast
/// candidates. Dropping them at generation (not just penalising the intent
/// score) is required — the offensive damage step-factor is scored
/// independently of the intent column and would otherwise let an attack
/// win on raw damage even under Flee.
#[test]
fn flee_regime_excludes_offensive_cast_candidates() {
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);

    let mut content = empty_content();
    let off = strike_def("strike", 1, 1);
    content.abilities.insert(off.id.clone(), off);

    let difficulty = DifficultyProfile::hard();
    let ctx = make_ctx(&content, &difficulty);
    let maps = empty_maps();

    // Control: a non-fleeing unit DOES generate the offensive cast.
    let plain = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20)
        .ap(1)
        .ability_names(&["strike"])
        .build();
    let plain_id = plain.entity;
    let snap_plain = snapshot_from(vec![plain, target.clone()], 1);
    let plans_plain = generate_plans(plain_id, &ctx, &snap_plain, &maps);
    assert!(
        plans_plain
            .iter()
            .any(|p| p.steps.iter().any(|s| matches!(s, PlanStep::Cast { .. }))),
        "control: a non-fleeing unit must generate the offensive cast",
    );

    // Fleeing: the same setup must yield NO Cast steps at all.
    let fleeing = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20)
        .ap(1)
        .ability_names(&["strike"])
        .forced_mode(Some(crate::combat::ai::adapt::EvaluationMode::Flee))
        .build();
    let fleeing_id = fleeing.entity;
    let snap_flee = snapshot_from(vec![fleeing, target], 1);
    let plans_flee = generate_plans(fleeing_id, &ctx, &snap_flee, &maps);
    assert!(
        plans_flee
            .iter()
            .all(|p| p.steps.iter().all(|s| !matches!(s, PlanStep::Cast { .. }))),
        "fleeing unit must not generate any offensive Cast candidate",
    );
}

// ── Annotation outcomes match sim outcomes ─────────────────────────────

#[test]
fn annotation_enemy_damage_populated_for_cast_steps() {
    // Step 4.12: generator fills `annotation.enemy_damage` via from_sim_step.
    // The value is the sim-rolled damage (not the formula expected value) —
    // so we check it is positive and within the ability's dice range,
    // not bit-identical to calc.expected().
    use crate::content::abilities::CasterContext;

    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;

    let mut content = empty_content();
    let def = strike_def("strike", 1, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    let caster_ctx = CasterContext::default();
    // Max possible rolled damage for 1d1 (strike_def uses sides=1) = 1.
    let calc = def.effect.calc(&caster_ctx).expect("strike has calc");
    let max_raw = calc.expected(); // deterministic for 1d1

    // Every Cast plan: annotation.enemy_damage must be populated (>= 0)
    // and within plausible range.
    let mut found_cast = false;
    for plan in plans.iter().filter(|p| !p.steps.is_empty()) {
        for (i, ann) in plan.annotation.outcomes.iter().enumerate() {
            if !matches!(plan.steps.get(i), Some(PlanStep::Cast { .. })) {
                continue;
            }
            found_cast = true;
            // sim-derived enemy_damage может включать str_mod / armor / damage_taken_bonus —
            // не сравниваем bit-identical с calc.expected() (raw dice). Проверяем range.
            assert!(
                    ann.enemy_damage >= max_raw && ann.enemy_damage <= max_raw + 10.0,
                    "plan step {i}: annotation.enemy_damage ({}) should be in plausible range around raw expected ({})",
                    ann.enemy_damage, max_raw
                );
        }
    }
    assert!(
        found_cast,
        "should have found at least one Cast step in plans"
    );
}

// ── Beam pruning respects width ────────────────────────────────────────

#[test]
fn beam_pruning_limits_per_depth_frontier() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 3);
    let mut units = vec![actor];
    // 6 targets so a naive generator would emit ≥ 6 cast candidates at
    // depth 1.
    for i in 0..6u32 {
        units.push(unit(
            10 + i,
            Team::Player,
            hex_from_offset(1 + i as i32, 0),
            20,
            1,
        ));
    }
    let actor_id = units[0].entity;

    let mut content = empty_content();
    let def = strike_def("strike", 10, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 2;
    difficulty.plan_beam_width = 2;
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(units, 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // Count plans by depth. Beam=2 ⇒ depth-1 frontier size ≤ 2, depth-2 ≤ 2.
    let at_depth_1 = plans.iter().filter(|p| p.steps.len() == 1).count();
    let at_depth_2 = plans.iter().filter(|p| p.steps.len() == 2).count();
    assert!(
        at_depth_1 <= 2,
        "beam=2 should cap depth-1 frontier; got {}",
        at_depth_1
    );
    assert!(
        at_depth_2 <= 2,
        "beam=2 should cap depth-2 frontier; got {}",
        at_depth_2
    );
}

// ── Sim state carries into next depth: killed targets are gone ────────

#[test]
fn killed_target_absent_in_second_step_enumeration() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 2);
    let weak = unit(2, Team::Player, hex_from_offset(1, 0), 1, 1); // 1 HP, dies to any hit
    let other = unit(3, Team::Player, hex_from_offset(2, 0), 20, 1);
    let actor_id = actor.entity;
    let weak_id = weak.entity;

    let mut content = empty_content();
    let def = strike_def("strike", 10, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 2;
    difficulty.plan_beam_width = 8;
    let _caster = CasterContext {
        str_mod: 4,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, weak, other], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // Find depth-2 plans that target the weak unit first. In step 2 they
    // must not cast at weak again (it's dead post step 1).
    for p in plans.iter().filter(|p| p.steps.len() == 2) {
        let (PlanStep::Cast { target: t1, .. }, PlanStep::Cast { target: t2, .. }) =
            (&p.steps[0], &p.steps[1])
        else {
            continue;
        };
        if *t1 == weak_id {
            assert_ne!(
                *t2, weak_id,
                "step 2 must not target a unit killed in step 1"
            );
        }
    }
}

// ── AP exhaustion gates extension ──────────────────────────────────────

#[test]
fn ap_exhaustion_stops_cast_extension() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;

    let mut content = empty_content();
    let def = strike_def("strike", 1, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 3;
    difficulty.plan_beam_width = 8;
    let _caster = CasterContext {
        str_mod: 4,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, target], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // With max_ap=1, no plan should have more than one Cast step.
    for p in &plans {
        let casts = p
            .steps
            .iter()
            .filter(|s| matches!(s, PlanStep::Cast { .. }))
            .count();
        assert!(
            casts <= 1,
            "plan has {} casts but actor has 1 AP: {:?}",
            casts,
            p.steps
        );
    }
}

// ── Logical-key dedup: identical (ability, target, cast_tile) collapse ─

#[test]
fn dedup_collapses_same_ability_target_cast_tile() {
    let actor_start = hex_from_offset(0, 0);
    let target = ent(42);
    let cast_tile = hex_from_offset(2, 0);
    let target_pos = hex_from_offset(3, 0);
    let cost_ap = 1;

    // Three plans, all end at cast_tile and cast the same ability on the
    // same target — via three different move paths. Logically equivalent.
    let mk_plan = |path: Vec<Hex>| TurnPlan {
        steps: vec![
            PlanStep::Move { path },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target,
                target_pos,
            },
        ],
        final_pos: cast_tile,
        residual_ap: 0,
        residual_mp: 0,
        outcomes: vec![],
        partial_score: 1.0,
        sim_snapshots: Vec::new(),
        annotation: Default::default(),
    };
    let _ = cost_ap;

    let plans = vec![
        mk_plan(vec![hex_from_offset(1, 0), cast_tile]),
        mk_plan(vec![
            hex_from_offset(1, 0),
            hex_from_offset(1, 1),
            cast_tile,
        ]),
        mk_plan(vec![
            hex_from_offset(0, 1),
            hex_from_offset(1, 1),
            hex_from_offset(2, 1),
            cast_tile,
        ]),
    ];

    let deduped = super::dedup_by_logical_key(plans, actor_start);
    assert_eq!(
        deduped.len(),
        1,
        "three path-variants of same Cast should collapse to one",
    );
    // And the surviving one is the shortest path (2-step).
    if let PlanStep::Move { path } = &deduped[0].steps[0] {
        assert_eq!(path.len(), 2, "should keep the shortest-path variant");
    } else {
        panic!("expected Move as first step");
    }
}

#[test]
fn dedup_keeps_distinct_targets() {
    let actor_start = hex_from_offset(0, 0);
    let t1 = ent(10);
    let t2 = ent(11);
    let cast_tile = hex_from_offset(2, 0);
    let mk = |target: Entity, target_pos: Hex| TurnPlan {
        steps: vec![
            PlanStep::Move {
                path: vec![hex_from_offset(1, 0), cast_tile],
            },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target,
                target_pos,
            },
        ],
        final_pos: cast_tile,
        residual_ap: 0,
        residual_mp: 0,
        outcomes: vec![],
        partial_score: 1.0,
        sim_snapshots: Vec::new(),
        annotation: Default::default(),
    };
    let plans = vec![mk(t1, hex_from_offset(3, 0)), mk(t2, hex_from_offset(3, 1))];
    let deduped = super::dedup_by_logical_key(plans, actor_start);
    assert_eq!(deduped.len(), 2, "distinct targets must not collapse");
}

// ── ai_policy_ok: AI heuristic layer (overheal, wasted CC, AoE FF ratio) ───
//
// Game-rule cases (taunt, team-safety, blocks_mana_abilities, range)
// are covered at the `check_legality` layer (actions/mod.rs + arch
// D.a) and end-to-end via `generate_plans_*` tests below.

use crate::content::abilities::{StatusApplication, StatusOn};
use crate::content::statuses::StatusDef;
use combat_engine::StatusId;

fn heal_def(id: &str, range: u32) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::Heal {
                dice: DiceExpr::new(1, 6, 0),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    }
}

fn stun_def(id: &str, range: u32, aoe: AoEShape) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::None,
            costs: Vec::new(),
            cost_ap: 1,
            aoe,
            friendly_fire: false,
            statuses: vec![StatusApplication {
                status: StatusId::from("stun"),
                duration_rounds: 1,
                on: StatusOn::Target,
            }],
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    }
}

fn fireball_def(id: &str, range: u32, radius: u32) -> AbilityDef {
    AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::SpellDamage {
                dice: DiceExpr::new(1, 6, 0),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius },
            friendly_fire: true,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    }
}

fn stun_status() -> StatusDef {
    StatusDef {
        id: StatusId::from("stun"),
        name: "stun".into(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses::default(),
            skips_turn: true,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            heal_per_tick: 0,
            causes_disadvantage: false,
        },
    }
}

// Rule 1: Overheal

#[test]
fn overheal_rejects_target_above_90_percent() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    // max_hp=20, hp=19 → 95%
    let mut fine = unit(2, Team::Enemy, hex_from_offset(0, 1), 19, 1);
    fine.max_hp = 20;
    // max_hp=20, hp=10 → 50%
    let mut hurt = unit(3, Team::Enemy, hex_from_offset(0, 2), 10, 1);
    hurt.max_hp = 20;

    let heal = heal_def("heal", 3);
    let mut content = empty_content();
    content.abilities.insert(heal.id.clone(), heal.clone());
    let difficulty = DifficultyProfile::hard();
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![heal.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor.clone(), fine.clone(), hurt.clone()], 1);
    let sim = SimState::from_snapshot(&snap, actor.entity, empty_status_tag_cache());
    let actor_view = snap.unit(actor.entity).unwrap();

    assert!(
        !ai_policy_ok(&heal, actor_view, fine.entity, fine.pos, &sim, &ctx),
        "heal on near-full ally must be rejected",
    );
    assert!(
        ai_policy_ok(&heal, actor_view, hurt.entity, hurt.pos, &sim, &ctx),
        "heal on wounded ally must be allowed",
    );
}

// Rule 3: Wasted CC

#[test]
fn wasted_single_target_cc_on_stunned_rejected() {
    use crate::combat::ai::test_helpers::status_view;
    use crate::combat::ai::world::tags::cache::build_caches;

    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let mut stunned = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    stunned.statuses.push(status_view("stun", 1, 0));
    let awake = unit(3, Team::Player, hex_from_offset(0, 1), 20, 1);

    let def = stun_def("stun_bolt", 5, AoEShape::None);
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    content
        .statuses
        .insert(StatusId::from("stun"), stun_status());

    let difficulty = DifficultyProfile::hard();
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);

    let (status_tag_cache, ability_tag_cache) = build_caches(&content);
    let ctx = AiWorld {
        content: &content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
        ability_tags: &ability_tag_cache,
        status_tags: &status_tag_cache,
    };

    let snap = snapshot_from(vec![actor.clone(), stunned.clone(), awake.clone()], 1);
    let sim = SimState::from_snapshot(&snap, actor.entity, &status_tag_cache);
    let actor_view = snap.unit(actor.entity).unwrap();

    assert!(
        !ai_policy_ok(&def, actor_view, stunned.entity, stunned.pos, &sim, &ctx),
        "single-target CC on already-stunned target must be rejected",
    );
    assert!(
        ai_policy_ok(&def, actor_view, awake.entity, awake.pos, &sim, &ctx),
        "CC on un-stunned target must be allowed",
    );
}

#[test]
fn aoe_cc_on_stunned_target_still_allowed() {
    // AoE CC keeps the candidate: dropping the whole blast because one
    // enemy in it is stunned is wrong — others in the area still benefit.
    use crate::combat::ai::test_helpers::status_view;
    use crate::combat::ai::world::tags::cache::build_caches;

    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let mut stunned = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    stunned.statuses.push(status_view("stun", 1, 0));

    let def = stun_def("aoe_stun", 5, AoEShape::Circle { radius: 1 });
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    content
        .statuses
        .insert(StatusId::from("stun"), stun_status());

    let difficulty = DifficultyProfile::hard();
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);

    let (status_tag_cache, ability_tag_cache) = build_caches(&content);
    let ctx = AiWorld {
        content: &content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
        ability_tags: &ability_tag_cache,
        status_tags: &status_tag_cache,
    };

    let snap = snapshot_from(vec![actor.clone(), stunned.clone()], 1);
    let sim = SimState::from_snapshot(&snap, actor.entity, &status_tag_cache);
    let actor_view = snap.unit(actor.entity).unwrap();

    assert!(
        ai_policy_ok(&def, actor_view, stunned.entity, stunned.pos, &sim, &ctx),
        "AoE CC must not be rejected just because the primary target is stunned",
    );
}

// Rule 4: AoE friendly-fire

#[test]
fn aoe_friendly_fire_rejected_when_hits_ally_without_enough_enemies() {
    // Fireball radius=1 centered on (1,0). Hits both (1,0) and (0,0).
    // Place an ally at (0,0) (actor itself) — allies_hit=1, enemies_hit=1
    // → need enemies_hit >= 2*allies_hit = 2 → reject.
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);

    let def = fireball_def("fireball", 5, 1);
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    let difficulty = DifficultyProfile::hard();
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 4,
        spell_power: 2,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor.clone(), enemy.clone()], 1);
    let sim = SimState::from_snapshot(&snap, actor.entity, empty_status_tag_cache());
    let actor_view = snap.unit(actor.entity).unwrap();

    assert!(
        !ai_policy_ok(&def, actor_view, enemy.entity, enemy.pos, &sim, &ctx),
        "friendly-fire AoE that hits self without 2x enemy value must be rejected",
    );
}

#[test]
fn aoe_friendly_fire_accepted_when_enemies_outnumber_allies_two_to_one() {
    // Centre far from actor so self isn't hit. Two enemies in the blast,
    // one ally: enemies_hit=2, allies_hit=1 → 2 >= 2 → accept.
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let e1 = unit(2, Team::Player, hex_from_offset(4, 0), 20, 1);
    let e2 = unit(3, Team::Player, hex_from_offset(5, 0), 20, 1);
    let ally = unit(4, Team::Enemy, hex_from_offset(4, 1), 20, 1);

    let def = fireball_def("fireball", 10, 1);
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    let difficulty = DifficultyProfile::hard();
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 4,
        spell_power: 2,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor.clone(), e1.clone(), e2.clone(), ally.clone()], 1);
    let sim = SimState::from_snapshot(&snap, actor.entity, empty_status_tag_cache());
    let actor_view = snap.unit(actor.entity).unwrap();

    assert!(
        ai_policy_ok(&def, actor_view, e1.entity, e1.pos, &sim, &ctx),
        "AoE must be accepted when enemies_hit >= 2*allies_hit",
    );
}

// End-to-end: confirm `generate_plans` wires the legality + policy
// filters, not just that they work in isolation.

#[test]
fn generate_plans_excludes_taunt_violating_casts() {
    use crate::combat::ai::test_helpers::status_view;
    use crate::combat::ai::world::tags::cache::build_caches;

    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let mut taunter = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
    taunter.statuses.push(status_view("taunt", 1, 0));
    let adjacent_non_taunter = unit(3, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;
    let taunter_id = taunter.entity;

    let def = strike_def("strike", 5, 1);
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());

    // Add a taunt status with forces_targeting=true so the cache classifies
    // it as Compulsion and the legality check enforces targeting.
    let taunt_status = StatusDef {
        id: StatusId::from("taunt"),
        name: "Taunt".to_string(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses::default(),
            skips_turn: false,
            forces_targeting: true,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            heal_per_tick: 0,
            causes_disadvantage: false,
        },
    };
    content
        .statuses
        .insert(StatusId::from("taunt"), taunt_status);

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let _caster = CasterContext {
        str_mod: 4,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);

    let (status_tag_cache, ability_tag_cache) = build_caches(&content);
    let ctx = AiWorld {
        content: &content,
        difficulty: &difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
        ability_tags: &ability_tag_cache,
        status_tags: &status_tag_cache,
    };

    let snap = snapshot_from(vec![actor, taunter, adjacent_non_taunter], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // No plan in the pool may contain a Cast at anyone other than the taunter.
    for p in &plans {
        for step in &p.steps {
            if let PlanStep::Cast { target, .. } = step {
                assert_eq!(
                    *target, taunter_id,
                    "plan pool leaked a non-taunter Cast: {:?}",
                    p.steps,
                );
            }
        }
    }
}

/// F6: AI plan generator must not propose a Cast through a static obstacle
/// when the ability has `requires_los = true`. Previously verified only at
/// `check_legality` unit level; this exercises the full pipeline.
///
/// Two-phase: a negative control (no obstacle → Cast IS proposed) sandwich
/// confirms the planner *is* willing to cast at this target absent LOS
/// blockage, ensuring the positive assertion isn't a false-positive caused
/// by some other gate excluding the cast unconditionally.
#[test]
fn generate_plans_excludes_los_blocked_cast() {
    use crate::combat::ai::world::tags::cache::build_caches;

    // Use "strike" (already in unit() builder's known-ability list) and
    // reconfigure it as a ranged LOS-required attack via the content map.
    let make_setup = || {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
        let enemy = unit(2, Team::Player, hex_from_offset(4, 0), 20, 1);
        let actor_id = actor.entity;
        let enemy_id = enemy.entity;

        let mut def = strike_def("strike", 5, 1);
        def.engine.requires_los = true;
        let mut content = empty_content();
        content.abilities.insert(def.id.clone(), def.clone());

        let mut difficulty = DifficultyProfile::hard();
        difficulty.plan_max_depth = 1;

        (actor, enemy, actor_id, enemy_id, content, difficulty)
    };

    let snap_has_cast_at = |snap: &crate::combat::ai::world::snapshot::BattleSnapshot,
                            content: &crate::content::content_view::ActiveContentData,
                            difficulty: &DifficultyProfile,
                            actor_id,
                            target_id| {
        let (status_tag_cache, ability_tag_cache) = build_caches(content);
        let ctx = AiWorld {
            content,
            difficulty,
            tuning: &content.ai_tuning,
            crit_fail_chance: 0.0,
            ability_tags: &ability_tag_cache,
            status_tags: &status_tag_cache,
        };
        let plans = generate_plans(actor_id, &ctx, snap, &empty_maps());
        plans.iter().any(|p| {
            p.steps
                .iter()
                .any(|s| matches!(s, PlanStep::Cast { target, .. } if *target == target_id))
        })
    };

    // ── Phase 1 — negative control: WITHOUT obstacle, planner must
    //    propose at least one Cast at the enemy. Otherwise the positive
    //    assertion below would be vacuous.
    {
        let (actor, enemy, actor_id, enemy_id, content, difficulty) = make_setup();
        let snap = snapshot_from(vec![actor, enemy], 1);
        assert!(
            snap_has_cast_at(&snap, &content, &difficulty, actor_id, enemy_id),
            "control: planner must propose at least one Cast at the enemy when LOS is clear",
        );
    }

    // ── Phase 2 — positive assertion: WITH obstacle on the line,
    //    planner must NOT propose a Cast at the obstructed enemy.
    {
        let (actor, enemy, actor_id, enemy_id, content, difficulty) = make_setup();
        let mut snap = snapshot_from(vec![actor, enemy], 1);
        snap.state.blocked_hexes.insert(hex_from_offset(2, 0));
        assert!(
            !snap_has_cast_at(&snap, &content, &difficulty, actor_id, enemy_id),
            "planner leaked a Cast through obstacle when requires_los=true",
        );
    }
}

/// Regression: AI must respect `blocks_mana_abilities` at planning time.
/// Pre-arch-D the planner only checked `can_afford` (AP + resource amount),
/// missing the status flag — so a unit under `broken_faith` would plan
/// mana-cost casts, lose the round to validation's reject, and `EndTurn`.
/// Now `check_legality` gates every Cast candidate and filters them out.
#[test]
fn generate_plans_excludes_mana_casts_under_blocks_mana_status() {
    use crate::combat::ai::test_helpers::status_view;
    use combat_engine::ResourceKind;

    // Actor has broken_faith + enough mana + both a mana spell and a
    // no-cost melee fallback.
    let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 2);
    actor.mana = Some((10, 10));
    actor.statuses.push(status_view("broken_faith", 3, 0));
    let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;

    let mut mana_bolt = strike_def("mana_bolt", 5, 1);
    mana_bolt.costs = vec![crate::content::abilities::ResourceCost {
        resource: ResourceKind::Mana,
        amount: 5,
    }];
    let melee = strike_def("melee", 1, 1);

    let mut content = empty_content();
    content
        .abilities
        .insert(mana_bolt.id.clone(), mana_bolt.clone());
    content.abilities.insert(melee.id.clone(), melee.clone());
    content.statuses.insert(
        StatusId::from("broken_faith"),
        StatusDef {
            id: StatusId::from("broken_faith"),
            name: "broken_faith".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses::default(),
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: true,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
            },
        },
    );

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let _caster = CasterContext {
        str_mod: 0,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![mana_bolt.id.clone(), melee.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // No plan may use the mana spell.
    let mana_id = mana_bolt.id.clone();
    for p in &plans {
        for step in &p.steps {
            if let PlanStep::Cast { ability, .. } = step {
                assert_ne!(
                    *ability, mana_id,
                    "broken_faith must filter mana casts out of the plan pool",
                );
            }
        }
    }

    // Sanity: plans with the melee fallback are still there — AI
    // doesn't starve.
    let melee_id = melee.id.clone();
    let has_melee = plans.iter().any(|p| {
        p.steps
            .iter()
            .any(|s| matches!(s, PlanStep::Cast { ability, .. } if *ability == melee_id))
    });
    assert!(has_melee, "non-mana fallback cast must still be available");
}

/// Ground-targeted abilities: generator must enumerate candidate
/// landing cells (one per in-range enemy), emitting
/// `(actor_entity, enemy.pos)` pairs — target entity is the actor
/// sentinel, target_pos is where the AoE lands. Regression guard for
/// the phase-1 empty-candidates stub: without this, AI can never cast
/// fireball / thunderstrike post-Ground-conversion.
#[test]
fn ground_generator_emits_enemy_centered_cells() {
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let actor_id = actor.entity;
    let enemy_a = unit(2, Team::Player, hex_from_offset(3, 0), 20, 1);
    let enemy_b = unit(3, Team::Player, hex_from_offset(0, 3), 20, 1);
    let enemy_a_pos = enemy_a.pos;
    let enemy_b_pos = enemy_b.pos;

    let fireball = AbilityDef {
        id: AbilityId::from("fireball"),
        name: "fireball".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Ground,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage {
                dice: DiceExpr::new(2, 3, 0),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius: 1 },
            friendly_fire: true,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    };

    let mut content = empty_content();
    content
        .abilities
        .insert(fireball.id.clone(), fireball.clone());
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let ctx = make_ctx(&content, &difficulty);
    let snap = snapshot_from(vec![actor, enemy_a, enemy_b], 1);
    let maps = empty_maps();

    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    let fireball_id = fireball.id.clone();
    let landed_cells: HashSet<Hex> = plans
        .iter()
        .flat_map(|p| p.steps.iter())
        .filter_map(|s| match s {
            PlanStep::Cast {
                ability,
                target,
                target_pos,
            } if *ability == fireball_id => {
                // Sentinel check: Ground uses actor as target entity.
                assert_eq!(
                    *target, actor_id,
                    "Ground Cast target must be actor sentinel"
                );
                Some(*target_pos)
            }
            _ => None,
        })
        .collect();

    assert!(
        landed_cells.contains(&enemy_a_pos),
        "enemy A's cell must be a landing candidate (landed: {landed_cells:?})",
    );
    assert!(
        landed_cells.contains(&enemy_b_pos),
        "enemy B's cell must be a landing candidate (landed: {landed_cells:?})",
    );
}

/// Regression for arch-debt-A: when the top-K-by-rank enemies are all
/// illegal (out-of-range / taunt-blocked), the planner must still
/// surface a legal lower-ranked target. Pre-fix, `rank_targets` picked
/// top-K first then `check_legality` dropped them all → 0 candidates
/// even though a legal target existed in the pool.
///
/// Setup: 3 high-threat enemies (top-K candidates) all out of strike
/// range, plus 1 low-threat enemy in range. Expectation: planner
/// generates a Cast at the in-range enemy.
#[test]
fn rank_targets_picks_legal_when_top_k_by_rank_all_illegal() {
    // Strike range = 1, melee. High-threat enemies parked out of reach.
    let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let actor_id = actor.entity;
    let mut far1 = unit(2, Team::Player, hex_from_offset(8, 0), 20, 1);
    far1.threat = 100.0;
    let mut far2 = unit(3, Team::Player, hex_from_offset(7, 1), 20, 1);
    far2.threat = 90.0;
    let mut far3 = unit(4, Team::Player, hex_from_offset(8, 2), 20, 1);
    far3.threat = 80.0;
    // The only legal target — adjacent, low threat.
    let mut close = unit(5, Team::Player, hex_from_offset(1, 0), 20, 1);
    close.threat = 1.0;
    let close_id = close.entity;

    let def = strike_def("strike", 1, 1);
    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let _caster = CasterContext {
        str_mod: 4,
        int_mod: 0,
        spell_power: 0,
        weapon_dice: None,
        dex_mod: 0,
        ranged_dice: None,
    };
    let _abilities = Abilities(vec![def.id.clone()]);
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, far1, far2, far3, close], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // The legal close enemy must surface as a Cast in some plan.
    let strike_id = def.id.clone();
    let has_close_cast = plans.iter().any(|p| {
        p.steps.iter().any(|s| {
            matches!(s, PlanStep::Cast { ability, target, .. }
                         if *ability == strike_id && *target == close_id)
        })
    });
    assert!(
        has_close_cast,
        "rank_targets must dig past illegal top-K to find the legal close target",
    );
}

/// Disadvantage (from `causes_disadvantage` status) must discount the
/// damage estimate on every Cast step in generated plans. Baseline
/// (no status) vs dis-status run of the same setup: dis damage should
/// be strictly less. Closes arch-audit divergence A2 — AI was
/// over-estimating disoriented unit's damage.
#[test]
fn disadvantage_status_discounts_plan_damage_estimate() {
    use crate::combat::ai::test_helpers::status_view;
    use crate::content::statuses::StatusDef;
    use combat_engine::StatusId;

    let base_actor = || unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 1);
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = base_actor().entity;

    let def = AbilityDef {
        id: AbilityId::from("strike"),
        name: "strike".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage {
                dice: DiceExpr::new(2, 6, 0),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    };

    let mut content = empty_content();
    content.abilities.insert(def.id.clone(), def.clone());
    content.statuses.insert(
        StatusId::from("disoriented"),
        StatusDef {
            id: StatusId::from("disoriented"),
            name: "disoriented".into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses::default(),
                skips_turn: false,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: true,
            },
        },
    );

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let ctx = make_ctx(&content, &difficulty);
    let maps = empty_maps();

    // Baseline: no status.
    let snap_base = snapshot_from(vec![base_actor(), target.clone()], 1);
    let plans_base = generate_plans(actor_id, &ctx, &snap_base, &maps);
    let dmg_base: f32 = cast_damage_sum(&plans_base);

    // Under disadvantage status.
    let mut dis_actor = base_actor();
    dis_actor.statuses.push(status_view("disoriented", 3, 0));
    let snap_dis = snapshot_from(vec![dis_actor, target], 1);
    let plans_dis = generate_plans(actor_id, &ctx, &snap_dis, &maps);
    let dmg_dis: f32 = cast_damage_sum(&plans_dis);

    assert!(
        dmg_base > 0.0 && dmg_dis > 0.0,
        "both runs must generate at least one Cast plan (base={dmg_base}, dis={dmg_dis})",
    );
    assert!(
        dmg_dis < dmg_base,
        "disadvantage must discount damage: base={dmg_base}, dis={dmg_dis}",
    );
}

/// Summon cap must prune Cast candidates when live summons already fill
/// the slot. Regression guard for the bug where `SummonedBy` survived
/// death → AI planned a cast that would be blocked by spawn at runtime,
/// wasting AP.
#[test]
fn generate_plans_excludes_summon_when_cap_reached() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20)
        .ap(2)
        .ability_names(&["summon_spirit"])
        .build();
    let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
    let s1 = UnitBuilder::new(3, Team::Enemy, hex_from_offset(1, 0))
        .hp(10)
        .summoner(actor.entity)
        .ability_names(&[])
        .build();
    let s2 = UnitBuilder::new(4, Team::Enemy, hex_from_offset(0, 1))
        .hp(10)
        .summoner(actor.entity)
        .ability_names(&[])
        .build();
    let actor_id = actor.entity;

    let summon_def = AbilityDef {
        id: AbilityId::from("summon_spirit"),
        name: "summon_spirit".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template_id: "spirit".into(),
                max_active: Some(2),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    };

    let mut content = empty_content();
    content
        .abilities
        .insert(summon_def.id.clone(), summon_def.clone());
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 2;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy, s1, s2], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    for p in &plans {
        for step in &p.steps {
            if let PlanStep::Cast { ability, .. } = step {
                assert_ne!(
                    *ability, summon_def.id,
                    "cap-reached summon must be pruned from plan pool",
                );
            }
        }
    }
}

/// Dead summons must NOT occupy a cap slot: with cap=2, one live + one
/// dead summon leaves room for one more. Mirrors the spawn-side fix.
#[test]
fn generate_plans_allows_summon_when_only_dead_summons_fill_slots() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20)
        .ap(2)
        .ability_names(&["summon_spirit"])
        .build();
    let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
    let alive = UnitBuilder::new(3, Team::Enemy, hex_from_offset(1, 0))
        .hp(10)
        .summoner(actor.entity)
        .ability_names(&[])
        .build();
    // hp=0 ⇒ !is_alive(), should be excluded from the cap count.
    let dead = UnitBuilder::new(4, Team::Enemy, hex_from_offset(0, 1))
        .hp(0)
        .summoner(actor.entity)
        .ability_names(&[])
        .build();
    let actor_id = actor.entity;

    let summon_def = AbilityDef {
        id: AbilityId::from("summon_spirit"),
        name: "summon_spirit".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template_id: "spirit".into(),
                max_active: Some(2),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    };

    let mut content = empty_content();
    content
        .abilities
        .insert(summon_def.id.clone(), summon_def.clone());
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy, alive, dead], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    let has_summon = plans.iter().any(|p| {
        p.steps.iter().any(|s| {
            matches!(
                s, PlanStep::Cast { ability, .. } if *ability == summon_def.id
            )
        })
    });
    assert!(
        has_summon,
        "dead summons must not occupy cap slots — summon must still be planned",
    );
}

/// Multi-step plans must also respect cap: with cap=1 and 0 live summons,
/// at most ONE summon cast per plan — sim.apply_step doesn't materialise
/// the summon, so the second step must be pruned by the plan-level count.
#[test]
fn generate_plans_caps_multiple_summons_within_single_plan() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
        .hp(20)
        .ap(3)
        .ability_names(&["summon_spirit"])
        .build();
    let enemy = unit(2, Team::Player, hex_from_offset(5, 0), 20, 1);
    let actor_id = actor.entity;

    let summon_def = AbilityDef {
        id: AbilityId::from("summon_spirit"),
        name: "summon_spirit".into(),
        magic_domains: Vec::new(),
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: combat_engine::AbilityDef {
            target_type: TargetType::Myself,
            range: AbilityRange { min: 0, max: 0 },
            effect: EffectDef::Summon {
                template_id: "spirit".into(),
                max_active: Some(1),
            },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            key: None,
            requires_los: false,
            passive: vec![],
            requires_tags: Default::default(),
            excludes_tags: Default::default(),
        },
    };

    let mut content = empty_content();
    content
        .abilities
        .insert(summon_def.id.clone(), summon_def.clone());
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 3;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    for p in &plans {
        let summons = p
            .steps
            .iter()
            .filter(|s| {
                matches!(
                    s, PlanStep::Cast { ability, .. } if *ability == summon_def.id
                )
            })
            .count();
        assert!(
            summons <= 1,
            "plan stacked {summons} summon casts with cap=1: {:?}",
            p.steps,
        );
    }
}

/// Helper: total cast damage across every Cast step in every plan.
fn cast_damage_sum(plans: &[TurnPlan]) -> f32 {
    plans
        .iter()
        .flat_map(|p| p.outcomes.iter().zip(p.steps.iter()))
        .filter(|(_, s)| matches!(s, PlanStep::Cast { .. }))
        .map(|(o, _)| o.damage)
        .sum()
}

// ── AoO truncation (step 12.2) ──────────────────────────────────────────

/// Actor with hp=1 adjacent to a taunter with lethal AoO: no plan in the
/// pool should extend beyond the first Move step that kills the actor.
///
/// Layout (even-r): actor at (3,3), enemy-with-AoO at (4,3). Actor has
/// 1 AP and mp=3, so Move→Cast sequences are possible. After a Move that
/// triggers the lethal AoO, the branch must stop — no [Move, Cast] plans.
#[test]
fn enumerate_terminates_when_actor_dies_mid_plan() {
    // Actor: hp=1, 1 AP, mp=3 (can move and would otherwise cast).
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(1)
        .ap(1)
        .speed(3)
        .ability_names(&["strike"])
        .build();
    // Enemy with lethal AoO (raw=10 >> hp=1, no armor) and 1 reaction.
    // Placed at (4,3) — adjacent to actor start. Give it 0 AP so it
    // doesn't generate actions for the generator (it's a simple target).
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .hp(20)
        .ap(0)
        .aoo(10.0, 1)
        .build();
    let actor_id = actor.entity;

    let mut content = empty_content();
    let def = strike_def("strike", 1, 1);
    content.abilities.insert(def.id.clone(), def);

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 3;
    difficulty.plan_beam_width = 20; // wide beam to see all branches
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // Any plan whose first step is a Move-out-of-adjacency that deals
    // lethal AoO (self_damage >= 1) must have length == 1 (no extension).
    for p in &plans {
        if p.steps.is_empty() {
            continue;
        }
        if matches!(p.steps[0], PlanStep::Move { .. }) && p.outcomes[0].self_damage >= 1.0 {
            assert_eq!(
                p.steps.len(),
                1,
                "lethal-AoO Move branch must not be extended; got steps={:?}",
                p.steps,
            );
        }
    }
}

/// The lethal-move plan itself IS in the pool (we record it, just don't
/// extend it) so downstream critics can score it and correctly reject it.
#[test]
fn single_step_lethal_move_still_recorded() {
    let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
        .hp(1)
        .ap(1)
        .speed(3)
        .ability_names(&["strike"])
        .build();
    let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
        .hp(20)
        .ap(0)
        .aoo(10.0, 1)
        .build();
    let actor_id = actor.entity;

    let mut content = empty_content();
    content
        .abilities
        .insert("strike".into(), strike_def("strike", 1, 1));

    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 3;
    difficulty.plan_beam_width = 20;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor, enemy], 1);
    let maps = empty_maps();
    let plans = generate_plans(actor_id, &ctx, &snap, &maps);

    // At least one single-step Move plan with lethal self_damage must exist.
    let lethal_move_plans: Vec<_> = plans
        .iter()
        .filter(|p| {
            p.steps.len() == 1
                && matches!(p.steps[0], PlanStep::Move { .. })
                && p.outcomes[0].self_damage >= 1.0
        })
        .collect();

    assert!(
        !lethal_move_plans.is_empty(),
        "at least one single-step lethal-Move plan must be in the pool",
    );
}

// ── apply_endturn double-tick regression (4f) ───────────────────────────

/// Verifies that `apply_endturn` is called **exactly once per plan branch**:
///
/// - A depth-1 plan's terminal snapshot shows the actor's DoT status ticked
///   once (`rounds_remaining` decremented by 1).
/// - A depth-2 plan's terminal snapshot shows it ticked twice total — once
///   per depth level — not 4 times (double-tick within a single expansion).
///
/// Spec §8 gotcha: "Wire conservatively — single tick per branch."
#[test]
fn apply_endturn_ticks_status_exactly_once_per_branch() {
    use crate::combat::ai::world::tags::cache::build_caches;
    use crate::content::statuses::StatusDef;
    use combat_engine::StatusId;

    // ── Content: one damage ability + a DoT status def ──────────────────
    let mut content = empty_content();
    let def = strike_def("strike", 1, 1);
    content.abilities.insert(def.id.clone(), def.clone());

    let poison_id = StatusId::from("poison");
    let poison_def = StatusDef {
        id: poison_id.clone(),
        name: "poison".into(),
        dot_dice: Some(combat_engine::DiceExpr::new(0, 0, 3)),
        ai_controlled: false,
        buff_class: None,
        engine: combat_engine::StatusDef {
            bonuses: combat_engine::StatusBonuses::default(),
            skips_turn: false,
            forces_targeting: false,
            blocks_mana_abilities: false,
            hp_percent_dot: 0,
            heal_per_tick: 0,
            causes_disadvantage: false,
        },
    };
    content.statuses.insert(poison_id.clone(), poison_def);

    // ── Snapshot: actor poisoned by themselves (applier == actor) ────────
    // tick_actor_statuses filters by applier == actor_uid, and
    // snapshot_to_combat_state sets applier = entity_to_uid(unit.entity)
    // for every status, so this actor's own status will be ticked.
    let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 30, 2);
    // applier must match actor_uid (entity.to_bits()) so tick_actor_statuses picks it up.
    actor.statuses = vec![combat_engine::state::ActiveStatus {
        id: poison_id.clone(),
        rounds_remaining: 3,
        dot_per_tick: 3,
        applier: combat_engine::state::EffectSource::Unit(combat_engine::state::UnitId(
            actor.entity.to_bits(),
        )),
    }];
    let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 1);
    let actor_id = actor.entity;

    let (_status_tag_cache, _) = build_caches(&content);

    // ── Depth-1: each stepped plan should show exactly one tick ──────────
    let mut difficulty = DifficultyProfile::hard();
    difficulty.plan_max_depth = 1;
    difficulty.plan_beam_width = 10;
    let ctx = make_ctx(&content, &difficulty);

    let snap = snapshot_from(vec![actor.clone(), target.clone()], 1);
    let maps = empty_maps();
    let depth1_plans = generate_plans(actor_id, &ctx, &snap, &maps);

    for plan in depth1_plans.iter().filter(|p| !p.steps.is_empty()) {
        let last_snap = plan
            .sim_snapshots
            .last()
            .expect("non-empty plan has snapshot");
        let actor_snap = last_snap.unit(actor_id).expect("actor in snapshot");
        let poison = actor_snap
            .statuses
            .iter()
            .find(|s| s.id == poison_id)
            .expect("poison survives one tick (started at 3 rounds)");
        assert_eq!(
            poison.rounds_remaining, 2,
            "depth-1: poison should tick once (3→2), got {}",
            poison.rounds_remaining
        );
    }

    // ── Depth-2: two-step plans tick once per depth, so 3→1 total ───────
    let mut difficulty2 = DifficultyProfile::hard();
    difficulty2.plan_max_depth = 2;
    difficulty2.plan_beam_width = 10;
    let ctx2 = make_ctx(&content, &difficulty2);
    let depth2_plans = generate_plans(actor_id, &ctx2, &snap, &maps);

    for plan in depth2_plans.iter().filter(|p| p.steps.len() == 2) {
        let last_snap = plan
            .sim_snapshots
            .last()
            .expect("two-step plan has snapshot");
        let actor_snap = last_snap.unit(actor_id).expect("actor in snapshot");
        let poison = actor_snap
            .statuses
            .iter()
            .find(|s| s.id == poison_id)
            .expect("poison survives two ticks (started at 3 rounds)");
        assert_eq!(
            poison.rounds_remaining, 1,
            "depth-2: poison should tick twice total (3→1), got {} \
                 (0 would indicate double-ticking)",
            poison.rounds_remaining
        );
    }
}
