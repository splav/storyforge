use crate::content::abilities::{CasterContext, EffectDef, StatusOn, TargetType};
use crate::core::DiceRng;
use crate::game::components::{ActionPoints, BonusMovement, CombatStats, Equipment, Mana, Rage};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, ValidatedAction};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::GameDb;
use bevy::prelude::*;

pub fn resolve_action_system(
    mut commands: Commands,
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
    mut log: ResMut<CombatLog>,
    mut events: MessageReader<ValidatedAction>,
    mut actors: Query<(
        &CombatStats,
        &mut ActionPoints,
        Option<&Equipment>,
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
        let Ok((stats, mut ap, equip, mut rage, mut mana)) = actors.get_mut(ev.actor) else {
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

        let ctx = CasterContext::new(stats, equip, &db.weapons);

        if let Some(calc) = def.effect.calc(&ctx) {
            let (roll_total, dice_str) = if let Some(ref dice) = calc.dice {
                rng.roll_dice(dice)
            } else {
                (0, String::new())
            };
            let raw = roll_total + calc.bonus;
            let breakdown = effect_breakdown(&dice_str, calc.bonus, raw);

            if calc.is_heal {
                heal_writer.write(ApplyHeal {
                    source: ev.actor,
                    target,
                    amount: raw,
                    breakdown,
                });
            } else {
                dmg_writer.write(ApplyDamage {
                    source: ev.actor,
                    target,
                    amount: raw,
                    breakdown,
                    pierces_armor: calc.pierces_armor,
                });
            }
        } else if let EffectDef::GrantMovement { distance } = &def.effect {
            ap.movement = true;
            commands.entity(ev.actor).insert(BonusMovement(*distance));
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

        if !matches!(def.effect, EffectDef::GrantMovement { .. }) {
            end_turn.write(EndTurn { actor: ev.actor });
        }
    }
}

fn effect_breakdown(dice_str: &str, bonus: i32, total: i32) -> String {
    if dice_str.is_empty() {
        return format!("{total}");
    }
    if bonus == 0 {
        format!("{dice_str} = {total}")
    } else {
        format!("{dice_str} + {bonus} = {total}")
    }
}
