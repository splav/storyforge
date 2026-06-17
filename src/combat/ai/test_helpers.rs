//! Shared test scaffolding for the `ai/` tree (UnitFixture, InfluenceMaps,
//! ActiveContentData, scoring/stage/critic harnesses).
//!
//! Module-wide `allow(dead_code)`: helpers are used only from `#[cfg(test)]`
//! blocks and integration tests, but this `pub mod` is compiled by the lib
//! (non-test) pass — where every helper looks dead.
#![allow(dead_code)]

use crate::combat::ai::config::difficulty::DifficultyProfile;
use crate::combat::ai::config::role::AxisProfile;
use crate::combat::ai::intent::agenda::Agenda;
use crate::combat::ai::intent::{IntentReason, TacticalIntent};
use crate::combat::ai::orchestration::{AiWorld, ScoringCtx};
use crate::combat::ai::outcome::{AdaptationData, PerItemEval, PipelineAnnotation};
use crate::combat::ai::pipeline::{ScoredPool, StageCtx};
use crate::combat::ai::plan::types::TurnPlan;
use crate::combat::ai::scoring::factors::PlanFactorValues;
use crate::combat::ai::world::cache::{AiCache, UnitAiCache};
use crate::combat::ai::world::influence::{InfluenceMap, InfluenceMaps};
use crate::combat::ai::world::reservations::Reservations;
use crate::combat::ai::world::snapshot::BattleSnapshot;
use crate::combat::ai::world::tags::AiTags;
use crate::combat::ai::world::tags::{AbilityTagCache, StatusTagCache};
use crate::content::abilities::AbilityDef as BevyAbilityDef;
use crate::content::abilities::CasterContext;
use crate::content::content_view::ActiveContentData;
use crate::content::races::CritFailEffect;
use crate::content::statuses::StatusDef as BevyStatusDef;
use crate::game::components::Team;
use crate::game::hex::Hex;
use bevy::prelude::Entity;
use combat_engine::{AbilityId, DiceRng, StatusId};
use std::sync::OnceLock;

/// Shared empty `AbilityTagCache` for test contexts that don't exercise
/// tag-based logic. Lives in a `OnceLock` to satisfy the `'a` lifetime
/// requirement in `AiWorld<'a>.ability_tags` without caller cascade.
static EMPTY_ABILITY_TAG_CACHE: OnceLock<AbilityTagCache> = OnceLock::new();

pub(crate) fn empty_ability_tag_cache() -> &'static AbilityTagCache {
    EMPTY_ABILITY_TAG_CACHE.get_or_init(AbilityTagCache::default)
}

/// Shared empty `StatusTagCache` for test contexts that don't exercise
/// status-tag logic. Lives in a `OnceLock` to satisfy the `'a` lifetime
/// requirement in `AiWorld<'a>.status_tags` without caller cascade.
static EMPTY_STATUS_TAG_CACHE: OnceLock<StatusTagCache> = OnceLock::new();

pub(crate) fn empty_status_tag_cache() -> &'static StatusTagCache {
    EMPTY_STATUS_TAG_CACHE.get_or_init(StatusTagCache::default)
}

/// Build an empty `(StatusTagCache, AbilityTagCache)` pair for test contexts
/// that need to pass owned caches (e.g., `pick_action` integration tests).
pub(crate) fn empty_caches() -> (StatusTagCache, AbilityTagCache) {
    (StatusTagCache::default(), AbilityTagCache::default())
}

// ── Utility context ────────────────────────────────────────────────────────

/// Build an `AiWorld` with the conventional test defaults
/// (`crit_fail_chance: 0.0`, `tuning: AiTuning::default()`). Caller supplies
/// content + difficulty.
///
/// Per-actor data — caster / abilities / crit_fail_effect — lives on
/// `UnitSnapshot`; configure via `UnitBuilder::caster_ctx` /
/// `UnitBuilder::ability_names` / `UnitBuilder::crit_fail_effect`.
pub(crate) fn make_test_ctx<'a>(
    content: &'a ActiveContentData,
    difficulty: &'a DifficultyProfile,
) -> AiWorld<'a> {
    AiWorld {
        content,
        difficulty,
        tuning: &content.ai_tuning,
        crit_fail_chance: 0.0,
        ability_tags: empty_ability_tag_cache(),
        status_tags: empty_status_tag_cache(),
    }
}

/// Bundle the per-test refs into a `ScoringCtx`, mirroring `pick_action`.
/// Callers own `maps`/`reservations` so a test can pre-seed tiles. `active`'s
/// entity resolves its `UnitView`; panics if absent from the snapshot.
pub(crate) fn make_scoring_ctx<'a>(
    world: &'a AiWorld<'a>,
    snap: &'a BattleSnapshot,
    maps: &'a InfluenceMaps,
    reservations: &'a Reservations,
    active: &'a UnitFixture,
) -> ScoringCtx<'a, 'a> {
    let active_view = snap.unit(active.entity).expect(
        "test fixture: active must be in snap — pass an entity present in snapshot_from(...)",
    );
    ScoringCtx {
        world,
        maps,
        reservations,
        snap,
        active: active_view,
        need_signals: Default::default(),
        last_goal: None,
    }
}

