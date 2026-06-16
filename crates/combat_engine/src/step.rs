//! `step()` — the public engine entry point.
//!
//! Validates an action, expands it into an effect queue, then pumps effects one
//! at a time, scanning for reactions (AoOs) after each `MovePosition`.
//!
//! ## Strict failure (decision 6.5)
//! A `Damage` effect on an already-dead unit that is **not** the actor →
//! `Err(TargetGone)` + rollback. If the dead target *is* the actor (killed by an
//! earlier reaction) the effect is silently skipped (see truncation below). The
//! non-actor branch is only reachable for AoE bursts where one target dies
//! mid-burst; `Action::Move`'s only Damage targets are AoO victims (= the actor).
//!
//! ## Actor-liveness truncation
//! Reactions after a `MovePosition` run via per-reaction sub-queues; if the mover
//! died from the previous reaction, the rest are skipped — no `ReactionFired`
//! emitted, `reactions_left` not decremented. Further `MovePosition`s on the path
//! are skipped by the dead-actor guard atop the pump loop.
//!
//! ## Reaction depth cap
//! >100 reaction expansions → `Err(ReactionDepthExceeded)` + rollback.

use std::collections::{HashMap, VecDeque};

use crate::{
    action::{Action, ActionError},
    content::{AbilityDef, CasterContext, ContentView, EffectDef, StatusDef},
    dice::{DiceExpr, DiceSource},
    effect::{apply_and_drain, apply_effect, ApplyCtx, Effect},
    event::Event,
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
/// `pub` so legality-parity tests can build it directly and compare against the
/// Bevy `BevyActions` adapter.
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
        // Engine doesn't track per-unit ability lists, so `AbilityNotInList`
        // can't fire from engine pre-validate — Bevy/snapshot backends enforce
        // it upstream (`validate_action_system` / `generate_plans`). Returning
        // `true` defers the check to that boundary.
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
        // Engine is grid-topology-agnostic; `TargetOutOfBounds` can't fire from
        // engine pre-validate — the Bevy gate catches it client-side.
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

    fn actor_weapon_channels(
        &self,
        actor: crate::state::UnitId,
    ) -> (Option<crate::DiceExpr>, Option<crate::DiceExpr>) {
        self.state
            .unit(actor)
            .map(|u| (u.caster_context.weapon_dice, u.caster_context.ranged_dice))
            .unwrap_or((None, None))
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
/// `power` is the ability-level multiplier (see `AbilityDef::power()`).
///
/// Returns `None` for non-damage variants (`None`, `Heal`, `GrantMovement`,
/// `RestoreResources`) or `WeaponAttack` without `weapon_dice`.
fn effect_for_target(
    effect: &EffectDef,
    power: f32,
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
                magic: false,
            })
        }
        EffectDef::SpellDamage { dice } => {
            let sp_scaled = (power * caster.spell_power as f32).round() as i32;
            let raw = (roll!(*dice) + caster.int_mod + sp_scaled) as f32;
            Some(Effect::Damage {
                target,
                raw,
                source,
                // magic=true → mitigated by magic_resist branch (not armor);
                // pierces=false so magic_resist applies.
                pierces: false,
                magic: true,
            })
        }
        EffectDef::WeaponAttack { ranged } => {
            let dice = if *ranged {
                caster.ranged_dice
            } else {
                caster.weapon_dice
            }?;
            let m = if *ranged {
                caster.dex_mod
            } else {
                caster.str_mod
            };
            let raw = roll!(dice) as f32 * power + m as f32;
            Some(Effect::Damage {
                target,
                raw,
                source,
                pierces: false,
                magic: false,
            })
        }
        EffectDef::Heal { dice } => {
            let sp_scaled = (power * caster.spell_power as f32).round() as i32;
            let amount = (roll!(*dice) + caster.int_mod + sp_scaled).max(0);
            Some(Effect::Heal { target, amount })
        }
        // Per-actor / non-damage effects — handled elsewhere, not here.
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
/// **Exhaustive match — no wildcard.** New `Event` variants must be classified
/// here explicitly; default to non-benign (halts movement). `UnitMoved` for a
/// different actor is non-benign intentionally.
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
    // recursion. Budget allows full-queue traversal + one round boundary.
    let mut turn_advance_budget: usize = state.turn_queue.order.len() * 3 + 8;

    // (current actor, round) before any effects. Compared against final values
    // after the pump loop to detect turn advances — including a round-wrap where
    // the same actor acts again in round N+1 (caught by round inequality).
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
            // Engine-side legality check; maps IllegalReason → ActionError::Illegal
            // so callers share Bevy `validate_action_system`'s rejection vocabulary.
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
            // Pre-validate already confirmed the actor can afford every cost.
            // Rebuild AbilityDef from ContentView (cheap; avoids carrying it).
            let def = content.ability_def(ability).expect(
                "cast: ability_def returns Some — already verified by legality pre-validate",
            );
            let caster = state
                .unit(*actor)
                .map(|u| u.caster_context.clone())
                .unwrap_or_default();

            // Re-run check_legality only to capture the disadvantage flag; the
            // pre-validate arm confirmed Ok so this can't fail (cheap pure read).
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

            // Crit-fail roll: hard-coded d20 (matches Bevy crit_fail_die default).
            // On a 1, branch on CritFailOutcome and skip normal fanout.
            let crit_fail = rng.roll(DiceExpr::new(1, 20, 0)) == 1;

            // Emit CritFailed before cost payment: ActionStarted → CritFailed →
            // [cost events] → ActionFinished.
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
                            magic: false,
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
                            def.power(),
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

                // Status fanout: StatusOn::Target → affected units; MySelf → actor.
                // Applied after damage/heal so RefreshAggregates sees post-damage state.
                //
                // A dice-DoT status rolls ONCE here, baking
                // `roll + int_mod + round(power × spell_power)` into `dot_per_tick`.
                // Non-DoT statuses (`dot_dice = None`) consume no RNG.
                let dot_power = def.power();
                for status_app in &def.statuses {
                    let dot_dice = content
                        .status_def(&status_app.status)
                        .and_then(|sd| sd.dot_dice);
                    match status_app.on {
                        crate::content::StatusOn::Target => {
                            for &affected_id in &affected {
                                let dot_per_tick = dot_dice.map_or(0, |d| {
                                    let sp_scaled =
                                        (dot_power * caster.spell_power as f32).round() as i32;
                                    rng.roll(d) + caster.int_mod + sp_scaled
                                });
                                effect_queue.push_back(Effect::ApplyStatus {
                                    target: affected_id,
                                    status: status_app.status.clone(),
                                    rounds: status_app.duration_rounds,
                                    dot_per_tick,
                                    applier: crate::state::EffectSource::Unit(*actor),
                                });
                            }
                        }
                        crate::content::StatusOn::MySelf => {
                            let dot_per_tick = dot_dice.map_or(0, |d| {
                                let sp_scaled =
                                    (dot_power * caster.spell_power as f32).round() as i32;
                                rng.roll(d) + caster.int_mod + sp_scaled
                            });
                            effect_queue.push_back(Effect::ApplyStatus {
                                target: *actor,
                                status: status_app.status.clone(),
                                rounds: status_app.duration_rounds,
                                dot_per_tick,
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
    // `halt` → a non-benign event fired during a MovePosition; remaining
    // MovePosition+DecrementMP pairs are skipped, preserving unused MP.
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
        // Rollback for a dead non-actor Damage target; silently skip for the
        // actor (mid-action death handled by actor-liveness truncation).
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

        // Apply the effect, emit primary event, drain pool + skip events.
        let (derived, ctx) =
            apply_and_drain(state, &effect, content, Some(pos_before), &mut events);

        // Summon initiative roll: on Effect::Spawn, roll a d20 into the new
        // unit's initiative. Insertion into turn_queue.order is deferred to the
        // next BumpRound's reconcile_turn_order(), so the summon skips its spawn
        // round and first acts next round.
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

                    let (sub_derived, _sub_ctx): (Vec<Effect>, _) =
                        apply_and_drain(state, &sub_eff, content, Some(pos_before), &mut events);

                    for ef in sub_derived.into_iter().rev() {
                        sub_queue.push_front(ef);
                    }
                }
            }

            // ── Trap trigger (arrival) ────────────────────────────────────────
            // Traps fire on arrival, AoO on the leaving edge → AoO precedes the
            // trap for the same step. Any unit entering a hazard hex triggers it.
            // Reuses the AoO sub-queue + liveness discipline so a lethal trap
            // derives Death/AdvanceTurn and the dead-mover guard truncates the path.
            if state.unit(mover_id).is_some_and(|u| u.is_alive()) {
                if let Some(trap_idx) = state.environment.iter().position(|e| {
                    e.hex == new_pos && matches!(e.kind, crate::state::EnvKind::Hazard)
                }) {
                    // One-shot: remove the trap BEFORE resolving effects so a
                    // re-entrant scan can't re-fire it. (`EnvRevealed` is reserved
                    // for the future reveal mechanic, not firing.)
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
                            def.power(),
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

                        // Same discipline as the AoO sub-queue: strict liveness
                        // check (skip mover, error others), shared depth cap.
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

                            let (sub_derived, _sub_ctx): (Vec<Effect>, _) = apply_and_drain(
                                state,
                                &sub_eff,
                                content,
                                Some(pos_before),
                                &mut events,
                            );

                            for ef in sub_derived.into_iter().rev() {
                                trap_sub.push_front(ef);
                            }
                        }
                    }
                }
            }

            // ── On-move interrupt detection ───────────────────────────────────
            // Run on-move passives (e.g. scout_traps reveal) AFTER the position
            // update (scan centers on new_pos) and BEFORE the eventful check, so
            // a newly-revealed hazard counts as a non-benign event → halt.
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

            // No-stacking on interrupt: a path may pass *through* a friendly-
            // occupied hex, but an interrupt halts the mover on its current hex —
            // possibly one of those, stacking two units. Slide forward along the
            // validated path to the first free hex; the validated final hex is
            // always free, so a landing always exists. No RNG, no extra scan.
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
    // Covers Action::EndTurn (AdvanceTurn cascade) and death-mid-action (Death
    // of the current actor derives AdvanceTurn). Emitted after the pump loop
    // settles so TurnStarted refers to the actor who actually acts next.
    let final_current = (state.turn_queue.current(), state.round);
    if initial_current != final_current {
        if let Some(next_actor) = final_current.0 {
            events.push(Event::TurnStarted { actor: next_actor });
            // Refill AP/MP, regen mana/energy, tick statuses for the incoming
            // actor — absorbed here so the turn lifecycle flows through one stream.
            events.extend(state.start_actor_turn(next_actor, content));
        }
    }

    // ── S6: voluntary auto-end after Cast exhausts AP+MP ─────────────────────
    // If a Cast didn't already advance the turn and the actor now has AP ≤ 0 and
    // MP ≤ 0, emit TurnEnded{ResourcesExhausted} and queue AdvanceTurn as
    // Action::EndTurn would — the single authoritative auto-end path.
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
