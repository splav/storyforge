//! Camp screen: lets the player re-equip heroes from the party stash. Entered
//! via `AppState::Camp` on two triggers (see `scenario::advance_scenario_system`):
//!   1. between two Story scenes (Story→Story, when a campaign is active and the
//!      source scene's `no_camp` is false), and
//!   2. **forced at the start of a new chapter** when the carried-over stash is
//!      non-empty — so the previous chapter's boss drop can be equipped.
//!
//! Chapters always open on a Story scene, so a "Continue" button (or Enter/Space)
//! transitions to `AppState::Story` to show the already-queued next scene.
//!
//! ## Layout
//! - Hero equipment grids: one labeled section per class-hero with 5 square
//!   cells (MainHand / OffHand / Chest / Legs / Feet), each showing the current
//!   item short-name.
//! - Backpack grid: 6-column wrap of 56×56 cells for every item in
//!   `CampaignState.stash`.  Empty stash shows a placeholder label.
//! - Continue button (also Space / Enter).
//!
//! ## Interaction
//! Click any cell to select it (bright yellow border highlight).  Click a
//! second cell to attempt a swap:
//!   - `EquipCell ↔ BackpackCell`: validated via `try_equip`; on success the
//!     backpack item moves into the equip slot and the displaced item takes the
//!     backpack slot (true swap — no item loss or duplication).
//!   - `BackpackCell ↔ BackpackCell`: swaps positions in the stash vector.
//!   - `EquipCell ↔ EquipCell`: not supported in this pass; clicking a second
//!     equip cell while one is selected deselects the first and selects the new
//!     one instead.
//!     Clicking the already-selected cell deselects it (no action taken).

use super::button::{spawn_standard_button, ButtonStyle};
use crate::app_state::AppState;
use crate::content::armor::{ArmorDef, ArmorSlot, ArmorWeight};
use crate::content::content_view::ActiveContent;
use crate::content::item_ref::{EquipmentSave, ItemRef};
use crate::content::scenarios::active_party;
use crate::content::settings::GameSettings;
use crate::content::weapons::{HandType, WeaponDef};
use crate::game::components::Equipment;
use crate::game::resources::{CampaignState, GameDb, ScenarioState};
use crate::persistence::{save_repo, PersistencePaths};
use bevy::prelude::*;
use combat_engine::{ArmorId, WeaponId};

// ── Marker components ────────────────────────────────────────────────────────

/// Root node for the camp screen — despawned on `OnExit(Camp)`.
#[derive(Component)]
pub struct CampScreenRoot;

/// Marker on the Continue button.
#[derive(Component)]
pub struct CampContinueButton;

/// Marks a hero equipment-slot cell in the grid.
#[derive(Component, Clone)]
pub struct EquipCell {
    pub hero_id: String,
    pub slot: EquipSlot,
}

/// Marks a backpack (stash) cell in the grid.
#[derive(Component, Clone)]
pub struct BackpackCell {
    pub index: usize,
}

/// Marks the currently-selected cell entity so we can reset its highlight.
#[derive(Component)]
pub struct SelectedCellMarker;

/// Which slot in the hero's loadout the `EquipCell` represents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EquipSlot {
    MainHand,
    OffHand,
    Chest,
    Legs,
    Feet,
}

// ── Camp state resources ─────────────────────────────────────────────────────

/// Which cell is currently selected, waiting for a second click to swap with.
#[derive(Resource, Default)]
pub struct CampEquipSelection {
    pub selected: Option<CellKind>,
}

/// Which kind of cell is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellKind {
    Equip { hero_id: String, slot: EquipSlot },
    Backpack { index: usize },
}

/// Set to `true` after an equip operation to trigger a UI teardown+rebuild
/// within the same frame's cleanup pass.
#[derive(Resource, Default)]
pub struct CampNeedsRebuild(pub bool);

// ── try_equip — pure, Bevy-free equip logic ──────────────────────────────────

/// Error returned when an equip operation is invalid.
#[derive(Debug, PartialEq, Eq)]
pub enum EquipError {
    /// The item is a weapon but the target slot is not a hand slot.
    WeaponIntoArmorSlot,
    /// The item is a `HandType::OffHand` weapon being placed into main-hand.
    OffHandIntoMainHand,
    /// The item is armor but the target slot is not the armor's native slot.
    ArmorSlotMismatch { expected: ArmorSlot, got: EquipSlot },
    /// The item is armor but the target slot is a hand slot.
    ArmorIntoHandSlot,
    /// Item ID not found in the content registries.
    UnknownItem,
}

/// Result of a successful equip: the hero's new `EquipmentSave` and optionally
/// the `ItemRef` that was displaced from the slot (so it can be returned to the
/// stash).
#[derive(Debug)]
pub struct EquipResult {
    pub new_save: EquipmentSave,
    /// The item displaced from the target slot, if any.  Armor slots always
    /// have an occupant so displacement is always `Some` for armor (unless the
    /// slot contained an empty sentinel — see notes in the interaction system).
    /// Hand slots may be vacant, so displacement may be `None`.
    pub displaced: Option<ItemRef>,
}

/// Try to equip `item` into `slot` for a hero whose current equipment is
/// represented by `current_save`.
///
/// Validation rules:
/// - `ItemRef::Weapon` may only go into `MainHand` or `OffHand`.
///   - A `TwoHanded` weapon may only go into `MainHand`; equipping it clears
///     `off_hand` (the off-hand item is returned separately by the caller).
///   - An `OffHand` weapon may only go into `OffHand`.
///   - A `MainHand` weapon may go into either `MainHand` or `OffHand`.
/// - `ItemRef::Armor` may only go into its matching `ArmorSlot`.
///
/// Returns `Ok(EquipResult)` on success, `Err(EquipError)` on validation
/// failure.  Does not mutate anything — callers apply the result.
pub fn try_equip(
    current_save: &EquipmentSave,
    slot: &EquipSlot,
    item: ItemRef,
    weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    armor: &std::collections::HashMap<ArmorId, ArmorDef>,
) -> Result<EquipResult, EquipError> {
    match &item {
        ItemRef::Weapon(wid) => {
            let def = weapons.get(wid).ok_or(EquipError::UnknownItem)?;
            match slot {
                EquipSlot::MainHand => {
                    if def.hand == HandType::OffHand {
                        return Err(EquipError::OffHandIntoMainHand);
                    }
                    // Two-handed clears off_hand; the caller handles returning
                    // the displaced off-hand item to the stash separately.
                    let displaced = current_save
                        .main_hand
                        .as_ref()
                        .map(|id| ItemRef::Weapon(id.clone()));
                    let mut new_save = current_save.clone();
                    new_save.main_hand = Some(wid.clone());
                    if def.hand == HandType::TwoHanded {
                        new_save.off_hand = None;
                    }
                    Ok(EquipResult {
                        new_save,
                        displaced,
                    })
                }
                EquipSlot::OffHand => {
                    if def.hand == HandType::TwoHanded {
                        // Two-handed can only go into main-hand.
                        return Err(EquipError::WeaponIntoArmorSlot);
                    }
                    let displaced = current_save
                        .off_hand
                        .as_ref()
                        .map(|id| ItemRef::Weapon(id.clone()));
                    let mut new_save = current_save.clone();
                    new_save.off_hand = Some(wid.clone());
                    Ok(EquipResult {
                        new_save,
                        displaced,
                    })
                }
                EquipSlot::Chest | EquipSlot::Legs | EquipSlot::Feet => {
                    Err(EquipError::WeaponIntoArmorSlot)
                }
            }
        }
        ItemRef::Armor(aid) => {
            let def = armor.get(aid).ok_or(EquipError::UnknownItem)?;
            match slot {
                EquipSlot::MainHand | EquipSlot::OffHand => Err(EquipError::ArmorIntoHandSlot),
                EquipSlot::Chest => {
                    if def.slot != ArmorSlot::Chest {
                        return Err(EquipError::ArmorSlotMismatch {
                            expected: def.slot,
                            got: EquipSlot::Chest,
                        });
                    }
                    let displaced = Some(ItemRef::Armor(current_save.chest.clone()));
                    let mut new_save = current_save.clone();
                    new_save.chest = aid.clone();
                    Ok(EquipResult {
                        new_save,
                        displaced,
                    })
                }
                EquipSlot::Legs => {
                    if def.slot != ArmorSlot::Legs {
                        return Err(EquipError::ArmorSlotMismatch {
                            expected: def.slot,
                            got: EquipSlot::Legs,
                        });
                    }
                    let displaced = Some(ItemRef::Armor(current_save.legs.clone()));
                    let mut new_save = current_save.clone();
                    new_save.legs = aid.clone();
                    Ok(EquipResult {
                        new_save,
                        displaced,
                    })
                }
                EquipSlot::Feet => {
                    if def.slot != ArmorSlot::Feet {
                        return Err(EquipError::ArmorSlotMismatch {
                            expected: def.slot,
                            got: EquipSlot::Feet,
                        });
                    }
                    let displaced = Some(ItemRef::Armor(current_save.feet.clone()));
                    let mut new_save = current_save.clone();
                    new_save.feet = aid.clone();
                    Ok(EquipResult {
                        new_save,
                        displaced,
                    })
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve a hero's current effective equipment: saved loadout (if any) or
/// class default.
fn resolve_hero_equipment(
    hero_id: &str,
    class_id: &str,
    campaign: &CampaignState,
    content: &crate::content::content_view::ActiveContentData,
) -> EquipmentSave {
    if let Some(saved) = campaign.loadouts.get(hero_id) {
        return saved.clone();
    }
    if let Some(class_def) = content.classes.get(class_id) {
        EquipmentSave::from_equipment(&Equipment {
            main_hand: Some(class_def.main_hand.clone()),
            off_hand: class_def.off_hand.clone(),
            chest: class_def.chest.clone(),
            legs: class_def.legs.clone(),
            feet: class_def.feet.clone(),
        })
    } else {
        warn!(
            "camp: unknown class_id '{}' for hero '{}'",
            class_id, hero_id
        );
        EquipmentSave {
            main_hand: None,
            off_hand: None,
            chest: ArmorId::from(""),
            legs: ArmorId::from(""),
            feet: ArmorId::from(""),
        }
    }
}

fn weapon_name<'a>(
    id: &WeaponId,
    content: &'a crate::content::content_view::ActiveContentData,
) -> &'a str {
    content
        .weapons
        .get(id)
        .map(|d| d.name.as_str())
        .unwrap_or("?")
}

fn armor_name<'a>(
    id: &ArmorId,
    content: &'a crate::content::content_view::ActiveContentData,
) -> &'a str {
    content
        .armor
        .get(id)
        .map(|d| d.name.as_str())
        .unwrap_or("?")
}

/// Russian label for an armor-weight class, shown on the comparison card.
fn armor_weight_label(weight: ArmorWeight) -> &'static str {
    match weight {
        ArmorWeight::Light => "Лёгкая",
        ArmorWeight::Medium => "Средняя",
        ArmorWeight::Heavy => "Тяжёлая",
    }
}

/// Weight class of an item — `Some` only for armor (weapons have no class).
fn item_weight(
    item: &ItemRef,
    content: &crate::content::content_view::ActiveContentData,
) -> Option<ArmorWeight> {
    match item {
        ItemRef::Armor(aid) => content.armor.get(aid).map(|d| d.weight),
        ItemRef::Weapon(_) => None,
    }
}

/// Short display label for an item (first 8 chars to fit in the 56px cell).
fn item_abbrev(
    item: &ItemRef,
    content: &crate::content::content_view::ActiveContentData,
) -> String {
    let full = match item {
        ItemRef::Weapon(wid) => weapon_name(wid, content),
        ItemRef::Armor(aid) => armor_name(aid, content),
    };
    // Take up to 8 chars (char boundary safe).
    let end = full
        .char_indices()
        .nth(8)
        .map(|(i, _)| i)
        .unwrap_or(full.len());
    full[..end].to_string()
}

