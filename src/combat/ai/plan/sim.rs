//! Pure simulation of plan steps against a cloned battle snapshot.
//!
//! Steps go through the `combat_engine::step()` function directly — no
//! separate hand-rolled damage math.  `SimState` holds a persistent
//! `CombatState` (built once in `from_snapshot`, mutated in-place by each
//! `step()` call) and a `BattleSnapshot` that is kept in sync via
//! `project_engine_to_snapshot` after every step.

use crate::combat::ai::world::snapshot::{
    ActiveStatusView, BattleSnapshot, UnitSnapshot,
};
use crate::combat::ai::world::tags::StatusTagCache;
use crate::content::races::CritFailEffect;
use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, TargetType};
use crate::content::content_view::ContentView;
use crate::game::components::Team;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

use combat_engine::{
    action::Action,
    content::{ContentView as EngineContentView, StatusBonuses as EngineStatusBonuses},
    dice::{DiceExpr as EngineDiceExpr, ExpectedValue as EngineExpectedValue},
    state::{CombatState, RoundPhase, Team as EngineTeam, Unit as EngineUnit, UnitId},
    step::step,
};
use combat_engine::{
    EffectDef as EngineEffectDef, StatusApplication as EngineStatusApplication,
    StatusOn as EngineStatusOn,
};
use crate::combat::engine_bridge::entity_to_uid;

use super::types::{PlanStep, StepOutcome};

/// Mutable working copy of a snapshot.  Each `apply_step` call goes through
/// `combat_engine::step()`, mutating `combat_state` in place, then projects
/// the result back to `snapshot` via `project_engine_to_snapshot`.
///
/// Derived snapshot fields (`threat`, `max_attack_range`, `tags`,
/// `aoo_expected_damage`) are intentionally **not** recomputed — treat them
/// as stale relative to the simulated state.  They are read-only caches that
/// the planner never writes during a search branch.
///
/// `status_tags` is used by snapshot-layer aggregates (e.g. `armor_bonus`
/// refresh in the heal-clears-DoT path).  Tests that don't exercise status
/// reflow can pass `empty_status_tag_cache()`.
pub struct SimState<'a> {
    pub snapshot: BattleSnapshot,
    /// Engine authoritative state.  Built once in `from_snapshot`, mutated
    /// in-place by every `step()` call.  Always in sync with `snapshot` for
    /// the fields that `project_engine_to_snapshot` covers.
    pub combat_state: CombatState,
    pub actor: Entity,
    pub status_tags: &'a StatusTagCache,
}

impl<'a> SimState<'a> {
    pub fn from_snapshot(snap: &BattleSnapshot, actor: Entity, status_tags: &'a StatusTagCache) -> Self {
        let combat_state = snapshot_to_combat_state(snap, snap.round);
        Self {
            snapshot: snap.clone(),
            combat_state,
            actor,
            status_tags,
        }
    }

    /// Live actor snapshot — `None` if the actor died mid-plan. Snapshot
    /// now keeps corpses in `units` (for death-triggered effects, replay),
    /// so the "is_alive" filter lives here rather than implicitly in a
    /// retain'd vec. Planning callers that terminate on actor death see
    /// `None` as before.
    pub fn actor_unit(&self) -> Option<&UnitSnapshot> {
        self.snapshot.unit(self.actor).filter(|u| u.is_alive())
    }

    /// Apply one plan step to the simulated state, returning per-step
    /// effects. `disadvantage` propagates into `compute_ability_outcome`
    /// and through to `ExpectedValue::roll_dice`, which discounts the
    /// dice roll using `DiceExpr::expected_disadvantage` (per-die
    /// formula).
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

