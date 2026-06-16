//! Unit inspection panel — shown when the player left-clicks any unit.
//!
//! Displays name, combat stats, resources, active statuses (coloured by
//! buff/debuff/neutral), and ability list for the inspected unit.
//! Gated on `UiDirtyFlags::INSPECT`.
//!
//! Absolute-positioned bottom-right, above the turn-order strip; hidden until
//! `SelectionState.inspected` is `Some`.

use super::{
    InspectAbilityText, InspectNameText, InspectPanel, InspectResourcesText, InspectStatsText,
    InspectStatusText,
};
use crate::content::content_view::ActiveContent;
use crate::game::components::{
    Abilities, ActionPoints, CombatStats, Energy, Faction, Mana, Rage, RuntimeStatsMirror,
    StatusEffects, Team, Vital,
};
use crate::game::resources::{SelectionState, UiDirty, UiDirtyFlags};
use crate::ui::hex_grid::{classify_status, StatusTint};
use bevy::prelude::*;

// ── Layout constants ──────────────────────────────────────────────────────────

const PANEL_WIDTH: f32 = 170.0;
/// Gap between the panel right edge and the screen right edge (turn-order width + padding).
const RIGHT_OFFSET: f32 = 168.0;
/// Gap from the bottom edge.
const BOTTOM_OFFSET: f32 = 12.0;

const CLR_BG: Color = Color::srgba(0.07, 0.07, 0.09, 0.93);
const CLR_BORDER: Color = Color::srgb(0.28, 0.28, 0.34);
const CLR_HEADER: Color = Color::srgb(0.92, 0.88, 0.60);
const CLR_SECTION: Color = Color::srgb(0.55, 0.75, 0.95);
const CLR_BODY: Color = Color::srgb(0.80, 0.80, 0.86);

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the inspect panel as a top-level (non-flex-child) absolute node.
pub fn spawn_inspect_panel(commands: &mut Commands, font: &Handle<Font>) {
    let text_font = |size: f32| TextFont {
        font: font.clone(),
        font_size: size,
        ..default()
    };

    commands
        .spawn((
            InspectPanel,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(RIGHT_OFFSET),
                bottom: Val::Px(BOTTOM_OFFSET),
                width: Val::Px(PANEL_WIDTH),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(8.0)),
                row_gap: Val::Px(4.0),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(CLR_BORDER),
            BackgroundColor(CLR_BG),
            Visibility::Hidden,
            ZIndex(40),
        ))
        .with_children(|panel| {
            // Name header
            panel.spawn((
                InspectNameText,
                Text::new(""),
                text_font(13.0),
                TextColor(CLR_HEADER),
            ));

            // Stats line
            panel.spawn((
                InspectStatsText,
                Text::new(""),
                text_font(11.0),
                TextColor(CLR_BODY),
            ));

            // Resources line
            panel.spawn((
                InspectResourcesText,
                Text::new(""),
                text_font(11.0),
                TextColor(CLR_BODY),
            ));

            // Statuses section label + text
            panel.spawn((
                Text::new("Статусы:"),
                text_font(11.0),
                TextColor(CLR_SECTION),
            ));
            panel.spawn((
                InspectStatusText,
                Text::new(""),
                text_font(11.0),
                TextColor(CLR_BODY),
            ));

            // Abilities section label + text
            panel.spawn((
                Text::new("Способности:"),
                text_font(11.0),
                TextColor(CLR_SECTION),
            ));
            panel.spawn((
                InspectAbilityText,
                Text::new(""),
                text_font(11.0),
                TextColor(CLR_BODY),
            ));
        });
}

