//! `step()` — the public engine entry point.
//!
//! Validates an action, expands it into an effect queue, then pumps effects
//! one at a time while scanning for reactions (AoOs) after each `MovePosition`
//! effect.
//!
//! ## Strict failure (decision 6.5)
//! If any `Damage` effect targets a unit that is already dead and that unit is
//! **not** the current action's actor, `step()` returns `Err(TargetGone)` and
//! rolls back state to entry. If the dead target *is* the actor (i.e. the mover
//! was killed by an earlier reaction), the effect is silently skipped — see the
//! actor-liveness truncation below.
//!
//! This branch is currently only reachable for Phase 2+ Cast/AoE actions where
//! one target in an AoE burst dies mid-burst and a follow-up effect targets
//! a different (also now dead) unit. For `Action::Move` the only Damage targets
//! are AoO victims (= the mover = the actor), so the non-actor branch cannot
//! trigger during Phase 0/1.
//!
//! ## Actor-liveness truncation
//! After each `MovePosition` effect is applied, reactions are processed one by
//! one via per-reaction sub-queues. Before expanding each reaction the mover's
//! liveness is checked: if the mover died from the previous reaction, the
//! remaining reactions for this step are skipped. No `ReactionFired` event is
//! emitted for skipped reactions and `reactions_left` on those enemies is not
//! decremented.
//!
//! Subsequent `MovePosition` effects for the same path are also skipped (the
//! dead-actor guard at the top of the main pump loop handles this).
//!
//! ## Reaction depth cap
//! A counter tracks how many reaction expansions have fired. Exceeding 100
//! returns `Err(ReactionDepthExceeded)` (state rolled back).

use std::collections::{HashMap, VecDeque};

use crate::{
    action::{Action, ActionError},
    content::{AbilityDef, CasterContext, ContentView, EffectDef, StatusDef},
    dice::{DiceExpr, DiceSource},
    effect::{apply_effect, ApplyCtx, Effect},
    event::{effect_to_event, Event},
    legality::{check_legality, ActionState, ActorView, ProposedAction},
    reaction::{expand_reaction, scan_reactions, Reaction, ReactionKind},
    state::{CombatState, Team},
    AbilityId, StatusId,
};

const REACTION_DEPTH_LIMIT: usize = 100;

/// Returns `true` for effects that can change aura membership (positions, tags,
/// presence) and therefore require a before/after membership diff to emit
/// `AuraStatusGained`/`AuraStatusLost`.
///
/// Note: `Spawn` is intentionally excluded — adding it would shift existing
/// summon traces; that is deferred to a later slice.
fn effect_changes_aura_membership(e: &crate::effect::Effect) -> bool {
    matches!(
        e,
        crate::effect::Effect::MovePosition { .. }
            | crate::effect::Effect::Death { .. }
            | crate::effect::Effect::EnterPhase { .. }
    )
}

// ── EngineCheckState ──────────────────────────────────────────────────────────

/// `ActionState` adapter for engine-side legality checks during `step()`.
///
/// Bundles the engine's authoritative state (`CombatState`) with a
/// `ContentView` reference so `check_legality` can answer questions about
/// abilities, statuses, and units uniformly.
///
/// Exposed as `pub` so that integration tests can construct the engine-side
/// `ActionState` directly and compare it against the Bevy `BevyActions`
/// adapter for legality-parity assertions.
pub struct EngineCheckState<'a> {
    pub state: &'a CombatState,
    pub content: &'a dyn ContentView,
}

