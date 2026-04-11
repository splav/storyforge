use super::{AbilitySlot, AbilitySlotLabel, HudCombatants, HudLog, HudPhase, HudTurnOrder, UiFont};
use crate::app_state::CombatPhase;
use crate::content::abilities::{EffectDef, TargetType};
use crate::content::weapons::WeaponDef;
use crate::core::{modifier, DiceExpr};
use crate::game::components::{
    Abilities, ActionPoints, CombatStats, Combatant, Dead, EquippedWeapon, Faction, Initiative,
    Mana, Rage, StatusEffects, Team, Vital,
};
use crate::game::resources::{CombatContext, GameDb, SelectionState, TurnQueue};
use bevy::prelude::*;

const MAX_SLOTS: usize = 5;

const CLR_SLOT_BG: Color = Color::srgb(0.10, 0.10, 0.12);
const CLR_SLOT_BORDER: Color = Color::srgb(0.30, 0.30, 0.35);
const CLR_SLOT_SEL_BG: Color = Color::srgb(0.18, 0.16, 0.06);
const CLR_SLOT_SEL_BORDER: Color = Color::srgb(0.90, 0.80, 0.20);
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

    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            padding: UiRect::all(Val::Px(14.0)),
            row_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|root| {
            // ── Phase / hint ─────────────────────────────────────────────
            let (tf, _) = txt(14.0);
            root.spawn((HudPhase, Text::new(""), tf, TextColor(CLR_HINT)));

            // ── Turn order strip ─────────────────────────────────────────
            let (tf, _) = txt(13.0);
            root.spawn((
                HudTurnOrder,
                Text::new(""),
                tf,
                TextColor(Color::srgb(0.75, 0.75, 0.75)),
            ));

            // ── Combatants list ──────────────────────────────────────────
            let (tf, tc) = txt(15.0);
            root.spawn((
                HudCombatants,
                Text::new(""),
                tf,
                tc,
                Node {
                    flex_grow: 1.0,
                    align_self: AlignSelf::FlexStart,
                    ..default()
                },
            ));

            // ── Ability panel ────────────────────────────────────────────
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|panel| {
                for i in 0..MAX_SLOTS {
                    panel
                        .spawn((
                            AbilitySlot(i),
                            Node {
                                border: UiRect::all(Val::Px(1.5)),
                                padding: UiRect::all(Val::Px(8.0)),
                                min_width: Val::Px(140.0),
                                flex_direction: FlexDirection::Column,
                                ..default()
                            },
                            BorderColor::all(CLR_SLOT_BORDER),
                            BackgroundColor(CLR_SLOT_BG),
                            Visibility::Hidden,
                        ))
                        .with_children(|slot| {
                            let (tf, tc) = txt(13.0);
                            slot.spawn((AbilitySlotLabel(i), Text::new(""), tf, tc));
                        });
                }
            });

            // ── Combat log ───────────────────────────────────────────────
            let (tf, _) = txt(13.0);
            root.spawn((
                HudLog,
                Text::new(""),
                tf,
                TextColor(Color::srgb(0.6, 0.6, 0.6)),
            ));
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

pub fn update_combatants(
    ctx: Res<CombatContext>,
    sel: Res<SelectionState>,
    db: Res<GameDb>,
    combatants: Query<
        (
            Entity,
            &Name,
            &Vital,
            &CombatStats,
            &Faction,
            &ActionPoints,
            &StatusEffects,
            Option<&EquippedWeapon>,
            Option<&Rage>,
            Option<&Mana>,
            Has<Dead>,
        ),
        With<Combatant>,
    >,
    mut text_q: Query<&mut Text, With<HudCombatants>>,
) {
    let Ok(mut t) = text_q.single_mut() else {
        return;
    };

    let (mut players, mut enemies): (Vec<_>, Vec<_>) = combatants
        .iter()
        .partition(|(_, _, _, _, f, _, _, _, _, _, _)| f.0 == Team::Player);
    players.sort_by_key(|(e, ..)| *e);
    enemies.sort_by_key(|(e, ..)| *e);

    let mut s = String::new();
    s.push_str("── ОТРЯД ────────────────────────────────\n");
    for row in &players {
        s.push_str(&fmt_row(row, &ctx, &sel, &db));
    }
    s.push_str("\n── ВРАГИ ────────────────────────────────\n");
    for row in &enemies {
        s.push_str(&fmt_row(row, &ctx, &sel, &db));
    }
    t.0 = s;
}

type Row<'a> = (
    Entity,
    &'a Name,
    &'a Vital,
    &'a CombatStats,
    &'a Faction,
    &'a ActionPoints,
    &'a StatusEffects,
    Option<&'a EquippedWeapon>,
    Option<&'a Rage>,
    Option<&'a Mana>,
    bool,
);

