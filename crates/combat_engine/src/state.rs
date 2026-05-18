//! `CombatState` — canonical in-engine battle state.
//!
//! Uses `Vec<Unit>` for deterministic iteration order (critical for replay)
//! with a `HashMap<UnitId, usize>` index for O(1) lookup. (Decision 6.1.)
//!
//! `UnitId(u64)` is an opaque new-type; the Entity↔UnitId mapping lives in
//! `crate::combat::engine_bridge` (the Bevy boundary). (Decision 6.2.)

use std::collections::HashMap;

use hexx::Hex;

use crate::content::ContentView;
use crate::event::Event;
use crate::turn_queue::TurnQueue;
use crate::StatusId;

// ── Identity ──────────────────────────────────────────────────────────────────

/// Opaque unit identifier inside the engine.  Maps 1-to-1 with a Bevy
/// `Entity` via `crate::combat::engine_bridge::UnitIdMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct UnitId(pub u64);

// ── Resource pools ────────────────────────────────────────────────────────────

/// A (current, max) resource pool that may or may not exist on a unit.
pub type Pool = (i32, i32);

// ── Status effects ────────────────────────────────────────────────────────────

/// Engine-local mirror of `game::components::ActiveStatus`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActiveStatus {
    pub id: StatusId,
    pub rounds_remaining: u32,
    /// DoT damage per end-of-turn tick. 0 = no DoT.
    pub dot_per_tick: i32,
    /// The unit whose end-turn ticks down `rounds_remaining`.  Used by
    /// status-removal (`advance_turn`) and aura cleanup (`auras_system`) to
    /// distinguish ability-applied from aura-applied entries.  Engine itself
    /// doesn't read this in Phase 2 — recorded for the projector + Phase 3
    /// DoT tick attribution.
    pub applier: UnitId,
}

// ── Team ──────────────────────────────────────────────────────────────────────

/// Combat team — canonical engine-side enum.  `game::components::Team`
/// is a re-export of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Team {
    Player,
    Enemy,
}

// ── Round phase ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundPhase {
    PreRound,
    ActorTurn,
    EndRound,
}

// ── Unit ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Unit {
    pub id: UnitId,
    pub team: Team,
    pub pos: Hex,
    /// Current HP. 0 = dead (unit stays in `units` vec as tombstone).
    pub hp: i32,
    pub max_hp: i32,
    /// Base armor value (equipment). Bonus from statuses is tracked separately
    /// and folded in by `refresh_aggregates`.
    pub armor: i32,
    /// Armor bonus from active statuses (recomputed by `RefreshAggregates`).
    pub armor_bonus: i32,
    /// Base speed (without status speed_bonus).
    pub base_speed: i32,
    /// Effective speed = base_speed + speed bonuses from statuses.
    pub speed: i32,
    pub action_points: i32,
    pub max_ap: i32,
    pub movement_points: i32,
    pub reactions_left: i32,
    /// Maximum reactions per round. Populated by the bridge from `Reactions.max`.
    pub reactions_max: i32,
    pub statuses: Vec<ActiveStatus>,
    /// `None` if the unit has no rage mechanic.
    pub rage: Option<Pool>,
    /// `None` if the unit has no mana mechanic.
    pub mana: Option<Pool>,
    /// `None` if the unit has no energy mechanic.
    pub energy: Option<Pool>,
    /// Set when this unit was spawned via `Effect::Spawn`. `None` for units
    /// present at combat start (loaded from ECS).
    pub summoner: Option<UnitId>,
    /// Resolved caster stats (weapon dice, modifiers, crit-fail outcome).
    /// Populated at combat init from `Equipment` + `CombatStats` ECS components.
    /// Used by the Cast fanout (damage / heal formulas).
    #[serde(default)]
    pub caster_context: crate::content::CasterContext,
    /// AoO dice for this unit, if it can perform opportunity attacks.
    /// `Some(dice)` iff the unit has a melee `WeaponAttack` ability and an
    /// equipped weapon; bonus already includes the strength modifier.
    /// `None` means "cannot AoO" — distinct from `caster_context.weapon_dice`,
    /// which carries the raw weapon dice used for Cast damage rolls (ranged
    /// units have weapon_dice but no aoo_dice).
    #[serde(default)]
    pub aoo_dice: Option<crate::dice::DiceExpr>,
    /// Passive aura definitions emitted by this unit.
    /// Populated at combat init from the `AuraSource` ECS component.
    /// Empty for units with no auras.
    #[serde(default)]
    pub auras: Vec<crate::content::AuraDef>,
    /// Pending phase-transition thresholds for this unit (boss-only).
    /// First entry = next phase to trigger. Bridge translator pops entry[0]
    /// on `Event::PhaseEntered`. Empty for non-bosses.
    #[serde(default)]
    pub enemy_phases: Vec<crate::content::PhaseEntry>,
}

