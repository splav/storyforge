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
    /// Incoming-damage multiplier bonus from active statuses (recomputed by
    /// `RefreshAggregates`). Positive = unit takes more damage (vulnerability).
    /// Mirrors `UnitSnapshot.damage_taken_bonus`; kept in sync via the engine's
    /// aggregate refresh.
    #[serde(default)]
    pub damage_taken_bonus: i32,
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

    /// **Phase C-2 parallel-shape.** New unified resource table. Currently
    /// populated alongside legacy fields; not yet read by any code path.
    /// C3 migrates readers; C5 removes legacy fields.
    ///
    /// Iteration order: `Mana, Rage, Energy, Ap, Mp` (declaration order of
    /// `PoolKind`). Load-bearing for replay-trace determinism.
    ///
    /// **Invariants:**
    /// - `pools[Mana]`/`pools[Rage]`/`pools[Energy]`: Some iff legacy field is Some.
    /// - `pools[Ap]`/`pools[Mp]`: Some for every alive combat unit; None
    ///   reserved for future non-combatant entities (none exist today).
    #[serde(default)]
    pub pools: enum_map::EnumMap<crate::PoolKind, Option<(i32, i32)>>,

    /// **Phase C-2 parallel-shape.** Per-pool turn-start regen policy
    /// copied from `UnitTemplate.regen_per_pool` at spawn. Currently unused
    /// by `start_actor_turn`; C3 wires the unified regen loop.
    #[serde(default)]
    pub regen_per_pool: enum_map::EnumMap<crate::PoolKind, crate::RegenRule>,
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
///
/// Serialization skips `idx` (it is a pure cache); deserialization rebuilds
/// it automatically via the `From<CombatStateRepr>` conversion.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(into = "CombatStateRepr")]
pub struct CombatState {
    units: Vec<Unit>,
    /// `UnitId → index` in `units`. Rebuilt by `rebuild_idx` after bulk mutations.
    /// Skipped during serialization (derived from `units`).
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

/// Wire format for `CombatState` — identical layout except `idx` is absent.
/// Used by `serde(into/from)` so the index cache is automatically rebuilt on
/// deserialization without a custom `Deserialize` impl.
#[derive(serde::Serialize, serde::Deserialize)]
struct CombatStateRepr {
    units: Vec<Unit>,
    pub round: u32,
    pub phase: RoundPhase,
    pub turn_queue: TurnQueue,
    pub random_seed: u64,
    next_synthetic_uid: u64,
}

impl From<CombatState> for CombatStateRepr {
    fn from(s: CombatState) -> Self {
        CombatStateRepr {
            units: s.units,
            round: s.round,
            phase: s.phase,
            turn_queue: s.turn_queue,
            random_seed: s.random_seed,
            next_synthetic_uid: s.next_synthetic_uid,
        }
    }
}

impl From<CombatStateRepr> for CombatState {
    fn from(r: CombatStateRepr) -> Self {
        let mut s = CombatState {
            units: r.units,
            idx: HashMap::new(),
            round: r.round,
            phase: r.phase,
            turn_queue: r.turn_queue,
            random_seed: r.random_seed,
            next_synthetic_uid: r.next_synthetic_uid,
        };
        s.rebuild_idx();
        s
    }
}

impl<'de> serde::Deserialize<'de> for CombatState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(CombatState::from(CombatStateRepr::deserialize(d)?))
    }
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
                use crate::{PoolKind, RegenRule};

                // Capture effective speed before the mutable pool iteration —
                // needed to sync pools[Mp].max for RefillToMax (mirrors prior
                // `u.movement_points = u.speed` behavior).
                let effective_speed = u.speed;

                // Unified regen loop: iteration order is load-bearing for
                // determinism (Mana, Rage, Energy, Ap, Mp).
                for (kind, rule) in u.regen_per_pool.iter() {
                    let Some((cur, max)) = u.pools[kind].as_mut() else { continue };
                    match rule {
                        RegenRule::None => {}
                        RegenRule::Increment(amount) => {
                            let new = (*cur + amount).min(*max);
                            if new != *cur {
                                *cur = new;
                                // Emit legacy per-pool events for Mana/Energy.
                                match kind {
                                    PoolKind::Mana => events.push(Event::ManaRegenerated {
                                        unit: actor,
                                        current: new,
                                        max: *max,
                                    }),
                                    PoolKind::Energy => events.push(Event::EnergyRegenerated {
                                        unit: actor,
                                        current: new,
                                        max: *max,
                                    }),
                                    // No legacy event for other Increment pools (none today).
                                    _ => {}
                                }
                            }
                        }
                        RegenRule::RefillToMax => {
                            // For Mp, the effective max is the unit's current speed
                            // (which includes status/aura bonuses via RefreshAggregates),
                            // not the stale pool max. Sync it here so refill matches
                            // the prior `u.movement_points = u.speed` behavior.
                            if kind == PoolKind::Mp {
                                *max = effective_speed;
                            }
                            *cur = *max; // unconditional
                        }
                    }
                }

                // Mirror back to legacy fields — mandatory until C5 removes them.
                // Regen happens via pools first; legacy fields are then synced.
                if let Some((cur, max)) = u.pools[PoolKind::Mana] {
                    u.mana = Some((cur, max));
                }
                if let Some((cur, max)) = u.pools[PoolKind::Rage] {
                    u.rage = Some((cur, max));
                }
                if let Some((cur, max)) = u.pools[PoolKind::Energy] {
                    u.energy = Some((cur, max));
                }
                if let Some((cur, _max)) = u.pools[PoolKind::Ap] {
                    u.action_points = cur;
                }
                if let Some((cur, _max)) = u.pools[PoolKind::Mp] {
                    u.movement_points = cur;
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
                // Fold all bonuses (speed, armor, damage_taken) and flags via one call.
                let b = content.status_bonuses(&aura.status_id);
                out.speed_bonus         += b.speed_bonus;
                out.armor_bonus         += b.armor_bonus;
                out.damage_taken_bonus  += b.damage_taken_bonus;
                if let Some(def) = content.status_def(&aura.status_id) {
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

    static STUB_STATUS_DEF: StatusDef = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 0 },
        hp_percent_dot: 0,
    };

    impl ContentView for StubContent {
        fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
        fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
            Some(&STUB_STATUS_DEF)
        }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> { None }
    }

    fn make_unit(id: UnitId, action_points: i32, max_ap: i32, mana: Option<Pool>) -> Unit {
        use crate::{PoolKind, RegenRule};
        let pools = enum_map::enum_map! {
            PoolKind::Mana   => mana,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((action_points, max_ap)),
            PoolKind::Mp     => Some((3, 3)),
        };
        Unit {
            id,
            team: Team::Player,
            pos: Hex::ZERO,
            hp: 10,
            max_hp: 10,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
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
            pools,
            regen_per_pool: enum_map::enum_map! {
                PoolKind::Mana   => RegenRule::Increment(1),
                PoolKind::Rage   => RegenRule::None,
                PoolKind::Energy => RegenRule::Increment(1),
                PoolKind::Ap     => RegenRule::RefillToMax,
                PoolKind::Mp     => RegenRule::RefillToMax,
            },
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

        // Fused DotDamaged event replaces the old StatusTicked + UnitDamaged pair.
        let dot_ev = events.iter().find(|e| matches!(e,
            Event::DotDamaged { target, source_status, amount, .. }
            if *target == victim && source_status.0 == "burning" && *amount == 3
        ));
        assert!(dot_ev.is_some(), "DotDamaged(target=victim, status=burning, amount=3) expected");

        // No standalone StatusTicked for the same tick (would indicate regression to old pair).
        let ticked = events.iter().any(|e| matches!(e,
            Event::StatusTicked { target, status, .. }
            if *target == victim && status.0 == "burning"
        ));
        assert!(!ticked, "StatusTicked must NOT appear for a damaging tick (regression guard)");

        // No standalone UnitDamaged for the same tick target (fusion guard).
        let standalone_damaged = events.iter().any(|e| matches!(e,
            Event::UnitDamaged { target, .. }
            if *target == victim
        ));
        assert!(!standalone_damaged, "standalone UnitDamaged must NOT appear for a DoT tick (regression guard)");

        assert_eq!(state.unit(victim).unwrap().hp, 17, "HP should be reduced by 3");
        assert_eq!(state.unit(victim).unwrap().statuses[0].rounds_remaining, 2, "rounds_remaining decremented");
    }

    /// A buff-only status (dot_per_tick=0, hp_percent_dot=0) emits `StatusTicked`
    /// and does NOT reduce HP.
    #[test]
    fn zero_damage_status_tick_still_emits_status_ticked() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 10;
        victim_unit.max_hp = 10;
        // dot_per_tick = 0 → zero-damage tick; StubContent has hp_percent_dot = 0.
        victim_unit.statuses.push(make_status("haste", applier, 2, 0));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let ticked = events.iter().any(|e| matches!(e,
            Event::StatusTicked { target, status, .. }
            if *target == victim && status.0 == "haste"
        ));
        assert!(ticked, "StatusTicked expected for zero-damage buff tick");

        // No DotDamaged emitted for a zero-damage tick.
        let dot = events.iter().any(|e| matches!(e, Event::DotDamaged { .. }));
        assert!(!dot, "DotDamaged must NOT appear for a zero-damage buff tick");

        // HP untouched.
        assert_eq!(state.unit(victim).unwrap().hp, 10, "HP must be unchanged for zero-damage tick");
    }

    /// Two different DoT statuses on the same victim (both from the same applier)
    /// each produce their own `DotDamaged` event — no cross-contamination.
    #[test]
    fn multiple_dot_statuses_each_emit_own_dot_damaged() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.hp = 20;
        victim_unit.max_hp = 20;
        victim_unit.statuses.push(make_status("poison", applier, 2, 3));
        victim_unit.statuses.push(make_status("burning", applier, 2, 2));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let poison_ev = events.iter().any(|e| matches!(e,
            Event::DotDamaged { target, source_status, amount, .. }
            if *target == victim && source_status.0 == "poison" && *amount == 3
        ));
        let burning_ev = events.iter().any(|e| matches!(e,
            Event::DotDamaged { target, source_status, amount, .. }
            if *target == victim && source_status.0 == "burning" && *amount == 2
        ));
        assert!(poison_ev, "DotDamaged(poison, 3) expected");
        assert!(burning_ev, "DotDamaged(burning, 2) expected");

        // HP reduced by both: 20 - 3 - 2 = 15.
        assert_eq!(state.unit(victim).unwrap().hp, 15, "HP reduced by both DoT ticks");
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

    /// ContentView stub that returns a StatusDef with damage_taken_bonus = 2
    /// for any status id, used for the aggregate-refresh unit test.
    struct VulnContent;
    static VULN_STATUS_DEF: StatusDef = StatusDef {
        causes_disadvantage: false,
        blocks_mana_abilities: false,
        forces_targeting: false,
        skips_turn: false,
        bonuses: StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 2 },
        hp_percent_dot: 0,
    };
    impl ContentView for VulnContent {
        fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
        fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { Some(&VULN_STATUS_DEF) }
        fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> { None }
    }

    #[test]
    fn refresh_aggregates_recomputes_damage_taken_bonus() {
        // A unit with one active status that carries damage_taken_bonus = 2.
        // After RefreshAggregates fires, unit.damage_taken_bonus must equal 2.
        use crate::effect::{apply_effect, Effect};
        let uid = UnitId(1);
        let mut unit = make_unit(uid, 0, 2, None);
        unit.statuses.push(ActiveStatus {
            id: StatusId("vuln".into()),
            rounds_remaining: 3,
            dot_per_tick: 0,
            applier: uid,
        });
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = VulnContent;

        apply_effect(&mut state, &Effect::RefreshAggregates { unit: uid }, &content);

        assert_eq!(
            state.unit(uid).unwrap().damage_taken_bonus,
            2,
            "damage_taken_bonus must reflect the active status bonus after RefreshAggregates"
        );
    }


    #[test]
    fn pools_shape_matches_legacy_fields_after_spawn() {
        // Verify Phase C-2 invariant: Unit.pools tracks legacy resource fields 1:1.
        // Uses make_unit (which populates both shapes) as the test fixture.
        use crate::PoolKind;

        // Unit with mana set — exercises all five pools.
        let mana: Pool = (5, 10);
        let u = make_unit(UnitId(1), 2, 3, Some(mana));

        // Mana: pools[Mana] == mana
        assert_eq!(u.pools[PoolKind::Mana], u.mana, "pools[Mana] must mirror legacy mana");

        // Rage: both None (make_unit sets no rage)
        assert_eq!(u.pools[PoolKind::Rage], u.rage, "pools[Rage] must mirror legacy rage");

        // Energy: both None (make_unit sets no energy)
        assert_eq!(u.pools[PoolKind::Energy], u.energy, "pools[Energy] must mirror legacy energy");

        // Ap: pools[Ap] == Some((action_points, max_ap))
        assert_eq!(
            u.pools[PoolKind::Ap],
            Some((u.action_points, u.max_ap)),
            "pools[Ap] must mirror legacy action_points/max_ap",
        );

        // Mp: pools[Mp] == Some((movement_points, movement_points))
        // mp-max == current at spawn (speed-derived, no separate max field yet)
        assert_eq!(
            u.pools[PoolKind::Mp],
            Some((u.movement_points, u.movement_points)),
            "pools[Mp] must mirror legacy movement_points",
        );
    }

    /// C3: verify unified regen loop drives all 5 pools correctly.
    ///
    /// - Mana/Energy: incremented by 1, legacy fields mirrored.
    /// - Ap/Mp: refilled to max, legacy fields mirrored.
    /// - Rage: skipped (RegenRule::None), unchanged.
    /// - Legacy fields match pools after the call.
    #[test]
    fn unified_regen_loop_increments_mana_energy_refills_ap_mp_skips_rage() {
        use crate::PoolKind;

        let uid = UnitId(42);
        // Start with all resources partially spent / not full.
        let mut unit = make_unit(uid, 1, 3, Some((4, 10))); // ap=1/3, mana=4/10
        unit.energy = Some((2, 8));
        unit.rage   = Some((3, 6));
        // Sync pools to match (make_unit only sets pools for mana; update others).
        unit.pools[PoolKind::Energy] = Some((2, 8));
        unit.pools[PoolKind::Rage]   = Some((3, 6));
        // ap already set via make_unit: pools[Ap] = Some((1,3)), action_points=1
        // mp: make_unit sets pools[Mp]=Some((3,3)), movement_points=3 — spend 1
        unit.movement_points = 2;
        unit.pools[PoolKind::Mp] = Some((2, 3));
        // Set regen rules: Mana/Energy increment, Ap/Mp refill, Rage none.
        use crate::RegenRule;
        unit.regen_per_pool[PoolKind::Mana]   = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Rage]   = RegenRule::None;
        unit.regen_per_pool[PoolKind::Energy] = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Ap]     = RegenRule::RefillToMax;
        unit.regen_per_pool[PoolKind::Mp]     = RegenRule::RefillToMax;

        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);
        let u = state.unit(uid).unwrap();

        // Mana: 4 → 5 (incremented), legacy mirrored.
        assert_eq!(u.pools[PoolKind::Mana], Some((5, 10)), "pools[Mana] must increment");
        assert_eq!(u.mana, Some((5, 10)), "legacy mana must mirror pools[Mana]");

        // Energy: 2 → 3 (incremented), legacy mirrored.
        assert_eq!(u.pools[PoolKind::Energy], Some((3, 8)), "pools[Energy] must increment");
        assert_eq!(u.energy, Some((3, 8)), "legacy energy must mirror pools[Energy]");

        // Rage: unchanged at 3 (RegenRule::None).
        assert_eq!(u.pools[PoolKind::Rage], Some((3, 6)), "pools[Rage] must not change");
        assert_eq!(u.rage, Some((3, 6)), "legacy rage must mirror pools[Rage]");

        // Ap: refilled to max=3.
        assert_eq!(u.pools[PoolKind::Ap], Some((3, 3)), "pools[Ap] must refill to max");
        assert_eq!(u.action_points, 3, "legacy action_points must mirror pools[Ap]");

        // Mp: refilled to max=3.
        assert_eq!(u.pools[PoolKind::Mp], Some((3, 3)), "pools[Mp] must refill to max");
        assert_eq!(u.movement_points, 3, "legacy movement_points must mirror pools[Mp]");

        // Events: ManaRegenerated + EnergyRegenerated (Mana first, then Energy — iteration order).
        let event_kinds: Vec<&str> = events.iter().map(|e| match e {
            Event::ManaRegenerated { .. }   => "mana",
            Event::EnergyRegenerated { .. } => "energy",
            _                               => "other",
        }).collect();
        assert_eq!(event_kinds, vec!["mana", "energy"], "events must be Mana then Energy");
    }
}
