use crate::content::abilities::{AoEShape, CasterContext, EffectDef, StatusOn, TargetType};
use crate::core::DiceRng;
use crate::game::components::{
    ActionPoints, BonusMovement, CombatStats, Combatant, Equipment, Faction, Mana, Rage, Team, Vital,
};
use crate::game::hex::{hex_circle, hex_line};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, ValidatedAction};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::{GameDb, HexPositions};
use bevy::prelude::*;

pub fn resolve_action_system(
    mut commands: Commands,
    db: Res<GameDb>,
    positions: Res<HexPositions>,
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
    combatant_q: Query<(Entity, &Faction, &Vital), With<Combatant>>,
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

        // Primary target (for log display).
        let primary_target = match def.target_type {
            TargetType::Myself => ev.actor,
            TargetType::SingleEnemy | TargetType::SingleAlly => ev.target,
        };

        let cost_str = {
            let mut parts = Vec::new();
            if def.mana_cost > 0 {
                if let Some(ref m) = mana {
                    parts.push(format!(
                        "мана: {} - {} = {}",
                        m.current, def.mana_cost, m.current - def.mana_cost
                    ));
                }
            }
            if def.rage_cost > 0 {
                if let Some(ref r) = rage {
                    parts.push(format!(
                        "ярость: {} - {} = {}",
                        r.current, def.rage_cost, r.current - def.rage_cost
                    ));
                }
            }
            parts.join(", ")
        };
        log.push(CombatEvent::AbilityUsed {
            actor: ev.actor,
            ability_name: def.name.clone(),
            target: primary_target,
            cost_str,
        });

        let ctx = CasterContext::new(stats, equip, &db.weapons);

        // Compute all affected targets.
        let affected: Vec<Entity> = compute_aoe_targets(
            ev.actor,
            ev.target_pos,
            def.aoe,
            def.friendly_fire,
            primary_target,
            &positions,
            &combatant_q,
        );

        if let Some(calc) = def.effect.calc(&ctx) {
            // Roll dice ONCE for the entire AoE.
            let (roll_total, dice_str) = if let Some(ref dice) = calc.dice {
                if ev.disadvantage {
                    rng.roll_dice_disadvantage(dice)
                } else {
                    rng.roll_dice(dice)
                }
            } else {
                (0, String::new())
            };
            let raw = roll_total + calc.bonus;
            let breakdown = effect_breakdown(&dice_str, calc.bonus, raw);

            for &target in &affected {
                if calc.is_heal {
                    heal_writer.write(ApplyHeal {
                        source: ev.actor,
                        target,
                        amount: raw,
                        breakdown: breakdown.clone(),
                    });
                } else {
                    dmg_writer.write(ApplyDamage {
                        source: ev.actor,
                        target,
                        amount: raw,
                        breakdown: breakdown.clone(),
                        pierces_armor: calc.pierces_armor,
                    });
                }
            }
        } else if let EffectDef::GrantMovement { distance } = &def.effect {
            ap.movement = true;
            commands.entity(ev.actor).insert(BonusMovement(*distance));
        }

        for sa in &def.statuses {
            for &target in &affected {
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

/// Compute all entities affected by an ability.
/// For single-target (no AoE): returns just the primary target.
/// For AoE: returns all living combatants in the area, filtered by friendly_fire.
fn compute_aoe_targets(
    actor: Entity,
    target_pos: (i32, i32),
    aoe: AoEShape,
    friendly_fire: bool,
    primary_target: Entity,
    positions: &HexPositions,
    combatant_q: &Query<(Entity, &Faction, &Vital), With<Combatant>>,
) -> Vec<Entity> {
    if aoe == AoEShape::None {
        return vec![primary_target];
    }

    let actor_pos = positions.get(&actor).unwrap_or((0, 0));

    let affected_cells: Vec<(i32, i32)> = match aoe {
        AoEShape::None => unreachable!(),
        AoEShape::Circle { radius } => hex_circle(target_pos.0, target_pos.1, radius),
        AoEShape::Line { length } => hex_line(actor_pos.0, actor_pos.1, target_pos.0, target_pos.1, length),
    };

    let actor_team = combatant_q
        .get(actor)
        .map(|(_, f, _)| f.0)
        .unwrap_or(Team::Player);

    let mut targets = Vec::new();
    for &(q, r) in &affected_cells {
        if let Some(entity) = positions.entity_at(q, r) {
            if entity == actor {
                // Caster can be hit by own AoE only with friendly_fire.
                if friendly_fire {
                    if let Ok((_, _, vital)) = combatant_q.get(entity) {
                        if vital.is_alive() {
                            targets.push(entity);
                        }
                    }
                }
                continue;
            }
            if let Ok((_, faction, vital)) = combatant_q.get(entity) {
                if !vital.is_alive() {
                    continue;
                }
                if !friendly_fire && faction.0 == actor_team {
                    continue;
                }
                targets.push(entity);
            }
        }
    }

    targets
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
