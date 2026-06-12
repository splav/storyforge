//! Pure simulation of plan steps against a cloned battle snapshot.
//!
//! Steps go through the `combat_engine::step()` function directly — no
//! separate hand-rolled damage math.  `SimState` holds a persistent
//! `CombatState` (built once in `from_snapshot` directly from `snap.state`,
//! mutated in-place by each `step()` call).
//!
//! `snapshot.units` and `snapshot.state` are **frozen at construction** and
//! are never updated after steps (U4-cleanup invariant).  Read post-step
//! unit state via `sim.unit(entity)` / `sim.actor_unit()` / `sim.enemies_of()`,
//! which pull from `combat_state` directly.  Engine `CombatState`
//! (`combat_state`) is the sole mutable working copy.

use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::world::tags::StatusTagCache;
use crate::content::abilities::{AbilityDef, CasterContext};
use crate::content::content_view::ContentView;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

use crate::combat::bridge::entity_to_uid;
use combat_engine::{
    action::Action,
    content::{ContentView as EngineContentView, StatusBonuses as EngineStatusBonuses},
    dice::ExpectedValue as EngineExpectedValue,
    state::{CombatState, Team, UnitId},
    step::step,
};

use super::types::{PlanStep, StepOutcome};

/// Mutable working copy of a snapshot for plan search.
///
/// Each `apply_step` call goes through `combat_engine::step()`, mutating
/// `combat_state` in place.  Read post-step unit state via `sim.unit(entity)`
/// or `sim.actor_unit()` — these read from `combat_state` directly, not from
/// `snapshot.state`.
///
/// **U4-cleanup invariant:** `snapshot.state` is frozen at construction and is
/// never updated after steps.  `snapshot.units` is also frozen (U4 invariant).
/// Derived AI fields (`threat`, `max_attack_range`, `tags`, `aoo_expected_damage`,
/// etc.) are read-only caches from decision-entry time and are intentionally
/// stale for the duration of a search branch.
///
/// `status_tags` is retained for `SnapshotContentView` construction.  Tests
/// that don't exercise status resolution can pass `empty_status_tag_cache()`.
pub struct SimState<'a> {
    pub snapshot: BattleSnapshot,
    /// Engine authoritative state.  Built once in `from_snapshot` directly
    /// from `snap.state`, mutated in-place by every `step()` call.
    /// This is the sole source of post-step mutable truth.
    pub combat_state: CombatState,
    pub actor: Entity,
    pub status_tags: &'a StatusTagCache,
}

impl<'a> SimState<'a> {
    pub fn from_snapshot(
        snap: &BattleSnapshot,
        actor: Entity,
        status_tags: &'a StatusTagCache,
    ) -> Self {
        let combat_state = snap.state.clone();
        Self {
            snapshot: snap.clone(),
            combat_state,
            actor,
            status_tags,
        }
    }

