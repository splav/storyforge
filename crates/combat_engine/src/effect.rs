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

/// Expected-value final-damage formula shared between live engine path and
/// AI sim / scoring projections.
///
/// `max(1.0, raw − (armor unless pierced) + vulnerability)` — the min-1 floor
/// matches the live contract: any damage-intent attack that hits leaves at
/// least 1 HP of impact, even vs. heavy armor.
pub fn final_damage_f32(raw: f32, armor: f32, vulnerability: f32, pierces_armor: bool) -> f32 {
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
    Damage {
        target: UnitId,
        raw: f32,
        source: crate::state::EffectSource,
        pierces: bool,
    },
    /// Restore HP on `target`, after first neutralizing active DoT
    /// statuses (Bevy parity — see `apply_effects.rs:73-114`).
    /// `amount` is the raw heal pool; final HP gain may be less if DoTs
    /// consume some of it.
    Heal { target: UnitId, amount: i32 },
    /// Deduct a resource pool (mana / rage / energy / hp) from `actor`.
    /// Mirrors `effects_outcome::pay_costs`.
    PayCost {
        actor: UnitId,
        kind: ResourceKind,
        amount: i32,
    },
    /// Add or refresh a status on `target`.  Re-apply replaces an existing
    /// entry with the same `id` (matches `apply_effects_system`'s reapply
    /// semantics).  Derives `RefreshAggregates` so derived stats catch any
    /// armor / speed bonus the new status carries.
    ApplyStatus {
        target: UnitId,
        status: StatusId,
        rounds: u32,
        dot_per_tick: i32,
        applier: crate::state::EffectSource,
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
    /// Apply one HoT tick for `status` on `target`. Reads `heal_per_tick` from
    /// content (content-driven, analogous to `hp_percent_dot` for DoT).
    /// Restores HP clamped to `max_hp`. Never derives Death, EnterPhase, or
    /// rage — healing cannot kill, phase-trigger, or grant rage.
    TickHeal { target: UnitId, status: StatusId },
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

    // ── Passive ability effects ───────────────────────────────────────────
    /// Scan `state.environment` for hazards not yet visible to the caster's
    /// team within `range` hexes of `caster`.  Derives one
    /// `RevealEnv { id, revealer }` per match.  No own event.  Zero rng.
    RevealEnvInRange { caster: UnitId, range: i32 },

    /// Insert `revealer`'s team into `revealed_to` of the env object with `id`.
    ///
    /// Idempotent per team: re-revealing the same object to the same team is a
    /// no-op and emits no event.  Never inserts the *opponent's* team — the
    /// `revealer` field ensures only the caster's own team gains knowledge.
    ///
    /// Emits `Event::EnvRevealed { env_id: id }` when newly revealed.
    RevealEnv {
        id: crate::state::EnvId,
        revealer: crate::state::Team,
    },
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

/// Structured DoT tick breakdown produced by the `TickDot` effect arm.
///
/// Populated when a tick deals damage (dot_per_tick > 0 or hp_percent_dot > 0).
/// `mitigation` is always 0 (DoT pierces armor). `pierces` is always true.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DotDamageCtx {
    pub source: crate::state::EffectSource,
    pub source_status: crate::StatusId,
    pub raw: f32,
    pub mitigation: i32,
    pub pierces: bool,
    pub final_amount: i32,
}

/// Structured HoT tick breakdown produced by the `TickHeal` effect arm.
///
/// Populated when a tick restores HP (`heal_per_tick > 0` from content).
/// `amount` is the actual HP restored (may be < `heal_per_tick` if the unit
/// was near max HP).
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HotHealCtx {
    pub source: crate::state::EffectSource,
    pub source_status: crate::StatusId,
    pub amount: i32,
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
    /// Set by `TickDot` when the tick deals damage (dot_per_tick > 0 or
    /// hp_percent_dot > 0).  Carries the fused breakdown so `effect_to_event`
    /// can emit `Event::DotDamaged` instead of the now-separate StatusTicked +
    /// UnitDamaged pair.
    pub dot_damage: Option<DotDamageCtx>,
    /// Set by `TickHeal` when the tick restores HP (`heal_per_tick > 0`).
    /// Carries source + amount so `effect_to_event` can emit
    /// `Event::HotHealed`.  `None` when `heal_per_tick == 0` or the unit is
    /// already at max HP (no-op tick).
    pub hot_heal: Option<HotHealCtx>,
    /// Additional `Event::PoolChanged` events produced by pool-mutation effects
    /// (`PayCost`, `DecrementAP`, `DecrementMP`, `GainRage`). Drained by the
    /// pump loop in `step()` immediately after the main event from
    /// `effect_to_event`.  Dual-emitted alongside legacy per-pool events in C4;
    /// legacy events will be removed in a follow-up cleanup.
    pub pool_events: Vec<crate::event::Event>,
    /// Set by `RevealEnv` when the env object was newly revealed (was hidden
    /// before this effect).  `false` when the object was already revealed
    /// (idempotent no-op) — `effect_to_event` emits `EnvRevealed` only when
    /// this is `true`.
    pub env_revealed: bool,
    /// Set by `step()` when a `Move` action was interrupted mid-path because
    /// a non-benign event occurred during a `MovePosition` step (e.g. AoO,
    /// trap trigger).  `false` for clean moves and all non-Move actions.
    pub interrupted: bool,
}

pub(crate) fn skip_or_settle_current(
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
        u.statuses
            .iter()
            .any(|s| content.status_def(&s.id).is_some_and(|d| d.skips_turn))
    });
    let is_stunned_by_aura = state.aura_effects_on(actor, content).skips_turn;
    if is_stunned_by_status || is_stunned_by_aura {
        // Tick the stunned actor's statuses (sirota-DoT + status expiry for
        // statuses applied BY this actor) so that 1-turn stuns expire properly
        // and DoTs on victims tick each round even when their source is stunned.
        let tick_events = state.tick_actor_statuses(actor, content);
        let mut ctx = ApplyCtx::default();
        ctx.turn_skip_events.extend(tick_events);
        ctx.turn_skip_events.push(crate::event::Event::TurnSkipped {
            actor,
            reason: TurnSkipReason::Stunned,
        });
        return (vec![Effect::AdvanceTurn], ctx);
    }

    // Actor is alive and not stunned — they are the settled next actor.
    (vec![], ApplyCtx::default())
}

