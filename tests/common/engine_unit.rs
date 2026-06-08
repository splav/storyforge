//! Shared engine-level test fixtures.
//!
//! Provides:
//! - [`EngineUnitBuilder`] — fluent builder for `combat_engine::state::Unit`,
//!   mirroring the AI-side `UnitBuilder` style. Eliminates the ~10 copies of
//!   inline `Unit::new(…19 args…)` scattered across engine test files.
//! - [`StubContent`] — configurable `ContentView` stub backed by hash-maps.
//!   Replaces the handful of one-off all-`None` stubs that differ only in which
//!   ability/status ids they recognise.
//!
//! `#![allow(dead_code)]`: not every test file uses every setter.
#![allow(dead_code)]

use std::collections::HashMap;

use hexx::Hex;
use storyforge::combat_engine::{
    content::{AbilityDef, CasterContext, ContentView, StatusDef, UnitTemplate},
    state::{ActiveStatus, Team, Unit, UnitId},
    AbilityId, DiceExpr, PoolKind, RegenRule, StatusId, TagId,
};
use storyforge::game::hex::hex_from_offset;

// ── EngineUnitBuilder ─────────────────────────────────────────────────────────

/// Fluent builder for [`Unit`] in engine integration tests.
///
/// **Defaults** (the dominant test case):\
/// - `team`: `Team::Player`
/// - `pos`: `hex_from_offset(0, 0)`
/// - `Hp`: `(20, 20)`
/// - `Ap`: `(2, 2)`
/// - `Mp`: `(6, 6)`
/// - `Mana` / `Energy` / `Rage`: `None`
/// - `speed` / `base_speed`: `6`
/// - `reactions_left` / `reactions_max`: `1`
/// - `armor` / `armor_bonus` / `damage_taken_bonus`: `0`
/// - `aoo_dice`: `None`
/// - `initiative`: `None`
/// - `tags`: empty
/// - All regens follow the canonical pattern:
///   `Hp=None, Mana=Increment(1), Rage=None, Energy=Increment(1), Ap=RefillToMax, Mp=RefillToMax`
pub struct EngineUnitBuilder {
    id: UnitId,
    team: Team,
    pos_col: i32,
    pos_row: i32,
    pos_hex: Option<Hex>, // overrides pos_col/pos_row when set
    hp_cur: i32,
    hp_max: i32,
    ap_cur: i32,
    ap_max: i32,
    mp_cur: i32,
    mp_max: i32,
    mana: Option<(i32, i32)>,
    energy: Option<(i32, i32)>,
    rage: Option<(i32, i32)>,
    regens: storyforge::combat_engine::enum_map::EnumMap<PoolKind, RegenRule>,
    base_speed: i32,
    speed: i32,
    reactions_left: i32,
    reactions_max: i32,
    armor: i32,
    armor_bonus: i32,
    damage_taken_bonus: i32,
    aoo_dice: Option<DiceExpr>,
    caster_context: CasterContext,
    statuses: Vec<ActiveStatus>,
    summoner: Option<UnitId>,
    template: Option<String>,
    initiative: Option<i32>,
    tags: std::collections::BTreeSet<TagId>,
}

impl EngineUnitBuilder {
    /// Start building a unit with the given id.
    pub fn new(id: u64) -> Self {
        use storyforge::combat_engine::enum_map::enum_map;
        Self {
            id: UnitId(id),
            team: Team::Player,
            pos_col: 0,
            pos_row: 0,
            pos_hex: None,
            hp_cur: 20,
            hp_max: 20,
            ap_cur: 2,
            ap_max: 2,
            mp_cur: 6,
            mp_max: 6,
            mana: None,
            energy: None,
            rage: None,
            regens: enum_map! {
                PoolKind::Hp     => RegenRule::None,
                PoolKind::Mana   => RegenRule::Increment(1),
                PoolKind::Rage   => RegenRule::None,
                PoolKind::Energy => RegenRule::Increment(1),
                PoolKind::Ap     => RegenRule::RefillToMax,
                PoolKind::Mp     => RegenRule::RefillToMax,
            },
            base_speed: 6,
            speed: 6,
            reactions_left: 1,
            reactions_max: 1,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            aoo_dice: None,
            caster_context: Default::default(),
            statuses: vec![],
            summoner: None,
            template: None,
            initiative: None,
            tags: std::collections::BTreeSet::new(),
        }
    }

    pub fn team(mut self, t: Team) -> Self {
        self.team = t;
        self
    }

    /// Set position from offset coordinates (col, row) — uses `hex_from_offset`.
    pub fn pos(mut self, col: i32, row: i32) -> Self {
        self.pos_col = col;
        self.pos_row = row;
        self.pos_hex = None;
        self
    }

    /// Set position directly from an axial `Hex` (bypasses offset conversion).
    pub fn pos_hex(mut self, hex: Hex) -> Self {
        self.pos_hex = Some(hex);
        self
    }

    pub fn hp(mut self, cur: i32, max: i32) -> Self {
        self.hp_cur = cur;
        self.hp_max = max;
        self
    }

    /// Convenience: set hp_cur == hp_max.
    pub fn hp_full(mut self, hp: i32) -> Self {
        self.hp_cur = hp;
        self.hp_max = hp;
        self
    }