/// Asset path (relative to `assets/images/`) of an item's icon, if it has one.
fn item_image_path<'a>(
    item: &ItemRef,
    content: &'a crate::content::content_view::ActiveContentData,
) -> Option<&'a str> {
    match item {
        ItemRef::Weapon(wid) => content.weapons.get(wid)?.image.as_deref(),
        ItemRef::Armor(aid) => content.armor.get(aid)?.image.as_deref(),
    }
}

/// Damage label for a weapon: dice notation plus expected value, e.g.
/// `"2d6 (7.0)"`. `None` for armor or weapons missing from content. The
/// expected value keeps one decimal so 1d8 reads `4.5`, not a rounded `4`.
fn weapon_damage_label(
    item: &ItemRef,
    content: &crate::content::content_view::ActiveContentData,
) -> Option<String> {
    let ItemRef::Weapon(wid) = item else {
        return None;
    };
    let dice = content.weapons.get(wid)?.dice;
    let mut notation = format!("{}d{}", dice.count, dice.sides);
    if dice.bonus != 0 {
        notation.push_str(&format!("{:+}", dice.bonus));
    }
    Some(format!("{notation} ({:.1})", dice.expected()))
}

/// Color for a stat delta: green when the change favors the hero, red when it
/// hurts, grey when neutral.
fn delta_color(delta: f32) -> Color {
    if delta > 0.0 {
        Color::srgb(0.3, 0.9, 0.3)
    } else if delta < 0.0 {
        Color::srgb(0.9, 0.3, 0.3)
    } else {
        Color::srgb(0.6, 0.6, 0.6)
    }
}

// ── Stat comparison card ─────────────────────────────────────────────────────

/// Marker for the fixed comparison-card panel.
/// Spawned once inside `CampScreenRoot`; shown/hidden by `camp_comparison_system`.
#[derive(Component)]
pub struct ComparisonCard;

/// One stat row in the comparison card.
///
/// Orientation is **role-based, not selection-based**: `equipped_val` is always
/// the stat of the item currently worn by the hero, `incoming_val` the stat of
/// the candidate item from the backpack. The displayed delta is therefore always
/// `incoming − equipped` = "the change for the character", regardless of whether
/// the player clicked the worn item or the backpack item first.
#[derive(Debug, Clone)]
pub struct CompareRow {
    pub label: String,
    /// Value for the currently-equipped item (left column / baseline).
    pub equipped_val: f32,
    /// Value for the incoming backpack item (right column / candidate).
    pub incoming_val: f32,
}

/// Build comparison rows between the currently-equipped item and an incoming one.
///
/// Returns only rows where at least one item has a non-zero value.
/// Delegates stat extraction to `item_stats`.
///
/// Pure function — no Bevy types, fully unit-testable.
pub fn compare_items(
    equipped: &ItemRef,
    incoming: &ItemRef,
    weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    armor: &std::collections::HashMap<ArmorId, ArmorDef>,
) -> Vec<CompareRow> {
    // All possible stat labels in display order.
    const LABELS: &[&str] = &[
        "Урон",
        "Сила закл.",
        "Мана",
        "Броня",
        "Сопр. магии",
        "HP",
        "СИЛ",
        "ЛОВ",
        "ТЕЛ",
        "ИНТ",
        "МУД",
        "ХАР",
    ];

    let eq_stats = item_stats(equipped, weapons, armor);
    let in_stats = item_stats(incoming, weapons, armor);

    let val_of = |stats: &[(String, f32)], label: &str| -> f32 {
        stats
            .iter()
            .find(|(l, _)| l == label)
            .map_or(0.0, |(_, v)| *v)
    };

    LABELS
        .iter()
        .filter_map(|label| {
            let ev = val_of(&eq_stats, label);
            let iv = val_of(&in_stats, label);
            if ev == 0.0 && iv == 0.0 {
                return None;
            }
            Some(CompareRow {
                label: label.to_string(),
                equipped_val: ev,
                incoming_val: iv,
            })
        })
        .collect()
}

/// Orient a worn-vs-incoming comparison by **role**, not selection order.
///
/// Fixes the sign-flip bug where the delta direction depended on which cell the
/// player clicked first. `cell_compatible` only ever offers an Equip↔Backpack
/// pair, so the selected cell's kind fully determines roles: returns
/// `(worn, incoming)` with the equip-slot item as `worn`. A delta computed as
/// `incoming − worn` is then always "the change for the hero".
///
/// Pure function — no Bevy types, fully unit-testable.
fn orient_comparison<T>(selected_kind: &CellKind, selected: T, hovered: T) -> (T, T) {
    if matches!(selected_kind, CellKind::Equip { .. }) {
        (selected, hovered) // selected worn, hovered backpack
    } else {
        (hovered, selected) // selected backpack, hovered worn
    }
}

/// How to render a cell when a selection is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellStyle {
    /// No selection active — normal idle appearance.
    Idle,
    /// This cell is the currently selected cell.
    Selected,
    /// A selection is active and this cell is a valid swap target.
    Active,
    /// A selection is active but this cell is not a valid swap target.
    Inactive,
}

/// Extract stat values from an item for display in the comparison card.
///
/// Returns `(label, value)` pairs for every stat that is non-zero for this item.
/// Урон = average dice damage via `DiceExpr::expected()`.
///
/// Pure function — no Bevy types, fully unit-testable.
pub fn item_stats(
    item: &ItemRef,
    weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    armor: &std::collections::HashMap<ArmorId, ArmorDef>,
) -> Vec<(String, f32)> {
    let (
        damage,
        spell_power,
        armor_val,
        magic_resist,
        max_hp,
        strength,
        dexterity,
        constitution,
        intelligence,
        wisdom,
        charisma,
        mana,
    ) = match item {
        ItemRef::Weapon(wid) => {
            if let Some(def) = weapons.get(wid) {
                (
                    def.dice.expected(),
                    def.spell_power as f32,
                    def.stats.armor as f32,
                    def.stats.magic_resist as f32,
                    def.stats.combat.max_hp as f32,
                    def.stats.combat.strength as f32,
                    def.stats.combat.dexterity as f32,
                    def.stats.combat.constitution as f32,
                    def.stats.combat.intelligence as f32,
                    def.stats.combat.wisdom as f32,
                    def.stats.combat.charisma as f32,
                    0.0, // weapons carry no mana bonus
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
            }
        }
        ItemRef::Armor(aid) => {
            if let Some(def) = armor.get(aid) {
                (
                    0.0,
                    0.0,
                    def.stats.armor as f32,
                    def.stats.magic_resist as f32,
                    def.stats.combat.max_hp as f32,
                    def.stats.combat.strength as f32,
                    def.stats.combat.dexterity as f32,
                    def.stats.combat.constitution as f32,
                    def.stats.combat.intelligence as f32,
                    def.stats.combat.wisdom as f32,
                    def.stats.combat.charisma as f32,
                    def.stats.mana as f32,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
            }
        }
    };

    let candidates: &[(&str, f32)] = &[
        ("Урон", damage),
        ("Сила закл.", spell_power),
        ("Мана", mana),
        ("Броня", armor_val),
        ("Сопр. магии", magic_resist),
        ("HP", max_hp),
        ("СИЛ", strength),
        ("ЛОВ", dexterity),
        ("ТЕЛ", constitution),
        ("ИНТ", intelligence),
        ("МУД", wisdom),
        ("ХАР", charisma),
    ];

    candidates
        .iter()
        .filter(|(_, v)| *v != 0.0)
        .map(|(label, v)| (label.to_string(), *v))
        .collect()
}

/// Returns `true` if `target` is a valid swap destination when `selection` is active.
///
/// Compatibility rules:
/// - `Backpack{index}` (item I) → compatible with `Equip{hero, slot}` iff
///   `try_equip(hero's loadout, slot, I)` succeeds. All other `Backpack` cells
///   are incompatible (only fitting equip slots are offered).
/// - `Equip{hero, slot}` (item J) → compatible with `Backpack{index}` (item K)
///   iff `try_equip(hero's loadout, slot, K)` succeeds. All other `Equip` cells
///   are incompatible (equip↔equip is not supported).
/// - The selected cell itself should be handled separately by the caller.
///
/// Pure function — no Bevy types, fully unit-testable.
pub fn cell_compatible(
    selection: &CellKind,
    target: &CellKind,
    campaign: &CampaignState,
    db: &GameDb,
    scenario_state: &ScenarioState,
    content: &crate::content::content_view::ActiveContentData,
) -> bool {
    match (selection, target) {
        // Backpack → Equip: check whether the backpack item fits the equip slot.
        (CellKind::Backpack { index: bp_idx }, CellKind::Equip { hero_id, slot }) => {
            let Some(item) = campaign.stash.get(*bp_idx) else {
                return false;
            };
            let scen = db.scenarios.get(&scenario_state.scenario_id);
            let Some(scen) = scen else { return false };
            let party = active_party(scen, scenario_state.scene_index);
            let class_id = party
                .iter()
                .find(|m| m.id == *hero_id)
                .map(|m| m.class_id.as_str())
                .unwrap_or("");
            let eq = resolve_hero_equipment(hero_id, class_id, campaign, content);
            try_equip(&eq, slot, item.clone(), &content.weapons, &content.armor).is_ok()
                && hero_can_wear(class_id, item, content)
        }
        // Backpack → Backpack: always incompatible (we only offer equip-slot targets).
        (CellKind::Backpack { .. }, CellKind::Backpack { .. }) => false,

        // Equip → Backpack: check whether the backpack item fits back into the same slot.
        (CellKind::Equip { hero_id, slot }, CellKind::Backpack { index: bp_idx }) => {
            let Some(item) = campaign.stash.get(*bp_idx) else {
                return false;
            };
            let scen = db.scenarios.get(&scenario_state.scenario_id);
            let Some(scen) = scen else { return false };
            let party = active_party(scen, scenario_state.scene_index);
            let class_id = party
                .iter()
                .find(|m| m.id == *hero_id)
                .map(|m| m.class_id.as_str())
                .unwrap_or("");
            let eq = resolve_hero_equipment(hero_id, class_id, campaign, content);
            try_equip(&eq, slot, item.clone(), &content.weapons, &content.armor).is_ok()
                && hero_can_wear(class_id, item, content)
        }
        // Equip → Equip: not supported.
        (CellKind::Equip { .. }, CellKind::Equip { .. }) => false,
    }
}

/// Camp-only passive proficiency gate: may a hero of `class_id` wear `item`?
///
/// Weapons and Light armor are unrestricted. Medium/Heavy armor require the
/// hero's class to list that weight in `armor_proficiencies`. Unknown class or
/// unknown armor id → denied (fail-closed): the camp only ever shows real player
/// classes, so a miss signals a content/resolution bug we want surfaced, not a
/// mage silently granted plate. Light armor stays allowed even for an unknown
/// class. Enforced ONLY here (camp); engine/combat never consult this.
fn hero_can_wear(
    class_id: &str,
    item: &ItemRef,
    content: &crate::content::content_view::ActiveContentData,
) -> bool {
    let ItemRef::Armor(aid) = item else {
        return true;
    }; // weapons: always
    let Some(def) = content.armor.get(aid) else {
        return false;
    }; // unknown item: deny
    match def.weight {
        ArmorWeight::Light => true, // anyone
        w => content
            .classes
            .get(class_id)
            .is_some_and(|c| c.armor_proficiencies.contains(&w)), // unknown class: deny
    }
}

/// Resolve which item (if any) lives in a given `CellKind`.
///
/// - `Backpack { index }` → `stash[index]`  
/// - `Equip { hero_id, slot }` → resolve loadout, read the slot  
fn cell_item(
    kind: &CellKind,
    campaign: &CampaignState,
    db: &GameDb,
    scenario_state: &ScenarioState,
    content: &crate::content::content_view::ActiveContentData,
) -> Option<ItemRef> {
    match kind {
        CellKind::Backpack { index } => campaign.stash.get(*index).cloned(),
        CellKind::Equip { hero_id, slot } => {
            let scen = db.scenarios.get(&scenario_state.scenario_id)?;
            let party = active_party(scen, scenario_state.scene_index);
            let class_id = party
                .iter()
                .find(|m| m.id == *hero_id)
                .map(|m| m.class_id.as_str())
                .unwrap_or("");
            let eq = resolve_hero_equipment(hero_id, class_id, campaign, content);
            match slot {
                EquipSlot::MainHand => eq.main_hand.map(ItemRef::Weapon),
                EquipSlot::OffHand => eq.off_hand.map(ItemRef::Weapon),
                EquipSlot::Chest => {
                    let id = eq.chest;
                    if id.0.is_empty() {
                        None
                    } else {
                        Some(ItemRef::Armor(id))
                    }
                }
                EquipSlot::Legs => {
                    let id = eq.legs;
                    if id.0.is_empty() {
                        None
                    } else {
                        Some(ItemRef::Armor(id))
                    }
                }
                EquipSlot::Feet => {
                    let id = eq.feet;
                    if id.0.is_empty() {
                        None
                    } else {
                        Some(ItemRef::Armor(id))
                    }
                }
            }
        }
    }
}

// ── Visual constants ─────────────────────────────────────────────────────────

const CELL_SIZE: f32 = 56.0;
const CELL_GAP: f32 = 6.0;
/// Border color for an idle (unselected) cell.
const CELL_IDLE_BORDER: Color = Color::srgb(0.35, 0.32, 0.28);
/// Background for idle cells.
const CELL_IDLE_BG: Color = Color::srgb(0.10, 0.10, 0.08);
/// Border color for the selected (highlighted) cell.
const CELL_SELECTED_BORDER: Color = Color::srgb(0.9, 0.85, 0.3);
/// Background tint for the selected cell.
const CELL_SELECTED_BG: Color = Color::srgb(0.18, 0.17, 0.06);
/// Border color for a dimmed/incompatible cell when a selection is active.
const CELL_INACTIVE_BORDER: Color = Color::srgba(0.25, 0.23, 0.20, 0.4);
/// Background for a dimmed/incompatible cell.
const CELL_INACTIVE_BG: Color = Color::srgba(0.08, 0.08, 0.07, 0.4);

// ── Cell spawn helper ─────────────────────────────────────────────────────────

/// Spawns a square 56×56 Button cell. If `icon` is `Some`, renders an image
/// filling the cell; otherwise renders the text `label` as fallback.
fn spawn_cell<'a>(
    parent: &'a mut ChildSpawnerCommands,
    font: Handle<Font>,
    label: impl Into<String>,
    icon: Option<Handle<Image>>,
    style: CellStyle,
) -> EntityCommands<'a> {
    let (border_color, bg_color, text_alpha) = match style {
        CellStyle::Idle | CellStyle::Active => (CELL_IDLE_BORDER, CELL_IDLE_BG, 1.0_f32),
        CellStyle::Selected => (CELL_SELECTED_BORDER, CELL_SELECTED_BG, 1.0_f32),
        CellStyle::Inactive => (CELL_INACTIVE_BORDER, CELL_INACTIVE_BG, 0.4_f32),
    };

    let mut ec = parent.spawn((
        Button,
        Node {
            width: Val::Px(CELL_SIZE),
            height: Val::Px(CELL_SIZE),
            border: UiRect::all(Val::Px(1.5)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            overflow: Overflow::clip(),
            ..default()
        },
        BorderColor::all(border_color),
        BackgroundColor(bg_color),
    ));
    ec.with_children(|btn| {
        if let Some(handle) = icon {
            btn.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                ImageNode {
                    image: handle,
                    color: Color::WHITE.with_alpha(text_alpha),
                    ..default()
                },
            ));
        } else {
            btn.spawn((
                Text::new(label),
                TextFont {
                    font,
                    font_size: 10.0,
                    ..default()
                },
                TextColor(Color::WHITE.with_alpha(text_alpha)),
            ));
        }
    });
    ec
}

