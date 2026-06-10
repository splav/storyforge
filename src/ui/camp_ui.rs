//! Camp screen: displayed between two Story scenes so the player can re-equip
//! heroes from the party stash.  Entered via `AppState::Camp`; a "Continue"
//! button (or Enter/Space) transitions to `AppState::Story` to show the
//! already-queued next scene.

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

/// Marker on an equip-slot button.
#[derive(Component, Clone)]
pub struct EquipSlotButton {
    pub hero_id: String,
    pub slot: EquipSlot,
}

/// Marker on a stash-item button.
#[derive(Component, Clone)]
pub struct StashItemButton {
    pub stash_index: usize,
}

/// Which slot in the hero's loadout the `EquipSlotButton` represents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EquipSlot {
    MainHand,
    OffHand,
    Chest,
    Legs,
    Feet,
}

// ── Camp state resources ─────────────────────────────────────────────────────

/// Which hero + slot is currently "highlighted" waiting for a stash item to be
/// chosen.  `None` means nothing selected.
#[derive(Resource, Default)]
pub struct CampEquipSelection {
    pub selected: Option<(String, EquipSlot)>,
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

    // Only class-heroes get a loadout row; template-NPCs are skipped for MVP.
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
                Text::new("Выберите слот, затем предмет из инвентаря"),
                TextFont { font: font.clone(), font_size: 13.0, ..default() },
                TextColor(Color::srgb(0.6, 0.6, 0.6)),
            ));

            // Hero loadout rows
            for member in &class_heroes {
                let eq = resolve_hero_equipment(&member.id, &member.class_id, campaign, content);
                root.spawn((
                    Node {
                        width: Val::Px(720.0),
                        flex_direction: FlexDirection::Column,
                        border: UiRect::all(Val::Px(1.0)),
                        padding: UiRect::all(Val::Px(8.0)),
                        row_gap: Val::Px(6.0),
                        ..default()
                    },
                    BorderColor::all(Color::srgb(0.35, 0.30, 0.25)),
                ))
                .with_children(|hero_panel| {
                    hero_panel.spawn((
                        Text::new(member.name.clone()),
                        TextFont { font: font.clone(), font_size: 16.0, ..default() },
                        TextColor(Color::srgb(0.9, 0.85, 0.6)),
                    ));

                    hero_panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(6.0),
                            flex_wrap: FlexWrap::Wrap,
                            ..default()
                        })
                        .with_children(|slots_row| {
                            let id = member.id.clone();

                            let mh_label = eq
                                .main_hand
                                .as_ref()
                                .map(|w| format!("Правая: {}", weapon_name(w, content)))
                                .unwrap_or_else(|| "Правая: —".into());
                            spawn_standard_button(
                                slots_row, font.clone(), mh_label,
                                Val::Auto, Val::Auto, ButtonStyle::Default,
                            )
                            .insert(EquipSlotButton { hero_id: id.clone(), slot: EquipSlot::MainHand });

                            let oh_label = eq
                                .off_hand
                                .as_ref()
                                .map(|w| format!("Левая: {}", weapon_name(w, content)))
                                .unwrap_or_else(|| "Левая: —".into());
                            spawn_standard_button(
                                slots_row, font.clone(), oh_label,
                                Val::Auto, Val::Auto, ButtonStyle::Default,
                            )
                            .insert(EquipSlotButton { hero_id: id.clone(), slot: EquipSlot::OffHand });

                            let chest_label =
                                format!("Нагр: {}", armor_name(&eq.chest, content));
                            spawn_standard_button(
                                slots_row, font.clone(), chest_label,
                                Val::Auto, Val::Auto, ButtonStyle::Default,
                            )
                            .insert(EquipSlotButton { hero_id: id.clone(), slot: EquipSlot::Chest });

                            let legs_label =
                                format!("Ноги: {}", armor_name(&eq.legs, content));
                            spawn_standard_button(
                                slots_row, font.clone(), legs_label,
                                Val::Auto, Val::Auto, ButtonStyle::Default,
                            )
                            .insert(EquipSlotButton { hero_id: id.clone(), slot: EquipSlot::Legs });

                            let feet_label =
                                format!("Обувь: {}", armor_name(&eq.feet, content));
                            spawn_standard_button(
                                slots_row, font.clone(), feet_label,
                                Val::Auto, Val::Auto, ButtonStyle::Default,
                            )
                            .insert(EquipSlotButton { hero_id: id.clone(), slot: EquipSlot::Feet });
                        });
                });
            }

            // Stash header
            root.spawn((
                Text::new("Инвентарь:"),
                TextFont { font: font.clone(), font_size: 14.0, ..default() },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
            ));

            // Stash items
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(8.0),
                row_gap: Val::Px(4.0),
                width: Val::Px(720.0),
                ..default()
            })
            .with_children(|stash_row| {
                for (i, item) in campaign.stash.iter().enumerate() {
                    let label = match item {
                        ItemRef::Weapon(wid) => weapon_name(wid, content).to_string(),
                        ItemRef::Armor(aid) => armor_name(aid, content).to_string(),
                    };
                    spawn_standard_button(
                        stash_row, font.clone(), label,
                        Val::Auto, Val::Auto, ButtonStyle::Default,
                    )
                    .insert(StashItemButton { stash_index: i });
                }
            });

            // Continue button
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