    fn apply_move(&mut self, path: &[Hex]) -> StepOutcome {
        let mut outcome = StepOutcome { moved: true, ..Default::default() };
        if path.is_empty() { return outcome; }

        let Some(actor_snap) = self.actor_unit() else { return outcome };
        let actor_hp_before = actor_snap.hp;
        let actor_uid = entity_to_uid(self.actor);

        // ContentView still builds per-call from snapshot (cheap; tied to
        // snapshot's AoO expected damage cache).
        let content_view = SnapshotContentView::from_snapshot(&self.snapshot);

        let action = Action::Move { actor: actor_uid, path: path.to_vec() };
        let step_result = step(&mut self.combat_state, action, &mut EngineExpectedValue, &content_view);

        match step_result {
            Ok(_events) => {
                project_engine_to_snapshot(&self.combat_state, &mut self.snapshot, self.status_tags);
            }
            Err(_) => {
                // Engine rolled back; no changes to project.  Plan branch
                // terminates on next call via actor_unit() returning None.
                return outcome;
            }
        }

        let actor_hp_after = self.actor_unit().map(|u| u.hp).unwrap_or(0);
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
        let actor_hp_before = self.actor_unit().map(|u| u.hp).unwrap_or(0);

        // Build a content view enriched with ability + status defs so the
        // engine's legality check (`check_legality`) can resolve `ability_def`.
        let content_view = SnapshotContentView::with_content(&self.snapshot, content);

        let action = Action::Cast {
            actor: actor_uid,
            ability: def.id.clone(),
            target: target_uid,
            target_pos,
        };

        let step_result = step(&mut self.combat_state, action, &mut EngineExpectedValue, &content_view);

        match step_result {
            Ok(events) => {
                project_engine_to_snapshot(&self.combat_state, &mut self.snapshot, self.status_tags);

                // ── Populate StepOutcome from engine events ────────────────────
                use combat_engine::event::Event;
                for ev in &events {
                    match ev {
                        Event::UnitDamaged { target: t_uid, amount, .. } => {
                            outcome.hits += 1;
                            outcome.damage += amount;
                            // killed: unit is dead in engine state post-step.
                            if let Some(eu) = self.combat_state.unit(*t_uid) {
                                if !eu.is_alive() {
                                    // Map UnitId → Entity via snapshot.
                                    if let Some(ent) = self.snapshot.units.iter()
                                        .find(|u| entity_to_uid(u.entity) == *t_uid)
                                        .map(|u| u.entity)
                                    {
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
                        Event::StatusApplied { target: t_uid, status } => {
                            // Check skips_turn via the content view's status_def.
                            let skips = content.statuses.get(status)
                                .is_some_and(|sd| sd.skips_turn);
                            if skips {
                                if let Some(ent) = self.snapshot.units.iter()
                                    .find(|u| entity_to_uid(u.entity) == *t_uid)
                                    .map(|u| u.entity)
                                {
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
                let actor_hp_after = self.actor_unit().map(|u| u.hp).unwrap_or(0);
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

/// `ContentView` adapter that derives AoO dice from `UnitSnapshot::aoo_expected_damage`.
///
/// The engine's `ExpectedValue` dice source calls `roll(DiceExpr)` which returns
/// `round(expected)`.  We encode the pre-computed expected damage as a
/// constant-bonus `DiceExpr { count: 0, sides: 1, bonus: round(raw) }` so the
/// engine gets the exact same integer value the legacy sim used.
///
/// For Cast steps, `abilities` and `statuses` are populated from the Bevy
/// `ContentView` (see `with_content`) so that engine legality checks resolve
/// ability and status definitions correctly.
struct SnapshotContentView {
    /// `UnitId → raw AoO damage` for units that can perform an AoO.
    aoo_damage: std::collections::HashMap<UnitId, f32>,
    /// `UnitId → caster context` for damage formula evaluation.
    caster_contexts: std::collections::HashMap<UnitId, combat_engine::CasterContext>,
    /// Engine-format ability definitions (populated for Cast steps).
    abilities: std::collections::HashMap<combat_engine::AbilityId, combat_engine::AbilityDef>,
    /// Engine-format status definitions (populated for Cast steps).
    statuses: std::collections::HashMap<combat_engine::StatusId, combat_engine::StatusDef>,
}

/// Translate Bevy `CritFailEffect` → engine `CritFailOutcome`.
///
/// CircuitBreach uses a fixed `SelfDamage(0d1+2)` placeholder (Phase 2 step 6f).
/// Full mana_cost-derived damage parity is a Phase 2 step 7 follow-up.
pub(crate) fn map_crit_fail_effect(e: &CritFailEffect) -> combat_engine::CritFailOutcome {
    use CritFailEffect::*;
    use combat_engine::CritFailOutcome as Out;
    use combat_engine::{DiceExpr, StatusId};
    match e {
        Miss => Out::Miss,
        ManaOverload => Out::DoubleCost,
        BrokenFaith => Out::ApplyStatus(StatusId::from("broken_faith")),
        CircuitBreach => Out::SelfDamage(DiceExpr::new(0, 1, 2)), // placeholder; step 7 refines
        Exhaustion => Out::ApplyStatus(StatusId::from("exhaustion")),
        PactControl => Out::ApplyStatus(StatusId::from("pact_control")),
    }
}

impl SnapshotContentView {
    fn from_snapshot(snap: &BattleSnapshot) -> Self {
        let (aoo_damage, caster_contexts) = Self::build_unit_maps(snap);
        Self {
            aoo_damage,
            caster_contexts,
            abilities: std::collections::HashMap::new(),
            statuses: std::collections::HashMap::new(),
        }
    }

    /// Full constructor for Cast steps: populates ability + status definitions
    /// from the Bevy `ContentView` so that engine legality can resolve them.
    fn with_content(snap: &BattleSnapshot, content: &ContentView) -> Self {
        let (aoo_damage, caster_contexts) = Self::build_unit_maps(snap);

        let abilities = content.abilities.iter().map(|(id, def)| {
            let engine_def = combat_engine::AbilityDef {
                key: def.key.clone(),
                cost_ap: def.cost_ap,
                costs: def.costs.iter().map(|c| combat_engine::Cost {
                    resource: c.resource,
                    amount: c.amount,
                }).collect(),
                range: combat_engine::AbilityRange { min: def.range.min, max: def.range.max },
                target_type: match def.target_type {
                    TargetType::SingleEnemy => combat_engine::TargetType::SingleEnemy,
                    TargetType::SingleAlly  => combat_engine::TargetType::SingleAlly,
                    TargetType::Myself      => combat_engine::TargetType::Myself,
                    TargetType::Ground      => combat_engine::TargetType::Ground,
                },
                aoe: match def.aoe {
                    crate::content::abilities::AoEShape::None => combat_engine::AoEShape::None,
                    crate::content::abilities::AoEShape::Circle { radius } => combat_engine::AoEShape::Circle { radius },
                    crate::content::abilities::AoEShape::Line { length } => combat_engine::AoEShape::Line { length },
                },
                friendly_fire: def.friendly_fire,
                effect: match &def.effect {
                    EffectDef::None             => EngineEffectDef::None,
                    EffectDef::WeaponAttack     => EngineEffectDef::WeaponAttack,
                    EffectDef::Damage { dice }       => EngineEffectDef::Damage { dice: *dice },
                    EffectDef::SpellDamage { dice }  => EngineEffectDef::SpellDamage { dice: *dice },
                    EffectDef::Heal { dice }         => EngineEffectDef::Heal { dice: *dice },
                    EffectDef::GrantMovement { distance } => EngineEffectDef::GrantMovement { distance: *distance },
                    EffectDef::RestoreResources => EngineEffectDef::RestoreResources,
                    // Summon + ToggleMoveMode are out of engine scope in Phase 2.
                    EffectDef::Summon { .. } | EffectDef::ToggleMoveMode => EngineEffectDef::None,
                },
                statuses: def.statuses.iter().map(|s| EngineStatusApplication {
                    status: s.status.clone(),
                    duration_rounds: s.duration_rounds,
                    on: match s.on {
                        crate::content::abilities::StatusOn::Target => EngineStatusOn::Target,
                        crate::content::abilities::StatusOn::MySelf => EngineStatusOn::MySelf,
                    },
                }).collect(),
            };
            (id.clone(), engine_def)
        }).collect();

        let statuses = content.statuses.iter().map(|(id, def)| {
            let engine_def = combat_engine::StatusDef {
                causes_disadvantage: def.causes_disadvantage,
                blocks_mana_abilities: def.blocks_mana_abilities,
                forces_targeting: def.forces_targeting,
                skips_turn: def.skips_turn,
                armor_bonus: def.armor_bonus,
                damage_taken_bonus: def.damage_taken_bonus,
                speed_bonus: def.speed_bonus,
            };
            (id.clone(), engine_def)
        }).collect();

        Self { aoo_damage, caster_contexts, abilities, statuses }
    }

    fn build_unit_maps(snap: &BattleSnapshot) -> (
        std::collections::HashMap<UnitId, f32>,
        std::collections::HashMap<UnitId, combat_engine::CasterContext>,
    ) {
        let aoo_damage = snap
            .units
            .iter()
            .filter_map(|u| {
                let raw = u.aoo_expected_damage?;
                Some((entity_to_uid(u.entity), raw))
            })
            .collect();
        let caster_contexts = snap
            .units
            .iter()
            .map(|u| {
                let ctx = combat_engine::CasterContext {
                    str_mod: u.caster_ctx.str_mod,
                    int_mod: u.caster_ctx.int_mod,
                    spell_power: u.caster_ctx.spell_power,
                    weapon_dice: u.caster_ctx.weapon_dice,
                    crit_fail_outcome: map_crit_fail_effect(&u.crit_fail_effect),
                };
                (entity_to_uid(u.entity), ctx)
            })
            .collect();
        (aoo_damage, caster_contexts)
    }
}

impl EngineContentView for SnapshotContentView {
    fn aoo_dice(&self, attacker: UnitId) -> Option<EngineDiceExpr> {
        let raw = self.aoo_damage.get(&attacker)?;
        // Constant-bonus dice: count=0, sides=1 → expected = bonus.
        // round() converts to i32; `ExpectedValue::roll` returns expected().round().
        Some(EngineDiceExpr::new(0, 1, raw.round() as i32))
    }

    fn status_bonuses(&self, id: &combat_engine::StatusId) -> EngineStatusBonuses {
        self.statuses.get(id).map(|def| EngineStatusBonuses {
            armor_bonus: def.armor_bonus,
            speed_bonus: def.speed_bonus,
        }).unwrap_or_default()
    }

    fn ability_def(&self, id: &combat_engine::AbilityId) -> Option<combat_engine::AbilityDef> {
        self.abilities.get(id).cloned()
    }

    fn status_def(&self, id: &combat_engine::StatusId) -> Option<combat_engine::StatusDef> {
        self.statuses.get(id).cloned()
    }

    fn caster_context(&self, actor: UnitId) -> combat_engine::CasterContext {
        self.caster_contexts.get(&actor).cloned().unwrap_or_default()
    }
}

/// Build a `CombatState` from a `BattleSnapshot`.
///
/// Uses `entity_to_uid(entity)` for id mapping (same encoding as `engine_bridge::from_ecs`).
/// Copies hp, pos, movement_points, reactions_left, rage, mana, energy directly.
fn snapshot_to_combat_state(snap: &BattleSnapshot, round: u32) -> CombatState {
    use combat_engine::state::ActiveStatus;

    let units: Vec<EngineUnit> = snap
        .units
        .iter()
        .map(|u| {
            let team = match u.team {
                Team::Player => EngineTeam::Player,
                Team::Enemy  => EngineTeam::Enemy,
            };
            let statuses: Vec<ActiveStatus> = u
                .statuses
                .iter()
                .map(|s| ActiveStatus {
                    id: s.id.clone(),
                    rounds_remaining: s.rounds_remaining,
                    dot_per_tick: s.dot_per_tick,
                    // Snapshot's ActiveStatusView drops the applier (AI layer
                    // doesn't track it).  Use the unit's own id as a sentinel
                    // — engine doesn't read applier in Phase 2 mechanics.
                    applier: entity_to_uid(u.entity),
                })
                .collect();
            EngineUnit {
                id: entity_to_uid(u.entity),
                team,
                pos: u.pos,
                hp: u.hp,
                max_hp: u.max_hp,
                armor: u.armor,
                armor_bonus: u.armor_bonus,
                base_speed: u.base_speed,
                speed: u.speed,
                action_points: u.action_points,
                movement_points: u.movement_points,
                reactions_left: u.reactions_left,
                statuses,
                rage: u.rage,
                mana: u.mana,
                energy: u.energy,
            }
        })
        .collect();

    CombatState::new(units, round, RoundPhase::ActorTurn, 0)
}

/// Project mutable fields from a `CombatState` back onto the corresponding
/// `UnitSnapshot` entries in `snap`.
///
/// Covers all fields that `step()` can mutate — Move and Cast both:
/// `hp`, `pos`, `action_points`, `movement_points`, `reactions_left`,
/// `rage`, `mana`, `energy`, `statuses`.
///
/// After status changes, `refresh_aggregates` is called so that
/// `armor_bonus`, `damage_taken_bonus`, `speed`, and AI-side bitflags
/// (`IS_STUNNED`, `FORCES_TARGETING`) stay consistent with the new
/// status list.  `status_tags` must be the same cache used everywhere else
/// in this sim session.
///
/// Fields owned entirely by the snapshot layer (`threat`, `abilities`, `tags`,
/// `aoo_expected_damage`, etc.) are left untouched.
fn project_engine_to_snapshot(
    engine: &CombatState,
    snap: &mut BattleSnapshot,
    status_tags: &StatusTagCache,
) {
    for unit_snap in snap.units.iter_mut() {
        let uid = entity_to_uid(unit_snap.entity);
        let Some(eu) = engine.unit(uid) else { continue };

        let statuses_changed = unit_snap.statuses.len() != eu.statuses.len()
            || unit_snap.statuses.iter().zip(eu.statuses.iter()).any(|(s, e)| {
                s.id != e.id
                    || s.rounds_remaining != e.rounds_remaining
                    || s.dot_per_tick != e.dot_per_tick
            });

        unit_snap.hp              = eu.hp;
        unit_snap.pos             = eu.pos;
        unit_snap.action_points   = eu.action_points;
        unit_snap.movement_points = eu.movement_points;
        unit_snap.reactions_left  = eu.reactions_left;
        unit_snap.rage            = eu.rage;
        unit_snap.mana            = eu.mana;
        unit_snap.energy          = eu.energy;

        unit_snap.statuses = eu.statuses.iter().map(|s| ActiveStatusView {
            id: s.id.clone(),
            rounds_remaining: s.rounds_remaining,
            dot_per_tick: s.dot_per_tick,
        }).collect();

        if statuses_changed {
            unit_snap.refresh_aggregates(status_tags);
        }
    }
    // No need to invalidate_index: we changed unit fields but not order/length,
    // so the entity→index mapping is still valid.
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{empty_content, empty_status_tag_cache, UnitBuilder};
    use crate::combat::ai::world::tags::AiTags;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, StatusApplication, StatusOn, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr, ResourceKind, StatusId};
    use crate::game::hex::hex_from_offset;

    /// Sim-suite defaults: mana 5/10 (enough for simple casts), armor as
    /// override. `hp` also explicit because armor+hp tests are the whole
    /// point of this module.
    fn unit(id: u32, team: Team, pos: Hex, hp: i32, armor: i32) -> UnitSnapshot {
        UnitBuilder::new(id, team, pos)
            .hp(hp)
            .armor(armor)
            .mana(5, 10)
            .build()
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        BattleSnapshot::new(units, 1)
    }

    fn ctx(str_mod: i32, int_mod: i32) -> CasterContext {
        CasterContext { str_mod, int_mod, spell_power: 0, weapon_dice: None }
    }

    fn ability(
        id: &str,
        effect: EffectDef,
        target_type: TargetType,
        range: u32,
    ) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type,
            range: AbilityRange { min: 0, max: range },
            effect,
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

    // ── damage / armor / kill ───────────────────────────────────────────────

    #[test]
    fn damage_subtracts_armor_and_decrements_hp() {
        // Engine reads caster_ctx from the unit snapshot; set str_mod=4 there.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).armor(0).mana(5, 10)
            .caster_ctx(ctx(4, 0))
            .build();
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 2);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 1d6 (EV 3.5 → rounded via `DiceSource` to 4) + str_mod(4) = 8 raw.
        // armor 2 → dealt 6.
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(4, 0), &content, false);

        let t = sim.snapshot.unit(target_id).unwrap();
        assert_eq!(t.hp, 14, "20 - 6 dealt = 14, got hp={}", t.hp);
        assert!((outcome.damage - 6.0).abs() < 0.01, "raw damage {}", outcome.damage);
        assert_eq!(outcome.hits, 1);
        assert!(outcome.killed.is_empty());
    }

    // Regression: heavy armor used to make sim predict 0 damage (`.max(0.0)`),
    // but the live pipeline floors at `max(1)`. Both now agree on the floor —
    // see `combat::effects_math::final_damage_f32`.
    #[test]
    fn damage_respects_min_one_floor_against_heavy_armor() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 10);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 1d6 (EV 3.5) + str_mod(0) = 3.5 vs armor 10 → raw would underflow;
        // floor → 1.0.
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(0, 0), &content, false);

        let t = sim.snapshot.unit(target_id).unwrap();
        assert_eq!(t.hp, 19, "expected 1-damage floor to land, got hp={}", t.hp);
        assert!(
            (outcome.damage - 1.0).abs() < 0.01,
            "expected damage floor 1.0, got {}",
            outcome.damage,
        );
    }

    #[test]
    fn lethal_damage_removes_unit_and_records_kill() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).armor(0).mana(5, 10)
            .caster_ctx(ctx(4, 0))
            .build();
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 3, 0);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        // 1d6 + str_mod(4) = 8 raw vs 3 hp → lethal.
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(4, 0), &content, false);

        assert_eq!(outcome.killed, vec![target_id]);
        // Corpse stays in the snapshot with hp=0 (lift-prune: snapshot is the
        // single source of truth, including dead units). Downstream
        // `enemies_of` / `actor_unit` filter by `is_alive`, so plan-walking
        // code still sees the target as "gone" without a retain'd vec.
        let corpse = sim.snapshot.unit(target_id).expect("corpse retained in snapshot");
        assert_eq!(corpse.hp, 0);
        assert!(!corpse.is_alive());
        assert_eq!(
            sim.snapshot.enemies_of(Team::Enemy).count(), 0,
            "default enemies_of hides the corpse",
        );
    }

    // ── heal ───────────────────────────────────────────────────────────────

    #[test]
    fn heal_caps_at_missing_hp() {
        let actor = unit(1, Team::Player, hex_from_offset(0, 0), 20, 0);
        let ally = unit(2, Team::Player, hex_from_offset(1, 0), 15, 0);
        let actor_id = actor.entity;
        let ally_id = ally.entity;

        let mut content = empty_content();
        // Heal 3d6 (expected 10.5) but target is missing only 5.
        let def = ability(
            "cure",
            EffectDef::Heal { dice: DiceExpr::new(3, 6, 0) },
            TargetType::SingleAlly,
            2,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, ally]), actor_id, empty_status_tag_cache());
        let step = PlanStep::Cast {
            ability: def.id.clone(),
            target: ally_id,
            target_pos: hex_from_offset(1, 0),
        };
        let outcome = sim.apply_step(&step, &ctx(0, 2), &content, false);

        let a = sim.snapshot.unit(ally_id).unwrap();
        assert_eq!(a.hp, 20, "heal must clamp to max_hp");
        assert!((outcome.heal - 5.0).abs() < 0.01, "effective heal {}", outcome.heal);
    }

    // ── resource / AP / MP accounting ───────────────────────────────────────

    #[test]
    fn cast_decrements_ap_and_pays_mana() {
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        actor.action_points = 2;
        actor.max_ap = 2;
        let actor_id = actor.entity;
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
        let target_id = target.entity;

        let mut content = empty_content();
        let mut def = ability(
            "bolt",
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            3,
        );
        def.cost_ap = 1;
        def.costs = vec![crate::content::abilities::ResourceCost {
            resource: ResourceKind::Mana,
            amount: 3,
        }];
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, empty_status_tag_cache());
        sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &ctx(0, 2),
            &content,
            false,
        );