// ── Spawn helper ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn spawn_camp_ui(
    commands: &mut Commands,
    font: Handle<Font>,
    asset_server: &AssetServer,
    db: &GameDb,
    scenario_state: &ScenarioState,
    campaign: &CampaignState,
    content: &crate::content::content_view::ActiveContentData,
    selection: &CampEquipSelection,
) {
    let scen = db.scenarios.get(&scenario_state.scenario_id).unwrap();
    let party = active_party(scen, scenario_state.scene_index);

    // Only class-heroes get a loadout row; template-NPCs are skipped.
    let class_heroes: Vec<_> = party.iter().filter(|m| m.template.is_none()).collect();

    /// Resolve the `CellStyle` for a given cell, given the current selection.
    fn cell_style(
        cell: &CellKind,
        selection: &CampEquipSelection,
        campaign: &CampaignState,
        db: &GameDb,
        scenario_state: &ScenarioState,
        content: &crate::content::content_view::ActiveContentData,
    ) -> CellStyle {
        let Some(sel) = &selection.selected else {
            return CellStyle::Idle;
        };
        if sel == cell {
            return CellStyle::Selected;
        }
        if cell_compatible(sel, cell, campaign, db, scenario_state, content) {
            CellStyle::Active
        } else {
            CellStyle::Inactive
        }
    }

    commands
        .spawn((
            CampScreenRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::FlexStart,
                padding: UiRect::all(Val::Px(24.0)),
                row_gap: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.05, 0.10, 0.95)),
            ZIndex(200),
        ))
        .with_children(|root| {
            // Title
            root.spawn((
                Text::new("Привал"),
                TextFont {
                    font: font.clone(),
                    font_size: 28.0,
                    ..default()
                },
                TextColor(Color::srgb(0.9, 0.85, 0.7)),
            ));

            // Instruction hint
            root.spawn((
                Text::new("Нажмите ячейку, затем ячейку назначения, чтобы поменять местами"),
                TextFont {
                    font: font.clone(),
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.6, 0.6, 0.6)),
            ));

            // ── Per-hero equipment grids ──────────────────────────────────
            for (hero_idx, member) in class_heroes.iter().enumerate() {
                let eq = resolve_hero_equipment(&member.id, &member.class_id, campaign, content);
                root.spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        border: UiRect::all(Val::Px(1.0)),
                        padding: UiRect::all(Val::Px(8.0)),
                        row_gap: Val::Px(6.0),
                        ..default()
                    },
                    BorderColor::all(Color::srgb(0.35, 0.30, 0.25)),
                ))
                .with_children(|hero_panel| {
                    // Hero name label
                    hero_panel.spawn((
                        Text::new(member.name.clone()),
                        TextFont {
                            font: font.clone(),
                            font_size: 16.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.9, 0.85, 0.6)),
                    ));

                    // Refinement 3: slot column headers above the FIRST hero only.
                    if hero_idx == 0 {
                        const SLOT_HEADERS: &[&str] = &["Прав.", "Лев.", "Грудь", "Ноги", "Стопы"];
                        hero_panel
                            .spawn(Node {
                                flex_direction: FlexDirection::Row,
                                column_gap: Val::Px(CELL_GAP),
                                ..default()
                            })
                            .with_children(|header_row| {
                                for &label in SLOT_HEADERS {
                                    header_row
                                        .spawn(Node {
                                            width: Val::Px(CELL_SIZE),
                                            justify_content: JustifyContent::Center,
                                            align_items: AlignItems::Center,
                                            ..default()
                                        })
                                        .with_children(|cell_wrapper| {
                                            cell_wrapper.spawn((
                                                Text::new(label),
                                                TextFont {
                                                    font: font.clone(),
                                                    font_size: 10.0,
                                                    ..default()
                                                },
                                                TextColor(Color::srgba(0.6, 0.6, 0.6, 0.7)),
                                            ));
                                        });
                                }
                            });
                    }

                    // 5 slot cells in a row
                    hero_panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(CELL_GAP),
                            ..default()
                        })
                        .with_children(|slots_row| {
                            let id = member.id.clone();

                            let slots = [
                                (
                                    EquipSlot::MainHand,
                                    eq.main_hand.as_ref().map(|w| ItemRef::Weapon(w.clone())),
                                ),
                                (
                                    EquipSlot::OffHand,
                                    eq.off_hand.as_ref().map(|w| ItemRef::Weapon(w.clone())),
                                ),
                                (EquipSlot::Chest, Some(ItemRef::Armor(eq.chest.clone()))),
                                (EquipSlot::Legs, Some(ItemRef::Armor(eq.legs.clone()))),
                                (EquipSlot::Feet, Some(ItemRef::Armor(eq.feet.clone()))),
                            ];

                            for (slot, maybe_item) in slots {
                                let label = match &maybe_item {
                                    Some(item) => item_abbrev(item, content),
                                    None => "—".into(),
                                };
                                let icon = maybe_item
                                    .as_ref()
                                    .and_then(|it| item_image_path(it, content))
                                    .map(|p| asset_server.load(format!("images/{p}")));
                                let cell_kind = CellKind::Equip {
                                    hero_id: id.clone(),
                                    slot: slot.clone(),
                                };
                                let style = cell_style(
                                    &cell_kind,
                                    selection,
                                    campaign,
                                    db,
                                    scenario_state,
                                    content,
                                );
                                let mut ec =
                                    spawn_cell(slots_row, font.clone(), label, icon, style);
                                ec.insert(EquipCell {
                                    hero_id: id.clone(),
                                    slot,
                                });
                                // Re-insert SelectedCellMarker so camp_interaction_system can find it.
                                if style == CellStyle::Selected {
                                    ec.insert(SelectedCellMarker);
                                }
                            }
                        });
                });
            }

            // ── Backpack (stash) grid ─────────────────────────────────────
            root.spawn((
                Text::new("Рюкзак"),
                TextFont {
                    font: font.clone(),
                    font_size: 14.0,
                    ..default()
                },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
            ));

            if campaign.stash.is_empty() {
                root.spawn((
                    Text::new("— пусто —"),
                    TextFont {
                        font: font.clone(),
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.4, 0.4, 0.4)),
                ));
            } else {
                root.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(CELL_GAP),
                    row_gap: Val::Px(CELL_GAP),
                    max_width: Val::Px((CELL_SIZE + CELL_GAP) * 6.0),
                    ..default()
                })
                .with_children(|pack_grid| {
                    for (i, item) in campaign.stash.iter().enumerate() {
                        let label = item_abbrev(item, content);
                        let icon = item_image_path(item, content)
                            .map(|p| asset_server.load(format!("images/{p}")));
                        let cell_kind = CellKind::Backpack { index: i };
                        let style = cell_style(
                            &cell_kind,
                            selection,
                            campaign,
                            db,
                            scenario_state,
                            content,
                        );
                        let mut ec = spawn_cell(pack_grid, font.clone(), label, icon, style);
                        ec.insert(BackpackCell { index: i });
                        if style == CellStyle::Selected {
                            ec.insert(SelectedCellMarker);
                        }
                    }
                });
            }

            // ── Stat comparison card (fixed top-right, starts hidden) ────
            root.spawn((
                ComparisonCard,
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(24.0),
                    top: Val::Px(24.0),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(10.0)),
                    row_gap: Val::Px(4.0),
                    min_width: Val::Px(200.0),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.08, 0.08, 0.14, 0.95)),
                BorderColor::all(Color::srgb(0.45, 0.40, 0.30)),
                Visibility::Hidden,
            ));

            // ── Continue button ───────────────────────────────────────────
            spawn_standard_button(
                root,
                font.clone(),
                "Продолжить",
                Val::Px(200.0),
                Val::Auto,
                ButtonStyle::Default,
            )
            .insert(CampContinueButton);
        });
}

// ── Setup ─────────────────────────────────────────────────────────────────────

/// Spawned on `OnEnter(AppState::Camp)`.
pub fn setup_camp_screen(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
    campaign: Option<Res<CampaignState>>,
    active_content: Res<ActiveContent>,
) {
    let Some(campaign) = campaign else {
        warn!("camp: entered AppState::Camp without CampaignState");
        return;
    };

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    commands.init_resource::<CampEquipSelection>();
    commands.init_resource::<CampNeedsRebuild>();
    // On first entry the selection is always empty, so pass the default.
    let selection = CampEquipSelection::default();
    spawn_camp_ui(
        &mut commands,
        font,
        &asset_server,
        &db,
        &scenario_state,
        &campaign,
        &active_content.0,
        &selection,
    );
}

// ── Rebuild system ────────────────────────────────────────────────────────────

