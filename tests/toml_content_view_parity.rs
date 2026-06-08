//! Parity test: `TomlContentView` must agree with `EcsContentView` for all
//! content loaded from `assets/data/`.
//!
//! Strategy: load both views from the same source, then for every id in the
//! bridge `ContentView`, assert the engine-typed outputs are equal.
//!
//! `EcsContentView` is `pub(crate)` in the bridge, so we replicate its
//! mapping logic here — which is exactly what the parity test should guard
//! against drifting.  Any divergence between the inline mapping below and
//! `EcsContentView` is itself a bug that this test would not catch; the test
//! catches bugs in `TomlContentView`'s own parsing or mapping.

use std::path::Path;

use storyforge::content::content_view::ContentView as BridgeContentView;
use storyforge::combat_engine::{
    content::ContentView as EngineContentView,
    AbilityDef, AbilityId, EffectDef,
    StatusApplication, StatusBonuses, StatusDef,
    StatusId, TomlContentView, UnitTemplate,
};
// BridgeEffectDef removed — EffectDef is now pub use combat_engine::EffectDef in bridge.
use storyforge::game::components::Equipment;

// ── Helpers to map bridge types → engine types (mirrors EcsContentView) ───────

fn map_ability(content: &BridgeContentView, id: &AbilityId) -> Option<AbilityDef> {
    // Since EffectDef is now pub use combat_engine::EffectDef in the bridge,
    // bridge AbilityDef fields are the same types as engine fields.
    // is_move_toggle abilities have EffectDef::None as their engine effect.
    Some(content.abilities.get(id)?.into())
}

fn map_status(content: &BridgeContentView, id: &StatusId) -> Option<StatusDef> {
    let def = content.statuses.get(id)?;
    Some(StatusDef {
        causes_disadvantage:  def.causes_disadvantage,
        blocks_mana_abilities: def.blocks_mana_abilities,
        forces_targeting:     def.forces_targeting,
        skips_turn:           def.skips_turn,
        bonuses: StatusBonuses {
            armor_bonus:        def.bonuses.armor_bonus,
            damage_taken_bonus: def.bonuses.damage_taken_bonus,
            speed_bonus:        def.bonuses.speed_bonus,
        },
        hp_percent_dot:       def.hp_percent_dot,
        heal_per_tick: 0,
    })
}

fn map_status_bonuses(content: &BridgeContentView, id: &StatusId) -> StatusBonuses {
    content.statuses.get(id).map(|d| StatusBonuses {
        speed_bonus: d.bonuses.speed_bonus,
        armor_bonus: d.bonuses.armor_bonus,
        damage_taken_bonus: d.bonuses.damage_taken_bonus,
    }).unwrap_or_default()
}

