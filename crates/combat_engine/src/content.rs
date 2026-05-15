//! `ContentView` — read-only content access trait for the engine.
//!
//! The engine needs only a minimal slice of `ContentDb`.  This trait expresses
//! exactly that slice so the engine has zero dependency on `crate::content`.
//!
//! **Phase 0** exposes only what `step(Action::Move)` needs:
//! - `aoo_dice(attacker)` — weapon dice for AoO expansion.
//! - `status_bonuses(id)` — speed/armor bonuses for `RefreshAggregates`.
//!
//! Callers implement this trait for real (`ActiveContent` adapter); the engine
//! only ever calls through the trait object.  Step 8+ agent extends as needed.

use crate::{dice::DiceExpr, state::UnitId, AbilityId, ResourceKind, StatusId};

/// Outcome when a cast crit-fails (d20 roll lands on 1).
///
/// Engine primitives only — content-specific labels (BrokenFaith,
/// ManaOverload, etc.) translate to these at the bridge boundary.
#[derive(Debug, Clone, Default)]
pub enum CritFailOutcome {
    /// Cast misses entirely — no damage / heal / status; costs still paid.
    #[default]
    Miss,
    /// Cost amounts doubled for this cast.  No damage / heal / status.
    DoubleCost,
    /// Caster takes `dice` raw damage (non-piercing).  No normal damage / heal / status.
    SelfDamage(DiceExpr),
    /// Apply `status` to the caster, 3 rounds, no DoT.  No normal damage / heal / status.
    ApplyStatus(StatusId),
}

/// Cached caster stats needed for damage / heal formulas.
/// Mirrors `crate::content::abilities::CasterContext`.
#[derive(Debug, Clone, Default)]
pub struct CasterContext {
    pub str_mod: i32,
    pub int_mod: i32,
    pub spell_power: i32,
    pub weapon_dice: Option<DiceExpr>,
    /// Behaviour when this caster rolls a 1 on the crit-fail d20.
    pub crit_fail_outcome: CritFailOutcome,
}

/// Where a status application lands.  Mirrors `crate::content::abilities::StatusOn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusOn {
    /// The ability's resolved target (enemy / ally / self per `target_type`).
    Target,
    /// Always the actor who used the ability.
    MySelf,
}

/// Status to apply when the ability resolves.  Mirrors
/// `crate::content::abilities::StatusApplication`.
#[derive(Debug, Clone)]
pub struct StatusApplication {
    pub status: StatusId,
    pub duration_rounds: u32,
    pub on: StatusOn,
}

/// Engine-side effect kinds.  Mirrors `crate::content::abilities::EffectDef`
/// minus `Summon` (Phase 3 scope) and `ToggleMoveMode` (UI-only).
///
/// Phase 2 step 6c-e implements expansion in `step()`'s `Action::Cast` arm.
#[derive(Debug, Clone)]
pub enum EffectDef {
    /// No direct damage / heal — ability only applies statuses.
    None,
    /// Uses caster's equipped weapon dice + str_mod.
    WeaponAttack,
    /// Physical damage from a fixed dice roll + str_mod.
    Damage { dice: DiceExpr },
    /// Magical damage: spell_power + int_mod + dice, pierces armor.
    SpellDamage { dice: DiceExpr },
    /// Heal: spell_power + int_mod + dice.
    Heal { dice: DiceExpr },
    /// Grants bonus movement to the actor.  Does NOT end the turn.
    GrantMovement { distance: i32 },
    /// Restores HP and all resources (mana, rage, energy) by 1.
    RestoreResources,
}

/// Per-status stat bonuses relevant to engine aggregate recomputation.
///
/// Mirrors the fields read by `BattleSnapshot::refresh_aggregates` and
/// `snapshot::status_bonuses`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusBonuses {
    /// Added to `base_speed` to get effective `speed`.
    pub speed_bonus: i32,
    /// Added to equipment `armor` to get effective mitigation.
    pub armor_bonus: i32,
}

