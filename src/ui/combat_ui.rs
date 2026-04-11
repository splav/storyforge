use super::log_ui::LogScrollState;
use super::{
    AbilitySlot, AbilitySlotLabel, HudPhase, HudTurnOrder, LogScrollClip, LogScrollThumb, LogText,
    UiFont,
};
use crate::app_state::CombatPhase;
use crate::content::abilities::{AbilityDef, EffectDef, StatusOn, TargetType};
use crate::content::weapons::WeaponDef;
use crate::core::{modifier, DiceExpr};
use crate::game::components::{
    Abilities, CombatStats, Combatant, Dead, EquippedWeapon, Faction, Initiative, Mana, Rage,
    Team, Vital,
};
use crate::game::resources::{CombatContext, GameDb, SelectionState, TurnQueue};
use bevy::prelude::*;

const MAX_SLOTS: usize = 5;

const CLR_SLOT_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_SLOT_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_SLOT_SEL_BG: Color = Color::srgb(0.18, 0.16, 0.06);
const CLR_SLOT_SEL_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
const CLR_SLOT_DIM_BG: Color = Color::srgb(0.08, 0.08, 0.09);
const CLR_SLOT_DIM_BORDER: Color = Color::srgb(0.18, 0.18, 0.20);
const CLR_HINT: Color = Color::srgb(0.55, 0.55, 0.30);

pub fn setup_hud(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font: Handle<Font> = asset_server.load("fonts/unicode.ttf");
    commands.insert_resource(UiFont(font.clone()));

    let txt = |size: f32| -> (TextFont, TextColor) {
        (
            TextFont {
                font: font.clone(),
                font_size: size,
                ..default()
            },
            TextColor(Color::WHITE),
        )
    };

    // ── Root: full screen, row layout ────────────────────────────────────────
    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            ..default()
        })
        .with_children(|root| {
            // ── Left panel: abilities ─────────────────────────────────────
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(10.0)),
                width: Val::Px(160.0),
                ..default()
            })
            .with_children(|panel| {
                for i in 0..MAX_SLOTS {
                    panel
                        .spawn((
                            AbilitySlot(i),
                            Button,
                            Node {
                                border: UiRect::all(Val::Px(1.5)),
                                padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                                width: Val::Percent(100.0),
                                height: Val::Px(70.0),
                                flex_direction: FlexDirection::Column,
                                justify_content: JustifyContent::SpaceBetween,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BorderColor::all(CLR_SLOT_BORDER),
                            BackgroundColor(CLR_SLOT_BG),
                            Visibility::Hidden,
                        ))
                        .with_children(|slot| {
                            let (tf, tc) = txt(12.0);
                            slot.spawn((AbilitySlotLabel(i), Text::new(""), tf, tc));
                        });
                }
            });

            // ── Center: spacer (hex grid shows through) ───────────────────
            root.spawn(Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            })
            .with_children(|center| {
                // Phase / hint
                let (tf, _) = txt(14.0);
                center.spawn((HudPhase, Text::new(""), tf, TextColor(CLR_HINT)));

                // Turn order strip
                let (tf, _) = txt(13.0);
                center.spawn((
                    HudTurnOrder,
                    Text::new(""),
                    tf,
                    TextColor(Color::srgb(0.75, 0.75, 0.75)),
                ));

                center.spawn(Node {
                    flex_grow: 1.0,
                    ..default()
                });

                // Combat log — row: clip area + scrollbar track
                {
                    let (tf, _) = txt(13.0);
                    center
                        .spawn((
                            Node {
                                height: Val::Px(110.0),
                                flex_direction: FlexDirection::Row,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.3)),
                        ))
                        .with_children(|container| {
                            // Scroll clip area
                            container
                                .spawn((
                                    LogScrollClip,
                                    LogScrollState::default(),
                                    Button,
                                    Node {
                                        flex_grow: 1.0,
                                        height: Val::Percent(100.0),
                                        overflow: Overflow::scroll_y(),
                                        flex_direction: FlexDirection::Column,
                                        ..default()
                                    },
                                ))
                                .with_children(|clip| {
                                    clip.spawn((
                                        LogText,
                                        Text::new(""),
                                        tf,
                                        TextColor(Color::srgb(0.6, 0.6, 0.6)),
                                    ));
                                });

                            // Scrollbar track
                            container
                                .spawn(Node {
                                    width: Val::Px(6.0),
                                    height: Val::Percent(100.0),
                                    ..default()
                                })
                                .with_children(|track| {
                                    track.spawn((
                                        LogScrollThumb,
                                        Node {
                                            position_type: PositionType::Absolute,
                                            width: Val::Percent(100.0),
                                            top: Val::Px(0.0),
                                            height: Val::Percent(100.0),
                                            border_radius: BorderRadius::all(Val::Px(2.0)),
                                            ..default()
                                        },
                                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.35)),
                                    ));
                                });
                        });
                }
            });
        });
}