/// After a swap or selection change, rebuilds the camp UI in-place (despawn + respawn).
/// Runs after `camp_interaction_system` under `run_if(in_state(Camp))`.
#[allow(clippy::too_many_arguments)]
pub fn camp_rebuild_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
    campaign: Option<Res<CampaignState>>,
    active_content: Res<ActiveContent>,
    selection: Res<CampEquipSelection>,
    roots: Query<Entity, With<CampScreenRoot>>,
    mut rebuild: ResMut<CampNeedsRebuild>,
) {
    if !rebuild.0 {
        return;
    }
    rebuild.0 = false;

    // Despawn old UI.
    for entity in &roots {
        commands.entity(entity).despawn();
    }

    let Some(campaign) = campaign else { return };
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    spawn_camp_ui(
        &mut commands,
        font,
        &asset_server,
        &db,
        &scenario_state,
        &campaign,
        &active_content.0,
        &selection,
    );
}

// ── Input ─────────────────────────────────────────────────────────────────────

/// Handles cell selection, swap attempts, and Continue.
///
/// Interaction pattern (mirrors `main_menu_ui`):
/// - Query `Changed<Interaction>`, check `*i == Interaction::Pressed`.
/// - First press → select cell; triggers rebuild to show dimming.
/// - Second press → attempt swap, then rebuild + clear selection.
/// - Press already-selected cell → deselect; triggers rebuild to clear dimming.
/// - EquipCell ↔ EquipCell → re-select the new cell (equip↔equip not supported
///   this pass; see module doc); triggers rebuild.
#[allow(clippy::too_many_arguments)]
pub fn camp_interaction_system(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    equip_cells: Query<(Entity, &Interaction, &EquipCell), Changed<Interaction>>,
    backpack_cells: Query<(Entity, &Interaction, &BackpackCell), Changed<Interaction>>,
    continue_buttons: Query<&Interaction, (Changed<Interaction>, With<CampContinueButton>)>,
    selected_entities: Query<Entity, With<SelectedCellMarker>>,
    mut borders: Query<&mut BorderColor>,
    mut backgrounds: Query<&mut BackgroundColor>,
    mut selection: ResMut<CampEquipSelection>,
    mut campaign: Option<ResMut<CampaignState>>,
    active_content: Res<ActiveContent>,
    mut next_state: ResMut<NextState<AppState>>,
    mut rebuild: ResMut<CampNeedsRebuild>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
) {
    // ── Continue ──────────────────────────────────────────────────────────
    let key_continue = keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter);
    let btn_continue = continue_buttons.iter().any(|i| *i == Interaction::Pressed);
    if key_continue || btn_continue {
        next_state.set(AppState::Story);
        return;
    }

    // ── Collect pressed cells ─────────────────────────────────────────────
    let pressed_equip: Option<(Entity, EquipCell)> = equip_cells
        .iter()
        .find(|(_, i, _)| **i == Interaction::Pressed)
        .map(|(e, _, c)| (e, c.clone()));

    let pressed_backpack: Option<(Entity, BackpackCell)> = backpack_cells
        .iter()
        .find(|(_, i, _)| **i == Interaction::Pressed)
        .map(|(e, _, c)| (e, c.clone()));

    // At most one press per frame (Button guarantees this, but defensive).
    let pressed: Option<(Entity, CellKind)> = match (pressed_equip, pressed_backpack) {
        (Some((e, c)), None) => Some((
            e,
            CellKind::Equip {
                hero_id: c.hero_id,
                slot: c.slot,
            },
        )),
        (None, Some((e, c))) => Some((e, CellKind::Backpack { index: c.index })),
        _ => None,
    };

    let Some((_pressed_entity, pressed_kind)) = pressed else {
        return;
    };

    // ── Helper: set highlight on an entity ───────────────────────────────
    let highlight_entity =
        |entity: Entity,
         selected: bool,
         borders: &mut Query<&mut BorderColor>,
         backgrounds: &mut Query<&mut BackgroundColor>| {
            if let Ok(mut bc) = borders.get_mut(entity) {
                *bc = BorderColor::all(if selected {
                    CELL_SELECTED_BORDER
                } else {
                    CELL_IDLE_BORDER
                });
            }
            if let Ok(mut bg) = backgrounds.get_mut(entity) {
                *bg = BackgroundColor(if selected {
                    CELL_SELECTED_BG
                } else {
                    CELL_IDLE_BG
                });
            }
        };

    // ── De-select helper (remove marker + reset visuals) ─────────────────
    let clear_selection =
        |commands: &mut Commands,
         selected_entities: &Query<Entity, With<SelectedCellMarker>>,
         borders: &mut Query<&mut BorderColor>,
         backgrounds: &mut Query<&mut BackgroundColor>| {
            for e in selected_entities.iter() {
                highlight_entity(e, false, borders, backgrounds);
                commands.entity(e).remove::<SelectedCellMarker>();
            }
        };

    // ── Case: no selection yet — select pressed cell ──────────────────────
    let Some(current_selection) = selection.selected.clone() else {
        selection.selected = Some(pressed_kind);
        // Trigger rebuild so all other cells get dimmed/activated.
        rebuild.0 = true;
        return;
    };

    // ── Case: pressed the already-selected cell — deselect ───────────────
    if current_selection == pressed_kind {
        selection.selected = None;
        clear_selection(
            &mut commands,
            &selected_entities,
            &mut borders,
            &mut backgrounds,
        );
        // Trigger rebuild to remove dimming.
        rebuild.0 = true;
        return;
    }

    // ── Case: second cell pressed — attempt swap ──────────────────────────
    match (&current_selection, &pressed_kind) {
        // EquipCell ↔ EquipCell: not supported; re-select the new cell.
        (CellKind::Equip { .. }, CellKind::Equip { .. }) => {
            clear_selection(
                &mut commands,
                &selected_entities,
                &mut borders,
                &mut backgrounds,
            );
            selection.selected = Some(pressed_kind);
            // Trigger rebuild to update dimming for the new selection.
            rebuild.0 = true;
        }

        // BackpackCell ↔ BackpackCell: swap positions in stash.
        (CellKind::Backpack { index: idx_a }, CellKind::Backpack { index: idx_b }) => {
            let (idx_a, idx_b) = (*idx_a, *idx_b);
            if let Some(ref mut camp) = campaign {
                if idx_a < camp.stash.len() && idx_b < camp.stash.len() {
                    camp.stash.swap(idx_a, idx_b);
                }
            }
            selection.selected = None;
            clear_selection(
                &mut commands,
                &selected_entities,
                &mut borders,
                &mut backgrounds,
            );
            rebuild.0 = true;
        }

        // EquipCell ↔ BackpackCell (either order): try_equip + true swap.
        (first, second) => {
            let (equip_kind, backpack_idx) = match (first, second) {
                (CellKind::Equip { hero_id, slot }, CellKind::Backpack { index }) => (
                    CellKind::Equip {
                        hero_id: hero_id.clone(),
                        slot: slot.clone(),
                    },
                    *index,
                ),
                (CellKind::Backpack { index }, CellKind::Equip { hero_id, slot }) => (
                    CellKind::Equip {
                        hero_id: hero_id.clone(),
                        slot: slot.clone(),
                    },
                    *index,
                ),
                _ => unreachable!(),
            };

            let CellKind::Equip { hero_id, slot } = equip_kind else {
                unreachable!()
            };

            let Some(ref mut camp) = campaign else {
                selection.selected = None;
                clear_selection(
                    &mut commands,
                    &selected_entities,
                    &mut borders,
                    &mut backgrounds,
                );
                rebuild.0 = true;
                return;
            };

            let item = match camp.stash.get(backpack_idx) {
                Some(i) => i.clone(),
                None => {
                    // Stash index stale; clear and bail.
                    selection.selected = None;
                    clear_selection(
                        &mut commands,
                        &selected_entities,
                        &mut borders,
                        &mut backgrounds,
                    );
                    rebuild.0 = true;
                    return;
                }
            };

            // Resolve hero class for equipment snapshot.
            let scen = db.scenarios.get(&scenario_state.scenario_id).unwrap();
            let party = active_party(scen, scenario_state.scene_index);
            let class_id = party
                .iter()
                .find(|m| m.id == hero_id)
                .map(|m| m.class_id.clone())
                .unwrap_or_default();
            let content = &active_content.0;
            let current_save = resolve_hero_equipment(&hero_id, &class_id, camp, content);

            let proficient = hero_can_wear(&class_id, &item, content);
            match try_equip(
                &current_save,
                &slot,
                item.clone(),
                &content.weapons,
                &content.armor,
            ) {
                Ok(result) if proficient => {
                    // ── True swap: backpack item → equip slot, displaced → same backpack cell ──

                    // Two-handed weapon: the off-hand that gets cleared also returns
                    // to the stash.  We push it first so it goes to the END; then we
                    // place the displaced item (old main-hand) into the backpack slot.
                    if let ItemRef::Weapon(wid) = &item {
                        if let Some(def) = content.weapons.get(wid) {
                            if def.hand == HandType::TwoHanded {
                                if let Some(old_oh) = current_save.off_hand.as_ref() {
                                    camp.stash.push(ItemRef::Weapon(old_oh.clone()));
                                }
                            }
                        }
                    }

                    // Replace the backpack cell with the displaced item (if any),
                    // or remove it if the equip slot was empty (hand slot, None displaced).
                    let is_empty_sentinel =
                        |d: &ItemRef| matches!(d, ItemRef::Armor(aid) if aid.0.is_empty());
                    match result.displaced {
                        Some(displaced) if !is_empty_sentinel(&displaced) => {
                            // True swap: put the old equipped item into the same stash cell.
                            camp.stash[backpack_idx] = displaced;
                        }
                        _ => {
                            // Slot was empty (hand slot, no previous item) — just remove
                            // the stash entry.
                            camp.stash.remove(backpack_idx);
                        }
                    }

                    // Write updated loadout.
                    camp.loadouts.insert(hero_id.clone(), result.new_save);

                    // Clear selection and rebuild.
                    selection.selected = None;
                    clear_selection(
                        &mut commands,
                        &selected_entities,
                        &mut borders,
                        &mut backgrounds,
                    );
                    rebuild.0 = true;
                }
                Ok(_) => {
                    warn!("camp: '{}' lacks armor proficiency for {:?}", hero_id, item);
                    selection.selected = None;
                    clear_selection(
                        &mut commands,
                        &selected_entities,
                        &mut borders,
                        &mut backgrounds,
                    );
                    rebuild.0 = true;
                }
                Err(e) => {
                    warn!("camp: equip rejected for hero '{}': {:?}", hero_id, e);
                    // Reject: clear selection and rebuild (removes dimming).
                    selection.selected = None;
                    clear_selection(
                        &mut commands,
                        &selected_entities,
                        &mut borders,
                        &mut backgrounds,
                    );
                    rebuild.0 = true;
                }
            }
        }
    }
}

// ── Cleanup ───────────────────────────────────────────────────────────────────

/// Despawns all camp UI entities on `OnExit(AppState::Camp)`.
/// Persists `CampaignState` via autosave.
pub fn cleanup_camp_screen(
    mut commands: Commands,
    roots: Query<Entity, With<CampScreenRoot>>,
    campaign: Option<Res<CampaignState>>,
    scenario_state: Res<ScenarioState>,
    paths: Option<Res<PersistencePaths>>,
    settings: Res<GameSettings>,
) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
    commands.remove_resource::<CampEquipSelection>();
    commands.remove_resource::<CampNeedsRebuild>();

    // Persist loadouts + stash on camp exit (only if campaign present).
    if let Some(camp) = campaign {
        if let Some(p) = paths.as_deref() {
            if let Err(e) = save_repo::record_progress(
                &p.0,
                settings.current_slot,
                &camp,
                &scenario_state.scenario_id,
                scenario_state.scene_index,
            ) {
                warn!("camp: autosave failed: {e}");
            }
        }
    }
}

