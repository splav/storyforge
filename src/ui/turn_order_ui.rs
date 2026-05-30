use crate::content::content_view::ActiveContent;
use crate::content::content_view::ContentView;
use super::{TurnOrderCard, TurnOrderCardHp, TurnOrderCardName, TurnOrderTooltip, TurnOrderTooltipText};
use crate::content::armor::ArmorDef;
use crate::content::weapons::WeaponDef;
use combat_engine::{ArmorId, WeaponId};
use crate::content::abilities::{AbilityDef, AoEShape, EffectDef, ResourceCost};
use combat_engine::ResourceKind;
use crate::game::components::{Abilities, ActiveCombatant, Combatant, Dead, Equipment, Faction, Team, Vital};
use crate::game::resources::{TurnQueue, UiDirty, UiDirtyFlags};
use bevy::prelude::*;

pub const MAX_TURN_CARDS: usize = 8;

const CLR_CARD_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_CARD_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_CARD_PLAYER_BG: Color = Color::srgb(0.08, 0.10, 0.16);
const CLR_CARD_PLAYER_BORDER: Color = Color::srgb(0.25, 0.35, 0.60);
const CLR_CARD_ENEMY_BG: Color = Color::srgb(0.16, 0.08, 0.08);
const CLR_CARD_ENEMY_BORDER: Color = Color::srgb(0.55, 0.20, 0.20);
const CLR_CARD_ACTIVE_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
const CLR_CARD_DEAD_BG: Color = Color::srgb(0.06, 0.06, 0.07);
const CLR_CARD_DEAD_BORDER: Color = Color::srgb(0.18, 0.18, 0.20);

/// Spawns the right-side turn order panel as a child of the root row node.
pub fn spawn_turn_order_panel(parent: &mut ChildSpawnerCommands, font: &Handle<Font>) {
    let txt = |size: f32| -> TextFont {
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        }
    };

    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            justify_content: JustifyContent::FlexStart,
            row_gap: Val::Px(4.0),
            padding: UiRect::all(Val::Px(8.0)),
            width: Val::Px(160.0),
            ..default()
        })
        .with_children(|panel| {
            for i in 0..MAX_TURN_CARDS {
                panel
                    .spawn((
                        TurnOrderCard(i),
                        Button,
                        Node {
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::Center,
                            padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                            border: UiRect::all(Val::Px(1.5)),
                            width: Val::Percent(100.0),
                            min_height: Val::Px(44.0),
                            ..default()
                        },
                        BorderColor::all(CLR_CARD_BORDER),
                        BackgroundColor(CLR_CARD_BG),
                        Visibility::Hidden,
                    ))
                    .with_children(|card| {
                        card.spawn((
                            TurnOrderCardName(i),
                            Text::new(""),
                            txt(12.0),
                            TextColor(Color::WHITE),
                        ));
                        card.spawn((
                            TurnOrderCardHp(i),
                            Text::new(""),
                            txt(11.0),
                            TextColor(Color::srgb(0.55, 0.85, 0.55)),
                        ));
                    });
            }
        });
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn update_turn_order(
    dirty: Res<UiDirty>,
    queue: Res<TurnQueue>,
    combatants: Query<(&Name, &Vital, &Faction, Has<Dead>), With<Combatant>>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut cards: Query<(
        &TurnOrderCard,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Visibility,
    )>,
    mut name_texts: Query<(&TurnOrderCardName, &mut Text, &mut TextColor)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TURN_ORDER) {
        return;
    }

    let len = queue.order.len();
    if len == 0 {
        for (_, _, _, mut vis) in &mut cards {
            *vis = Visibility::Hidden;
        }
        for (_, mut text, _) in &mut name_texts {
            text.0.clear();
        }
        return;
    }

    // Find the active-slot index: the position in queue.order of the entity
    // that currently holds ActiveCombatant.  Falls back to queue.index if no
    // ActiveCombatant is set (e.g. during a brief transition frame).
    let active_idx = active_q
        .single()
        .ok()
        .and_then(|ent| queue.order.iter().position(|&e| e == ent))
        .unwrap_or(queue.index);

    let display: Vec<(usize, bool)> = (0..len.min(MAX_TURN_CARDS))
        .map(|i| ((active_idx + i) % len, i == 0))
        .collect();

    for (card, mut border, mut bg, mut vis) in &mut cards {
        let slot = card.0;
        if let Some(&(queue_idx, is_active)) = display.get(slot) {
            let entity = queue.order[queue_idx];
            if let Ok((_, _, faction, is_dead)) = combatants.get(entity) {
                *vis = Visibility::Visible;
                if is_dead {
                    *border = BorderColor::all(CLR_CARD_DEAD_BORDER);
                    *bg = BackgroundColor(CLR_CARD_DEAD_BG);
                } else if is_active {
                    *border = BorderColor::all(CLR_CARD_ACTIVE_BORDER);
                    *bg = BackgroundColor(match faction.0 {
                        Team::Player => CLR_CARD_PLAYER_BG,
                        Team::Enemy => CLR_CARD_ENEMY_BG,
                    });
                } else {
                    let (bg_clr, border_clr) = match faction.0 {
                        Team::Player => (CLR_CARD_PLAYER_BG, CLR_CARD_PLAYER_BORDER),
                        Team::Enemy => (CLR_CARD_ENEMY_BG, CLR_CARD_ENEMY_BORDER),
                    };
                    *border = BorderColor::all(border_clr);
                    *bg = BackgroundColor(bg_clr);
                }
            }
        } else {
            *vis = Visibility::Hidden;
        }
    }

    for (label, mut text, mut color) in &mut name_texts {
        let slot = label.0;
        if let Some(&(queue_idx, is_active)) = display.get(slot) {
            let entity = queue.order[queue_idx];
            if let Ok((name, _, faction, is_dead)) = combatants.get(entity) {
                let prefix = if is_active { "▶ " } else { "  " };
                let team_icon = match faction.0 {
                    Team::Player => "⚔",
                    Team::Enemy => "☠",
                };
                let dead_suffix = if is_dead { " ✗" } else { "" };
                text.0 = format!("{prefix}{team_icon} {}{dead_suffix}", name.as_str());
                *color = TextColor(if is_dead {
                    Color::srgb(0.35, 0.35, 0.38)
                } else if is_active {
                    Color::srgb(1.0, 0.95, 0.5)
                } else {
                    Color::WHITE
                });
            }
        } else {
            text.0.clear();
        }
    }
}