// ── Update: phase hint ────────────────────────────────────────────────────────

pub fn update_phase_hint(
    phase: Res<State<CombatPhase>>,
    ctx: Res<CombatContext>,
    sel: Res<SelectionState>,
    combatants: Query<(&Name, &Faction), With<Combatant>>,
    mut phase_q: Query<&mut Text, With<HudPhase>>,
) {
    let Ok(mut t) = phase_q.single_mut() else {
        return;
    };
    t.0 = match phase.get() {
        CombatPhase::AwaitCommand => {
            let actor_name = ctx
                .active
                .and_then(|e| combatants.get(e).ok())
                .filter(|(_, f)| f.0 == Team::Player)
                .map(|(n, _)| n.as_str())
                .unwrap_or("Враг");
            let confirm = if sel.selected_ability.is_some() && sel.selected_target.is_some() {
                "  Enter: подтвердить"
            } else {
                ""
            };
            format!("Ход: {actor_name}  |  [1-5]: способность   Tab: выбор цели{confirm}")
        }
        CombatPhase::Victory => "★  ПОБЕДА".into(),
        CombatPhase::Defeat => "✗  ПОРАЖЕНИЕ".into(),
        p => format!("{p:?}"),
    };
}

// ── Update: combatants list ───────────────────────────────────────────────────

// ── Update: ability panel ─────────────────────────────────────────────────────

