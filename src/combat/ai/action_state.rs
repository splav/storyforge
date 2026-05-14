//! Snapshot-side backend for `combat_engine::legality::ActionState`. Mirrors
//! `combat::validation::BevyActions` but reads out of a `BattleSnapshot` +
//! `ContentView`, for use from AI / replay / sim contexts that have no live
//! ECS world.

use combat_engine::legality::{ActionState, ActorView};
use combat_engine::{
    AbilityDef, AbilityId, AbilityRange, AoEShape, Cost, StatusDef, StatusId, TargetType,
};

use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::world::tags::AiTags;
use crate::content::abilities;
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
        let u = self.snap.unit(actor)?;
        // Status flags are content-level — walk active statuses and OR the
        // flags off their definitions. Mirrors the Bevy side's fold; both
        // stay O(statuses) per lookup, and the list is tiny in practice.
        let (causes_disadvantage, blocks_mana_abilities) = u.statuses.iter().fold(
            (false, false),
            |(d, m), s| {
                let def = self.content.statuses.get(&s.id);
                (
                    d || def.is_some_and(|x| x.causes_disadvantage),
                    m || def.is_some_and(|x| x.blocks_mana_abilities),
                )
            },
        );
        Some(ActorView {
            pos: u.pos,
            team: u.team,
            hp: u.hp,
            ap: u.action_points,
            mana: u.mana.map(|(cur, _)| cur),
            rage: u.rage.map(|(cur, _)| cur),
            energy: u.energy.map(|(cur, _)| cur),
            causes_disadvantage,
            blocks_mana_abilities,
            is_alive: u.hp > 0,
        })
    }

    fn actor_knows_ability(&self, actor: Entity, ability: &AbilityId) -> bool {
        self.snap
            .unit(actor)
            .is_some_and(|u| u.abilities.iter().any(|a| a == ability))
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

    fn taunter_for(&self, actor_team: Team) -> Option<Entity> {
        // Any live enemy with FORCES_TARGETING binds opposing-team casts.
        // `enemies_of` already filters live.
        self.snap
            .enemies_of(actor_team)
            .find(|u| u.tags.contains(AiTags::FORCES_TARGETING))
            .map(|u| u.entity)
    }

    fn is_in_bounds(&self, pos: Hex) -> bool {
        in_bounds(pos)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use combat_engine::legality::{check_legality, IllegalReason, ProposedAction};
    use crate::combat::ai::world::snapshot::{ActiveStatusView, UnitSnapshot};
    use crate::combat::ai::test_helpers::{empty_content, UnitBuilder};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost, TargetType,
    };
    use crate::content::statuses::StatusDef;
    use crate::core::{DiceExpr, ResourceKind, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    fn attack_ability() -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("strike"),
            name: "strike".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 2 },
            effect: EffectDef::WeaponAttack,
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn mana_spell() -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("mana_bolt"),
            name: "mana_bolt".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 6, 0) },
            costs: vec![ResourceCost { resource: ResourceKind::Mana, amount: 5 }],
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    fn snapshot_with(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    #[test]
    fn legal_cast_in_range_succeeds() {
        let actor_pos = hex_from_offset(0, 0);
        let actor = UnitBuilder::new(1, Team::Enemy, actor_pos)
            .ap(2)
            .ability_names(&["strike"])
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .ap(2)
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
        let state = SnapshotActionState { content: &content, snap: &snap };

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
                forces_targeting: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: false,
                causes_disadvantage: false,
                blocks_mana_abilities: true,
                buff_class: None,
            },
        );
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
            .build();
        // Max range 2, target at distance 5.
        let far = hex_from_offset(5, 0);
        let target = UnitBuilder::new(2, Team::Player, far).build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
                forces_targeting: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: false,
                causes_disadvantage: true,
                blocks_mana_abilities: false,
                buff_class: None,
            },
        );
        let snap = snapshot_with(vec![actor.clone(), target.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
            .build();
        let corpse = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .hp(0)
            .build();
        let mut content = empty_content();
        let def = attack_ability();
        content.abilities.insert(def.id.clone(), def);
        let snap = snapshot_with(vec![actor.clone(), corpse.clone()]);
        let state = SnapshotActionState { content: &content, snap: &snap };

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