// ── Unit fixture ─────────────────────────────────────────────────────────────

/// Test-only plain-data mirror of `UnitSnapshot` fields. No serde, no
/// aggregate-refresh: tests set `speed`/`armor_bonus` explicitly. The
/// `statuses` field stores engine `ActiveStatus` directly — use [`status_view`].
#[derive(Clone, Debug)]
pub struct UnitFixture {
    pub entity: bevy::prelude::Entity,
    pub team: Team,
    pub role: crate::combat::ai::config::role::AxisProfile,
    pub pos: Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub armor_bonus: i32,
    pub magic_resist: i32,
    pub action_points: i32,
    pub max_ap: i32,
    pub movement_points: i32,
    pub base_speed: i32,
    pub speed: i32,
    pub mana: Option<(i32, i32)>,
    pub rage: Option<(i32, i32)>,
    pub energy: Option<(i32, i32)>,
    pub abilities: Vec<crate::combat_engine::AbilityId>,
    pub threat: f32,
    pub tags: AiTags,
    pub max_attack_range: u32,
    pub summoner: Option<bevy::prelude::Entity>,
    pub reactions_left: i32,
    pub aoo_expected_damage: Option<f32>,
    /// Engine `ActiveStatus` values. Use [`status_view`] to fabricate entries.
    pub statuses: Vec<combat_engine::state::ActiveStatus>,
    pub caster_ctx: CasterContext,
    pub crit_fail_effect: CritFailEffect,
    pub damage_horizon: Vec<f32>,
    pub ai_tuning_override: Option<crate::combat::ai::config::tuning::AiTuningOverride>,
    pub forced_mode: Option<crate::combat::ai::adapt::EvaluationMode>,
}

/// Fabricate a `combat_engine::state::ActiveStatus` for test fixtures.
///
/// `applier` is always `EffectSource::Unit(UnitId(0))` — a sentinel adequate
/// for test scenarios that do not exercise applier-attribution logic.
pub(crate) fn status_view(id: &str, rounds: u32, dot: i32) -> combat_engine::state::ActiveStatus {
    combat_engine::state::ActiveStatus {
        id: combat_engine::StatusId::from(id),
        rounds_remaining: rounds,
        dot_per_tick: dot,
        applier: combat_engine::state::EffectSource::Unit(combat_engine::state::UnitId(0)),
    }
}

/// Convert a `UnitFixture` into the engine pair `(Unit, UnitAiCache)`.
///
/// This is the single canonical conversion path — `UnitBuilder::build_pair`
/// delegates here, eliminating the former duplicated `unit_snapshot_to_pair`.
pub(crate) fn fixture_to_pair(
    u: &UnitFixture,
) -> (
    combat_engine::state::Unit,
    crate::combat::ai::world::cache::UnitAiCache,
) {
    use crate::content::races::CritFailEffect as Cfe;
    use combat_engine::state::{Team as EngineTeam, UnitId};
    use combat_engine::CritFailOutcome as Out;
    let team = match u.team {
        Team::Player => EngineTeam::Player,
        Team::Enemy => EngineTeam::Enemy,
    };
    let uid = UnitId(u.entity.to_bits());
    let crit_fail_outcome = match &u.crit_fail_effect {
        Cfe::Miss => Out::Miss,
        Cfe::ManaOverload => Out::DoubleCost,
        Cfe::BrokenFaith => Out::ApplyStatus(combat_engine::StatusId::from("broken_faith")),
        Cfe::CircuitBreach => Out::SelfDamage(combat_engine::DiceExpr::new(0, 1, 2)),
        Cfe::Exhaustion => Out::ApplyStatus(combat_engine::StatusId::from("exhaustion")),
        Cfe::PactControl => Out::ApplyStatus(combat_engine::StatusId::from("pact_control")),
    };
    let caster_context = combat_engine::CasterContext {
        str_mod: u.caster_ctx.str_mod,
        int_mod: u.caster_ctx.int_mod,
        spell_power: u.caster_ctx.spell_power,
        weapon_dice: u.caster_ctx.weapon_dice,
        ranged_dice: u.caster_ctx.ranged_dice,
        crit_fail_outcome,
        dex_mod: u.caster_ctx.dex_mod,
    };
    let aoo_dice = u
        .aoo_expected_damage
        .map(|raw| combat_engine::DiceExpr::new(0, 1, raw.round() as i32));
    // Derive runtime_bonus from UnitFixture's explicit armor_bonus + speed fields.
    // speed_bonus = speed - base_speed (the fixture stores effective speed separately).
    let runtime_bonus = combat_engine::RuntimeStatsDelta(combat_engine::RuntimeStats {
        armor: u.armor_bonus,
        magic_resist: 0, // UnitFixture has no magic_resist_bonus field yet
        base_speed: u.speed - u.base_speed,
    });
    let engine_unit = combat_engine::state::Unit::new(
        uid,
        team,
        u.pos,
        combat_engine::RuntimeStats {
            armor: u.armor,
            magic_resist: u.magic_resist,
            base_speed: u.base_speed,
        },
        runtime_bonus,
        u.reactions_left,
        1,
        u.statuses.clone(),
        u.summoner
            .map(|e| combat_engine::state::UnitId(e.to_bits())),
        None, // initiative: not yet rolled
        caster_context,
        aoo_dice,
        Vec::new(),
        Vec::new(),
        combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Hp     => Some((u.hp, u.max_hp)),
            combat_engine::PoolKind::Mana   => u.mana,
            combat_engine::PoolKind::Rage   => u.rage,
            combat_engine::PoolKind::Energy => u.energy,
            combat_engine::PoolKind::Ap     => Some((u.action_points, u.max_ap)),
            combat_engine::PoolKind::Mp     => Some((u.movement_points, u.movement_points)),
        },
        combat_engine::enum_map::enum_map! {
            combat_engine::PoolKind::Hp     => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Mana   => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Rage   => combat_engine::RegenRule::None,
            combat_engine::PoolKind::Energy => combat_engine::RegenRule::Increment(1),
            combat_engine::PoolKind::Ap     => combat_engine::RegenRule::RefillToMax,
            combat_engine::PoolKind::Mp     => combat_engine::RegenRule::RefillToMax,
        },
        None,
    );
    let ai_cache = crate::combat::ai::world::cache::UnitAiCache {
        entity: u.entity,
        role: u.role,
        threat: u.threat,
        tags: u.tags,
        max_attack_range: u.max_attack_range,
        aoo_expected_damage: u.aoo_expected_damage,
        damage_horizon: u.damage_horizon.clone(),
        crit_fail_effect: u.crit_fail_effect.clone(),
        ai_tuning_override: u.ai_tuning_override.clone(),
        abilities: u.abilities.clone(),
        caster_ctx: u.caster_ctx.clone(),
        forced_mode: u.forced_mode,
    };
    (engine_unit, ai_cache)
}