/// Resource cost for an ability (one entry per resource kind).
///
/// Mirrors `crate::content::abilities::ResourceCost`.
#[derive(Debug, Clone, Copy)]
pub struct Cost {
    pub resource: ResourceKind,
    pub amount: i32,
}

/// Who/where an ability targets.  Mirrors `crate::content::abilities::TargetType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    SingleAlly,
    Myself,
    Ground,
}

/// Range in hex-steps.  `max == 0` means self-only.
/// Mirrors `crate::content::abilities::AbilityRange`.
#[derive(Debug, Clone, Copy)]
pub struct AbilityRange {
    pub min: u32,
    pub max: u32,
}

/// Area-of-effect pattern.  Mirrors `crate::content::abilities::AoEShape`.
/// `None` = single-target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AoEShape {
    #[default]
    None,
    Circle { radius: u32 },
    Line { length: u32 },
}

/// Engine-side minimal ability definition.  Legality + targeting fields;
/// `effect` and `statuses` populated by Phase 2 step 6a, expanded in step 6c-e.
#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub key: Option<String>,
    pub cost_ap: i32,
    pub costs: Vec<Cost>,
    pub range: AbilityRange,
    pub target_type: TargetType,
    pub aoe: AoEShape,
    /// If true, AoE damages allies + actor too.  Targeting filters
    /// (`compute_affected_targets`) consult this; non-AoE single-target
    /// abilities ignore it.
    pub friendly_fire: bool,
    /// What the ability does to its primary affected target(s).
    pub effect: EffectDef,
    /// Statuses applied alongside `effect`.
    pub statuses: Vec<StatusApplication>,
}

/// Engine-side minimal status definition — legality + aggregate-relevant fields.
///
/// Aggregates (`armor_bonus`, `speed_bonus`) overlap with `StatusBonuses`;
/// Phase 2 keeps both, `status_bonuses()` may later be derived from this.
#[derive(Debug, Clone, Copy)]
pub struct StatusDef {
    pub causes_disadvantage: bool,
    pub blocks_mana_abilities: bool,
    pub forces_targeting: bool,
    pub skips_turn: bool,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    pub speed_bonus: i32,
    /// Percent of max_hp dealt as DoT per tick; ceil formula: `(max_hp * pct + 99) / 100`.
    pub hp_percent_dot: i32,
}

/// Read-only view onto game content that the engine needs.
///
/// Implemented by `crate::combat::engine_bridge::EcsContentView` (live path)
/// and `crate::combat::ai::plan::sim::SnapshotContentView` (sim path).
/// Test implementations return simple stubs.
pub trait ContentView {
    /// Weapon dice for the attacker's AoO strike.
    ///
    /// Returns `None` if the unit has no equipped melee weapon — in which case
    /// `expand_reaction` will not emit an AoO (mirroring `movement_system`'s
    /// "prefer weapon dice; if no weapon, skip" logic).
    fn aoo_dice(&self, attacker: UnitId) -> Option<DiceExpr>;

    /// Stat bonuses granted by a single status instance.
    ///
    /// Returns `StatusBonuses::default()` (all zeros) for unknown status ids.
    /// This matches the real path: statuses not present in `content.statuses`
    /// contribute nothing.
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses;

    /// Engine-side ability definition.  `None` if the id is unknown.
    ///
    /// Used by `check_legality` (Phase 2 step 2c) and `expand_action(Cast)`
    /// (Phase 2 step 6).
    fn ability_def(&self, id: &AbilityId) -> Option<AbilityDef>;

    /// Engine-side status definition.  `None` if the id is unknown.
    fn status_def(&self, id: &StatusId) -> Option<StatusDef>;

    /// Caster stat bundle for damage / heal formulas.  Returns
    /// `CasterContext::default()` for unknown units.
    fn caster_context(&self, actor: UnitId) -> CasterContext;
}