    pub fn ap(mut self, cur: i32, max: i32) -> Self {
        self.ap_cur = cur;
        self.ap_max = max;
        self
    }

    pub fn mp(mut self, cur: i32, max: i32) -> Self {
        self.mp_cur = cur;
        self.mp_max = max;
        self
    }

    pub fn mana(mut self, cur: i32, max: i32) -> Self {
        self.mana = Some((cur, max));
        self
    }

    pub fn energy(mut self, cur: i32, max: i32) -> Self {
        self.energy = Some((cur, max));
        self
    }

    pub fn rage(mut self, cur: i32, max: i32) -> Self {
        self.rage = Some((cur, max));
        self
    }

    pub fn speed(mut self, s: i32) -> Self {
        self.base_speed = s;
        self.speed = s;
        self
    }

    pub fn reactions(mut self, left: i32, max: i32) -> Self {
        self.reactions_left = left;
        self.reactions_max = max;
        self
    }

    pub fn armor(mut self, a: i32) -> Self {
        self.armor = a;
        self
    }
    pub fn armor_bonus(mut self, b: i32) -> Self {
        self.armor_bonus = b;
        self
    }
    pub fn damage_taken_bonus(mut self, d: i32) -> Self {
        self.damage_taken_bonus = d;
        self
    }

    pub fn aoo_dice(mut self, d: DiceExpr) -> Self {
        self.aoo_dice = Some(d);
        self
    }

    pub fn caster_context(mut self, ctx: CasterContext) -> Self {
        self.caster_context = ctx;
        self
    }

    /// Set `base_speed` and `speed` independently (when they differ).
    /// Use `.speed(s)` when both should be equal.
    pub fn base_speed_raw(mut self, base: i32) -> Self {
        self.base_speed = base;
        self
    }

    /// Set only `speed` (current, post-modifier) without changing `base_speed`.
    /// Use `.speed(s)` when both should be equal.
    pub fn speed_only(mut self, s: i32) -> Self {
        self.speed = s;
        self
    }

    pub fn status(mut self, s: ActiveStatus) -> Self {
        self.statuses.push(s);
        self
    }

    pub fn summoner(mut self, id: u64) -> Self {
        self.summoner = Some(UnitId(id));
        self
    }

    pub fn initiative(mut self, v: i32) -> Self {
        self.initiative = Some(v);
        self
    }

    pub fn template(mut self, id: impl Into<String>) -> Self {
        self.template = Some(id.into());
        self
    }

    pub fn regen(mut self, kind: PoolKind, rule: RegenRule) -> Self {
        self.regens[kind] = rule;
        self
    }

    /// Set creature tags on the built unit.
    pub fn tags(mut self, tags: std::collections::BTreeSet<TagId>) -> Self {
        self.tags = tags;
        self
    }

    /// Assemble and return the [`Unit`].
    pub fn build(self) -> Unit {
        use storyforge::combat_engine::enum_map::enum_map;
        let pools = enum_map! {
            PoolKind::Hp     => Some((self.hp_cur, self.hp_max)),
            PoolKind::Mana   => self.mana,
            PoolKind::Rage   => self.rage,
            PoolKind::Energy => self.energy,
            PoolKind::Ap     => Some((self.ap_cur, self.ap_max)),
            PoolKind::Mp     => Some((self.mp_cur, self.mp_max)),
        };
        let pos = self
            .pos_hex
            .unwrap_or_else(|| hex_from_offset(self.pos_col, self.pos_row));
        Unit::new(
            self.id,
            self.team,
            pos,
            self.armor,
            self.armor_bonus,
            self.damage_taken_bonus,
            self.base_speed,
            self.speed,
            self.reactions_left,
            self.reactions_max,
            self.statuses,
            self.summoner,
            self.initiative,
            self.caster_context,
            self.aoo_dice,
            vec![], // auras
            vec![], // enemy_phases
            pools,
            self.regens,
            self.template,
        )
        .with_tags(self.tags)
    }
}

// ── StubContent ───────────────────────────────────────────────────────────────

/// Configurable `ContentView` stub backed by `HashMap`s.
///
/// Returns `None` for any key not explicitly registered.  Use the `with_*`
/// builders to register ability/status definitions.
///
/// Replaces the many one-off `struct NoContent` / `struct DeterminismContent`
/// definitions scattered across engine test files that only differ in which ids
/// they recognise.
pub struct StubContent {
    abilities: HashMap<AbilityId, AbilityDef>,
    statuses: HashMap<StatusId, StatusDef>,
}

impl StubContent {
    pub fn new() -> Self {
        Self {
            abilities: HashMap::new(),
            statuses: HashMap::new(),
        }
    }

    pub fn with_ability(mut self, id: impl Into<String>, def: AbilityDef) -> Self {
        self.abilities.insert(AbilityId(id.into()), def);
        self
    }

    pub fn with_status(mut self, id: StatusId, def: StatusDef) -> Self {
        self.statuses.insert(id, def);
        self
    }
}

impl Default for StubContent {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentView for StubContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.get(id)
    }

    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        self.statuses.get(id)
    }

    fn unit_template(&self, _: &str) -> Option<UnitTemplate> {
        None
    }
}