fn fmt_row(row: &Row, ctx: &CombatContext, sel: &SelectionState, db: &GameDb) -> String {
    let (entity, name, vital, stats, _, ap, statuses, weapon, rage, mana, is_dead) = row;
    let active = if ctx.active == Some(*entity) {
        "▶"
    } else {
        " "
    };
    let target = if sel.selected_target == Some(*entity) {
        "→"
    } else {
        " "
    };
    let action = if ap.action { "●" } else { "○" };
    let dead = if *is_dead { "  [мертв]" } else { "" };
    let rage_str = rage
        .map(|r| {
            let filled = "★".repeat(r.current as usize);
            let empty = "☆".repeat((r.max - r.current) as usize);
            format!("  ярость:{filled}{empty}")
        })
        .unwrap_or_default();
    let mana_str = mana
        .map(|m| format!("  мана:{}/{}", m.current, m.max))
        .unwrap_or_default();
    let status_tags: String = statuses
        .0
        .iter()
        .filter_map(|s| db.statuses.get(&s.id))
        .map(|def| format!(" [{}]", def.name))
        .collect();

    let weapon_str = weapon
        .and_then(|w| db.weapons.get(&w.0))
        .map(|wd| {
            let d = &wd.dice;
            format!(" [{}  {}d{}+{}]", wd.name, d.count, d.sides, stats.strength)
        })
        .unwrap_or_default();

    format!(
        " {active} {target} {name:<15} HP:{:>2}/{:<2}  ARM:{:<2}{weapon_str}  {action}{dead}{rage_str}{mana_str}{status_tags}\n",
        vital.hp, vital.max_hp, stats.armor,
    )
}

// ── Update: ability panel ─────────────────────────────────────────────────────

pub fn update_ability_panel(
    ctx: Res<CombatContext>,
    sel: Res<SelectionState>,
    db: Res<GameDb>,
    combatants: Query<
        (&Faction, &Abilities, &CombatStats, Option<&EquippedWeapon>),
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
        .filter(|(f, _, _, _)| f.0 == Team::Player)
        .map(|(_, abilities, stats, weapon)| {
            let weapon_def = weapon.and_then(|w| db.weapons.get(&w.0));
            (abilities.0.clone(), stats.clone(), weapon_def)
        });

    let (abilities, stats, weapon_def) = match actor_data {
        Some(d) => d,
        None => return,
    };

    for (slot, mut node, mut border, mut bg, mut vis) in &mut slots {
        let idx = slot.0;
        let ability_id = abilities.get(idx).cloned();
        let selected = ability_id.is_some() && sel.selected_ability == ability_id;

        *vis = if ability_id.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        node.border = if selected {
            UiRect::all(Val::Px(3.0))
        } else {
            UiRect::all(Val::Px(1.5))
        };
        *border = BorderColor::all(if selected {
            CLR_SLOT_SEL_BORDER
        } else {
            CLR_SLOT_BORDER
        });
        *bg = BackgroundColor(if selected {
            CLR_SLOT_SEL_BG
        } else {
            CLR_SLOT_BG
        });
    }

    for (label, mut text, mut color) in &mut labels {
        let idx = label.0;
        if let Some(id) = abilities.get(idx).cloned() {
            if let Some(def) = db.abilities.get(&id) {
                let target_hint = match def.target_type {
                    TargetType::SingleEnemy => "Tab → враг",
                    TargetType::SingleAlly => "Tab → союзник",
                    TargetType::Myself => "на себя",
                };
                let effect_str = ability_effect_str(&def.effect, &stats, weapon_def, &db);
                let mut costs = String::new();
                if def.rage_cost > 0 {
                    costs += &format!("  ярость:{}", def.rage_cost);
                }
                if def.mana_cost > 0 {
                    costs += &format!("  мана:{}", def.mana_cost);
                }
                text.0 = format!(
                    "[{}] {}{}\n    {}\n    {}",
                    idx + 1,
                    def.name,
                    costs,
                    effect_str,
                    target_hint
                );
                let selected = sel.selected_ability == Some(id);
                *color = TextColor(if selected {
                    Color::srgb(1.0, 0.95, 0.5)
                } else {
                    Color::WHITE
                });
            }
        }
    }
}

fn ability_effect_str(
    effect: &EffectDef,
    stats: &CombatStats,
    weapon_def: Option<&WeaponDef>,
    db: &GameDb,
) -> String {
    let str_mod = modifier(stats.strength);
    let int_mod = modifier(stats.intelligence);
    match effect {
        EffectDef::WeaponAttack => {
            if let Some(wd) = weapon_def {
                format!("{} урон", dice_bonus_str(&wd.dice, str_mod))
            } else {
                format!("{str_mod} урон")
            }
        }
        EffectDef::Damage { dice } => format!("{} урон", dice_bonus_str(dice, str_mod)),
        EffectDef::SpellDamage { dice } => {
            let sp = weapon_def.map_or(0, |wd| wd.spell_power);
            format!("{} урон (заклинание)", dice_bonus_str(dice, sp + int_mod))
        }
        EffectDef::Heal { dice } => {
            let sp = weapon_def.map_or(0, |wd| wd.spell_power);
            format!("{} лечение", dice_bonus_str(dice, sp + int_mod))
        }
        EffectDef::ApplyStatus {
            status,
            duration_rounds,
        } => {
            let status_name = db
                .statuses
                .get(status)
                .map(|s| s.name.as_str())
                .unwrap_or("?");
            format!("→ {status_name} ({duration_rounds} ход.)")
        }
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
