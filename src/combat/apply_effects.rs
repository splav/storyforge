use crate::game::components::{Combatant, Dead, Rage, StatusEffects, Vital};
use crate::game::messages::{ApplyDamage, ApplyHeal};
use crate::game::resources::{CombatEvent, CombatLog, GameDb};
use bevy::prelude::*;

/// Consumes ApplyDamage and ApplyHeal messages.
/// Applies damage (with armor + status mitigation), healing, death, and rage gain.
pub fn apply_effects_system(
    mut commands: Commands,
    mut dmg_events: MessageReader<ApplyDamage>,
    mut heal_events: MessageReader<ApplyHeal>,
    mut vitals: Query<&mut Vital>,
    statuses: Query<&StatusEffects, With<Combatant>>,
    mut rage_query: Query<&mut Rage>,
    mut log: ResMut<CombatLog>,
    db: Res<GameDb>,
) {
    let damages: Vec<(Entity, Entity, i32, String, bool)> = dmg_events
        .read()
        .map(|e| {
            (
                e.target,
                e.source,
                e.amount,
                e.breakdown.clone(),
                e.pierces_armor,
            )
        })
        .collect();
    let heals: Vec<(Entity, i32, String)> = heal_events
        .read()
        .map(|e| (e.target, e.amount, e.breakdown.clone()))
        .collect();

    // Apply damage with armor + status mitigation; mark dead units.
    for (target, _source, raw, formula, pierces_armor) in &damages {
        let Ok(mut v) = vitals.get_mut(*target) else {
            continue;
        };

        let status_sums = statuses
            .get(*target)
            .map(|se| {
                se.0.iter()
                    .filter_map(|s| db.statuses.get(&s.id))
                    .fold((0i32, 0i32), |(armor, vuln), def| {
                        (armor + def.armor_bonus, vuln + def.damage_taken_bonus)
                    })
            })
            .unwrap_or((0, 0));

        let total_armor = if *pierces_armor {
            0
        } else {
            v.armor + status_sums.0
        };
        let vulnerability = status_sums.1;

        let final_damage = (raw - total_armor + vulnerability).max(1);
        v.apply_damage(final_damage);

        log.push(CombatEvent::DamageResult {
            target: *target,
            formula: formula.clone(),
            armor_reduced: total_armor,
            final_damage,
        });

        if !v.is_alive() {
            commands.entity(*target).insert(Dead);
            log.push(CombatEvent::UnitDied { entity: *target });
        }
    }

    // Apply heals.
    for (target, amount, formula) in &heals {
        if let Ok(mut v) = vitals.get_mut(*target) {
            let before = v.hp;
            v.apply_heal(*amount);
            let actual = v.hp - before;
            log.push(CombatEvent::HealResult {
                target: *target,
                formula: formula.clone(),
                amount: actual,
            });
        }
    }

    // Rage: +1 for attacker (dealt damage) and defender (received damage).
    for (target, source, _, _, _) in &damages {
        for actor in [source, target] {
            if let Ok(mut rage) = rage_query.get_mut(*actor) {
                let current = rage.gain();
                log.push(CombatEvent::RageGained {
                    actor: *actor,
                    current,
                    max: rage.max,
                });
            }
        }
    }
}