impl Unit {
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Check whether this unit should enter a new phase after its HP dropped
    /// to `new_hp` (out of `max_hp`).
    ///
    /// Peeks at `self.enemy_phases[0]` without consuming it — the bridge
    /// translator pops the entry on `Event::PhaseEntered`.
    ///
    /// Returns `(phase_idx, transition)` for the first pending phase whose
    /// threshold is crossed, or `None` if no phase fires.
    pub fn check_phase_trigger(
        &self,
        new_hp: i32,
        max_hp: i32,
    ) -> Option<(usize, crate::content::PhaseTransition)> {
        let entry = self.enemy_phases.first()?;
        if max_hp == 0 || new_hp * 100 > max_hp * entry.pct {
            return None;
        }
        let new_max_hp = if entry.new_max_hp > 0 { entry.new_max_hp } else { max_hp };
        Some((0, crate::content::PhaseTransition {
            new_max_hp,
            new_armor: 0,
            new_base_speed: 0,
            heal_to_full: entry.heal_to_full,
        }))
    }
}

// ── CombatState ───────────────────────────────────────────────────────────────

/// Counter for engine-generated UnitIds. Starts above `Entity::to_bits()` range
/// in practice so synthetic UIDs never collide with bridge-derived UIDs.
pub(crate) const SYNTHETIC_UID_BASE: u64 = 1u64 << 63;

/// Canonical engine state for one combat encounter.
///
/// `units` is the authoritative list; `idx` is a derived cache — always in
/// sync via `insert_unit` / `remove_unit`.  Never mutate `units` directly;
/// go through the provided methods so the cache stays consistent.
#[derive(Debug, Clone)]
pub struct CombatState {
    units: Vec<Unit>,
    /// `UnitId → index` in `units`. Rebuilt by `rebuild_idx` after bulk mutations.
    idx: HashMap<UnitId, usize>,
    pub round: u32,
    pub phase: RoundPhase,
    /// Engine-owned turn order. Populated by the bridge via `set_turn_queue` at
    /// combat init.  Nothing reads this field yet in Phase 4a — Bevy still owns
    /// advance logic.  Phase 4b wires `Effect::AdvanceTurn` to consume it.
    pub turn_queue: TurnQueue,
    /// Seed carried along for replay reproducibility.
    pub random_seed: u64,
    next_synthetic_uid: u64,
}

impl Default for CombatState {
    fn default() -> Self {
        Self::new(vec![], 0, RoundPhase::PreRound, 0)
    }
}

impl CombatState {
    /// Construct from a pre-built unit list. Eagerly builds the index.
    /// The `turn_queue` starts empty; populate it via `set_turn_queue` after construction.
    pub fn new(units: Vec<Unit>, round: u32, phase: RoundPhase, random_seed: u64) -> Self {
        let mut state = Self {
            units,
            idx: HashMap::new(),
            round,
            phase,
            turn_queue: TurnQueue::default(),
            random_seed,
            next_synthetic_uid: SYNTHETIC_UID_BASE,
        };
        state.rebuild_idx();
        state
    }

    /// Set the engine turn queue from an externally-ordered `Vec<UnitId>`.
    ///
    /// Called by `init_state_from_ecs` to mirror the ECS `Res<TurnQueue>` into
    /// engine state at combat init.  `index` is the current cursor position
    /// (typically 0 at combat start, but preserved across hot-reloads).
    pub fn set_turn_queue(&mut self, order: Vec<UnitId>, index: usize) {
        self.turn_queue = TurnQueue { order, index };
    }

