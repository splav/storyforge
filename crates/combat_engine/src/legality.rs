//! Single-source-of-truth action-legality checker.
//!
//! Migrated from `src/combat/actions/mod.rs` in Phase 2 step 2c.  The engine
//! owns the rule layer; player UI (Bevy `BevyActions` adapter), AI planner
//! (`SnapshotActions` adapter), and engine `step()` pre-validate all share
//! this function via backend-specific `ActionState` impls.
//!
//! Scope:
//! - **In**: ability existence, actor alive + knows-ability, AP / resource
//!   affordability, range / in-bounds, target-team match
//!   (`SingleEnemy` / `SingleAlly` / `Myself`), taunt (`forces_targeting`
//!   status), target alive for non-AoE.
//! - **Out**: turn ownership (global game state; caller gates that), AI-only
//!   heuristics (overheal skip / wasted-CC / AoE friendly-fire ratio — those
//!   live in the AI scoring layer).

use hexx::Hex;

use crate::content::{AoEShape, TargetType};
use crate::state::Team;
use crate::{AbilityDef, AbilityId, ResourceKind, StatusDef, StatusId};

/// Per-actor cross-cutting legality inputs.  Owned `Copy` for borrow-friendliness.
#[derive(Clone, Copy, Debug)]
pub struct ActorView {
    pub pos: Hex,
    pub team: Team,
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
    /// Pool amount for a resource kind.  Mirrors `validation::check`:
    /// `Hp` reads the vital; cost-less resources return 0 when the actor
    /// doesn't track them (no mana pool ⇒ no mana to spend).
    pub fn resource_amount(&self, kind: ResourceKind) -> i32 {
        match kind {
            ResourceKind::Hp => self.hp,
            ResourceKind::Mana => self.mana.unwrap_or(0),
            ResourceKind::Rage => self.rage.unwrap_or(0),
            ResourceKind::Energy => self.energy.unwrap_or(0),
        }
    }
}

/// Proposed action — "actor X uses ability A on target T at tile P".
///
/// Generic over `Id` so each backend supplies its native identifier type:
/// Bevy uses `Entity`; AI snapshot uses `Entity`; engine uses `UnitId`.
#[derive(Clone, Copy)]
pub struct ProposedAction<'a, Id> {
    pub actor: Id,
    pub ability: &'a AbilityId,
    pub target: Id,
    pub target_pos: Hex,
}

/// Outcome of a successful legality check.  `disadvantage` is a soft flag —
/// the action fires but with roll disadvantage (short-range penalty or a
/// status like "disoriented"); callers propagate it to the resolver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegalAction {
    pub disadvantage: bool,
}

/// Every reason `check_legality` can reject an action.  Grouped so UI can
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
    /// `target_type == Myself` but `target != actor`.
    SelfOnlyTargetMismatch,
    /// `SingleEnemy` cast at an own-team unit, or `SingleAlly` cast at an
    /// opposing-team one.
    WrongTargetTeam,
    /// `SingleEnemy` cast while a `forces_targeting` enemy is alive and the
    /// target is someone else.
    TauntForcesTarget,
    TargetUnknown,
    TargetDead,
    /// `EndTurn` issued by an actor who is not the current queue cursor.
    NotCurrent,
}

/// Backend adapter — implementors translate game-state reads for the
/// engine-side legality function.  Each backend lives at its callsite:
///
/// - `src/combat/legality_adapter.rs::BevyActions` (live ECS, player UI).
/// - `src/combat/ai/plan/generator.rs::SnapshotActions` (AI plan generation).
/// - `crates/combat_engine/src/step.rs` (engine `step(Cast)` pre-validate).
pub trait ActionState {
    /// Identifier type for actors and targets.  `Entity` for Bevy backends,
    /// `UnitId` for the engine impl.
    type Id: Copy + Eq;

    /// Content-layer lookup for ability definitions.  Backends typically
    /// delegate to a held `combat_engine::ContentView` reference.
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef>;

    /// Content-layer lookup for status definitions.
    fn status_def(&self, id: &StatusId) -> Option<StatusDef>;