    /// Live actor view — `None` if the actor died mid-plan. Snapshot
    /// now keeps corpses in `units` (for death-triggered effects, replay),
    /// so the "is_alive" filter lives here rather than implicitly in a
    /// retain'd vec. Planning callers that terminate on actor death see
    /// `None` as before.
    pub fn actor_unit(&self) -> Option<UnitView<'_>> {
        self.unit(self.actor).filter(|u| u.is_alive())
    }

    /// Read the post-step state for any unit by Bevy `Entity`.
    ///
    /// Reads from `combat_state` (authoritative post-step) + `snapshot.cache`
    /// (frozen at construction).  Mirrors `BattleSnapshot::unit()` but always
    /// returns the current engine state, not the (frozen) `snapshot.state`.
    ///
    /// Returns `Some` even for dead units (corpses retained by the engine) —
    /// callers that want only alive units should use `actor_unit()` or
    /// filter with `.filter(|u| u.is_alive())`.
    pub fn unit(&self, entity: Entity) -> Option<UnitView<'_>> {
        let uid = self.snapshot.uid_for_entity(entity)?;
        let state = self.combat_state.unit(uid)?;
        let cache = self.snapshot.cache.unit(entity)?;
        Some(UnitView { state, cache })
    }

    /// Live enemies of `team` from the current (post-step) engine state.
    ///
    /// Mirrors `BattleSnapshot::enemies_of()` but reads from `combat_state`.
    pub fn enemies_of(&self, team: Team) -> impl Iterator<Item = UnitView<'_>> {
        self.combat_state.units().iter().filter_map(move |u| {
            if u.team == team || u.hp() <= 0 {
                return None;
            }
            let entity = self.snapshot.entity_for_uid(u.id)?;
            let cache = self.snapshot.cache.unit(entity)?;
            Some(UnitView { state: u, cache })
        })
    }

    /// Consume `self` and return a `BattleSnapshot` whose `state` reflects the
    /// current (post-step) engine state.
    ///
    /// Used by callers that need to hand off a `BattleSnapshot` object — for
    /// example, parity tests that compare engine vs sim over a returned snapshot.
    /// For read access within sim code, prefer `sim.unit(entity)` directly.
    pub fn into_snapshot(mut self) -> BattleSnapshot {
        self.snapshot.state = self.combat_state;
        self.snapshot
    }

    /// Apply one plan step to the simulated state via engine `step()`,
    /// returning per-step effects. Disadvantage is derived inside the
    /// engine from the actual cast distance; the `disadvantage` parameter
    /// is kept for call-site compatibility but no longer feeds the roll.
    pub fn apply_step(
        &mut self,
        step: &PlanStep,
        caster_ctx: &CasterContext,
        content: &ContentView,
        disadvantage: bool,
    ) -> StepOutcome {
        // Safety net: if the actor is already dead on entry, nothing to apply.
        // Normally the generator terminates branches after a lethal step, so
        // this should not be reached. Kept as a defence against future callers.
        if self.actor_unit().is_none() {
            return StepOutcome::default();
        }
        match step {
            PlanStep::Move { path } => self.apply_move(path),
            PlanStep::Cast {
                ability,
                target,
                target_pos,
            } => {
                let Some(def) = content.abilities.get(ability) else {
                    return StepOutcome::default();
                };
                self.apply_cast(def, *target, *target_pos, caster_ctx, content, disadvantage)
            }
        }
    }

    /// Advance time past the actor's turn end: tick active statuses (DoT damage,
    /// duration decrements, expiry) for the actor.
    ///
    /// After U4-cleanup: `combat_state` is the sole mutable source of truth.
    /// `snapshot.state` is frozen at construction; read post-tick values via
    /// `sim.unit(entity)` or `sim.actor_unit()`.
    /// `snapshot.units` is frozen at construction (U4 invariant).
    ///
    /// **Single-tick invariant:** call exactly once per branch expansion — never
    /// inside the step loop for multi-step plans.  `generate_plans` enforces this
    /// by calling `apply_endturn` immediately before storing the branch snapshot,
    /// so each stored snapshot is already post-endturn.  The next depth's
    /// `from_snapshot` starts from that already-ticked base, avoiding double-tick.
    pub fn apply_endturn(&mut self, actor: UnitId) {
        let content_view = SnapshotContentView::from_snapshot(&self.snapshot);
        let _ = self.combat_state.tick_actor_statuses(actor, &content_view);
    }

    fn apply_move(&mut self, path: &[Hex]) -> StepOutcome {
        let mut outcome = StepOutcome {
            moved: true,
            ..Default::default()
        };
        if path.is_empty() {
            return outcome;
        }

        let Some(actor_snap) = self.actor_unit() else {
            return outcome;
        };
        let actor_hp_before = actor_snap.hp();
        let actor_uid = entity_to_uid(self.actor);

        // ContentView still builds per-call from snapshot (cheap; tied to
        // snapshot's AoO expected damage cache).
        let content_view = SnapshotContentView::from_snapshot(&self.snapshot);

        let action = Action::Move {
            actor: actor_uid,
            path: path.to_vec(),
        };
        let step_result = step(
            &mut self.combat_state,
            action,
            &mut EngineExpectedValue,
            &content_view,
        );

        match step_result {
            Ok((_events, _ctx)) => {
                // combat_state is already updated in-place by step().
                // No sync needed — read post-step values via sim.unit(entity).
            }
            Err(_) => {
                // Engine rolled back; no changes to project.  Plan branch
                // terminates on next call via actor_unit() returning None.
                return outcome;
            }
        }

        let actor_hp_after = self.actor_unit().map(|u| u.hp()).unwrap_or(0);
        let hp_delta = (actor_hp_before - actor_hp_after).max(0);
        outcome.self_damage = hp_delta as f32;

        outcome
    }

    fn apply_cast(
        &mut self,
        def: &AbilityDef,
        target: Entity,
        target_pos: Hex,
        _caster_ctx: &CasterContext,
        content: &ContentView,
        _disadvantage: bool,
    ) -> StepOutcome {
        let mut outcome = StepOutcome::default();

        if self.actor_unit().is_none() {
            return outcome;
        }

        let actor_uid = entity_to_uid(self.actor);
        let target_uid = entity_to_uid(target);
        let actor_hp_before = self.actor_unit().map(|u| u.hp()).unwrap_or(0);

        // Build a content view enriched with ability + status defs so the
        // engine's legality check (`check_legality`) can resolve `ability_def`.
        let content_view = SnapshotContentView::with_content(&self.snapshot, content);

        let action = Action::Cast {
            actor: actor_uid,
            ability: def.id.clone(),
            target: target_uid,
            target_pos,
        };

        let step_result = step(
            &mut self.combat_state,
            action,
            &mut EngineExpectedValue,
            &content_view,
        );

        match step_result {
            Ok((events, _ctx)) => {
                // combat_state is already updated in-place by step().
                // No sync needed — read post-step values via sim.unit(entity).

                // ── Populate StepOutcome from engine events ────────────────────
                use combat_engine::event::Event;
                for ev in &events {
                    match ev {
                        Event::UnitDamaged {
                            target: t_uid,
                            amount,
                            ..
                        } => {
                            outcome.hits += 1;
                            outcome.damage += *amount as f32;
                            // killed: unit is dead in engine state post-step.
                            if let Some(eu) = self.combat_state.unit(*t_uid) {
                                if !eu.is_alive() {
                                    // Map UnitId → Entity via explicit uid_to_entity map.
                                    if let Some(ent) = self.snapshot.entity_for_uid(*t_uid) {
                                        if !outcome.killed.contains(&ent) {
                                            outcome.killed.push(ent);
                                        }
                                    }
                                }
                            }
                        }
                        Event::UnitHealed { amount, .. } => {
                            outcome.hits += 1;
                            outcome.heal += *amount as f32;
                        }
                        Event::StatusApplied {
                            target: t_uid,
                            status,
                        } => {
                            // Check skips_turn via the content view's status_def.
                            let skips = content
                                .statuses
                                .get(status.0.as_str())
                                .is_some_and(|sd| sd.skips_turn);
                            if skips {
                                // Map UnitId → Entity via explicit uid_to_entity map.
                                if let Some(ent) = self.snapshot.entity_for_uid(*t_uid) {
                                    if !outcome.stunned.contains(&ent) {
                                        outcome.stunned.push(ent);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // self_damage: actor HP lost this step (e.g. from crit-fail SelfDamage).
                let actor_hp_after = self.actor_unit().map(|u| u.hp()).unwrap_or(0);
                let hp_delta = (actor_hp_before - actor_hp_after).max(0);
                outcome.self_damage = hp_delta as f32;
            }
            Err(_) => {
                // Engine rolled back (legality rejection).  Snapshot unchanged.
            }
        }

        outcome
    }
}

// ── Engine shim helpers ───────────────────────────────────────────────────────

/// `ContentView` adapter for the AI sim (5c.1: static content only).
///
/// Per-combat state (caster contexts, AoO dice) lives on engine `Unit`
/// fields in `SimState.combat_state` (set at construction from `snap.state`).
/// This struct only carries static content: ability and status definitions
/// for engine legality.
struct SnapshotContentView {
    /// Engine-format ability definitions (populated for Cast steps).
    abilities: std::collections::HashMap<combat_engine::AbilityId, combat_engine::AbilityDef>,
    /// Engine-format status definitions (populated for Cast steps).
    statuses: std::collections::HashMap<combat_engine::StatusId, combat_engine::StatusDef>,
}

impl SnapshotContentView {
    fn from_snapshot(_snap: &BattleSnapshot) -> Self {
        Self {
            abilities: std::collections::HashMap::new(),
            statuses: std::collections::HashMap::new(),
        }
    }

    /// Full constructor for Cast steps: populates ability + status definitions
    /// from the Bevy `ContentView` so that engine legality can resolve them.
    fn with_content(_snap: &BattleSnapshot, content: &ContentView) -> Self {
        let abilities = content
            .abilities
            .iter()
            .map(|(id, def)| (id.clone(), crate::content::to_engine::ability_def(def)))
            .collect();

        let statuses = content
            .statuses
            .iter()
            .map(|(id, def)| (id.clone(), crate::content::to_engine::status_def(def)))
            .collect();

        Self {
            abilities,
            statuses,
        }
    }
}

impl EngineContentView for SnapshotContentView {
    fn status_bonuses(&self, id: &combat_engine::StatusId) -> EngineStatusBonuses {
        self.statuses
            .get(id)
            .map(|def| EngineStatusBonuses {
                armor_bonus: def.bonuses.armor_bonus,
                speed_bonus: def.bonuses.speed_bonus,
                damage_taken_bonus: def.bonuses.damage_taken_bonus,
            })
            .unwrap_or_default()
    }

    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<&combat_engine::AbilityDef> {
        self.abilities.get(id)
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<&combat_engine::StatusDef> {
        self.statuses.get(id)
    }

    fn unit_template(&self, _id: &str) -> Option<combat_engine::UnitTemplate> {
        // Sim never generates summons; return None.
        None
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "sim_tests.rs"]
mod tests;
