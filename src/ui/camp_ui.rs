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
use crate::content::armor::{ArmorDef, ArmorSlot};
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
                    Ok(EquipResult { new_save, displaced })
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
                    Ok(EquipResult { new_save, displaced })
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
                    Ok(EquipResult { new_save, displaced })
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
                    Ok(EquipResult { new_save, displaced })
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
                    Ok(EquipResult { new_save, displaced })
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
    content: &crate::content::content_view::ContentView,
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
        warn!("camp: unknown class_id '{}' for hero '{}'", class_id, hero_id);
        EquipmentSave {
            main_hand: None,
            off_hand: None,
            chest: ArmorId::from(""),
            legs: ArmorId::from(""),
            feet: ArmorId::from(""),
        }
    }
}

fn weapon_name<'a>(id: &WeaponId, content: &'a crate::content::content_view::ContentView) -> &'a str {
    content.weapons.get(id).map(|d| d.name.as_str()).unwrap_or("?")
}

fn armor_name<'a>(id: &ArmorId, content: &'a crate::content::content_view::ContentView) -> &'a str {
    content.armor.get(id).map(|d| d.name.as_str()).unwrap_or("?")
}

/// Short display label for an item (first 8 chars to fit in the 56px cell).
fn item_abbrev(item: &ItemRef, content: &crate::content::content_view::ContentView) -> String {
    let full = match item {
        ItemRef::Weapon(wid) => weapon_name(wid, content),
        ItemRef::Armor(aid) => armor_name(aid, content),
    };
    // Take up to 8 chars (char boundary safe).
    let end = full.char_indices().nth(8).map(|(i, _)| i).unwrap_or(full.len());
    full[..end].to_string()
}

// ── Stat comparison card ─────────────────────────────────────────────────────

/// Marker for the fixed comparison-card panel.
/// Spawned once inside `CampScreenRoot`; shown/hidden by `camp_comparison_system`.
#[derive(Component)]
pub struct ComparisonCard;

/// One stat row in the comparison card.
#[derive(Debug, Clone)]
pub struct CompareRow {
    pub label: String,
    /// Value for the selected item (left column).
    pub selected_val: f32,
    /// Value for the hovered item (right column).
    pub hovered_val: f32,
}