    /// Snapshot of the actor's cross-cutting legality inputs.
    fn actor_view(&self, actor: Self::Id) -> Option<ActorView>;

    /// Does the actor know this (non-keyed) ability?  Answered directly so
    /// backends don't have to hand out a borrowed ability list.
    fn actor_knows_ability(&self, actor: Self::Id, ability: &AbilityId) -> bool;

    /// `None` — target unknown; `Some(false)` — known dead; `Some(true)` — alive.
    fn is_target_alive(&self, target: Self::Id) -> Option<bool>;

    /// Target's team, or `None` if unknown.  Backs the `SingleEnemy` /
    /// `SingleAlly` target-type rules.
    fn target_team(&self, target: Self::Id) -> Option<Team>;

    /// All live enemy units whose `forces_targeting` status binds `actor_team`'s
    /// SingleEnemy casts.  Empty when no taunter is active.  When multiple
    /// taunters are present (e.g. two enemies both taunted), the actor may
    /// target any one of them.
    fn taunters_for(&self, actor_team: Team) -> Vec<Self::Id>;

    /// Grid-bounds predicate.  Engine is grid-topology-agnostic; backends
    /// supply the appropriate check (e.g. even-r hex bounds for `storyforge`).
    fn is_in_bounds(&self, pos: Hex) -> bool;
}

/// Decide whether `action` is legal against `state`.  Single-pass, no side
/// effects.  Returns `Ok(LegalAction { disadvantage })` or `Err(reason)`.
///
/// **Does not** check whose turn it is — callers that care gate that
/// separately.  Rationale: "turn ownership" is global game state, not a
/// property of the action.
pub fn check_legality<S: ActionState>(
    action: ProposedAction<'_, S::Id>,
    state: &S,
) -> Result<LegalAction, IllegalReason> {
    let def = state
        .ability_def(action.ability)
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

    // Range / in-bounds.  `range.max == 0` means the ability fires in place —
    // `target_pos` is irrelevant (Myself / Ground dispatch below handles it).
    if def.range.max > 0 {
        if !state.is_in_bounds(action.target_pos) {
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

    // Target-type semantics.  One place enforces the enemy/ally/self split
    // for both backends.
    match def.target_type {
        TargetType::Myself => {
            if action.actor != action.target {
                return Err(IllegalReason::SelfOnlyTargetMismatch);
            }
        }
        TargetType::SingleEnemy => {
            match state.target_team(action.target) {
                Some(t) if t != actor.team => {}
                None => return Err(IllegalReason::TargetUnknown),
                _ => return Err(IllegalReason::WrongTargetTeam),
            }
                        // Taunt: any live enemy with `forces_targeting` binds every
            // SingleEnemy cast to one of the active taunters.  Multiple
            // taunters are allowed; the actor may choose any of them.
            let taunters = state.taunters_for(actor.team);
            if !taunters.is_empty() && !taunters.iter().any(|t| *t == action.target) {
                return Err(IllegalReason::TauntForcesTarget);
            }
        }
        TargetType::SingleAlly => {
            match state.target_team(action.target) {
                Some(t) if t == actor.team => {}
                None => return Err(IllegalReason::TargetUnknown),
                _ => return Err(IllegalReason::WrongTargetTeam),
            }
        }
        TargetType::Ground => {
            // Position-based: `target` is a sentinel (typically the actor);
            // team / alive checks are meaningless here.
        }
    }

    // For non-AoE, the primary target must be alive.  AoE can land on empty
    // cells (target entity is a sentinel / irrelevant).  Ground bypasses the
    // alive check — there's no entity target to validate.
    if !matches!(def.aoe, AoEShape::None) || matches!(def.target_type, TargetType::Ground) {
        return Ok(LegalAction { disadvantage });
    }
    match state.is_target_alive(action.target) {
        None => Err(IllegalReason::TargetUnknown),
        Some(false) => Err(IllegalReason::TargetDead),
        Some(true) => Ok(LegalAction { disadvantage }),
    }
}