// ── Unit fixture builder ───────────────────────────────────────────────────

/// Fluent builder for [`UnitFixture`] test fixtures. Call sites override
/// only the fields that matter for their scenario (`.hp(5).tags(LOW_HP)`).
pub struct UnitBuilder {
    inner: UnitFixture,
}

#[allow(dead_code)] // full chain kept for future tests; used ones rotate.
impl UnitBuilder {
    /// Reasonable "generic melee bruiser" defaults. Tests override via the
    /// chain methods below. Canonical defaults (picked to match the most
    /// common old factory): hp/max_hp=20, ap=1/max=1, speed=3, mp=3,
    /// threat=5.0, max_attack_range=1, role=Bruiser, tags=empty.
    pub fn new(id: u32, team: Team, pos: Hex) -> Self {
        Self {
            inner: UnitFixture {
                entity: Entity::from_raw_u32(id).expect("valid entity id"),
                team,
                role: crate::combat::ai::config::role::AxisProfile {
                    tank: 0.5,
                    melee: 0.5,
                    ranged: 0.0,
                    control: 0.0,
                    support: 0.0,
                },
                pos,
                hp: 20,
                max_hp: 20,
                armor: 0,
                armor_bonus: 0,
                magic_resist: 0,
                action_points: 1,
                max_ap: 1,
                movement_points: 3,
                base_speed: 3,
                speed: 3,
                mana: None,
                rage: None,
                energy: None,
                abilities: Vec::new(),
                threat: 5.0,
                tags: AiTags::empty(),
                max_attack_range: 1,
                summoner: None,
                reactions_left: 0,
                aoo_expected_damage: None,
                statuses: Vec::new(),
                caster_ctx: Default::default(),
                crit_fail_effect: Default::default(),
                damage_horizon: Vec::new(),
                ai_tuning_override: None,
                forced_mode: None,
            },
        }
    }

