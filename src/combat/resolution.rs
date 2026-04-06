use bevy::prelude::*;
use crate::content::abilities::{EffectDef, TargetType};
use crate::core::DiceRng;
use crate::game::components::{ActionPoints, CombatStats, EquippedWeapon};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, ValidatedAction};
use crate::game::resources::{CombatEvent, CombatLog, GameDb};

pub fn resolve_action_system(
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
    mut log: ResMut<CombatLog>,
    mut events: MessageReader<ValidatedAction>,
    mut actors: Query<(&CombatStats, &mut ActionPoints, Option<&EquippedWeapon>)>,
    mut dmg_writer: MessageWriter<ApplyDamage>,
    mut heal_writer: MessageWriter<ApplyHeal>,
    mut status_writer: MessageWriter<ApplyStatus>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    for ev in events.read() {
        let Some(def) = db.abilities.get(&ev.ability) else { continue };
        let Ok((stats, mut ap, weapon)) = actors.get_mut(ev.actor) else { continue };

        ap.action = false;

        let target = match def.target_type {
            TargetType::Myself      => ev.actor,
            TargetType::SingleEnemy => ev.target,
            TargetType::SingleAlly  => ev.target,
        };

        log.push(CombatEvent::AbilityUsed {
            actor: ev.actor,
            ability_name: def.name.clone(),
            target,
        });

        match &def.effect {
            EffectDef::WeaponAttack => {
                let weapon_def = weapon.and_then(|w| db.weapons.get(&w.0));
                let (raw, breakdown) = if let Some(wd) = weapon_def {
                    let (dice_total, dice_str) = rng.roll_dice(&wd.dice);
                    let raw = dice_total + stats.damage;
                    let s = if stats.damage != 0 {
                        format!("{} + {}(атк) = {}", dice_str, stats.damage, raw)
                    } else {
                        format!("{} = {}", dice_str, raw)
                    };
                    (raw, s)
                } else {
                    (stats.damage, format!("{}(атк)", stats.damage))
                };
                dmg_writer.write(ApplyDamage { source: ev.actor, target, amount: raw, breakdown });
            }
            EffectDef::Damage { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let raw = dice_total + stats.damage;
                let breakdown = if stats.damage != 0 {
                    format!("{} + {}(атк) = {}", dice_str, stats.damage, raw)
                } else {
                    format!("{} = {}", dice_str, raw)
                };
                dmg_writer.write(ApplyDamage { source: ev.actor, target, amount: raw, breakdown });
            }
            EffectDef::SpellDamage { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let sp = weapon.and_then(|w| db.weapons.get(&w.0)).map_or(0, |wd| wd.spell_power);
                let intel = stats.intelligence;
                let raw = dice_total + sp + intel;
                let breakdown = spell_breakdown(&dice_str, sp, intel, raw);
                dmg_writer.write(ApplyDamage { source: ev.actor, target, amount: raw, breakdown });
            }
            EffectDef::Heal { dice } => {
                let (dice_total, dice_str) = rng.roll_dice(dice);
                let sp = weapon.and_then(|w| db.weapons.get(&w.0)).map_or(0, |wd| wd.spell_power);
                let intel = stats.intelligence;
                let amount = dice_total + sp + intel;
                let breakdown = spell_breakdown(&dice_str, sp, intel, amount);
                heal_writer.write(ApplyHeal { source: ev.actor, target, amount, breakdown });
            }
            EffectDef::ApplyStatus { status, duration_rounds } => {
                status_writer.write(ApplyStatus {
                    target,
                    status: status.clone(),
                    duration_rounds: *duration_rounds,
                });
                log.push(CombatEvent::StatusApplied { target, status: status.clone() });
            }
        }

        end_turn.write(EndTurn { actor: ev.actor });
    }
}

fn spell_breakdown(dice_str: &str, spell_power: i32, intelligence: i32, total: i32) -> String {
    let mut parts = vec![dice_str.to_string()];
    if spell_power != 0 { parts.push(format!("{}(сила)", spell_power)); }
    if intelligence != 0 { parts.push(format!("{}(инт)", intelligence)); }
    format!("{} = {}", parts.join(" + "), total)
}