/// Updates the stat-comparison card each frame while in `AppState::Camp`.
///
/// Card visibility rules:
/// - Nothing selected and nothing hovered (or hovered cell has no item) → hidden.
/// - Nothing selected but hovering a cell with an item → single-column card
///   showing the hovered item's stats (header "Предмет").
/// - Selection active + hovering a **compatible** cell that has an item →
///   two-column comparison (Выбрано / Наведено).
/// - Selection active, but no hover / hovering the selected cell / hovering an
///   incompatible or empty cell → single-column card showing the selected item
///   stats (header "Выбрано").
///
/// The card is a fixed top-right panel (absolute position, never overlaps the
/// grids), so it cannot intercept pointer events — the cell buttons always
/// receive `Interaction::Pressed` correctly.
///
/// Uses a `Local` to avoid rebuilding the card children every frame.
/// The key is `(anchor_kind, compare_hovered, has_selection)`.
#[allow(clippy::too_many_arguments)]
pub fn camp_comparison_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    selection: Res<CampEquipSelection>,
    campaign: Option<Res<CampaignState>>,
    active_content: Res<ActiveContent>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
    equip_cells: Query<(&Interaction, &EquipCell)>,
    backpack_cells: Query<(&Interaction, &BackpackCell)>,
    mut cards: Query<(Entity, &mut Visibility), With<ComparisonCard>>,
    mut last: Local<Option<(CellKind, Option<CellKind>, bool)>>,
) {
    let Ok((card_entity, mut card_vis)) = cards.single_mut() else {
        return;
    };

    let Some(campaign) = campaign else {
        *card_vis = Visibility::Hidden;
        *last = None;
        return;
    };
    let content = &active_content.0;

    // Find the single hovered cell (if any).
    let hovered_kind: Option<CellKind> = {
        let from_equip = equip_cells
            .iter()
            .find(|(i, _)| **i == Interaction::Hovered)
            .map(|(_, c)| CellKind::Equip {
                hero_id: c.hero_id.clone(),
                slot: c.slot.clone(),
            });
        let from_backpack = backpack_cells
            .iter()
            .find(|(i, _)| **i == Interaction::Hovered)
            .map(|(_, c)| CellKind::Backpack { index: c.index });
        from_equip.or(from_backpack)
    };

    let has_selection = selection.selected.is_some();

    // Determine anchor: selected item if present, else hovered item with an item.
    let anchor_kind: Option<CellKind> = selection.selected.clone().or_else(|| {
        hovered_kind.as_ref().and_then(|hk| {
            cell_item(hk, &campaign, &db, &scenario_state, content)?;
            Some(hk.clone())
        })
    });

    let Some(anchor_kind) = anchor_kind else {
        *card_vis = Visibility::Hidden;
        *last = None;
        return;
    };

    // Determine if the hovered cell qualifies for two-column comparison.
    // Only possible when a selection is active.
    let compare_hovered: Option<CellKind> = if has_selection {
        let selected_kind = selection.selected.as_ref().unwrap();
        hovered_kind.and_then(|hk| {
            if hk == *selected_kind {
                return None; // hovering the selected cell — single-column mode
            }
            if !cell_compatible(selected_kind, &hk, &campaign, &db, &scenario_state, content) {
                return None; // incompatible cell — single-column mode
            }
            // Must have an item to compare against.
            cell_item(&hk, &campaign, &db, &scenario_state, content)?;
            Some(hk)
        })
    } else {
        None
    };

    // The card key: (anchor_kind, compare_hovered, has_selection).
    let new_key = (anchor_kind.clone(), compare_hovered.clone(), has_selection);

    // Only rebuild children when the key changes.
    if *last == Some(new_key.clone()) {
        *card_vis = Visibility::Inherited;
        return;
    }

    // Resolve anchor item (required — if missing, hide).
    let Some(anchor_item) = cell_item(&anchor_kind, &campaign, &db, &scenario_state, content)
    else {
        *card_vis = Visibility::Hidden;
        *last = None;
        return;
    };

    *last = Some(new_key);
    *card_vis = Visibility::Inherited;

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    let anchor_name = match &anchor_item {
        ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
        ItemRef::Armor(aid) => armor_name(aid, content).to_string(),
    };

    commands.entity(card_entity).despawn_related::<Children>();
    commands.entity(card_entity).with_children(|card| {
        if let Some(hov_kind) = compare_hovered {
            // ── Two-column comparison mode ───────────────────────────────
            // Orientation is role-based, not click-order-based: the worn item is
            // always the left/baseline column, the backpack candidate the right
            // column, so the delta always reads as "change for the hero".
            // `cell_compatible` only ever pairs an Equip cell with a Backpack
            // cell, so exactly one of {selected, hovered} is each kind.
            let selected_kind = selection.selected.as_ref().unwrap();
            let sel_item = cell_item(selected_kind, &campaign, &db, &scenario_state, content)
                .expect("selected item already validated above");
            let sel_name = match &sel_item {
                ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
                ItemRef::Armor(aid) => armor_name(aid, content).to_string(),
            };
            let hov_item = cell_item(&hov_kind, &campaign, &db, &scenario_state, content)
                .expect("hov item already validated above");
            let hov_name = match &hov_item {
                ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
                ItemRef::Armor(aid) => armor_name(aid, content).to_string(),
            };

            let (worn_item, new_item) = orient_comparison(selected_kind, &sel_item, &hov_item);
            let (worn_name, new_name) = orient_comparison(selected_kind, &sel_name, &hov_name);

            let rows = compare_items(worn_item, new_item, &content.weapons, &content.armor);

            // Header row: "<надето> / <новое>" (worn on the left, candidate right).
            card.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|row| {
                row.spawn((
                    Text::new(worn_name.clone()),
                    TextFont {
                        font: font.clone(),
                        font_size: 12.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.95, 0.85, 0.5)),
                ));
                row.spawn((
                    Text::new(" / "),
                    TextFont {
                        font: font.clone(),
                        font_size: 12.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.6, 0.6, 0.6)),
                ));
                row.spawn((
                    Text::new(new_name.clone()),
                    TextFont {
                        font: font.clone(),
                        font_size: 12.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.5, 0.85, 0.95)),
                ));
            });

            // Armor-class row (categorical, no numeric delta). Only for armor —
            // weapons have no weight class. Both columns are the same item kind
            // (cell_compatible never offers weapon↔armor), so both resolve.
            if let (Some(worn_w), Some(new_w)) = (
                item_weight(worn_item, content),
                item_weight(new_item, content),
            ) {
                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Text::new(format!("{:<10}", "Класс")),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.75, 0.75, 0.75)),
                    ));
                    row.spawn((
                        Text::new(armor_weight_label(worn_w)),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.85, 0.5)),
                    ));
                    row.spawn((
                        Text::new(" → "),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                    ));
                    row.spawn((
                        Text::new(armor_weight_label(new_w)),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.5, 0.85, 0.95)),
                    ));
                });
            }

            // Stat rows: "<current> → <new> = <coloured delta>". Damage shows
            // dice notation + expected value (1 decimal); other stats are
            // integers. Delta is always incoming − equipped.
            for row_data in &rows {
                let is_damage = row_data.label == "Урон";
                let delta = row_data.incoming_val - row_data.equipped_val;
                let (cur_str, new_str, delta_str) = if is_damage {
                    (
                        weapon_damage_label(worn_item, content)
                            .unwrap_or_else(|| format!("{:.1}", row_data.equipped_val)),
                        weapon_damage_label(new_item, content)
                            .unwrap_or_else(|| format!("{:.1}", row_data.incoming_val)),
                        format!("{delta:+.1}"),
                    )
                } else {
                    (
                        format!("{:.0}", row_data.equipped_val),
                        format!("{:.0}", row_data.incoming_val),
                        format!("{delta:+.0}"),
                    )
                };

                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Text::new(format!("{:<10}", row_data.label)),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.75, 0.75, 0.75)),
                    ));
                    row.spawn((
                        Text::new(cur_str),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.85, 0.5)),
                    ));
                    row.spawn((
                        Text::new(" → "),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                    ));
                    row.spawn((
                        Text::new(new_str),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.5, 0.85, 0.95)),
                    ));
                    row.spawn((
                        Text::new(" = "),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                    ));
                    row.spawn((
                        Text::new(delta_str),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(delta_color(delta)),
                    ));
                });
            }

            if rows.is_empty() {
                card.spawn((
                    Text::new("Нет статов"),
                    TextFont {
                        font,
                        font_size: 11.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.5, 0.5, 0.5)),
                ));
            }
        } else {
            // ── Single-column mode: show anchor item's stats only ────────
            // Header label: "Выбрано" when item is selected, "Предмет" when hover-only.
            let header_label = if has_selection {
                "Выбрано"
            } else {
                "Предмет"
            };
            card.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|row| {
                row.spawn((
                    Text::new(header_label),
                    TextFont {
                        font: font.clone(),
                        font_size: 11.0,
                        ..default()
                    },
                    TextColor(Color::srgba(0.7, 0.7, 0.7, 0.8)),
                ));
                row.spawn((
                    Text::new(format!(" {anchor_name}")),
                    TextFont {
                        font: font.clone(),
                        font_size: 12.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.95, 0.85, 0.5)),
                ));
            });

            // Armor-class line (armor only — weapons have no weight class).
            if let Some(w) = item_weight(&anchor_item, content) {
                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Text::new(format!("{:<10}", "Класс")),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.75, 0.75, 0.75)),
                    ));
                    row.spawn((
                        Text::new(armor_weight_label(w)),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.85, 0.5)),
                    ));
                });
            }

            let stats = item_stats(&anchor_item, &content.weapons, &content.armor);
            for (label, val) in &stats {
                // Damage shows dice notation + expected value; other stats integer.
                let value_str = if label == "Урон" {
                    weapon_damage_label(&anchor_item, content)
                        .unwrap_or_else(|| format!("{val:.1}"))
                } else {
                    format!("{val:.0}")
                };
                card.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Text::new(format!("{:<10}", label)),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.75, 0.75, 0.75)),
                    ));
                    row.spawn((
                        Text::new(value_str),
                        TextFont {
                            font: font.clone(),
                            font_size: 11.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.85, 0.5)),
                    ));
                });
            }

            if stats.is_empty() {
                card.spawn((
                    Text::new("Нет статов"),
                    TextFont {
                        font,
                        font_size: 11.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.5, 0.5, 0.5)),
                ));
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::armor::{ArmorDef, ArmorSlot, ArmorWeight};
    use crate::content::weapons::{HandType, WeaponDef};
    use combat_engine::{ArmorId, DiceExpr, WeaponId};
    use std::collections::HashMap;
    use toml;

    // ── Test content builders ────────────────────────────────────────────────

    fn make_weapon(id: &str, hand: HandType) -> (WeaponId, WeaponDef) {
        let wid = WeaponId::from(id);
        let def = WeaponDef {
            id: wid.clone(),
            name: id.to_string(),
            hand,
            dice: DiceExpr {
                count: 1,
                sides: 6,
                bonus: 0,
            },
            ranged: false,
            spell_power: 0,
            image: None,
            stats: Default::default(),
        };
        (wid, def)
    }

    fn make_armor(id: &str, slot: ArmorSlot) -> (ArmorId, ArmorDef) {
        let aid = ArmorId::from(id);
        let def = ArmorDef {
            id: aid.clone(),
            name: id.to_string(),
            slot,
            weight: ArmorWeight::Light,
            image: None,
            stats: crate::content::item_stats::ItemStats {
                armor: 1,
                ..Default::default()
            },
        };
        (aid, def)
    }

    fn content_with_items() -> (HashMap<WeaponId, WeaponDef>, HashMap<ArmorId, ArmorDef>) {
        let mut weapons = HashMap::new();
        let mut armor = HashMap::new();
        for (id, def) in [
            make_weapon("mh_sword", HandType::MainHand),
            make_weapon("oh_dagger", HandType::OffHand),
            make_weapon("2h_axe", HandType::TwoHanded),
        ] {
            weapons.insert(id, def);
        }
        for (id, def) in [
            make_armor("chest_plate", ArmorSlot::Chest),
            make_armor("legs_plate", ArmorSlot::Legs),
            make_armor("feet_boots", ArmorSlot::Feet),
        ] {
            armor.insert(id, def);
        }
        (weapons, armor)
    }

    fn base_save() -> EquipmentSave {
        EquipmentSave {
            main_hand: Some(WeaponId::from("mh_sword")),
            off_hand: Some(WeaponId::from("oh_dagger")),
            chest: ArmorId::from("chest_plate"),
            legs: ArmorId::from("legs_plate"),
            feet: ArmorId::from("feet_boots"),
        }
    }

    // ── try_equip truth table ────────────────────────────────────────────────

    /// Main-hand weapon into main-hand slot: succeeds, displaced = old main-hand.
    #[test]
    fn equip_main_hand_into_main_hand() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(),
            &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("mh_sword")),
            &w,
            &a,
        )
        .unwrap();
        assert_eq!(result.new_save.main_hand, Some(WeaponId::from("mh_sword")));
        assert_eq!(
            result.displaced,
            Some(ItemRef::Weapon(WeaponId::from("mh_sword")))
        );
    }

    /// Off-hand weapon into off-hand slot: succeeds.
    #[test]
    fn equip_off_hand_into_off_hand() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(),
            &EquipSlot::OffHand,
            ItemRef::Weapon(WeaponId::from("oh_dagger")),
            &w,
            &a,
        )
        .unwrap();
        assert_eq!(result.new_save.off_hand, Some(WeaponId::from("oh_dagger")));
    }

    /// Off-hand weapon into main-hand slot: rejected.
    #[test]
    fn equip_off_hand_into_main_hand_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(),
            &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("oh_dagger")),
            &w,
            &a,
        )
        .unwrap_err();
        assert_eq!(err, EquipError::OffHandIntoMainHand);
    }

    /// Two-handed weapon into main-hand: succeeds and clears off_hand in new save.
    #[test]
    fn equip_two_handed_clears_off_hand() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(),
            &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("2h_axe")),
            &w,
            &a,
        )
        .unwrap();
        assert_eq!(result.new_save.main_hand, Some(WeaponId::from("2h_axe")));
        assert_eq!(
            result.new_save.off_hand, None,
            "off_hand cleared by two-handed"
        );
    }

    /// Two-handed weapon into off-hand: rejected.
    #[test]
    fn equip_two_handed_into_off_hand_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(),
            &EquipSlot::OffHand,
            ItemRef::Weapon(WeaponId::from("2h_axe")),
            &w,
            &a,
        )
        .unwrap_err();
        assert_eq!(err, EquipError::WeaponIntoArmorSlot);
    }

    /// Weapon into armor slot: rejected.
    #[test]
    fn equip_weapon_into_armor_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(),
            &EquipSlot::Chest,
            ItemRef::Weapon(WeaponId::from("mh_sword")),
            &w,
            &a,
        )
        .unwrap_err();
        assert_eq!(err, EquipError::WeaponIntoArmorSlot);
    }

    /// Armor into correct slot: succeeds, displaced = old armor.
    #[test]
    fn equip_armor_into_correct_slot() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(),
            &EquipSlot::Chest,
            ItemRef::Armor(ArmorId::from("chest_plate")),
            &w,
            &a,
        )
        .unwrap();
        assert_eq!(result.new_save.chest, ArmorId::from("chest_plate"));
        assert_eq!(
            result.displaced,
            Some(ItemRef::Armor(ArmorId::from("chest_plate")))
        );
    }

    /// Armor into wrong armor slot: rejected with ArmorSlotMismatch.
    #[test]
    fn equip_armor_into_wrong_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(),
            &EquipSlot::Legs,
            ItemRef::Armor(ArmorId::from("chest_plate")),
            &w,
            &a,
        )
        .unwrap_err();
        assert!(matches!(err, EquipError::ArmorSlotMismatch { .. }));
    }

    /// Armor into hand slot: rejected.
    #[test]
    fn equip_armor_into_hand_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(),
            &EquipSlot::MainHand,
            ItemRef::Armor(ArmorId::from("chest_plate")),
            &w,
            &a,
        )
        .unwrap_err();
        assert_eq!(err, EquipError::ArmorIntoHandSlot);
    }

    // ── Equip flow: stash ↔ loadout (pure) ──────────────────────────────────

    /// Equip a new chest from stash: item leaves stash, displaced (old chest) goes back.
    #[test]
    fn equip_flow_stash_and_loadout() {
        let (w, a) = content_with_items();
        let save = base_save();
        let result = try_equip(
            &save,
            &EquipSlot::Chest,
            ItemRef::Armor(ArmorId::from("chest_plate")),
            &w,
            &a,
        )
        .unwrap();
        assert_eq!(result.new_save.chest, ArmorId::from("chest_plate"));
        // Old chest_plate is displaced (same item in this test).
        assert_eq!(
            result.displaced,
            Some(ItemRef::Armor(ArmorId::from("chest_plate")))
        );
    }

    // ── Swap resolution: EquipCell ↔ BackpackCell ────────────────────────────

    /// Swapping a backpack armor item into its correct equip slot puts the old
    /// equipped item into the same backpack index — no loss, no dupe.
    #[test]
    fn swap_equip_backpack_true_swap() {
        let (w, a) = content_with_items();
        // Stash has a new chest plate; hero wears "chest_plate" already.
        let mut stash = [ItemRef::Armor(ArmorId::from("chest_plate"))];
        let stash: &mut [ItemRef] = &mut stash;
        let save = base_save(); // chest = "chest_plate"

        let result = try_equip(&save, &EquipSlot::Chest, stash[0].clone(), &w, &a).unwrap();

        // Simulate the true-swap: replace stash[0] with displaced.
        let displaced = result.displaced.unwrap();
        stash[0] = displaced.clone();

        assert_eq!(result.new_save.chest, ArmorId::from("chest_plate"));
        assert_eq!(stash.len(), 1, "no item created or destroyed");
        assert_eq!(stash[0], ItemRef::Armor(ArmorId::from("chest_plate")));
    }

    /// Swapping a backpack weapon into a hand slot where the slot was empty:
    /// backpack cell is removed (not replaced), no item lost.
    #[test]
    fn swap_equip_backpack_empty_slot_removes_cell() {
        let (w, a) = content_with_items();
        let save_no_oh = EquipmentSave {
            main_hand: Some(WeaponId::from("mh_sword")),
            off_hand: None,
            chest: ArmorId::from("chest_plate"),
            legs: ArmorId::from("legs_plate"),
            feet: ArmorId::from("feet_boots"),
        };
        let mut stash = vec![ItemRef::Weapon(WeaponId::from("oh_dagger"))];

        let result = try_equip(&save_no_oh, &EquipSlot::OffHand, stash[0].clone(), &w, &a).unwrap();

        // displaced = None (slot was empty); remove the stash cell.
        assert!(result.displaced.is_none());
        stash.remove(0);

        assert_eq!(result.new_save.off_hand, Some(WeaponId::from("oh_dagger")));
        assert!(stash.is_empty(), "stash cell removed when slot was empty");
    }

    /// Invalid swap (type mismatch) must not change the stash.
    #[test]
    fn swap_invalid_is_noop() {
        let (w, a) = content_with_items();
        let stash_before = [ItemRef::Armor(ArmorId::from("chest_plate"))];
        let save = base_save();

        // Armor into a hand slot — rejected.
        let err =
            try_equip(&save, &EquipSlot::MainHand, stash_before[0].clone(), &w, &a).unwrap_err();

        assert_eq!(err, EquipError::ArmorIntoHandSlot);
        // Caller does not modify stash on error — stash unchanged.
    }

    // ── should_enter_camp decision table ────────────────────────────────────

    use crate::content::scenarios::SceneDef;
    use crate::scenario::should_enter_camp;

    fn story_scene(no_camp: bool) -> SceneDef {
        SceneDef::Story {
            lines: vec![],
            party_add: vec![],
            party_remove: vec![],
            status_ops: vec![],
            requires_flag: None,
            no_camp,
        }
    }

    fn combat_scene() -> SceneDef {
        SceneDef::Combat {
            encounter_id: "enc".into(),
            location: None,
            on_victory_flags: vec![],
            requires_flag: None,
        }
    }

    /// Story → Story with campaign and no_camp=false → enter camp.
    #[test]
    fn should_enter_camp_story_to_story_with_campaign() {
        assert!(should_enter_camp(
            &story_scene(false),
            &story_scene(false),
            true
        ));
    }

    /// no_camp=true on the FROM scene → skip camp.
    #[test]
    fn should_enter_camp_no_camp_true_skips() {
        assert!(!should_enter_camp(
            &story_scene(true),
            &story_scene(false),
            true
        ));
    }

    /// No CampaignState → skip camp.
    #[test]
    fn should_enter_camp_no_campaign_skips() {
        assert!(!should_enter_camp(
            &story_scene(false),
            &story_scene(false),
            false
        ));
    }

    /// Story → Combat → skip camp.
    #[test]
    fn should_enter_camp_story_to_combat_skips() {
        assert!(!should_enter_camp(
            &story_scene(false),
            &combat_scene(),
            true
        ));
    }

    /// Combat → Story → skip camp (only Story→Story qualifies).
    #[test]
    fn should_enter_camp_combat_to_story_skips() {
        assert!(!should_enter_camp(
            &combat_scene(),
            &story_scene(false),
            true
        ));
    }

    // ── compare_items ────────────────────────────────────────────────────────

    /// Helper: weapon with specific stats.
    fn make_weapon_stats(
        id: &str,
        hand: HandType,
        dice: DiceExpr,
        spell_power: i32,
        armor: i32,
        max_hp: i32,
    ) -> (WeaponId, WeaponDef) {
        let wid = WeaponId::from(id);
        let def = WeaponDef {
            id: wid.clone(),
            name: id.to_string(),
            hand,
            dice,
            ranged: false,
            spell_power,
            image: None,
            stats: crate::content::item_stats::ItemStats {
                armor,
                combat: crate::game::components::CombatStats {
                    max_hp,
                    ..Default::default()
                },
                mana: 0,
                magic_resist: 0,
            },
        };
        (wid, def)
    }

    /// Helper: armor with specific stats.
    fn make_armor_stats(
        id: &str,
        slot: ArmorSlot,
        armor_val: i32,
        max_hp: i32,
        strength: i32,
        weight: ArmorWeight,
    ) -> (ArmorId, ArmorDef) {
        let aid = ArmorId::from(id);
        let def = ArmorDef {
            id: aid.clone(),
            name: id.to_string(),
            slot,
            weight,
            image: None,
            stats: crate::content::item_stats::ItemStats {
                armor: armor_val,
                combat: crate::game::components::CombatStats {
                    max_hp,
                    strength,
                    ..Default::default()
                },
                mana: 0,
                magic_resist: 0,
            },
        };
        (aid, def)
    }

    /// Two weapons: compare produces "Урон" row with correct expected-damage values.
    #[test]
    fn compare_two_weapons_damage_row() {
        let mut weapons = HashMap::new();
        let dice_a = DiceExpr {
            count: 1,
            sides: 6,
            bonus: 0,
        }; // expected = 3.5
        let dice_b = DiceExpr {
            count: 2,
            sides: 4,
            bonus: 1,
        }; // expected = 6.0
        let (id_a, def_a) = make_weapon_stats("sword", HandType::MainHand, dice_a, 0, 0, 0);
        let (id_b, def_b) = make_weapon_stats("axe", HandType::MainHand, dice_b, 0, 0, 0);
        weapons.insert(id_a.clone(), def_a);
        weapons.insert(id_b.clone(), def_b);
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();

        let rows = compare_items(
            &ItemRef::Weapon(id_a),
            &ItemRef::Weapon(id_b),
            &weapons,
            &armor,
        );

        let dmg = rows
            .iter()
            .find(|r| r.label == "Урон")
            .expect("Урон row present");
        assert!(
            (dmg.equipped_val - 3.5).abs() < 0.01,
            "sword expected = 3.5"
        );
        assert!((dmg.incoming_val - 6.0).abs() < 0.01, "axe expected = 6.0");
    }

    /// Two armors: compare produces "Броня", "HP", "СИЛ" rows; no "Урон" row.
    #[test]
    fn compare_two_armors_stat_rows() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        // chest_a: armor=3, hp=10, str=0
        let (id_a, def_a) =
            make_armor_stats("chest_a", ArmorSlot::Chest, 3, 10, 0, ArmorWeight::Light);
        // chest_b: armor=5, hp=0, str=2
        let (id_b, def_b) =
            make_armor_stats("chest_b", ArmorSlot::Chest, 5, 0, 2, ArmorWeight::Light);
        armor.insert(id_a.clone(), def_a);
        armor.insert(id_b.clone(), def_b);

        let rows = compare_items(
            &ItemRef::Armor(id_a),
            &ItemRef::Armor(id_b),
            &weapons,
            &armor,
        );

        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"Броня"), "Броня row present");
        assert!(labels.contains(&"HP"), "HP row present");
        assert!(labels.contains(&"СИЛ"), "СИЛ row present");
        assert!(!labels.contains(&"Урон"), "no Урон row for armor");

        let bronya = rows.iter().find(|r| r.label == "Броня").unwrap();
        assert_eq!(bronya.equipped_val as i32, 3);
        assert_eq!(bronya.incoming_val as i32, 5);

        let hp = rows.iter().find(|r| r.label == "HP").unwrap();
        assert_eq!(hp.equipped_val as i32, 10);
        assert_eq!(hp.incoming_val as i32, 0);
    }

    /// When both items have all-zero stats (plain default weapon), compare returns empty.
    #[test]
    fn compare_zero_items_returns_empty_except_damage() {
        let mut weapons = HashMap::new();
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();
        // A weapon with 1d1+0 dice has expected = 1.0 (non-zero), so Урон row appears.
        // Use count=0 to get expected=0 (edge case: no dice).
        let wid = WeaponId::from("empty_w");
        let def = WeaponDef {
            id: wid.clone(),
            name: "empty_w".into(),
            hand: HandType::MainHand,
            dice: DiceExpr {
                count: 0,
                sides: 6,
                bonus: 0,
            },
            ranged: false,
            spell_power: 0,
            image: None,
            stats: Default::default(),
        };
        weapons.insert(wid.clone(), def);

        let rows = compare_items(
            &ItemRef::Weapon(wid.clone()),
            &ItemRef::Weapon(wid),
            &weapons,
            &armor,
        );
        assert!(rows.is_empty(), "all-zero stats → no rows");
    }

    // ── item_stats ────────────────────────────────────────────────────────────

    /// Weapon with non-zero damage returns only Урон row.
    #[test]
    fn item_stats_weapon_returns_damage() {
        let mut weapons = HashMap::new();
        let dice = DiceExpr {
            count: 2,
            sides: 6,
            bonus: 2,
        }; // expected = 9.0
        let (wid, wdef) = make_weapon_stats("sword", HandType::MainHand, dice, 0, 0, 0);
        weapons.insert(wid.clone(), wdef);
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();

        let stats = item_stats(&ItemRef::Weapon(wid), &weapons, &armor);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "Урон");
        assert!((stats[0].1 - 9.0).abs() < 0.01, "2d6+2 expected = 9.0");
    }

    /// Armor returns Броня and HP rows (non-zero), no Урон.
    #[test]
    fn item_stats_armor_returns_bronya_and_hp() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        let (aid, adef) = make_armor_stats("plate", ArmorSlot::Chest, 5, 20, 0, ArmorWeight::Light);
        armor.insert(aid.clone(), adef);

        let stats = item_stats(&ItemRef::Armor(aid), &weapons, &armor);
        let labels: Vec<&str> = stats.iter().map(|(l, _)| l.as_str()).collect();
        assert!(labels.contains(&"Броня"));
        assert!(labels.contains(&"HP"));
        assert!(!labels.contains(&"Урон"), "no damage for armor");
        let bronya = stats.iter().find(|(l, _)| l == "Броня").unwrap();
        assert_eq!(bronya.1 as i32, 5);
    }

    /// Unknown item id → empty stats (graceful miss).
    #[test]
    fn item_stats_unknown_item_returns_empty() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();
        let stats = item_stats(
            &ItemRef::Weapon(WeaponId::from("no_such")),
            &weapons,
            &armor,
        );
        assert!(stats.is_empty());
    }

    /// Armor with mana > 0 yields a "Мана" row; weapon never does.
    #[test]
    fn item_stats_armor_with_mana_yields_mana_row() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        let aid = ArmorId::from("mage_robe");
        let def = ArmorDef {
            id: aid.clone(),
            name: "mage_robe".into(),
            slot: ArmorSlot::Chest,
            weight: ArmorWeight::Light,
            image: None,
            stats: crate::content::item_stats::ItemStats {
                mana: 2,
                ..Default::default()
            },
        };
        armor.insert(aid.clone(), def);

        let stats = item_stats(&ItemRef::Armor(aid), &weapons, &armor);
        let labels: Vec<&str> = stats.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            labels.contains(&"Мана"),
            "armor with mana should show Мана row"
        );
        let mana_row = stats.iter().find(|(l, _)| l == "Мана").unwrap();
        assert!((mana_row.1 - 2.0).abs() < 0.01);
    }

    /// Armor with magic_resist > 0 yields a "Сопр. магии" row.
    #[test]
    fn item_stats_armor_with_magic_resist_yields_sopr_row() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        let aid = ArmorId::from("resist_robe");
        let def = ArmorDef {
            id: aid.clone(),
            name: "resist_robe".into(),
            slot: ArmorSlot::Chest,
            weight: ArmorWeight::Light,
            image: None,
            stats: crate::content::item_stats::ItemStats {
                magic_resist: 3,
                ..Default::default()
            },
        };
        armor.insert(aid.clone(), def);

        let stats = item_stats(&ItemRef::Armor(aid), &weapons, &armor);
        let labels: Vec<&str> = stats.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            labels.contains(&"Сопр. магии"),
            "armor with magic_resist should show Сопр. магии row"
        );
        let row = stats.iter().find(|(l, _)| l == "Сопр. магии").unwrap();
        assert!((row.1 - 3.0).abs() < 0.01);
    }

    /// Armor without magic_resist does NOT yield a "Сопр. магии" row (zero is omitted).
    #[test]
    fn item_stats_armor_without_magic_resist_omits_sopr_row() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        let (aid, adef) = make_armor("plain_chest", ArmorSlot::Chest);
        armor.insert(aid.clone(), adef);

        let stats = item_stats(&ItemRef::Armor(aid), &weapons, &armor);
        let labels: Vec<&str> = stats.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            !labels.contains(&"Сопр. магии"),
            "armor with magic_resist=0 must not show Сопр. магии row"
        );
    }

    /// Weapon never yields a "Мана" row even when mana was the caller's intent.
    #[test]
    fn item_stats_weapon_has_no_mana_row() {
        let dice = DiceExpr {
            count: 1,
            sides: 6,
            bonus: 0,
        };
        let mut weapons = HashMap::new();
        let (wid, wdef) = make_weapon_stats("staff", HandType::MainHand, dice, 0, 0, 0);
        weapons.insert(wid.clone(), wdef);
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();

        let stats = item_stats(&ItemRef::Weapon(wid), &weapons, &armor);
        let labels: Vec<&str> = stats.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            !labels.contains(&"Мана"),
            "weapons must not show a Мана row"
        );
    }

    // ── cell_compatible helpers ───────────────────────────────────────────────

    use crate::content::content_view::ActiveContentData;
    use crate::content::scenarios::{PartyMemberDef, ScenarioDef};
    use crate::game::resources::{GameDb, ScenarioState};

    /// Build a minimal GameDb + ScenarioState with one hero in the party.
    fn compat_fixture(
        hero_id: &str,
        class_id: &str,
        weapons: HashMap<WeaponId, WeaponDef>,
        armor_map: HashMap<ArmorId, ArmorDef>,
    ) -> (GameDb, ScenarioState, CampaignState, ActiveContentData) {
        let content = ActiveContentData {
            weapons,
            armor: armor_map,
            ..ActiveContentData::default()
        };

        let scen_id = "test_scen".to_string();
        let member = PartyMemberDef {
            id: hero_id.to_string(),
            name: hero_id.to_string(),
            race: String::new(),
            faction: None,
            path: None,
            class_id: class_id.to_string(),
            hex_pos: hexx::Hex::ZERO,
            template: None,
        };
        let scen = ScenarioDef {
            id: scen_id.clone(),
            name: scen_id.clone(),
            party: vec![member],
            scenes: vec![],
            content: ActiveContentData::default(),
            encounters: HashMap::new(),
        };

        let mut scenarios = HashMap::new();
        scenarios.insert(scen_id.clone(), scen);
        let db = GameDb {
            scenarios,
            campaigns: HashMap::new(),
            campaign_order: vec![],
        };

        let scenario_state = ScenarioState {
            scenario_id: scen_id,
            scene_index: 0,
        };

        let campaign = CampaignState {
            campaign_id: "test".to_string(),
            scenario_index: 0,
            flags: Default::default(),
            stash: vec![],
            loadouts: Default::default(),
        };

        (db, scenario_state, campaign, content)
    }

    /// Backpack weapon → MainHand equip slot is compatible.
    #[test]
    fn cell_compatible_backpack_weapon_into_main_hand() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![ItemRef::Weapon(WeaponId::from("mh_sword"))];

        let selection = CellKind::Backpack { index: 0 };
        let target = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::MainHand,
        };
        assert!(cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Backpack weapon → Chest equip slot is incompatible.
    #[test]
    fn cell_compatible_backpack_weapon_into_chest_incompatible() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![ItemRef::Weapon(WeaponId::from("mh_sword"))];

        let selection = CellKind::Backpack { index: 0 };
        let target = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::Chest,
        };
        assert!(!cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Backpack armor (Chest) → Chest slot is compatible.
    #[test]
    fn cell_compatible_backpack_armor_into_correct_slot() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![ItemRef::Armor(ArmorId::from("chest_plate"))];

        let selection = CellKind::Backpack { index: 0 };
        let target = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::Chest,
        };
        assert!(cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Backpack armor (Chest) → Legs slot is incompatible.
    #[test]
    fn cell_compatible_backpack_armor_into_wrong_slot() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![ItemRef::Armor(ArmorId::from("chest_plate"))];

        let selection = CellKind::Backpack { index: 0 };
        let target = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::Legs,
        };
        assert!(!cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Backpack → Backpack is always incompatible (we only offer equip slots).
    #[test]
    fn cell_compatible_backpack_to_backpack_incompatible() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![
            ItemRef::Weapon(WeaponId::from("mh_sword")),
            ItemRef::Armor(ArmorId::from("chest_plate")),
        ];

        let selection = CellKind::Backpack { index: 0 };
        let target = CellKind::Backpack { index: 1 };
        assert!(!cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// EquipCell (MainHand slot) → Backpack with a compatible weapon is compatible.
    #[test]
    fn cell_compatible_equip_to_backpack_fitting_item() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        // Backpack has a main-hand weapon; hero's main-hand slot → compatible.
        campaign.stash = vec![ItemRef::Weapon(WeaponId::from("mh_sword"))];

        let selection = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::MainHand,
        };
        let target = CellKind::Backpack { index: 0 };
        assert!(cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// EquipCell (MainHand slot) → Backpack with armor is incompatible.
    #[test]
    fn cell_compatible_equip_to_backpack_wrong_item_type() {
        let (w, a) = content_with_items();
        let (db, ss, mut campaign, content) = compat_fixture("hero", "cls", w, a);
        campaign.stash = vec![ItemRef::Armor(ArmorId::from("chest_plate"))];

        let selection = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::MainHand,
        };
        let target = CellKind::Backpack { index: 0 };
        assert!(!cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Equip → Equip is always incompatible.
    #[test]
    fn cell_compatible_equip_to_equip_incompatible() {
        let (w, a) = content_with_items();
        let (db, ss, campaign, content) = compat_fixture("hero", "cls", w, a);

        let selection = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::MainHand,
        };
        let target = CellKind::Equip {
            hero_id: "hero".to_string(),
            slot: EquipSlot::OffHand,
        };
        assert!(!cell_compatible(
            &selection, &target, &campaign, &db, &ss, &content
        ));
    }

    /// Weapon vs armor: weapon contributes Урон, armor contributes Броня — both rows appear.
    #[test]
    fn compare_weapon_vs_armor_mixed_rows() {
        let mut weapons = HashMap::new();
        let mut armor = HashMap::new();
        let dice = DiceExpr {
            count: 1,
            sides: 8,
            bonus: 0,
        }; // expected = 4.5
        let (wid, wdef) = make_weapon_stats("longsword", HandType::MainHand, dice, 0, 0, 0);
        let (aid, adef) = make_armor_stats("plate", ArmorSlot::Chest, 6, 0, 0, ArmorWeight::Light);
        weapons.insert(wid.clone(), wdef);
        armor.insert(aid.clone(), adef);

        let rows = compare_items(
            &ItemRef::Weapon(wid),
            &ItemRef::Armor(aid),
            &weapons,
            &armor,
        );

        let dmg = rows.iter().find(|r| r.label == "Урон");
        let bronya = rows.iter().find(|r| r.label == "Броня");
        assert!(dmg.is_some(), "Урон row (weapon side is non-zero)");
        assert!(bronya.is_some(), "Броня row (armor side is non-zero)");
        let dmg = dmg.unwrap();
        assert!((dmg.equipped_val - 4.5).abs() < 0.01);
        assert_eq!(dmg.incoming_val as i32, 0);
    }

    // ── orient_comparison (role-based, not selection-order) ────────────────────

    /// The delta direction must be `backpack − worn` regardless of which cell the
    /// player selected first. Selecting the worn item first and selecting the
    /// backpack item first must yield the same (worn, incoming) orientation.
    #[test]
    fn orient_comparison_is_role_based_not_click_order() {
        let worn = ItemRef::Weapon(WeaponId::from("worn"));
        let incoming = ItemRef::Weapon(WeaponId::from("incoming"));
        let equip = CellKind::Equip {
            hero_id: "aldric".into(),
            slot: EquipSlot::MainHand,
        };
        let backpack = CellKind::Backpack { index: 0 };

        // Case A: player selected the worn (equip) cell, hovers the backpack item.
        let (w_a, i_a) = orient_comparison(&equip, &worn, &incoming);
        // Case B: player selected the backpack item, hovers the worn (equip) cell.
        // Selection-order is reversed, so selected=incoming, hovered=worn.
        let (w_b, i_b) = orient_comparison(&backpack, &incoming, &worn);

        assert_eq!(w_a, &worn, "case A: worn resolved from equip cell");
        assert_eq!(i_a, &incoming, "case A: incoming resolved from backpack");
        assert_eq!(w_b, &worn, "case B: worn still the equip-slot item");
        assert_eq!(i_b, &incoming, "case B: incoming still the backpack item");
    }

    // ── ArmorWeight serde round-trip ─────────────────────────────────────────

    #[test]
    fn armor_weight_serde_round_trip() {
        #[derive(serde::Deserialize)]
        struct W {
            weight: ArmorWeight,
        }

        let cases = [
            (r#"weight = "light""#, ArmorWeight::Light),
            (r#"weight = "medium""#, ArmorWeight::Medium),
            (r#"weight = "heavy""#, ArmorWeight::Heavy),
        ];
        for (src, expected) in cases {
            let w: W = toml::from_str(src).unwrap();
            assert_eq!(w.weight, expected, "src: {src}");
        }

        // Missing field → default = Light
        #[derive(serde::Deserialize)]
        struct WOpt {
            #[serde(default)]
            weight: ArmorWeight,
        }
        let w: WOpt = toml::from_str("").unwrap();
        assert_eq!(w.weight, ArmorWeight::Light);
    }

    // ── hero_can_wear ────────────────────────────────────────────────────────

    /// Build a `ActiveContentData` with light/medium/heavy chest pieces and
    /// warrior/ranger/mage class defs — enough for proficiency tests.
    fn proficiency_content() -> ActiveContentData {
        use crate::content::classes::ClassDef;
        use crate::game::components::CombatStats;

        let mut armor: HashMap<ArmorId, ArmorDef> = HashMap::new();
        let (id, def) =
            make_armor_stats("light_robe", ArmorSlot::Chest, 0, 0, 0, ArmorWeight::Light);
        armor.insert(id, def);
        let (id, def) =
            make_armor_stats("mail_shirt", ArmorSlot::Chest, 1, 0, 0, ArmorWeight::Medium);
        armor.insert(id, def);
        let (id, def) =
            make_armor_stats("full_plate", ArmorSlot::Chest, 3, 0, 0, ArmorWeight::Heavy);
        armor.insert(id, def);

        let blank_stats = CombatStats {
            max_hp: 10,
            strength: 0,
            dexterity: 0,
            constitution: 0,
            intelligence: 0,
            wisdom: 0,
            charisma: 0,
        };
        let make_class = |id: &str, profs: Vec<ArmorWeight>| ClassDef {
            id: id.to_string(),
            name: id.to_string(),
            stats: blank_stats.clone(),
            speed: 3,
            abilities: vec![],
            main_hand: WeaponId::from(""),
            off_hand: None,
            chest: ArmorId::from(""),
            legs: ArmorId::from(""),
            feet: ArmorId::from(""),
            rage_max: 0,
            mana_max: 0,
            energy_max: 0,
            armor_proficiencies: profs,
        };

        let mut classes = HashMap::new();
        classes.insert(
            "warrior".to_string(),
            make_class("warrior", vec![ArmorWeight::Medium, ArmorWeight::Heavy]),
        );
        classes.insert(
            "ranger".to_string(),
            make_class("ranger", vec![ArmorWeight::Medium]),
        );
        classes.insert("mage".to_string(), make_class("mage", vec![]));

        ActiveContentData {
            armor,
            classes,
            ..ActiveContentData::default()
        }
    }

    #[test]
    fn hero_can_wear_light_armor_always_allowed() {
        let content = proficiency_content();
        let item = ItemRef::Armor(ArmorId::from("light_robe"));
        for class_id in ["warrior", "ranger", "mage", "unknown_class"] {
            assert!(
                hero_can_wear(class_id, &item, &content),
                "light armor must be allowed for '{class_id}'"
            );
        }
    }

    #[test]
    fn hero_can_wear_medium_armor_proficiency() {
        let content = proficiency_content();
        let item = ItemRef::Armor(ArmorId::from("mail_shirt"));
        assert!(
            hero_can_wear("warrior", &item, &content),
            "warrior can wear medium"
        );
        assert!(
            hero_can_wear("ranger", &item, &content),
            "ranger can wear medium"
        );
        assert!(
            !hero_can_wear("mage", &item, &content),
            "mage cannot wear medium"
        );
        assert!(
            !hero_can_wear("unknown_class", &item, &content),
            "unknown class cannot wear medium"
        );
    }

    #[test]
    fn hero_can_wear_heavy_armor_proficiency() {
        let content = proficiency_content();
        let item = ItemRef::Armor(ArmorId::from("full_plate"));
        assert!(
            hero_can_wear("warrior", &item, &content),
            "warrior can wear heavy"
        );
        assert!(
            !hero_can_wear("ranger", &item, &content),
            "ranger cannot wear heavy"
        );
        assert!(
            !hero_can_wear("mage", &item, &content),
            "mage cannot wear heavy"
        );
        assert!(
            !hero_can_wear("unknown_class", &item, &content),
            "unknown class cannot wear heavy"
        );
    }

    #[test]
    fn hero_can_wear_weapon_always_allowed() {
        let content = proficiency_content();
        let item = ItemRef::Weapon(WeaponId::from("any_sword"));
        for class_id in ["warrior", "mage", "unknown_class"] {
            assert!(
                hero_can_wear(class_id, &item, &content),
                "weapons must be allowed for '{class_id}'"
            );
        }
    }

    #[test]
    fn hero_can_wear_unknown_armor_id_denied() {
        let content = proficiency_content();
        let item = ItemRef::Armor(ArmorId::from("nonexistent_armor"));
        // Even a proficient class gets denied if the armor id is unknown.
        assert!(!hero_can_wear("warrior", &item, &content));
    }

    /// `item_weight` drives the comparison-card class line: armor → its weight,
    /// weapons → None (no class line), unknown armor → None.
    #[test]
    fn item_weight_resolves_armor_only() {
        let content = proficiency_content();
        assert_eq!(
            item_weight(&ItemRef::Armor(ArmorId::from("mail_shirt")), &content),
            Some(ArmorWeight::Medium),
        );
        assert_eq!(
            item_weight(&ItemRef::Armor(ArmorId::from("full_plate")), &content),
            Some(ArmorWeight::Heavy),
        );
        assert_eq!(
            item_weight(&ItemRef::Weapon(WeaponId::from("any_sword")), &content),
            None,
            "weapons have no weight class",
        );
        assert_eq!(
            item_weight(&ItemRef::Armor(ArmorId::from("nope")), &content),
            None,
            "unknown armor id → None",
        );
    }

    // ── Proficiency gate in cell_compatible ──────────────────────────────────

    /// A mage with a pre-equipped heavy plate can still swap in a light robe
    /// from the backpack (gate checks the INCOMING item, not the worn one).
    #[test]
    fn cell_compatible_mage_can_swap_out_heavy_with_light() {
        // Build a content view where mage has no armor proficiencies.
        use crate::content::classes::ClassDef;
        use crate::game::components::CombatStats;

        let blank_stats = CombatStats {
            max_hp: 10,
            strength: 0,
            dexterity: 0,
            constitution: 0,
            intelligence: 0,
            wisdom: 0,
            charisma: 0,
        };

        let mage_class = ClassDef {
            id: "mage".to_string(),
            name: "Mage".to_string(),
            stats: blank_stats,
            speed: 4,
            abilities: vec![],
            main_hand: WeaponId::from("staff"),
            off_hand: None,
            chest: ArmorId::from("light_robe"),
            legs: ArmorId::from("cloth_pants"),
            feet: ArmorId::from("cloth_shoes"),
            rage_max: 0,
            mana_max: 10,
            energy_max: 0,
            armor_proficiencies: vec![],
        };

        let mut armor: HashMap<ArmorId, ArmorDef> = HashMap::new();
        let (id, def) =
            make_armor_stats("light_robe", ArmorSlot::Chest, 0, 0, 0, ArmorWeight::Light);
        armor.insert(id, def);
        let (id, def) =
            make_armor_stats("full_plate", ArmorSlot::Chest, 3, 0, 0, ArmorWeight::Heavy);
        armor.insert(id, def);

        let mut classes = HashMap::new();
        classes.insert("mage".to_string(), mage_class);

        let content = ActiveContentData {
            armor: armor.clone(),
            classes,
            ..ActiveContentData::default()
        };

        // Mage currently wearing full_plate (pre-seeded loadout).
        let (db, ss, mut campaign, _) = compat_fixture("mage_hero", "mage", HashMap::new(), armor);
        let loadout = crate::content::item_ref::EquipmentSave {
            main_hand: None,
            off_hand: None,
            chest: ArmorId::from("full_plate"),
            legs: ArmorId::from("cloth_pants"),
            feet: ArmorId::from("cloth_shoes"),
        };
        campaign.loadouts.insert("mage_hero".to_string(), loadout);
        // Light robe is in backpack at index 0, heavy plate also at index 1.
        campaign.stash = vec![
            ItemRef::Armor(ArmorId::from("light_robe")),
            ItemRef::Armor(ArmorId::from("full_plate")),
        ];

        let light_bp = CellKind::Backpack { index: 0 };
        let heavy_bp = CellKind::Backpack { index: 1 };
        let chest_slot = CellKind::Equip {
            hero_id: "mage_hero".to_string(),
            slot: EquipSlot::Chest,
        };

        // Light robe in backpack → mage chest slot: COMPATIBLE (incoming is light).
        assert!(
            cell_compatible(&light_bp, &chest_slot, &campaign, &db, &ss, &content),
            "mage should be able to swap out heavy plate by equipping a light robe"
        );

        // Heavy plate in backpack → mage chest slot: NOT compatible (mage lacks heavy proficiency).
        assert!(
            !cell_compatible(&heavy_bp, &chest_slot, &campaign, &db, &ss, &content),
            "mage should NOT be able to equip a heavy plate from backpack"
        );
    }
}
