//! Bevy-side `ActionState` adapter for `combat_engine::check_legality`.
//!
//! `BevyActions` wires the engine's legality layer against live ECS queries.
//! UI tooltip code calls `combat_engine::check_legality` against an instance
//! of this struct to determine whether a proposed action is legal (and why not,
//! for display purposes).
//!
//! The former `validate_action_system` (which translated `UseAbility` →
//! `ValidatedAction`) has been deleted in Phase 2 step 9d; the engine bridge's
//! `process_action_system` now handles legality via `Action::Cast` in
//! `combat_engine::step()`.

use bevy::prelude::*;

use combat_engine::legality::{ActionState, ActorView};
use combat_engine::{
    AbilityDef, AbilityId, AbilityRange, AoEShape, Cost, EffectDef as EngineEffectDef,
    StatusApplication as EngineStatusApplication, StatusDef, StatusId, StatusOn as EngineStatusOn,
    TargetType,
};

use crate::content::abilities;
use crate::content::content_view::ActiveContent;
use crate::game::components::{Team, ValidationActorQ, ValidationTargetQ};
use crate::game::hex::{in_bounds, Hex};
use crate::game::resources::HexPositions;

/// `ActionState` impl over live ECS queries.  Holds references with a single
/// named lifetime `'a` — every borrow taken from the system's parameters
/// lives at least as long as the adapter, which is built and consumed
/// inside one system iteration.
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
            effect: match &def.effect {
                abilities::EffectDef::None => EngineEffectDef::None,
                abilities::EffectDef::WeaponAttack => EngineEffectDef::WeaponAttack,
                abilities::EffectDef::Damage { dice } => EngineEffectDef::Damage { dice: *dice },
                abilities::EffectDef::SpellDamage { dice } => EngineEffectDef::SpellDamage { dice: *dice },
                abilities::EffectDef::Heal { dice } => EngineEffectDef::Heal { dice: *dice },
                abilities::EffectDef::GrantMovement { distance } => EngineEffectDef::GrantMovement { distance: *distance },
                abilities::EffectDef::RestoreResources => EngineEffectDef::RestoreResources,
                abilities::EffectDef::Summon { .. } | abilities::EffectDef::ToggleMoveMode => EngineEffectDef::None,
            },
            statuses: def.statuses.iter().map(|s| EngineStatusApplication {
                status: s.status.clone(),
                duration_rounds: s.duration_rounds,
                on: match s.on {
                    abilities::StatusOn::Target => EngineStatusOn::Target,
                    abilities::StatusOn::MySelf => EngineStatusOn::MySelf,
                },
            }).collect(),
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
            hp_percent_dot: def.hp_percent_dot,
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
