use crate::content::abilities::{AoEShape, CasterContext, EffectDef, StatusOn, TargetType};
use crate::core::{DiceRng, ResourceKind};
use crate::game::components::{
    ActionPoints, BonusMovement, CombatStats, Combatant, Energy, Equipment, Faction, Mana, Rage, Team, Vital,
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
    mut actors: ParamSet<(
        Query<(
            &CombatStats,
            &mut ActionPoints,
            Option<&Equipment>,
            Option<&mut Rage>,
            Option<&mut Mana>,
            Option<&mut Energy>,
            &mut Vital,
        )>,
        Query<(Entity, &Faction, &Vital), With<Combatant>>,
    )>,
    mut dmg_writer: MessageWriter<ApplyDamage>,
    mut heal_writer: MessageWriter<ApplyHeal>,
    mut status_writer: MessageWriter<ApplyStatus>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    // Collect events to allow alternating ParamSet access.
    let validated: Vec<ValidatedAction> = events.read().cloned().collect();

    for ev in &validated {
        let Some(def) = db.abilities.get(&ev.ability) else {
            continue;
        };

        let primary_target = match def.target_type {
            TargetType::Myself => ev.actor,
            TargetType::SingleEnemy | TargetType::SingleAlly => ev.target,
        };

        // Phase 1: compute AoE targets using read-only combatant query.
        let affected: Vec<Entity> = compute_aoe_targets(
            ev.actor,
            ev.target_pos,
            def.aoe,
            def.friendly_fire,
            primary_target,
            &positions,
            &actors.p1(),
        );

        // Phase 2: access actor mutably for costs, dice, effects.
        let mut actor_q = actors.p0();
        let Ok((stats, mut ap, equip, mut rage, mut mana, mut energy, mut vital)) = actor_q.get_mut(ev.actor) else {
            continue;
        };

        ap.action = false;

        let cost_str = {
            let mut parts = Vec::new();
            for cost in &def.costs {
                let (label, current) = match cost.resource {
                    ResourceKind::Hp => ("HP", vital.hp),
                    ResourceKind::Mana => ("мана", mana.as_ref().map_or(0, |m| m.current)),
                    ResourceKind::Rage => ("ярость", rage.as_ref().map_or(0, |r| r.current)),
                    ResourceKind::Energy => ("энергия", energy.as_ref().map_or(0, |e| e.current)),
                };
                parts.push(format!(
                    "{}: {} - {} = {}",
                    label, current, cost.amount, current - cost.amount
                ));
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

        for cost in &def.costs {
            match cost.resource {
                ResourceKind::Hp => { vital.apply_damage(cost.amount); }
                ResourceKind::Mana => { if let Some(ref mut m) = mana { m.spend(cost.amount); } }
                ResourceKind::Rage => { if let Some(ref mut r) = rage { r.spend(cost.amount); } }
                ResourceKind::Energy => { if let Some(ref mut e) = energy { e.spend(cost.amount); } }
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
