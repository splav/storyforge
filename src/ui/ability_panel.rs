#![allow(clippy::type_complexity, clippy::too_many_arguments)]
//! Left-side ability panel: compact slot list plus a description panel that
//! shows the full details of the currently selected ability.

use super::{AbilityDescPanel, AbilityDescText, AbilitySlot, AbilitySlotLabel, EndTurnButton};
use crate::content::abilities::{
    AbilityDef, AoEShape, CasterContext, EffectCalcExt, EffectDef, StatusOn, TargetType,
};
use crate::content::content_view::{ActiveContent, ActiveContentData};
use crate::content::statuses::StatusDef;
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, CombatStats, Combatant, Dead, Energy, Equipment,
    Faction, Mana, Rage, Team, Vital,
};
use crate::game::messages::ActionInput;
use crate::game::resources::{HexPositions, SelectionState, UiDirty, UiDirtyFlags};
use bevy::prelude::*;
use combat_engine::{AbilityId, DiceExpr, ResourceKind};

/// Max keyed (universal) + class ability slots.
pub const MAX_SLOTS: usize = 7;

const CLR_SLOT_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_SLOT_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_SLOT_SEL_BG: Color = Color::srgb(0.18, 0.16, 0.06);
const CLR_SLOT_SEL_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
const CLR_SLOT_DIM_BG: Color = Color::srgb(0.08, 0.08, 0.09);
const CLR_SLOT_DIM_BORDER: Color = Color::srgb(0.18, 0.18, 0.20);
const CLR_DESC_BG: Color = Color::srgba(0.07, 0.07, 0.09, 0.92);
const CLR_DESC_BORDER: Color = Color::srgb(0.22, 0.22, 0.26);

pub const PANEL_WIDTH: f32 = 150.0;
const SLOT_HEIGHT: f32 = 38.0;

/// Window (seconds) for detecting a double-click on a Myself keyed ability.
const DOUBLE_CLICK_WINDOW: f32 = 0.5;

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the ability panel (slot list + description area) as a child of `root`.
pub fn spawn_ability_panel(root: &mut ChildSpawnerCommands, font: &Handle<Font>) {
    let text_font = |size: f32| TextFont {
        font: font.clone(),
        font_size: size,
        ..default()
    };

    root.spawn(Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(Val::Px(8.0)),
        row_gap: Val::Px(8.0),
        width: Val::Px(PANEL_WIDTH),
        ..default()
    })
    .with_children(|panel| {
        // Slot list
        panel
            .spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                ..default()
            })
            .with_children(|slots| {
                for i in 0..MAX_SLOTS {
                    slots
                        .spawn((
                            AbilitySlot(i),
                            Button,
                            Node {
                                border: UiRect::all(Val::Px(1.0)),
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                                width: Val::Percent(100.0),
                                height: Val::Px(SLOT_HEIGHT),
                                align_items: AlignItems::Center,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BorderColor::all(CLR_SLOT_BORDER),
                            BackgroundColor(CLR_SLOT_BG),
                            Visibility::Hidden,
                        ))
                        .with_children(|slot| {
                            slot.spawn((
                                AbilitySlotLabel(i),
                                Text::new(""),
                                text_font(12.0),
                                TextColor(Color::WHITE),
                            ));
                        });
                }
            });

        // End-turn button (between slots and description) — matches slot styling.
        panel
            .spawn((
                EndTurnButton,
                Button,
                Node {
                    border: UiRect::all(Val::Px(1.0)),
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    width: Val::Percent(100.0),
                    height: Val::Px(SLOT_HEIGHT),
                    align_items: AlignItems::Center,
                    overflow: Overflow::clip(),
                    ..default()
                },
                BorderColor::all(CLR_SLOT_BORDER),
                BackgroundColor(CLR_SLOT_BG),
            ))
            .with_children(|btn| {
                btn.spawn((
                    Text::new("[E] Конец хода"),
                    text_font(12.0),
                    TextColor(Color::WHITE),
                ));
            });

        // Description panel (below slots)
        panel
            .spawn((
                AbilityDescPanel,
                Node {
                    flex_grow: 1.0,
                    min_height: Val::Px(0.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BorderColor::all(CLR_DESC_BORDER),
                BackgroundColor(CLR_DESC_BG),
                Visibility::Hidden,
            ))
            .with_children(|desc| {
                desc.spawn((
                    AbilityDescText,
                    Text::new(""),
                    text_font(11.0),
                    TextColor(Color::srgb(0.82, 0.82, 0.88)),
                ));
            });
    });
}

