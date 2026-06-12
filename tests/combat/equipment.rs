use combat_engine::DiceExpr;
use storyforge::content::armor::{ArmorDef, ArmorSlot, ArmorWeight};
use storyforge::content::content_view::ContentView;
use storyforge::content::weapons::{HandType, WeaponDef};
use storyforge::game::components::{CombatStats, Equipment};

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
        weight: ArmorWeight::Light,
        image: None,
        stats: storyforge::content::item_stats::ItemStats {
            armor,
            ..Default::default()
        },
    }
}

fn armor_with_bonus(
    id: &str,
    slot: ArmorSlot,
    armor: i32,
    strength: i32,
    intelligence: i32,
) -> ArmorDef {
    ArmorDef {
        id: id.into(),
        name: id.into(),
        slot,
        weight: ArmorWeight::Light,
        image: None,
        stats: storyforge::content::item_stats::ItemStats {
            armor,
            combat: storyforge::game::components::CombatStats {
                strength,
                intelligence,
                ..Default::default()
            },
            mana: 0,
            magic_resist: 0,
        },
    }
}

fn weapon(id: &str, hand: HandType) -> WeaponDef {
    WeaponDef {
        id: id.into(),
        name: id.into(),
        hand,
        dice: DiceExpr::new(1, 6, 0),
        ranged: false,
        spell_power: 0,
        image: None,
        stats: Default::default(),
    }
}

fn weapon_with_bonus(id: &str, strength: i32, armor: i32) -> WeaponDef {
    WeaponDef {
        id: id.into(),
        name: id.into(),
        hand: HandType::MainHand,
        dice: DiceExpr::new(1, 6, 0),
        ranged: false,
        spell_power: 0,
        image: None,
        stats: storyforge::content::item_stats::ItemStats {
            armor,
            combat: storyforge::game::components::CombatStats {
                strength,
                ..Default::default()
            },
            mana: 0,
            magic_resist: 0,
        },
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
    assert_eq!(
        result.strength,
        4 + 2 + 1 + 1,
        "base(4) + sword(2) + chest(1) + legs(1)"
    );
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

// ── equipment_mana_bonus ─────────────────────────────────────────────────────

fn armor_with_mana(id: &str, slot: ArmorSlot, mana: i32) -> ArmorDef {
    ArmorDef {
        id: id.into(),
        name: id.into(),
        slot,
        weight: ArmorWeight::Light,
        image: None,
        stats: storyforge::content::item_stats::ItemStats {
            mana,
            ..Default::default()
        },
    }
}

#[test]
fn mana_bonus_from_chest_armor() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_with_mana("mage_robe", ArmorSlot::Chest, 1),
            armor_piece("legs", ArmorSlot::Legs, 0),
            armor_piece("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("sword", "mage_robe", "legs", "feet");
    assert_eq!(db.equipment_mana_bonus(&eq), 1, "mage_robe gives +1 mana");
}

#[test]
fn mana_bonus_sums_across_armor_slots() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_with_mana("chest", ArmorSlot::Chest, 1),
            armor_with_mana("legs", ArmorSlot::Legs, 2),
            armor_with_mana("feet", ArmorSlot::Feet, 0),
        ],
    );
    let eq = equip("sword", "chest", "legs", "feet");
    assert_eq!(
        db.equipment_mana_bonus(&eq),
        3,
        "chest(1) + legs(2) + feet(0) = 3"
    );
}

#[test]
fn mana_bonus_zero_for_non_magical_armor() {
    let db = db_with(
        vec![weapon("sword", HandType::MainHand)],
        vec![
            armor_piece("heavy_plate", ArmorSlot::Chest, 3),
            armor_piece("legs", ArmorSlot::Legs, 1),
            armor_piece("feet", ArmorSlot::Feet, 1),
        ],
    );
    let eq = equip("sword", "heavy_plate", "legs", "feet");
    assert_eq!(db.equipment_mana_bonus(&eq), 0);
}
