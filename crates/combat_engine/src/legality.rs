//! Single-source-of-truth action-legality checker.
//!
//! The engine owns the rule layer; player UI (`BevyActions`), AI planner
//! (`SnapshotActions`), and engine `step()` pre-validate all share this via
//! backend-specific `ActionState` impls.
//!
//! Scope:
//! - **In**: ability existence, actor alive + knows-ability, AP/resource
//!   affordability, range/in-bounds, target-team match, taunt
//!   (`forces_targeting`), target alive for non-AoE.
//! - **Out**: turn ownership (caller gates that) and AI-only heuristics
//!   (overheal skip / wasted-CC / friendly-fire ratio — in the scoring layer).

use hexx::Hex;

use crate::content::{AoEShape, TargetType};
use crate::state::Team;
use crate::{AbilityDef, AbilityId, ResourceKind, StatusDef, StatusId};

/// Per-actor cross-cutting legality inputs.  Owned `Copy` for borrow-friendliness.
#[derive(Clone, Debug)]
pub struct ActorView {
    pub pos: Hex,
    pub team: Team,
    pub hp: i32,
    pub ap: i32,
    /// Per-pool current amounts (no max needed for legality — only "can afford").
    /// HP is excluded: it lives in the dedicated `hp` field above.
    pub pools: enum_map::EnumMap<crate::PoolKind, Option<i32>>,
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
            ResourceKind::Mana => self.pools[crate::PoolKind::Mana].unwrap_or(0),
            ResourceKind::Rage => self.pools[crate::PoolKind::Rage].unwrap_or(0),
            ResourceKind::Energy => self.pools[crate::PoolKind::Energy].unwrap_or(0),
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
    /// Ranged ability with `requires_los = true` but the line from actor to
    /// target passes through a blocked hex.
    NoLineOfSight,
    /// Ability has `target_type == Environment` — it is passive-only and can
    /// never be actively cast by the player or AI.
    PassiveNotCastable,
    /// The target does not satisfy the ability's `requires_tags` / `excludes_tags`
    /// predicate (`SingleEnemy` and `SingleAlly` only).
    WrongTargetTags,
    /// `WeaponAttack { ranged: true }` but the caster has no `ranged_dice`, or
    /// `WeaponAttack { ranged: false }` but the caster has no `weapon_dice`.
    MissingWeapon,
}

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

    /// Hexes blocked by static obstacles (преграды).  Default: empty set —
    /// for backends without obstacles (mock/stub adapters in tests).
    ///
    /// This is the **single point** of divergence between backends for LOS
    /// purposes; `is_blocked_los` is computed uniformly via `has_los`.
    fn blocked_hexes(&self) -> &std::collections::HashSet<Hex> {
        static EMPTY: std::sync::OnceLock<std::collections::HashSet<Hex>> =
            std::sync::OnceLock::new();
        EMPTY.get_or_init(std::collections::HashSet::new)
    }

    /// Returns `true` if the direct hex-line from `from` to `to` is blocked
    /// by an obstacle (i.e., LOS is obstructed).
    ///
    /// **Do not override** — this is the canonical LOS impl shared by all
    /// backends.  Override `blocked_hexes` instead to expose obstacles.
    /// Parity is structural: same input set → same output (by construction).
    fn is_blocked_los(&self, from: Hex, to: Hex) -> bool {
        let blocked = self.blocked_hexes();
        !crate::geom::has_los(from, to, |h| blocked.contains(&h))
    }

    /// Returns `true` if `target` satisfies the tag predicate:
    /// `requires ⊆ target.tags ∧ excludes ∩ target.tags = ∅`.
    ///
    /// No default implementation — every backend must implement this so the
    /// compiler enforces parity across `EngineCheckState`, `SnapshotActionState`,
    /// and `BevyActions`.
    fn has_tags(
        &self,
        target: Self::Id,
        requires: &std::collections::BTreeSet<crate::TagId>,
        excludes: &std::collections::BTreeSet<crate::TagId>,
    ) -> bool;

    /// Returns `(weapon_dice, ranged_dice)` for the actor's caster context.
    /// Used to validate WeaponAttack dice availability at legality check time.
    /// Default returns `(None, None)` — backends that don't populate weapon dice
    /// (Bevy legality adapter, tests without full CasterContext) always pass.
    fn actor_weapon_channels(
        &self,
        actor: Self::Id,
    ) -> (Option<crate::DiceExpr>, Option<crate::DiceExpr>) {
        let _ = actor;
        (None, None)
    }
}

/// Decide whether `action` is legal against `state`. Single-pass, no side
/// effects. Returns `Ok(LegalAction { disadvantage })` or `Err(reason)`.
/// Does not check turn ownership (see module doc).
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
    if actor.blocks_mana_abilities && def.costs.iter().any(|c| c.resource == ResourceKind::Mana) {
        return Err(IllegalReason::BlockedByStatus);
    }

    let mut disadvantage = actor.causes_disadvantage;

    // WeaponAttack dice availability check.
    // Illegal if the matching dice channel (melee or ranged) is absent.
    if let crate::content::EffectDef::WeaponAttack { ranged, .. } = &def.effect {
        let (weapon_dice, ranged_dice) = state.actor_weapon_channels(action.actor);
        let has_dice = if *ranged {
            ranged_dice.is_some()
        } else {
            weapon_dice.is_some()
        };
        if !has_dice {
            return Err(IllegalReason::MissingWeapon);
        }
    }

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

        // LOS check: ranged abilities with requires_los=true reject actions
        // where any intermediate hex on the line is blocked.
        if def.requires_los
            && def.range.max > 1
            && state.is_blocked_los(actor.pos, action.target_pos)
        {
            return Err(IllegalReason::NoLineOfSight);
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
            if !taunters.is_empty() && !taunters.contains(&action.target) {
                return Err(IllegalReason::TauntForcesTarget);
            }
            if !state.has_tags(action.target, &def.requires_tags, &def.excludes_tags) {
                return Err(IllegalReason::WrongTargetTags);
            }
        }
        TargetType::SingleAlly => {
            match state.target_team(action.target) {
                Some(t) if t == actor.team => {}
                None => return Err(IllegalReason::TargetUnknown),
                _ => return Err(IllegalReason::WrongTargetTeam),
            }
            if !state.has_tags(action.target, &def.requires_tags, &def.excludes_tags) {
                return Err(IllegalReason::WrongTargetTags);
            }
        }
        TargetType::Ground => {
            // Position-based: `target` is a sentinel (typically the actor);
            // team / alive checks are meaningless here.
        }
        TargetType::Environment => {
            // Environment-targeted abilities are passive-only; they must never
            // reach the legality check via active cast.
            return Err(IllegalReason::PassiveNotCastable);
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