// ── Displayed list helper ─────────────────────────────────────────────────────

fn displayed_abilities(
    content: &ActiveContentData,
    class_abilities: &[AbilityId],
) -> Vec<AbilityId> {
    let mut result: Vec<AbilityId> = content.keyed_abilities.clone();
    result.extend(class_abilities.iter().cloned());
    result
}

// ── Update: slot visuals & labels ─────────────────────────────────────────────

pub fn update_ability_panel(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    content: Res<ActiveContent>,
    combatants: Query<
        (
            &Faction,
            &Abilities,
            &ActionPoints,
            &Vital,
            Option<&Mana>,
            Option<&Rage>,
            Option<&Energy>,
        ),
        (With<Combatant>, Without<Dead>),
    >,
    mut slots: Query<(
        &AbilitySlot,
        &mut Node,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Visibility,
    )>,
    mut labels: Query<(&AbilitySlotLabel, &mut Text, &mut TextColor)>,
) {
    if !dirty
        .0
        .intersects(UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::MOVE_BTN)
    {
        return;
    }
    let actor_data = active_q
        .single()
        .ok()
        .and_then(|e| combatants.get(e).ok())
        .filter(|(f, _, _, _, _, _, _)| f.0 == Team::Player);

    let (_, abilities, ap, vital, mana, rage, energy) = match actor_data {
        Some(d) => d,
        None => return,
    };

    let displayed = displayed_abilities(&content, &abilities.0);
    let hp_cur = vital.hp;
    let mana_cur = mana.map(|m| m.current).unwrap_or(0);
    let rage_cur = rage.map(|r| r.current).unwrap_or(0);
    let energy_cur = energy.map(|e| e.current).unwrap_or(0);

    let can_afford = |def: &AbilityDef| -> bool {
        def.costs.iter().all(|cost| {
            let available = match cost.resource {
                ResourceKind::Hp => hp_cur,
                ResourceKind::Mana => mana_cur,
                ResourceKind::Rage => rage_cur,
                ResourceKind::Energy => energy_cur,
            };
            available >= cost.amount
        })
    };

    let keyed_count = content.keyed_abilities.len();

    for (slot, mut node, mut border, mut bg, mut vis) in &mut slots {
        let idx = slot.0;
        let ability_id = displayed.get(idx).cloned();
        let def = ability_id.as_ref().and_then(|id| content.abilities.get(id));
        let is_move = def.is_some_and(|d| d.is_move_toggle);

        let selected = if is_move {
            sel.move_mode
        } else {
            ability_id.is_some() && sel.selected_ability == ability_id
        };
        let available = if is_move {
            ap.can_move()
        } else {
            def.is_some_and(|d| ap.can_act_for(d.cost_ap) && can_afford(d))
        };

        *vis = if ability_id.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        node.border = if selected {
            UiRect::all(Val::Px(2.0))
        } else {
            UiRect::all(Val::Px(1.0))
        };
        *border = BorderColor::all(if selected {
            CLR_SLOT_SEL_BORDER
        } else if available {
            CLR_SLOT_BORDER
        } else {
            CLR_SLOT_DIM_BORDER
        });
        *bg = BackgroundColor(if selected {
            CLR_SLOT_SEL_BG
        } else if available {
            CLR_SLOT_BG
        } else {
            CLR_SLOT_DIM_BG
        });
    }

    for (label, mut text, mut color) in &mut labels {
        let idx = label.0;
        let Some(id) = displayed.get(idx).cloned() else {
            continue;
        };
        let Some(def) = content.abilities.get(&id) else {
            continue;
        };
        let is_move = def.is_move_toggle;

        let prefix = if let Some(ref key) = def.key {
            key.clone()
        } else {
            format!("{}", idx - keyed_count + 1)
        };

        let mut costs = String::new();
        if is_move {
            costs = format!("  {}", ap.movement_points);
        } else {
            for cost in &def.costs {
                let lbl = match cost.resource {
                    ResourceKind::Hp => "HP",
                    ResourceKind::Mana => "M",
                    ResourceKind::Rage => "R",
                    ResourceKind::Energy => "E",
                };
                costs += &format!("  {}:{}", lbl, cost.amount);
            }
        }
        text.0 = format!("[{prefix}] {}{}", def.name, costs);

        let selected = if is_move {
            sel.move_mode
        } else {
            sel.selected_ability == Some(id)
        };
        let available = if is_move {
            ap.can_move()
        } else {
            ap.can_act_for(def.cost_ap) && can_afford(def)
        };
        *color = TextColor(if selected {
            Color::srgb(1.0, 0.95, 0.5)
        } else if available {
            Color::WHITE
        } else {
            Color::srgb(0.35, 0.35, 0.38)
        });
    }
}

