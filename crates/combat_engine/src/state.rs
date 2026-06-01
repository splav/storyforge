//! `CombatState` — canonical in-engine battle state.
//!
//! Uses `Vec<Unit>` for deterministic iteration order (critical for replay)
//! with a `HashMap<UnitId, usize>` index for O(1) lookup. (Decision 6.1.)
//!
//! `UnitId(u64)` is an opaque new-type; the Entity↔UnitId mapping lives in
//! `crate::combat::engine_bridge` (the Bevy boundary). (Decision 6.2.)

use std::collections::{HashMap, HashSet};

use hexx::Hex;

use crate::content::{ContentView, UnitTemplate};
use crate::event::Event;
use crate::turn_queue::TurnQueue;
use crate::StatusId;

// ── Identity ──────────────────────────────────────────────────────────────────

/// Opaque unit identifier inside the engine.  Maps 1-to-1 with a Bevy
/// `Entity` via `crate::combat::engine_bridge::UnitIdMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct UnitId(pub u64);

/// Opaque environment-object identifier. Used as the payload for
/// `EffectSource::Env`. No environment objects are constructed yet —
/// this variant is reserved for the trap/hazard system (a later commit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
pub struct EnvId(pub u32);

/// Broad category of environment object.  One variant for now; more expected
/// (e.g. `Terrain`, `Interactive`) as the env system grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EnvKind {
    Hazard,
}

/// A passive environment object placed on the grid (trap, hazard, etc.).
///
/// `ability` identifies which `AbilityDef` to resolve when the trap fires;
/// the damage/status fanout is driven by that definition.
///
/// **One-shot.** A hazard fires once and is then **removed** from
/// `CombatState.environment` (it deals its effect and disappears — no
/// lingering "spent" marker). There is therefore no `triggered` flag.
///
/// `revealed` gates visibility of an *armed* object: `false` = hidden (not
/// rendered; absent from AI snapshots, so the AI can be baited onto it),
/// `true` = known to the party (rendered in UI, present in AI planning so the
/// AI avoids it). It is flipped by the reveal mechanic (e.g. a scout spotting
/// traps) — NOT by firing, since a fired trap is gone.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnvObject {
    pub id: EnvId,
    pub hex: hexx::Hex,
    pub kind: EnvKind,
    pub ability: crate::AbilityId,
    pub revealed: bool,
}

/// Who produced a damage or status effect — either a living unit or an
/// environment object (trap, hazard, etc.).
///
/// `EffectSource::Env` is defined here for forward-compatibility but is
/// **never constructed** in this commit; it arrives with the trap system.
/// Consumers that cannot handle env sources yet should call
/// `.as_unit()` and `unreachable!` / skip on `None`.
///
/// **Backward-compatible deserialization.** Before schema v45 this field was a
/// bare `UnitId` (serialized as an integer). The hand-written `Deserialize`
/// below accepts BOTH the legacy bare-integer form (→ `Unit`) and the current
/// externally-tagged enum form (`{"Unit": n}` / `{"Env": n}`), so v43/v44 logs
/// and engine traces keep parsing without a fixture rebuild — mirroring the
/// `de_legacy_pool` approach used for legacy resource pools. Serialization is
/// always the new tagged form (writes are v45).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum EffectSource {
    Unit(UnitId),
    Env(EnvId),
}

impl EffectSource {
    /// Returns `Some(UnitId)` if this source is a unit, `None` if it is
    /// an environment object.
    pub fn as_unit(self) -> Option<UnitId> {
        match self {
            EffectSource::Unit(u) => Some(u),
            EffectSource::Env(_) => None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for EffectSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EffectSourceVisitor;

        impl<'de> serde::de::Visitor<'de> for EffectSourceVisitor {
            type Value = EffectSource;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a legacy bare unit id (integer) or an EffectSource enum map")
            }

            // Legacy form: a bare integer was the old `UnitId` source.
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<EffectSource, E> {
                Ok(EffectSource::Unit(UnitId(v)))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<EffectSource, E> {
                if v < 0 {
                    return Err(E::custom("negative unit id in legacy EffectSource"));
                }
                Ok(EffectSource::Unit(UnitId(v as u64)))
            }

            // Current form: externally-tagged enum `{"Unit": n}` / `{"Env": n}`.
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<EffectSource, A::Error> {
                #[derive(serde::Deserialize)]
                enum Tagged {
                    Unit(UnitId),
                    Env(EnvId),
                }
                let tagged = <Tagged as serde::Deserialize>::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )?;
                Ok(match tagged {
                    Tagged::Unit(u) => EffectSource::Unit(u),
                    Tagged::Env(e) => EffectSource::Env(e),
                })
            }
        }

        deserializer.deserialize_any(EffectSourceVisitor)
    }
}

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
    pub applier: EffectSource,
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

/// A single combat participant in `CombatState`.
///
/// **HP layout (post Stage 3c / v44):** HP is stored exclusively in
/// `pools[PoolKind::Hp]` — `(current_hp, max_hp)`. The legacy `hp` and
/// `max_hp` fields were removed; use the `Unit::hp()` / `Unit::max_hp()`
/// accessors. All other resource pools (`Mana`, `Rage`, `Energy`, `Ap`, `Mp`)
/// follow the same `pools` layout.
///
/// Serialization goes through `UnitWire` (serde `into`). Pre-v44 traces
/// that contain legacy `hp`/`max_hp` fields are back-compat read via
/// `#[serde(default)]` on those fields in `UnitWire`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(into = "UnitWire")]
pub struct Unit {
    pub id: UnitId,
    pub team: Team,
    pub pos: Hex,
    /// Base armor value (equipment). Bonus from statuses is tracked separately
    /// and folded in by `refresh_aggregates`.
    pub armor: i32,
    /// Armor bonus from active statuses (recomputed by `RefreshAggregates`).
    pub armor_bonus: i32,
    /// Incoming-damage multiplier bonus from active statuses (recomputed by
    /// `RefreshAggregates`). Positive = unit takes more damage (vulnerability).
    /// Mirrors `UnitSnapshot.damage_taken_bonus`; kept in sync via the engine's
    /// aggregate refresh.
    pub damage_taken_bonus: i32,
    /// Base speed (without status speed_bonus).
    pub base_speed: i32,
    /// Effective speed = base_speed + speed bonuses from statuses.
    pub speed: i32,
    pub reactions_left: i32,
    /// Maximum reactions per round. Populated by the bridge from `Reactions.max`.
    pub reactions_max: i32,
    pub statuses: Vec<ActiveStatus>,
    /// Set when this unit was spawned via `Effect::Spawn`. `None` for units
    /// present at combat start (loaded from ECS).
    pub summoner: Option<UnitId>,
    /// Rolled initiative value for turn-order resolution.
    /// `None` = "not yet rolled" (initial state; roller populates this in a
    /// later wave). Present in serialized state for replay determinism.
    pub initiative: Option<i32>,
    /// Resolved caster stats (weapon dice, modifiers, crit-fail outcome).
    /// Populated at combat init from `Equipment` + `CombatStats` ECS components.
    /// Used by the Cast fanout (damage / heal formulas).
    pub caster_context: crate::content::CasterContext,
    /// AoO dice for this unit, if it can perform opportunity attacks.
    /// `Some(dice)` iff the unit has a melee `WeaponAttack` ability and an
    /// equipped weapon; bonus already includes the strength modifier.
    /// `None` means "cannot AoO" — distinct from `caster_context.weapon_dice`,
    /// which carries the raw weapon dice used for Cast damage rolls (ranged
    /// units have weapon_dice but no aoo_dice).
    pub aoo_dice: Option<crate::dice::DiceExpr>,
    /// Passive aura definitions emitted by this unit.
    /// Populated at combat init from the `AuraSource` ECS component.
    /// Empty for units with no auras.
    pub auras: Vec<crate::content::AuraDef>,
    /// Pending phase-transition thresholds for this unit (boss-only).
    /// First entry = next phase to trigger. Bridge translator reads this to
    /// write ECS-only deltas on `Event::PhaseEntered`. Empty for non-bosses.
    pub enemy_phases: Vec<crate::content::PhaseEntry>,

