use crate::content::abilities::{EffectDef, StatusOn, TargetType};
use crate::core::{modifier, DiceRng};
use crate::game::components::{ActionPoints, CombatStats, EquippedWeapon, Mana, Rage};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, ValidatedAction};
use crate::game::resources::{CombatEvent, CombatLog, GameDb};
use bevy::prelude::*;

pub fn resolve_action_system(
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
    mut log: ResMut<CombatLog>,
    mut events: MessageReader<ValidatedAction>,
    mut actors: Query<(
        &CombatStats,
        &mut ActionPoints,
        Option<&EquippedWeapon>,
        Option<&mut Rage>,
        Option<&mut Mana>,
    )>,
    mut dmg_writer: MessageWriter<ApplyDamage>,
    mut heal_writer: MessageWriter<ApplyHeal>,
    mut status_writer: MessageWriter<ApplyStatus>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    for ev in events.read() {
        let Some(def) = db.abilities.get(&ev.ability) else {
            continue;
        };
        let Ok((stats, mut ap, weapon, mut rage, mut mana)) = actors.get_mut(ev.actor) else {
            continue;
        };

        ap.action = false;

        let target = match def.target_type {
            TargetType::Myself => ev.actor,
            TargetType::SingleEnemy => ev.target,
            TargetType::SingleAlly => ev.target,
        };

        let cost_str = {
            let mut parts = Vec::new();
            if def.mana_cost > 0 {
                if let Some(ref m) = mana {
                    parts.push(format!(
                        "мана: {} - {} = {}",
                        m.current,
                        def.mana_cost,
                        m.current - def.mana_cost
                    ));
                }
            }
            if def.rage_cost > 0 {
                if let Some(ref r) = rage {
                    parts.push(format!(
                        "ярость: {} - {} = {}",
                        r.current,
                        def.rage_cost,
                        r.current - def.rage_cost
                    ));
                }
            }
            parts.join(", ")
        };
        log.push(CombatEvent::AbilityUsed {
            actor: ev.actor,
            ability_name: def.name.clone(),
            target,
            cost_str,
        });

        match &def.effect {
            EffectDef::WeaponAttack => {
                let weapon_def = weapon.and_then(|w| db.weapons.get(&w.0));
                let (raw, breakdown) = if let Some(wd) = weapon_def {
                    let (dice_total, dice_str) = rng.roll_dice(&wd.dice);
                    let str_mod = modifier(stats.strength);
                    let raw = dice_total + str_mod;
                    let s = if str_mod != 0 {
                        format!("{} + {}(сил) = {}", dice_str, str_mod, raw)
                    } else {
                        format!("{} = {}", dice_str, raw)
                    };
                    (raw, s)
                } else {
                    let str_mod = modifier(stats.strength);
                    (str_mod, format!("{}(сил)", str_mod))
                };
                dmg_writer.write(ApplyDamage {
                    source: ev.actor,
                    target,
                    amount: raw,
                    breakdown,
                    pierces_armor: false,
                });
            }
            EffectDef::Damage { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let str_mod = modifier(stats.strength);
                let raw = dice_total + str_mod;
                let breakdown = if str_mod != 0 {
                    format!("{} + {}(сил) = {}", dice_str, str_mod, raw)
                } else {
                    format!("{} = {}", dice_str, raw)
                };
                dmg_writer.write(ApplyDamage {
                    source: ev.actor,
                    target,
                    amount: raw,
                    breakdown,
                    pierces_armor: false,
                });
            }
            EffectDef::SpellDamage { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let sp = weapon
                    .and_then(|w| db.weapons.get(&w.0))
                    .map_or(0, |wd| wd.spell_power);
                let intel = modifier(stats.intelligence);
                let raw = dice_total + sp + intel;
                let breakdown = spell_breakdown(&dice_str, sp, intel, raw);
                dmg_writer.write(ApplyDamage {
                    source: ev.actor,
                    target,
                    amount: raw,
                    breakdown,
                    pierces_armor: true,
                });
            }
            EffectDef::Heal { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let sp = weapon
                    .and_then(|w| db.weapons.get(&w.0))
                    .map_or(0, |wd| wd.spell_power);
                let intel = modifier(stats.intelligence);
                let amount = dice_total + sp + intel;
                let breakdown = spell_breakdown(&dice_str, sp, intel, amount);
                heal_writer.write(ApplyHeal {
                    source: ev.actor,
                    target,
                    amount,
                    breakdown,
                });
            }
            EffectDef::None => {}
        }

        for sa in &def.statuses {
            let status_target = match sa.on {
                StatusOn::Target => target,
                StatusOn::MySelf => ev.actor,
            };
            status_writer.write(ApplyStatus {
                source: ev.actor,
                target: status_target,
                status: sa.status.clone(),
                duration_rounds: sa.duration_rounds,
            });
            log.push(CombatEvent::StatusApplied {
                target: status_target,
                status: sa.status.clone(),
            });
        }

        if def.rage_cost > 0 {
            if let Some(ref mut r) = rage {
                r.spend(def.rage_cost);
            }
        }
        if def.mana_cost > 0 {
            if let Some(ref mut m) = mana {
                m.spend(def.mana_cost);
            }
        }

        end_turn.write(EndTurn { actor: ev.actor });
    }
}

fn spell_breakdown(dice_str: &str, spell_power: i32, intelligence: i32, total: i32) -> String {
    let mut parts = vec![dice_str.to_string()];
    if spell_power != 0 {
        parts.push(format!("{}(маг)", spell_power));
    }
    if intelligence != 0 {
        parts.push(format!("{}(инт)", intelligence));
    }
    format!("{} = {}", parts.join(" + "), total)
}
