//! `Effect` enum — atomic state mutations produced by the engine.
//!
//! `apply_effect(state, effect, content) -> (Vec<Effect>, ApplyCtx)` mutates
//! `state` and returns:
//! - derived effects (e.g. `Damage` → `GainRage` × 2 + possibly `Death`), and
//! - `ApplyCtx` — side-channel context for event generation.
//!
//! **Per-target ordering (decision 6.3):** `Damage` derives, in order:
//!   1. `GainRage { target: source }`
//!   2. `GainRage { target }` (the hit unit)
//!   3. `Death { unit: target }` — only if hp ≤ 0 after mitigation
//!
//! This differs from the current ECS pipeline (all-damages-then-all-rage).
//!
//! **Phase 0 set (7 variants — gate criterion 1 requires < 15).**

use hexx::Hex;

use crate::content::ContentView;
use crate::event::TurnSkipReason;
use crate::state::{ActiveStatus, CombatState, Unit, UnitId};
use crate::{ResourceKind, StatusId};

/// Expected-value variant of final-damage math (inline copy; mirrors
/// `storyforge::combat::effects_math::final_damage_f32`).
///
/// `max(1.0, raw − (armor unless pierced) + vulnerability)`
fn final_damage_f32(raw: f32, armor: f32, vulnerability: f32, pierces_armor: bool) -> f32 {
    let armor = if pierces_armor { 0.0 } else { armor };
    (raw - armor + vulnerability).max(1.0)
}

/// Atomic state mutation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Move the actor's position to `to`.
    MovePosition { actor: UnitId, to: Hex },
    /// Deduct `by` movement points from the actor.
    DecrementMP { actor: UnitId, by: i32 },
    /// Deduct `by` action points from the actor.
    DecrementAP { actor: UnitId, by: i32 },
    /// Deal raw (pre-mitigation) damage from `source` to `target`.
    Damage { target: UnitId, raw: f32, source: UnitId, pierces: bool },
    /// Restore HP on `target`, after first neutralizing active DoT
    /// statuses (Bevy parity — see `apply_effects.rs:73-114`).
    /// `amount` is the raw heal pool; final HP gain may be less if DoTs
    /// consume some of it.
    Heal { target: UnitId, amount: i32 },
    /// Deduct a resource pool (mana / rage / energy / hp) from `actor`.
    /// Mirrors `effects_outcome::pay_costs`.
    PayCost { actor: UnitId, kind: ResourceKind, amount: i32 },
    /// Add or refresh a status on `target`.  Re-apply replaces an existing
    /// entry with the same `id` (matches `apply_effects_system`'s reapply
    /// semantics).  Derives `RefreshAggregates` so derived stats catch any
    /// armor / speed bonus the new status carries.
    ApplyStatus {
        target: UnitId,
        status: StatusId,
        rounds: u32,
        dot_per_tick: i32,
        applier: UnitId,
    },
    /// Remove all entries with `status` id from `target`.  Derives
    /// `RefreshAggregates` if any were removed.
    RemoveStatus { target: UnitId, status: StatusId },
    /// Grant +1 rage (clamped to max) to `target`.
    GainRage { target: UnitId },
    /// Spend one reaction from the actor.
    DecrementReactions { actor: UnitId },
    /// Mark `unit` as dead (hp already 0 when this fires).
    Death { unit: UnitId },
    /// Recompute derived stats (speed, armor_bonus) from current statuses.
    RefreshAggregates { unit: UnitId },
    /// Apply one DoT tick for `status` on `target`. Reads `dot_per_tick` from
    /// the active status entry and `hp_percent_dot` from content to derive
    /// zero, one, or two `Damage` effects. DoT bypasses armor (`pierces=true`)
    /// to match ECS parity (`tick_statuses_on_entity` calls `apply_damage`
    /// directly, skipping the armor formula).
    TickDot { target: UnitId, status: StatusId },
    /// Decrement `rounds_remaining` by 1 for `status` on `target`.
    /// If it reaches 0: remove and derive `RefreshAggregates { unit: target }`.
    ExpireStatus { target: UnitId, status: StatusId },
    /// Spawn a new unit summoned by `summoner`. Resolves template via
    /// `ContentView::unit_template`, picks a free position via ring search around
    /// the summoner, enforces `max_active` cap. On success emits
    /// `Event::UnitSpawned`; on failure emits `Event::SpawnBlocked`.
    Spawn {
        summoner: UnitId,
        template_id: String,
        max_active: Option<u32>,
    },
    /// Advance the turn-queue cursor by one slot.
    ///
    /// If the next slot is dead or stunned (via direct statuses), derives another
    /// `Effect::AdvanceTurn` (skip recursion, bounded by queue length).
    /// If the cursor wraps, derives `Effect::BumpRound` instead (which then
    /// re-advances to index 0 via `start_round`).
    ///
    /// `Event::TurnSkipped` is pushed directly onto the provided accumulator
    /// during `apply_effect` for skip cases — the caller threads `skip_events`
    /// through the ctx.
    AdvanceTurn,
    /// Increment `state.round`, call `start_round(content)` (resets reactions,
    /// queue index, phase), and derive `RefreshAggregates` for every alive unit.
    /// Emits `Event::RoundStarted`.
    BumpRound,

    // ── Phase-transition atomics (Phase 4 step 4d) ────────────────────────

    /// Boss enters phase `phase_idx`.  Cascades into `SetMaxHp`, `SetArmor`,
    /// `SetBaseSpeed`, optionally `Heal`, and `RefreshAggregates`.
    ///
    /// Derived by `apply_effect(Damage)` when
    /// `content.check_phase_trigger(target, new_hp, max_hp)` returns `Some`.
    /// **This derivation preempts `Effect::Death`**: if the triggering damage
    /// was lethal AND `heal_to_full`, the unit's HP is restored before any
    /// death check sees it.
    EnterPhase { unit: UnitId, phase_idx: usize },

    /// Set `unit.max_hp` to `max_hp`.  No derived effects.
    SetMaxHp { unit: UnitId, max_hp: i32 },

    /// Set `unit.armor` (base armor) to `armor`.  No derived effects.
    SetArmor { unit: UnitId, armor: i32 },

    /// Set `unit.base_speed` to `base_speed`.  No derived effects.
    SetBaseSpeed { unit: UnitId, base_speed: i32 },
}