    /// Unified resource table. Canonical source of truth for all resource pools
    /// (Hp, Mana, Rage, Energy, Ap, Mp) since Stage 3c.
    ///
    /// Iteration order: `Hp, Mana, Rage, Energy, Ap, Mp` (declaration order of
    /// `PoolKind`). Load-bearing for replay-trace determinism.
    ///
    /// **Invariants:**
    /// - `pools[Hp]`: Some for every combat unit; `(current, max)`.
    /// - `pools[Mana]`/`pools[Rage]`/`pools[Energy]`: Some iff the unit has
    ///   that resource mechanic.
    /// - `pools[Ap]`/`pools[Mp]`: Some for every alive combat unit; None
    ///   reserved for future non-combatant entities (none exist today).
    pub pools: enum_map::EnumMap<crate::PoolKind, Option<(i32, i32)>>,

    /// Per-pool turn-start regen policy copied from `UnitTemplate.regen_per_pool`
    /// at spawn.
    pub regen_per_pool: enum_map::EnumMap<crate::PoolKind, crate::RegenRule>,

    /// Id of the `UnitTemplate` this unit was spawned from (template-flow only).
    /// `None` for class-based heroes and enemies without an explicit template.
    /// Used by `CombatState::apply_initial_statuses` to look up statuses that
    /// must be applied at combat start (engine-side, idempotent).
    pub template_id: Option<String>,

    /// Passive ability ids for this unit.
    ///
    /// Populated transiently at combat init from ECS `Abilities` component
    /// (filtered to abilities with `passive.is_some()`).  Not serialized to
    /// trace files — defaults to empty on deserialization.  The engine reads
    /// this list in `resolve_turn_start_passives` to auto-fire passives.
    pub passives: Vec<crate::AbilityId>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UnitWire {
    pub id: UnitId,
    pub team: Team,
    pub pos: Hex,
    pub armor: i32,
    pub armor_bonus: i32,
    #[serde(default)]
    pub damage_taken_bonus: i32,
    pub base_speed: i32,
    pub speed: i32,
    pub reactions_left: i32,
    pub reactions_max: i32,
    pub statuses: Vec<ActiveStatus>,
    pub summoner: Option<UnitId>,
    /// Rolled initiative value; `None` until the initiative roller fires.
    /// Serialized so replays preserve the rolled order.
    #[serde(default)]
    pub initiative: Option<i32>,
    #[serde(default)]
    pub caster_context: crate::content::CasterContext,
    #[serde(default)]
    pub aoo_dice: Option<crate::dice::DiceExpr>,
    #[serde(default)]
    pub auras: Vec<crate::content::AuraDef>,
    #[serde(default)]
    pub enemy_phases: Vec<crate::content::PhaseEntry>,

    // ── canonical pools (Stage 3c+) ─────────────────────────────────────────
    // pools[Hp] is the canonical HP representation.
    #[serde(default)]
    pub pools: enum_map::EnumMap<crate::PoolKind, Option<(i32, i32)>>,
    #[serde(default)]
    pub regen_per_pool: enum_map::EnumMap<crate::PoolKind, crate::RegenRule>,

    // ── template id (optional; absent in older traces) ──────────────────────
    #[serde(default)]
    pub template_id: Option<String>,

    // ── passives (transient; not serialized to trace files) ─────────────────
    // Always defaulted to empty on deserialization — the bridge re-populates
    // this from ECS at combat init, so stored traces round-trip cleanly.
    #[serde(default, skip_serializing)]
    pub passives: Vec<crate::AbilityId>,

    // ── legacy fields (pre-C6, for backward-compat deserialization only) ───
    // Silently ignored on read if `pools` is already populated.
    #[serde(default)]
    action_points: Option<i32>,
    #[serde(default)]
    max_ap: Option<i32>,
    #[serde(default)]
    movement_points: Option<i32>,
    // Stored as Option<(i32, i32)> in old JSON: [current, max]
    #[serde(default, deserialize_with = "de_legacy_pool")]
    mana: Option<(i32, i32)>,
    #[serde(default, deserialize_with = "de_legacy_pool")]
    rage: Option<(i32, i32)>,
    #[serde(default, deserialize_with = "de_legacy_pool")]
    energy: Option<(i32, i32)>,

    // ── legacy hp fields (pre-Stage-3c, for backward-compat only) ──────────
    // If pools[Hp] is absent (old traces), these fields are used to populate it.
    // On new traces, hp/max_hp are absent; pools[Hp] is canonical.
    #[serde(default)]
    hp: Option<i32>,
    #[serde(default)]
    max_hp: Option<i32>,
}

/// Deserializes a legacy pool field from either a JSON array `[cur, max]`
/// or `null`. Pre-C6 fixtures serialized `mana`/`rage`/`energy` as arrays.
fn de_legacy_pool<'de, D>(d: D) -> Result<Option<(i32, i32)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let raw: Option<serde_json::Value> = Option::deserialize(d)?;
    match raw {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(arr)) if arr.len() == 2 => {
            let cur = arr[0].as_i64().ok_or_else(|| serde::de::Error::custom("expected int"))? as i32;
            let max = arr[1].as_i64().ok_or_else(|| serde::de::Error::custom("expected int"))? as i32;
            Ok(Some((cur, max)))
        }
        _ => Ok(None),
    }
}

