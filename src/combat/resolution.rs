use bevy::prelude::*;
use crate::content::abilities::{EffectDef, TargetType};
use crate::core::DiceRng;
use crate::game::components::{ActionPoints, CombatStats, EquippedWeapon};
use crate::game::messages::{ApplyDamage, ApplyStatus, EndTurn, ValidatedAction};
use crate::game::resources::{CombatEvent, CombatLog, GameDb};

pub fn resolve_action_system(
    db: Res<GameDb>,
    mut rng: ResMut<DiceRng>,
    mut log: ResMut<CombatLog>,
    mut events: MessageReader<ValidatedAction>,
    mut actors: Query<(&CombatStats, &mut ActionPoints, Option<&EquippedWeapon>)>,
    mut dmg_writer: MessageWriter<ApplyDamage>,
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
        };

        match &def.effect {
            EffectDef::WeaponAttack => {
                let dice = weapon
                    .and_then(|w| db.weapons.get(&w.0))
                    .map(|wd| wd.dice.clone());

                let raw = if let Some(dice) = dice {
                    rng.roll(&dice) + stats.damage
                } else {
                    stats.damage // no weapon: flat damage only
                };

                dmg_writer.write(ApplyDamage { source: ev.actor, target, amount: raw });
                log.push(CombatEvent::DamageDealt { source: ev.actor, target, amount: raw });
            }
            EffectDef::Damage { dice } => {
                let raw = rng.roll(dice) + stats.damage;
                dmg_writer.write(ApplyDamage { source: ev.actor, target, amount: raw });
                log.push(CombatEvent::DamageDealt { source: ev.actor, target, amount: raw });
            }
            EffectDef::ApplyStatus { status, duration_rounds } => {
                status_writer.write(ApplyStatus {
                    target,
                    status: *status,
                    duration_rounds: *duration_rounds,
                });
                log.push(CombatEvent::StatusApplied { target, status: *status });
            }
        }

        end_turn.write(EndTurn { actor: ev.actor });
    }
}