pub fn update_turn_order_tooltip(
    queue: Res<TurnQueue>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    cards: Query<(&TurnOrderCard, &Interaction)>,
    combatants: Query<(&Name, &Equipment, &Abilities), With<Combatant>>,
    content: Res<ActiveContent>,
    mut tooltip_vis: Query<&mut Visibility, With<TurnOrderTooltip>>,
    mut tooltip_text: Query<&mut Text, With<TurnOrderTooltipText>>,
) {
    let Ok(mut vis) = tooltip_vis.single_mut() else { return };
    let Ok(mut text) = tooltip_text.single_mut() else { return };

    let hovered = cards.iter().find(|(_, i)| **i == Interaction::Hovered);
    let Some((card, _)) = hovered else {
        *vis = Visibility::Hidden;
        return;
    };

    let len = queue.order.len();
    if len == 0 || card.0 >= len.min(MAX_TURN_CARDS) {
        *vis = Visibility::Hidden;
        return;
    }

    // Use ActiveCombatant to find the active slot index (same logic as update_turn_order).
    let active_idx = active_q
        .single()
        .ok()
        .and_then(|ent| queue.order.iter().position(|&e| e == ent))
        .unwrap_or(queue.index);

    let queue_idx = (active_idx + card.0) % len;
    let entity = queue.order[queue_idx];

    let Ok((name, equipment, abilities)) = combatants.get(entity) else {
        *vis = Visibility::Hidden;
        return;
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("── {} ──", name.as_str()));

    if let Some(ref wid) = equipment.main_hand {
        lines.push(weapon_line("Гл. рука", wid, &content));
    } else {
        lines.push("Гл. рука: —".into());
    }
    if let Some(ref wid) = equipment.off_hand {
        lines.push(weapon_line("Доп. рука", wid, &content));
    }
    lines.push(armor_line("Нагрудник", &equipment.chest, &content));
    lines.push(armor_line("Ноги", &equipment.legs, &content));
    lines.push(armor_line("Обувь", &equipment.feet, &content));

    lines.push(String::new());
    lines.push("── Способности ──".into());
    for aid in &abilities.0 {
        if let Some(def) = content.abilities.get(aid) {
            lines.push(ability_line(def));
        }
    }

    text.0 = lines.join("\n");
    *vis = Visibility::Visible;
}

fn weapon_line(slot: &str, id: &WeaponId, content: &ContentView) -> String {
    if let Some(w) = content.weapons.get(id) {
        let dice_str = format!("{}d{}", w.dice.count, w.dice.sides);
        let bonuses = weapon_bonus_str(w);
        if bonuses.is_empty() {
            format!("{slot}: {}  {dice_str}", w.name)
        } else {
            format!("{slot}: {}  {dice_str}  ({bonuses})", w.name)
        }
    } else {
        format!("{slot}: {}", id.0)
    }
}

fn armor_line(slot: &str, id: &ArmorId, content: &ContentView) -> String {
    if let Some(a) = content.armor.get(id) {
        let bonuses = armor_bonus_str(a);
        if bonuses.is_empty() {
            format!("{slot}: {}", a.name)
        } else {
            format!("{slot}: {}  ({bonuses})", a.name)
        }
    } else {
        format!("{slot}: {}", id.0)
    }
}