    pub fn hp(mut self, hp: i32) -> Self {
        self.inner.hp = hp;
        self
    }
    pub fn max_hp(mut self, max_hp: i32) -> Self {
        self.inner.max_hp = max_hp;
        self
    }
    pub fn full_hp(mut self, hp: i32) -> Self {
        self.inner.hp = hp;
        self.inner.max_hp = hp;
        self
    }
    pub fn armor(mut self, armor: i32) -> Self {
        self.inner.armor = armor;
        self
    }
    pub fn armor_bonus(mut self, bonus: i32) -> Self {
        self.inner.armor_bonus = bonus;
        self
    }
    pub fn magic_resist(mut self, mr: i32) -> Self {
        self.inner.magic_resist = mr;
        self
    }
    pub fn ap(mut self, ap: i32) -> Self {
        self.inner.action_points = ap;
        self.inner.max_ap = ap;
        self
    }
    pub fn speed(mut self, speed: i32) -> Self {
        self.inner.base_speed = speed;
        self.inner.speed = speed;
        self.inner.movement_points = speed;
        self
    }
    pub fn threat(mut self, threat: f32) -> Self {
        self.inner.threat = threat;
        self
    }
    pub fn role(mut self, role: AxisProfile) -> Self {
        self.inner.role = role;
        self
    }
    pub fn tags(mut self, tags: AiTags) -> Self {
        self.inner.tags = tags;
        self
    }
    pub fn abilities(mut self, ids: Vec<AbilityId>) -> Self {
        self.inner.abilities = ids;
        self
    }
    pub fn ability_names(self, names: &[&str]) -> Self {
        self.abilities(names.iter().map(|s| AbilityId::from(*s)).collect())
    }
    pub fn max_attack_range(mut self, r: u32) -> Self {
        self.inner.max_attack_range = r;
        self
    }
    pub fn mana(mut self, current: i32, max: i32) -> Self {
        self.inner.mana = Some((current, max));
        self
    }
    pub fn rage(mut self, current: i32, max: i32) -> Self {
        self.inner.rage = Some((current, max));
        self
    }
    pub fn energy(mut self, current: i32, max: i32) -> Self {
        self.inner.energy = Some((current, max));
        self
    }
    pub fn summoner(mut self, entity: Entity) -> Self {
        self.inner.summoner = Some(entity);
        self
    }
    pub fn aoo(mut self, expected_damage: f32, reactions: i32) -> Self {
        self.inner.aoo_expected_damage = Some(expected_damage);
        self.inner.reactions_left = reactions;
        self
    }
    /// Set the actor's casting profile (str_mod / int_mod / spell_power /
    /// weapon_dice). Tests that depend on caster-driven scoring (damage
    /// estimation, heal magnitude) configure it here; default is zeros.
    pub fn caster_ctx(mut self, ctx: CasterContext) -> Self {
        self.inner.caster_ctx = ctx;
        self
    }
    pub fn crit_fail_effect(mut self, eff: CritFailEffect) -> Self {
        self.inner.crit_fail_effect = eff;
        self
    }
    pub fn damage_horizon(mut self, horizon: Vec<f32>) -> Self {
        self.inner.damage_horizon = horizon;
        self
    }
    pub fn statuses(mut self, statuses: Vec<combat_engine::state::ActiveStatus>) -> Self {
        self.inner.statuses = statuses;
        self
    }
    /// Override only `movement_points`, leaving `base_speed` and `speed` unchanged.
    pub fn movement_points(mut self, mp: i32) -> Self {
        self.inner.movement_points = mp;
        self
    }
    /// Override only `speed` (effective speed after bonuses), leaving `base_speed` and
    /// `movement_points` unchanged.
    pub fn speed_override(mut self, speed: i32) -> Self {
        self.inner.speed = speed;
        self
    }
    /// Set the forced evaluation mode (e.g., `Some(EvaluationMode::Flee)`).
    pub fn forced_mode(mut self, mode: Option<crate::combat::ai::adapt::EvaluationMode>) -> Self {
        self.inner.forced_mode = mode;
        self
    }
    pub fn build(self) -> UnitFixture {
        self.inner
    }

    /// Build both halves: an engine `Unit` (for `CombatState`) and a
    /// `UnitAiCache` (for `AiCache`). Use with `snapshot_from` to produce
    /// a `BattleSnapshot` via the authoritative `BattleSnapshot::new(state, cache)`
    /// constructor instead of the legacy `new_from_unit_snapshots`.
    pub fn build_pair(self) -> (combat_engine::state::Unit, UnitAiCache) {
        fixture_to_pair(&self.inner)
    }
}

/// Build a `BattleSnapshot` from a vec of [`UnitFixture`] values via the
/// authoritative `BattleSnapshot::new(state, cache)` constructor.
#[allow(dead_code)]
pub fn snapshot_from(units: Vec<UnitFixture>, round: u32) -> BattleSnapshot {
    snapshot_from_pairs(units.iter().map(fixture_to_pair).collect(), round)
}

/// Lower-level variant for callers that already have `build_pair()` output.
#[allow(dead_code)]
pub fn snapshot_from_pairs(
    pairs: Vec<(combat_engine::state::Unit, UnitAiCache)>,
    round: u32,
) -> BattleSnapshot {
    use combat_engine::state::RoundPhase;
    let (engine_units, ai_units): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    let state =
        combat_engine::state::CombatState::new(engine_units, round, RoundPhase::ActorTurn, 0);
    let cache = AiCache::from_units(ai_units);
    BattleSnapshot::new(state, cache)
}

/// Short-hand for `UnitBuilder::new(id, team, pos).build()` — the dominant
/// single-line fixture shape across test modules.
pub(crate) fn unit(id: u32, team: Team, pos: Hex) -> UnitFixture {
    UnitBuilder::new(id, team, pos).build()
}

/// Convenience for making an `Entity` from a raw u32 test id.
pub fn ent(id: u32) -> Entity {
    Entity::from_raw_u32(id).expect("valid entity id")
}

/// Empty `TurnPlan` — sugar for `TurnPlan::default()`. Used by stages whose
/// tests need a no-op plan input.
pub fn empty_plan() -> TurnPlan {
    TurnPlan::default()
}