impl<'a> ActionState for EngineCheckState<'a> {
    type Id = crate::state::UnitId;

    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef> {
        self.content.ability_def(id).cloned()
    }

    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        self.content.status_def(id).copied()
    }

    fn actor_view(&self, actor: crate::state::UnitId) -> Option<ActorView> {
        let u = self.state.unit(actor)?;
        // Fold status flags from content lookups.
        let (causes_disadvantage, blocks_mana_abilities) =
            u.statuses.iter().fold((false, false), |(d, m), s| {
                let def = self.content.status_def(&s.id);
                (
                    d || def.is_some_and(|x| x.causes_disadvantage),
                    m || def.is_some_and(|x| x.blocks_mana_abilities),
                )
            });
        use crate::PoolKind;
        let mana_cur = u.pools[PoolKind::Mana].map(|(c, _)| c);
        let rage_cur = u.pools[PoolKind::Rage].map(|(c, _)| c);
        let energy_cur = u.pools[PoolKind::Energy].map(|(c, _)| c);
        let pools = enum_map::enum_map! {
            // Hp is not a resource-cost kind for legality checks; excluded.
            PoolKind::Hp     => None,
            PoolKind::Mana   => mana_cur,
            PoolKind::Rage   => rage_cur,
            PoolKind::Energy => energy_cur,
            // Ap/Mp are not resource-cost kinds; excluded from legality pools.
            PoolKind::Ap     => None,
            PoolKind::Mp     => None,
        };
        let ap_cur = u.pools[PoolKind::Ap].map(|(c, _)| c).unwrap_or(0);
        Some(ActorView {
            pos: u.pos,
            team: u.team,
            hp: u.hp(),
            ap: ap_cur,
            pools,
            causes_disadvantage,
            blocks_mana_abilities,
            is_alive: u.is_alive(),
        })
    }

    fn actor_knows_ability(&self, _actor: crate::state::UnitId, _ability: &AbilityId) -> bool {
        // Phase 2 step 6b limitation: engine doesn't yet track per-unit
        // ability lists.  Bevy + snapshot backends still enforce this via
        // their own `actor_knows_ability` impls before step() runs (player
        // path: `validate_action_system`; AI path: `generate_plans` already
        // pre-screens via `check_legality` against `SnapshotActionState`).
        // Returning `true` here means the engine pre-validate cannot
        // produce `IllegalReason::AbilityNotInList` — that branch fires
        // only at the Bevy / sim boundary today.  Adding ability lists to
        // engine `Unit` is deferred until per-unit ability tracking
        // becomes engine-authoritative.
        true
    }

    fn is_target_alive(&self, target: crate::state::UnitId) -> Option<bool> {
        self.state.unit(target).map(|u| u.is_alive())
    }

    fn target_team(&self, target: crate::state::UnitId) -> Option<Team> {
        self.state.unit(target).map(|u| u.team)
    }

    fn taunters_for(&self, actor_team: Team) -> Vec<crate::state::UnitId> {
        self.state
            .units()
            .iter()
            .filter(|u| {
                u.is_alive()
                    && u.team != actor_team
                    && u.statuses.iter().any(|s| {
                        self.content
                            .status_def(&s.id)
                            .map(|d| d.forces_targeting)
                            .unwrap_or(false)
                    })
            })
            .map(|u| u.id)
            .collect()
    }

    fn is_in_bounds(&self, _pos: hexx::Hex) -> bool {
        // Phase 2 step 6b limitation: engine is grid-topology-agnostic.
        // Bevy backend (`BevyActions::is_in_bounds`) calls
        // `crate::game::hex::in_bounds`; engine assumes all hexes are
        // in-bounds.  `IllegalReason::TargetOutOfBounds` cannot fire from
        // engine pre-validate — Bevy gate catches it client-side.
        true
    }

    fn blocked_hexes(&self) -> &std::collections::HashSet<hexx::Hex> {
        &self.state.blocked_hexes
    }

    fn has_tags(
        &self,
        target: crate::state::UnitId,
        requires: &std::collections::BTreeSet<crate::TagId>,
        excludes: &std::collections::BTreeSet<crate::TagId>,
    ) -> bool {
        self.state
            .unit(target)
            .is_some_and(|u| requires.is_subset(&u.tags) && excludes.is_disjoint(&u.tags))
    }
}

// ── EngineTargetState ─────────────────────────────────────────────────────────

/// `TargetState` adapter over `&CombatState` for engine-side target enumeration.
///
/// `unit_at_cell` is O(N) per call.  Acceptable for Phase 2 step 6 given
/// typical unit counts (≤ 20).  Future micro-opt: index by hex in `CombatState`.
struct EngineTargetState<'a> {
    state: &'a CombatState,
}

impl<'a> crate::targeting::TargetState for EngineTargetState<'a> {
    type Id = crate::state::UnitId;

    fn actor_pos(&self, actor: crate::state::UnitId) -> Option<hexx::Hex> {
        self.state.unit(actor).map(|u| u.pos)
    }

    fn unit_at_cell(
        &self,
        pos: hexx::Hex,
    ) -> Option<crate::targeting::TargetRef<crate::state::UnitId>> {
        self.state
            .units()
            .iter()
            .find(|u| u.pos == pos)
            .map(|u| crate::targeting::TargetRef {
                id: u.id,
                team: u.team,
                alive: u.is_alive(),
            })
    }

    fn team_of(&self, id: crate::state::UnitId) -> Option<crate::state::Team> {
        self.state.unit(id).map(|u| u.team)
    }
}

// ── damage_effect_for ─────────────────────────────────────────────────────────

/// Builds a `Damage` Effect for one target given the ability's `EffectDef`.
///
/// Returns `None` for non-damage variants (`None`, `Heal`, `GrantMovement`,
/// `RestoreResources`) or `WeaponAttack` without `weapon_dice`.
fn effect_for_target(
    effect: &EffectDef,
    source: crate::state::EffectSource,
    target: crate::state::UnitId,
    caster: &CasterContext,
    rng: &mut dyn DiceSource,
    disadvantage: bool,
) -> Option<Effect> {
    macro_rules! roll {
        ($dice:expr) => {
            if disadvantage {
                rng.roll_disadvantage($dice)
            } else {
                rng.roll($dice)
            }
        };
    }
    match effect {
        EffectDef::Damage { dice } => {
            let raw = (roll!(*dice) + caster.str_mod) as f32;
            Some(Effect::Damage {
                target,
                raw,
                source,
                pierces: false,
            })
        }
        EffectDef::SpellDamage { dice } => {
            let raw = (roll!(*dice) + caster.int_mod + caster.spell_power) as f32;
            Some(Effect::Damage {
                target,
                raw,
                source,
                pierces: true,
            })
        }
        EffectDef::WeaponAttack => {
            let dice = caster.weapon_dice?;
            let raw = (roll!(dice) + caster.str_mod) as f32;
            Some(Effect::Damage {
                target,
                raw,
                source,
                pierces: false,
            })
        }
        EffectDef::Heal { dice } => {
            let amount = (roll!(*dice) + caster.int_mod + caster.spell_power).max(0);
            Some(Effect::Heal { target, amount })
        }
        // Per-actor effects — not dispatched through this fn.
        // None: status-only ability (statuses handled below).
        // RevealEnvInRange: passive-only, never cast actively.
        EffectDef::None
        | EffectDef::GrantMovement { .. }
        | EffectDef::RestoreResources
        | EffectDef::Summon { .. }
        | EffectDef::RevealEnvInRange { .. } => None,
    }
}