    /// Reset per-round state at the beginning of a new round.
    ///
    /// - Resets `reactions_left = reactions_max` for all alive units.
    /// - Sets `turn_queue.index = 0`.
    /// - Sets `phase = RoundPhase::ActorTurn`.
    ///
    /// Returns an empty `Vec<Event>` for Phase 4a; later phases may emit
    /// `Event::RoundStarted` here.
    pub fn start_round(&mut self, _content: &dyn ContentView) -> Vec<Event> {
        for unit in self.units.iter_mut() {
            if unit.is_alive() {
                unit.reactions_left = unit.reactions_max;
            }
        }
        self.turn_queue.index = 0;
        self.phase = RoundPhase::ActorTurn;
        vec![]
    }

    /// Rebuild the `UnitId → index` cache after any bulk mutation.
    pub fn rebuild_idx(&mut self) {
        self.idx.clear();
        for (i, u) in self.units.iter().enumerate() {
            self.idx.insert(u.id, i);
        }
    }

    /// Append a new unit and keep the index in sync.
    pub(crate) fn insert_unit(&mut self, unit: Unit) {
        let pos = self.units.len();
        self.idx.insert(unit.id, pos);
        self.units.push(unit);
    }

    pub(crate) fn alloc_synthetic_uid(&mut self) -> UnitId {
        let uid = UnitId(self.next_synthetic_uid);
        self.next_synthetic_uid = self.next_synthetic_uid
            .checked_add(1)
            .expect("synthetic UID exhaustion — combat lifetime > 2^63 spawns");
        uid
    }

    /// Current value of the synthetic UID counter (before the next alloc).
    ///
    /// Exposed for trace `InitLine` serialization so replay can re-seed the
    /// counter to the same starting value (Phase 5 D1).
    pub fn next_synthetic_uid(&self) -> u64 {
        self.next_synthetic_uid
    }

    /// Restore the synthetic UID counter after deserialization (replay).
    ///
    /// Call this after constructing `CombatState::new(...)` from an `InitLine`
    /// to ensure spawned units receive the same IDs as in the original run.
    pub fn set_next_synthetic_uid(&mut self, n: u64) {
        self.next_synthetic_uid = n;
    }

    /// Look up a unit by id. Returns `None` if not present.
    pub fn unit(&self, id: UnitId) -> Option<&Unit> {
        self.idx.get(&id).map(|&i| &self.units[i])
    }

    /// Mutable unit lookup.
    pub fn unit_mut(&mut self, id: UnitId) -> Option<&mut Unit> {
        self.idx.get(&id).map(|&i| &mut self.units[i])
    }

    /// Iterate all units (alive + dead tombstones).
    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    /// Iterate alive units only.
    pub fn alive_units(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter().filter(|u| u.is_alive())
    }

    /// All living enemies of `actor_id`.
    pub fn enemies_of(&self, actor_id: UnitId) -> impl Iterator<Item = &Unit> {
        let team = self.unit(actor_id).map(|u| u.team);
        self.units.iter().filter(move |u| u.is_alive() && Some(u.team) != team)
    }

    /// Refill AP and MP, then regen resources for the actor whose turn is beginning.
    ///
    /// Returns events for any resource that changed so the bridge can log them.
    /// AP and MP refills are silent (projected back to ECS directly). Ticks fire
    /// for both alive and dead appliers (sirota-DoT case).
    pub fn start_actor_turn(
        &mut self,
        actor: UnitId,
        content: &dyn crate::content::ContentView,
    ) -> Vec<Event> {
        let mut events = Vec::new();

        if let Some(u) = self.unit_mut(actor) {
            if u.is_alive() {
                u.action_points = u.max_ap;
                u.movement_points = u.speed;
                if let Some((cur, max)) = u.mana.as_mut() {
                    let new = (*cur + 1).min(*max);
                    if new != *cur {
                        *cur = new;
                        events.push(Event::ManaRegenerated { unit: actor, current: new, max: *max });
                    }
                }
                if let Some((cur, max)) = u.energy.as_mut() {
                    let new = (*cur + 1).min(*max);
                    if new != *cur {
                        *cur = new;
                        events.push(Event::EnergyRegenerated { unit: actor, current: new, max: *max });
                    }
                }
            }
        }

        events.extend(self.tick_actor_statuses(actor, content));
        events
    }

