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
use std::collections::HashSet;

use combat_engine::legality::{ActionState, ActorView};
use combat_engine::{AbilityDef, AbilityId, StatusDef, StatusId};

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
    /// Hexes blocked by static obstacles — used for LOS checks on ranged abilities.
    pub blocked_hexes: &'a HashSet<Hex>,
}

impl ActionState for BevyActions<'_, '_, '_> {
    type Id = Entity;

    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> {
        self.content.abilities.get(id).map(Into::into)
    }

    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        self.content.statuses.get(id).map(|s| s.engine)
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
            pools: {
                use combat_engine::{enum_map, PoolKind};
                enum_map::enum_map! {
                    // Hp is not a resource-cost kind for legality checks.
                    PoolKind::Hp     => None,
                    PoolKind::Mana   => a.mana.map(|m| m.current),
                    PoolKind::Rage   => a.rage.map(|r| r.current),
                    PoolKind::Energy => a.energy.map(|e| e.current),
                    PoolKind::Ap     => None,
                    PoolKind::Mp     => None,
                }
            },
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

    fn taunters_for(&self, actor_team: Team) -> Vec<Entity> {
        self.targets
            .iter()
            .filter(|t| {
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
            .collect()
    }

    fn is_in_bounds(&self, pos: Hex) -> bool {
        in_bounds(pos)
    }

    fn blocked_hexes(&self) -> &std::collections::HashSet<Hex> {
        self.blocked_hexes
    }

    fn has_tags(
        &self,
        target: Entity,
        requires: &std::collections::BTreeSet<combat_engine::TagId>,
        excludes: &std::collections::BTreeSet<combat_engine::TagId>,
    ) -> bool {
        let Ok(item) = self.targets.get(target) else {
            return false;
        };
        let empty = std::collections::BTreeSet::new();
        let tags = item.tags.map_or(&empty, |t| &t.0);
        requires.is_subset(tags) && excludes.is_disjoint(tags)
    }
}
