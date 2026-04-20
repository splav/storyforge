#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::combat::effects_outcome::{
    compute_ability_outcome, CritOutcome, OutcomePrimary, RngDice,
};
use crate::combat::effects_state::{compute_affected_targets, TargetRef, TargetState};
use crate::content::content_view::ActiveContent;
use crate::content::abilities::{AoEShape, CasterContext, EffectDef, TargetType};
use crate::content::races::CritFailEffect;
use crate::content::settings::GameSettings;
use crate::core::{DiceRng, ResourceKind};
use crate::game::components::{
    ActionPoints, BonusMovement, CombatPath, CombatStats, Combatant, Energy, Equipment, Faction, Mana, Rage, Team, Vital,
};
use crate::game::messages::{ApplyDamage, ApplyHeal, ApplyStatus, EndTurn, SpawnUnit, ValidatedAction};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::resources::HexPositions;
use bevy::prelude::*;

pub fn resolve_action_system(
    mut commands: Commands,
    content: Res<ActiveContent>,
    settings: Res<GameSettings>,
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
            Option<&CombatPath>,
        )>,
        Query<(Entity, &Faction, &Vital), With<Combatant>>,
    )>,
    mut dmg_writer: MessageWriter<ApplyDamage>,
    mut heal_writer: MessageWriter<ApplyHeal>,
    mut status_writer: MessageWriter<ApplyStatus>,
    mut spawn_writer: MessageWriter<SpawnUnit>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    // Collect events to allow alternating ParamSet access.
    let validated: Vec<ValidatedAction> = events.read().cloned().collect();

    for ev in &validated {
        let Some(def) = content.abilities.get(&ev.ability) else {
            continue;
        };

        let primary_target = match def.target_type {
            // Ground: no entity target; sentinel = actor. For AoE the value
            // is unused (enumeration is position-driven); for non-AoE Ground
            // (future teleport/spawn) it would be a no-op on the actor.
            TargetType::Myself | TargetType::Ground => ev.actor,
            TargetType::SingleEnemy | TargetType::SingleAlly => ev.target,
        };

        // Phase 1: compute affected entities using read-only combatant query.
        // Shared with the AI sim — see `combat::effects_state`.
        let affected: Vec<Entity> = {
            let combatants = actors.p1();
            let state = BevyTargetState {
                positions: &positions,
                combatants: &combatants,
            };
            compute_affected_targets(ev.actor, def, primary_target, ev.target_pos, &state)
        };

        // Phase 2: access actor mutably for costs, dice, effects.
        let mut actor_q = actors.p0();
        let Ok((stats, mut ap, equip, mut rage, mut mana, mut energy, mut vital, combat_path)) = actor_q.get_mut(ev.actor) else {
            continue;
        };

        ap.action_points = (ap.action_points - def.cost_ap).max(0);

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
            target_pos: ev.target_pos,
            is_aoe: def.aoe != AoEShape::None,
            cost_str,
        });

        let caster_ctx = CasterContext::new(stats, equip, &content.weapons);
        let crit_fail_effect = combat_path
            .and_then(|cp| content.paths.get(&cp.0))
            .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());

        // Roll crit-fail at the call site (real backend only); shared core
        // takes the bool and returns either a side-effect (`outcome.crit`)
        // or the normal damage/heal/status payload.
        let crit_failed = rng.roll_d(settings.crit_fail_die) == 1;
        let outcome = {
            let mut dice = RngDice(&mut rng);
            compute_ability_outcome(
                ev.actor,
                def,
                affected,
                &caster_ctx,
                ev.disadvantage,
                crit_failed,
                &crit_fail_effect,
                &mut dice,
            )
        };

        // Crit-fail side effects (if any). `ManaOverload` does NOT log
        // `CriticalMiss` — it surfaces through `WillOverload` at mana-payment
        // time below. The `skips_primary()` branches all share the miss log.
        match &outcome.crit {
            CritOutcome::None | CritOutcome::ManaOverload => {}
            CritOutcome::Miss => {
                log.push(CombatEvent::CriticalMiss { actor: ev.actor });
            }
            CritOutcome::SelfStatus { status, duration_rounds, log_description } => {
                log.push(CombatEvent::CriticalMiss { actor: ev.actor });
                status_writer.write(ApplyStatus {
                    source: ev.actor,
                    target: ev.actor,
                    status: status.clone(),
                    duration_rounds: *duration_rounds,
                });
                log.push(CombatEvent::CritFailSideEffect {
                    actor: ev.actor,
                    effect_name: log_description.clone(),
                });
            }
            CritOutcome::SelfDamage { amount, damage_breakdown, log_description } => {
                log.push(CombatEvent::CriticalMiss { actor: ev.actor });
                if *amount > 0 {
                    dmg_writer.write(ApplyDamage {
                        source: ev.actor,
                        target: ev.actor,
                        amount: *amount,
                        breakdown: damage_breakdown.clone(),
                        pierces_armor: true,
                    });
                }
                log.push(CombatEvent::CritFailSideEffect {
                    actor: ev.actor,
                    effect_name: log_description.clone(),
                });
            }
        }

        // Primary effects + statuses only run when no crit-fail skipped them.
        // `compute_ability_outcome` returns `primary = None` and empty
        // `statuses` on skip paths, so these matches no-op naturally.
        match &outcome.primary {
            OutcomePrimary::Damage { raw, pierces_armor } => {
                for &target in &outcome.affected {
                    dmg_writer.write(ApplyDamage {
                        source: ev.actor,
                        target,
                        amount: *raw,
                        breakdown: outcome.breakdown.clone(),
                        pierces_armor: *pierces_armor,
                    });
                }
            }
            OutcomePrimary::Heal { amount } => {
                for &target in &outcome.affected {
                    heal_writer.write(ApplyHeal {
                        source: ev.actor,
                        target,
                        amount: *amount,
                        breakdown: outcome.breakdown.clone(),
                    });
                }
            }
            OutcomePrimary::GrantMovement { distance } => {
                ap.movement_points += *distance;
                commands.entity(ev.actor).insert(BonusMovement);
            }
            OutcomePrimary::RestoreResources => {
                if let Some(ref mut m) = mana { m.restore(1); }
                if let Some(ref mut r) = rage { r.gain(); }
                if let Some(ref mut e) = energy { e.restore(1); }
                vital.apply_heal(1);
            }
            OutcomePrimary::Summon { template, max_active } => {
                spawn_writer.write(SpawnUnit {
                    summoner: ev.actor,
                    template_id: template.clone(),
                    max_active: *max_active,
                });
            }
            OutcomePrimary::None => {}
        }

        for app in &outcome.statuses {
            status_writer.write(ApplyStatus {
                source: ev.actor,
                target: app.target,
                status: app.status.clone(),
                duration_rounds: app.duration_rounds,
            });
            log.push(CombatEvent::StatusApplied {
                target: app.target,
                status: app.status.clone(),
            });
        }

        let mana_overload = outcome.crit.is_mana_overload();

        // Pay resource costs (always, even on crit miss).
        for cost in &def.costs {
            match cost.resource {
                ResourceKind::Hp => { vital.apply_damage(cost.amount); }
                ResourceKind::Mana => {
                    if let Some(ref mut m) = mana {
                        let amount = if mana_overload { cost.amount * 2 } else { cost.amount };
                        let actual_spend = m.current.min(amount);
                        let deficit = amount - actual_spend;
                        m.spend(actual_spend);
                        if deficit > 0 {
                            vital.apply_damage(deficit);
                        }
                        if mana_overload {
                            log.push(CombatEvent::WillOverload {
                                actor: ev.actor,
                                extra_mana: cost.amount,
                                hp_lost: deficit,
                            });
                        }
                    }
                }
                ResourceKind::Rage => { if let Some(ref mut r) = rage { r.spend(cost.amount); } }
                ResourceKind::Energy => { if let Some(ref mut e) = energy { e.spend(cost.amount); } }
            }
        }

        // End turn only when AP pool is exhausted. With max_ap=1 this always
        // fires after a cast (old behaviour). With larger pools, remaining AP
        // lets the actor take another action in this turn.
        if !matches!(def.effect, EffectDef::GrantMovement { .. }) && ap.action_points <= 0 {
            end_turn.write(EndTurn { actor: ev.actor });
        }
    }
}

/// `TargetState` adapter over the live combatant query + `HexPositions`.
/// Thin shim — the filtering logic lives in
/// `combat::effects_state::compute_affected_targets`.
struct BevyTargetState<'a, 'w, 's> {
    positions: &'a HexPositions,
    combatants: &'a Query<'w, 's, (Entity, &'static Faction, &'static Vital), With<Combatant>>,
}

impl TargetState for BevyTargetState<'_, '_, '_> {
    fn actor_pos(&self, actor: Entity) -> Option<hexx::Hex> {
        self.positions.get(&actor)
    }
    fn unit_at_cell(&self, pos: hexx::Hex) -> Option<TargetRef> {
        let entity = self.positions.entity_at(pos)?;
        let (_, faction, vital) = self.combatants.get(entity).ok()?;
        Some(TargetRef {
            entity,
            team: faction.0,
            alive: vital.is_alive(),
        })
    }
    fn team_of(&self, entity: Entity) -> Option<Team> {
        self.combatants.get(entity).ok().map(|(_, f, _)| f.0)
    }
}

