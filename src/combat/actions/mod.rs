//! Action-legality layer. Single source of truth for "can this actor use
//! this ability against this target right now?", shared between
//! `combat::validation` (Bevy gate) and — eventually — the AI planner and
//! UI overlay.
//!
//! Mirrors the design of `combat::effects_state::TargetState`: a minimal
//! trait + owned view types, with backend impls living at the call sites
//! so this module stays ignorant of both Bevy ECS and `BattleSnapshot`.
//!
//! Scope is deliberately narrow:
//!
//! - **In**: ability existence, actor alive+has-ability, AP/resource
//!   affordability, range/in-bounds, target alive for non-AoE, status
//!   flags (`blocks_mana_abilities`, `causes_disadvantage`).
//! - **Out**: "whose turn is it" (caller's responsibility — game state,
//!   not action legality), AI-policy constraints like taunt or overheal
//!   (those live in the AI layer as additional filters on top).

use crate::content::abilities::AoEShape;
use crate::content::content_view::ContentView;
use crate::core::{AbilityId, ResourceKind};
use crate::game::hex::{in_bounds, Hex};
use bevy::prelude::Entity;

// ── Public API ─────────────────────────────────────────────────────────────

/// What the legality check needs to read. Implementors are trivial adapters
/// over Bevy queries (live gate) or `BattleSnapshot` (AI planner).
///
/// Deliberately no borrow-typed return values: the Bevy backend resolves
/// actor data via `Query::get(e)`, which hands back a fetch item whose
/// lifetime ends at the call boundary — so we can't hand a borrowed slice
/// (e.g. the ability list) out to trait consumers. Methods return owned
/// `ActorView` copies or answer direct boolean questions instead.
pub trait ActionState {
    /// Ability / status definitions.
    fn content(&self) -> &ContentView;

    /// Snapshot of the actor's cross-cutting legality inputs. `None` when
    /// the entity is unknown (despawned / not in the snapshot);
    /// `ActorView::is_alive` carries the dead-but-still-present case.
    fn actor_view(&self, actor: Entity) -> Option<ActorView>;

    /// Does the actor know this (non-keyed) ability? Answered directly by
    /// the backend so we don't have to hand out a borrowed ability list.
    fn actor_knows_ability(&self, actor: Entity, ability: &AbilityId) -> bool;

    /// `None` — target entity unknown.
    /// `Some(false)` — known, dead.
    /// `Some(true)` — alive.
    fn is_target_alive(&self, target: Entity) -> Option<bool>;
}

/// Owned snapshot of an actor's cross-cutting legality inputs. `Copy`-like
/// (all primitive fields), built per lookup — a handful of i32/bool reads,
/// no allocation. Keeping it `'static` sidesteps the Bevy-Query-item
/// variance issue around borrowed fields.
#[derive(Clone, Copy, Debug)]
pub struct ActorView {
    pub pos: Hex,
    pub hp: i32,
    pub ap: i32,
    pub mana: Option<i32>,
    pub rage: Option<i32>,
    pub energy: Option<i32>,
    pub causes_disadvantage: bool,
    pub blocks_mana_abilities: bool,
    pub is_alive: bool,
}

impl ActorView {
    /// Pool amount for a resource kind. Mirrors the `validation::check`
    /// lookup: `Hp` reads the vital, cost-less resources return 0 when the
    /// actor doesn't track them (no mana pool ⇒ no mana to spend).
    pub fn resource_amount(&self, kind: ResourceKind) -> i32 {
        match kind {
            ResourceKind::Hp => self.hp,
            ResourceKind::Mana => self.mana.unwrap_or(0),
            ResourceKind::Rage => self.rage.unwrap_or(0),
            ResourceKind::Energy => self.energy.unwrap_or(0),
        }
    }
}

