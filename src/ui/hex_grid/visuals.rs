#![allow(clippy::type_complexity)]
use super::render::{
    HexBorder, HexCellLink, HexGridOffset, HexHover, HexHpLabel, HexManaLabel, HexMaterials,
    HexNameLabel, HexStatusBadge, STATUS_BADGE_SLOTS,
};
use crate::combat::bridge::CombatStateRes;
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ActiveContent;
use crate::content::statuses::StatusDef;
use crate::game::components::{
    ActionPoints, ActiveCombatant, Dead, Energy, Faction, HexCombatantQ, Mana, Rage, StatusEffects,
    Team, UnitToken, Vital,
};
use crate::game::hex::{hex_circle, hex_line, Hex, LAYOUT};
use crate::game::hex_map::HexMap;
use crate::game::pathfinding::{reach_from, MovementEnv};
use crate::game::resources::{
    HexCorpses, HexPositions, SelectionState, TurnQueue, UiDirty, UiDirtyFlags,
};
use crate::ui::animation::MovePath;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use std::collections::HashSet;

// ── System: UI dirty bridge ───────────────────────────────────────────────────

#[derive(Default)]
pub struct DirtyBridgePrev {
    active: Option<Entity>,
    ability: Option<combat_engine::AbilityId>,
    move_mode: bool,
    target: Option<Entity>,
    hover: Option<Hex>,
    pos_gen: u64,
    corpse_gen: u64,
    initialized: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn ui_dirty_bridge(
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    positions: Res<HexPositions>,
    corpses: Res<HexCorpses>,
    queue: Res<TurnQueue>,
    hover: Res<HexHover>,
    vitals_q: Query<(), Changed<Vital>>,
    dead_q: Query<(), Added<Dead>>,
    removed_dead: RemovedComponents<Dead>,
    mana_q: Query<(), Changed<Mana>>,
    rage_q: Query<(), Changed<Rage>>,
    energy_q: Query<(), Changed<Energy>>,
    ap_q: Query<(), Changed<ActionPoints>>,
    status_q: Query<(), Changed<StatusEffects>>,
    mut dirty: ResMut<UiDirty>,
    mut prev: Local<DirtyBridgePrev>,
) {
    if !prev.initialized {
        prev.initialized = true;
        dirty.0 = UiDirtyFlags::all();
        prev.active = active_q.single().ok();
        prev.ability = sel.selected_ability.clone();
        prev.move_mode = sel.move_mode;
        prev.target = sel.selected_target;
        prev.hover = hover.0;
        prev.pos_gen = positions.generation;
        prev.corpse_gen = corpses.generation;
        return;
    }

    dirty.0 = UiDirtyFlags::empty();

    if active_q.single().ok() != prev.active {
        prev.active = active_q.single().ok();
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::ABILITY_PANEL
            | UiDirtyFlags::TURN_ORDER
            | UiDirtyFlags::PHASE_HINT
            | UiDirtyFlags::MOVE_BTN;
    }

    let ability_changed = sel.selected_ability != prev.ability;
    if ability_changed {
        prev.ability = sel.selected_ability.clone();
        dirty.0 |= UiDirtyFlags::OVERLAY | UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::PHASE_HINT;
    }

    if sel.move_mode != prev.move_mode {
        prev.move_mode = sel.move_mode;
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::PHASE_HINT
            | UiDirtyFlags::MOVE_BTN
            | UiDirtyFlags::HEX_FILL;
    }

    if sel.selected_target != prev.target {
        prev.target = sel.selected_target;
        dirty.0 |= UiDirtyFlags::HEX_FILL;
    }

    if positions.generation != prev.pos_gen {
        prev.pos_gen = positions.generation;
        dirty.0 |= UiDirtyFlags::OVERLAY
            | UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::TOKENS
            | UiDirtyFlags::STATUS_BADGES;
    }

    // Corpse layer mutations: cell fill / token / labels read corpses via
    // HexMap, so a corpse-only change still needs UI refresh.  OVERLAY stays
    // unchanged — overlays query alive units only (movement reach, ability range).
    if corpses.generation != prev.corpse_gen {
        prev.corpse_gen = corpses.generation;
        dirty.0 |= UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::TOKENS
            | UiDirtyFlags::TOOLTIP
            | UiDirtyFlags::STATUS_BADGES;
    }

    if queue.is_changed() {
        dirty.0 |= UiDirtyFlags::TURN_ORDER;
    }

    if !vitals_q.is_empty() {
        dirty.0 |= UiDirtyFlags::LABELS | UiDirtyFlags::TURN_ORDER;
    }

    if !dead_q.is_empty() || !removed_dead.is_empty() {
        dirty.0 |= UiDirtyFlags::HEX_FILL | UiDirtyFlags::TOKENS | UiDirtyFlags::OVERLAY;
    }

    if !mana_q.is_empty() || !rage_q.is_empty() || !energy_q.is_empty() {
        dirty.0 |= UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::LABELS;
    }

    if !ap_q.is_empty() {
        dirty.0 |= UiDirtyFlags::OVERLAY | UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::MOVE_BTN;
    }

    if !status_q.is_empty() {
        dirty.0 |= UiDirtyFlags::STATUS_BADGES;
    }

    let hover_changed = hover.0 != prev.hover;
    if hover_changed {
        prev.hover = hover.0;
        dirty.0 |= UiDirtyFlags::TOOLTIP | UiDirtyFlags::HEX_FILL;
    }

    // Recompute forecast whenever hover or selected ability changes and an
    // ability is currently selected (cheap gate — compute_forecast itself
    // handles the case where the target is invalid and clears the forecast).
    if (hover_changed || ability_changed) && sel.selected_ability.is_some() {
        dirty.0 |= UiDirtyFlags::FORECAST;
    }
}

/// Combined overlay caches to stay within Bevy's system-param limit.
#[derive(Default)]
pub struct CachedOverlay {
    range: HashSet<Hex>,
    disadvantage: HashSet<Hex>,
    movement: HashSet<Hex>,
    aoe_preview: HashSet<Hex>,
}

// ── System: Update visuals ────────────────────────────────────────────────────

/// Bundled label + border queries for `update_hex_visuals`.
/// Grouped into a `SystemParam` to stay within Bevy's 16-param system limit.
#[derive(SystemParam)]
pub struct HexLabelParams<'w, 's> {
    pub borders: Query<
        'w,
        's,
        (
            &'static mut Visibility,
            &'static mut MeshMaterial2d<ColorMaterial>,
        ),
        (
            With<HexBorder>,
            Without<HexNameLabel>,
            Without<HexHpLabel>,
            Without<HexManaLabel>,
            Without<HexStatusBadge>,
        ),
    >,
    pub name_labels: Query<
        'w,
        's,
        (
            &'static HexCellLink,
            &'static mut Text2d,
            &'static mut Visibility,
        ),
        (
            With<HexNameLabel>,
            Without<HexBorder>,
            Without<HexHpLabel>,
            Without<HexManaLabel>,
            Without<HexStatusBadge>,
        ),
    >,
    pub hp_labels: Query<
        'w,
        's,
        (
            &'static HexCellLink,
            &'static mut Text2d,
            &'static mut Visibility,
        ),
        (
            With<HexHpLabel>,
            Without<HexBorder>,
            Without<HexNameLabel>,
            Without<HexManaLabel>,
            Without<HexStatusBadge>,
        ),
    >,
    pub mana_labels: Query<
        'w,
        's,
        (
            &'static HexCellLink,
            &'static mut Text2d,
            &'static mut Visibility,
        ),
        (
            With<HexManaLabel>,
            Without<HexBorder>,
            Without<HexNameLabel>,
            Without<HexHpLabel>,
            Without<HexStatusBadge>,
        ),
    >,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_hex_visuals(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    hover: Res<HexHover>,
    content: Res<ActiveContent>,
    engine_state: Res<CombatStateRes>,
    map: HexMap,
    mats: Res<HexMaterials>,
    cells: Query<(Entity, &Hex, &Children)>,
    combatant_q: Query<HexCombatantQ>,
    ap_q: Query<&ActionPoints>,
    mut labels: HexLabelParams,
    mut cell_mats: Query<&mut MeshMaterial2d<ColorMaterial>, (With<Hex>, Without<HexBorder>)>,
    mut overlay: Local<CachedOverlay>,
) {
    let flags = dirty.0;
    if !flags.intersects(
        UiDirtyFlags::OVERLAY
            | UiDirtyFlags::HEX_FILL
            | UiDirtyFlags::LABELS
            | UiDirtyFlags::TOOLTIP,
    ) {
        return;
    }

    if flags.contains(UiDirtyFlags::OVERLAY) {
        overlay.range = if !sel.move_mode {
            let info = active_q.single().ok().and_then(|e| map.position_of(e)).zip(
                sel.selected_ability
                    .as_ref()
                    .and_then(|id| content.abilities.get(id))
                    .filter(|ab| {
                        ab.is_actively_castable()
                            && ab.target_type != TargetType::Myself
                            && ab.range.max > 0
                    }),
            );
            if let Some((actor_pos, ab)) = info {
                // LOS filter: for abilities that require line-of-sight, exclude
                // cells the hex-line algorithm would block via `state.blocked_hexes`.
                // Without this the overlay shows cells that look reachable but
                // get rejected by check_legality at cast time (UI lie).
                let needs_los = ab.requires_los && ab.range.max > 1;
                let blocked = &engine_state.0.blocked_hexes;
                cells
                    .iter()
                    .filter(|(_, &hc, _)| {
                        let d = actor_pos.unsigned_distance_to(hc);
                        if d < ab.range.min || d > ab.range.max {
                            return false;
                        }
                        if needs_los {
                            crate::game::hex::has_los(actor_pos, hc, |mid| blocked.contains(&mid))
                        } else {
                            true
                        }
                    })
                    .map(|(_, &hc, _)| hc)
                    .collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

        // Disadvantage zone: cells within max range but below min range.
        overlay.disadvantage = if !sel.move_mode {
            let info = active_q.single().ok().and_then(|e| map.position_of(e)).zip(
                sel.selected_ability
                    .as_ref()
                    .and_then(|id| content.abilities.get(id))
                    .filter(|ab| {
                        ab.is_actively_castable()
                            && ab.target_type != TargetType::Myself
                            && ab.range.min > 0
                    }),
            );
            if let Some((actor_pos, ab)) = info {
                cells
                    .iter()
                    .filter(|(_, &hc, _)| {
                        let d = actor_pos.unsigned_distance_to(hc);
                        d > 0 && d < ab.range.min
                    })
                    .map(|(_, &hc, _)| hc)
                    .collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

        overlay.movement = if sel.move_mode {
            if let Ok(actor) = active_q.single() {
                if let (Some(actor_pos), Ok(ap)) = (map.position_of(actor), ap_q.get(actor)) {
                    let enemy_positions: HashSet<Hex> = map
                        .iter_living()
                        .filter(|(&e, _)| {
                            e != actor
                                && combatant_q
                                    .get(e)
                                    .is_ok_and(|c| c.faction.0 == Team::Enemy && !c.is_dead)
                        })
                        .map(|(_, &p)| p)
                        .collect();
                    let stop_blockers: HashSet<Hex> = map
                        .iter_living()
                        .filter(|(&e, _)| e != actor)
                        .map(|(_, &p)| p)
                        .collect();
                    let env = MovementEnv {
                        enemy_positions,
                        stop_blockers,
                        blocked_hexes: engine_state.0.blocked_hexes.clone(),
                        hazard_costs: std::collections::HashMap::new(), // UI never routes via hazards (T9 wires AI only)
                    };
                    reach_from(actor_pos, ap.movement_points, &env).destinations
                } else {
                    HashSet::new()
                }
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };
    }

    // AoE preview: recompute whenever hover or ability selection changes.
    if flags.intersects(UiDirtyFlags::TOOLTIP | UiDirtyFlags::OVERLAY) {
        overlay.aoe_preview = if let Some(hovered) = hover.0 {
            let actor_pos = active_q.single().ok().and_then(|e| map.position_of(e));
            let aoe_def = sel
                .selected_ability
                .as_ref()
                .and_then(|id| content.abilities.get(id))
                .filter(|ab| ab.aoe != AoEShape::None);
            if let (Some(a_pos), Some(ab)) = (actor_pos, aoe_def) {
                match ab.aoe {
                    AoEShape::None => HashSet::new(),
                    AoEShape::Circle { radius } => {
                        hex_circle(hovered, radius).into_iter().collect()
                    }
                    AoEShape::Line { length } => {
                        hex_line(a_pos, hovered, length).into_iter().collect()
                    }
                }
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };
    }

    let range_cells = &overlay.range;
    let disadv_cells = &overlay.disadvantage;
    let move_cells = &overlay.movement;
    let aoe_cells = &overlay.aoe_preview;
    let active = active_q.single().ok();

    for (cell_entity, &hex_cell, children) in &cells {
        let pos = hex_cell;
        let occupant = map.living_at(pos);
        let is_active = occupant.is_some_and(|e| active == Some(e));
        let is_target = occupant.is_some_and(|e| sel.selected_target == Some(e));
        let is_in_range = range_cells.contains(&pos);
        let is_disadv = disadv_cells.contains(&pos);
        let is_in_move = move_cells.contains(&pos);
        let is_aoe = aoe_cells.contains(&pos);

        // Cell fill resolves to living unit first, then corpse — so the gray
        // corpse tile shows when a unit died on an otherwise empty hex.
        let fill_entity = map.any_at(pos);

        let is_obstacle = engine_state.0.blocked_hexes.contains(&pos);
        let is_revealed_trap = engine_state
            .0
            .environment
            .iter()
            .any(|e| e.hex == pos && e.visible_to(Team::Player));

        if let Ok(mut mat) = cell_mats.get_mut(cell_entity) {
            mat.0 = match fill_entity {
                None => {
                    if is_obstacle {
                        mats.obstacle.clone()
                    } else if is_revealed_trap {
                        mats.trap.clone()
                    } else if is_aoe {
                        mats.aoe_preview.clone()
                    } else if is_in_move {
                        mats.move_range.clone()
                    } else if is_in_range {
                        mats.in_range.clone()
                    } else if is_disadv {
                        mats.in_range_dim.clone()
                    } else {
                        mats.empty.clone()
                    }
                }
                Some(e) => {
                    if let Ok(c) = combatant_q.get(e) {
                        if c.is_dead {
                            mats.dead.clone()
                        } else if c.faction.0 == Team::Player {
                            mats.player.clone()
                        } else {
                            mats.enemy.clone()
                        }
                    } else {
                        mats.empty.clone()
                    }
                }
            };
        }

        for child in children.iter() {
            if let Ok((mut vis, mut bmat)) = labels.borders.get_mut(child) {
                if is_active || is_target || is_aoe || is_in_move || is_in_range || is_disadv {
                    *vis = Visibility::Visible;
                    bmat.0 = if is_active {
                        mats.border_active.clone()
                    } else if is_target {
                        mats.border_target.clone()
                    } else if is_aoe {
                        mats.border_aoe.clone()
                    } else if is_in_move {
                        mats.border_move.clone()
                    } else if is_disadv {
                        mats.border_in_range_dim.clone()
                    } else {
                        mats.border_in_range.clone()
                    };
                } else {
                    *vis = Visibility::Hidden;
                }
            }
        }
    }

    if !flags.contains(UiDirtyFlags::LABELS) {
        return;
    }

    for (link, mut text, mut vis) in &mut labels.name_labels {
        if let Some(c) =
            super::render::label_occupant(link, &cells, &map).and_then(|e| combatant_q.get(e).ok())
        {
            let n = c.name.as_str();
            text.0 = if n.chars().count() > 8 {
                n.chars().take(7).collect::<String>() + "."
            } else {
                n.to_string()
            };
            *vis = Visibility::Visible;
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (link, mut text, mut vis) in &mut labels.hp_labels {
        if let Some(c) =
            super::render::label_occupant(link, &cells, &map).and_then(|e| combatant_q.get(e).ok())
        {
            text.0 = format!("{}/{}", c.vital.hp, c.vital.max_hp);
            *vis = Visibility::Visible;
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (link, mut text, mut vis) in &mut labels.mana_labels {
        if let Some(c) =
            super::render::label_occupant(link, &cells, &map).and_then(|e| combatant_q.get(e).ok())
        {
            if let Some(m) = c.mana {
                text.0 = format!("M:{}/{}", m.current, m.max);
                *vis = Visibility::Visible;
            } else if let Some(r) = c.rage {
                text.0 = format!("R:{}/{}", r.current, r.max);
                *vis = Visibility::Visible;
            } else if let Some(e) = c.energy {
                text.0 = format!("E:{}/{}", e.current, e.max);
                *vis = Visibility::Visible;
            } else {
                *vis = Visibility::Hidden;
            }
        } else {
            *vis = Visibility::Hidden;
        }
    }
}

// ── Status badge helpers ──────────────────────────────────────────────────────

/// Tint category used for badge coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTint {
    /// Buffs: green. Heuristic: has a positive `buff_class` or positive armor.
    Buff,
    /// Debuffs: red. Heuristic: deals DoT, skips turn, or causes disadvantage.
    Debuff,
    /// Neither clearly buff nor debuff: gray.
    Neutral,
}

impl StatusTint {
    pub fn to_color(self) -> Color {
        match self {
            StatusTint::Buff => Color::srgb(0.30, 0.85, 0.40),
            StatusTint::Debuff => Color::srgb(0.90, 0.25, 0.20),
            StatusTint::Neutral => Color::srgb(0.65, 0.65, 0.65),
        }
    }
}

/// Classify a status definition as buff, debuff, or neutral.
///
/// Rule (simple, documented so the inspection panel can reuse it):
/// - **Debuff** if `dot_dice.is_some()` OR `skips_turn` OR `causes_disadvantage`.
/// - **Buff** if `buff_class.is_some()` OR `armor_bonus > 0`.
/// - **Neutral** otherwise.
///
/// Debuff check takes priority over buff to handle mixed-stats statuses correctly.
pub fn classify_status(def: &StatusDef) -> StatusTint {
    if def.dot_dice.is_some() || def.skips_turn || def.causes_disadvantage {
        StatusTint::Debuff
    } else if def.buff_class.is_some() || def.bonuses.runtime.0.armor > 0 {
        StatusTint::Buff
    } else {
        StatusTint::Neutral
    }
}

/// Produce the short abbreviation displayed on a badge (max 3 chars).
///
/// Takes the first char that is a Cyrillic or Latin letter from the name,
/// plus the second such char if present, to keep it compact on small badges.
fn status_abbrev(name: &str) -> String {
    let mut letters = name.chars().filter(|c| c.is_alphabetic()).take(2);
    match (letters.next(), letters.next()) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => a.to_string(),
        _ => "?".to_string(),
    }
}

/// Updates the status-badge label slots for all hex cells.
///
/// Gated on `UiDirtyFlags::STATUS_BADGES`. Each occupied cell shows up to
/// `STATUS_BADGE_SLOTS` badges (colored abbrev + rounds). Slots past the
/// unit's count are hidden. If a unit has more statuses than fit, the last
/// visible slot shows "+k" overflow text.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_hex_status_badges(
    dirty: Res<UiDirty>,
    content: Res<ActiveContent>,
    map: HexMap,
    cells: Query<(Entity, &Hex, &Children)>,
    combatant_q: Query<HexCombatantQ>,
    status_q: Query<&StatusEffects>,
    mut badge_q: Query<
        (
            &HexCellLink,
            &HexStatusBadge,
            &mut Text2d,
            &mut TextColor,
            &mut Visibility,
        ),
        (
            With<HexStatusBadge>,
            Without<HexNameLabel>,
            Without<HexHpLabel>,
            Without<HexManaLabel>,
            Without<HexBorder>,
        ),
    >,
) {
    if !dirty.0.contains(UiDirtyFlags::STATUS_BADGES) {
        return;
    }

    for (link, badge, mut text, mut color, mut vis) in &mut badge_q {
        let occupant = super::render::label_occupant(link, &cells, &map);

        // Only show badges for alive (non-dead) units.
        let is_alive = occupant
            .and_then(|e| combatant_q.get(e).ok())
            .map(|c| !c.is_dead)
            .unwrap_or(false);

        let statuses = occupant
            .filter(|_| is_alive)
            .and_then(|e| status_q.get(e).ok())
            .map(|se| &se.0);

        let Some(statuses) = statuses else {
            *vis = Visibility::Hidden;
            continue;
        };

        if statuses.is_empty() {
            *vis = Visibility::Hidden;
            continue;
        }

        let total = statuses.len();
        let slot = badge.slot as usize;
        let max_slots = STATUS_BADGE_SLOTS as usize;

        // If this slot is beyond what we need, hide it.
        if slot >= total.min(max_slots) {
            *vis = Visibility::Hidden;
            continue;
        }

        // Overflow: last slot shows "+k" when total > max_slots.
        let overflow = total > max_slots;
        if overflow && slot == max_slots - 1 {
            let extra = total - (max_slots - 1);
            text.0 = format!("+{extra}");
            color.0 = StatusTint::Neutral.to_color();
            *vis = Visibility::Visible;
            continue;
        }

        // Normal slot: abbrev + rounds.
        let active = &statuses[slot];
        let def = content.statuses.get(&active.id);
        let (abbrev, tint) = if let Some(d) = def {
            (status_abbrev(&d.name), classify_status(d))
        } else {
            // Unknown status — show id prefix.
            (status_abbrev(&active.id.0), StatusTint::Neutral)
        };

        text.0 = if active.rounds_remaining > 0 {
            format!("{abbrev}{}", active.rounds_remaining)
        } else {
            abbrev
        };
        color.0 = tint.to_color();
        *vis = Visibility::Visible;
    }
}

// ── System: Update token positions ────────────────────────────────────────────

/// Syncs UnitToken transforms with the hex position of each token's entity.
/// Living units use team color; dead units use `token_dead` (gray) and are
/// positioned at their corpse-layer hex so the tombstone sprite stays visible.
pub fn update_token_positions(
    dirty: Res<UiDirty>,
    map: HexMap,
    grid_offset: Res<HexGridOffset>,
    mats: Res<HexMaterials>,
    mut tokens: Query<(
        &UnitToken,
        &mut Transform,
        &mut MeshMaterial2d<ColorMaterial>,
        &mut Visibility,
        Has<MovePath>,
    )>,
    combatant_q: Query<(&Faction, Has<Dead>)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TOKENS) {
        return;
    }
    for (token, mut transform, mut mat, mut vis, is_moving) in &mut tokens {
        if is_moving {
            *vis = Visibility::Visible;
            continue;
        }

        let Some(pos) = map.position_of(token.0) else {
            *vis = Visibility::Hidden;
            continue;
        };

        let Ok((faction, is_dead)) = combatant_q.get(token.0) else {
            *vis = Visibility::Hidden;
            continue;
        };

        let pixel = LAYOUT.hex_to_world_pos(pos) + grid_offset.0;
        transform.translation.x = pixel.x;
        transform.translation.y = pixel.y;

        mat.0 = if is_dead {
            mats.token_dead.clone()
        } else if faction.0 == Team::Player {
            mats.token_player.clone()
        } else {
            mats.token_enemy.clone()
        };

        *vis = Visibility::Visible;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::statuses::{BuffClass, StatusDef};
    use combat_engine::StatusId;

    fn base_engine_status() -> combat_engine::StatusDef {
        combat_engine::StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: false,
            bonuses: combat_engine::StatusBonuses::default(),
            hp_percent_dot: 0,
            heal_per_tick: 0,
            ..Default::default()
        }
    }

    fn make_status(
        name: &str,
        dot_dice: Option<combat_engine::DiceExpr>,
        buff_class: Option<BuffClass>,
        engine: combat_engine::StatusDef,
    ) -> StatusDef {
        StatusDef {
            id: StatusId::from(name),
            name: name.to_string(),
            dot_dice,
            ai_controlled: false,
            buff_class,
            engine,
        }
    }

    /// Parametrised cases matching statuses from assets/data/statuses.toml.
    #[test]
    fn classify_status_cases() {
        struct Case {
            name: &'static str,
            def: fn() -> StatusDef,
            expected: StatusTint,
        }

        let cases = [
            Case {
                name: "defending (armor_bonus=4, buff_class=armor_buff) → Buff",
                def: || {
                    let mut eng = base_engine_status();
                    eng.bonuses.runtime.0.armor = 4;
                    make_status("defending", None, Some(BuffClass::ArmorBuff), eng)
                },
                expected: StatusTint::Buff,
            },
            Case {
                name: "poisoned (dot_dice=1d4) → Debuff",
                def: || {
                    make_status(
                        "poisoned",
                        Some(combat_engine::DiceExpr::new(1, 4, 0)),
                        None,
                        base_engine_status(),
                    )
                },
                expected: StatusTint::Debuff,
            },
            Case {
                name: "stunned (skips_turn) → Debuff",
                def: || {
                    let mut eng = base_engine_status();
                    eng.skips_turn = true;
                    make_status("stunned", None, None, eng)
                },
                expected: StatusTint::Debuff,
            },
            Case {
                name: "disoriented (causes_disadvantage) → Debuff",
                def: || {
                    let mut eng = base_engine_status();
                    eng.causes_disadvantage = true;
                    make_status("disoriented", None, None, eng)
                },
                expected: StatusTint::Debuff,
            },
            Case {
                name: "broken_faith (blocks_mana only) → Neutral",
                def: || {
                    let mut eng = base_engine_status();
                    eng.blocks_mana_abilities = true;
                    make_status("broken_faith", None, None, eng)
                },
                expected: StatusTint::Neutral,
            },
            Case {
                name: "armor_bonus > 0 but no buff_class → Buff",
                def: || {
                    let mut eng = base_engine_status();
                    eng.bonuses.runtime.0.armor = 2;
                    make_status("prototype_ward", None, None, eng)
                },
                expected: StatusTint::Buff,
            },
            Case {
                name: "dot_dice takes priority over buff_class (hypothetical mixed) → Debuff",
                def: || {
                    let mut eng = base_engine_status();
                    eng.bonuses.runtime.0.armor = 1;
                    make_status(
                        "mixed",
                        Some(combat_engine::DiceExpr::new(1, 6, 0)),
                        Some(BuffClass::ArmorBuff),
                        eng,
                    )
                },
                expected: StatusTint::Debuff,
            },
        ];

        for case in &cases {
            let def = (case.def)();
            let got = classify_status(&def);
            assert_eq!(got, case.expected, "classify_status: {}", case.name);
        }
    }

    #[test]
    fn status_abbrev_cyrillic() {
        assert_eq!(status_abbrev("Отравлен"), "От");
    }

    #[test]
    fn status_abbrev_latin() {
        assert_eq!(status_abbrev("Stunned"), "St");
    }

    #[test]
    fn status_abbrev_single_char() {
        assert_eq!(status_abbrev("X"), "X");
    }
}