// ── Influence maps ─────────────────────────────────────────────────────────

/// All four influence maps empty (zero danger / ally_support / opportunity /
/// escape). Test bodies add specific tiles via `maps.danger.add(...)` when
/// they care.
pub(crate) fn empty_maps() -> InfluenceMaps {
    InfluenceMaps {
        danger: InfluenceMap::new(),
        ally_support: InfluenceMap::new(),
        opportunity: InfluenceMap::new(),
        escape: InfluenceMap::new(),
    }
}

// ── Content ────────────────────────────────────────────────────────────────

/// Completely empty `ActiveContentData`. Tests that need a specific ability/status
/// insert it after construction.
pub(crate) fn empty_content() -> ActiveContentData {
    ActiveContentData::default()
}

/// Wrap a pure-engine `AbilityDef` in the Bevy `AbilityDef` shell with empty
/// bridge-only fields (no magic domains, no AI override, not a move toggle).
pub(crate) fn bevy_ability(
    id: &str,
    name: &str,
    engine: combat_engine::AbilityDef,
) -> BevyAbilityDef {
    BevyAbilityDef {
        id: AbilityId::from(id),
        name: name.into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine,
    }
}

/// Wrap a pure-engine `StatusDef` in the Bevy `StatusDef` shell with empty
/// bridge-only fields (no dot_dice, not AI-controlled, no buff class).
pub(crate) fn bevy_status(id: &str, engine: combat_engine::StatusDef) -> BevyStatusDef {
    BevyStatusDef {
        id: StatusId::from(id),
        name: id.into(),
        dot_dice: None,
        ai_controlled: false,
        buff_class: None,
        engine,
    }
}

// ── StageTestHarness ───────────────────────────────────────────────────────

/// Universal context for stage unit tests.  All fields are public — configure
/// via direct field mutation after `new()`.  Call `.run(|ctx| ...)` to build
/// the full `StageCtx` in a closure scope whose lifetime stays local: the
/// harness owns `actor`/`maps`/`reservations`, `run` borrows them to build the
/// `ScoringCtx` stack, and `body` gets a `&mut StageCtx` bound to that stack.
///
/// # Test structure (5 sections)
///
/// ```ignore
/// #[test]
/// fn name() {
///     // ── 1. Test data ──
///     let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0)).build();
///     let plans = vec![TurnPlan::default(), TurnPlan::default()];
///
///     // ── 2. Harness ──
///     let mut h = StageTestHarness::new(actor);
///     // Optional tweaks: h.intent = TacticalIntent::FocusTarget;
///     //                  h.maps.danger.add(tile, 1.0);
///     //                  h.agenda = Some(my_agenda);
///
///     // ── 3. Pool ──
///     let mut pool = PoolBuilder::new(plans)
///         .scores(&[0.5, 0.4])
///         .trace_base_eq_score() // required for downstream score-effect stages
///         .build();
///
///     // ── 4. Act ──
///     h.run(|ctx| SanityStage.apply(&mut pool, ctx));
///
///     // ── 5. Assert ──
///     for ann in &pool.annotations { assert!(ann.sanity.is_empty()); }
/// }
/// ```
pub(crate) struct StageTestHarness {
    pub actor: UnitFixture,
    pub intent: TacticalIntent,
    pub intent_reason: IntentReason,
    pub maps: InfluenceMaps,
    pub difficulty: DifficultyProfile,
    pub reservations: Reservations,
    pub agenda: Option<Agenda>,
    /// Extra units placed in the `BattleSnapshot` alongside the actor.
    /// Use when the stage under test reads `ctx.scoring.snap.units` (e.g.,
    /// critics that look for enemies or allies in the snapshot).
    pub extra_units: Vec<UnitFixture>,
}

impl StageTestHarness {
    /// Sane defaults: solo-actor `BattleSnapshot`, empty maps, default
    /// difficulty, empty reservations, `intent = Reposition`,
    /// `intent_reason = NoRuleDefault`, no agenda.
    pub fn new(actor: UnitFixture) -> Self {
        Self {
            actor,
            intent: TacticalIntent::Reposition,
            intent_reason: IntentReason::NoRuleDefault,
            maps: empty_maps(),
            difficulty: DifficultyProfile::default(),
            reservations: Reservations::default(),
            agenda: None,
            extra_units: vec![],
        }
    }

    /// Build the full context stack and run `body` with a `&mut StageCtx`.
    ///
    /// Internally builds: `ActiveContentData` → `AiWorld` → `BattleSnapshot` →
    /// `ScoringCtx` → `DiceRng` → `StageCtx`.  If `self.agenda` is `Some`,
    /// attaches it via `StageCtx::with_agenda` before handing ctx to `body`.
    /// Returns whatever `body` returns.
    pub fn run<R>(&self, body: impl FnOnce(&mut StageCtx) -> R) -> R {
        let content = empty_content();
        let world = make_test_ctx(&content, &self.difficulty);
        let mut snap_units = vec![self.actor.clone()];
        snap_units.extend(self.extra_units.iter().cloned());
        let snap = snapshot_from(snap_units, 1);
        let scoring = make_scoring_ctx(&world, &snap, &self.maps, &self.reservations, &self.actor);
        let mut rng = DiceRng::default();
        let mut ctx = StageCtx::new(
            &scoring,
            self.intent,
            self.intent_reason.clone(),
            self.actor.pos,
            &mut rng,
        );
        if let Some(ref agenda) = self.agenda {
            ctx = ctx.with_agenda(agenda);
        }
        body(&mut ctx)
    }
}