impl From<UnitWire> for Unit {
    fn from(w: UnitWire) -> Self {
        use crate::PoolKind;
        let mut pools = w.pools;
        // Migrate legacy AP/MP if pools[Ap/Mp] absent (pre-C6 fixtures).
        if pools[PoolKind::Ap].is_none() {
            if let (Some(ap), Some(max)) = (w.action_points, w.max_ap) {
                pools[PoolKind::Ap] = Some((ap, max));
            }
        }
        if pools[PoolKind::Mp].is_none() {
            if let Some(mp) = w.movement_points {
                // Legacy had no max_mp field — use base_speed as max.
                pools[PoolKind::Mp] = Some((mp, w.base_speed));
            }
        }
        if pools[PoolKind::Mana].is_none() {
            if let Some(v) = w.mana {
                pools[PoolKind::Mana] = Some(v);
            }
        }
        if pools[PoolKind::Rage].is_none() {
            if let Some(v) = w.rage {
                pools[PoolKind::Rage] = Some(v);
            }
        }
        if pools[PoolKind::Energy].is_none() {
            if let Some(v) = w.energy {
                pools[PoolKind::Energy] = Some(v);
            }
        }
        // Stage 3c backward-compat: populate pools[Hp] from legacy hp/max_hp
        // fields if pools[Hp] is absent (pre-Stage-3c traces).
        if pools[PoolKind::Hp].is_none() {
            if let (Some(hp), Some(max_hp)) = (w.hp, w.max_hp) {
                pools[PoolKind::Hp] = Some((hp, max_hp));
            }
        }

        Unit {
            id: w.id,
            team: w.team,
            pos: w.pos,
            armor: w.armor,
            armor_bonus: w.armor_bonus,
            damage_taken_bonus: w.damage_taken_bonus,
            base_speed: w.base_speed,
            speed: w.speed,
            reactions_left: w.reactions_left,
            reactions_max: w.reactions_max,
            statuses: w.statuses,
            summoner: w.summoner,
            initiative: w.initiative,
            caster_context: w.caster_context,
            aoo_dice: w.aoo_dice,
            auras: w.auras,
            enemy_phases: w.enemy_phases,
            pools,
            regen_per_pool: w.regen_per_pool,
            template_id: w.template_id,
            // Passives are transient — always empty after deserialization.
            // The bridge re-populates them from ECS at combat init.
            passives: w.passives,
        }
    }
}

impl From<Unit> for UnitWire {
    fn from(u: Unit) -> Self {
        UnitWire {
            id: u.id,
            team: u.team,
            pos: u.pos,
            armor: u.armor,
            armor_bonus: u.armor_bonus,
            damage_taken_bonus: u.damage_taken_bonus,
            base_speed: u.base_speed,
            speed: u.speed,
            reactions_left: u.reactions_left,
            reactions_max: u.reactions_max,
            statuses: u.statuses,
            summoner: u.summoner,
            initiative: u.initiative,
            caster_context: u.caster_context,
            aoo_dice: u.aoo_dice,
            auras: u.auras,
            enemy_phases: u.enemy_phases,
            pools: u.pools,
            regen_per_pool: u.regen_per_pool,
            template_id: u.template_id,
            // Passives are transient; skip_serializing ensures they are never
            // written to trace files, so serde-default-to-empty is correct.
            passives: u.passives,
            // Legacy fields never written — only read for backward compat.
            action_points: None,
            max_ap: None,
            movement_points: None,
            mana: None,
            rage: None,
            energy: None,
            hp: None,
            max_hp: None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Unit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Unit::from(UnitWire::deserialize(d)?))
    }
}

impl Unit {
    /// Canonical constructor — the **only** place in the codebase that builds a
    /// `Unit` struct literal.  All other constructors and test helpers must call
    /// this function.
    ///
    /// `pools` must have `pools[PoolKind::Hp] = Some((hp, max_hp))` before
    /// calling; this invariant is enforced by a debug-mode assertion.
    ///
    /// # Panics (debug only)
    /// Panics if `pools[PoolKind::Hp]` is `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: UnitId,
        team: Team,
        pos: Hex,
        armor: i32,
        armor_bonus: i32,
        damage_taken_bonus: i32,
        base_speed: i32,
        speed: i32,
        reactions_left: i32,
        reactions_max: i32,
        statuses: Vec<ActiveStatus>,
        summoner: Option<UnitId>,
        initiative: Option<i32>,
        caster_context: crate::content::CasterContext,
        aoo_dice: Option<crate::dice::DiceExpr>,
        auras: Vec<crate::content::AuraDef>,
        enemy_phases: Vec<crate::content::PhaseEntry>,
        pools: enum_map::EnumMap<crate::PoolKind, Option<(i32, i32)>>,
        regen_per_pool: enum_map::EnumMap<crate::PoolKind, crate::RegenRule>,
        template_id: Option<String>,
    ) -> Self {
        debug_assert!(
            pools[crate::PoolKind::Hp].is_some(),
            "Unit::new requires pools[PoolKind::Hp] = Some((hp, max_hp))"
        );
        Unit {
            id,
            team,
            pos,
            armor,
            armor_bonus,
            damage_taken_bonus,
            base_speed,
            speed,
            reactions_left,
            reactions_max,
            statuses,
            summoner,
            initiative,
            caster_context,
            aoo_dice,
            auras,
            enemy_phases,
            pools,
            regen_per_pool,
            template_id,
            passives: Vec::new(),
        }
    }

    /// Returns `true` iff the unit's HP pool is `Some` and current HP > 0.
    /// Invariant: `pools[Hp]` is always `Some` for combat units; `None` is
    /// only possible for non-combatant entities (currently unused).
    pub fn is_alive(&self) -> bool {
        self.pools[crate::PoolKind::Hp].is_some_and(|(cur, _)| cur > 0)
    }

    /// Current HP — reads from `pools[PoolKind::Hp]`, the canonical source of
    /// truth since Stage 3c. Returns 0 if the pool is absent (should not
    /// occur for live combat units).
    pub fn hp(&self) -> i32 {
        self.pools[crate::PoolKind::Hp].map_or(0, |(c, _)| c)
    }

    /// Max HP — reads from `pools[PoolKind::Hp]`, the canonical source of
    /// truth since Stage 3c. Returns 0 if the pool is absent (should not
    /// occur for live combat units).
    pub fn max_hp(&self) -> i32 {
        self.pools[crate::PoolKind::Hp].map_or(0, |(_, m)| m)
    }

    /// Debug assertion: `pools[Hp]` must be `Some` for every live unit.
    ///
    /// After Stage 3c the legacy `hp`/`max_hp` fields are gone and
    /// `pools[Hp]` is the sole source of truth. This assertion guards
    /// against accidental `None` after an HP-mutating effect.
    #[inline]
    pub fn assert_hp_pool_sync(&self) {
        #[cfg(debug_assertions)]
        {
            assert!(
                self.pools[crate::PoolKind::Hp].is_some(),
                "pools[Hp] must be Some for every unit (Stage 3c invariant)"
            );
        }
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
    /// Static obstacles that block both movement and LOS.
    /// Added in Wave 1 ch2 (schema v43). Serialized as sorted Vec in CombatStateRepr.
    pub blocked_hexes: HashSet<hexx::Hex>,
    /// Active environmental objects (traps, hazards) placed on the grid.
    /// Added in commit B of the environmental-traps feature.
    /// Serialized as a Vec sorted by id for deterministic output (CombatStateRepr).
    pub environment: Vec<EnvObject>,
}

/// Wire format for `CombatState` — identical layout except `idx` is absent.
/// Used by `serde(into/from)` so the index cache is automatically rebuilt on
/// deserialization without a custom `Deserialize` impl.
///
/// `blocked_hexes` is serialized as a sorted `Vec<Hex>` for deterministic output.
#[derive(serde::Serialize, serde::Deserialize)]
struct CombatStateRepr {
    units: Vec<Unit>,
    pub round: u32,
    pub phase: RoundPhase,
    pub turn_queue: TurnQueue,
    pub random_seed: u64,
    next_synthetic_uid: u64,
    #[serde(default)]
    blocked_hexes: Vec<hexx::Hex>,
    #[serde(default)]
    environment: Vec<EnvObject>,
}