/// A concrete "I want actor X to use ability A on target T at tile P" intent.
/// Shaped to match `UseAbility` so the live gate translates 1:1 and a future
/// AI-side candidate enumerator can share the call shape.
#[derive(Clone, Copy)]
pub struct ProposedAction<'a> {
    pub actor: Entity,
    pub ability: &'a AbilityId,
    pub target: Entity,
    pub target_pos: Hex,
}

/// Outcome of a successful legality check. Disadvantage is a soft flag — the
/// action fires but with roll disadvantage (short-range penalty or a status
/// like "disoriented"); callers propagate it to the resolver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegalAction {
    pub disadvantage: bool,
}

/// Every reason `check_legality` can reject an action. Grouped so UI can
/// render tooltips by category and tests can pin each branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IllegalReason {
    UnknownActor,
    ActorDead,
    UnknownAbility,
    AbilityNotInList,
    NotEnoughAp,
    InsufficientResource(ResourceKind),
    BlockedByStatus,
    OutOfRange,
    TargetOutOfBounds,
    SelfOnlyTargetMismatch,
    TargetUnknown,
    TargetDead,
}

/// Decide whether `action` is legal against `state`. Single-pass, no side
/// effects. Returns `Ok(LegalAction { disadvantage })` or `Err(reason)`.
///
/// **Does not** check whose turn it is — callers that care (the live
/// validator) gate that separately before invoking us. Rationale: "turn
/// ownership" is global game state, not a property of the action.
pub fn check_legality<S: ActionState>(
    action: ProposedAction<'_>,
    state: &S,
) -> Result<LegalAction, IllegalReason> {
    let content = state.content();
    let def = content
        .abilities
        .get(action.ability)
        .ok_or(IllegalReason::UnknownAbility)?;

    let actor = state
        .actor_view(action.actor)
        .ok_or(IllegalReason::UnknownActor)?;
    if !actor.is_alive {
        return Err(IllegalReason::ActorDead);
    }

    // Keyed (universal) abilities bypass the per-actor ability list.
    if def.key.is_none() && !state.actor_knows_ability(action.actor, action.ability) {
        return Err(IllegalReason::AbilityNotInList);
    }

    if actor.ap < def.cost_ap {
        return Err(IllegalReason::NotEnoughAp);
    }
    for cost in &def.costs {
        if actor.resource_amount(cost.resource) < cost.amount {
            return Err(IllegalReason::InsufficientResource(cost.resource));
        }
    }
    // Faith-crit-fail status forbids any mana-cost ability.
    if actor.blocks_mana_abilities
        && def.costs.iter().any(|c| c.resource == ResourceKind::Mana)
    {
        return Err(IllegalReason::BlockedByStatus);
    }

    let mut disadvantage = actor.causes_disadvantage;

    if def.range.max == 0 {
        if action.actor != action.target {
            return Err(IllegalReason::SelfOnlyTargetMismatch);
        }
    } else {
        if !in_bounds(action.target_pos) {
            return Err(IllegalReason::TargetOutOfBounds);
        }
        let dist = actor.pos.unsigned_distance_to(action.target_pos);
        if dist > def.range.max {
            return Err(IllegalReason::OutOfRange);
        }
        if dist < def.range.min {
            disadvantage = true;
        }
    }

    // For non-AoE, the primary target must be alive. AoE can land on empty
    // cells (target entity is a sentinel / irrelevant).
    if !matches!(def.aoe, AoEShape::None) {
        return Ok(LegalAction { disadvantage });
    }
    match state.is_target_alive(action.target) {
        None => Err(IllegalReason::TargetUnknown),
        Some(false) => Err(IllegalReason::TargetDead),
        Some(true) => Ok(LegalAction { disadvantage }),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, EffectDef, ResourceCost, TargetType,
    };
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    /// Minimal `ContentView` carrying just the abilities a row defines.
    fn content_with(abs: Vec<AbilityDef>) -> ContentView {
        let mut map: HashMap<AbilityId, AbilityDef> = HashMap::new();
        for d in abs {
            map.insert(d.id.clone(), d);
        }
        ContentView {
            abilities: map,
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    fn ability(
        id: &str,
        cost_ap: i32,
        range: (u32, u32),
        aoe: AoEShape,
        costs: Vec<ResourceCost>,
    ) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: range.0, max: range.1 },
            effect: EffectDef::WeaponAttack,
            costs,
            cost_ap,
            aoe,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    /// Hand-rolled state for tests. Single actor + single target; overrides
    /// per case flip the fields that matter.
    struct FakeState {
        content: ContentView,
        actor: Entity,
        actor_pos: Hex,
        hp: i32,
        ap: i32,
        mana: Option<i32>,
        abilities: Vec<AbilityId>,
        causes_disadvantage: bool,
        blocks_mana_abilities: bool,
        is_alive: bool,
        target_alive: Option<bool>,
    }

    impl FakeState {
        fn new(content: ContentView, actor: Entity, actor_pos: Hex) -> Self {
            Self {
                content,
                actor,
                actor_pos,
                hp: 10,
                ap: 2,
                mana: None,
                abilities: Vec::new(),
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                is_alive: true,
                target_alive: Some(true),
            }
        }
    }

    impl ActionState for FakeState {
        fn content(&self) -> &ContentView {
            &self.content
        }
        fn actor_view(&self, actor: Entity) -> Option<ActorView> {
            if actor != self.actor {
                return None;
            }
            Some(ActorView {
                pos: self.actor_pos,
                hp: self.hp,
                ap: self.ap,
                mana: self.mana,
                rage: None,
                energy: None,
                causes_disadvantage: self.causes_disadvantage,
                blocks_mana_abilities: self.blocks_mana_abilities,
                is_alive: self.is_alive,
            })
        }
        fn actor_knows_ability(&self, actor: Entity, ability: &AbilityId) -> bool {
            actor == self.actor && self.abilities.iter().any(|a| a == ability)
        }
        fn is_target_alive(&self, _target: Entity) -> Option<bool> {
            self.target_alive
        }
    }

    #[test]
    fn legality_rejections_cover_each_branch() {
        let actor = ent(1);
        let target = ent(2);
        let actor_pos = hex_from_offset(0, 0);
        let in_range = hex_from_offset(1, 0);
        // Far but still in-bounds — range check must fire BEFORE bounds,
        // and our ability's max range is 2.
        let too_far = hex_from_offset(5, 0);

        let strike = ability("strike", 1, (0, 2), AoEShape::None, Vec::new());
        let melee_only = ability("melee_only", 1, (0, 0), AoEShape::None, Vec::new());
        let mana_bolt = ability(
            "mana_bolt", 1, (0, 3), AoEShape::None,
            vec![ResourceCost { resource: ResourceKind::Mana, amount: 5 }],
        );

        // Each row produces a FakeState variant + a ProposedAction, then
        // asserts the expected `Result`. The state mutator runs after
        // default construction so defaults stay compact.
        type Mutate = fn(&mut FakeState);
        struct Row {
            name: &'static str,
            abilities: &'static [&'static str],
            mutate: Mutate,
            ability_id: &'static str,
            target_pos: Hex,
            target: Entity,
            expected: Result<bool, IllegalReason>,
        }

        let noop: Mutate = |_| {};
        let rows: Vec<Row> = vec![
            Row {
                name: "happy path",
                abilities: &["strike"], mutate: noop,
                ability_id: "strike", target_pos: in_range, target,
                expected: Ok(false),
            },
            Row {
                name: "unknown ability id",
                abilities: &["strike"], mutate: noop,
                ability_id: "missing", target_pos: in_range, target,
                expected: Err(IllegalReason::UnknownAbility),
            },
            Row {
                name: "ability not in list, not keyed",
                abilities: &[], mutate: noop,
                ability_id: "strike", target_pos: in_range, target,
                expected: Err(IllegalReason::AbilityNotInList),
            },
            Row {
                name: "actor dead",
                abilities: &["strike"], mutate: |s| s.is_alive = false,
                ability_id: "strike", target_pos: in_range, target,
                expected: Err(IllegalReason::ActorDead),
            },
            Row {
                name: "not enough AP",
                abilities: &["strike"], mutate: |s| s.ap = 0,
                ability_id: "strike", target_pos: in_range, target,
                expected: Err(IllegalReason::NotEnoughAp),
            },
            Row {
                name: "insufficient mana",
                abilities: &["mana_bolt"], mutate: |s| s.mana = Some(0),
                ability_id: "mana_bolt", target_pos: in_range, target,
                expected: Err(IllegalReason::InsufficientResource(ResourceKind::Mana)),
            },
            Row {
                name: "mana blocked by status",
                abilities: &["mana_bolt"],
                mutate: |s| { s.mana = Some(10); s.blocks_mana_abilities = true; },
                ability_id: "mana_bolt", target_pos: in_range, target,
                expected: Err(IllegalReason::BlockedByStatus),
            },
            Row {
                name: "out of range",
                abilities: &["strike"], mutate: noop,
                ability_id: "strike", target_pos: too_far, target,
                expected: Err(IllegalReason::OutOfRange),
            },
            Row {
                name: "self-only ability at non-self target",
                abilities: &["melee_only"], mutate: noop,
                ability_id: "melee_only", target_pos: in_range, target,
                expected: Err(IllegalReason::SelfOnlyTargetMismatch),
            },
            Row {
                name: "dead target on single-target",
                abilities: &["strike"], mutate: |s| s.target_alive = Some(false),
                ability_id: "strike", target_pos: in_range, target,
                expected: Err(IllegalReason::TargetDead),
            },
            Row {
                name: "unknown target",
                abilities: &["strike"], mutate: |s| s.target_alive = None,
                ability_id: "strike", target_pos: in_range, target,
                expected: Err(IllegalReason::TargetUnknown),
            },
            Row {
                name: "status sets disadvantage",
                abilities: &["strike"], mutate: |s| s.causes_disadvantage = true,
                ability_id: "strike", target_pos: in_range, target,
                expected: Ok(true),
            },
        ];

        for row in rows {
            let content = content_with(vec![
                strike.clone(), melee_only.clone(), mana_bolt.clone(),
            ]);
            let mut state = FakeState::new(content, actor, actor_pos);
            state.abilities = row.abilities.iter().map(|s| AbilityId::from(*s)).collect();
            (row.mutate)(&mut state);
            let ability_id = AbilityId::from(row.ability_id);
            let action = ProposedAction {
                actor, ability: &ability_id,
                target: row.target, target_pos: row.target_pos,
            };
            let got = check_legality(action, &state);
            match (row.expected, got) {
                (Ok(want_dis), Ok(LegalAction { disadvantage })) => assert_eq!(
                    disadvantage, want_dis,
                    "[{}] disadvantage flag", row.name,
                ),
                (Err(want), Err(got)) => assert_eq!(want, got, "[{}]", row.name),
                (want, got) => panic!("[{}] expected {:?}, got {:?}", row.name, want, got),
            }
        }
    }

    /// Min-range violation keeps the action legal but flips disadvantage,
    /// matching the live validator's "short-range penalty" branch.
    #[test]
    fn below_min_range_is_legal_with_disadvantage() {
        let actor = ent(1);
        let target = ent(2);
        let actor_pos = hex_from_offset(0, 0);
        let too_close = hex_from_offset(1, 0);

        let longbow = ability("longbow", 1, (3, 6), AoEShape::None, Vec::new());
        let content = content_with(vec![longbow.clone()]);
        let mut state = FakeState::new(content, actor, actor_pos);
        state.abilities = vec![AbilityId::from("longbow")];
        let ab = AbilityId::from("longbow");
        let action = ProposedAction {
            actor, ability: &ab, target, target_pos: too_close,
        };
        let got = check_legality(action, &state).expect("below min range is still legal");
        assert!(got.disadvantage, "short-range penalty must set disadvantage");
    }
}