/// Build comparison rows for two items.
///
/// Returns only rows where at least one item has a non-zero value.
/// For weapons, "Урон" is the average dice damage (`DiceExpr::expected()`).
///
/// Pure function — no Bevy types, fully unit-testable.
pub fn compare_items(
    selected: &ItemRef,
    hovered: &ItemRef,
    weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    armor: &std::collections::HashMap<ArmorId, ArmorDef>,
) -> Vec<CompareRow> {
    // Inline stat bundle extracted from whichever item kind we have.
    struct Stats {
        damage:      f32,
        spell_power: f32,
        armor:       f32,
        max_hp:      f32,
        strength:    f32,
        dexterity:   f32,
        constitution: f32,
        intelligence: f32,
        wisdom:       f32,
        charisma:     f32,
    }

    let extract = |item: &ItemRef| -> Stats {
        match item {
            ItemRef::Weapon(wid) => {
                if let Some(def) = weapons.get(wid) {
                    Stats {
                        damage:       def.dice.expected(),
                        spell_power:  def.spell_power as f32,
                        armor:        def.armor as f32,
                        max_hp:       def.max_hp as f32,
                        strength:     def.strength as f32,
                        dexterity:    def.dexterity as f32,
                        constitution: def.constitution as f32,
                        intelligence: def.intelligence as f32,
                        wisdom:       def.wisdom as f32,
                        charisma:     def.charisma as f32,
                    }
                } else {
                    Stats { damage: 0.0, spell_power: 0.0, armor: 0.0, max_hp: 0.0,
                            strength: 0.0, dexterity: 0.0, constitution: 0.0,
                            intelligence: 0.0, wisdom: 0.0, charisma: 0.0 }
                }
            }
            ItemRef::Armor(aid) => {
                if let Some(def) = armor.get(aid) {
                    Stats {
                        damage:       0.0,
                        spell_power:  0.0,
                        armor:        def.armor as f32,
                        max_hp:       def.max_hp as f32,
                        strength:     def.strength as f32,
                        dexterity:    def.dexterity as f32,
                        constitution: def.constitution as f32,
                        intelligence: def.intelligence as f32,
                        wisdom:       def.wisdom as f32,
                        charisma:     def.charisma as f32,
                    }
                } else {
                    Stats { damage: 0.0, spell_power: 0.0, armor: 0.0, max_hp: 0.0,
                            strength: 0.0, dexterity: 0.0, constitution: 0.0,
                            intelligence: 0.0, wisdom: 0.0, charisma: 0.0 }
                }
            }
        }
    };

    let s = extract(selected);
    let h = extract(hovered);

    let candidates: &[(&str, f32, f32)] = &[
        ("Урон",      s.damage,       h.damage),
        ("Сила закл.", s.spell_power, h.spell_power),
        ("Броня",     s.armor,        h.armor),
        ("HP",        s.max_hp,       h.max_hp),
        ("СИЛ",       s.strength,     h.strength),
        ("ЛОВ",       s.dexterity,    h.dexterity),
        ("ТЕЛ",       s.constitution, h.constitution),
        ("ИНТ",       s.intelligence, h.intelligence),
        ("МУД",       s.wisdom,       h.wisdom),
        ("ХАР",       s.charisma,     h.charisma),
    ];

    candidates
        .iter()
        .filter(|(_, sv, hv)| *sv != 0.0 || *hv != 0.0)
        .map(|(label, sv, hv)| CompareRow {
            label: label.to_string(),
            selected_val: *sv,
            hovered_val: *hv,
        })
        .collect()
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
    content: &crate::content::content_view::ContentView,
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
                EquipSlot::OffHand  => eq.off_hand.map(ItemRef::Weapon),
                EquipSlot::Chest    => {
                    let id = eq.chest;
                    if id.0.is_empty() { None } else { Some(ItemRef::Armor(id)) }
                }
                EquipSlot::Legs     => {
                    let id = eq.legs;
                    if id.0.is_empty() { None } else { Some(ItemRef::Armor(id)) }
                }
                EquipSlot::Feet     => {
                    let id = eq.feet;
                    if id.0.is_empty() { None } else { Some(ItemRef::Armor(id)) }
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

// ── Cell spawn helper ─────────────────────────────────────────────────────────

/// Spawns a square 56×56 Button cell with the given label, in idle style.
fn spawn_cell<'a>(
    parent: &'a mut ChildSpawnerCommands,
    font: Handle<Font>,
    label: impl Into<String>,
) -> EntityCommands<'a> {
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
        BorderColor::all(CELL_IDLE_BORDER),
        BackgroundColor(CELL_IDLE_BG),
    ));
    ec.with_children(|btn| {
        btn.spawn((
            Text::new(label),
            TextFont { font, font_size: 10.0, ..default() },
            TextColor(Color::WHITE),
        ));
    });
    ec
}

// ── Spawn helper ──────────────────────────────────────────────────────────────