impl From<CombatState> for CombatStateRepr {
    fn from(s: CombatState) -> Self {
        // Sort blocked_hexes for deterministic serialization output.
        let mut blocked_hexes: Vec<hexx::Hex> = s.blocked_hexes.into_iter().collect();
        blocked_hexes.sort_by_key(|h| (h.x, h.y));
        // Sort environment by id for deterministic serialization output.
        let mut environment = s.environment;
        environment.sort_by_key(|e| e.id);
        CombatStateRepr {
            units: s.units,
            round: s.round,
            phase: s.phase,
            turn_queue: s.turn_queue,
            random_seed: s.random_seed,
            next_synthetic_uid: s.next_synthetic_uid,
            blocked_hexes,
            environment,
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
            blocked_hexes: r.blocked_hexes.into_iter().collect(),
            environment: r.environment,
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
            blocked_hexes: HashSet::new(),
            environment: Vec::new(),
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

    /// Apply `initial_statuses` from each unit's template at combat bootstrap.
    ///
    /// Called once from `bootstrap_combat_state` after `from_ecs` populates the
    /// engine state.  For every unit that carries a `template_id`, the method
    /// looks up the template via `content` and applies each listed status with
    /// `PERMANENT_DURATION` if the unit does not already have a status with that
    /// id (idempotency guard — safe to call twice in tests).
    ///
    /// The unit is the applier of its own initial statuses (permanent stun has no
    /// meaningful external source).
    pub fn apply_initial_statuses(&mut self, content: &dyn ContentView) {
        for unit in self.units.iter_mut() {
            let Some(ref tid) = unit.template_id.clone() else { continue };
            let Some(template) = content.unit_template(tid) else { continue };
            apply_template_initial_statuses(unit, &template);
        }
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
                                events.push(Event::PoolChanged {
                                    unit: actor,
                                    pool: kind,
                                    current: new,
                                    max: *max,
                                    cause: crate::PoolChangeCause::Regen,
                                });
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
                            // Emit-on-change only: emit PoolChanged{Refill} when
                            // AP/MP were spent (cur < max).
                            if *cur != *max {
                                *cur = *max;
                                events.push(Event::PoolChanged {
                                    unit: actor,
                                    pool: kind,
                                    current: *cur,
                                    max: *max,
                                    cause: crate::PoolChangeCause::Refill,
                                });
                            } else {
                                *cur = *max; // already at max, no event
                            }
                        }
                    }
                }
            }
        }

        events.extend(self.tick_actor_statuses(actor, content));
        events.extend(self.resolve_turn_start_passives(actor, content));
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
                    .filter(|s| s.applier == EffectSource::Unit(actor))
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

    /// Auto-fire all `TurnStart` passive abilities owned by `actor`.
    ///
    /// Modeled on `tick_actor_statuses`: pumps a local effect queue through
    /// the standard `apply_effect` cascade and returns the resulting event
    /// stream.  Zero rng, zero cost, no crit-fail.
    ///
    /// Called from `start_actor_turn` AFTER the status-tick phase so the
    /// event stream reads: regen → status ticks → passive reveals.
    ///
    /// Does nothing when `actor.passives` is empty (the common case for every
    /// non-passive unit).
    pub fn resolve_turn_start_passives(
        &mut self,
        actor: UnitId,
        content: &dyn crate::content::ContentView,
    ) -> Vec<crate::event::Event> {
        use crate::content::PassiveTrigger;
        self.resolve_passives(actor, content, &PassiveTrigger::TurnStart)
    }

    /// Auto-fire all `OnMove` passive abilities owned by `actor`.
    ///
    /// Mirrors `resolve_turn_start_passives` but fires on each completed
    /// move step rather than at turn start.  Called from `step_inner` at
    /// the WAVE 3 insertion point — after `MovePosition` is applied so the
    /// reveal scan centers on the arrival hex.
    ///
    /// Returns the resulting `Event`s (e.g. `EnvRevealed`) so `step_inner`
    /// can append them before the `eventful` check.
    pub fn resolve_on_move_passives(
        &mut self,
        actor: UnitId,
        content: &dyn crate::content::ContentView,
    ) -> Vec<crate::event::Event> {
        use crate::content::PassiveTrigger;
        self.resolve_passives(actor, content, &PassiveTrigger::OnMove)
    }

    /// Shared implementation: fire all passives for `actor` whose
    /// `def.passive` contains `trigger`.
    ///
    /// Pumps a local VecDeque through `apply_effect` (same cascade as the
    /// main pump loop but capped at `MAX_PASSIVE_DEPTH` to guard against
    /// pathological content).  Returns the resulting event stream.
    fn resolve_passives(
        &mut self,
        actor: UnitId,
        content: &dyn crate::content::ContentView,
        trigger: &crate::content::PassiveTrigger,
    ) -> Vec<crate::event::Event> {
        use std::collections::VecDeque;
        use crate::effect::{apply_effect, Effect};
        use crate::event::effect_to_event;

        // Collect initial effects from matching passives.
        let initial: Vec<Effect> = {
            let passives = match self.unit(actor) {
                Some(u) => u.passives.clone(),
                None => return vec![],
            };
            passives
                .into_iter()
                .filter_map(|aid| {
                    let def = content.ability_def(&aid)?;
                    if !def.passive.contains(trigger) {
                        return None;
                    }
                    match &def.effect {
                        crate::content::EffectDef::RevealEnvInRange { range } => {
                            Some(Effect::RevealEnvInRange { caster: actor, range: *range })
                        }
                        // Other effect kinds are not passive-resolvable yet.
                        _ => None,
                    }
                })
                .collect()
        };

        if initial.is_empty() {
            return vec![];
        }

        let mut queue: VecDeque<Effect> = initial.into();
        let mut events: Vec<crate::event::Event> = Vec::new();
        let mut steps = 0;
        const MAX_PASSIVE_DEPTH: usize = 200;

        while let Some(eff) = queue.pop_front() {
            steps += 1;
            if steps > MAX_PASSIVE_DEPTH {
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


/// Apply `template.initial_statuses` to `unit` with `PERMANENT_DURATION`.
///
/// Shared helper between `CombatState::apply_initial_statuses` (bootstrap path)
/// and `Effect::Spawn` (mid-combat summon path) so that any unit created from
/// a `UnitTemplate` receives its initial statuses via a single code path.
///
/// Idempotent: skips statuses already present on the unit (same id).
pub(crate) fn apply_template_initial_statuses(unit: &mut Unit, template: &UnitTemplate) {
    for status_id in &template.initial_statuses {
        if unit.statuses.iter().any(|s| &s.id == status_id) {
            continue;
        }
        unit.statuses.push(ActiveStatus {
            id: status_id.clone(),
            rounds_remaining: crate::PERMANENT_DURATION,
            dot_per_tick: 0,
            applier: EffectSource::Unit(unit.id),
        });
    }
}

/// Resolve starting pool value for `kind` given a template, applying the
/// optional `initial_pools` override and clamping to `[0, pool_max]`.
///
/// Returns `None` if this pool kind is absent for the template (i.e. max is 0
/// for non-Hp pools). `Hp` is always `Some`.
///
/// Default policy when no override is set:
/// - `Rage` → 0 (empty start).
/// - All others → max.
pub(crate) fn template_starting_pool(
    template: &crate::content::UnitTemplate,
    kind: crate::PoolKind,
) -> Option<(i32, i32)> {
    let max = match kind {
        crate::PoolKind::Hp     => template.max_hp,
        crate::PoolKind::Mana   => template.mana_max,
        crate::PoolKind::Rage   => template.rage_max,
        crate::PoolKind::Energy => template.energy_max,
        crate::PoolKind::Ap     => template.max_ap,
        crate::PoolKind::Mp     => template.base_speed,
    };
    // Non-Hp pools are absent when max == 0.
    if max == 0 && kind != crate::PoolKind::Hp {
        return None;
    }
    let default_current = match kind {
        crate::PoolKind::Rage => 0,
        _                     => max,
    };
    let current = template.initial_pools[kind]
        .unwrap_or(default_current)
        .clamp(0, max);
    Some((current, max))
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

    // ── Round-start cursor settlement (chunk 1 — used by bridge in chunk 2) ──

    /// Shared advance-turn pump: drains `seed` effects through the
    /// `AdvanceTurn`/`BumpRound` cascade, collecting all skip + pool events.
    ///
    /// This REPLICATES the budget-bounded drain discipline that `step_inner`
    /// uses for the `EndTurn` cascade (derived-effects-to-front, same budget).
    /// `step_inner` does NOT call this helper today — keep the two in sync if
    /// `AdvanceTurn`/`BumpRound` semantics change. The divergence surface is
    /// small: only `AdvanceTurn`/`BumpRound`-derived effects are expected in
    /// `seed` (the caller must not seed move/damage effects here), and those
    /// cascades never produce Cast/Move/AoO sub-queues.
    fn pump_advance_turn(
        &mut self,
        seed: std::collections::VecDeque<crate::effect::Effect>,
        content: &dyn crate::content::ContentView,
        budget: &mut usize,
    ) -> Vec<crate::event::Event> {
        use std::collections::VecDeque;
        use crate::effect::{apply_effect, Effect};
        use crate::event::effect_to_event;

        let mut events: Vec<crate::event::Event> = Vec::new();
        let mut queue: VecDeque<Effect> = seed;

        while let Some(eff) = queue.pop_front() {
            if matches!(&eff, Effect::AdvanceTurn | Effect::BumpRound) {
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
            }

            let (derived, mut ctx) = apply_effect(self, &eff, content);

            // Emit the corresponding event (RoundStarted for BumpRound, etc.).
            if let Some(ev) = effect_to_event(&eff, self, None, &ctx) {
                events.push(ev);
            }

            // Drain pool events (from BumpRound's RefreshAggregates cascade).
            events.append(&mut ctx.pool_events);

            // Drain skip events (TurnSkipped + tick events from stun/dead skips).
            events.append(&mut ctx.turn_skip_events);

            // Derived effects go to the FRONT (matches step_inner ordering).
            for ef in derived.into_iter().rev() {
                queue.push_front(ef);
            }
        }

        events
    }

    /// Settle the turn cursor from its current position at round-start.
    ///
    /// Call this once after the turn queue has been set to index=0 (e.g. after
    /// bootstrap or `BumpRound`).  It:
    /// 1. Emits `Event::RoundStarted { round: self.round }`.
    /// 2. Skips dead/stunned actors via the same `AdvanceTurn` pump as `step()`.
    /// 3. Starts the first non-dead, non-stunned actor's turn:
    ///    emits `Event::TurnStarted` then `start_actor_turn` events.
    /// 4. If the budget exhausts before finding a valid actor (all dead/stunned),
    ///    returns what it has — no panic, no infinite loop.
    ///
    /// The round counter must already be correct at call time (do NOT call
    /// `BumpRound` — that would double-increment the round).
    pub fn settle_round_start(
        &mut self,
        content: &dyn crate::content::ContentView,
    ) -> Vec<crate::event::Event> {
        use std::collections::VecDeque;
        use crate::effect::Effect;

        let mut events: Vec<crate::event::Event> = Vec::new();

        // 1. Emit RoundStarted — always, even if all actors are dead/stunned.
        events.push(crate::event::Event::RoundStarted { round: self.round });

        // Budget: generous enough to cross one full round of skips.
        let mut budget: usize = self.turn_queue.order.len() * 3 + 8;

        // 2. Check the current cursor actor.  If dead/stunned, derive
        //    AdvanceTurn and let the pump settle the cascade.
        let (skip_effects, mut skip_ctx) =
            crate::effect::skip_or_settle_current(self, content);

        // Collect skip events from the initial check.
        events.append(&mut skip_ctx.turn_skip_events);

        if !skip_effects.is_empty() {
            // Cursor needs to advance — pump the cascade.
            let seed: VecDeque<Effect> = skip_effects.into_iter().collect();
            let pump_events = self.pump_advance_turn(seed, content, &mut budget);
            events.extend(pump_events);
        }

        // 3. After settling, start the turn for the current actor only if it
        //    is alive AND not stunned (budget exhaustion may leave cursor on a
        //    stunned actor when all actors are stunned/dead).
        let cursor = self.turn_queue.current();
        let is_valid = cursor.is_some_and(|id| {
            let alive = self.unit(id).is_some_and(|u| u.is_alive());
            if !alive { return false; }
            // Check for stun via status or aura — mirror skip_or_settle_current.
            let by_status = self.unit(id).is_some_and(|u| {
                u.statuses.iter().any(|s| {
                    content.status_def(&s.id).is_some_and(|d| d.skips_turn)
                })
            });
            let by_aura = self.aura_effects_on(id, content).skips_turn;
            !by_status && !by_aura
        });

        if is_valid {
            let next_actor = cursor.unwrap();
            // Mirror step_inner: TurnStarted first, then start_actor_turn events.
            events.push(crate::event::Event::TurnStarted { actor: next_actor });
            events.extend(self.start_actor_turn(next_actor, content));
        }

        events
    }
}

// ── Initiative roller and turn-order reconciler (Wave 2) ─────────────────────

impl CombatState {
    /// Roll initiative for all units that have not yet had their initiative set.
    ///
    /// # Processing order
    /// Units are processed in **ascending `UnitId` order** regardless of their
    /// insertion order in `self.units`.  This is the replay-determinism anchor:
    /// every machine with the same seed and the same unit roster will consume
    /// dice draws in the identical sequence, making traces reproducible.
    ///
    /// # Preset map
    /// If a unit's id appears in `preset`, its initiative is set to the preset
    /// value with **no dice roll and no event emitted**.  This matches the
    /// behaviour of the legacy `build_turn_order` preset path.
    ///
    /// # Dead units
    /// Dead units (hp == 0) are included — no `is_alive()` filter — to keep
    /// the dice-draw count identical to the legacy `build_turn_order` draw set
    /// (required for RNG parity during the migration window).
    pub fn roll_initiative_for_all(
        &mut self,
        rng: &mut dyn crate::dice::DiceSource,
        preset: &std::collections::HashMap<UnitId, i32>,
    ) -> Vec<crate::event::Event> {
        // Collect ids of units still needing initiative, sorted ascending for
        // deterministic RNG consumption order.
        let mut ids_to_roll: Vec<UnitId> = self
            .units
            .iter()
            .filter(|u| u.initiative.is_none())
            .map(|u| u.id)
            .collect();
        ids_to_roll.sort_unstable();

        let mut events = Vec::new();
        for id in ids_to_roll {
            if let Some(&preset_val) = preset.get(&id) {
                // Preset: set value, no roll, no event.
                if let Some(u) = self.unit_mut(id) {
                    u.initiative = Some(preset_val);
                }
            } else {
                let roll = rng.roll(crate::dice::DiceExpr::new(1, 20, 0));
                let dex = self.unit(id).map(|u| u.caster_context.dex_mod).unwrap_or(0);
                let total = roll + dex;
                if let Some(u) = self.unit_mut(id) {
                    u.initiative = Some(total);
                }
                events.push(crate::event::Event::InitiativeRolled {
                    unit: id,
                    roll,
                    dex_mod: dex,
                    total,
                });
            }
        }
        events
    }

    /// Rebuild `turn_queue.order` from the current initiative values.
    ///
    /// Sort key: descending initiative, ascending `UnitId` as tie-break.
    /// Both alive and dead units are included (dead units receive a virtual-tick
    /// turn so the engine can advance their statuses).
    ///
    /// `turn_queue.index` is intentionally left untouched — callers (Waves 3/5)
    /// manage the index pointer.
    pub fn reconcile_turn_order(&mut self) {
        let mut order: Vec<UnitId> = self.units.iter().map(|u| u.id).collect();
        // Stable sort: descending initiative, ascending UnitId tie-break.
        // `None` initiative sorts last (maps to i32::MIN via unwrap_or).
        order.sort_by(|a, b| {
            let init_a = self.idx.get(a).and_then(|&i| self.units[i].initiative).unwrap_or(i32::MIN);
            let init_b = self.idx.get(b).and_then(|&i| self.units[i].initiative).unwrap_or(i32::MIN);
            init_b
                .cmp(&init_a)              // descending initiative
                .then_with(|| a.cmp(b))    // ascending UnitId tie-break
        });
        self.turn_queue.order = order;
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
        Unit::new(
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
            None,               // initiative: not yet rolled
            Default::default(),
            None,
            Vec::new(),
            Vec::new(),
            enum_map::enum_map! {
                PoolKind::Hp     => Some((10, 10)),
                PoolKind::Mana   => mana,
                PoolKind::Rage   => None,
                PoolKind::Energy => None,
                PoolKind::Ap     => Some((action_points, max_ap)),
                PoolKind::Mp     => Some((3, 3)),
            },
            enum_map::enum_map! {
                PoolKind::Hp     => RegenRule::None,
                PoolKind::Mana   => RegenRule::Increment(1),
                PoolKind::Rage   => RegenRule::None,
                PoolKind::Energy => RegenRule::Increment(1),
                PoolKind::Ap     => RegenRule::RefillToMax,
                PoolKind::Mp     => RegenRule::RefillToMax,
            },
            None,
        )
    }

    fn make_status(id: &str, applier: UnitId, rounds: u32, dot: i32) -> ActiveStatus {
        ActiveStatus {
            id: StatusId(id.into()),
            rounds_remaining: rounds,
            dot_per_tick: dot,
            applier: EffectSource::Unit(applier),
        }
    }

    #[test]
    fn start_actor_turn_refills_ap_and_regens_mana() {
        use crate::PoolKind;
        let uid = UnitId(1);
        let mut unit = make_unit(uid, 0, 2, Some((1, 10)));
        unit.pools[PoolKind::Mp] = Some((0, 3)); // depleted MP
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        let u = state.unit(uid).unwrap();
        assert_eq!(u.pools[PoolKind::Ap].map(|(c, _)| c), Some(2), "AP refilled to max");
        assert_eq!(u.pools[PoolKind::Mp].map(|(c, _)| c), Some(3), "MP refilled to speed");
        assert_eq!(u.pools[PoolKind::Mana], Some((2, 10)), "mana incremented to 2");
        // C6: only PoolChanged events (no legacy events).
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { unit: UnitId(1), pool: crate::PoolKind::Mana,
                current: 2, max: 10, cause: crate::PoolChangeCause::Regen }
        )), "PoolChanged{{Regen, Mana}} must fire");
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { pool: crate::PoolKind::Ap,
                cause: crate::PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill, Ap}} must fire when AP was depleted");
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { pool: crate::PoolKind::Mp,
                cause: crate::PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill, Mp}} must fire when MP was depleted");
    }

    #[test]
    fn start_actor_turn_refills_movement_points_to_speed() {
        use crate::PoolKind;
        let uid = UnitId(11);
        let mut unit = make_unit(uid, 0, 2, None);
        unit.base_speed = 4;
        unit.speed = 4;
        unit.pools[PoolKind::Mp] = Some((0, 4)); // depleted, max matches speed
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        state.start_actor_turn(uid, &content);

        assert_eq!(
            state.unit(uid).unwrap().pools[PoolKind::Mp].map(|(c, _)| c),
            Some(4),
            "MP refilled to speed=4"
        );
    }

    #[test]
    fn start_actor_turn_refills_mp_to_effective_speed_including_bonus() {
        // When a status grants +2 speed_bonus, u.speed = base_speed + bonus.
        // start_actor_turn must refill to u.speed, not u.base_speed.
        use crate::PoolKind;
        let uid = UnitId(12);
        let mut unit = make_unit(uid, 0, 2, None);
        unit.base_speed = 3;
        unit.speed = 5; // reflects status speed_bonus of +2
        unit.pools[PoolKind::Mp] = Some((0, 3)); // old max was 3; will be updated to 5
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        state.start_actor_turn(uid, &content);

        assert_eq!(
            state.unit(uid).unwrap().pools[PoolKind::Mp].map(|(c, _)| c),
            Some(5),
            "should refill to effective speed, not base_speed"
        );
    }

    #[test]
    fn start_actor_turn_mana_clamps_at_max() {
        use crate::PoolKind;
        // make_unit(uid, 0, 1, ...) sets AP=0/max=1 — AP refill will fire.
        // The test focuses on mana: at max (10/10), no PoolChanged{Mana} fires.
        let uid = UnitId(2);
        let unit = make_unit(uid, 0, 1, Some((10, 10)));
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        assert_eq!(state.unit(uid).unwrap().pools[PoolKind::Mana], Some((10, 10)));
        assert!(
            !events.iter().any(|e| matches!(
                e, Event::PoolChanged { pool: crate::PoolKind::Mana, .. }
            )),
            "no PoolChanged{{Mana}} when mana already at max",
        );
    }

    #[test]
    fn start_actor_turn_skips_dead_unit_refills() {
        use crate::PoolKind;
        let uid = UnitId(3);
        let mut unit = make_unit(uid, 0, 2, Some((1, 10)));
        unit.pools[PoolKind::Hp] = Some((0, 10));
        unit.pools[PoolKind::Mp] = Some((0, 3)); // depleted
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        let u = state.unit(uid).unwrap();
        assert_eq!(u.pools[PoolKind::Ap].map(|(c, _)| c), Some(0), "dead unit AP unchanged");
        assert_eq!(u.pools[PoolKind::Mp].map(|(c, _)| c), Some(0), "dead unit MP unchanged");
        assert!(events.is_empty(), "no refill events and no statuses to tick");
    }

    #[test]
    fn start_actor_turn_ticks_dot_on_victims() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.pools[crate::PoolKind::Hp] = Some((20, 20));
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

        assert_eq!(state.unit(victim).unwrap().hp(), 17, "HP should be reduced by 3");
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
        assert_eq!(state.unit(victim).unwrap().hp(), 10, "HP must be unchanged for zero-damage tick");
    }

    /// Two different DoT statuses on the same victim (both from the same applier)
    /// each produce their own `DotDamaged` event — no cross-contamination.
    #[test]
    fn multiple_dot_statuses_each_emit_own_dot_damaged() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.pools[crate::PoolKind::Hp] = Some((20, 20));
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
        assert_eq!(state.unit(victim).unwrap().hp(), 15, "HP reduced by both DoT ticks");
    }

    #[test]
    fn start_actor_turn_expires_status_on_last_tick() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.pools[crate::PoolKind::Hp] = Some((20, 20));
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
        assert_eq!(state.unit(victim).unwrap().hp(), 17);
    }

    #[test]
    fn start_actor_turn_for_dead_applier_still_ticks_orphaned() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let mut applier_unit = make_unit(applier, 0, 2, Some((1, 10)));
        applier_unit.pools[crate::PoolKind::Hp] = Some((0, 10));
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.pools[crate::PoolKind::Hp] = Some((20, 20));
        victim_unit.statuses.push(make_status("poison", applier, 2, 4));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        // Dead applier: no pool regen events.
        let no_pool_regen = !events.iter().any(|e| matches!(e,
            Event::PoolChanged { cause: crate::PoolChangeCause::Regen, .. }
        ));
        assert!(no_pool_regen, "dead applier must not emit regen events");
        let damaged = events.iter().any(|e| matches!(e,
            Event::DotDamaged { target, amount, .. }
            if *target == victim && *amount == 4
        ));
        assert!(damaged, "tick still fires for dead applier");
        assert_eq!(state.unit(victim).unwrap().hp(), 16);
    }

    #[test]
    fn start_actor_turn_dot_lethal_emits_death_and_cleans_local_statuses() {
        let applier = UnitId(1);
        let victim = UnitId(2);
        let applier_unit = make_unit(applier, 0, 2, None);
        let mut victim_unit = make_unit(victim, 0, 2, None);
        victim_unit.pools[crate::PoolKind::Hp] = Some((1, 20));
        victim_unit.statuses.push(make_status("burning", applier, 3, 5));
        victim_unit.statuses.push(make_status("slowed", UnitId(99), 2, 0));
        let mut state = CombatState::new(vec![applier_unit, victim_unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(applier, &content);

        let died = events.iter().any(|e| matches!(e, Event::UnitDied { unit } if *unit == victim));
        assert!(died, "UnitDied expected when DoT is lethal");
        assert_eq!(state.unit(victim).unwrap().hp(), 0);
        assert!(state.unit(victim).unwrap().statuses.is_empty(), "death clears local statuses");
    }

    #[test]
    fn start_actor_turn_no_statuses_returns_only_refill_events() {
        use crate::PoolKind;
        // make_unit sets AP=0/max=2 and MP=3/max=3 (already full), mana=5/max=10.
        // C6: only PoolChanged{Regen,Mana} + PoolChanged{Refill,Ap}.
        // MP is already at max (3/3), so no PoolChanged{Refill,Mp}.
        let uid = UnitId(1);
        let unit = make_unit(uid, 0, 2, Some((5, 10)));
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        assert!(events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { pool: crate::PoolKind::Mana,
                cause: crate::PoolChangeCause::Regen, .. }
        )), "PoolChanged{{Regen,Mana}} must fire");
        // No PoolChanged{Refill,Mp}: MP was already at max in make_unit.
        assert!(!events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { pool: crate::PoolKind::Mp,
                cause: crate::PoolChangeCause::Refill, .. }
        )), "no PoolChanged{{Refill,Mp}} when MP was already full");
        // All events are pool-related; no status tick or damage events.
        for e in &events {
            assert!(
                matches!(e, Event::PoolChanged { .. }),
                "unexpected non-pool event with no statuses: {e:?}"
            );
        }
        // verify AP refill event present
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PoolChanged { pool: PoolKind::Ap,
                cause: crate::PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill,Ap}} must fire when AP was depleted");
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
            applier: EffectSource::Unit(uid),
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

    /// C3/C6: verify unified regen loop drives all 5 pools correctly.
    ///
    /// - Mana/Energy: incremented by 1.
    /// - Ap/Mp: refilled to max.
    /// - Rage: skipped (RegenRule::None), unchanged.
    #[test]
    fn unified_regen_loop_increments_mana_energy_refills_ap_mp_skips_rage() {
        use crate::{PoolKind, PoolChangeCause, RegenRule};

        let uid = UnitId(42);
        // Start with all resources partially spent / not full.
        let mut unit = make_unit(uid, 1, 3, Some((4, 10))); // ap=1/3, mana=4/10
        unit.pools[PoolKind::Energy] = Some((2, 8));
        unit.pools[PoolKind::Rage]   = Some((3, 6));
        // mp: make_unit sets pools[Mp]=Some((3,3)), spend 1
        unit.pools[PoolKind::Mp] = Some((2, 3));
        // Set regen rules: Mana/Energy increment, Ap/Mp refill, Rage none.
        unit.regen_per_pool[PoolKind::Mana]   = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Rage]   = RegenRule::None;
        unit.regen_per_pool[PoolKind::Energy] = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Ap]     = RegenRule::RefillToMax;
        unit.regen_per_pool[PoolKind::Mp]     = RegenRule::RefillToMax;

        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);
        let u = state.unit(uid).unwrap();

        // Mana: 4 → 5 (incremented).
        assert_eq!(u.pools[PoolKind::Mana], Some((5, 10)), "pools[Mana] must increment");

        // Energy: 2 → 3 (incremented).
        assert_eq!(u.pools[PoolKind::Energy], Some((3, 8)), "pools[Energy] must increment");

        // Rage: unchanged at 3 (RegenRule::None).
        assert_eq!(u.pools[PoolKind::Rage], Some((3, 6)), "pools[Rage] must not change");

        // Ap: refilled to max=3.
        assert_eq!(u.pools[PoolKind::Ap], Some((3, 3)), "pools[Ap] must refill to max");

        // Mp: refilled to max=3.
        assert_eq!(u.pools[PoolKind::Mp], Some((3, 3)), "pools[Mp] must refill to max");

        // C6: PoolChanged events only (no legacy events).
        assert!(events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mana, cause: PoolChangeCause::Regen, .. }
        )), "PoolChanged{{Regen,Mana}} must fire");
        assert!(events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Energy, cause: PoolChangeCause::Regen, .. }
        )), "PoolChanged{{Regen,Energy}} must fire");
        assert!(events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Ap, cause: PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill,Ap}} must fire");
        assert!(events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mp, cause: PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill,Mp}} must fire");
        // Iteration order: Mana before Energy.
        let mana_pos   = events.iter().position(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mana, cause: PoolChangeCause::Regen, .. }
        )).unwrap();
        let energy_pos = events.iter().position(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Energy, cause: PoolChangeCause::Regen, .. }
        )).unwrap();
        assert!(mana_pos < energy_pos, "Mana PoolChanged must precede Energy PoolChanged");
    }

    // ── C4/C6 tests: Event::PoolChanged ──────────────────────────────────────

    /// C4-1/C6: Every pool mutation kind (Regen, Refill, Spent, Gained) emits
    /// a `PoolChanged` event with the correct cause.
    #[test]
    fn pool_changed_emitted_for_each_mutation_kind() {
        use crate::{PoolKind, PoolChangeCause, RegenRule};
        use crate::effect::{apply_effect, Effect};
        use crate::content::{ContentView, StatusBonuses};
        use crate::{AbilityId, AbilityDef, StatusId, StatusDef};

        struct Stub;
        static DEF: StatusDef = StatusDef {
            causes_disadvantage: false, blocks_mana_abilities: false,
            forces_targeting: false, skips_turn: false,
            bonuses: StatusBonuses { speed_bonus: 0, armor_bonus: 0, damage_taken_bonus: 0 },
            hp_percent_dot: 0,
        };
        impl ContentView for Stub {
            fn ability_def(&self, _: &AbilityId) -> Option<&AbilityDef> { None }
            fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { Some(&DEF) }
            fn unit_template(&self, _: &str) -> Option<crate::content::UnitTemplate> { None }
        }

        // Unit with all pools populated.
        let uid = UnitId(1);
        let mut unit = make_unit(uid, 1, 3, Some((5, 10)));
        unit.pools[PoolKind::Rage]   = Some((2, 8));
        unit.pools[PoolKind::Energy] = Some((3, 6));
        unit.regen_per_pool[PoolKind::Mana]   = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Rage]   = RegenRule::None;
        unit.regen_per_pool[PoolKind::Energy] = RegenRule::Increment(1);
        unit.regen_per_pool[PoolKind::Ap]     = RegenRule::RefillToMax;
        unit.regen_per_pool[PoolKind::Mp]     = RegenRule::RefillToMax;
        // Spend AP so Refill fires.
        unit.pools[PoolKind::Ap] = Some((1, 3));

        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = Stub;

        // --- Regen: turn-start fires PoolChanged{Regen} for Mana and Energy ---
        let regen_events = state.start_actor_turn(uid, &content);
        assert!(regen_events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mana, cause: PoolChangeCause::Regen, .. }
        )), "PoolChanged{{Regen,Mana}} must fire");
        assert!(regen_events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Energy, cause: PoolChangeCause::Regen, .. }
        )), "PoolChanged{{Regen,Energy}} must fire");

        // --- Refill: AP was spent → PoolChanged{Refill,Ap} ---
        assert!(regen_events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Ap, cause: PoolChangeCause::Refill, .. }
        )), "PoolChanged{{Refill,Ap}} must fire when AP was spent");

        // --- Spent: PayCost{Mana} → PoolChanged{Spent,Mana} ---
        let pay_eff = Effect::PayCost { actor: uid, kind: crate::ResourceKind::Mana, amount: 2 };
        let (_, ctx) = apply_effect(&mut state, &pay_eff, &content);
        assert!(ctx.pool_events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mana, cause: PoolChangeCause::Spent, .. }
        )), "PoolChanged{{Spent,Mana}} must be in ctx.pool_events");

        // --- Gained: GainRage → PoolChanged{Gained,Rage} ---
        let rage_eff = Effect::GainRage { target: uid };
        let (_, rage_ctx) = apply_effect(&mut state, &rage_eff, &content);
        assert!(rage_ctx.pool_events.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Rage, cause: PoolChangeCause::Gained, .. }
        )), "PoolChanged{{Gained,Rage}} must be in ctx.pool_events");
    }

    /// C6: PoolChanged{Regen,Mana} fires with correct current/max values.
    #[test]
    fn pool_changed_regen_mana_carries_correct_values() {
        use crate::{PoolKind, PoolChangeCause};

        let uid = UnitId(7);
        let unit = make_unit(uid, 2, 2, Some((3, 10))); // mana=3/10
        let mut state = CombatState::new(vec![unit], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events = state.start_actor_turn(uid, &content);

        let unified = events.iter().find(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mana, cause: PoolChangeCause::Regen, .. }
        )).expect("PoolChanged{{Regen,Mana}} must fire");

        let (uni_cur, uni_max) = match unified {
            Event::PoolChanged { current, max, .. } => (*current, *max),
            _ => unreachable!(),
        };
        assert_eq!(uni_cur, 4, "mana should have incremented from 3 to 4");
        assert_eq!(uni_max, 10, "mana max should be 10");
    }

    /// C4-3/C6: `PoolChanged{Refill}` fires only when AP/MP were actually spent.
    /// When they are already at max, no Refill event is emitted.
    #[test]
    fn ap_mp_refill_emits_pool_changed_only_on_change() {
        use crate::{PoolKind, PoolChangeCause};

        let uid = UnitId(3);

        // --- Case A: AP/MP were depleted → Refill fires ---
        let mut unit_a = make_unit(uid, 0, 2, None); // AP=0/max=2
        unit_a.pools[PoolKind::Mp] = Some((0, 3));
        let mut state_a = CombatState::new(vec![unit_a], 1, RoundPhase::ActorTurn, 0);
        let content = StubContent;

        let events_a = state_a.start_actor_turn(uid, &content);
        assert!(events_a.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Ap, cause: PoolChangeCause::Refill, .. }
        )), "Refill must fire for AP when depleted");
        assert!(events_a.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mp, cause: PoolChangeCause::Refill, .. }
        )), "Refill must fire for MP when depleted");

        // --- Case B: AP/MP already at max → no Refill event ---
        let unit_b = make_unit(uid, 2, 2, None); // AP=2/max=2 (full)
        // make_unit sets MP=3/max=3 (full)
        let mut state_b = CombatState::new(vec![unit_b], 1, RoundPhase::ActorTurn, 0);

        let events_b = state_b.start_actor_turn(uid, &content);
        assert!(!events_b.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Ap, cause: PoolChangeCause::Refill, .. }
        )), "no Refill for AP when already at max");
        assert!(!events_b.iter().any(|e| matches!(
            e, Event::PoolChanged { pool: PoolKind::Mp, cause: PoolChangeCause::Refill, .. }
        )), "no Refill for MP when already at max");
    }
}