/// Structured damage breakdown produced by the `Damage` effect arm.
///
/// `mitigation` is 0 when `pierces = true` — armor is not applied in that case.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DamageCtx {
    pub raw: f32,
    pub mitigation: i32,
    pub pierces: bool,
    pub final_amount: i32,
}

/// Why a `Spawn` effect did not produce a new unit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnBlockedReason {
    TemplateMissing,
    MaxActiveReached,
    NoFreePosition,
}

/// Side-channel context produced by `apply_effect` for event generation.
///
/// Some events need data that isn't recoverable from `state` after the effect
/// has been applied (e.g. the previous position before `MovePosition`).
/// Rather than reading state twice, the driver captures this before calling
/// `apply_effect`, or `apply_effect` computes and returns it here.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ApplyCtx {
    /// Set by `Damage`: structured breakdown of raw/mitigation/final.
    pub damage: Option<DamageCtx>,
    /// Set by `Heal`: actual HP restored (after DoT-neutralization consumes
    /// some of the heal pool).  May be < `Heal.amount` if DoTs consumed
    /// part or all of the pool.
    pub heal_amount: Option<i32>,
    /// Set by `Spawn` on success: the newly generated `UnitId`.
    pub spawn_uid: Option<UnitId>,
    /// Set by `Spawn` on success: the position the new unit was placed at.
    pub spawn_pos: Option<hexx::Hex>,
    /// Set by `Spawn` when the spawn was blocked; carries the reason.
    pub spawn_blocked: Option<SpawnBlockedReason>,
    /// Set by `AdvanceTurn` for each unit whose turn was skipped (dead or
    /// stunned). The pump loop in `step()` drains these into the main event
    /// stream after calling `effect_to_event`.
    pub turn_skip_events: Vec<crate::event::Event>,
    /// Set by `EnterPhase`: carries (prev_max_hp, new_max_hp) so the event
    /// translator can populate `Event::PhaseEntered` correctly.
    pub phase_entered: Option<(i32, i32)>,
    /// Number of RNG calls consumed by this `step()` invocation (Phase 5 D2).
    ///
    /// Populated by `step()` as `rng.call_count()` after − before the effect
    /// cascade. Used by the trace writer to record a per-step canary; replay
    /// re-seeds the same `DiceRng` and asserts the delta matches.
    pub rng_calls: u64,
}

