mod common;

use bevy::prelude::*;

use common::*;
use storyforge::core::DiceRng;
use storyforge::game::bundles::hero_bundle;
use storyforge::game::components::{ActiveCombatant, CombatStats, Mana, Vital};
use storyforge::game::messages::{UseAbility, ValidatedAction};
use storyforge::game::resources::{HexPositions, TurnQueue};

// ── Full pipeline: UseAbility → target takes damage ──────────────────────────

#[test]
fn full_pipeline_melee_attack_damages_target() {
    let mut app = pipeline_app();
    let hero = app
        .world_mut()
        .spawn((Name::new("Hero"), test_hero(base_stats())))
        .id();
    let enemy = app
        .world_mut()
        .spawn((Name::new("Enemy"), test_enemy(base_stats())))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![hero, enemy];
        q.index = 0;
    }
    app.world_mut().entity_mut(hero).insert(ActiveCombatant);

    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(hero, (0, 0));
    positions.insert(enemy, (1, 0));

    // Script: d20=10 (no crit), weapon dice (short_sword 1d8) → 6.
    app.world_mut().resource_mut::<DiceRng>().script(&[10, 6]);

    write_message(
        &mut app,
        UseAbility {
            actor: hero,
            ability: MELEE_ATTACK.into(),
            target: enemy,
            target_pos: (1, 0),
        },
    );
    app.update();

    // Damage: 6 (dice) + 2 (STR mod for str=5) = 8. Enemy armor = 0. HP: 10 - 8 = 2.
    let hp = app.world().get::<Vital>(enemy).unwrap().hp;
    assert_eq!(hp, 2, "full pipeline: melee should deal dice+STR to target");
    assert!(app.world().get::<ActiveCombatant>(enemy).is_some());
}

// ── AoE hits multiple targets ────────────────────────────────────────────────

#[test]
fn aoe_damages_multiple_enemies() {
    let mut app = resolve_app();
    let actor = app
        .world_mut()
        .spawn((
            Name::new("Mage"),
            hero_bundle(
                CombatStats { max_hp: 20, strength: 0, dexterity: 0, constitution: 0,
                    intelligence: 4, wisdom: 0, charisma: 0 },
                0, 3, vec!["fireball".into()], test_equipment(),
            ),
            Mana::new(10),
        ))
        .id();

    let enemy1 = app
        .world_mut()
        .spawn((Name::new("E1"), test_enemy(base_stats())))
        .id();
    let enemy2 = app
        .world_mut()
        .spawn((Name::new("E2"), test_enemy(base_stats())))
        .id();

    {
        let mut q = app.world_mut().resource_mut::<TurnQueue>();
        q.order = vec![actor, enemy1, enemy2];
        q.index = 0;
    }
    app.world_mut().entity_mut(actor).insert(ActiveCombatant);

    let mut positions = app.world_mut().resource_mut::<HexPositions>();
    positions.insert(actor, (0, 0));
    positions.insert(enemy1, (3, 0));
    positions.insert(enemy2, (4, 0));

    // Script: d20=10 (no crit fail), then 2d3 → [2, 3] = 5 total.
    app.world_mut().resource_mut::<DiceRng>().script(&[10, 2, 3]);

    write_message(
        &mut app,
        ValidatedAction {
            actor,
            ability: "fireball".into(),
            target: enemy1,
            target_pos: (3, 0),
            disadvantage: false,
        },
    );
    app.update();

    let hp1 = app.world().get::<Vital>(enemy1).unwrap().hp;
    let hp2 = app.world().get::<Vital>(enemy2).unwrap().hp;
    assert!(hp1 < 10, "enemy1 should take AoE damage, hp={hp1}");
    assert!(hp2 < 10, "enemy2 should take AoE damage, hp={hp2}");
    assert_eq!(app.world().get::<Mana>(actor).unwrap().current, 5);
}