pub fn update_ability_panel(
    ctx: Res<CombatContext>,
    sel: Res<SelectionState>,
    db: Res<GameDb>,
    combatants: Query<
        (
            &Faction,
            &Abilities,
            &CombatStats,
            Option<&EquippedWeapon>,
            Option<&Mana>,
            Option<&Rage>,
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
    let actor_data = ctx
        .active
        .and_then(|e| combatants.get(e).ok())
        .filter(|(f, _, _, _, _, _)| f.0 == Team::Player)
        .map(|(_, abilities, stats, weapon, mana, rage)| {
            let weapon_def = weapon.and_then(|w| db.weapons.get(&w.0));
            (
                abilities.0.clone(),
                stats.clone(),
                weapon_def,
                mana.map(|m| m.current),
                rage.map(|r| r.current),
            )
        });

    let (abilities, stats, weapon_def, mana_cur, rage_cur) = match actor_data {
        Some(d) => d,
        None => return,
    };

    // Helper: can the actor afford this ability?
    let can_use = |def: &AbilityDef| -> bool {
        if def.mana_cost > 0 && mana_cur.unwrap_or(0) < def.mana_cost {
            return false;
        }
        if def.rage_cost > 0 && rage_cur.unwrap_or(0) < def.rage_cost {
            return false;
        }
        true
    };

    for (slot, mut node, mut border, mut bg, mut vis) in &mut slots {
        let idx = slot.0;
        let ability_id = abilities.get(idx).cloned();
        let selected = ability_id.is_some() && sel.selected_ability == ability_id;
        let affordable = ability_id
            .as_ref()
            .and_then(|id| db.abilities.get(id))
            .map(|def| can_use(def))
            .unwrap_or(false);

        *vis = if ability_id.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        node.border = if selected {
            UiRect::all(Val::Px(2.5))
        } else {
            UiRect::all(Val::Px(1.5))
        };
        *border = BorderColor::all(if selected {
            CLR_SLOT_SEL_BORDER
        } else if affordable {
            CLR_SLOT_BORDER
        } else {
            CLR_SLOT_DIM_BORDER
        });
        *bg = BackgroundColor(if selected {
            CLR_SLOT_SEL_BG
        } else if affordable {
            CLR_SLOT_BG
        } else {
            CLR_SLOT_DIM_BG
        });
    }

    for (label, mut text, mut color) in &mut labels {
        let idx = label.0;
        if let Some(id) = abilities.get(idx).cloned() {
            if let Some(def) = db.abilities.get(&id) {
                let effect_str = ability_effect_str(def, &stats, weapon_def, &db);
                let mut costs = String::new();
                if def.rage_cost > 0 {
                    costs += &format!(" R:{}", def.rage_cost);
                }
                if def.mana_cost > 0 {
                    costs += &format!(" M:{}", def.mana_cost);
                }
                text.0 = format!("[{}] {}{}\n{}", idx + 1, def.name, costs, effect_str);
                let affordable = can_use(def);
                let selected = sel.selected_ability == Some(id);
                *color = TextColor(if selected {
                    Color::srgb(1.0, 0.95, 0.5)
                } else if affordable {
                    Color::WHITE
                } else {
                    Color::srgb(0.35, 0.35, 0.38)
                });
            }
        }
    }
}

fn ability_effect_str(
    def: &AbilityDef,
    stats: &CombatStats,
    weapon_def: Option<&WeaponDef>,
    db: &GameDb,
) -> String {
    let str_mod = modifier(stats.strength);
    let int_mod = modifier(stats.intelligence);

    let mut lines: Vec<String> = Vec::new();

    match &def.effect {
        EffectDef::None => {}
        EffectDef::WeaponAttack => {
            let s = if let Some(wd) = weapon_def {
                format!("{} урон", dice_bonus_str(&wd.dice, str_mod))
            } else {
                format!("{str_mod} урон")
            };
            lines.push(s);
        }
        EffectDef::Damage { dice } => {
            lines.push(format!("{} урон", dice_bonus_str(dice, str_mod)));
        }
        EffectDef::SpellDamage { dice } => {
            let sp = weapon_def.map_or(0, |wd| wd.spell_power);
            lines.push(format!(
                "{} урон (закл.)",
                dice_bonus_str(dice, sp + int_mod)
            ));
        }
        EffectDef::Heal { dice } => {
            let sp = weapon_def.map_or(0, |wd| wd.spell_power);
            lines.push(format!("{} лечение", dice_bonus_str(dice, sp + int_mod)));
        }
    }

    for sa in &def.statuses {
        let status_name = db
            .statuses
            .get(&sa.status)
            .map(|s| s.name.as_str())
            .unwrap_or("?");
        let on_str = match sa.on {
            StatusOn::MySelf => "себя",
            StatusOn::Target => "цель",
        };
        lines.push(format!(
            "→ {} на {} ({} ход.)",
            status_name, on_str, sa.duration_rounds
        ));
    }

    if lines.is_empty() {
        "—".into()
    } else {
        lines.join("\n    ")
    }
}

fn dice_bonus_str(dice: &DiceExpr, bonus: i32) -> String {
    match bonus.cmp(&0) {
        std::cmp::Ordering::Greater => format!("{}d{}+{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Less => format!("{}d{}{}", dice.count, dice.sides, bonus),
        std::cmp::Ordering::Equal => format!("{}d{}", dice.count, dice.sides),
    }
}

// ── Update: turn order strip ──────────────────────────────────────────────────

pub fn update_turn_order(
    queue: Res<TurnQueue>,
    combatants: Query<(&Name, &Vital, &Initiative, &Faction, Has<Dead>), With<Combatant>>,
    mut text_q: Query<&mut Text, With<HudTurnOrder>>,
) {
    let Ok(mut t) = text_q.single_mut() else {
        return;
    };
    if queue.order.is_empty() {
        t.0.clear();
        return;
    }

    let len = queue.order.len();
    let mut s = String::from("Ход: ");

    for i in 0..len {
        let idx = (queue.index + i) % len;
        let entity = queue.order[idx];

        let Ok((name, vital, init, faction, is_dead)) = combatants.get(entity) else {
            continue;
        };

        let marker = if i == 0 { "▶ " } else { "" };
        let dead_mark = if is_dead { " ✗" } else { "" };
        let team_icon = match faction.0 {
            Team::Player => "🗡",
            Team::Enemy => "👹",
        };
        let hp_str = format!("HP:{}/{}", vital.hp, vital.max_hp);

        s.push_str(&format!(
            "{marker}{team_icon}{name} {hp_str} (ini:{init_val}){dead_mark}",
            init_val = init.0
        ));

        if i < len - 1 {
            s.push_str(" → ");
        }
    }

    t.0 = s;
}

// ── Ability slot click ────────────────────────────────────────────────────────

pub fn ability_slot_click_system(
    ctx: Res<CombatContext>,
    mut sel: ResMut<SelectionState>,
    slots: Query<(&AbilitySlot, &Interaction), Changed<Interaction>>,
    combatants: Query<(&Faction, &Abilities), (With<Combatant>, Without<Dead>)>,
) {
    for (slot, interaction) in &slots {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(active) = ctx.active else { continue };
        let Ok((faction, abilities)) = combatants.get(active) else {
            continue;
        };
        if faction.0 != Team::Player {
            continue;
        }
        if let Some(id) = abilities.0.get(slot.0).cloned() {
            sel.selected_ability = Some(id);
        }
    }
}