// ── PoolBuilder ───────────────────────────────────────────────────────────

/// Fluent builder for `ScoredPool` — sets per-`PipelineAnnotation` fields via
/// orthogonal, chainable setters.  Each per-plan setter asserts that the
/// supplied slice length matches the number of plans.
///
/// Use `customize()` as an escape hatch when the standard setters are not
/// expressive enough.
///
/// # Example
///
/// ```ignore
/// let pool = PoolBuilder::new(plans)
///     .scores(&[0.8, 0.5])
///     .trace_base_eq_score() // must come after scores()
///     .factors(vec![pfv_a, pfv_b])
///     .build();
/// ```
pub(crate) struct PoolBuilder {
    pool: ScoredPool,
}

impl PoolBuilder {
    /// Initialise from a plan list.  All annotations are zero-filled
    /// (`PipelineAnnotation::default()`).
    pub fn new(plans: Vec<TurnPlan>) -> Self {
        Self {
            pool: ScoredPool::new(plans),
        }
    }

    /// Set `ann.score` for each plan.
    pub fn scores(mut self, scores: &[f32]) -> Self {
        assert_eq!(
            scores.len(),
            self.pool.plans.len(),
            "PoolBuilder::scores — slice length {} != plans len {}",
            scores.len(),
            self.pool.plans.len()
        );
        for (ann, &s) in self.pool.annotations.iter_mut().zip(scores.iter()) {
            ann.set_score(s);
        }
        self
    }

    /// Set `ann.score_initial` for each plan.
    pub fn score_initials(mut self, initials: &[f32]) -> Self {
        assert_eq!(
            initials.len(),
            self.pool.plans.len(),
            "PoolBuilder::score_initials — slice length {} != plans len {}",
            initials.len(),
            self.pool.plans.len()
        );
        for (ann, &v) in self.pool.annotations.iter_mut().zip(initials.iter()) {
            ann.score_initial = v;
        }
        self
    }

    /// Set `ann.factors` for each plan.
    pub fn factors(mut self, factors: Vec<PlanFactorValues>) -> Self {
        assert_eq!(
            factors.len(),
            self.pool.plans.len(),
            "PoolBuilder::factors — vec length {} != plans len {}",
            factors.len(),
            self.pool.plans.len()
        );
        for (ann, f) in self.pool.annotations.iter_mut().zip(factors.into_iter()) {
            ann.factors = f;
        }
        self
    }

    /// Set `ann.adaptation` for each plan.
    pub fn adaptations(mut self, adaptations: Vec<Option<AdaptationData>>) -> Self {
        assert_eq!(
            adaptations.len(),
            self.pool.plans.len(),
            "PoolBuilder::adaptations — vec length {} != plans len {}",
            adaptations.len(),
            self.pool.plans.len()
        );
        for (ann, a) in self
            .pool
            .annotations
            .iter_mut()
            .zip(adaptations.into_iter())
        {
            ann.adaptation = a;
        }
        self
    }

    /// Set `ann.per_item` for each plan.
    pub fn per_items(mut self, items: Vec<Vec<PerItemEval>>) -> Self {
        assert_eq!(
            items.len(),
            self.pool.plans.len(),
            "PoolBuilder::per_items — vec length {} != plans len {}",
            items.len(),
            self.pool.plans.len()
        );
        for (ann, v) in self.pool.annotations.iter_mut().zip(items.into_iter()) {
            ann.per_item = v;
        }
        self
    }

    /// Copy current `ann.score` into `ann.score_trace.base` for every plan.
    ///
    /// Call this **after** `scores()`.  Mirrors what `FinalizeStage` does in
    /// production so that downstream score-effect stages (Sanity, Critics,
    /// ProtectSelf, KillableGate) see a non-zero `trace.base` when run in
    /// isolation.
    pub fn trace_base_eq_score(mut self) -> Self {
        for ann in self.pool.annotations.iter_mut() {
            ann.score_trace.base = ann.score;
        }
        self
    }

    /// Escape hatch: arbitrary mutation of the annotation slice after all
    /// field setters have run.
    pub fn customize(mut self, f: impl FnOnce(&mut [PipelineAnnotation])) -> Self {
        f(&mut self.pool.annotations);
        self
    }

    /// Consume the builder and return the finished `ScoredPool`.
    pub fn build(self) -> ScoredPool {
        self.pool
    }
}

// ── Critic test helpers ───────────────────────────────────────────────────