    /// Fan out `TickDot` and `ExpireStatus` effects for every (target, status)
    /// pair across all units where `status.applier == actor`. Processes the
    /// effects through the standard engine queue (apply_effect cascade) and
    /// returns the resulting event stream. Works for dead actors (sirota-DoT case).
    pub fn tick_actor_statuses(
        &mut self,
        actor: UnitId,
        content: &dyn crate::content::ContentView,
    ) -> Vec<crate::event::Event> {
        use std::collections::VecDeque;
        use crate::effect::{apply_effect, Effect};
        use crate::event::effect_to_event;

        let initial: Vec<Effect> = self
            .units()
            .iter()
            .flat_map(|u| {
                u.statuses
                    .iter()
                    .filter(|s| s.applier == actor)
                    .flat_map(move |s| {
                        [
                            Effect::TickDot { target: u.id, status: s.id.clone() },
                            Effect::ExpireStatus { target: u.id, status: s.id.clone() },
                        ]
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        let mut queue: VecDeque<Effect> = initial.into();
        let mut events: Vec<crate::event::Event> = Vec::new();
        let mut steps = 0;
        const MAX_TICK_DEPTH: usize = 500;

        while let Some(eff) = queue.pop_front() {
            steps += 1;
            if steps > MAX_TICK_DEPTH {
                break;
            }

            let prev_pos = match &eff {
                Effect::MovePosition { actor, .. } => self.unit(*actor).map(|u| u.pos),
                _ => None,
            };
            let (derived, ctx) = apply_effect(self, &eff, content);
            if let Some(ev) = effect_to_event(&eff, self, prev_pos, &ctx) {
                events.push(ev);
            }
            for d in derived {
                queue.push_back(d);
            }
        }

        events
    }
}

impl CombatState {
    // ── Aura query helpers (Phase 4 step 4c) ─────────────────────────────────

    /// Compute the aggregated aura effects on `target` from all alive aura sources.
    ///
    /// Pure query — walks alive units, reads their `unit.auras` field (5c.1),
    /// filters by `distance(source.pos, target.pos) ≤ radius` and team relation,
    /// then folds all matching status bonuses into an `AuraEffects` result.
    ///
    /// A dead target receives no aura effects (auras don't apply to corpses).
    /// A dead source contributes nothing (alive_units filter).
    pub fn aura_effects_on(&self, target: UnitId, content: &dyn crate::content::ContentView) -> crate::content::AuraEffects {
        use crate::content::{AuraEffects, TeamRelation};
        let mut out = AuraEffects::default();

        let (target_pos, target_team) = match self.unit(target) {
            Some(u) if u.is_alive() => (u.pos, u.team),
            _ => return out, // dead or unknown target → no aura effects
        };

        // Collect source ids first to avoid borrowing self while iterating.
        let source_ids: Vec<(UnitId, hexx::Hex, Team)> = self
            .alive_units()
            .filter(|u| u.id != target)
            .map(|u| (u.id, u.pos, u.team))
            .collect();

        for (src_id, src_pos, src_team) in source_ids {
            let auras = match self.unit(src_id) {
                Some(u) if !u.auras.is_empty() => u.auras.clone(),
                _ => continue,
            };
            let dist = src_pos.unsigned_distance_to(target_pos);
            for aura in &auras {
                if dist > aura.radius {
                    continue;
                }
                let matches = match aura.applies_to {
                    TeamRelation::Enemies => target_team != src_team,
                    TeamRelation::Allies  => target_team == src_team,
                    TeamRelation::All     => true,
                };
                if !matches {
                    continue;
                }
                // Fold status bonuses from this aura's status into the result.
                let b = content.status_bonuses(&aura.status_id);
                out.speed_bonus         += b.speed_bonus;
                out.armor_bonus         += b.armor_bonus;
                // StatusDef carries additional flags; retrieve them if available.
                if let Some(def) = content.status_def(&aura.status_id) {
                    out.damage_taken_bonus  += def.damage_taken_bonus;
                    out.skips_turn          |= def.skips_turn;
                    out.causes_disadvantage |= def.causes_disadvantage;
                }
            }
        }

        out
    }

    /// Snapshot of all (target, source, status_id) triples where an aura is
    /// currently in effect.
    ///
    /// Used by `step()` to compute diffs around `Effect::MovePosition` and
    /// `Effect::Death` and emit `Event::AuraStatusGained` / `AuraStatusLost`.
    ///
    /// Returns `BTreeSet` (not `HashSet`) to guarantee deterministic iteration
    /// order across calls — required for byte-equal event emission (Phase 5 §8).
    pub fn aura_membership_set(
        &self,
        _content: &dyn crate::content::ContentView,
    ) -> std::collections::BTreeSet<(UnitId, UnitId, crate::StatusId)> {
        use crate::content::TeamRelation;
        let mut set = std::collections::BTreeSet::new();

        let source_ids: Vec<(UnitId, hexx::Hex, Team)> = self
            .alive_units()
            .map(|u| (u.id, u.pos, u.team))
            .collect();

        for (src_id, src_pos, src_team) in &source_ids {
            let auras = match self.unit(*src_id) {
                Some(u) if !u.auras.is_empty() => u.auras.clone(),
                _ => continue,
            };
            // Check each alive unit as a potential target.
            for (tgt_id, tgt_pos, tgt_team) in &source_ids {
                if tgt_id == src_id {
                    continue;
                }
                let dist = src_pos.unsigned_distance_to(*tgt_pos);
                for aura in &auras {
                    if dist > aura.radius {
                        continue;
                    }
                    let matches = match aura.applies_to {
                        TeamRelation::Enemies => *tgt_team != *src_team,
                        TeamRelation::Allies  => *tgt_team == *src_team,
                        TeamRelation::All     => true,
                    };
                    if matches {
                        set.insert((*tgt_id, *src_id, aura.status_id.clone()));
                    }
                }
            }
        }

        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexx::Hex;
    use crate::content::{ContentView, StatusBonuses};
    use crate::{AbilityDef, AbilityId, StatusDef, StatusId};

    /// No-op `ContentView` for state-level unit tests. `start_actor_turn` and
    /// `tick_actor_statuses` take `&dyn ContentView` because their generic
    /// effect-pump may eventually consult content; the state tests below don't
    /// exercise those branches and need only the trait to be satisfied.
    struct StubContent;

    impl ContentView for StubContent {
        fn status_bonuses(&self, _: &StatusId) -> StatusBonuses { StatusBonuses::default() }
        fn ability_def(&self, _: &AbilityId) -> Option<AbilityDef> { None }
        fn status_def(&self, _: &StatusId) -> Option<StatusDef> {
            Some(StatusDef {
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                forces_targeting: false,
                skips_turn: false,
                armor_bonus: 0,
                damage_taken_bonus: 0,
                speed_bonus: 0,
                hp_percent_dot: 0,
            })
        }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> { None }
    }

    fn make_unit(id: UnitId, action_points: i32, max_ap: i32, mana: Option<Pool>) -> Unit {
        Unit {
            id,
            team: Team::Player,
            pos: Hex::ZERO,
            hp: 10,
            max_hp: 10,
            armor: 0,
            armor_bonus: 0,
            base_speed: 3,
            speed: 3,
            action_points,
            max_ap,
            movement_points: 3,
            reactions_left: 1,
            reactions_max: 1,
            statuses: vec![],
            rage: None,
            mana,
            energy: None,
            summoner: None,
            caster_context: Default::default(),
            aoo_dice: None,
            auras: Vec::new(),
            enemy_phases: Vec::new(),
        }
    }

    fn make_status(id: &str, applier: UnitId, rounds: u32, dot: i32) -> ActiveStatus {
        ActiveStatus {
            id: StatusId(id.into()),
            rounds_remaining: rounds,
            dot_per_tick: dot,
            applier,
        }
    }

    #[test]
    fn start_actor_turn_refills_ap_and_regens_mana() {
        let uid = UnitId(1);
        let mut unit = make_unit(uid, 0, 2, Some((1, 10)));
        unit.movement_points = 0; // depleted from previous turn
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        let u = state.unit(uid).unwrap();
        assert_eq!(u.action_points, 2);
        assert_eq!(u.movement_points, 3, "MP refilled to speed");
        assert_eq!(u.mana, Some((2, 10)));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            Event::ManaRegenerated { unit: UnitId(1), current: 2, max: 10 }
        ));
    }

    #[test]
    fn start_actor_turn_refills_movement_points_to_speed() {
        let uid = UnitId(11);
        let mut unit = make_unit(uid, 0, 2, None);
        unit.base_speed = 4;
        unit.speed = 4;
        unit.movement_points = 0;
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        state.start_actor_turn(uid, &content);

        assert_eq!(state.unit(uid).unwrap().movement_points, 4);
    }

    #[test]
    fn start_actor_turn_refills_mp_to_effective_speed_including_bonus() {
        // When a status grants +2 speed_bonus, u.speed = base_speed + bonus.
        // start_actor_turn must refill to u.speed, not u.base_speed.
        let uid = UnitId(12);
        let mut unit = make_unit(uid, 0, 2, None);
        unit.base_speed = 3;
        unit.speed = 5; // reflects status speed_bonus of +2
        unit.movement_points = 0;
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        state.start_actor_turn(uid, &content);

        assert_eq!(state.unit(uid).unwrap().movement_points, 5,
            "should refill to effective speed, not base_speed");
    }

    #[test]
    fn start_actor_turn_mana_clamps_at_max() {
        let uid = UnitId(2);
        let unit = make_unit(uid, 0, 1, Some((10, 10)));
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        assert_eq!(state.unit(uid).unwrap().mana, Some((10, 10)));
        assert!(events.is_empty(), "no event when mana already at max");
    }

    #[test]
    fn start_actor_turn_skips_dead_unit_refills() {
        let uid = UnitId(3);
        let mut unit = make_unit(uid, 0, 2, Some((1, 10)));
        unit.hp = 0;
        unit.movement_points = 0;
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        let u = state.unit(uid).unwrap();
        assert_eq!(u.action_points, 0, "dead unit AP unchanged");
        assert_eq!(u.movement_points, 0, "dead unit MP unchanged");
        assert!(events.is_empty(), "no refill events and no statuses to tick");
    }

    #[test]
    fn start_actor_turn_ticks_dot_on_victims() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 20;
        victim_unit.max_hp = 20;
        victim_unit.statuses.push(make_status("burning", applier, 3, 3));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let ticked = events.iter().any(|e| matches!(e,
            Event::StatusTicked { target, status, .. }
            if *target == victim && status.0 == "burning"
        ));
        let damaged = events.iter().any(|e| matches!(e,
            Event::UnitDamaged { target, amount, .. }
            if *target == victim && *amount == 3
        ));
        assert!(ticked, "StatusTicked expected");
        assert!(damaged, "UnitDamaged(3) expected");
        assert_eq!(state.unit(victim).unwrap().hp, 17);
        assert_eq!(state.unit(victim).unwrap().statuses[0].rounds_remaining, 2);
    }

    #[test]
    fn start_actor_turn_expires_status_on_last_tick() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 20;
        victim_unit.max_hp = 20;
        victim_unit.statuses.push(make_status("burning", applier, 1, 3));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let removed = events.iter().any(|e| matches!(e,
            Event::StatusRemoved { target, status }
            if *target == victim && status.0 == "burning"
        ));
        assert!(removed, "StatusRemoved expected on last tick");
        assert!(state.unit(victim).unwrap().statuses.is_empty(), "status cleared from unit");
        assert_eq!(state.unit(victim).unwrap().hp, 17);
    }

    #[test]
    fn start_actor_turn_for_dead_applier_still_ticks_sirota() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let mut applier_unit = make_unit(applier, 0, 2, Some((1, 10)));
        applier_unit.hp = 0;
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 20;
        victim_unit.max_hp = 20;
        victim_unit.statuses.push(make_status("poison", applier, 2, 4));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let no_mana_regen = !events.iter().any(|e| matches!(e, Event::ManaRegenerated { .. }));
        assert!(no_mana_regen, "dead applier must not regen mana");
        let damaged = events.iter().any(|e| matches!(e,
            Event::UnitDamaged { target, amount, .. }
            if *target == victim && *amount == 4
        ));
        assert!(damaged, "tick still fires for dead applier");
        assert_eq!(state.unit(victim).unwrap().hp, 16);
    }

    #[test]
    fn start_actor_turn_dot_lethal_emits_death_and_cleans_local_statuses() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 1;
        victim_unit.max_hp = 20;
        victim_unit.statuses.push(make_status("burning", applier, 3, 5));
        victim_unit.statuses.push(make_status("slowed", UnitId(99), 2, 0));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let died = events.iter().any(|e| matches!(e, Event::UnitDied { unit } if *unit == victim));
        assert!(died, "UnitDied expected when DoT is lethal");
        assert_eq!(state.unit(victim).unwrap().hp, 0);
        assert!(state.unit(victim).unwrap().statuses.is_empty(), "death clears local statuses");
    }

    #[test]
    fn start_actor_turn_no_statuses_returns_only_refill_events() {
        let uid = UnitId(1);
        let unit = make_unit(uid, 0, 2, Some((5, 10)));
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::ManaRegenerated { .. }));
    }
}