fn skip_or_settle_current(
    state: &mut CombatState,
    content: &dyn ContentView,
) -> (Vec<Effect>, ApplyCtx) {
    let Some(actor) = state.turn_queue.current() else {
        return (vec![], ApplyCtx::default());
    };

    // Dead-skip: tick sirota DoT for the dead actor, then recurse.
    if !state.unit(actor).is_some_and(|u| u.is_alive()) {
        let tick_events = state.tick_actor_statuses(actor, content);
        let mut ctx = ApplyCtx::default();
        ctx.turn_skip_events.extend(tick_events);
        ctx.turn_skip_events.push(crate::event::Event::TurnSkipped {
            actor,
            reason: TurnSkipReason::Dead,
        });
        return (vec![Effect::AdvanceTurn], ctx);
    }

    // Stunned-skip: walk direct statuses OR check aura stun (4c).
    let is_stunned_by_status = state.unit(actor).is_some_and(|u| {
        u.statuses.iter().any(|s| {
            content.status_def(&s.id).is_some_and(|d| d.skips_turn)
        })
    });
    let is_stunned_by_aura = state.aura_effects_on(actor, content).skips_turn;
    if is_stunned_by_status || is_stunned_by_aura {
        let mut ctx = ApplyCtx::default();
        ctx.turn_skip_events.push(crate::event::Event::TurnSkipped {
            actor,
            reason: TurnSkipReason::Stunned,
        });
        return (vec![Effect::AdvanceTurn], ctx);
    }

    // Actor is alive and not stunned — they are the settled next actor.
    (vec![], ApplyCtx::default())
}