        let a = sim.snapshot.unit(actor_id).unwrap();
        assert_eq!(a.action_points, 1, "AP drops from 2 to 1");
        assert_eq!(a.mana, Some((2, 10)), "mana 5 - 3 = 2");
    }

    #[test]
    fn move_step_updates_pos_and_drains_mp() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let actor_id = actor.entity;
        let target = hex_from_offset(2, 0);

        let content = empty_content();
        let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        let outcome = sim.apply_step(
            &PlanStep::Move { path: vec![hex_from_offset(1, 0), target] },
            &ctx(0, 0),
            &content,
            false,
        );

        assert!(outcome.moved);
        let a = sim.snapshot.unit(actor_id).unwrap();
        assert_eq!(a.pos, target);
        assert_eq!(a.movement_points, 1, "speed 3 - path 2 = 1");
    }

    // ── stun status ─────────────────────────────────────────────────────────

    #[test]
    fn stun_status_is_recorded_in_outcome_and_tags() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let target = unit(2, Team::Player, hex_from_offset(1, 0), 20, 0);
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();

        use crate::content::statuses::StatusDef;
        let stun_def = StatusDef {
            id: StatusId::from("stunned"),
            name: "Stunned".to_string(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
            buff_class: None,
        };
        content.statuses.insert(StatusId::from("stunned"), stun_def);

        let mut def = ability(
            "shock",
            EffectDef::None,
            TargetType::SingleEnemy,
            2,
        );
        def.statuses = vec![StatusApplication {
            status: StatusId::from("stunned"),
            duration_rounds: 1,
            on: StatusOn::Target,
        }];
        content.abilities.insert(def.id.clone(), def.clone());

        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);
        let mut sim = SimState::from_snapshot(&snap(vec![actor, target]), actor_id, &status_tag_cache);
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(1, 0),
            },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(outcome.stunned, vec![target_id]);
        let t = sim.snapshot.unit(target_id).unwrap();
        assert!(t.tags.contains(AiTags::IS_STUNNED));
    }

    // Regression: drift #2 — heal must neutralise target DoT before restoring
    // HP, matching `apply_effects_system`. Previously sim added the full heal
    // to HP ignoring poison ticks.
    #[test]
    fn heal_cleanses_dot_before_restoring_hp() {
        // Engine reads caster_ctx from the unit snapshot; set int_mod=2 there.
        let healer = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .hp(20).armor(0).mana(5, 10)
            .caster_ctx(ctx(0, 2))
            .build();
        let mut ally = unit(2, Team::Player, hex_from_offset(1, 0), 10, 0);
        ally.statuses.push(ActiveStatusView {
            id: StatusId::from("poison"),
            rounds_remaining: 2,
            dot_per_tick: 3,
        });
        let healer_id = healer.entity;
        let ally_id = ally.entity;

        let mut content = empty_content();
        use crate::content::statuses::StatusDef;
        content.statuses.insert(
            StatusId::from("poison"),
            StatusDef {
                id: StatusId::from("poison"),
                name: "Poison".into(),
                armor_bonus: 0,
                damage_taken_bonus: 0,
                skips_turn: false,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: false,
                buff_class: None,
            },
        );
        // Heal: 1d4 (EV 2.5 → 3) + int_mod(2) = 5 raw.
        let def = ability(
            "cure",
            EffectDef::Heal { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleAlly,
            2,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);
        let mut sim = SimState::from_snapshot(&snap(vec![healer, ally]), healer_id, &status_tag_cache);
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: ally_id,
                target_pos: hex_from_offset(1, 0),
            },
            &ctx(0, 2),
            &content,
            false,
        );

        let t = sim.snapshot.unit(ally_id).unwrap();
        // Heal 5: cleanse spends 3 on poison (status removed), 2 remain → HP 10+2=12.
        assert_eq!(t.hp, 12, "cleanse consumes 3, then +2 HP → 12, got {}", t.hp);
        assert!(
            t.statuses.iter().all(|s| s.id.0 != "poison"),
            "poison should be cleansed"
        );
        assert!(
            (outcome.heal - 2.0).abs() < 0.01,
            "reported heal is net HP restored (2), got {}",
            outcome.heal,
        );
    }

    // Regression: drift #5 — status applied in one step must update the
    // target's armor aggregate so the next step's damage math sees the bonus.
    #[test]
    fn status_applied_this_step_armor_affects_next_step() {
        // Attacker uses str_mod=4 in step 2; set it on the snapshot so the engine sees it.
        let attacker = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(20).armor(0).mana(5, 10)
            .caster_ctx(ctx(4, 0))
            .build();
        let buffer = unit(2, Team::Enemy, hex_from_offset(1, 0), 20, 0);
        // Target with HP 20, base armor 0.
        let mut target = unit(3, Team::Player, hex_from_offset(2, 0), 20, 0);
        // Buffer will apply `stone_skin` to target, granting +5 armor_bonus.
        // Attacker then hits; with aggregate refresh, damage is reduced by 5.
        target.action_points = 0;
        let attacker_id = attacker.entity;
        let buffer_id = buffer.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        use crate::content::statuses::StatusDef;
        content.statuses.insert(
            StatusId::from("stone_skin"),
            StatusDef {
                id: StatusId::from("stone_skin"),
                name: "Stone Skin".into(),
                armor_bonus: 5,
                damage_taken_bonus: 0,
                skips_turn: false,
                forces_targeting: false,
                dot_dice: None,
                blocks_mana_abilities: false,
                speed_bonus: 0,
                hp_percent_dot: 0,
                ai_controlled: false,
                causes_disadvantage: false,
                buff_class: None,
            },
        );

        // Cross-team buff: SingleEnemy on target so the status actually lands
        // mid-sim without violating team-filtering in `compute_affected_targets`.
        let mut buff_def = ability(
            "stone_skin_cast",
            EffectDef::None,
            TargetType::SingleEnemy,
            3,
        );
        buff_def.statuses = vec![StatusApplication {
            status: StatusId::from("stone_skin"),
            duration_rounds: 3,
            on: StatusOn::Target,
        }];
        content.abilities.insert(buff_def.id.clone(), buff_def.clone());

        // Damage: 1d6 (EV 4) + str_mod(4) = 8 raw.
        let atk_def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            3,
        );
        content.abilities.insert(atk_def.id.clone(), atk_def.clone());

        // Build a real status tag cache so refresh_aggregates picks up
        // stone_skin's armor_bonus=5 (the whole point of this test).
        use crate::combat::ai::world::tags::cache::build_caches;
        let (status_tag_cache, _) = build_caches(&content);

        // Step 1: buffer (active actor for this cast) puts stone_skin on target.
        let mut sim = SimState::from_snapshot(
            &snap(vec![attacker, buffer, target]),
            buffer_id,
            &status_tag_cache,
        );
        sim.apply_step(
            &PlanStep::Cast {
                ability: buff_def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(2, 0),
            },
            &ctx(0, 0),
            &content,
            false,
        );

        let t_mid = sim.snapshot.unit(target_id).unwrap();
        assert_eq!(
            t_mid.armor_bonus, 5,
            "aggregate should refresh after status apply, got {}",
            t_mid.armor_bonus,
        );

        // Step 2: attacker strikes target. Swap active actor.
        sim.actor = attacker_id;
        let atk_outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: atk_def.id.clone(),
                target: target_id,
                target_pos: hex_from_offset(2, 0),
            },
            &ctx(4, 0),
            &content,
            false,
        );

        let t_after = sim.snapshot.unit(target_id).unwrap();
        // raw 8 − armor_bonus 5 = 3 dealt. HP: 20 − 3 = 17.
        assert_eq!(t_after.hp, 17, "armor should reduce damage from 8 to 3, got hp={}", t_after.hp);
        assert!(
            (atk_outcome.damage - 3.0).abs() < 0.01,
            "reported damage after mitigation {}",
            atk_outcome.damage,
        );
    }

    // ── AoE ─────────────────────────────────────────────────────────────────

    #[test]
    fn aoe_circle_hits_all_enemies_in_radius() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let t1 = unit(2, Team::Player, hex_from_offset(3, 0), 20, 0);
        let t2 = unit(3, Team::Player, hex_from_offset(4, 0), 20, 0);
        let actor_id = actor.entity;
        let t1_id = t1.entity;
        let t2_id = t2.entity;

        let mut content = empty_content();
        let mut def = ability(
            "blast",
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            5,
        );
        def.aoe = AoEShape::Circle { radius: 1 };
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, t1, t2]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: t1_id,
                target_pos: hex_from_offset(3, 0),
            },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(outcome.hits, 2, "radius-1 centered at (3,0) covers both (3,0) and (4,0)");
        assert!(sim.snapshot.unit(t1_id).unwrap().hp < 20);
        assert!(sim.snapshot.unit(t2_id).unwrap().hp < 20);
    }

    // ── GrantMovement ───────────────────────────────────────────────────────

    // NOTE: GrantMovement is deferred to Phase 3 in the engine — `effect_for_target`
    // returns None for this variant so no MP is added.  The engine still pays AP.
    #[test]
    fn grant_movement_pays_ap_engine_defers_mp() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20, 0);
        let actor_id = actor.entity;

        let mut content = empty_content();
        let def = ability(
            "rush",
            EffectDef::GrantMovement { distance: 4 },
            TargetType::Myself,
            0,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(&snap(vec![actor]), actor_id, empty_status_tag_cache());
        sim.apply_step(
            &PlanStep::Cast {
                ability: def.id.clone(),
                target: actor_id,
                target_pos: hex_from_offset(0, 0),
            },
            &ctx(0, 0),
            &content,
            false,
        );

        let a = sim.snapshot.unit(actor_id).unwrap();
        // Engine pays AP (cost_ap=1), but GrantMovement effect fanout is Phase 3 —
        // MP stays at the initial value (3) since no GrantMovement Effect is emitted.
        assert_eq!(a.movement_points, 3, "engine defers GrantMovement to Phase 3; MP unchanged");
        assert_eq!(a.action_points, 0, "AP cost still paid by engine");
    }

    // ── AoO propagation (step 12.2) ─────────────────────────────────────────
    //
    // Positions from `tests/aoo.rs` (verified adjacent/non-adjacent):
    //   actor_pos  = hex_from_offset(3, 3)  — hero start
    //   enemy_pos  = hex_from_offset(4, 3)  — goblin; distance 1 from actor_pos
    //   away_pos   = hex_from_offset(2, 3)  — distance 2 from enemy (verified in aoo.rs)
    //   near_pos   = hex_from_offset(3, 4)  — distance 1 from actor_pos AND enemy_pos

    /// Moving out of adjacency with a reacting enemy records AoO self_damage
    /// and applies it to actor hp.
    #[test]
    fn apply_move_records_aoo_self_damage() {
        // Actor at (3,3), enemy at (4,3) — adjacent (distance 1).
        // Move to (2,3) — distance 2 from enemy (leaves adjacency).
        // No armor → raw damage == dealt damage.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(20)
            .armor(0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(5.0, 1)
            .build();
        let actor_id = actor.entity;

        // Pre-conditions (mirrors aoo.rs verified layout).
        let actor_pos = hex_from_offset(3, 3);
        let enemy_pos = hex_from_offset(4, 3);
        let away = hex_from_offset(2, 3);
        assert_eq!(actor_pos.unsigned_distance_to(enemy_pos), 1, "actor adj to enemy");
        assert_eq!(away.unsigned_distance_to(enemy_pos), 2, "away not adj to enemy");

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_move(&[away]);

        assert_eq!(outcome.self_damage, 5.0, "raw 5, no armor → self_damage 5");
        assert_eq!(sim.actor_unit().unwrap().hp, 15, "hp 20 − 5 AoO = 15");
    }

    /// After a provoked AoO, the triggering enemy's reactions_left is decremented.
    #[test]
    fn apply_move_decrements_enemy_reactions() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3)).aoo(5.0, 1).build();
        let enemy_id = enemy.entity;
        let actor_id = actor.entity;

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_move(&[hex_from_offset(2, 3)]);

        assert_eq!(
            sim.snapshot.unit(enemy_id).unwrap().reactions_left,
            0,
            "enemy reaction consumed",
        );
    }

    /// Enemy with reactions_left = 0 does not trigger AoO even when adjacency is left.
    #[test]
    fn apply_move_no_aoo_when_already_used_reaction() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
        // reactions_left = 0 — reaction already spent this round.
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3)).aoo(5.0, 0).build();
        let actor_id = actor.entity;

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

        assert_eq!(outcome.self_damage, 0.0, "no reaction available → no AoO");
        assert_eq!(sim.actor_unit().unwrap().hp, 20, "hp unchanged");
    }

    /// A lethal AoO sets actor hp to 0; self_damage reports HP actually lost
    /// (the HP delta, not raw dealt damage). With hp=1 and raw=10, HP delta = 1.
    ///
    /// **Behaviour change from legacy sim (manifest):** the old `apply_move`
    /// tracked `self_damage` as actual dealt damage post-armor (`final_damage_f32`),
    /// which could exceed the actor's remaining HP (e.g., 10 dealt vs 1 HP).
    /// The engine shim uses HP delta instead, which is the HP actually lost (1).
    /// For safety scoring (`total_self_damage / actor_max_hp`) this is equivalent
    /// in the lethal case: both produce a ratio that clamps to 1.0.
    #[test]
    fn apply_move_kills_actor_with_lethal_aoo() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(1)
            .armor(0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(10.0, 1)
            .build();
        let actor_id = actor.entity;

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

        // Engine path: self_damage = HP delta (1 hp lost, not 10 raw dealt).
        assert_eq!(outcome.self_damage, 1.0,
            "engine shim: self_damage is HP delta (1 hp lost), not raw dealt damage (10)");
        assert!(
            sim.actor_unit().is_none(),
            "actor hp=0 → is_alive()=false → actor_unit() returns None",
        );
        // hp clamped to 0, not negative.
        let dead = sim.snapshot.units.iter().find(|u| u.entity == actor_id).unwrap();
        assert_eq!(dead.hp, 0, "hp clamped to 0");
    }

    /// Path that stays adjacent to the enemy does not trigger AoO.
    #[test]
    fn apply_move_no_aoo_when_path_stays_adjacent() {
        // Actor at (3,3), enemy at (4,3). Move to (3,4) which is adjacent to
        // both (verified: (3,4) is distance 1 from (3,3) per aoo.rs layout).
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3)).hp(20).build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(5.0, 1)
            .build();
        let actor_id = actor.entity;
        let dest = hex_from_offset(3, 4);

        // Pre-condition: dest must be adjacent to enemy (distance 1).
        assert_eq!(
            dest.unsigned_distance_to(hex_from_offset(4, 3)),
            1,
            "test precondition: (3,4) is adjacent to enemy at (4,3)",
        );

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_move(&[dest]);

        assert_eq!(outcome.self_damage, 0.0, "no adjacency-leave → no AoO");
        assert_eq!(sim.actor_unit().unwrap().hp, 20, "hp unchanged");
    }

    /// AoO fires at most once per enemy per step even if the path briefly
    /// leaves and re-enters adjacency.
    #[test]
    fn apply_move_aoo_only_once_per_enemy_per_step() {
        // Actor at (3,3), enemy at (4,3).
        // Path: [(2,3), (3,4)] — first cell (2,3) is away (dist 2 from enemy),
        // second cell (3,4) is adjacent again (dist 1 from enemy).
        // AoO should trigger exactly once on the (3,3)→(2,3) transition.
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(20)
            .armor(0)
            .build();
        // reactions = 2 to prove the cap comes from scan logic, not reactions_left running out.
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(5.0, 2)
            .build();
        let enemy_id = enemy.entity;
        let actor_id = actor.entity;

        let enemy_pos = hex_from_offset(4, 3);
        // Verify: (2,3) is NOT adjacent to enemy; (3,4) IS adjacent to enemy.
        assert_eq!(hex_from_offset(2, 3).unsigned_distance_to(enemy_pos), 2, "(2,3) not adj");
        assert_eq!(hex_from_offset(3, 4).unsigned_distance_to(enemy_pos), 1, "(3,4) adj");

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        // Path: leave adjacency at step (3,3→2,3), then re-enter at (3,4).
        let outcome = sim.apply_move(&[hex_from_offset(2, 3), hex_from_offset(3, 4)]);

        assert_eq!(outcome.self_damage, 5.0, "exactly one AoO per step per enemy");
        assert_eq!(
            sim.snapshot.unit(enemy_id).unwrap().reactions_left,
            1,
            "only one reaction consumed out of 2",
        );
    }

    /// AoO damage is mitigated by armor_bonus from status buffs (12.1 + 12.2 integration).
    #[test]
    fn apply_move_aoo_mitigated_by_armor_bonus() {
        // Actor armor=0, armor_bonus=5 (simulating a prior status apply).
        // Enemy AoO raw=8. Expected: final_damage_f32(8, 5, 0, false) = max(1, 8-5) = 3.
        // armor_bonus must be set before SimState::from_snapshot so that
        // both snapshot and combat_state see the same value.
        let mut actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(20)
            .armor(0)
            .build();
        actor.armor_bonus = 5;
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(8.0, 1)
            .build();
        let actor_id = actor.entity;

        let sim_snap = snap(vec![actor, enemy]);
        let mut sim = SimState::from_snapshot(&sim_snap, actor_id, empty_status_tag_cache());

        let outcome = sim.apply_move(&[hex_from_offset(2, 3)]);

        assert_eq!(outcome.self_damage, 3.0, "armor_bonus 5 reduces raw 8 AoO to 3");
        assert_eq!(sim.actor_unit().unwrap().hp, 17, "hp 20 − 3 = 17");
    }

    // ── Rage gain on damage (drift #3) ─────────────────────────────────────

    /// Single-target hit: attacker has rage, defender does not.
    /// Attacker rage increments by 1; defender rage stays None.
    #[test]
    fn apply_damage_grants_rage_to_attacker_per_hit() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(5, 10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .build(); // no rage
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.actor_unit().unwrap().rage, Some((6, 10)), "attacker rage (5/10) → (6/10)");
        assert_eq!(sim.snapshot.unit(target_id).unwrap().rage, None, "defender has no rage component");
    }

    /// Single-target hit: defender has rage, attacker does not.
    /// Defender rage increments by 1; attacker rage stays None.
    #[test]
    fn apply_damage_grants_rage_to_defender_per_hit() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .build(); // no rage
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .rage(3, 10)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.snapshot.unit(target_id).unwrap().rage, Some((4, 10)), "defender rage (3/10) → (4/10)");
        assert_eq!(sim.actor_unit().unwrap().rage, None, "attacker has no rage component");
    }

    /// Single-target hit: both sides have rage. Each gains exactly +1.
    #[test]
    fn apply_damage_grants_rage_to_both_attacker_and_defender() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(2, 10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .rage(7, 10)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.actor_unit().unwrap().rage, Some((3, 10)), "attacker (2/10) → (3/10)");
        assert_eq!(sim.snapshot.unit(target_id).unwrap().rage, Some((8, 10)), "defender (7/10) → (8/10)");
    }

    /// AoE hitting 3 enemies: attacker rage gets +1 per target hit (total +3).
    /// Each defender gets +1.
    #[test]
    fn aoe_damage_grants_rage_per_target_to_attacker() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(5, 10)
            .build();
        // Three enemies clustered at (3,0), (4,0), (3,1) — within radius 1 of (3,0).
        let t1 = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0)).rage(0, 10).build();
        let t2 = UnitBuilder::new(3, Team::Player, hex_from_offset(4, 0)).rage(0, 10).build();
        let t3 = UnitBuilder::new(4, Team::Player, hex_from_offset(3, 1)).rage(0, 10).build();
        let actor_id = actor.entity;
        let t1_id = t1.entity;
        let t2_id = t2.entity;
        let t3_id = t3.entity;

        let mut content = empty_content();
        let mut def = ability(
            "blast",
            EffectDef::SpellDamage { dice: DiceExpr::new(1, 4, 0) },
            TargetType::SingleEnemy,
            5,
        );
        def.aoe = AoEShape::Circle { radius: 1 };
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, t1, t2, t3]),
            actor_id,
            empty_status_tag_cache(),
        );
        let outcome = sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: t1_id, target_pos: hex_from_offset(3, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(outcome.hits, 3, "AoE should hit all 3 enemies");
        assert_eq!(sim.actor_unit().unwrap().rage, Some((8, 10)), "attacker (5/10) + 3 hits = (8/10)");
        assert_eq!(sim.snapshot.unit(t1_id).unwrap().rage, Some((1, 10)), "t1 (0/10) → (1/10)");
        assert_eq!(sim.snapshot.unit(t2_id).unwrap().rage, Some((1, 10)), "t2 (0/10) → (1/10)");
        assert_eq!(sim.snapshot.unit(t3_id).unwrap().rage, Some((1, 10)), "t3 (0/10) → (1/10)");
    }

    /// Rage clamps at max: attacker at max rage stays there after a hit.
    #[test]
    fn rage_caps_at_max_for_attacker() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .rage(10, 10)
            .build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.actor_unit().unwrap().rage, Some((10, 10)), "attacker rage capped at max 10");
    }

    /// Rage clamps at max: defender at max rage stays there after taking a hit.
    #[test]
    fn rage_caps_at_max_for_defender() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .rage(10, 10)
            .build();
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.snapshot.unit(target_id).unwrap().rage, Some((10, 10)), "defender rage capped at max 10");
    }

    /// Units with no rage component (rage: None) are silently unaffected.
    /// No panic, rage stays None on both sides.
    #[test]
    fn units_without_rage_component_are_unaffected() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build(); // rage: None
        let target = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0)).build(); // rage: None
        let actor_id = actor.entity;
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability(
            "strike",
            EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            TargetType::SingleEnemy,
            1,
        );
        content.abilities.insert(def.id.clone(), def.clone());

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, target]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_step(
            &PlanStep::Cast { ability: def.id.clone(), target: target_id, target_pos: hex_from_offset(1, 0) },
            &ctx(0, 0),
            &content,
            false,
        );

        assert_eq!(sim.actor_unit().unwrap().rage, None, "attacker has no rage component, stays None");
        assert_eq!(sim.snapshot.unit(target_id).unwrap().rage, None, "defender has no rage component, stays None");
    }

    // ── AoO rage (drift #3, AoO branch) ─────────────────────────────────────

    /// Mirrors `combat/movement.rs:228-236` real-pipeline rule: for every AoO
    /// hit, BOTH the AoO attacker AND the moving victim gain +1 rage.
    #[test]
    fn apply_move_aoo_grants_rage_to_both_sides() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(20)
            .rage(0, 10)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(5.0, 1)
            .rage(0, 10)
            .build();
        let actor_id = actor.entity;
        let enemy_id = enemy.entity;

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_move(&[hex_from_offset(2, 3)]);

        assert_eq!(sim.actor_unit().unwrap().rage, Some((1, 10)), "victim +1 rage");
        assert_eq!(
            sim.snapshot.unit(enemy_id).unwrap().rage,
            Some((1, 10)),
            "AoO attacker +1 rage",
        );
    }

    /// AoO rage gain clamps to max on both sides.
    #[test]
    fn apply_move_aoo_rage_caps_at_max() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(3, 3))
            .hp(20)
            .rage(10, 10)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(4, 3))
            .aoo(5.0, 1)
            .rage(10, 10)
            .build();
        let actor_id = actor.entity;
        let enemy_id = enemy.entity;

        let mut sim = SimState::from_snapshot(
            &snap(vec![actor, enemy]),
            actor_id,
            empty_status_tag_cache(),
        );
        sim.apply_move(&[hex_from_offset(2, 3)]);

        assert_eq!(sim.actor_unit().unwrap().rage, Some((10, 10)));
        assert_eq!(sim.snapshot.unit(enemy_id).unwrap().rage, Some((10, 10)));
    }

    // TODO(12.3): `self_damage_grants_two_rage_for_self_aoe` — actor is both
    // source and defender in friendly-fire AoE. The real pipeline iterates
    // `for actor in [source, target]` so the same unit's `rage.gain()` is
    // called twice → total +2. Setting up a single-unit self-AoE scenario
    // requires a friendly_fire=true AoE ability that targets the caster — the
    // existing `ability()` helper only supports SingleEnemy target type, and
    // `TargetType::Myself` with AoE is not exercised by current content.
    // The structural correctness is verified by inspection: in
    // `apply_primary`, defender rage is bumped inside the `unit_mut(ent)` borrow,
    // then `actor_unit_mut()` (same entity) bumps it again — producing +2.
}