/// Owned context for direct-`evaluate` critic tests (Pattern B).
///
/// Holds all the data that the lifetime-chained `ScoringCtx` borrows from,
/// so the test body stays clean.  Use [`CriticScenarioBuilder`] to construct,
/// then call [`CriticScenario::run`] to receive a ready `ScoringCtx`.
///
/// # Example
///
/// ```ignore
/// let scn = CriticScenarioBuilder::new(caster)
///     .with_units(vec![target])
///     .with_ability("heal", heal_ability("heal"))
///     .build();
///
/// assert_critic_fires(&HealWithoutRescueValue, &plan, &scn,
///     CriticKind::HealWithoutRescueValue, expected_multiplier,
///     |reason| { /* inspect CriticReason fields */ });
/// ```
pub(crate) struct CriticScenario {
    actor: UnitFixture,
    content: ActiveContentData,
    difficulty: crate::combat::ai::config::difficulty::DifficultyProfile,
    snap_units: Vec<UnitFixture>,
    maps: crate::combat::ai::world::influence::InfluenceMaps,
    reservations: crate::combat::ai::world::reservations::Reservations,
}

impl CriticScenario {
    /// Build the lifetime-chained `ScoringCtx` and pass it to `body`.
    ///
    /// The closure receives `(&ScoringCtx, &TurnPlan, &PlanAnnotation)` so
    /// the caller never needs to keep plan/ann separately.
    pub fn run<R>(
        &self,
        body: impl FnOnce(&crate::combat::ai::orchestration::ScoringCtx<'_, '_>) -> R,
    ) -> R {
        let world = make_test_ctx(&self.content, &self.difficulty);
        let snap = snapshot_from(self.snap_units.clone(), 1);
        let ctx = make_scoring_ctx(&world, &snap, &self.maps, &self.reservations, &self.actor);
        body(&ctx)
    }
}

/// Fluent builder for [`CriticScenario`].
///
/// # Example
///
/// ```ignore
/// let scn = CriticScenarioBuilder::new(caster)
///     .with_units(vec![target])
///     .with_ability("buff_shield", buff_ability("buff_shield", "shield"))
///     .build();
/// ```
pub(crate) struct CriticScenarioBuilder {
    actor: UnitFixture,
    extra_units: Vec<UnitFixture>,
    abilities: Vec<(
        combat_engine::AbilityId,
        crate::content::abilities::AbilityDef,
    )>,
}

impl CriticScenarioBuilder {
    pub fn new(actor: UnitFixture) -> Self {
        Self {
            actor,
            extra_units: Vec::new(),
            abilities: Vec::new(),
        }
    }

    /// Additional units placed in the snapshot alongside the actor.
    pub fn with_units(mut self, units: Vec<UnitFixture>) -> Self {
        self.extra_units = units;
        self
    }

    /// Register one ability in the content view.  Call multiple times for
    /// multiple abilities.
    pub fn with_ability(mut self, id: &str, def: crate::content::abilities::AbilityDef) -> Self {
        self.abilities
            .push((combat_engine::AbilityId::from(id), def));
        self
    }

    pub fn build(self) -> CriticScenario {
        let mut content = empty_content();
        for (id, def) in self.abilities {
            content.abilities.insert(id, def);
        }
        let mut snap_units = vec![self.actor.clone()];
        snap_units.extend(self.extra_units);
        CriticScenario {
            actor: self.actor,
            content,
            difficulty: crate::combat::ai::config::difficulty::DifficultyProfile::default(),
            snap_units,
            maps: empty_maps(),
            reservations: crate::combat::ai::world::reservations::Reservations::default(),
        }
    }
}

/// Run `critic.evaluate(plan, ctx)` inside a [`CriticScenario`].
///
/// Returns the `Option<CriticHit>` from the critic.
pub(crate) fn run_critic<C: crate::combat::ai::pipeline::stages::critics::PlanCritic>(
    critic: &C,
    plan: &crate::combat::ai::plan::types::TurnPlan,
    scn: &CriticScenario,
) -> Option<crate::combat::ai::pipeline::stages::critics::CriticHit> {
    scn.run(|ctx| critic.evaluate(plan, ctx))
}

/// Assert that a critic **fires** for the given plan, checking kind,
/// multiplier, and reason.
///
/// - `expected_kind`: the `CriticKind` variant the hit must carry.
/// - `expected_multiplier`: exact multiplier value (compared with ε = 1e-6).
/// - `reason_check`: closure that receives `&CriticReason`; panic inside it
///   if the reason fields are wrong.
///
/// Returns the `CriticHit` so callers can do additional assertions.
pub(crate) fn assert_critic_fires<C>(
    critic: &C,
    plan: &crate::combat::ai::plan::types::TurnPlan,
    scn: &CriticScenario,
    expected_kind: crate::combat::ai::pipeline::stages::critics::CriticKind,
    expected_multiplier: f32,
    reason_check: impl FnOnce(&crate::combat::ai::pipeline::stages::critics::CriticReason),
) -> crate::combat::ai::pipeline::stages::critics::CriticHit
where
    C: crate::combat::ai::pipeline::stages::critics::PlanCritic,
{
    let hit = run_critic(critic, plan, scn)
        .unwrap_or_else(|| panic!("critic {:?} must fire, but returned None", expected_kind));
    assert_eq!(
        hit.critic, expected_kind,
        "critic kind mismatch: expected {:?}, got {:?}",
        expected_kind, hit.critic,
    );
    assert!(
        (hit.multiplier - expected_multiplier).abs() < 1e-6,
        "multiplier mismatch: expected {expected_multiplier}, got {}",
        hit.multiplier,
    );
    reason_check(&hit.reason);
    hit
}

