#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::combat::effects_math::final_damage_i32;
use crate::content::abilities::{CasterContext, EffectDef};
use crate::content::content_view::ActiveContent;
use crate::core::DiceRng;
use crate::game::components::{
    Abilities, ActionPoints, ActiveCombatant, BonusMovement, CombatStats, Combatant, Dead,
    Equipment, Faction, Rage, Reactions, StatusEffects, UnitToken, Vital,
};
use crate::game::hex::{in_bounds, Hex, LAYOUT};
use crate::game::messages::MoveUnit;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::HexPositions;
use crate::ui::animation::{AnimationQueue, PendingAnim};
use crate::ui::hex_grid::HexGridOffset;
use bevy::prelude::*;

/// Local snapshot of a potential AoO provoker. We cache the data up front so
/// the path walk does not juggle ECS queries per step.
struct Provoker {
    entity: Entity,
    pos: Hex,
    dice: crate::core::DiceExpr,
    str_mod: i32,
    reactions_left: u8,
}

pub fn movement_system(
    mut commands: Commands,
    active_q: Query<Entity, With<ActiveCombatant>>,
    mut events: MessageReader<MoveUnit>,
    mut positions: ResMut<HexPositions>,
    mut movers: Query<(
        &Faction,
        &mut ActionPoints,
        Option<&StatusEffects>,
        Has<BonusMovement>,
    )>,
    content: Res<ActiveContent>,
    mut log: ResMut<CombatLog>,
    tokens: Query<(Entity, &UnitToken)>,
    grid_offset: Res<HexGridOffset>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut rng: ResMut<DiceRng>,
    mut combatants: ParamSet<(
        Query<
            (
                Entity,
                &Faction,
                &Abilities,
                &Equipment,
                &CombatStats,
                &Vital,
                Option<&StatusEffects>,
                Has<Dead>,
            ),
            With<Combatant>,
        >,
        Query<&mut Vital>,
    )>,
    mut reactions_q: Query<&mut Reactions>,
    mut rage_q: Query<&mut Rage>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        if active != Some(ev.actor) {
            continue;
        }
        if ev.path.is_empty() {
            continue;
        }

        let Ok((a_faction, mut ap, a_statuses, has_bonus)) = movers.get_mut(ev.actor) else {
            continue;
        };
        if !ap.can_move() {
            continue;
        }
        if ev.path.len() as i32 > ap.movement_points {
            continue;
        }

        let dest = *ev.path.last().unwrap();
        if !in_bounds(dest) {
            continue;
        }

        let dest_occupied = positions
            .entity_at(dest)
            .is_some_and(|e| e != ev.actor);
        if dest_occupied {
            continue;
        }

        let old_pos = positions.get(&ev.actor).unwrap_or(Hex::ZERO);

        // Precompute A's mitigation from armor + statuses (for AoO damage).
        // A's Vital is read through the readonly combatant snapshot below.
        let a_team = a_faction.0;
        let (a_status_armor, a_vulnerability) = a_statuses
            .map(|se| {
                se.0.iter()
                    .filter_map(|s| content.statuses.get(&s.id))
                    .fold((0i32, 0i32), |(armor, vuln), def| {
                        (armor + def.armor_bonus, vuln + def.damage_taken_bonus)
                    })
            })
            .unwrap_or((0, 0));

        // Build provoker snapshot: enemies, alive, not stunned, melee weapon_attack, reactions left.
        let mut provokers: Vec<Provoker> = Vec::new();
        let mut a_base_armor = 0;
        let mut a_hp = 0;
        {
            let combatants_ro = combatants.p0();
            for (entity, faction, abilities, equipment, stats, vital, statuses, is_dead) in
                combatants_ro.iter()
            {
                if entity == ev.actor {
                    a_base_armor = vital.armor;
                    a_hp = vital.hp;
                    continue;
                }
                if is_dead || !vital.is_alive() {
                    continue;
                }
                if faction.0 == a_team {
                    continue;
                }
                // Stunned check: any status with skips_turn.
                let stunned = statuses
                    .map(|se| {
                        se.0.iter().any(|s| {
                            content
                                .statuses
                                .get(&s.id)
                                .is_some_and(|d| d.skips_turn)
                        })
                    })
                    .unwrap_or(false);
                if stunned {
                    continue;
                }
                // Melee weapon_attack ability with range.max == 1.
                let has_melee = abilities.0.iter().any(|aid| {
                    content.abilities.get(aid).is_some_and(|def| {
                        matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
                    })
                });
                if !has_melee {
                    continue;
                }
                let reactions_left = reactions_q
                    .get(entity)
                    .map(|r| r.remaining)
                    .unwrap_or(0);
                if reactions_left == 0 {
                    continue;
                }
                let Some(pos) = positions.get(&entity) else { continue };
                // Prefer the equipped main-hand weapon dice; if no weapon, skip.
                let ctx = CasterContext::new(stats, Some(equipment), &content.weapons);
                let Some(dice) = ctx.weapon_dice.clone() else { continue };
                provokers.push(Provoker {
                    entity,
                    pos,
                    dice,
                    str_mod: ctx.str_mod,
                    reactions_left,
                });
            }
        }

        let a_total_armor = a_base_armor + a_status_armor;

        // Walk the path step by step; fire AoOs; truncate on death.
        let mut hp_sim = a_hp;
        let mut prev_pos = old_pos;
        let mut walked: Vec<Hex> = Vec::new();
        let mut fired: Vec<(Entity, i32, bool)> = Vec::new(); // (attacker, damage, killed)
        let mut died = false;

        'path_walk: for &step in &ev.path {
            walked.push(step);
            for p in provokers.iter_mut() {
                if p.reactions_left == 0 {
                    continue;
                }
                let was_adj = prev_pos.unsigned_distance_to(p.pos) == 1;
                let still_adj = step.unsigned_distance_to(p.pos) == 1;
                if !was_adj || still_adj {
                    continue;
                }
                let (roll, _dice_str) = rng.roll_dice(&p.dice);
                let raw = roll + p.str_mod;
                let final_dmg =
                    final_damage_i32(raw, a_total_armor, a_vulnerability, /* pierces_armor */ false);
                hp_sim = (hp_sim - final_dmg).max(0);
                p.reactions_left -= 1;
                let killed = hp_sim == 0;
                fired.push((p.entity, final_dmg, killed));
                if killed {
                    died = true;
                    break 'path_walk;
                }
            }
            prev_pos = step;
        }

        let final_pos = walked.last().copied().unwrap_or(old_pos);

        // Apply AoO damage to A's vital (single aggregated mutation).
        let total_dmg: i32 = fired.iter().map(|(_, d, _)| *d).sum();
        if total_dmg > 0 {
            if let Ok(mut a_vital) = combatants.p1().get_mut(ev.actor) {
                a_vital.apply_damage(total_dmg);
                if !a_vital.is_alive() {
                    commands.entity(ev.actor).insert(Dead);
                }
            }
        }

        // Decrement reactions and grant rage.
        for (attacker, dmg, killed) in &fired {
            if let Ok(mut r) = reactions_q.get_mut(*attacker) {
                r.remaining = r.remaining.saturating_sub(1);
            }
            for actor in [*attacker, ev.actor] {
                if let Ok(mut rage) = rage_q.get_mut(actor) {
                    let current = rage.gain();
                    log.push(CombatEvent::RageGained {
                        actor,
                        current,
                        max: rage.max,
                    });
                }
            }
            log.push(CombatEvent::OpportunityAttack {
                attacker: *attacker,
                target: ev.actor,
                damage: *dmg,
                killed: *killed,
            });
        }
        if died {
            log.push(CombatEvent::UnitDied { entity: ev.actor });
        }

        // Animation: include only the hexes actually walked.
        let offset = grid_offset.0;
        let mut waypoints = vec![LAYOUT.hex_to_world_pos(old_pos) + offset];
        for &h in &walked {
            waypoints.push(LAYOUT.hex_to_world_pos(h) + offset);
        }
        if let Some((token_entity, _)) = tokens.iter().find(|(_, t)| t.0 == ev.actor) {
            anim_queue.0.push_back(PendingAnim::Movement {
                token: token_entity,
                waypoints,
            });
        }

        if final_pos != old_pos {
            // Defensive: if something else (a corpse, a freshly-spawned unit)
            // occupies the destination between AI planning and movement
            // execution, `positions.insert` would trip a debug_assert. Log
            // enough context to debug and abort the move rather than crash.
            if let Some(occupant) = positions.entity_at(final_pos) {
                if occupant != ev.actor {
                    warn!(
                        "movement: {:?} wanted to land on {:?} but it's held by {:?}; move aborted at {:?}",
                        ev.actor, final_pos, occupant, old_pos,
                    );
                    // Still spend the already-walked MP so the actor doesn't
                    // effectively get a free retry next tick.
                    ap.movement_points = (ap.movement_points - walked.len() as i32).max(0);
                    continue;
                }
            }
            positions.insert(ev.actor, final_pos);
        }

        ap.movement_points = (ap.movement_points - walked.len() as i32).max(0);

        if has_bonus && ap.movement_points == 0 {
            commands.entity(ev.actor).remove::<BonusMovement>();
        }

        log.push(CombatEvent::UnitMoved {
            actor: ev.actor,
            from: old_pos,
            to: final_pos,
        });
    }
}