/// After an equip, rebuilds the camp UI in-place (despawn + respawn).
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

/// Handles slot selection, stash-item clicks, and Continue.
/// Runs under `run_if(in_state(AppState::Camp))`.
#[allow(clippy::too_many_arguments)]
pub fn camp_interaction_system(
    keys: Res<ButtonInput<KeyCode>>,
    slot_buttons: Query<(&Interaction, &EquipSlotButton), Changed<Interaction>>,
    stash_buttons: Query<(&Interaction, &StashItemButton), Changed<Interaction>>,
    continue_buttons: Query<&Interaction, (Changed<Interaction>, With<CampContinueButton>)>,
    mut selection: ResMut<CampEquipSelection>,
    mut campaign: Option<ResMut<CampaignState>>,
    active_content: Res<ActiveContent>,
    mut next_state: ResMut<NextState<AppState>>,
    mut rebuild: ResMut<CampNeedsRebuild>,
    db: Res<GameDb>,
    scenario_state: Res<ScenarioState>,
) {
    let key_continue = keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter);
    let btn_continue = continue_buttons.iter().any(|i| *i == Interaction::Pressed);

    if key_continue || btn_continue {
        next_state.set(AppState::Story);
        return;
    }

    // --- Slot button clicked: select it ---
    for (interaction, btn) in &slot_buttons {
        if *interaction == Interaction::Pressed {
            selection.selected = Some((btn.hero_id.clone(), btn.slot.clone()));
        }
    }

    // --- Stash button clicked: try to equip into the selected slot ---
    for (interaction, stash_btn) in &stash_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some((hero_id, slot)) = selection.selected.clone() else {
            // No slot selected yet.
            continue;
        };
        let Some(ref mut camp) = campaign else { continue };

        let item = match camp.stash.get(stash_btn.stash_index) {
            Some(i) => i.clone(),
            None => continue,
        };

        // Resolve current save: saved loadout if present, else class defaults.
        // Using resolve_hero_equipment ensures a first equip writes a complete snapshot
        // (real class-default armor in untouched slots) rather than empty sentinels.
        let scen = db.scenarios.get(&scenario_state.scenario_id).unwrap();
        let party = crate::content::scenarios::active_party(scen, scenario_state.scene_index);
        let class_id = party
            .iter()
            .find(|m| m.id == hero_id)
            .map(|m| m.class_id.clone())
            .unwrap_or_default();
        let content = &active_content.0;
        let current_save = resolve_hero_equipment(&hero_id, &class_id, camp, content);

        match try_equip(&current_save, &slot, item.clone(), &content.weapons, &content.armor) {
            Ok(result) => {
                // Remove the equipped item from the stash.
                camp.stash.remove(stash_btn.stash_index);

                // Two-handed weapon: if there was an off-hand item, return it to stash.
                if let ItemRef::Weapon(wid) = &item {
                    if let Some(def) = content.weapons.get(wid) {
                        if def.hand == HandType::TwoHanded {
                            if let Some(old_oh) = current_save.off_hand.as_ref() {
                                camp.stash.push(ItemRef::Weapon(old_oh.clone()));
                            }
                        }
                    }
                }

                // Displaced item returns to stash (skip empty sentinel armor ids).
                if let Some(displaced) = result.displaced {
                    let is_empty_sentinel = match &displaced {
                        ItemRef::Armor(aid) => aid.0.is_empty(),
                        ItemRef::Weapon(_) => false,
                    };
                    if !is_empty_sentinel {
                        camp.stash.push(displaced);
                    }
                }

                // Write the updated loadout.
                camp.loadouts.insert(hero_id.clone(), result.new_save);

                // Clear selection and request UI rebuild.
                selection.selected = None;
                rebuild.0 = true;
            }
            Err(e) => {
                warn!("camp: equip failed for hero '{}': {:?}", hero_id, e);
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
}