fn spawn_camp_ui(
    commands: &mut Commands,
    font: Handle<Font>,
    db: &GameDb,
    scenario_state: &ScenarioState,
    campaign: &CampaignState,
    content: &crate::content::content_view::ContentView,
) {
    let scen = db.scenarios.get(&scenario_state.scenario_id).unwrap();
    let party = active_party(scen, scenario_state.scene_index);

    // Only class-heroes get a loadout row; template-NPCs are skipped.
    let class_heroes: Vec<_> = party.iter().filter(|m| m.template.is_none()).collect();

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
                TextFont { font: font.clone(), font_size: 28.0, ..default() },
                TextColor(Color::srgb(0.9, 0.85, 0.7)),
            ));

            // Instruction hint
            root.spawn((
                Text::new("Нажмите ячейку, затем ячейку назначения, чтобы поменять местами"),
                TextFont { font: font.clone(), font_size: 13.0, ..default() },
                TextColor(Color::srgb(0.6, 0.6, 0.6)),
            ));

            // ── Per-hero equipment grids ──────────────────────────────────
            for member in &class_heroes {
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
                        TextFont { font: font.clone(), font_size: 16.0, ..default() },
                        TextColor(Color::srgb(0.9, 0.85, 0.6)),
                    ));

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
                                (EquipSlot::MainHand, eq.main_hand.as_ref().map(|w| ItemRef::Weapon(w.clone()))),
                                (EquipSlot::OffHand,  eq.off_hand.as_ref().map(|w| ItemRef::Weapon(w.clone()))),
                                (EquipSlot::Chest, Some(ItemRef::Armor(eq.chest.clone()))),
                                (EquipSlot::Legs,  Some(ItemRef::Armor(eq.legs.clone()))),
                                (EquipSlot::Feet,  Some(ItemRef::Armor(eq.feet.clone()))),
                            ];

                            for (slot, maybe_item) in slots {
                                let label = match &maybe_item {
                                    Some(item) => item_abbrev(item, content),
                                    None => "—".into(),
                                };
                                spawn_cell(slots_row, font.clone(), label)
                                    .insert(EquipCell { hero_id: id.clone(), slot });
                            }
                        });
                });
            }

            // ── Backpack (stash) grid ─────────────────────────────────────
            root.spawn((
                Text::new("Рюкзак"),
                TextFont { font: font.clone(), font_size: 14.0, ..default() },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
            ));

            if campaign.stash.is_empty() {
                root.spawn((
                    Text::new("— пусто —"),
                    TextFont { font: font.clone(), font_size: 13.0, ..default() },
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
                        spawn_cell(pack_grid, font.clone(), label)
                            .insert(BackpackCell { index: i });
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
                root, font.clone(), "Продолжить",
                Val::Px(200.0), Val::Auto, ButtonStyle::Default,
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
    spawn_camp_ui(
        &mut commands,
        font,
        &db,
        &scenario_state,
        &campaign,
        &active_content.0,
    );
}

// ── Rebuild system ────────────────────────────────────────────────────────────

/// After a swap, rebuilds the camp UI in-place (despawn + respawn).
/// Runs after `camp_interaction_system` under `run_if(in_state(Camp))`.
#[allow(clippy::too_many_arguments)]
pub fn camp_rebuild_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
    campaign: Option<Res<CampaignState>>,
    active_content: Res<ActiveContent>,
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
        &db,
        &scenario_state,
        &campaign,
        &active_content.0,
    );
}

// ── Input ─────────────────────────────────────────────────────────────────────

