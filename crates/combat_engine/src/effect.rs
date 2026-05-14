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
use crate::state::{ActiveStatus, CombatState, UnitId};
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
#[derive(Debug, Clone)]
pub enum Effect {
    /// Move the actor's position to `to`.
    MovePosition { actor: UnitId, to: Hex },
    /// Deduct `by` movement points from the actor.
    DecrementMP { actor: UnitId, by: i32 },
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
}

/// Side-channel context produced by `apply_effect` for event generation.
///
/// Some events need data that isn't recoverable from `state` after the effect
/// has been applied (e.g. the previous position before `MovePosition`).
/// Rather than reading state twice, the driver captures this before calling
/// `apply_effect`, or `apply_effect` computes and returns it here.
#[derive(Debug, Default)]
pub struct ApplyCtx {
    /// Set by `Damage`: final post-mitigation damage actually dealt.
    pub final_damage: Option<f32>,
    /// Set by `Heal`: actual HP restored (after DoT-neutralization consumes
    /// some of the heal pool).  May be < `Heal.amount` if DoTs consumed
    /// part or all of the pool.
    pub heal_amount: Option<i32>,
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

            // Derive: GainRage{source}, GainRage{target}, Death{target} — in that order.
            let mut derived = vec![
                Effect::GainRage { target: *source },
                Effect::GainRage { target: *target },
            ];
            if hp_after <= 0 {
                derived.push(Effect::Death { unit: *target });
            }

            (derived, ApplyCtx { final_damage: Some(final_dmg), ..ApplyCtx::default() })
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
            // Mark as dead (hp already set to 0 by the preceding Damage effect).
            // No status removal on death — matches current ECS behavior which
            // only inserts Dead component; statuses expire normally via tick.
            if let Some(u) = state.unit_mut(*unit) {
                u.hp = 0;
            }
            (vec![], ApplyCtx::default())
        }

        Effect::RefreshAggregates { unit } => {
            // Recompute speed and armor_bonus from active statuses.
            // Reads status bonuses via ContentView — no Bevy dep in the engine.
            if let Some(u) = state.unit_mut(*unit) {
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
            (vec![], ApplyCtx::default())
        }
    }
}
