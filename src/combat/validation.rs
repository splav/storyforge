//! Bevy-side action-legality gate.  Wires `combat_engine::check_legality`
//! against live ECS queries; rejected actions still end the turn to keep
//! the pipeline forward-moving.
//!
//! All substantive rules live in `combat_engine::legality`; this file is a
//! thin adapter that translates between Bevy components and the engine's
//! `ActionState` trait.

use bevy::prelude::*;

use combat_engine::legality::{check_legality, ActionState, ActorView, ProposedAction};
use combat_engine::{
    AbilityDef, AbilityId, AbilityRange, AoEShape, Cost, StatusDef, StatusId, TargetType,
};

use crate::content::abilities;
use crate::content::content_view::ActiveContent;
use crate::game::components::{ActiveCombatant, Team, ValidationActorQ, ValidationTargetQ};
use crate::game::hex::{in_bounds, Hex};
use crate::game::messages::{EndTurn, UseAbility, ValidatedAction};
use crate::game::resources::HexPositions;

#[allow(clippy::too_many_arguments)]
pub fn validate_action_system(
    active_q: Query<Entity, With<ActiveCombatant>>,
    content: Res<ActiveContent>,
    positions: Res<HexPositions>,
    mut events: MessageReader<UseAbility>,
    actors: Query<ValidationActorQ>,
    targets: Query<ValidationTargetQ>,
    mut validated: MessageWriter<ValidatedAction>,
    mut end_turn: MessageWriter<EndTurn>,
) {
    let active = active_q.single().ok();
    for ev in events.read() {
        // Turn-ownership is outside the legality layer's scope — gate here.
        // Stray `UseAbility` from a non-current actor is silently dropped
        // (no EndTurn to avoid ending the real current actor's turn).
        if active != Some(ev.actor) {
            continue;
        }

        let state = BevyActions {
            content: &content,
            positions: &positions,
            actors: &actors,
            targets: &targets,
        };
        let proposal = ProposedAction {
            actor: ev.actor,
            ability: &ev.ability,
            target: ev.target,
            target_pos: ev.target_pos,
        };
        match check_legality(proposal, &state) {
            Ok(outcome) => {
                validated.write(ValidatedAction {
                    actor: ev.actor,
                    ability: ev.ability.clone(),
                    target: ev.target,
                    target_pos: ev.target_pos,
                    disadvantage: outcome.disadvantage,
                });
            }
            Err(_reason) => {
                // Rejected action still ends the turn to prevent infinite
                // loops from a stuck command source.  UI tooltip rendering
                // (where `_reason` matters) is a separate surface — Phase 2
                // step 2c migration kept the fail-forward behavior.
                end_turn.write(EndTurn { actor: ev.actor });
            }
        }
    }
}

// ── Bevy adapter ───────────────────────────────────────────────────────────

/// `ActionState` impl over live ECS queries.  Holds references with a single
/// named lifetime `'a` — every borrow taken from the system's parameters
/// lives at least as long as the adapter, which is built and consumed
/// inside one `validate_action_system` iteration.
///
/// No borrows leak through the trait: `actor_view` returns an owned
/// `ActorView` copy, `actor_knows_ability` answers a direct bool.
pub struct BevyActions<'w, 's, 'a> {
    pub content: &'a ActiveContent,
    pub positions: &'a HexPositions,
    pub actors: &'a Query<'w, 's, ValidationActorQ>,
    pub targets: &'a Query<'w, 's, ValidationTargetQ>,
}

impl ActionState for BevyActions<'_, '_, '_> {
    type Id = Entity;

    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> {
        let def = self.content.abilities.get(id)?;
        Some(AbilityDef {
            key: def.key.clone(),
            cost_ap: def.cost_ap,
            costs: def
                .costs
                .iter()
                .map(|c| Cost { resource: c.resource, amount: c.amount })
                .collect(),
            range: AbilityRange { min: def.range.min, max: def.range.max },
            target_type: match def.target_type {
                abilities::TargetType::SingleEnemy => TargetType::SingleEnemy,
                abilities::TargetType::SingleAlly => TargetType::SingleAlly,
                abilities::TargetType::Myself => TargetType::Myself,
                abilities::TargetType::Ground => TargetType::Ground,
            },
            aoe: match def.aoe {
                abilities::AoEShape::None => AoEShape::None,
                abilities::AoEShape::Circle { radius } => AoEShape::Circle { radius },
                abilities::AoEShape::Line { length } => AoEShape::Line { length },
            },
            friendly_fire: def.friendly_fire,
        })
    }

    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        let def = self.content.statuses.get(id)?;
        Some(StatusDef {
            causes_disadvantage: def.causes_disadvantage,
            blocks_mana_abilities: def.blocks_mana_abilities,
            forces_targeting: def.forces_targeting,
            skips_turn: def.skips_turn,
            armor_bonus: def.armor_bonus,
            damage_taken_bonus: def.damage_taken_bonus,
            speed_bonus: def.speed_bonus,
        })
    }

    fn actor_view(&self, actor: Entity) -> Option<ActorView> {
        let pos = self.positions.get(&actor)?;
        let a = self.actors.get(actor).ok()?;
        let (causes_disadvantage, blocks_mana_abilities) = match a.statuses {
            Some(se) => se.0.iter().fold((false, false), |(d, m), s| {
                let def = self.content.statuses.get(&s.id);
                (
                    d || def.is_some_and(|x| x.causes_disadvantage),
                    m || def.is_some_and(|x| x.blocks_mana_abilities),
                )
            }),
            None => (false, false),
        };
        Some(ActorView {
            pos,
            team: a.faction.0,
            hp: a.vital.hp,
            ap: a.ap.action_points,
            mana: a.mana.map(|m| m.current),
            rage: a.rage.map(|r| r.current),
            energy: a.energy.map(|e| e.current),
            causes_disadvantage,
            blocks_mana_abilities,
            is_alive: a.vital.is_alive(),
        })
    }

    fn actor_knows_ability(&self, actor: Entity, ability: &AbilityId) -> bool {
        self.actors
            .get(actor)
            .map(|a| a.abilities.0.contains(ability))
            .unwrap_or(false)
    }

    fn is_target_alive(&self, target: Entity) -> Option<bool> {
        self.targets.get(target).ok().map(|t| t.vital.is_alive())
    }

    fn target_team(&self, target: Entity) -> Option<Team> {
        self.targets.get(target).ok().map(|t| t.faction.0)
    }

    fn taunter_for(&self, actor_team: Team) -> Option<Entity> {
        self.targets
            .iter()
            .find(|t| {
                t.vital.is_alive()
                    && t.faction.0 != actor_team
                    && t.statuses.is_some_and(|se| {
                        se.0.iter().any(|s| {
                            self.content
                                .statuses
                                .get(&s.id)
                                .is_some_and(|d| d.forces_targeting)
                        })
                    })
            })
            .map(|t| t.entity)
    }

    fn is_in_bounds(&self, pos: Hex) -> bool {
        in_bounds(pos)
    }
}
