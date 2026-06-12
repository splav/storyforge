//! Snapshot-side backend for `combat_engine::legality::ActionState`. Mirrors
//! `combat::validation::BevyActions` but reads out of a `BattleSnapshot` +
//! `ContentView`, for use from AI / replay / sim contexts that have no live
//! ECS world.

use combat_engine::legality::{ActionState, ActorView};
use combat_engine::{AbilityDef, AbilityId, StatusDef, StatusId};

use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::content::content_view::ContentView;
use crate::game::components::Team;
use crate::game::hex::{in_bounds, Hex};
use bevy::prelude::Entity;

/// `ActionState` impl over a `BattleSnapshot`. Holds references only;
/// construction is cheap and the struct is short-lived (per-tick, per-check).
pub struct SnapshotActionState<'a> {
    pub content: &'a ContentView,
    pub snap: &'a BattleSnapshot,
}

impl ActionState for SnapshotActionState<'_> {
    type Id = Entity;

    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> {
        let def = self.content.abilities.get(id)?;
        Some(def.engine.clone())
    }

    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        self.content.statuses.get(id).map(|s| s.engine)
    }

    fn actor_view(&self, actor: Entity) -> Option<ActorView> {
        let u = self.snap.unit(actor)?;
        // Status flags are content-level — walk active statuses and OR the
        // flags off their definitions. Mirrors the Bevy side's fold; both
        // stay O(statuses) per lookup, and the list is tiny in practice.
        let (causes_disadvantage, blocks_mana_abilities) =
            u.statuses().iter().fold((false, false), |(d, m), s| {
                let def = self.content.statuses.get(&s.id);
                (
                    d || def.is_some_and(|x| x.causes_disadvantage),
                    m || def.is_some_and(|x| x.blocks_mana_abilities),
                )
            });
        use combat_engine::{enum_map, PoolKind};
        Some(ActorView {
            pos: u.pos,
            team: u.team,
            hp: u.hp(),
            ap: u.pools[PoolKind::Ap].map(|(c, _)| c).unwrap_or(0),
            pools: enum_map::enum_map! {
                // Hp is not a resource-cost kind for legality checks; excluded.
                PoolKind::Hp     => None,
                PoolKind::Mana   => u.pools[PoolKind::Mana].map(|(c, _)| c),
                PoolKind::Rage   => u.pools[PoolKind::Rage].map(|(c, _)| c),
                PoolKind::Energy => u.pools[PoolKind::Energy].map(|(c, _)| c),
                PoolKind::Ap     => None,
                PoolKind::Mp     => None,
            },
            causes_disadvantage,
            blocks_mana_abilities,
            is_alive: u.hp() > 0,
        })
    }

    fn actor_knows_ability(&self, actor: Entity, ability: &AbilityId) -> bool {
        self.snap
            .unit(actor)
            .is_some_and(|u| u.cache.abilities.iter().any(|a| a == ability))
    }

    fn is_target_alive(&self, target: Entity) -> Option<bool> {
        // Corpses live in the snapshot with `hp = 0`, so the two backends
        // (Bevy + Snapshot) now agree on the `TargetUnknown` vs
        // `TargetDead` distinction: absent ⇒ unknown, present+dead ⇒ dead.
        self.snap.unit(target).map(|u| u.is_alive())
    }

    fn target_team(&self, target: Entity) -> Option<Team> {
        self.snap.unit(target).map(|u| u.team)
    }

    fn taunters_for(&self, actor_team: Team) -> Vec<Entity> {
        // All live enemies whose active statuses include forces_targeting bind
        // opposing-team casts. Walk statuses via content defs — same data path
        // as the engine-side legality check.
        self.snap
            .enemies_of(actor_team)
            .filter_map(|view| {
                let has_taunt = view.statuses().iter().any(|s| {
                    self.content
                        .statuses
                        .get(&s.id)
                        .is_some_and(|sd| sd.engine.forces_targeting)
                });
                if has_taunt {
                    Some(view.entity())
                } else {
                    None
                }
            })
            .collect()
    }

    fn is_in_bounds(&self, pos: Hex) -> bool {
        in_bounds(pos)
    }

    fn blocked_hexes(&self) -> &std::collections::HashSet<Hex> {
        &self.snap.state.blocked_hexes
    }

    fn has_tags(
        &self,
        target: Entity,
        requires: &std::collections::BTreeSet<combat_engine::TagId>,
        excludes: &std::collections::BTreeSet<combat_engine::TagId>,
    ) -> bool {
        // UnitView derefs to &combat_engine::state::Unit, so .tags is directly accessible.
        self.snap
            .unit(target)
            .is_some_and(|v| requires.is_subset(&v.tags) && excludes.is_disjoint(&v.tags))
    }

    fn actor_weapon_channels(
        &self,
        actor: Entity,
    ) -> (
        Option<combat_engine::DiceExpr>,
        Option<combat_engine::DiceExpr>,
    ) {
        self.snap
            .unit(actor)
            .map(|v| {
                (
                    v.cache.caster_ctx.weapon_dice,
                    v.cache.caster_ctx.ranged_dice,
                )
            })
            .unwrap_or((None, None))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::{empty_content, UnitBuilder};
    use crate::combat::ai::world::snapshot::{ActiveStatusView, UnitSnapshot};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, ResourceCost, TargetType,
    };
    use crate::content::statuses::StatusDef;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::legality::{check_legality, IllegalReason, ProposedAction};
    use combat_engine::{DiceExpr, ResourceKind, StatusId};

    fn attack_ability() -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("strike"),
            name: "strike".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 2 },
                effect: EffectDef::WeaponAttack {
                    ranged: false,
                    power: 1.0,
                },
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        }
    }

    fn mana_spell() -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("mana_bolt"),
            name: "mana_bolt".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::SpellDamage {
                    dice: DiceExpr::new(1, 6, 0),
                },
                costs: vec![ResourceCost {
                    resource: ResourceKind::Mana,
                    amount: 5,
                }],
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        }
    }

    fn snapshot_with(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        snapshot_from(units, 1)
    }

    #[test]
    fn legal_cast_in_range_succeeds() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .caster_ctx(CasterContext {
                weapon_dice: Some(DiceExpr::new(1, 6, 0)),
                ..Default::default()
            })
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .ap(2)
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("strike");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: target.entity,
            target_pos: target.pos,
        };
        let outcome = check_legality(proposal, &state).expect("in-range cast");
        assert!(!outcome.disadvantage);
    }

    #[test]
    fn insufficient_mana_rejects() {
        let actor_pos = hex_from_offset(0, 0);
        // Actor has 2 mana, spell needs 5.
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .mana(2, 10)
            .ability_names(&["mana_bolt"])
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let mut content = empty_content();
        let def = mana_spell();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("mana_bolt");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: target.entity,
            target_pos: target.pos,
        };
        assert_eq!(
            check_legality(proposal, &state),
            Err(IllegalReason::InsufficientResource(ResourceKind::Mana)),
        );
    }

    #[test]
    fn blocks_mana_status_rejects_even_with_enough_mana() {
        let actor_pos = hex_from_offset(0, 0);
        let mut actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .mana(10, 10)
            .ability_names(&["mana_bolt"])
            .build();
        actor.statuses.push(ActiveStatusView {
            id: StatusId::from("broken_faith"),
            rounds_remaining: 3,
            dot_per_tick: 0,
        });
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let mut content = empty_content();
        let def = mana_spell();
        content.abilities.insert(def.id.clone(), def);
        // Minimal status def with blocks_mana_abilities = true.
        content.statuses.insert(
            StatusId::from("broken_faith"),
            StatusDef {
                id: StatusId::from("broken_faith"),
                name: "broken_faith".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: combat_engine::StatusDef {
                    forces_targeting: false,
                    bonuses: combat_engine::StatusBonuses::default(),
                    hp_percent_dot: 0,
                    heal_per_tick: 0,
                    skips_turn: false,
                    causes_disadvantage: false,
                    blocks_mana_abilities: true,
                },
            },
        );
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("mana_bolt");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: target.entity,
            target_pos: target.pos,
        };
        assert_eq!(
            check_legality(proposal, &state),
            Err(IllegalReason::BlockedByStatus),
        );
    }

    #[test]
    fn out_of_range_rejects() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .caster_ctx(CasterContext {
                weapon_dice: Some(DiceExpr::new(1, 6, 0)),
                ..Default::default()
            })
            .build();
        // Max range 2, target at distance 5.
        let far = hex_from_offset(5, 0);
        let target = UnitBuilder::new(2, Team::Player, far).build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("strike");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: target.entity,
            target_pos: far,
        };
        assert_eq!(
            check_legality(proposal, &state),
            Err(IllegalReason::OutOfRange),
        );
    }

    #[test]
    fn disorientation_sets_disadvantage_but_stays_legal() {
        let actor_pos = hex_from_offset(0, 0);
        let mut actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .caster_ctx(CasterContext {
                weapon_dice: Some(DiceExpr::new(1, 6, 0)),
                ..Default::default()
            })
            .build();
        actor.statuses.push(ActiveStatusView {
            id: StatusId::from("disoriented"),
            rounds_remaining: 2,
            dot_per_tick: 0,
        });
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        content.statuses.insert(
            StatusId::from("disoriented"),
            StatusDef {
                id: StatusId::from("disoriented"),
                name: "disoriented".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: combat_engine::StatusDef {
                    forces_targeting: false,
                    bonuses: combat_engine::StatusBonuses::default(),
                    hp_percent_dot: 0,
                    heal_per_tick: 0,
                    skips_turn: false,
                    causes_disadvantage: true,
                    blocks_mana_abilities: false,
                },
            },
        );
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("strike");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: target.entity,
            target_pos: target.pos,
        };
        let outcome = check_legality(proposal, &state).expect("still legal with disadvantage");
        assert!(outcome.disadvantage, "disoriented must carry disadvantage");
    }

    /// Target entity absent from the snapshot — unknown to the planner.
    /// Reported as `TargetUnknown` (distinct from `TargetDead`).
    #[test]
    fn missing_target_rejects_as_unknown() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .caster_ctx(CasterContext {
                weapon_dice: Some(DiceExpr::new(1, 6, 0)),
                ..Default::default()
            })
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("strike");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: Entity::from_raw_u32(999).unwrap(),
            target_pos: hex_from_offset(1, 0),
        };
        assert_eq!(
            check_legality(proposal, &state),
            Err(IllegalReason::TargetUnknown),
        );
    }

    /// Dead target (corpse with hp=0, still present in the snapshot) →
    /// `TargetDead`. Backends now agree on the unknown-vs-dead distinction.
    #[test]
    fn dead_target_rejects_as_dead() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .caster_ctx(CasterContext {
                weapon_dice: Some(DiceExpr::new(1, 6, 0)),
                ..Default::default()
            })
            .build();
        let corpse = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(0)
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), corpse.clone()]);
        let state = SnapshotActionState {
            content: &content,
            snap: &snap,
        };

        let ab = AbilityId::from("strike");
        let proposal = ProposedAction {
            actor: actor.entity,
            ability: &ab,
            target: corpse.entity,
            target_pos: corpse.pos,
        };
        assert_eq!(
            check_legality(proposal, &state),
            Err(IllegalReason::TargetDead),
        );
    }
}