fn map_unit_template(content: &BridgeContentView, id: &str) -> Option<UnitTemplate> {
    let tpl = content.unit_templates.get(id)?;
    let equipment = Equipment {
        main_hand: Some(tpl.equipment.main_hand.clone()),
        off_hand:  tpl.equipment.off_hand.clone(),
        chest:     tpl.equipment.chest.clone(),
        legs:      tpl.equipment.legs.clone(),
        feet:      tpl.equipment.feet.clone(),
    };
    let effective = content.effective_stats(&tpl.stats, &equipment);
    let armor     = content.equipment_armor(&equipment);
    // caster_context — mirror CasterContext::new (same as EcsContentView::unit_template).
    use storyforge::content::abilities::CasterContext as BridgeCasterContext;
    use storyforge::combat_engine::CasterContext as EngineCasterContext;
    use storyforge::combat_engine::CritFailOutcome;
    use storyforge::combat_engine::{DiceExpr as EngineDiceExpr, StatusId};
    use storyforge::content::abilities::EffectDef;
    use storyforge::content::races::CritFailEffect;
    let bevy_ctx = BridgeCasterContext::new(&tpl.stats, Some(&equipment), &content.weapons);
    let crit_fail_effect = tpl.path
        .as_deref()
        .and_then(|p| content.paths.get(p))
        .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
    // Inline translation of map_crit_fail_effect (pub(crate) in sim.rs).
    let crit_fail_outcome = match &crit_fail_effect {
        CritFailEffect::Miss         => CritFailOutcome::Miss,
        CritFailEffect::ManaOverload => CritFailOutcome::DoubleCost,
        CritFailEffect::BrokenFaith  => CritFailOutcome::ApplyStatus(StatusId::from("broken_faith")),
        CritFailEffect::CircuitBreach => CritFailOutcome::SelfDamage(EngineDiceExpr::new(0, 1, 2)),
        CritFailEffect::Exhaustion   => CritFailOutcome::ApplyStatus(StatusId::from("exhaustion")),
        CritFailEffect::PactControl  => CritFailOutcome::ApplyStatus(StatusId::from("pact_control")),
    };
    let engine_ctx = EngineCasterContext {
        str_mod:           bevy_ctx.str_mod,
        int_mod:           bevy_ctx.int_mod,
        spell_power:       bevy_ctx.spell_power,
        weapon_dice:       bevy_ctx.weapon_dice,
        crit_fail_outcome,
        dex_mod:           0,
    };
    // aoo_dice — mirror bootstrap AoO eligibility.
    let has_melee = tpl.ability_ids.iter().any(|aid| {
        content.abilities.get(aid).is_some_and(|def| {
            matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
        })
    });
    let str_mod = bevy_ctx.str_mod;
    let aoo_dice = if has_melee {
        bevy_ctx.weapon_dice.map(|core_dice| {
            EngineDiceExpr::new(core_dice.count, core_dice.sides, core_dice.bonus + str_mod)
        })
    } else {
        None
    };
    use storyforge::combat_engine::{PoolKind, RegenRule};
    Some(UnitTemplate {
        max_hp:     effective.max_hp,
        armor,
        base_speed: tpl.speed,
        max_ap:     1,
        mana_max:   tpl.resources.mana_max,
        energy_max: tpl.resources.energy_max,
        rage_max:   tpl.resources.rage_max,
        caster_context: engine_ctx,
        aoo_dice,
        auras:        Vec::new(),
        enemy_phases: Vec::new(),
        regen_per_pool: storyforge::combat_engine::enum_map::enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        initial_statuses: tpl.initial_statuses
            .iter()
            .map(|s| storyforge::combat_engine::StatusId::from(s.as_str()))
            .collect(),
        initial_pools: {
            let map = &tpl.initial_pools;
            storyforge::combat_engine::enum_map::enum_map! {
                PoolKind::Hp     => map.get("hp").copied(),
                PoolKind::Mana   => map.get("mana").copied(),
                PoolKind::Rage   => map.get("rage").copied(),
                PoolKind::Energy => map.get("energy").copied(),
                PoolKind::Ap     => map.get("ap").copied(),
                PoolKind::Mp     => map.get("mp").copied(),
            }
        },
        tags: Default::default(),
    })
}

// ── Assertions ────────────────────────────────────────────────────────────────

fn abilities_eq(a: &AbilityDef, b: &AbilityDef) -> bool {
    // AbilityDef contains EffectDef which doesn't impl PartialEq — compare
    // field-by-field for the parts that do.
    a.key          == b.key
        && a.cost_ap == b.cost_ap
        && a.costs_eq(b)
        && a.range.min  == b.range.min
        && a.range.max  == b.range.max
        && a.target_type == b.target_type
        && a.aoe         == b.aoe
        && a.friendly_fire == b.friendly_fire
        && a.requires_los == b.requires_los
        && a.passive      == b.passive
        && effect_eq(&a.effect, &b.effect)
        && statuses_eq(&a.statuses, &b.statuses)
}

trait AbilityExt {
    fn costs_eq(&self, other: &AbilityDef) -> bool;
}

impl AbilityExt for AbilityDef {
    fn costs_eq(&self, other: &AbilityDef) -> bool {
        if self.costs.len() != other.costs.len() { return false; }
        self.costs.iter().zip(&other.costs).all(|(a, b)| {
            a.resource == b.resource && a.amount == b.amount
        })
    }
}

fn effect_eq(a: &EffectDef, b: &EffectDef) -> bool {
    use EffectDef::*;
    match (a, b) {
        (None, None) | (WeaponAttack, WeaponAttack) | (RestoreResources, RestoreResources) => true,
        (Damage { dice: da }, Damage { dice: db })         => da == db,
        (SpellDamage { dice: da }, SpellDamage { dice: db }) => da == db,
        (Heal { dice: da }, Heal { dice: db })             => da == db,
        (GrantMovement { distance: a }, GrantMovement { distance: b }) => a == b,
        (Summon { template_id: ta, max_active: ma }, Summon { template_id: tb, max_active: mb }) => {
            ta == tb && ma == mb
        }
        (RevealEnvInRange { range: ra }, RevealEnvInRange { range: rb }) => ra == rb,
        _ => false,
    }
}