/// Returns the ids of all environment objects that are:
/// - kind == Hazard
/// - not yet visible to `caster_team`
/// - within `range` hexes (inclusive) of `center`
///
/// Preserves the iteration order of `state.environment` for determinism.
pub(crate) fn scan_revealable_in_range(
    state: &CombatState,
    center: Hex,
    range: i32,
    caster_team: crate::state::Team,
) -> Vec<crate::state::EnvId> {
    state
        .environment
        .iter()
        .filter(|e| {
            matches!(e.kind, crate::state::EnvKind::Hazard)
                && !e.visible_to(caster_team)
                && center.unsigned_distance_to(e.hex) as i32 <= range
        })
        .map(|e| e.id)
        .collect()
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
            let mut ctx = ApplyCtx::default();
            if let Some(u) = state.unit_mut(*actor) {
                if let Some((pc, max)) = u.pools[crate::PoolKind::Mp].as_mut() {
                    *pc = (*pc - by).max(0);
                    ctx.pool_events.push(crate::event::Event::PoolChanged {
                        unit: *actor,
                        pool: crate::PoolKind::Mp,
                        current: *pc,
                        max: *max,
                        cause: crate::PoolChangeCause::Spent,
                    });
                }
            }
            (vec![], ctx)
        }

        Effect::DecrementAP { actor, by } => {
            let mut ctx = ApplyCtx::default();
            if let Some(u) = state.unit_mut(*actor) {
                if let Some((pc, max)) = u.pools[crate::PoolKind::Ap].as_mut() {
                    *pc = (*pc - by).max(0);
                    ctx.pool_events.push(crate::event::Event::PoolChanged {
                        unit: *actor,
                        pool: crate::PoolKind::Ap,
                        current: *pc,
                        max: *max,
                        cause: crate::PoolChangeCause::Spent,
                    });
                }
            }
            (vec![], ctx)
        }

        Effect::Damage {
            target,
            raw,
            source,
            pierces,
        } => {
            // Read the target's current armor (base + bonus) for mitigation.
            let (armor, armor_bonus) = state
                .unit(*target)
                .map(|u| (u.armor, u.armor_bonus))
                .unwrap_or((0, 0));

            let mitigation = (armor + armor_bonus) as f32;
            let final_dmg = final_damage_f32(*raw, mitigation, 0.0, *pierces);

            // Apply HP reduction.
            let hp_after = if let Some(u) = state.unit_mut(*target) {
                let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                p.0 = (p.0 - final_dmg.round() as i32).max(0);
                u.hp()
            } else {
                0
            };

            // Derive: GainRage{source}, GainRage{target}, then phase-or-death.
            //
            // Phase check MUST come before Death: if a phase trigger fires with
            // `heal_to_full=true`, the cascade restores HP above 0 before any
            // Death check sees the unit — the boss never enters `Dead` state.
            // See spec §8 "Phase preempts Death — derived-effect ordering".
            // Env sources don't accumulate rage — only unit sources do.
            let mut derived: Vec<Effect> = Vec::new();
            if let crate::state::EffectSource::Unit(u) = source {
                derived.push(Effect::GainRage { target: *u });
            }
            derived.push(Effect::GainRage { target: *target });

            let max_hp = state.unit(*target).map(|u| u.max_hp()).unwrap_or(0);
            if let Some((phase_idx, _transition)) = state
                .unit(*target)
                .and_then(|u| u.check_phase_trigger(hp_after, max_hp))
            {
                derived.push(Effect::EnterPhase {
                    unit: *target,
                    phase_idx,
                });
            } else if hp_after <= 0 {
                derived.push(Effect::Death { unit: *target });
            }

            let mitigation = if *pierces { 0 } else { armor + armor_bonus };
            (
                derived,
                ApplyCtx {
                    damage: Some(DamageCtx {
                        raw: *raw,
                        mitigation,
                        pierces: *pierces,
                        final_amount: final_dmg.round() as i32,
                    }),
                    ..ApplyCtx::default()
                },
            )
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
                    let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                    let before = p.0;
                    p.0 = (p.0 + remaining).min(p.1);
                    u.hp() - before
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

            (
                derived,
                ApplyCtx {
                    heal_amount: Some(hp_restored),
                    ..ApplyCtx::default()
                },
            )
        }

        Effect::PayCost {
            actor,
            kind,
            amount,
        } => {
            let mut ctx = ApplyCtx::default();
            if let Some(u) = state.unit_mut(*actor) {
                match kind {
                    ResourceKind::Hp => {
                        let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                        p.0 = (p.0 - amount).max(0);
                    }
                    ResourceKind::Mana => {
                        if let Some((pc, max)) = u.pools[crate::PoolKind::Mana].as_mut() {
                            *pc = (*pc - amount).max(0);
                            ctx.pool_events.push(crate::event::Event::PoolChanged {
                                unit: *actor,
                                pool: crate::PoolKind::Mana,
                                current: *pc,
                                max: *max,
                                cause: crate::PoolChangeCause::Spent,
                            });
                        }
                    }
                    ResourceKind::Rage => {
                        if let Some((pc, max)) = u.pools[crate::PoolKind::Rage].as_mut() {
                            *pc = (*pc - amount).max(0);
                            ctx.pool_events.push(crate::event::Event::PoolChanged {
                                unit: *actor,
                                pool: crate::PoolKind::Rage,
                                current: *pc,
                                max: *max,
                                cause: crate::PoolChangeCause::Spent,
                            });
                        }
                    }
                    ResourceKind::Energy => {
                        if let Some((pc, max)) = u.pools[crate::PoolKind::Energy].as_mut() {
                            *pc = (*pc - amount).max(0);
                            ctx.pool_events.push(crate::event::Event::PoolChanged {
                                unit: *actor,
                                pool: crate::PoolKind::Energy,
                                current: *pc,
                                max: *max,
                                cause: crate::PoolChangeCause::Spent,
                            });
                        }
                    }
                }
            }
            (vec![], ctx)
        }

        Effect::ApplyStatus {
            target,
            status,
            rounds,
            dot_per_tick,
            applier,
        } => {
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
            let mut ctx = ApplyCtx::default();
            if let Some(u) = state.unit_mut(*target) {
                if let Some((pc, pmax)) = u.pools[crate::PoolKind::Rage].as_mut() {
                    *pc = (*pc + 1).min(*pmax);
                    ctx.pool_events.push(crate::event::Event::PoolChanged {
                        unit: *target,
                        pool: crate::PoolKind::Rage,
                        current: *pc,
                        max: *pmax,
                        cause: crate::PoolChangeCause::Gained,
                    });
                }
            }
            (vec![], ctx)
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
                u.pools[crate::PoolKind::Hp].as_mut().unwrap().0 = 0;
            }

            let mut derived: Vec<Effect> = statuses_to_clean
                .into_iter()
                .map(|status| Effect::RemoveStatus {
                    target: *unit,
                    status,
                })
                .collect();

            let mut ctx = ApplyCtx::default();

            // If the dying actor held the current turn, force-end their turn.
            // Emitting TurnEnded here (via turn_skip_events) and deriving
            // AdvanceTurn causes the cascade to settle on the next alive actor.
            // step_inner's "current changed" check will then emit TurnStarted +
            // start_actor_turn for the new actor.
            if state.turn_queue.current() == Some(*unit) {
                ctx.turn_skip_events.push(crate::event::Event::TurnEnded {
                    actor: *unit,
                    cause: crate::event::TurnEndCause::DeathOfActor,
                });
                derived.push(Effect::AdvanceTurn);
            }

            (derived, ctx)
        }

        Effect::RefreshAggregates { unit } => {
            // Recompute speed, armor_bonus, and damage_taken_bonus from active
            // statuses + aura effects.
            // Reads status bonuses via ContentView — no Bevy dep in the engine.
            let unit_id = *unit;
            if let Some(u) = state.unit_mut(unit_id) {
                let mut speed_bonus: i32 = 0;
                let mut armor_bonus: i32 = 0;
                let mut damage_taken_bonus: i32 = 0;
                for s in &u.statuses {
                    let b = content.status_bonuses(&s.id);
                    speed_bonus += b.speed_bonus;
                    armor_bonus += b.armor_bonus;
                    damage_taken_bonus += b.damage_taken_bonus;
                }
                u.speed = u.base_speed + speed_bonus;
                u.armor_bonus = armor_bonus;
                u.damage_taken_bonus = damage_taken_bonus;
            }
            // Fold aura bonuses on top of status-derived aggregates.
            let aura_fx = state.aura_effects_on(unit_id, content);
            if let Some(u) = state.unit_mut(unit_id) {
                u.speed += aura_fx.speed_bonus;
                u.armor_bonus += aura_fx.armor_bonus;
                u.damage_taken_bonus += aura_fx.damage_taken_bonus;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::TickDot { target, status } => {
            let mut derived: Vec<Effect> = Vec::new();

            let (dot_per_tick, applier, max_hp) = {
                let Some(u) = state.unit(*target) else {
                    return (derived, ApplyCtx::default());
                };
                let Some(s) = u.statuses.iter().find(|s| s.id == *status) else {
                    return (derived, ApplyCtx::default());
                };
                (s.dot_per_tick, s.applier, u.max_hp())
            };

            let percent = content
                .status_def(status)
                .map(|sd| sd.hp_percent_dot)
                .unwrap_or(0);

            // Compute total raw DoT damage from both components (flat + percent).
            let flat_raw = if dot_per_tick > 0 {
                dot_per_tick as f32
            } else {
                0.0
            };
            let percent_raw = if percent > 0 {
                let amount = (max_hp * percent + 99) / 100;
                amount as f32
            } else {
                0.0
            };
            let total_raw = flat_raw + percent_raw;

            if total_raw > 0.0 {
                // DoT always pierces — no armor mitigation.
                let final_amount = total_raw.round() as i32;

                // Apply HP reduction directly (bypass Effect::Damage derivation so we
                // can fuse into a single DotDamaged event without emitting UnitDamaged).
                let hp_after = if let Some(u) = state.unit_mut(*target) {
                    let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                    p.0 = (p.0 - final_amount).max(0);
                    u.hp()
                } else {
                    0
                };

                // Derive the same cascade that Effect::Damage would have derived.
                // Env sources don't accumulate rage.
                if let crate::state::EffectSource::Unit(u) = applier {
                    derived.push(Effect::GainRage { target: u });
                }
                derived.push(Effect::GainRage { target: *target });

                let cur_max_hp = state.unit(*target).map(|u| u.max_hp()).unwrap_or(0);
                if let Some((phase_idx, _transition)) = state
                    .unit(*target)
                    .and_then(|u| u.check_phase_trigger(hp_after, cur_max_hp))
                {
                    derived.push(Effect::EnterPhase {
                        unit: *target,
                        phase_idx,
                    });
                } else if hp_after <= 0 {
                    derived.push(Effect::Death { unit: *target });
                }

                let ctx = ApplyCtx {
                    dot_damage: Some(DotDamageCtx {
                        source: applier,
                        source_status: status.clone(),
                        raw: total_raw,
                        mitigation: 0,
                        pierces: true,
                        final_amount,
                    }),
                    ..ApplyCtx::default()
                };
                (derived, ctx)
            } else {
                // Zero-damage tick (buff-only status): no HP change, no cascade.
                // effect_to_event will emit StatusTicked so the log records the tick.
                (derived, ApplyCtx::default())
            }
        }

        Effect::TickHeal { target, status } => {
            let (applier, hp_before, max_hp) = {
                let Some(u) = state.unit(*target) else {
                    return (vec![], ApplyCtx::default());
                };
                let Some(s) = u.statuses.iter().find(|s| s.id == *status) else {
                    return (vec![], ApplyCtx::default());
                };
                (s.applier, u.hp(), u.max_hp())
            };

            let heal = content
                .status_def(status)
                .map(|sd| sd.heal_per_tick)
                .unwrap_or(0);

            if heal > 0 && hp_before < max_hp {
                let hp_after = if let Some(u) = state.unit_mut(*target) {
                    let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                    p.0 = (p.0 + heal).min(p.1);
                    u.hp()
                } else {
                    hp_before
                };

                let actual_restored = hp_after - hp_before;
                let ctx = ApplyCtx {
                    hot_heal: Some(HotHealCtx {
                        source: applier,
                        source_status: status.clone(),
                        amount: actual_restored,
                    }),
                    ..ApplyCtx::default()
                };
                (vec![], ctx)
            } else {
                // Zero-heal tick (heal_per_tick == 0 or already at max HP): no-op.
                // effect_to_event will emit StatusTicked so the log records the tick.
                (vec![], ApplyCtx::default())
            }
        }

        Effect::ExpireStatus { target, status } => {
            let expired = if let Some(u) = state.unit_mut(*target) {
                if let Some(s) = u.statuses.iter_mut().find(|s| s.id == *status) {
                    // Permanent statuses (PERMANENT_DURATION sentinel) never expire.
                    if s.rounds_remaining == crate::PERMANENT_DURATION {
                        return (vec![], ApplyCtx::default());
                    }
                    s.rounds_remaining = s.rounds_remaining.saturating_sub(1);
                    s.rounds_remaining == 0
                } else {
                    false
                }
            } else {
                false
            };

            let derived = if expired {
                vec![Effect::RemoveStatus {
                    target: *target,
                    status: status.clone(),
                }]
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
            let prev_max_hp = state.unit(*unit).map(|u| u.max_hp()).unwrap_or(0);

            // Re-call check_phase_trigger with current hp/max_hp to recover the
            // transition data.  Bridge's `EnemyPhases.pending` pop happens later
            // (in `apply_phase_ecs_writes`), but the engine state's own
            // `enemy_phases` must be popped HERE — otherwise `check_phase_trigger`
            // keeps returning the same first entry on every subsequent damage,
            // causing the same phase (with heal_to_full) to re-fire indefinitely.
            let transition = state
                .unit(*unit)
                .and_then(|u| u.check_phase_trigger(u.hp(), prev_max_hp))
                .map(|(_, t)| t);

            // Capture tag replacement before consuming the transition.
            let new_tags = transition.as_ref().and_then(|t| t.tags.clone());

            // Consume the just-triggered phase entry from engine state.
            if let Some(u) = state.unit_mut(*unit) {
                if !u.enemy_phases.is_empty() {
                    u.enemy_phases.remove(0);
                }
            }

            // Apply tag REPLACE if the phase carries a new tag-set.
            // Done in-arm, before derived stat effects, so it lands between
            // step.rs's before/after aura-membership snapshots.
            if let Some(tags) = new_tags {
                if let Some(u) = state.unit_mut(*unit) {
                    u.tags = tags;
                }
            }

            let (new_max_hp, new_armor, new_base_speed, heal_to_full) = transition
                .map(|t| (t.new_max_hp, t.new_armor, t.new_base_speed, t.heal_to_full))
                .unwrap_or((prev_max_hp, 0, 0, false));

            let mut derived: Vec<Effect> = vec![Effect::SetMaxHp {
                unit: *unit,
                max_hp: new_max_hp,
            }];
            if new_armor != 0 {
                derived.push(Effect::SetArmor {
                    unit: *unit,
                    armor: new_armor,
                });
            }
            if new_base_speed != 0 {
                derived.push(Effect::SetBaseSpeed {
                    unit: *unit,
                    base_speed: new_base_speed,
                });
            }
            if heal_to_full {
                derived.push(Effect::Heal {
                    target: *unit,
                    amount: new_max_hp,
                });
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
                let p = u.pools[crate::PoolKind::Hp].as_mut().unwrap();
                p.1 = *max_hp;
                p.0 = p.0.min(p.1);
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

        // ── Passive reveal effects ────────────────────────────────────────────
        Effect::RevealEnvInRange { caster, range } => {
            // Read caster position and team; bail if caster is unknown.
            let (caster_pos, caster_team) = match state.unit(*caster) {
                Some(u) => (u.pos, u.team),
                None => return (vec![], ApplyCtx::default()),
            };
            let targets = scan_revealable_in_range(state, caster_pos, *range, caster_team);
            let derived: Vec<Effect> = targets
                .into_iter()
                .map(|id| Effect::RevealEnv {
                    id,
                    revealer: caster_team,
                })
                .collect();
            (derived, ApplyCtx::default())
        }

        Effect::RevealEnv { id, revealer } => {
            // Insert revealer's team into revealed_to; idempotent per team.
            let changed = if let Some(e) = state.environment.iter_mut().find(|e| e.id == *id) {
                if !e.revealed_to.contains(*revealer) {
                    e.revealed_to.insert(*revealer);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            (
                vec![],
                ApplyCtx {
                    env_revealed: changed,
                    ..ApplyCtx::default()
                },
            )
        }

        Effect::BumpRound => {
            state.round += 1;
            // start_round resets index=0, sets phase=ActorTurn, resets
            // reactions_left=reactions_max for alive units.
            state.start_round(content);

            // Insert mid-combat summons into the turn order at their rolled
            // initiative position. Must run AFTER start_round (which resets
            // index=0) and BEFORE skip_or_settle_current (which settles the
            // index-0 actor against the final order).
            state.reconcile_turn_order();

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

        Effect::Spawn {
            summoner,
            template_id,
            max_active,
        } => {
            let template = match content.unit_template(template_id) {
                Some(t) => t,
                None => {
                    return (
                        vec![],
                        ApplyCtx {
                            spawn_blocked: Some(SpawnBlockedReason::TemplateMissing),
                            ..ApplyCtx::default()
                        },
                    )
                }
            };

            let (summoner_pos, summoner_team) = match state.unit(*summoner) {
                Some(u) => (u.pos, u.team),
                None => {
                    return (
                        vec![],
                        ApplyCtx {
                            spawn_blocked: Some(SpawnBlockedReason::TemplateMissing),
                            ..ApplyCtx::default()
                        },
                    )
                }
            };

            if let Some(cap) = max_active {
                let active = state
                    .alive_units()
                    .filter(|u| u.summoner == Some(*summoner))
                    .count() as u32;
                if active >= *cap {
                    return (
                        vec![],
                        ApplyCtx {
                            spawn_blocked: Some(SpawnBlockedReason::MaxActiveReached),
                            ..ApplyCtx::default()
                        },
                    );
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
                None => {
                    return (
                        vec![],
                        ApplyCtx {
                            spawn_blocked: Some(SpawnBlockedReason::NoFreePosition),
                            ..ApplyCtx::default()
                        },
                    )
                }
            };

            let new_uid = state.alloc_synthetic_uid();
            let mut new_unit = Unit::new(
                new_uid,
                summoner_team,
                pos,
                template.armor,
                0,
                0,
                template.base_speed,
                template.base_speed,
                0,
                1,
                Vec::new(),
                Some(*summoner),
                None, // initiative: not yet rolled
                template.caster_context.clone(),
                template.aoo_dice,
                template.auras.clone(),
                template.enemy_phases.clone(),
                enum_map::enum_map! {
                    crate::PoolKind::Hp     => crate::state::template_starting_pool(&template, crate::PoolKind::Hp),
                    crate::PoolKind::Mana   => crate::state::template_starting_pool(&template, crate::PoolKind::Mana),
                    crate::PoolKind::Rage   => crate::state::template_starting_pool(&template, crate::PoolKind::Rage),
                    crate::PoolKind::Energy => crate::state::template_starting_pool(&template, crate::PoolKind::Energy),
                    crate::PoolKind::Ap     => crate::state::template_starting_pool(&template, crate::PoolKind::Ap),
                    crate::PoolKind::Mp     => crate::state::template_starting_pool(&template, crate::PoolKind::Mp),
                },
                template.regen_per_pool,
                // Track template id so initial_statuses can be applied below
                // (parity with bootstrap path via apply_template_initial_statuses).
                Some(template_id.clone()),
            );
            new_unit.tags = template.tags.clone();

            state.insert_unit(new_unit);

            // Apply `template.initial_statuses` to the freshly spawned unit
            // (e.g. permanent stun on a summoned non-acting ally). Mirrors the
            // bootstrap path in `CombatState::apply_initial_statuses`.
            if let Some(unit) = state.unit_mut(new_uid) {
                crate::state::apply_template_initial_statuses(unit, &template);
            }

            (
                vec![],
                ApplyCtx {
                    spawn_uid: Some(new_uid),
                    spawn_pos: Some(pos),
                    ..ApplyCtx::default()
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{ContentView, StatusBonuses};
    use crate::state::UnitId;
    use crate::state::{ActiveStatus, CombatState, EffectSource, Team};
    use crate::{AbilityDef, AbilityId, PoolKind, RegenRule, StatusDef, StatusId};
    use enum_map::enum_map;
    use hexx::Hex;

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn make_unit_hp(id: UnitId, hp: i32, max_hp: i32) -> crate::state::Unit {
        crate::state::Unit::new(
            id,
            Team::Player,
            Hex::ZERO,
            0,
            0,
            0,
            3,
            3,
            1,
            1,
            vec![],
            None,
            None,
            Default::default(),
            None,
            Vec::new(),
            Vec::new(),
            enum_map! {
                PoolKind::Hp     => Some((hp, max_hp)),
                PoolKind::Mana   => None,
                PoolKind::Rage   => None,
                PoolKind::Energy => None,
                PoolKind::Ap     => Some((1, 2)),
                PoolKind::Mp     => Some((3, 3)),
            },
            enum_map! {
                PoolKind::Hp     => RegenRule::None,
                PoolKind::Mana   => RegenRule::None,
                PoolKind::Rage   => RegenRule::None,
                PoolKind::Energy => RegenRule::None,
                PoolKind::Ap     => RegenRule::RefillToMax,
                PoolKind::Mp     => RegenRule::RefillToMax,
            },
            None,
        )
    }

    /// ContentView stubs for HoT tests — one for `heal_per_tick = 4` and one for 0.
    struct HotContent4;
    struct HotContent0;

    static HOT_DEF_4: StatusDef = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses {
            speed_bonus: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
        },
        hp_percent_dot: 0,
        heal_per_tick: 4,
    };
    static HOT_DEF_10: StatusDef = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses {
            speed_bonus: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
        },
        hp_percent_dot: 0,
        heal_per_tick: 10,
    };
    static HOT_DEF_0: StatusDef = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses {
            speed_bonus: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
        },
        hp_percent_dot: 0,
        heal_per_tick: 0,
    };

    impl ContentView for HotContent4 {
        fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> {
            None
        }
        fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
            Some(&HOT_DEF_4)
        }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> {
            None
        }
    }
    impl ContentView for HotContent0 {
        fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> {
            None
        }
        fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
            Some(&HOT_DEF_0)
        }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> {
            None
        }
    }

    /// ContentView that returns heal_per_tick = 10 (for clamping test).
    struct HotContent10;
    impl ContentView for HotContent10 {
        fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> {
            None
        }
        fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
            Some(&HOT_DEF_10)
        }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> {
            None
        }
    }

    fn make_state_with_unit(uid: UnitId, hp: i32, max_hp: i32) -> CombatState {
        let unit = make_unit_hp(uid, hp, max_hp);
        CombatState::new(vec![unit], 1, crate::state::RoundPhase::ActorTurn, 0)
    }

    fn add_hot_status(state: &mut CombatState, target: UnitId, applier: UnitId, status_id: &str) {
        if let Some(u) = state.unit_mut(target) {
            u.statuses.push(ActiveStatus {
                id: StatusId(status_id.into()),
                rounds_remaining: 2,
                dot_per_tick: 0,
                applier: EffectSource::Unit(applier),
            });
        }
    }

    // ── TickHeal tests ────────────────────────────────────────────────────────

    #[test]
    fn tick_heal_restores_hp() {
        let uid = UnitId(1);
        let mut state = make_state_with_unit(uid, 6, 10);
        add_hot_status(&mut state, uid, uid, "hot");

        let eff = Effect::TickHeal {
            target: uid,
            status: StatusId("hot".into()),
        };
        let (derived, ctx) = apply_effect(&mut state, &eff, &HotContent4);

        assert!(
            derived.is_empty(),
            "TickHeal must not derive secondary effects"
        );
        let hot = ctx.hot_heal.expect("hot_heal should be set");
        assert_eq!(hot.amount, 4);
        assert_eq!(state.unit(uid).unwrap().hp(), 10);
    }

    #[test]
    fn tick_heal_clamps_to_max_hp() {
        // heal_per_tick = 10, only 2 deficit → actual restore = 2
        let uid = UnitId(1);
        let mut state = make_state_with_unit(uid, 8, 10);
        add_hot_status(&mut state, uid, uid, "hot");

        let eff = Effect::TickHeal {
            target: uid,
            status: StatusId("hot".into()),
        };
        let (_, ctx) = apply_effect(&mut state, &eff, &HotContent10);

        let hot = ctx.hot_heal.expect("hot_heal should be set");
        assert_eq!(hot.amount, 2);
        assert_eq!(state.unit(uid).unwrap().hp(), 10);
    }

    #[test]
    fn tick_heal_zero_when_already_at_max() {
        let uid = UnitId(1);
        let mut state = make_state_with_unit(uid, 10, 10); // full HP
        add_hot_status(&mut state, uid, uid, "hot");

        let eff = Effect::TickHeal {
            target: uid,
            status: StatusId("hot".into()),
        };
        let (_, ctx) = apply_effect(&mut state, &eff, &HotContent4);

        assert!(ctx.hot_heal.is_none(), "no-op when already at max HP");
        assert_eq!(state.unit(uid).unwrap().hp(), 10);
    }

    #[test]
    fn tick_heal_noop_when_heal_per_tick_zero() {
        let uid = UnitId(1);
        let mut state = make_state_with_unit(uid, 5, 10);
        add_hot_status(&mut state, uid, uid, "hot");

        let eff = Effect::TickHeal {
            target: uid,
            status: StatusId("hot".into()),
        };
        let (_, ctx) = apply_effect(&mut state, &eff, &HotContent0);

        assert!(ctx.hot_heal.is_none());
        assert_eq!(
            state.unit(uid).unwrap().hp(),
            5,
            "HP unchanged for zero-heal tick"
        );
    }

    #[test]
    fn tick_heal_never_derives_death_or_phase() {
        // Even at low HP a heal tick must never produce Death/EnterPhase.
        let uid = UnitId(1);
        let mut state = make_state_with_unit(uid, 1, 10);
        add_hot_status(&mut state, uid, uid, "hot");

        let eff = Effect::TickHeal {
            target: uid,
            status: StatusId("hot".into()),
        };
        let (derived, _) = apply_effect(&mut state, &eff, &HotContent4);

        for d in &derived {
            assert!(
                !matches!(d, Effect::Death { .. } | Effect::EnterPhase { .. }),
                "TickHeal must not derive Death or EnterPhase, got: {d:?}"
            );
        }
    }
}