/// Returns `true` for events that are the expected, predictable consequence of
/// a unit's own movement.  Any non-benign event during a move step interrupts
/// the remaining movement.
///
/// **Exhaustive match — no wildcard.**  NEW `Event` variants that can occur
/// during a Move must be explicitly classified here (default: non-benign /
/// halts movement).
///
/// Note: `UnitMoved` for a different actor (shouldn't occur during a Move
/// action) is non-benign by this rule — that's intentional.
fn is_benign_move_event(ev: &crate::event::Event, mover: crate::state::UnitId) -> bool {
    use crate::event::Event;
    match ev {
        // Own movement is always benign.
        Event::UnitMoved { actor, .. } if *actor == mover => true,
        // Aura membership changes driven purely by movement are benign.
        Event::AuraStatusGained { .. } => true,
        Event::AuraStatusLost { .. } => true,

        // Every other variant is non-benign and halts remaining movement.
        Event::UnitMoved { .. } => false,
        Event::ActionStarted { .. } => false,
        Event::UnitDamaged { .. } => false,
        Event::UnitHealed { .. } => false,
        Event::StatusApplied { .. } => false,
        Event::StatusRemoved { .. } => false,
        Event::StatusTicked { .. } => false,
        Event::DotDamaged { .. } => false,
        Event::HotHealed { .. } => false,
        Event::ReactionFired { .. } => false,
        Event::UnitDied { .. } => false,
        Event::CritFailed { .. } => false,
        Event::ActionFinished { .. } => false,
        Event::UnitSpawned { .. } => false,
        Event::SpawnBlocked { .. } => false,
        Event::TurnEnded { .. } => false,
        Event::TurnStarted { .. } => false,
        Event::TurnSkipped { .. } => false,
        Event::RoundStarted { .. } => false,
        Event::PhaseEntered { .. } => false,
        Event::HazardTriggered { .. } => false,
        Event::EnvRevealed { .. } => false,
        Event::PoolChanged { .. } => false,
        Event::InitiativeRolled { .. } => false,
    }
}