// ── Update: description panel ─────────────────────────────────────────────────

pub fn update_ability_description(
    dirty: Res<UiDirty>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    sel: Res<SelectionState>,
    content: Res<ActiveContent>,
    combatants: Query<
        (&Faction, &CombatStats, Option<&Equipment>),
        (With<Combatant>, Without<Dead>),
    >,
    mut panels: Query<&mut Visibility, With<AbilityDescPanel>>,
    mut texts: Query<&mut Text, With<AbilityDescText>>,
) {
    if !dirty
        .0
        .intersects(UiDirtyFlags::ABILITY_PANEL | UiDirtyFlags::MOVE_BTN)
    {
        return;
    }

    let Ok(mut vis) = panels.single_mut() else {
        return;
    };
    let Ok(mut text) = texts.single_mut() else {
        return;
    };

    // In move mode, show the move ability's description.
    let shown_id: Option<AbilityId> = if sel.move_mode {
        content
            .keyed_abilities
            .iter()
            .find(|id| content.abilities.get(*id).is_some_and(|d| d.is_move_toggle))
            .cloned()
    } else {
        sel.selected_ability.clone()
    };

    let Some(id) = shown_id else {
        *vis = Visibility::Hidden;
        text.0.clear();
        return;
    };
    let Some(def) = content.abilities.get(&id) else {
        *vis = Visibility::Hidden;
        text.0.clear();
        return;
    };

    let ctx = active_q
        .single()
        .ok()
        .and_then(|e| combatants.get(e).ok())
        .filter(|(f, _, _)| f.0 == Team::Player)
        .map(|(_, stats, equip)| CasterContext::new(stats, equip, &content.weapons));

    *vis = Visibility::Visible;
    text.0 = build_description(def, ctx.as_ref(), &content);
}

// ── Click: double-click fires keyed self-target abilities (e.g. Rest) ─────────

#[derive(Default)]
pub struct LastSlotClick {
    slot: Option<usize>,
    at: f32,
}