// ── Update ────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_inspect_panel(
    dirty: Res<UiDirty>,
    sel: Res<SelectionState>,
    content: Res<ActiveContent>,
    unit_q: Query<(
        &Name,
        &Vital,
        &CombatStats,
        &StatusEffects,
        &Abilities,
        &Faction,
        Option<&Mana>,
        Option<&Rage>,
        Option<&Energy>,
        &ActionPoints,
        Option<&RuntimeStatsMirror>,
    )>,
    mut panel_q: Query<&mut Visibility, With<InspectPanel>>,
    mut name_q: Query<
        &mut Text,
        (
            With<InspectNameText>,
            Without<InspectStatsText>,
            Without<InspectResourcesText>,
            Without<InspectStatusText>,
            Without<InspectAbilityText>,
        ),
    >,
    mut stats_q: Query<
        &mut Text,
        (
            With<InspectStatsText>,
            Without<InspectNameText>,
            Without<InspectResourcesText>,
            Without<InspectStatusText>,
            Without<InspectAbilityText>,
        ),
    >,
    mut res_q: Query<
        &mut Text,
        (
            With<InspectResourcesText>,
            Without<InspectNameText>,
            Without<InspectStatsText>,
            Without<InspectStatusText>,
            Without<InspectAbilityText>,
        ),
    >,
    mut status_q: Query<
        &mut Text,
        (
            With<InspectStatusText>,
            Without<InspectNameText>,
            Without<InspectStatsText>,
            Without<InspectResourcesText>,
            Without<InspectAbilityText>,
        ),
    >,
    mut ability_q: Query<
        &mut Text,
        (
            With<InspectAbilityText>,
            Without<InspectNameText>,
            Without<InspectStatsText>,
            Without<InspectResourcesText>,
            Without<InspectStatusText>,
        ),
    >,
) {
    if !dirty.0.contains(UiDirtyFlags::INSPECT) {
        return;
    }

    let Ok(mut vis) = panel_q.single_mut() else {
        return;
    };

    let Some(entity) = sel.inspected else {
        *vis = Visibility::Hidden;
        return;
    };

    let Ok((name, vital, stats, statuses, abilities, faction, mana, rage, energy, ap, runtime_opt)) =
        unit_q.get(entity)
    else {
        // Entity may have been despawned (e.g. end of combat) — hide gracefully.
        *vis = Visibility::Hidden;
        return;
    };

    // ── Name ──────────────────────────────────────────────────────────────────
    let team_label = if faction.0 == Team::Player {
        "союзник"
    } else {
        "враг"
    };
    if let Ok(mut t) = name_q.single_mut() {
        t.0 = format!("{} ({})", name.as_str(), team_label);
    }

    // ── Stats ─────────────────────────────────────────────────────────────────
    if let Ok(mut t) = stats_q.single_mut() {
        t.0 = format!(
            "СИЛ {} | ЛОВ {} | ВЫН {} | ИНТ {} | МУД {} | ХАР {}",
            stats.strength,
            stats.dexterity,
            stats.constitution,
            stats.intelligence,
            stats.wisdom,
            stats.charisma,
        );
    }

    // ── Resources ─────────────────────────────────────────────────────────────
    if let Ok(mut t) = res_q.single_mut() {
        let armor = runtime_opt.map_or(0, |r| r.0.armor);
        let mut parts = vec![
            format!("HP {}/{}", vital.hp, vital.max_hp),
            format!("Броня {}", armor),
        ];
        if let Some(m) = mana {
            parts.push(format!("Мана {}/{}", m.current, m.max));
        }
        if let Some(r) = rage {
            parts.push(format!("Ярость {}/{}", r.current, r.max));
        }
        if let Some(e) = energy {
            parts.push(format!("Энергия {}/{}", e.current, e.max));
        }
        parts.push(format!("AP {}/{}", ap.action_points, ap.max_ap));
        t.0 = parts.join("  ");
    }

    // ── Statuses ──────────────────────────────────────────────────────────────
    if let Ok(mut t) = status_q.single_mut() {
        if statuses.0.is_empty() {
            t.0 = "—".to_string();
        } else {
            let lines: Vec<String> = statuses
                .0
                .iter()
                .map(|s| {
                    let def = content.statuses.get(&s.id);
                    let status_name = def.map(|d| d.name.as_str()).unwrap_or("?");
                    let tint = def.map(classify_status).unwrap_or(StatusTint::Neutral);
                    // Colour prefix: ▲ buff, ▼ debuff, • neutral
                    let marker = match tint {
                        StatusTint::Buff => "▲",
                        StatusTint::Debuff => "▼",
                        StatusTint::Neutral => "•",
                    };
                    format!("{} {} ({} ход.)", marker, status_name, s.rounds_remaining)
                })
                .collect();
            t.0 = lines.join("\n");
        }
    }

    // ── Abilities ─────────────────────────────────────────────────────────────
    if let Ok(mut t) = ability_q.single_mut() {
        if abilities.0.is_empty() {
            t.0 = "—".to_string();
        } else {
            let lines: Vec<String> = abilities
                .0
                .iter()
                .map(|id| {
                    let ability_name = content
                        .abilities
                        .get(id)
                        .map(|d| d.name.as_str())
                        .unwrap_or(id.0.as_str());
                    ability_name.to_string()
                })
                .collect();
            t.0 = lines.join("\n");
        }
    }

    *vis = Visibility::Visible;
}

// `classify_status` is already parametrically tested in `src/ui/hex_grid/visuals.rs`.
// No duplicate coverage needed here.