/// Handles cell selection, swap attempts, and Continue.
///
/// Interaction pattern (mirrors `main_menu_ui`):
/// - Query `Changed<Interaction>`, check `*i == Interaction::Pressed`.
/// - First press → select cell (highlight).
/// - Second press → attempt swap, then rebuild + clear selection.
/// - Press already-selected cell → deselect.
/// - EquipCell ↔ EquipCell → re-select the new cell (equip↔equip not supported
///   this pass; see module doc).
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
        (Some((e, c)), None) => Some((e, CellKind::Equip { hero_id: c.hero_id, slot: c.slot })),
        (None, Some((e, c))) => Some((e, CellKind::Backpack { index: c.index })),
        _ => None,
    };

    let Some((pressed_entity, pressed_kind)) = pressed else {
        return;
    };

    // ── Helper: set highlight on an entity ───────────────────────────────
    let highlight_entity = |entity: Entity,
                             selected: bool,
                             borders: &mut Query<&mut BorderColor>,
                             backgrounds: &mut Query<&mut BackgroundColor>| {
        if let Ok(mut bc) = borders.get_mut(entity) {
            *bc = BorderColor::all(if selected { CELL_SELECTED_BORDER } else { CELL_IDLE_BORDER });
        }
        if let Ok(mut bg) = backgrounds.get_mut(entity) {
            *bg = BackgroundColor(if selected { CELL_SELECTED_BG } else { CELL_IDLE_BG });
        }
    };

    // ── De-select helper (remove marker + reset visuals) ─────────────────
    let clear_selection = |commands: &mut Commands,
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
        // Select this cell.
        selection.selected = Some(pressed_kind);
        commands.entity(pressed_entity).insert(SelectedCellMarker);
        highlight_entity(pressed_entity, true, &mut borders, &mut backgrounds);
        return;
    };

    // ── Case: pressed the already-selected cell — deselect ───────────────
    if current_selection == pressed_kind {
        selection.selected = None;
        clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
        return;
    }

    // ── Case: second cell pressed — attempt swap ──────────────────────────
    match (&current_selection, &pressed_kind) {
        // EquipCell ↔ EquipCell: not supported; re-select the new cell.
        (CellKind::Equip { .. }, CellKind::Equip { .. }) => {
            clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
            selection.selected = Some(pressed_kind);
            commands.entity(pressed_entity).insert(SelectedCellMarker);
            highlight_entity(pressed_entity, true, &mut borders, &mut backgrounds);
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
            clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
            rebuild.0 = true;
        }

        // EquipCell ↔ BackpackCell (either order): try_equip + true swap.
        (first, second) => {
            let (equip_kind, backpack_idx) = match (first, second) {
                (CellKind::Equip { hero_id, slot }, CellKind::Backpack { index }) => {
                    (CellKind::Equip { hero_id: hero_id.clone(), slot: slot.clone() }, *index)
                }
                (CellKind::Backpack { index }, CellKind::Equip { hero_id, slot }) => {
                    (CellKind::Equip { hero_id: hero_id.clone(), slot: slot.clone() }, *index)
                }
                _ => unreachable!(),
            };

            let CellKind::Equip { hero_id, slot } = equip_kind else { unreachable!() };

            let Some(ref mut camp) = campaign else {
                selection.selected = None;
                clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
                return;
            };

            let item = match camp.stash.get(backpack_idx) {
                Some(i) => i.clone(),
                None => {
                    // Stash index stale; clear and bail.
                    selection.selected = None;
                    clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
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

            match try_equip(&current_save, &slot, item.clone(), &content.weapons, &content.armor) {
                Ok(result) => {
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
                    let is_empty_sentinel = |d: &ItemRef| matches!(d, ItemRef::Armor(aid) if aid.0.is_empty());
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
                    clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
                    rebuild.0 = true;
                }
                Err(e) => {
                    warn!("camp: equip rejected for hero '{}': {:?}", hero_id, e);
                    // Reject: clear selection, no state change.
                    selection.selected = None;
                    clear_selection(&mut commands, &selected_entities, &mut borders, &mut backgrounds);
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
/// Shows the card when:
///   - a cell is selected (`CampEquipSelection.selected.is_some()`), AND
///   - exactly one cell is hovered that is NOT the selected cell, AND
///   - both cells resolve to an item.
///
/// The card is a fixed top-right panel (absolute position, never overlaps the
/// grids), so it cannot intercept pointer events — the cell buttons always
/// receive `Interaction::Pressed` correctly.
///
/// Uses a `Local` to track the last `(selected, hovered)` pair so children are
/// only rebuilt when the pair changes, not every frame.
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
    mut last: Local<Option<(CellKind, CellKind)>>,
) {
    // Find the single hovered cell (if any).
    let hovered_kind: Option<CellKind> = {
        let from_equip = equip_cells
            .iter()
            .find(|(i, _)| **i == Interaction::Hovered)
            .map(|(_, c)| CellKind::Equip { hero_id: c.hero_id.clone(), slot: c.slot.clone() });
        let from_backpack = backpack_cells
            .iter()
            .find(|(i, _)| **i == Interaction::Hovered)
            .map(|(_, c)| CellKind::Backpack { index: c.index });
        from_equip.or(from_backpack)
    };

    // Determine whether we should show the card.
    let show = (|| -> Option<(CellKind, CellKind)> {
        let selected_kind = selection.selected.clone()?;
        let hovered = hovered_kind?;
        if hovered == selected_kind {
            return None; // hovering the selected cell — hide
        }
        Some((selected_kind, hovered))
    })();

    let Ok((card_entity, mut card_vis)) = cards.single_mut() else {
        return;
    };

    let Some((selected_kind, hovered_kind)) = show else {
        // Hide card and reset last pair.
        *card_vis = Visibility::Hidden;
        *last = None;
        return;
    };

    // Only rebuild children when the pair changes.
    if *last == Some((selected_kind.clone(), hovered_kind.clone())) {
        *card_vis = Visibility::Inherited;
        return;
    }

    let Some(campaign) = campaign else { return };
    let content = &active_content.0;

    // Resolve items for both cells.
    let sel_item = cell_item(&selected_kind, &campaign, &db, &scenario_state, content);
    let hov_item = cell_item(&hovered_kind, &campaign, &db, &scenario_state, content);

    let (Some(sel_item), Some(hov_item)) = (sel_item, hov_item) else {
        // One cell is empty — hide card.
        *card_vis = Visibility::Hidden;
        *last = None;
        return;
    };

    // Item names for headers.
    let sel_name = match &sel_item {
        ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
        ItemRef::Armor(aid)  => armor_name(aid, content).to_string(),
    };
    let hov_name = match &hov_item {
        ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
        ItemRef::Armor(aid)  => armor_name(aid, content).to_string(),
    };

    let rows = compare_items(&sel_item, &hov_item, &content.weapons, &content.armor);

    // Rebuild card children.
    *last = Some((selected_kind, hovered_kind));
    *card_vis = Visibility::Inherited;

    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");

    commands.entity(card_entity).despawn_related::<Children>();
    commands.entity(card_entity).with_children(|card| {
        // Header row: "Выбрано | Наведено"
        card.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Text::new(sel_name),
                TextFont { font: font.clone(), font_size: 12.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.5)),
            ));
            row.spawn((
                Text::new(" / "),
                TextFont { font: font.clone(), font_size: 12.0, ..default() },
                TextColor(Color::srgb(0.6, 0.6, 0.6)),
            ));
            row.spawn((
                Text::new(hov_name),
                TextFont { font: font.clone(), font_size: 12.0, ..default() },
                TextColor(Color::srgb(0.5, 0.85, 0.95)),
            ));
        });

        // Stat rows.
        for row_data in &rows {
            let delta = row_data.hovered_val - row_data.selected_val;
            let delta_color = if delta > 0.0 {
                Color::srgb(0.3, 0.9, 0.3)
            } else if delta < 0.0 {
                Color::srgb(0.9, 0.3, 0.3)
            } else {
                Color::srgb(0.6, 0.6, 0.6)
            };
            let delta_str = if delta > 0.0 {
                format!("+{:.0}", delta)
            } else {
                format!("{:.0}", delta)
            };

            card.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|row| {
                // Label
                row.spawn((
                    Text::new(format!("{:<10}", row_data.label)),
                    TextFont { font: font.clone(), font_size: 11.0, ..default() },
                    TextColor(Color::srgb(0.75, 0.75, 0.75)),
                ));
                // Selected value
                row.spawn((
                    Text::new(format!("{:.0}", row_data.selected_val)),
                    TextFont { font: font.clone(), font_size: 11.0, ..default() },
                    TextColor(Color::srgb(0.95, 0.85, 0.5)),
                ));
                // Arrow + delta
                row.spawn((
                    Text::new(format!("→ {}", delta_str)),
                    TextFont { font: font.clone(), font_size: 11.0, ..default() },
                    TextColor(delta_color),
                ));
                // Hovered value
                row.spawn((
                    Text::new(format!("{:.0}", row_data.hovered_val)),
                    TextFont { font: font.clone(), font_size: 11.0, ..default() },
                    TextColor(Color::srgb(0.5, 0.85, 0.95)),
                ));
            });
        }

        if rows.is_empty() {
            card.spawn((
                Text::new("Нет статов"),
                TextFont { font, font_size: 11.0, ..default() },
                TextColor(Color::srgb(0.5, 0.5, 0.5)),
            ));
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::armor::{ArmorDef, ArmorSlot};
    use crate::content::weapons::{HandType, WeaponDef};
    use combat_engine::{ArmorId, DiceExpr, WeaponId};
    use std::collections::HashMap;

    // ── Test content builders ────────────────────────────────────────────────

    fn make_weapon(id: &str, hand: HandType) -> (WeaponId, WeaponDef) {
        let wid = WeaponId::from(id);
        let def = WeaponDef {
            id: wid.clone(),
            name: id.to_string(),
            hand,
            dice: DiceExpr { count: 1, sides: 6, bonus: 0 },
            spell_power: 0, armor: 0, max_hp: 0,
            strength: 0, dexterity: 0, constitution: 0,
            intelligence: 0, wisdom: 0, charisma: 0,
        };
        (wid, def)
    }

    fn make_armor(id: &str, slot: ArmorSlot) -> (ArmorId, ArmorDef) {
        let aid = ArmorId::from(id);
        let def = ArmorDef {
            id: aid.clone(),
            name: id.to_string(),
            slot,
            armor: 1, max_hp: 0,
            strength: 0, dexterity: 0, constitution: 0,
            intelligence: 0, wisdom: 0, charisma: 0,
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
            &base_save(), &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("mh_sword")), &w, &a,
        ).unwrap();
        assert_eq!(result.new_save.main_hand, Some(WeaponId::from("mh_sword")));
        assert_eq!(result.displaced, Some(ItemRef::Weapon(WeaponId::from("mh_sword"))));
    }

    /// Off-hand weapon into off-hand slot: succeeds.
    #[test]
    fn equip_off_hand_into_off_hand() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(), &EquipSlot::OffHand,
            ItemRef::Weapon(WeaponId::from("oh_dagger")), &w, &a,
        ).unwrap();
        assert_eq!(result.new_save.off_hand, Some(WeaponId::from("oh_dagger")));
    }

    /// Off-hand weapon into main-hand slot: rejected.
    #[test]
    fn equip_off_hand_into_main_hand_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(), &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("oh_dagger")), &w, &a,
        ).unwrap_err();
        assert_eq!(err, EquipError::OffHandIntoMainHand);
    }

    /// Two-handed weapon into main-hand: succeeds and clears off_hand in new save.
    #[test]
    fn equip_two_handed_clears_off_hand() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(), &EquipSlot::MainHand,
            ItemRef::Weapon(WeaponId::from("2h_axe")), &w, &a,
        ).unwrap();
        assert_eq!(result.new_save.main_hand, Some(WeaponId::from("2h_axe")));
        assert_eq!(result.new_save.off_hand, None, "off_hand cleared by two-handed");
    }

    /// Two-handed weapon into off-hand: rejected.
    #[test]
    fn equip_two_handed_into_off_hand_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(), &EquipSlot::OffHand,
            ItemRef::Weapon(WeaponId::from("2h_axe")), &w, &a,
        ).unwrap_err();
        assert_eq!(err, EquipError::WeaponIntoArmorSlot);
    }

    /// Weapon into armor slot: rejected.
    #[test]
    fn equip_weapon_into_armor_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(), &EquipSlot::Chest,
            ItemRef::Weapon(WeaponId::from("mh_sword")), &w, &a,
        ).unwrap_err();
        assert_eq!(err, EquipError::WeaponIntoArmorSlot);
    }

    /// Armor into correct slot: succeeds, displaced = old armor.
    #[test]
    fn equip_armor_into_correct_slot() {
        let (w, a) = content_with_items();
        let result = try_equip(
            &base_save(), &EquipSlot::Chest,
            ItemRef::Armor(ArmorId::from("chest_plate")), &w, &a,
        ).unwrap();
        assert_eq!(result.new_save.chest, ArmorId::from("chest_plate"));
        assert_eq!(result.displaced, Some(ItemRef::Armor(ArmorId::from("chest_plate"))));
    }

    /// Armor into wrong armor slot: rejected with ArmorSlotMismatch.
    #[test]
    fn equip_armor_into_wrong_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(), &EquipSlot::Legs,
            ItemRef::Armor(ArmorId::from("chest_plate")), &w, &a,
        ).unwrap_err();
        assert!(matches!(err, EquipError::ArmorSlotMismatch { .. }));
    }

    /// Armor into hand slot: rejected.
    #[test]
    fn equip_armor_into_hand_slot_rejected() {
        let (w, a) = content_with_items();
        let err = try_equip(
            &base_save(), &EquipSlot::MainHand,
            ItemRef::Armor(ArmorId::from("chest_plate")), &w, &a,
        ).unwrap_err();
        assert_eq!(err, EquipError::ArmorIntoHandSlot);
    }

    // ── Equip flow: stash ↔ loadout (pure) ──────────────────────────────────

    /// Equip a new chest from stash: item leaves stash, displaced (old chest) goes back.
    #[test]
    fn equip_flow_stash_and_loadout() {
        let (w, a) = content_with_items();
        let save = base_save();
        let result = try_equip(
            &save, &EquipSlot::Chest,
            ItemRef::Armor(ArmorId::from("chest_plate")), &w, &a,
        ).unwrap();
        assert_eq!(result.new_save.chest, ArmorId::from("chest_plate"));
        // Old chest_plate is displaced (same item in this test).
        assert_eq!(result.displaced, Some(ItemRef::Armor(ArmorId::from("chest_plate"))));
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

        let result = try_equip(
            &save, &EquipSlot::Chest,
            stash[0].clone(), &w, &a,
        ).unwrap();

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

        let result = try_equip(
            &save_no_oh, &EquipSlot::OffHand,
            stash[0].clone(), &w, &a,
        ).unwrap();

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
        let err = try_equip(
            &save, &EquipSlot::MainHand,
            stash_before[0].clone(), &w, &a,
        ).unwrap_err();

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
        assert!(should_enter_camp(&story_scene(false), &story_scene(false), true));
    }

    /// no_camp=true on the FROM scene → skip camp.
    #[test]
    fn should_enter_camp_no_camp_true_skips() {
        assert!(!should_enter_camp(&story_scene(true), &story_scene(false), true));
    }

    /// No CampaignState → skip camp.
    #[test]
    fn should_enter_camp_no_campaign_skips() {
        assert!(!should_enter_camp(&story_scene(false), &story_scene(false), false));
    }

    /// Story → Combat → skip camp.
    #[test]
    fn should_enter_camp_story_to_combat_skips() {
        assert!(!should_enter_camp(&story_scene(false), &combat_scene(), true));
    }

    /// Combat → Story → skip camp (only Story→Story qualifies).
    #[test]
    fn should_enter_camp_combat_to_story_skips() {
        assert!(!should_enter_camp(&combat_scene(), &story_scene(false), true));
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
            spell_power,
            armor,
            max_hp,
            strength: 0,
            dexterity: 0,
            constitution: 0,
            intelligence: 0,
            wisdom: 0,
            charisma: 0,
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
    ) -> (ArmorId, ArmorDef) {
        let aid = ArmorId::from(id);
        let def = ArmorDef {
            id: aid.clone(),
            name: id.to_string(),
            slot,
            armor: armor_val,
            max_hp,
            strength,
            dexterity: 0,
            constitution: 0,
            intelligence: 0,
            wisdom: 0,
            charisma: 0,
        };
        (aid, def)
    }

    /// Two weapons: compare produces "Урон" row with correct expected-damage values.
    #[test]
    fn compare_two_weapons_damage_row() {
        let mut weapons = HashMap::new();
        let dice_a = DiceExpr { count: 1, sides: 6, bonus: 0 }; // expected = 3.5
        let dice_b = DiceExpr { count: 2, sides: 4, bonus: 1 }; // expected = 6.0
        let (id_a, def_a) = make_weapon_stats("sword", HandType::MainHand, dice_a, 0, 0, 0);
        let (id_b, def_b) = make_weapon_stats("axe",   HandType::MainHand, dice_b, 0, 0, 0);
        weapons.insert(id_a.clone(), def_a);
        weapons.insert(id_b.clone(), def_b);
        let armor: HashMap<ArmorId, ArmorDef> = HashMap::new();

        let rows = compare_items(
            &ItemRef::Weapon(id_a),
            &ItemRef::Weapon(id_b),
            &weapons,
            &armor,
        );

        let dmg = rows.iter().find(|r| r.label == "Урон").expect("Урон row present");
        assert!((dmg.selected_val - 3.5).abs() < 0.01, "sword expected = 3.5");
        assert!((dmg.hovered_val  - 6.0).abs() < 0.01, "axe expected = 6.0");
    }

    /// Two armors: compare produces "Броня", "HP", "СИЛ" rows; no "Урон" row.
    #[test]
    fn compare_two_armors_stat_rows() {
        let weapons: HashMap<WeaponId, WeaponDef> = HashMap::new();
        let mut armor = HashMap::new();
        // chest_a: armor=3, hp=10, str=0
        let (id_a, def_a) = make_armor_stats("chest_a", ArmorSlot::Chest, 3, 10, 0);
        // chest_b: armor=5, hp=0, str=2
        let (id_b, def_b) = make_armor_stats("chest_b", ArmorSlot::Chest, 5, 0, 2);
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
        assert!(labels.contains(&"HP"),    "HP row present");
        assert!(labels.contains(&"СИЛ"),   "СИЛ row present");
        assert!(!labels.contains(&"Урон"), "no Урон row for armor");

        let bronya = rows.iter().find(|r| r.label == "Броня").unwrap();
        assert_eq!(bronya.selected_val as i32, 3);
        assert_eq!(bronya.hovered_val  as i32, 5);

        let hp = rows.iter().find(|r| r.label == "HP").unwrap();
        assert_eq!(hp.selected_val as i32, 10);
        assert_eq!(hp.hovered_val  as i32, 0);
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
            dice: DiceExpr { count: 0, sides: 6, bonus: 0 },
            spell_power: 0, armor: 0, max_hp: 0,
            strength: 0, dexterity: 0, constitution: 0,
            intelligence: 0, wisdom: 0, charisma: 0,
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

    /// Weapon vs armor: weapon contributes Урон, armor contributes Броня — both rows appear.
    #[test]
    fn compare_weapon_vs_armor_mixed_rows() {
        let mut weapons = HashMap::new();
        let mut armor = HashMap::new();
        let dice = DiceExpr { count: 1, sides: 8, bonus: 0 }; // expected = 4.5
        let (wid, wdef) = make_weapon_stats("longsword", HandType::MainHand, dice, 0, 0, 0);
        let (aid, adef) = make_armor_stats("plate", ArmorSlot::Chest, 6, 0, 0);
        weapons.insert(wid.clone(), wdef);
        armor.insert(aid.clone(), adef);

        let rows = compare_items(
            &ItemRef::Weapon(wid),
            &ItemRef::Armor(aid),
            &weapons,
            &armor,
        );

        let dmg   = rows.iter().find(|r| r.label == "Урон");
        let bronya = rows.iter().find(|r| r.label == "Броня");
        assert!(dmg.is_some(),    "Урон row (weapon side is non-zero)");
        assert!(bronya.is_some(), "Броня row (armor side is non-zero)");
        let dmg = dmg.unwrap();
        assert!((dmg.selected_val - 4.5).abs() < 0.01);
        assert_eq!(dmg.hovered_val as i32, 0);
    }
}