pub fn ability_slot_click_system(
    time: Res<Time>,
    active_q: Query<Entity, With<ActiveCombatant>>,
    content: Res<ActiveContent>,
    positions: Res<HexPositions>,
    mut sel: ResMut<SelectionState>,
    mut last_click: Local<LastSlotClick>,
    slots: Query<(&AbilitySlot, &Interaction), Changed<Interaction>>,
    combatants: Query<(&Faction, &Abilities, &ActionPoints), (With<Combatant>, Without<Dead>)>,
    mut action_input: MessageWriter<ActionInput>,
) {
    let now = time.elapsed_secs();

    for (slot, interaction) in &slots {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Ok(active) = active_q.single() else {
            continue;
        };
        let Ok((faction, abilities, ap)) = combatants.get(active) else {
            continue;
        };
        if faction.0 != Team::Player {
            continue;
        }
        let displayed = displayed_abilities(&content, &abilities.0);
        let Some(id) = displayed.get(slot.0).cloned() else {
            continue;
        };
        let Some(def) = content.abilities.get(&id) else {
            continue;
        };

        if def.is_move_toggle {
            if ap.can_move() {
                sel.move_mode = !sel.move_mode;

                if sel.move_mode {
                    sel.selected_ability = None;
                    sel.selected_target = None;
                }
            }
            last_click.slot = None;
        } else if def.target_type == TargetType::Myself && def.key.is_some() {
            // Keyed self-target abilities (e.g. Rest): first click selects,
            // a second click within the window confirms and fires.
            let is_double =
                last_click.slot == Some(slot.0) && (now - last_click.at) <= DOUBLE_CLICK_WINDOW;
            if is_double && ap.can_act_for(def.cost_ap) {
                let target_pos = positions.get(&active).unwrap_or(hexx::Hex::ZERO);
                action_input.write(ActionInput::Cast {
                    actor: active,
                    ability: id,
                    target: active,
                    target_pos,
                });
                sel.clear();
                last_click.slot = None;
            } else {
                sel.selected_ability = Some(id);
                sel.selected_target = Some(active);
                sel.move_mode = false;
                last_click.slot = Some(slot.0);
                last_click.at = now;
            }
        } else {
            sel.selected_ability = Some(id);
            sel.move_mode = false;
            if def.target_type == TargetType::Myself {
                sel.selected_target = Some(active);
            }
            last_click.slot = None;
        }
    }
}

// ── End-turn button click ────────────────────────────────────────────────────

pub fn end_turn_button_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    combatants: Query<&Faction, (With<Combatant>, Without<Dead>)>,
    buttons: Query<&Interaction, (Changed<Interaction>, With<EndTurnButton>)>,
    mut sel: ResMut<SelectionState>,
    mut action_input: MessageWriter<ActionInput>,
) {
    if !buttons.iter().any(|i| *i == Interaction::Pressed) {
        return;
    }
    let Ok(actor) = active_q.single() else { return };
    let Ok(faction) = combatants.get(actor) else {
        return;
    };
    if faction.0 != Team::Player {
        return;
    }
    action_input.write(ActionInput::EndTurn { actor });
    sel.clear();
}

// ── Description formatting ───────────────────────────────────────────────────