/// Apply one atomic effect to `state`.
///
/// Returns `(derived_effects, ctx)`:
/// - `derived_effects`: additional effects to enqueue (in the order listed).
/// - `ctx`: side-channel data needed for event generation.
///
/// The caller is responsible for enqueueing derived effects in order.
///
/// **Decision 6.5:** liveness of the target is checked by `step()` before
/// calling `apply_effect`.  Here we only mutate and derive.
pub fn apply_effect(
    state: &mut CombatState,
    effect: &Effect,
    content: &dyn ContentView,
) -> (Vec<Effect>, ApplyCtx) {
    match effect {
        Effect::MovePosition { actor, to } => {
            if let Some(u) = state.unit_mut(*actor) {
                u.pos = *to;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::DecrementMP { actor, by } => {
            if let Some(u) = state.unit_mut(*actor) {
                u.movement_points = (u.movement_points - by).max(0);
            }
            (vec![], ApplyCtx::default())
        }

        Effect::DecrementAP { actor, by } => {
            if let Some(u) = state.unit_mut(*actor) {
                u.action_points = (u.action_points - by).max(0);
            }
            (vec![], ApplyCtx::default())
        }

        Effect::Damage { target, raw, source, pierces } => {
            // Read the target's current armor (base + bonus) for mitigation.
            let (armor, armor_bonus) = state
                .unit(*target)
                .map(|u| (u.armor, u.armor_bonus))
                .unwrap_or((0, 0));

            let mitigation = (armor + armor_bonus) as f32;
            let final_dmg = final_damage_f32(*raw, mitigation, 0.0, *pierces);

            // Apply HP reduction.
            let hp_after = if let Some(u) = state.unit_mut(*target) {
                u.hp = (u.hp - final_dmg.round() as i32).max(0);
                u.hp
            } else {
                0
            };

            // Derive: GainRage{source}, GainRage{target}, then phase-or-death.
            //
            // Phase check MUST come before Death: if a phase trigger fires with
            // `heal_to_full=true`, the cascade restores HP above 0 before any
            // Death check sees the unit — the boss never enters `Dead` state.
            // See spec §8 "Phase preempts Death — derived-effect ordering".
            let mut derived = vec![
                Effect::GainRage { target: *source },
                Effect::GainRage { target: *target },
            ];

            let max_hp = state.unit(*target).map(|u| u.max_hp).unwrap_or(0);
            if let Some((phase_idx, _transition)) =
                state.unit(*target).and_then(|u| u.check_phase_trigger(hp_after, max_hp))
            {
                derived.push(Effect::EnterPhase { unit: *target, phase_idx });
            } else if hp_after <= 0 {
                derived.push(Effect::Death { unit: *target });
            }

            let mitigation = if *pierces { 0 } else { armor + armor_bonus };
            (derived, ApplyCtx {
                damage: Some(DamageCtx {
                    raw: *raw,
                    mitigation,
                    pierces: *pierces,
                    final_amount: final_dmg.round() as i32,
                }),
                ..ApplyCtx::default()
            })
        }

        Effect::Heal { target, amount } => {
            // Two-phase heal mirrors `src/combat/apply_effects.rs:73-114`:
            //   1. Walk active DoT statuses (`dot_per_tick > 0`).  Each
            //      consumes heal in order: if `remaining >= dot`, fully
            //      neutralize that DoT (set its rounds=0 for removal,
            //      dot=0).  Otherwise, partially weaken it and exhaust
            //      the heal pool.
            //   2. Remaining heal restores HP, clamped at max_hp.
            // Returns ApplyCtx.heal_amount = actual HP restored (may be 0
            // if all heal went into DoT neutralization).
            let mut remaining = *amount;
            let mut any_status_removed = false;
            if let Some(u) = state.unit_mut(*target) {
                for s in u.statuses.iter_mut() {
                    if remaining <= 0 {
                        break;
                    }
                    if s.dot_per_tick > 0 {
                        if remaining >= s.dot_per_tick {
                            remaining -= s.dot_per_tick;
                            s.dot_per_tick = 0;
                            s.rounds_remaining = 0;
                        } else {
                            s.dot_per_tick -= remaining;
                            remaining = 0;
                        }
                    }
                }
                // Drop statuses marked for removal (rounds_remaining == 0).
                let before = u.statuses.len();
                u.statuses.retain(|s| s.rounds_remaining > 0);
                any_status_removed = u.statuses.len() < before;
            }

            let hp_restored = if remaining > 0 {
                if let Some(u) = state.unit_mut(*target) {
                    let before = u.hp;
                    u.hp = (u.hp + remaining).min(u.max_hp);
                    u.hp - before
                } else {
                    0
                }
            } else {
                0
            };

            // If a status was removed, derive RefreshAggregates so
            // armor_bonus / speed stay current.
            let derived = if any_status_removed {
                vec![Effect::RefreshAggregates { unit: *target }]
            } else {
                vec![]
            };

            (derived, ApplyCtx { heal_amount: Some(hp_restored), ..ApplyCtx::default() })
        }

        Effect::PayCost { actor, kind, amount } => {
            if let Some(u) = state.unit_mut(*actor) {
                match kind {
                    ResourceKind::Hp => {
                        u.hp = (u.hp - amount).max(0);
                    }
                    ResourceKind::Mana => {
                        if let Some((current, _max)) = u.mana.as_mut() {
                            *current = (*current - amount).max(0);
                        }
                    }
                    ResourceKind::Rage => {
                        if let Some((current, _max)) = u.rage.as_mut() {
                            *current = (*current - amount).max(0);
                        }
                    }
                    ResourceKind::Energy => {
                        if let Some((current, _max)) = u.energy.as_mut() {
                            *current = (*current - amount).max(0);
                        }
                    }
                }
            }
            (vec![], ApplyCtx::default())
        }

        Effect::ApplyStatus { target, status, rounds, dot_per_tick, applier } => {
            if let Some(u) = state.unit_mut(*target) {
                // Re-apply replaces existing entries with the same id.
                u.statuses.retain(|s| s.id != *status);
                u.statuses.push(ActiveStatus {
                    id: status.clone(),
                    rounds_remaining: *rounds,
                    dot_per_tick: *dot_per_tick,
                    applier: *applier,
                });
            }
            (
                vec![Effect::RefreshAggregates { unit: *target }],
                ApplyCtx::default(),
            )
        }

        Effect::RemoveStatus { target, status } => {
            let any_removed = if let Some(u) = state.unit_mut(*target) {
                let before = u.statuses.len();
                u.statuses.retain(|s| s.id != *status);
                u.statuses.len() < before
            } else {
                false
            };
            let derived = if any_removed {
                vec![Effect::RefreshAggregates { unit: *target }]
            } else {
                vec![]
            };
            (derived, ApplyCtx::default())
        }

        Effect::GainRage { target } => {
            if let Some(u) = state.unit_mut(*target) {
                if let Some((current, max)) = u.rage.as_mut() {
                    *current = (*current + 1).min(*max);
                }
            }
            (vec![], ApplyCtx::default())
        }

        Effect::DecrementReactions { actor } => {
            if let Some(u) = state.unit_mut(*actor) {
                u.reactions_left = (u.reactions_left - 1).max(0);
            }
            (vec![], ApplyCtx::default())
        }

        Effect::Death { unit } => {
            // Collect unique status ids on the dying unit before zeroing hp.
            // Linear dedup preserves insertion order — determinism: same input
            // → same derived-event order across runs.
            let statuses_to_clean: Vec<StatusId> = state
                .unit(*unit)
                .map(|u| {
                    let mut ids: Vec<StatusId> = Vec::new();
                    for s in &u.statuses {
                        if !ids.contains(&s.id) {
                            ids.push(s.id.clone());
                        }
                    }
                    ids
                })
                .unwrap_or_default();

            if let Some(u) = state.unit_mut(*unit) {
                u.hp = 0;
            }

            let derived = statuses_to_clean
                .into_iter()
                .map(|status| Effect::RemoveStatus { target: *unit, status })
                .collect();

            (derived, ApplyCtx::default())
        }

                Effect::RefreshAggregates { unit } => {
            // Recompute speed and armor_bonus from active statuses + aura effects (4c).
            // Reads status bonuses via ContentView — no Bevy dep in the engine.
            let unit_id = *unit;
            if let Some(u) = state.unit_mut(unit_id) {
                let mut speed_bonus: i32 = 0;
                let mut armor_bonus: i32 = 0;
                for s in &u.statuses {
                    let b = content.status_bonuses(&s.id);
                    speed_bonus += b.speed_bonus;
                    armor_bonus += b.armor_bonus;
                }
                u.speed = u.base_speed + speed_bonus;
                u.armor_bonus = armor_bonus;
            }
            // Fold aura bonuses on top of status-derived aggregates.
            let aura_fx = state.aura_effects_on(unit_id, content);
            if let Some(u) = state.unit_mut(unit_id) {
                u.speed       += aura_fx.speed_bonus;
                u.armor_bonus += aura_fx.armor_bonus;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::TickDot { target, status } => {
            let mut derived: Vec<Effect> = Vec::new();

            let (dot_per_tick, applier, max_hp) = {
                let Some(u) = state.unit(*target) else { return (derived, ApplyCtx::default()); };
                let Some(s) = u.statuses.iter().find(|s| s.id == *status) else {
                    return (derived, ApplyCtx::default());
                };
                (s.dot_per_tick, s.applier, u.max_hp)
            };

            let percent = content.status_def(status).map(|sd| sd.hp_percent_dot).unwrap_or(0);

            if dot_per_tick > 0 {
                derived.push(Effect::Damage {
                    target: *target,
                    raw: dot_per_tick as f32,
                    source: applier,
                    pierces: true,
                });
            }
            if percent > 0 {
                let amount = (max_hp * percent + 99) / 100;
                if amount > 0 {
                    derived.push(Effect::Damage {
                        target: *target,
                        raw: amount as f32,
                        source: applier,
                        pierces: true,
                    });
                }
            }

            (derived, ApplyCtx::default())
        }

        Effect::ExpireStatus { target, status } => {
            let expired = if let Some(u) = state.unit_mut(*target) {
                if let Some(s) = u.statuses.iter_mut().find(|s| s.id == *status) {
                    s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
                    s.rounds_remaining == 0
                } else {
                    false
                }
            } else {
                false
            };

            let derived = if expired {
                vec![Effect::RemoveStatus { target: *target, status: status.clone() }]
            } else {
                vec![]
            };

            (derived, ApplyCtx::default())
        }

        Effect::AdvanceTurn => {
            if state.turn_queue.is_empty() {
                return (vec![], ApplyCtx::default());
            }

            let prev_idx = state.turn_queue.index;
            state.turn_queue.advance();

            // Wrap detection: cursor wrapped → start a new round.
            // BumpRound resets index=0 via start_round and then runs
            // skip_or_settle_current at the new index.
            if state.turn_queue.wrapped_after(prev_idx) {
                return (vec![Effect::BumpRound], ApplyCtx::default());
            }

            // Not a wrap: check whether the new cursor actor can act.
            skip_or_settle_current(state, content)
        }

        // ── Phase-transition atomics (4d) ─────────────────────────────────────

        Effect::EnterPhase { unit, phase_idx: _ } => {
            // Re-read the current max_hp before any mutation for the event.
            let prev_max_hp = state.unit(*unit).map(|u| u.max_hp).unwrap_or(0);

            // Re-call check_phase_trigger with current hp/max_hp to recover the
            // transition data.  The bridge's EnemyPhases.pending has not been
            // popped yet (pop happens in bridge translator on Event::PhaseEntered),
            // so this returns the same result as the Damage arm's call.
            let transition = state
                .unit(*unit)
                .and_then(|u| u.check_phase_trigger(u.hp, prev_max_hp))
                .map(|(_, t)| t);

            let (new_max_hp, new_armor, new_base_speed, heal_to_full) =
                transition.map(|t| (t.new_max_hp, t.new_armor, t.new_base_speed, t.heal_to_full))
                    .unwrap_or((prev_max_hp, 0, 0, false));

            let mut derived: Vec<Effect> = vec![Effect::SetMaxHp { unit: *unit, max_hp: new_max_hp }];
            if new_armor != 0 {
                derived.push(Effect::SetArmor { unit: *unit, armor: new_armor });
            }
            if new_base_speed != 0 {
                derived.push(Effect::SetBaseSpeed { unit: *unit, base_speed: new_base_speed });
            }
            if heal_to_full {
                derived.push(Effect::Heal { target: *unit, amount: new_max_hp });
            }
            derived.push(Effect::RefreshAggregates { unit: *unit });

            let ctx = ApplyCtx {
                phase_entered: Some((prev_max_hp, new_max_hp)),
                ..ApplyCtx::default()
            };
            (derived, ctx)
        }

        Effect::SetMaxHp { unit, max_hp } => {
            if let Some(u) = state.unit_mut(*unit) {
                u.max_hp = *max_hp;
                // Clamp current hp to new max in case it exceeds it.
                u.hp = u.hp.min(u.max_hp);
            }
            (vec![], ApplyCtx::default())
        }

        Effect::SetArmor { unit, armor } => {
            if let Some(u) = state.unit_mut(*unit) {
                u.armor = *armor;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::SetBaseSpeed { unit, base_speed } => {
            if let Some(u) = state.unit_mut(*unit) {
                u.base_speed = *base_speed;
                // Also update effective speed to match (RefreshAggregates will
                // fine-tune it, but keeping them in sync avoids a stale window).
                u.speed = *base_speed;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::BumpRound => {
            state.round += 1;
            // start_round resets index=0, sets phase=ActorTurn, resets
            // reactions_left=reactions_max for alive units.
            state.start_round(content);

            // Derive RefreshAggregates for every alive unit first.
            // RefreshAggregates doesn't affect skips_turn (status-driven),
            // so it's safe to run skip_or_settle_current before they fire —
            // ordering in derived ensures RefreshAggregates come first in queue.
            let alive_ids: Vec<UnitId> = state.alive_units().map(|u| u.id).collect();
            let mut derived: Vec<Effect> = alive_ids
                .into_iter()
                .map(|id| Effect::RefreshAggregates { unit: id })
                .collect();

            // Check index-0 actor: if dead/stunned, emit skip events and
            // derive AdvanceTurn to continue the chain; if alive+active, settle.
            let (skip_derived, skip_ctx) = skip_or_settle_current(state, content);
            derived.extend(skip_derived);

            (derived, skip_ctx)
        }

        Effect::Spawn { summoner, template_id, max_active } => {
            let template = match content.unit_template(template_id) {
                Some(t) => t,
                None => return (vec![], ApplyCtx {
                    spawn_blocked: Some(SpawnBlockedReason::TemplateMissing),
                    ..ApplyCtx::default()
                }),
            };

            let (summoner_pos, summoner_team) = match state.unit(*summoner) {
                Some(u) => (u.pos, u.team),
                None => return (vec![], ApplyCtx {
                    spawn_blocked: Some(SpawnBlockedReason::TemplateMissing),
                    ..ApplyCtx::default()
                }),
            };

            if let Some(cap) = max_active {
                let active = state
                    .alive_units()
                    .filter(|u| u.summoner == Some(*summoner))
                    .count() as u32;
                if active >= *cap {
                    return (vec![], ApplyCtx {
                        spawn_blocked: Some(SpawnBlockedReason::MaxActiveReached),
                        ..ApplyCtx::default()
                    });
                }
            }

            // Include dead tombstones: ECS keeps their `HexPositions` entry until
            // the entity is despawned, so a spawn landing on a corpse would
            // collide downstream. Treating tombstones as occupied matches the
            // ECS view and avoids a bridge-side `positions.insert` panic.
            let occupied: std::collections::HashSet<hexx::Hex> =
                state.units().iter().map(|u| u.pos).collect();

            let pos = summoner_pos
                .range(2)
                .find(|h| *h != summoner_pos && !occupied.contains(h));

            let pos = match pos {
                Some(p) => p,
                None => return (vec![], ApplyCtx {
                    spawn_blocked: Some(SpawnBlockedReason::NoFreePosition),
                    ..ApplyCtx::default()
                }),
            };

            let new_uid = state.alloc_synthetic_uid();
            let new_unit = Unit {
                id: new_uid,
                team: summoner_team,
                pos,
                hp: template.max_hp,
                max_hp: template.max_hp,
                armor: template.armor,
                armor_bonus: 0,
                base_speed: template.base_speed,
                speed: template.base_speed,
                action_points: template.max_ap,
                max_ap: template.max_ap,
                movement_points: template.base_speed,
                reactions_left: 0,
                reactions_max: 1,
                statuses: Vec::new(),
                rage: if template.rage_max > 0 { Some((0, template.rage_max)) } else { None },
                mana: if template.mana_max > 0 { Some((template.mana_max, template.mana_max)) } else { None },
                energy: if template.energy_max > 0 { Some((template.energy_max, template.energy_max)) } else { None },
                summoner: Some(*summoner),
                caster_context: crate::content::CasterContext::default(),
                aoo_dice: None,
                auras: Vec::new(),
                enemy_phases: Vec::new(),
            };

            state.insert_unit(new_unit);

            (vec![], ApplyCtx {
                spawn_uid: Some(new_uid),
                spawn_pos: Some(pos),
                ..ApplyCtx::default()
            })
        }
    }
}