fn statuses_eq(a: &[StatusApplication], b: &[StatusApplication]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).all(|(x, y)| {
        x.status == y.status
            && x.duration_rounds == y.duration_rounds
            && x.on == y.on
    })
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn toml_content_view_matches_ecs_content_view() {
    let data_dir = Path::new("assets/data");
    let toml_view = TomlContentView::load_from_dir(data_dir)
        .expect("TomlContentView::load_from_dir failed");

    let bridge_view = BridgeContentView::load_layered(data_dir, data_dir);

    let mut failures: Vec<String> = Vec::new();

    // ── ability_def ───────────────────────────────────────────────────────────
    for id in bridge_view.abilities.keys() {
        let expected = map_ability(&bridge_view, id);
        let got      = toml_view.ability_def(id).cloned();

        match (&expected, &got) {
            (Some(e), Some(g)) => {
                if !abilities_eq(e, g) {
                    failures.push(format!(
                        "ability_def({id}): mismatch\n  expected: {e:?}\n  got:      {g:?}"
                    ));
                }
            }
            (Some(_), Option::None) => {
                failures.push(format!("ability_def({id}): missing in TomlContentView"));
            }
            (Option::None, Some(_)) => {
                failures.push(format!("ability_def({id}): present in TomlContentView but absent in bridge"));
            }
            (Option::None, Option::None) => {} // both absent = consistent
        }
    }
    // IDs only in TomlContentView (should not exist after loading same source)
    // No easy way to enumerate without exposing internals; skip this direction.

    // ── status_def ────────────────────────────────────────────────────────────
    for id in bridge_view.statuses.keys() {
        let expected = map_status(&bridge_view, id);
        let got      = toml_view.status_def(id).copied();

        match (&expected, &got) {
            (Some(e), Some(g)) => {
                if e.causes_disadvantage  != g.causes_disadvantage
                    || e.blocks_mana_abilities != g.blocks_mana_abilities
                    || e.forces_targeting  != g.forces_targeting
                    || e.skips_turn        != g.skips_turn
                    || e.bonuses.armor_bonus       != g.bonuses.armor_bonus
                    || e.bonuses.damage_taken_bonus != g.bonuses.damage_taken_bonus
                    || e.bonuses.speed_bonus       != g.bonuses.speed_bonus
                    || e.hp_percent_dot    != g.hp_percent_dot
                {
                    failures.push(format!(
                        "status_def({id}): mismatch\n  expected: {e:?}\n  got:      {g:?}"
                    ));
                }
            }
            (Some(_), Option::None) => {
                failures.push(format!("status_def({id}): missing in TomlContentView"));
            }
            (Option::None, Some(_)) => {
                failures.push(format!("status_def({id}): present in TomlContentView but absent in bridge"));
            }
            (Option::None, Option::None) => {}
        }
    }

    // ── status_bonuses ────────────────────────────────────────────────────────
    for id in bridge_view.statuses.keys() {
        let expected = map_status_bonuses(&bridge_view, id);
        let got      = toml_view.status_bonuses(id);
        if expected != got {
            failures.push(format!(
                "status_bonuses({id}): expected {expected:?}, got {got:?}"
            ));
        }
    }

    // ── unit_template ─────────────────────────────────────────────────────────
    for id in bridge_view.unit_templates.keys() {
        let expected = map_unit_template(&bridge_view, id);
        let got      = toml_view.unit_template(id);

        match (&expected, &got) {
            (Some(e), Some(g)) => {
                if e.max_hp            != g.max_hp
                    || e.armor         != g.armor
                    || e.base_speed    != g.base_speed
                    || e.max_ap        != g.max_ap
                    || e.mana_max      != g.mana_max
                    || e.energy_max    != g.energy_max
                    || e.rage_max      != g.rage_max
                    || e.caster_context != g.caster_context
                    || e.aoo_dice      != g.aoo_dice
                {
                    failures.push(format!(
                        "unit_template({id}): mismatch\n  expected: {e:?}\n  got:      {g:?}"
                    ));
                }
            }
            (Some(_), Option::None) => {
                failures.push(format!("unit_template({id}): missing in TomlContentView"));
            }
            (Option::None, Some(_)) => {
                failures.push(format!("unit_template({id}): present in TomlContentView but absent in bridge"));
            }
            (Option::None, Option::None) => {}
        }
    }

    // Sanity: we must have checked at least some abilities and statuses.
    // If these counts are 0 the test is vacuously passing — that's a setup bug.
    let n_abilities = bridge_view.abilities.len();
    let n_statuses  = bridge_view.statuses.len();
    assert!(n_abilities > 0, "no abilities loaded — parity test is vacuous");
    assert!(n_statuses  > 0, "no statuses loaded — parity test is vacuous");

    if !failures.is_empty() {
        panic!(
            "{} parity failure(s) between TomlContentView and EcsContentView \
             (checked {n_abilities} abilities, {n_statuses} statuses):\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }

    // Success report for visibility.
    println!(
        "Parity OK: {n_abilities} abilities, {n_statuses} statuses, \
         {} unit_templates checked",
        bridge_view.unit_templates.len(),
    );
}

/// Regression: both parsers must agree on the environment-targeted scout_traps
/// ability: target_type=Environment, aoe=Circle{radius:2},
/// effect=RevealEnvInRange{range:2}, passive=[TurnStart, OnMove].
///
/// This pins the single-source-of-radius guarantee (range comes from aoe_size,
/// not a separate reveal_range field), the Vec-form passive migration, and the
/// Wave-3 addition of OnMove to scout_traps.
#[test]
fn environment_ability_parity_scout_traps() {
    use storyforge::combat_engine::{
        content::ContentView as EngineContentView,
        AoEShape, EffectDef, PassiveTrigger, TargetType, TomlContentView,
    };
    use storyforge::content::content_view::ContentView as BridgeContentView;
    use storyforge::combat_engine::AbilityId;

    let data_dir = std::path::Path::new("assets/data");

    let toml_view = TomlContentView::load_from_dir(data_dir)
        .expect("TomlContentView::load_from_dir failed");

    let bridge_view = BridgeContentView::load_layered(data_dir, data_dir);

    let id = AbilityId::from("scout_traps");

    let engine_def = toml_view.ability_def(&id).cloned()
        .expect("scout_traps not found in TomlContentView");
    let bridge_def = bridge_view.abilities.get(&id)
        .expect("scout_traps not found in BridgeContentView");
    // bridge_def is storyforge::content::abilities::AbilityDef; its engine sub-field
    // holds the combat_engine::AbilityDef.  Deref gives us direct access.
    let bridge_engine_def: &storyforge::combat_engine::AbilityDef = bridge_def;

    // target_type
    assert_eq!(engine_def.target_type, TargetType::Environment,
        "TomlContentView: scout_traps target_type should be Environment");
    assert_eq!(bridge_engine_def.target_type, TargetType::Environment,
        "BridgeContentView: scout_traps target_type should be Environment");

    // aoe
    assert_eq!(engine_def.aoe, AoEShape::Circle { radius: 2 },
        "TomlContentView: scout_traps aoe should be Circle{{radius:2}}");
    assert_eq!(bridge_engine_def.aoe, AoEShape::Circle { radius: 2 },
        "BridgeContentView: scout_traps aoe should be Circle{{radius:2}}");

    // effect (RevealEnvInRange with range derived from aoe_size)
    match &engine_def.effect {
        EffectDef::RevealEnvInRange { range } =>
            assert_eq!(*range, 2, "TomlContentView: scout_traps reveal range should be 2"),
        other => panic!("TomlContentView: expected RevealEnvInRange, got {other:?}"),
    }
    match &bridge_engine_def.effect {
        EffectDef::RevealEnvInRange { range } =>
            assert_eq!(*range, 2, "BridgeContentView: scout_traps reveal range should be 2"),
        other => panic!("BridgeContentView: expected RevealEnvInRange, got {other:?}"),
    }

    // passive triggers (Wave 3: OnMove added alongside TurnStart)
    assert_eq!(
        engine_def.passive,
        vec![PassiveTrigger::TurnStart, PassiveTrigger::OnMove],
        "TomlContentView: scout_traps passive should be [TurnStart, OnMove]"
    );
    assert_eq!(
        bridge_engine_def.passive,
        vec![PassiveTrigger::TurnStart, PassiveTrigger::OnMove],
        "BridgeContentView: scout_traps passive should be [TurnStart, OnMove]"
    );

    // Both parsers agree with each other
    assert!(abilities_eq(&engine_def, bridge_engine_def),
        "scout_traps: TomlContentView and BridgeContentView disagree\n  toml:   {engine_def:?}\n  bridge: {bridge_engine_def:?}");
}