fn build_description(
    def: &AbilityDef,
    ctx: Option<&CasterContext>,
    content: &ActiveContentData,
) -> String {
    let mut out = String::new();

    // Header: name + hotkey / target / range / AoE summary
    out.push_str(&def.name);
    if let Some(ref k) = def.key {
        out.push_str(&format!("  [{k}]"));
    }
    out.push('\n');

    let target = match def.target_type {
        TargetType::SingleEnemy => "цель: враг",
        TargetType::SingleAlly => "цель: союзник",
        TargetType::Myself => "цель: себя",
        TargetType::Ground => "цель: клетка",
        TargetType::Environment => "цель: окружение",
    };
    out.push_str(target);

    if def.range.max > 0 {
        if def.range.min > 0 {
            out.push_str(&format!(", дальн. {}–{}", def.range.min, def.range.max));
        } else {
            out.push_str(&format!(", дальн. {}", def.range.max));
        }
    }
    match def.aoe {
        AoEShape::Circle { radius } => out.push_str(&format!(", обл. r{radius}")),
        AoEShape::Line { length } => out.push_str(&format!(", линия {length}")),
        AoEShape::None => {}
    }
    if def.friendly_fire {
        out.push_str(", задевает союзников");
    }
    out.push('\n');

    // Costs
    if !def.costs.is_empty() {
        let parts: Vec<String> = def
            .costs
            .iter()
            .map(|c| {
                let lbl = match c.resource {
                    ResourceKind::Hp => "HP",
                    ResourceKind::Mana => "мана",
                    ResourceKind::Rage => "ярость",
                    ResourceKind::Energy => "энергия",
                };
                format!("{} {}", lbl, c.amount)
            })
            .collect();
        out.push_str(&format!("Цена: {}\n", parts.join(", ")));
    }

    // Effect
    let effect_line = effect_line_ru(def, ctx);
    if !effect_line.is_empty() {
        out.push_str(&effect_line);
        out.push('\n');
    }

    // Statuses
    for sa in &def.statuses {
        let status = content.statuses.get(&sa.status);
        let name = status.map(|s| s.name.as_str()).unwrap_or("?");
        let on = match sa.on {
            StatusOn::MySelf => "себе",
            StatusOn::Target => "цели",
        };
        out.push_str(&format!("→ {} ({} ход.) {}", name, sa.duration_rounds, on));
        if let Some(sd) = status {
            let d = status_desc_ru(sd);
            if !d.is_empty() {
                out.push_str(&format!(": {d}"));
            }
        }
        out.push('\n');
    }

    // Trim trailing newline
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn effect_line_ru(def: &AbilityDef, ctx: Option<&CasterContext>) -> String {
    if let Some(ctx) = ctx {
        if let Some(calc) = def.effect.calc(ctx) {
            let label = if calc.is_heal {
                "лечение"
            } else if calc.pierces_armor {
                "магический урон (игнорирует броню)"
            } else {
                "урон"
            };
            return if let Some(ref dice) = calc.dice {
                format!("{}: {}", label, dice_bonus_str(dice, calc.bonus))
            } else {
                format!("{}: {}", label, calc.bonus)
            };
        }
    }
    match &def.effect {
        EffectDef::GrantMovement { distance } => format!("движение +{distance}"),
        EffectDef::RestoreResources => "восстанавливает HP/ману/ярость/энергию +1".into(),
        EffectDef::None => {
            // is_move_toggle abilities show a fixed label instead of an effect line.
            if def.is_move_toggle {
                "режим перемещения".into()
            } else {
                String::new()
            }
        }
        // Fallbacks when ctx is None — show raw dice if available.
        EffectDef::Damage { dice } | EffectDef::SpellDamage { dice } | EffectDef::Heal { dice } => {
            format!("{}d{}", dice.count, dice.sides)
        }
        EffectDef::WeaponAttack { ranged, .. } => {
            if *ranged {
                "дальняя атака оружием".into()
            } else {
                "атака оружием".into()
            }
        }
        EffectDef::Summon {
            template_id,
            max_active,
        } => match max_active {
            Some(cap) => format!("призыв {template_id} (не более {cap})"),
            None => format!("призыв {template_id}"),
        },
        EffectDef::RevealEnvInRange { range } => format!("обнаружить ловушки (радиус {range})"),
    }
}

fn status_desc_ru(def: &StatusDef) -> String {
    let mut parts: Vec<String> = Vec::new();
    if def.bonuses.runtime.0.armor != 0 {
        parts.push(format!("броня {:+}", def.bonuses.runtime.0.armor));
    }
    if def.bonuses.runtime.0.magic_resist != 0 {
        parts.push(format!(
            "магзащита {:+}",
            def.bonuses.runtime.0.magic_resist
        ));
    }
    if def.bonuses.damage_taken_bonus != 0 {
        parts.push(format!(
            "получаемый урон {:+}",
            def.bonuses.damage_taken_bonus
        ));
    }
    if def.skips_turn {
        parts.push("пропускает ход".into());
    }
    if def.forces_targeting {
        parts.push("враги обязаны атаковать цель".into());
    }
    if let Some(ref d) = def.dot_dice {
        parts.push(format!("урон {}d{}/ход", d.count, d.sides));
    }
    if def.blocks_mana_abilities {
        parts.push("нельзя тратить ману".into());
    }
    if def.bonuses.runtime.0.base_speed != 0 {
        parts.push(format!("скорость {:+}", def.bonuses.runtime.0.base_speed));
    }
    if def.hp_percent_dot > 0 {
        parts.push(format!("−{}% макс. HP/ход", def.hp_percent_dot));
    }
    if def.ai_controlled {
        parts.push("под контролем ИИ".into());
    }
    parts.join(", ")
}

fn dice_bonus_str(dice: &DiceExpr, bonus: i32) -> String {
    match bonus.cmp(&0) {
        std::cmp::Ordering::Greater => format!("{}d{}+{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Less => format!("{}d{}{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Equal => format!("{}d{}", dice.count, dice.sides),
    }
}
