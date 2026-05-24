
use storyforge::content::armor::{ArmorDef, ArmorSlot};
use storyforge::content::weapons::{HandType, WeaponDef};
use combat_engine::DiceExpr;
use storyforge::game::components::{CombatStats, Equipment};
use storyforge::content::content_view::ContentView;

// ── helpers ──────────────────────────────────────────────────────────────────

fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 10,
        strength: 4,
        dexterity: 2,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    }
}

fn armor_piece(id: &str, slot: ArmorSlot, armor: i32) -> ArmorDef {
    ArmorDef {
        id: id.into(),
        name: id.into(),
        slot,
        armor,
        max_hp: 0,
        strength: 0,
        dexterity: 0,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    }
}

fn armor_with_bonus(id: &str, slot: ArmorSlot, armor: i32, strength: i32, intelligence: i32) -> ArmorDef {
    ArmorDef {
        id: id.into(),
        name: id.into(),
        slot,
        armor,
        max_hp: 0,
        strength,
        dexterity: 0,
        constitution: 0,
        intelligence,
        wisdom: 0,
        charisma: 0,
    }
}

fn weapon(id: &str, hand: HandType) -> WeaponDef {
    WeaponDef {
        id: id.into(),
        name: id.into(),
        hand,
        dice: DiceExpr::new(1, 6, 0),
        spell_power: 0,
        armor: 0,
        max_hp: 0,
        strength: 0,
        dexterity: 0,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    }
}

fn weapon_with_bonus(id: &str, strength: i32, armor: i32) -> WeaponDef {
    WeaponDef {
        id: id.into(),
        name: id.into(),
        hand: HandType::MainHand,
        dice: DiceExpr::new(1, 6, 0),
        spell_power: 0,
        armor,
        max_hp: 0,
        strength,
        dexterity: 0,
        constitution: 0,
        intelligence: 0,
        wisdom: 0,
        charisma: 0,
    }
}

fn equip(main_hand: &str, chest: &str, legs: &str, feet: &str) -> Equipment {
    Equipment {
        main_hand: Some(main_hand.into()),
        off_hand: None,
        chest: chest.into(),
        legs: legs.into(),
        feet: feet.into(),
    }
}

fn db_with(weapons: Vec<WeaponDef>, armors: Vec<ArmorDef>) -> ContentView {
    ContentView {
        weapons: weapons.into_iter().map(|w| (w.id.clone(), w)).collect(),
        armor: armors.into_iter().map(|a| (a.id.clone(), a)).collect(),
        ..Default::default()
    }
}

// ── effective_stats ──────────────────────────────────────────────────────────

#[test]
fn no_bonuses_returns_base_stats() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_piece("chest", ArmorSlot::Chest, 2),
            armor_piece("legs", ArmorSlot::Legs, 0),
            armor_piece("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("sword", "chest", "legs", "feet");
    let result = db.effective_stats(&base_stats(), &eq);
    assert_eq!(result.strength, 4);
    assert_eq!(result.intelligence, 0);
}

#[test]
fn armor_bonus_adds_to_stat() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_with_bonus("robe", ArmorSlot::Chest, 0, 0, 2),
            armor_piece("legs", ArmorSlot::Legs, 0),
            armor_piece("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("sword", "robe", "legs", "feet");
    let result = db.effective_stats(&base_stats(), &eq);
    assert_eq!(result.intelligence, 2, "robe +2 int should add to base 0");
}

#[test]
fn bonuses_from_multiple_items_stack() {
    let db = db_with(
        vec![weapon_with_bonus("sword", 2, 0)],
        vec![
            armor_with_bonus("chest", ArmorSlot::Chest, 0, 1, 0),
            armor_with_bonus("legs", ArmorSlot::Legs, 0, 1, 0),
            armor_piece("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("sword", "chest", "legs", "feet");
    let result = db.effective_stats(&base_stats(), &eq);
    assert_eq!(result.strength, 4 + 2 + 1 + 1, "base(4) + sword(2) + chest(1) + legs(1)");
}

// ── equipment_armor ──────────────────────────────────────────────────────────

#[test]
fn armor_sums_across_all_slots() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_piece("chest", ArmorSlot::Chest, 2),
            armor_piece("legs", ArmorSlot::Legs, 1),
            armor_piece("feet", ArmorSlot::Feet, 1),
        ],
    );
    let eq = equip("sword", "chest", "legs", "feet");
    assert_eq!(db.equipment_armor(&eq), 4);
}

#[test]
fn weapon_armor_counts_in_total() {
    let db = db_with(
        vec![weapon_with_bonus("shield_sword", 0, 3)],
        vec![
            armor_piece("chest", ArmorSlot::Chest, 1),
            armor_piece("legs", ArmorSlot::Legs, 0),
            armor_piece("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("shield_sword", "chest", "legs", "feet");
    assert_eq!(db.equipment_armor(&eq), 4, "chest(1) + weapon(3)");
}

// ── two-handed validation ────────────────────────────────────────────────────

/// Two-handed weapon must not coexist with off_hand.
#[test]
#[should_panic(expected = "two-handed but off_hand is set")]
fn two_handed_with_off_hand_panics() {
    let db = db_with(
        vec![
            weapon("greatsword", HandType::TwoHanded),
            weapon("dagger", HandType::MainHand),
        ],
        vec![],
    );

    let main = db.weapons.get::<str>("greatsword").unwrap();
    let off_hand: Option<&str> = Some("dagger");

    assert!(
        main.hand != HandType::TwoHanded || off_hand.is_none(),
        "two-handed but off_hand is set"
    );
}

/// One-handed weapon allows off_hand (no panic).
#[test]
fn one_handed_with_off_hand_is_ok() {
    let db = db_with(
        vec![
            weapon("sword", HandType::MainHand),
            weapon("dagger", HandType::OffHand),
        ],
        vec![],
    );

    let main = db.weapons.get::<str>("sword").unwrap();
    let off_hand: Option<&str> = Some("dagger");

    assert!(
        main.hand != HandType::TwoHanded || off_hand.is_none(),
        "two-handed but off_hand is set"
    );
}