/// Assert that a critic **does not fire** for the given plan.
pub(crate) fn assert_critic_passes<C>(
    critic: &C,
    plan: &crate::combat::ai::plan::types::TurnPlan,
    scn: &CriticScenario,
) where
    C: crate::combat::ai::pipeline::stages::critics::PlanCritic,
{
    let result = run_critic(critic, plan, scn);
    assert!(
        result.is_none(),
        "critic must not fire, but returned {:?}",
        result,
    );
}

// ── Stage-flow critic helpers (Pattern A) ────────────────────────────────

/// Assert that a critic fired via the full stage flow
/// (`CriticsStage → PoolBuilder → StageTestHarness`).
///
/// Checks that exactly one `MultiplierKind::Critic` hit appears in
/// `pool.annotations[0].score_trace.multipliers`, that its `value` matches
/// `expected_multiplier` (ε = 1e-6), and that its `CriticKind` matches
/// `expected_kind`.  Then calls `reason_check` with the `&CriticReason`.
///
/// # Arguments
///
/// * `harness` — pre-configured `StageTestHarness` (actor + maps + extra_units).
/// * `plans`   — plans to put in the pool (first plan is inspected).
/// * `critic`  — the `PlanCritic` to wrap in `CriticsStage`.
/// * `reason_check` — closure receiving `&CriticReason`; panic inside to fail.
pub(crate) fn assert_stage_critic_fires<C>(
    harness: &StageTestHarness,
    plans: Vec<crate::combat::ai::plan::types::TurnPlan>,
    critic: C,
    expected_kind: crate::combat::ai::pipeline::stages::critics::CriticKind,
    expected_multiplier: f32,
    reason_check: impl FnOnce(&crate::combat::ai::pipeline::stages::critics::CriticReason),
) where
    C: crate::combat::ai::pipeline::stages::critics::PlanCritic + 'static,
{
    use crate::combat::ai::pipeline::score_trace::{MultiplierDetail, MultiplierKind};
    use crate::combat::ai::pipeline::stages::critics::CriticsStage;
    use crate::combat::ai::pipeline::PlanStage;

    let stage = CriticsStage::single(critic);
    let mut pool = PoolBuilder::new(plans)
        .scores(&[1.0])
        .trace_base_eq_score()
        .build();
    harness.run(|ctx| stage.apply(&mut pool, ctx));

    let hits: Vec<_> = pool.annotations[0]
        .score_trace
        .multipliers
        .iter()
        .filter(|m| matches!(m.kind, MultiplierKind::Critic))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one Critic multiplier, got {}",
        hits.len()
    );
    let hit = hits[0];
    assert!(
        (hit.value - expected_multiplier).abs() < 1e-6,
        "stage critic multiplier mismatch: expected {expected_multiplier}, got {}",
        hit.value,
    );
    if let Some(MultiplierDetail::Critic {
        critic: kind,
        reason,
    }) = &hit.detail
    {
        assert_eq!(
            *kind, expected_kind,
            "critic kind mismatch: expected {:?}, got {:?}",
            expected_kind, kind,
        );
        reason_check(reason);
    } else {
        panic!(
            "Critic multiplier must carry MultiplierDetail::Critic, got {:?}",
            hit.detail,
        );
    }
}

/// Assert that a critic **does not fire** via the full stage flow.
///
/// Checks that no `MultiplierKind::Critic` hit appears in
/// `pool.annotations[0].score_trace.multipliers`.
pub(crate) fn assert_stage_critic_passes<C>(
    harness: &StageTestHarness,
    plans: Vec<crate::combat::ai::plan::types::TurnPlan>,
    critic: C,
) where
    C: crate::combat::ai::pipeline::stages::critics::PlanCritic + 'static,
{
    use crate::combat::ai::pipeline::score_trace::MultiplierKind;
    use crate::combat::ai::pipeline::stages::critics::CriticsStage;
    use crate::combat::ai::pipeline::PlanStage;

    let stage = CriticsStage::single(critic);
    let mut pool = PoolBuilder::new(plans)
        .scores(&[1.0])
        .trace_base_eq_score()
        .build();
    harness.run(|ctx| stage.apply(&mut pool, ctx));

    let critic_hits: Vec<_> = pool.annotations[0]
        .score_trace
        .multipliers
        .iter()
        .filter(|m| matches!(m.kind, MultiplierKind::Critic))
        .collect();
    assert!(
        critic_hits.is_empty(),
        "critic must not fire, but got {} Critic multiplier(s): {:?}",
        critic_hits.len(),
        critic_hits,
    );
}