/// Advance `state` by one action.
///
/// Returns the ordered list of events that occurred and a side-channel
/// [`ApplyCtx`] carrying the per-step RNG-call delta (`ctx.rng_calls`), or
/// an error if the action was illegal or a strict-failure condition was hit
/// (see `ActionError`).
///
/// `ctx.rng_calls` is the number of [`DiceSource::roll`] invocations consumed
/// by this step — used as a trace canary (Phase 5 D2). The count spans the
/// entire effect cascade including AoO sub-queues.
///
/// State is rolled back (no mutation) on any error.
pub fn step(
    state: &mut CombatState,
    action: Action,
    rng: &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<(Vec<Event>, ApplyCtx), ActionError> {
    // Clone state at entry for rollback on error (decision 6.5).
    let snapshot = state.clone();

    let before = rng.call_count();
    let result = step_inner(state, action, rng, content);
    let after = rng.call_count();

    match result {
        Ok((events, interrupted)) => {
            let ctx = ApplyCtx {
                rng_calls: after - before,
                interrupted,
                ..Default::default()
            };
            Ok((events, ctx))
        }
        Err(e) => {
            *state = snapshot;
            Err(e)
        }
    }
}

fn step_inner(
    state: &mut CombatState,
    action: Action,
    rng: &mut dyn DiceSource,
    content: &dyn ContentView,
) -> Result<(Vec<Event>, bool), ActionError> {
    let mut events: Vec<Event> = Vec::new();
    let mut effect_queue: VecDeque<Effect> = VecDeque::new();
    let mut reaction_depth: usize = 0;
    // Guard against all-stunned / all-dead infinite loops in the AdvanceTurn
    // recursion (e.g. all remaining actors stunned → wrap → BumpRound →
    // skip all again → wrap → …). Budget is generous to allow full-queue
    // traversal + one round boundary.
    let mut turn_advance_budget: usize = state.turn_queue.order.len() * 3 + 8;

    // Capture (current actor, round) before any effects are applied.
    // After the pump loop we compare against the final values to detect turn
    // advances — including cases beyond Action::EndTurn such as the current
    // actor dying mid-Move from an AoO.
    // We compare (current, round) rather than just current so that a round-wrap
    // scenario where the same actor acts again in round N+1 is still detected
    // as a turn change (initial.round != final.round).
    let initial_current = (state.turn_queue.current(), state.round);

    // ── Pre-validate ──────────────────────────────────────────────────────────

    match &action {
        Action::Move { actor, path } => {
            let unit = state.unit(*actor).ok_or(ActionError::UnknownActor)?;
            if !unit.is_alive() {
                return Err(ActionError::UnknownActor);
            }
            if path.is_empty() {
                return Err(ActionError::NoPath);
            }
            let mp_cur = unit.pools[crate::PoolKind::Mp].map(|(c, _)| c).unwrap_or(0);
            if path.len() as i32 > mp_cur {
                return Err(ActionError::OutOfMP);
            }

            // Build occupancy map: alive non-actor units keyed by position.
            let actor_team = unit.team;
            let occupancy: HashMap<hexx::Hex, (crate::state::UnitId, crate::state::Team)> = state
                .units()
                .iter()
                .filter(|u| u.is_alive() && u.id != *actor)
                .map(|u| (u.pos, (u.id, u.team)))
                .collect();

            let last = path.len() - 1;
            for (i, &hex) in path.iter().enumerate() {
                if i == last {
                    if occupancy.contains_key(&hex) {
                        return Err(ActionError::DestinationOccupied { hex });
                    }
                } else if let Some(&(_, team)) = occupancy.get(&hex) {
                    if team != actor_team {
                        return Err(ActionError::PathBlockedByEnemy { hex });
                    }
                }
            }
        }
        Action::EndTurn { actor } => {
            state.unit(*actor).ok_or(ActionError::UnknownActor)?;
            // Turn ownership check: a dead actor may still issue EndTurn
            // (mid-action death), but only the current queue cursor may do so.
            if state.turn_queue.current() != Some(*actor) {
                return Err(ActionError::Illegal(
                    crate::legality::IllegalReason::NotCurrent,
                ));
            }
        }
        Action::Cast {
            actor,
            ability,
            target,
            target_pos,
        } => {
            // Engine-side legality check.  Translates IllegalReason to
            // ActionError::Illegal so callers (bridge, sim) see the same
            // rejection vocabulary as Bevy `validate_action_system`.
            let check = EngineCheckState { state, content };
            let proposal = ProposedAction {
                actor: *actor,
                ability,
                target: *target,
                target_pos: *target_pos,
            };
            match check_legality(proposal, &check) {
                Ok(_legal) => {
                    // disadvantage is captured below in the expand arm; nothing
                    // to do in the pre-validate arm.
                }
                Err(reason) => return Err(ActionError::Illegal(reason)),
            }
        }
    }

    // ── Emit ActionStarted event ──────────────────────────────────────────────

    events.push(Event::ActionStarted {
        action: action.clone(),
    });

    // ── Expand action into initial effect queue ───────────────────────────────

    match &action {
        Action::Move { actor, path } => {
            // Per-step MP: interleave DecrementMP{by:1} before each MovePosition
            // so that a halted move consumes only the MP for completed steps.
            for &hex in path {
                effect_queue.push_back(Effect::DecrementMP {
                    actor: *actor,
                    by: 1,
                });
                effect_queue.push_back(Effect::MovePosition {
                    actor: *actor,
                    to: hex,
                });
            }
        }
        Action::EndTurn { actor } => {
            // TurnEnded fires before the AdvanceTurn cascade so the stream
            // reads: outgoing ends → queue advances → skips/round → next starts.
            events.push(Event::TurnEnded {
                actor: *actor,
                cause: crate::event::TurnEndCause::Manual,
            });
            effect_queue.push_back(Effect::AdvanceTurn);
        }
        Action::Cast {
            actor,
            ability,
            target,
            target_pos,
        } => {
            // Legality pre-validate (step 6b) already ran and confirmed the
            // actor can afford every cost.  We rebuild AbilityDef here from
            // ContentView; cheap and avoids carrying the def around.
            let def = content.ability_def(ability).expect(
                "cast: ability_def returns Some — already verified by legality pre-validate",
            );
            let caster = state
                .unit(*actor)
                .map(|u| u.caster_context.clone())
                .unwrap_or_default();

            // Re-run check_legality to capture the disadvantage flag.  The
            // pre-validate arm above already confirmed Ok, so this cannot fail;
            // unwrap is safe.  Duplicate legality run is cheap (pure read).
            let legal = {
                let check = EngineCheckState { state, content };
                let proposal = ProposedAction {
                    actor: *actor,
                    ability,
                    target: *target,
                    target_pos: *target_pos,
                };
                check_legality(proposal, &check)
                    .expect("legality already confirmed in pre-validate arm")
            };

            // Step 6f: crit-fail roll.  Engine hard-codes d20 (matches Bevy
            // settings.crit_fail_die default).  On a 1, branch based on
            // caster's CritFailOutcome; skip normal damage/heal/status fanout.
            let crit_fail = rng.roll(DiceExpr::new(1, 20, 0)) == 1;

            // Emit CritFailed before cost payment so the event stream reads:
            // ActionStarted → CritFailed → [cost events] → ActionFinished.
            if crit_fail {
                events.push(Event::CritFailed {
                    actor: *actor,
                    outcome: caster.crit_fail_outcome.clone(),
                });
            }

            // Cost multiplier: DoubleCost crit-fail doubles resource costs.
            let cost_mult = if crit_fail
                && matches!(
                    caster.crit_fail_outcome,
                    crate::content::CritFailOutcome::DoubleCost
                ) {
                2
            } else {
                1
            };

            // Step 6c: cost payment (possibly doubled on DoubleCost crit-fail).
            if def.cost_ap > 0 {
                effect_queue.push_back(Effect::DecrementAP {
                    actor: *actor,
                    by: def.cost_ap,
                });
            }
            for cost in &def.costs {
                if cost.amount > 0 {
                    effect_queue.push_back(Effect::PayCost {
                        actor: *actor,
                        kind: cost.resource,
                        amount: cost.amount * cost_mult,
                    });
                }
            }

            if crit_fail {
                // Step 6f: crit-fail branch — skip normal damage/heal/status.
                match &caster.crit_fail_outcome {
                    crate::content::CritFailOutcome::Miss
                    | crate::content::CritFailOutcome::DoubleCost => {
                        // No further effects.
                    }
                    crate::content::CritFailOutcome::SelfDamage(dice) => {
                        let raw = rng.roll(*dice) as f32;
                        effect_queue.push_back(Effect::Damage {
                            target: *actor,
                            raw,
                            source: crate::state::EffectSource::Unit(*actor),
                            pierces: false,
                        });
                    }
                    crate::content::CritFailOutcome::ApplyStatus(status_id) => {
                        effect_queue.push_back(Effect::ApplyStatus {
                            target: *actor,
                            status: status_id.clone(),
                            rounds: 3, // Phase 2 step 6f: fixed 3-round duration.
                            dot_per_tick: 0,
                            applier: crate::state::EffectSource::Unit(*actor),
                        });
                    }
                }
            } else {
                // Summon is per-actor; everything else is per-target fanout.
                let mut affected: Vec<crate::state::UnitId> = Vec::new();

                if let EffectDef::Summon {
                    template_id,
                    max_active,
                } = &def.effect
                {
                    effect_queue.push_back(Effect::Spawn {
                        summoner: *actor,
                        template_id: template_id.clone(),
                        max_active: *max_active,
                    });
                    // `affected` intentionally empty — status loop below applies only MySelf statuses.
                } else {
                    // Step 6d: target enumeration + damage/heal fanout.
                    let target_state = EngineTargetState { state };
                    affected = crate::targeting::compute_affected_targets(
                        *actor,
                        def,
                        *target,
                        *target_pos,
                        &target_state,
                    );

                    // Step 6d/6e: per-target effect fanout (damage or heal).
                    for &affected_id in &affected {
                        if let Some(eff) = effect_for_target(
                            &def.effect,
                            crate::state::EffectSource::Unit(*actor),
                            affected_id,
                            &caster,
                            rng,
                            legal.disadvantage,
                        ) {
                            effect_queue.push_back(eff);
                        }
                    }
                }

                // Step 6e: status fanout.
                //
                // StatusOn::Target → applied to every affected unit.
                // StatusOn::MySelf → applied to the actor only.
                //
                // Applied after damage/heal so RefreshAggregates from ApplyStatus
                // sees the post-damage state.
                //
                // Phase 2 limitation: dot_per_tick = 0.  Phase 3 owns DoT roll.
                for status_app in &def.statuses {
                    match status_app.on {
                        crate::content::StatusOn::Target => {
                            for &affected_id in &affected {
                                effect_queue.push_back(Effect::ApplyStatus {
                                    target: affected_id,
                                    status: status_app.status.clone(),
                                    rounds: status_app.duration_rounds,
                                    dot_per_tick: 0,
                                    applier: crate::state::EffectSource::Unit(*actor),
                                });
                            }
                        }
                        crate::content::StatusOn::MySelf => {
                            effect_queue.push_back(Effect::ApplyStatus {
                                target: *actor,
                                status: status_app.status.clone(),
                                rounds: status_app.duration_rounds,
                                dot_per_tick: 0,
                                applier: crate::state::EffectSource::Unit(*actor),
                            });
                        }
                    }
                }
            }
        }
    }

    // ── Pump loop ─────────────────────────────────────────────────────────────
    //
    // We track the actor's "previous position" so that each MovePosition step
    // can tell the AoO scanner where the mover came from.

    let actor_id = match &action {
        Action::Move { actor, .. } | Action::Cast { actor, .. } | Action::EndTurn { actor } => {
            *actor
        }
    };
    // prev_pos starts as the actor's position before any effects are applied.
    let mut prev_pos = state.unit(actor_id).map(|u| u.pos).unwrap_or_default();

    // ── Move-interrupt tracking ───────────────────────────────────────────────
    // `halt` becomes true when a non-benign event fires during a MovePosition
    // step.  Remaining move effects (MovePosition + DecrementMP pairs) are
    // skipped, preserving the unused MP.
    // `interrupted` is surfaced in the returned ApplyCtx.
    let mut halt = false;
    let mut interrupted = false;

    while let Some(effect) = effect_queue.pop_front() {
        // ── Turn-advance budget guard ─────────────────────────────────────────
        // Each AdvanceTurn/BumpRound consumes one unit of budget. When the
        // budget hits zero (all-stunned / all-dead scenario) we stop processing
        // further turn-cycle effects rather than looping forever.
        if matches!(&effect, Effect::AdvanceTurn | Effect::BumpRound) {
            if turn_advance_budget == 0 {
                break;
            }
            turn_advance_budget -= 1;
        }

        // ── Halt guard: skip remaining move effects after an interrupt ────────
        // Non-move effects (none in a pure Move action, but safe for others)
        // still proceed so nothing structural is dropped.
        if halt
            && matches!(
                &effect,
                Effect::MovePosition { .. } | Effect::DecrementMP { .. }
            )
        {
            continue;
        }

        // ── Dead-actor guard: skip remaining MovePositions when mover died ────
        if let Effect::MovePosition { actor, .. } = &effect {
            if !state.unit(*actor).is_some_and(|u| u.is_alive()) {
                continue;
            }
        }

        // ── Strict failure check (decision 6.5) ──────────────────────────────
        // Rollback for non-actor Damage targets; silently skip for the actor
        // (mid-action actor death is handled by actor-liveness truncation).
        // NOTE: in Phase 0/1 (Action::Move only) the sole Damage targets are
        // AoO victims which are always the mover (= actor_id), so the Err
        // branch below is reserved for Phase 2+ Cast/AoE actions.
        if let Effect::Damage { target, .. } = &effect {
            if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                if *target == actor_id {
                    continue; // actor died mid-action — skip silently
                }
                return Err(ActionError::TargetGone);
            }
        }

        // Capture the actor's position before MovePosition updates it.
        // For non-move effects this is unused but harmless — always prev_pos.
        let pos_before = prev_pos;

        // Capture event-stream index before this effect's events are pushed.
        // Used by the MovePosition interrupt detection to slice only the events
        // produced during this single step (UnitMoved + aura + AoO + trap).
        let ev_start_for_move = if matches!(&effect, Effect::MovePosition { .. }) {
            Some(events.len())
        } else {
            None
        };

        // Aura diff-on-move/death/phase (4c + slice C1): snapshot membership BEFORE the effect.
        // Per-effect snapshots (not per-step) so intermediate transitions are captured.
        let aura_snap_before = if effect_changes_aura_membership(&effect) {
            Some(state.aura_membership_set(content))
        } else {
            None
        };

        // Apply the effect.
        let (derived, mut ctx) = apply_effect(state, &effect, content);

        // Emit the corresponding event.
        if let Some(ev) = effect_to_event(&effect, state, Some(pos_before), &ctx) {
            events.push(ev);
        }

        // Drain unified PoolChanged events (dual-emitted alongside legacy events).
        events.append(&mut ctx.pool_events);

        // Drain skip events from AdvanceTurn/BumpRound cascades.
        events.append(&mut ctx.turn_skip_events);

        // Summon initiative roll: when Effect::Spawn succeeds, roll a d20 for
        // the new unit and record it in its initiative field. The summon is NOT
        // inserted into turn_queue.order here — reconcile_turn_order() in the
        // next BumpRound (Effect::BumpRound arm in effect.rs) does that, so the
        // summon correctly skips its spawn round and acts starting the next round.
        if let Some(uid) = ctx.spawn_uid {
            let roll = rng.roll(DiceExpr::new(1, 20, 0));
            let dex = state
                .unit(uid)
                .map(|u| u.caster_context.dex_mod)
                .unwrap_or(0);
            let total = roll + dex;
            if let Some(u) = state.unit_mut(uid) {
                u.initiative = Some(total);
            }
            events.push(Event::InitiativeRolled {
                unit: uid,
                roll,
                dex_mod: dex,
                total,
            });
        }

        // Aura diff-on-move/death (4c): emit AuraStatusGained/Lost for delta.
        if let Some(before) = aura_snap_before {
            let after = state.aura_membership_set(content);
            // Triples in `after` but not in `before` → gained.
            for (tgt, src, sid) in after.difference(&before) {
                events.push(Event::AuraStatusGained {
                    target: *tgt,
                    source: *src,
                    status_id: sid.clone(),
                });
            }
            // Triples in `before` but not in `after` → lost.
            for (tgt, src, sid) in before.difference(&after) {
                events.push(Event::AuraStatusLost {
                    target: *tgt,
                    source: *src,
                    status_id: sid.clone(),
                });
            }
        }

        // After MovePosition: process reactions one at a time via per-reaction
        // sub-queues, with an actor-liveness check before each expansion.
        if let Effect::MovePosition { actor, to } = &effect {
            let new_pos = *to;
            let mover_id = *actor;

            let reactions = scan_reactions(state, mover_id, pos_before, new_pos, content);

            for reaction in reactions {
                // Actor died from a previous reaction this step — truncate chain.
                if !state.unit(mover_id).is_some_and(|u| u.is_alive()) {
                    break;
                }

                // Depth-cap: count reactions actually processed.
                reaction_depth += 1;
                if reaction_depth > REACTION_DEPTH_LIMIT {
                    return Err(ActionError::ReactionDepthExceeded);
                }

                // Emit ReactionFired only for reactions we actually expand.
                match &reaction {
                    Reaction::OpportunityAttack { from, victim } => {
                        events.push(Event::ReactionFired {
                            actor: *from,
                            kind: ReactionKind::OpportunityAttack,
                            against: *victim,
                        });
                    }
                }

                // Expand into a sub-queue and resolve fully (incl. derived
                // Damage→GainRage→Death) before pulling the next reaction.
                let mut sub_queue: VecDeque<Effect> =
                    expand_reaction(&reaction, state, content, rng)
                        .into_iter()
                        .collect();

                while let Some(sub_eff) = sub_queue.pop_front() {
                    // Strict failure check (decision 6.5) within sub-queue —
                    // keep for non-mover targets; skip silently for the mover.
                    if let Effect::Damage { target, .. } = &sub_eff {
                        if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                            if *target == mover_id {
                                continue;
                            }
                            return Err(ActionError::TargetGone);
                        }
                    }

                    let (sub_derived, mut sub_ctx) = apply_effect(state, &sub_eff, content);

                    if let Some(ev) = effect_to_event(&sub_eff, state, Some(pos_before), &sub_ctx) {
                        events.push(ev);
                    }

                    // Drain unified PoolChanged events (dual-emitted alongside legacy events).
                    events.append(&mut sub_ctx.pool_events);

                    events.append(&mut sub_ctx.turn_skip_events);

                    for ef in sub_derived.into_iter().rev() {
                        sub_queue.push_front(ef);
                    }
                }
            }

            // ── Trap trigger (arrival) ────────────────────────────────────────
            //
            // AoO fires on the leaving edge (before arrival), traps fire on
            // arrival — so AoO always precedes the trap for the same step.
            // Symmetric across teams: any unit entering a hazard hex triggers it.
            //
            // Reuse the same sub-queue + liveness discipline as the AoO path
            // above so that a lethal trap correctly derives Death/AdvanceTurn and
            // the existing "skip remaining MovePositions when mover died" guard
            // truncates the rest of the path without special-casing here.
            if state.unit(mover_id).is_some_and(|u| u.is_alive()) {
                if let Some(trap_idx) = state.environment.iter().position(|e| {
                    e.hex == new_pos && matches!(e.kind, crate::state::EnvKind::Hazard)
                }) {
                    // One-shot: remove the trap from the board BEFORE resolving
                    // effects. It deals its damage/status once and disappears —
                    // no lingering marker, and a re-entrant scan can't fire it
                    // again. (`EnvRevealed` is reserved for the future reveal
                    // mechanic — Kael spotting an *armed* trap — not firing.)
                    let trap = state.environment.remove(trap_idx);
                    let trap_id = trap.id;
                    let trap_ability = trap.ability;

                    // Log/animation hook; damage/status events follow from the fanout.
                    events.push(Event::HazardTriggered {
                        env_id: trap_id,
                        victim: mover_id,
                    });

                    // Resolve the ability definition; skip defensively if missing.
                    if let Some(def) = content.ability_def(&trap_ability).cloned() {
                        let env_source = crate::state::EffectSource::Env(trap_id);
                        let caster = crate::content::CasterContext::default();

                        // Build effects for the mover: damage component (if any)
                        // + StatusOn::Target statuses.  AoE / StatusOn::MySelf
                        // are not meaningful for a single-target ground trap.
                        let mut trap_sub: std::collections::VecDeque<Effect> =
                            std::collections::VecDeque::new();

                        if let Some(dmg_eff) = effect_for_target(
                            &def.effect,
                            env_source,
                            mover_id,
                            &caster,
                            rng,
                            false,
                        ) {
                            trap_sub.push_back(dmg_eff);
                        }

                        for status_app in &def.statuses {
                            if matches!(status_app.on, crate::content::StatusOn::Target) {
                                trap_sub.push_back(Effect::ApplyStatus {
                                    target: mover_id,
                                    status: status_app.status.clone(),
                                    rounds: status_app.duration_rounds,
                                    dot_per_tick: 0,
                                    applier: env_source,
                                });
                            }
                        }

                        // Drain the trap sub-queue with the same discipline as
                        // the AoO sub-queue: strict target-liveness check (skip
                        // silently for the mover, error for others), depth cap
                        // shared with AoO reactions.
                        while let Some(sub_eff) = trap_sub.pop_front() {
                            if let Effect::Damage { target, .. } = &sub_eff {
                                if !state.unit(*target).is_some_and(|u| u.is_alive()) {
                                    if *target == mover_id {
                                        continue;
                                    }
                                    return Err(ActionError::TargetGone);
                                }
                            }

                            reaction_depth += 1;
                            if reaction_depth > REACTION_DEPTH_LIMIT {
                                return Err(ActionError::ReactionDepthExceeded);
                            }

                            let (sub_derived, mut sub_ctx) = apply_effect(state, &sub_eff, content);

                            if let Some(ev) =
                                effect_to_event(&sub_eff, state, Some(pos_before), &sub_ctx)
                            {
                                events.push(ev);
                            }

                            events.append(&mut sub_ctx.pool_events);
                            events.append(&mut sub_ctx.turn_skip_events);

                            for ef in sub_derived.into_iter().rev() {
                                trap_sub.push_front(ef);
                            }
                        }
                    }
                }
            }

            // ── On-move interrupt detection ───────────────────────────────────
            // WAVE 3: Run on-move passives (e.g. scout_traps reveal) AFTER the
            // position is updated (so the scan centers on new_pos) and BEFORE
            // the eventful check so a newly-revealed hazard counts as a
            // non-benign event and triggers halt/interrupt.
            //
            // Borrow-checker note: `resolve_on_move_passives` takes `&mut state`.
            // We call it first (completing the &mut borrow), then extend `events`
            // — no simultaneous mutable borrows.
            let reveal_events = state.resolve_on_move_passives(mover_id, content);
            events.extend(reveal_events);

            let ev_start = ev_start_for_move.unwrap_or(events.len());
            let eventful = events[ev_start..]
                .iter()
                .any(|e| !is_benign_move_event(e, mover_id));
            if eventful {
                halt = true;
                interrupted = true;
            }

            // ── No-stacking on interrupt: slide off an occupied pass-through hex ──
            //
            // Pre-validate allows a path to *pass through* a friendly-occupied hex
            // (only the final hex and enemy-occupied intermediates are rejected).
            // It assumes the mover always reaches the validated final hex. But an
            // interrupt (AoO / trap / reveal) halts movement on whatever hex the
            // current step just landed on — which may be one of those friendly
            // pass-through hexes, leaving two alive units stacked (violates the
            // "landing must be empty" rule, and trips the bridge's one-unit-per-hex
            // index → debug_assert panic / silent corruption in release).
            //
            // Fix: when a halt would rest the mover on a hex occupied by another
            // alive unit, slide it forward along the already-validated remaining
            // path to the first unoccupied hex. The pre-validated final hex is
            // guaranteed unoccupied (only the mover changes position within a step,
            // and deaths only free cells), so a legal landing always exists. No new
            // RNG is drawn and no further reactions are scanned for the slide — the
            // mover's turn is still interrupted; it just cannot physically rest on
            // an ally's cell.
            if halt && state.unit(mover_id).is_some_and(|u| u.is_alive()) {
                let resting_occupied = state
                    .units()
                    .iter()
                    .any(|u| u.is_alive() && u.id != mover_id && u.pos == new_pos);
                if resting_occupied {
                    // Remaining queued path hexes, in order, for this same Move.
                    let next_free = effect_queue.iter().find_map(|e| match e {
                        Effect::MovePosition { to, .. }
                            if !state
                                .units()
                                .iter()
                                .any(|u| u.is_alive() && u.id != mover_id && u.pos == *to) =>
                        {
                            Some(*to)
                        }
                        _ => None,
                    });
                    if let Some(landing) = next_free {
                        if let Some(u) = state.unit_mut(mover_id) {
                            u.pos = landing;
                        }
                        events.push(Event::UnitMoved {
                            actor: mover_id,
                            from: new_pos,
                            to: landing,
                        });
                    }
                }
            }

            // Update prev_pos for the next move step.
            // (Irrelevant once the mover is dead / halted, but harmless to advance.)
            prev_pos = new_pos;
        }

        // Derived effects (e.g. GainRage, Death from Damage in the main queue)
        // go to the front to preserve per-target ordering (decision 6.3).
        for ef in derived.into_iter().rev() {
            effect_queue.push_front(ef);
        }
    }

    // ── Emit TurnStarted whenever the current actor changed during this step ──
    //
    // Covers two paths:
    //   1. Action::EndTurn — AdvanceTurn cascade always changes current.
    //   2. Death-mid-action — Effect::Death of the current actor derives
    //      AdvanceTurn (and pushes TurnEnded via turn_skip_events), which
    //      advances the queue so final_current != initial_current.
    //
    // Emitted after the full pump loop has settled so TurnStarted always
    // refers to the actor who will actually act next.
    let final_current = (state.turn_queue.current(), state.round);
    if initial_current != final_current {
        if let Some(next_actor) = final_current.0 {
            events.push(Event::TurnStarted { actor: next_actor });
            // Refill AP/MP, regen mana/energy, tick statuses for the incoming actor.
            // Was previously done by bridge's engine_turn_start_system; absorbed here
            // so the full turn-lifecycle flows through one event stream.
            events.extend(state.start_actor_turn(next_actor, content));
        }
    }

    // ── S6: voluntary auto-end after Cast exhausts AP+MP ─────────────────────
    //
    // If the action was a Cast and (a) the turn did NOT already advance via
    // death-cascade or an explicit AdvanceTurn, AND (b) the actor now has
    // AP ≤ 0 AND MP ≤ 0, emit TurnEnded{cause: ResourcesExhausted} and queue
    // AdvanceTurn exactly as Action::EndTurn would — but without a second
    // step() call.  The bridge's separate auto-end block is removed; this path
    // is the single authoritative source.
    if matches!(&action, Action::Cast { .. }) && initial_current == final_current {
        if let Some(actor_unit) = state.unit(actor_id) {
            let ap_left = actor_unit.pools[crate::PoolKind::Ap]
                .map(|(c, _)| c)
                .unwrap_or(0);
            let mp_left = actor_unit.pools[crate::PoolKind::Mp]
                .map(|(c, _)| c)
                .unwrap_or(0);
            if ap_left <= 0 && mp_left <= 0 {
                // Emit TurnEnded before the AdvanceTurn cascade.
                events.push(Event::TurnEnded {
                    actor: actor_id,
                    cause: crate::event::TurnEndCause::ResourcesExhausted,
                });
                // Push AdvanceTurn and drain it inline so TurnStarted lands
                // in the same event stream (mirrors the EndTurn arm).
                let mut advance_queue: std::collections::VecDeque<Effect> =
                    std::collections::VecDeque::from([Effect::AdvanceTurn]);
                while let Some(eff) = advance_queue.pop_front() {
                    if matches!(&eff, Effect::AdvanceTurn | Effect::BumpRound) {
                        if turn_advance_budget == 0 {
                            break;
                        }
                        turn_advance_budget -= 1;
                    }
                    let (derived, mut ctx) = apply_effect(state, &eff, content);
                    events.append(&mut ctx.turn_skip_events);
                    for ef in derived.into_iter().rev() {
                        advance_queue.push_front(ef);
                    }
                }
                // Emit TurnStarted if the cursor moved.
                let s6_final = (state.turn_queue.current(), state.round);
                if initial_current != s6_final {
                    if let Some(next_actor) = s6_final.0 {
                        events.push(Event::TurnStarted { actor: next_actor });
                        events.extend(state.start_actor_turn(next_actor, content));
                    }
                }
            }
        }
    }

    // ── Emit ActionFinished ───────────────────────────────────────────────────

    events.push(Event::ActionFinished { action });

    Ok((events, interrupted))
}