fn weapon_bonus_str(w: &WeaponDef) -> String {
    let mut parts: Vec<String> = Vec::new();
    if w.armor != 0 { parts.push(format!("броня {}", w.armor)); }
    if w.max_hp != 0 { parts.push(format!("хп {:+}", w.max_hp)); }
    if w.strength != 0 { parts.push(format!("сил {:+}", w.strength)); }
    if w.dexterity != 0 { parts.push(format!("лов {:+}", w.dexterity)); }
    if w.constitution != 0 { parts.push(format!("тел {:+}", w.constitution)); }
    if w.intelligence != 0 { parts.push(format!("инт {:+}", w.intelligence)); }
    if w.wisdom != 0 { parts.push(format!("мдр {:+}", w.wisdom)); }
    if w.charisma != 0 { parts.push(format!("хар {:+}", w.charisma)); }
    if w.spell_power != 0 { parts.push(format!("маг {:+}", w.spell_power)); }
    parts.join(", ")
}

fn ability_line(def: &AbilityDef) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Effect / dice
    match &def.effect {
        EffectDef::WeaponAttack => parts.push("ближний бой".into()),
        EffectDef::Damage { dice } => parts.push(format!("{}d{}", dice.count, dice.sides)),
        EffectDef::SpellDamage { dice } => parts.push(format!("{}d{} маг", dice.count, dice.sides)),
        EffectDef::Heal { dice } => parts.push(format!("лечение {}d{}", dice.count, dice.sides)),
        EffectDef::GrantMovement { distance } => parts.push(format!("+{} движ", distance)),
        EffectDef::RestoreResources => parts.push("ресурсы +1".into()),
        EffectDef::Summon { template_id, .. } => parts.push(format!("призыв ({template_id})")),
        EffectDef::RevealEnvInRange { range } => parts.push(format!("ловушки r{range}")),
        EffectDef::None => {}
    }

    // Range
    if def.range.max > 0 {
        if def.range.min > 0 {
            parts.push(format!("{}-{} кл", def.range.min, def.range.max));
        } else {
            parts.push(format!("{} кл", def.range.max));
        }
    }

    // AoE
    match def.aoe {
        AoEShape::Circle { radius } => parts.push(format!("обл r{radius}")),
        AoEShape::Line { length } => parts.push(format!("линия {length}")),
        AoEShape::None => {}
    }

    // Cost
    let cost_str = cost_summary(&def.costs);
    if !cost_str.is_empty() {
        parts.push(cost_str);
    }

    format!("  {} — {}", def.name, parts.join(", "))
}

fn cost_summary(costs: &[ResourceCost]) -> String {
    costs
        .iter()
        .map(|c| {
            let label = match c.resource {
                ResourceKind::Hp => "HP",
                ResourceKind::Mana => "мана",
                ResourceKind::Rage => "ярость",
                ResourceKind::Energy => "энергия",
            };
            format!("{} {}", c.amount, label)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn armor_bonus_str(a: &ArmorDef) -> String {
    let mut parts: Vec<String> = Vec::new();
    if a.armor != 0 { parts.push(format!("броня {}", a.armor)); }
    if a.max_hp != 0 { parts.push(format!("хп {:+}", a.max_hp)); }
    if a.strength != 0 { parts.push(format!("сил {:+}", a.strength)); }
    if a.dexterity != 0 { parts.push(format!("лов {:+}", a.dexterity)); }
    if a.constitution != 0 { parts.push(format!("тел {:+}", a.constitution)); }
    if a.intelligence != 0 { parts.push(format!("инт {:+}", a.intelligence)); }
    if a.wisdom != 0 { parts.push(format!("мдр {:+}", a.wisdom)); }
    if a.charisma != 0 { parts.push(format!("хар {:+}", a.charisma)); }
    parts.join(", ")
}

pub fn update_turn_order_hp(
    dirty: Res<UiDirty>,
    queue: Res<TurnQueue>,
    combatants: Query<(&Vital, Has<Dead>), With<Combatant>>,
    mut hp_q: Query<(&TurnOrderCardHp, &mut Text, &mut TextColor)>,
) {
    if !dirty.0.contains(UiDirtyFlags::TURN_ORDER) {
        return;
    }
    let len = queue.order.len();
    for (label, mut text, mut color) in &mut hp_q {
        let slot = label.0;
        if slot < len.min(MAX_TURN_CARDS) {
            let queue_idx = (queue.index + slot) % len;
            let entity = queue.order[queue_idx];
            if let Ok((vital, is_dead)) = combatants.get(entity) {
                text.0 = format!("HP {}/{}", vital.hp, vital.max_hp);
                *color = TextColor(if is_dead {
                    Color::srgb(0.28, 0.28, 0.30)
                } else {
                    let ratio = vital.hp as f32 / vital.max_hp.max(1) as f32;
                    if ratio > 0.5 {
                        Color::srgb(0.40, 0.80, 0.40)
                    } else if ratio > 0.25 {
                        Color::srgb(0.85, 0.70, 0.20)
                    } else {
                        Color::srgb(0.85, 0.25, 0.20)
                    }
                });
            }
        } else {
            text.0.clear();
        }
    }
}
