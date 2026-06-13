//! Trap / hazard severity scoring — soft AI-avoidance ranking weight.
//!
//! `severity` returns a unit-independent HP-equivalent score for a trap
//! identified by its trigger ability.  The value is a **ranking heuristic**,
//! not a per-target damage prediction:
//!
//! - `expected_damage_avg` uses bare dice expected value — no caster modifiers,
//!   no armor mitigation — because the trap's effective caster context is
//!   unknown at precompute time.
//! - `status_cost` delegates to `policy::status::value` with a *neutral
//!   reference* unit rather than the real mover.  The neutral reference
//!   approximates an "average defender", so CC trap value is fixed rather
//!   than scaled to the actual victim.  This is acceptable because severity
//!   only nudges pathfinding penalties, never ability legality or true damage.
//!
//! One cached severity value per `EnvId` is therefore valid for all consumers
//! in the same decision cycle.

use crate::combat::ai::scoring::policy::status;
use crate::combat::ai::world::cache::UnitAiCache;
use crate::combat::ai::world::snapshot::UnitView;
use crate::content::content_view::ActiveContentData;
use combat_engine::AbilityId;
use combat_engine::EffectDef;

/// Heuristic "average defender" HP for trap-severity ranking.
///
/// Chosen as the midpoint of a typical frontliner HP range (15–25 at
/// encounter level 1–2).  The exact value only affects %HP DoT scaling in
/// `policy::status::value`; directional ordering of traps is robust to ±5 HP.
pub(crate) const NEUTRAL_REF_MAX_HP: i32 = 20;

/// Neutral per-turn threat used when `damage_horizon` is empty.
///
/// `policy::status::value` falls back to `threat × duration` for stun/silence
/// cost when `damage_horizon` is empty (see `horizon::horizon_window_sum`).
/// 5.0 matches the canonical `UnitBuilder` bruiser default and represents a
/// "deal ~5 HP per round on average" attacker — a reasonable midpoint between
/// low-damage supports (2–3) and burst mages (8–12).  We intentionally leave
/// `damage_horizon` empty to keep severity deterministic and unit-independent.
pub(crate) const NEUTRAL_REF_THREAT: f32 = 5.0;

/// Construct the canonical "neutral reference" pair used by `build_snapshot`
/// for trap severity precomputation.
///
/// Returns `(engine Unit, UnitAiCache)` — callers bind them to locals and
/// construct a `UnitView { state: &u, cache: &c }` to pass to `severity`.
/// The Hp pool is explicitly set so that `max_hp()` == `NEUTRAL_REF_MAX_HP`.
pub(crate) fn neutral_reference_pair() -> (combat_engine::state::Unit, UnitAiCache) {
    use crate::combat::ai::world::tags::AiTags;
    use bevy::prelude::Entity;
    use combat_engine::state::{Team as EngineTeam, UnitId};
    use combat_engine::{
        enum_map::enum_map, CasterContext as EngineCasterContext, CritFailOutcome, PoolKind,
        RegenRule,
    };

    let entity = Entity::from_raw_u32(0).expect("raw 0 is a valid Entity");
    let uid = UnitId(entity.to_bits());

    let engine_unit = combat_engine::state::Unit::new(
        uid,
        EngineTeam::Player,
        crate::game::hex::hex_from_offset(0, 0),
        0,          // armor
        0,          // magic_resist
        0,          // armor_bonus
        0,          // damage_taken_bonus
        0,          // base_speed
        0,          // speed
        0,          // reactions_left
        1,          // ap_max (placeholder; not read by severity)
        Vec::new(), // statuses
        None,       // summoner
        None,       // initiative
        EngineCasterContext {
            str_mod: 0,
            int_mod: 0,
            spell_power: 0,
            weapon_dice: None,
            ranged_dice: None,
            crit_fail_outcome: CritFailOutcome::Miss,
            dex_mod: 0,
        },
        None,       // aoo_dice
        Vec::new(), // env_objects (unused)
        Vec::new(), // ability list (unused)
        enum_map! {
            PoolKind::Hp     => Some((NEUTRAL_REF_MAX_HP, NEUTRAL_REF_MAX_HP)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((1, 1)),
            PoolKind::Mp     => Some((0, 0)),
        },
        enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        None,
    );

    let ai_cache = UnitAiCache {
        entity,
        role: crate::combat::ai::config::role::AxisProfile::default(),
        threat: NEUTRAL_REF_THREAT,
        tags: AiTags::empty(),
        max_attack_range: 0,
        aoo_expected_damage: None,
        // Empty horizon → stun/silence cost uses `threat × duration` fallback,
        // which is deterministic and unit-independent.
        damage_horizon: Vec::new(),
        crit_fail_effect: crate::content::races::CritFailEffect::default(),
        ai_tuning_override: None,
        abilities: Vec::new(),
        caster_ctx: Default::default(),
        forced_mode: None,
    };

    (engine_unit, ai_cache)
}

/// Soft avoidance ranking weight for a trap whose trigger ability is `ability`.
///
/// Returns `0.0` if the ability is absent from `content` or has no damage/status
/// effect.  The match over `EffectDef` is exhaustive — a future engine variant
/// becomes a compile error rather than a silent `0.0`.
pub fn severity(
    ability: &AbilityId,
    content: &ActiveContentData,
    neutral_ref: UnitView<'_>,
) -> f32 {
    let Some(def) = content.abilities.get(ability) else {
        return 0.0;
    };

    let expected_damage_avg: f32 = match &def.effect {
        EffectDef::Damage { dice } => dice.expected(),
        EffectDef::SpellDamage { dice } => dice.expected(),
        // Non-damage variants contribute zero to the damage component.
        EffectDef::None => 0.0,
        EffectDef::WeaponAttack { .. } => 0.0,
        EffectDef::Heal { .. } => 0.0,
        EffectDef::GrantMovement { .. } => 0.0,
        EffectDef::RestoreResources => 0.0,
        EffectDef::Summon { .. } => 0.0,
        EffectDef::RevealEnvInRange { .. } => 0.0,
    };

    let status_cost = status::value(def, neutral_ref, content);

    expected_damage_avg + status_cost
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::scoring::policy::status;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::abilities::AbilityDef;
    use crate::content::content_view::ActiveContentData;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::{
        AbilityDef as EngineAbilityDef, AbilityId, AbilityRange, AoEShape, DiceExpr, EffectDef,
        StatusApplication, TargetType,
    };

    fn empty_content() -> ActiveContentData {
        ActiveContentData::default()
    }

    /// Build a minimal `AbilityDef` (bridge wrapper) with the given engine effect.
    fn make_ability(id: &str, effect: EffectDef, statuses: Vec<StatusApplication>) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_owned(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: EngineAbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect,
                costs: Vec::new(),
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses,
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
            },
        }
    }

    fn insert_ability(content: &mut ActiveContentData, def: AbilityDef) {
        content.abilities.insert(def.id.clone(), def);
    }

    // ── damage = dice.expected ────────────────────────────────────────────────

    /// `neutral_reference_pair()` must produce a unit where `max_hp()` == 20,
    /// confirming the pool-backed getter returns the right value (not 0).
    #[test]
    fn neutral_ref_pair_max_hp_is_pool_backed() {
        let (u, c) = neutral_reference_pair();
        let view = UnitView {
            state: &u,
            cache: &c,
        };
        assert_eq!(
            view.max_hp(),
            NEUTRAL_REF_MAX_HP,
            "neutral reference max_hp() must be pool-backed to {NEUTRAL_REF_MAX_HP}"
        );
    }

    /// %HP DoT on the neutral reference: tick = ceil(20 * 5 / 100) = 1.0,
    /// severity contribution = 1.0 * 2 rounds = 2.0.
    /// The ability has no direct damage and no other status effects, so
    /// severity == exactly 2.0.
    #[test]
    fn severity_counts_hp_percent_dot_against_neutral_max_hp() {
        use combat_engine::StatusId;

        let status_id = StatusId::from("test_dot");

        // Build a minimal content::statuses::StatusDef with only hp_percent_dot = 5.
        let mut content = empty_content();
        content.statuses.insert(
            status_id.clone(),
            crate::content::statuses::StatusDef {
                id: status_id.clone(),
                name: "test_dot".to_owned(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: combat_engine::StatusDef {
                    hp_percent_dot: 5,
                    ..Default::default()
                },
            },
        );

        let ability_id = AbilityId::from("pct_dot_2r");
        let def = make_ability(
            "pct_dot_2r",
            EffectDef::None,
            vec![StatusApplication {
                status: status_id,
                duration_rounds: 2,
                on: combat_engine::StatusOn::Target,
            }],
        );
        insert_ability(&mut content, def);

        let (u, c) = neutral_reference_pair();
        let neutral = UnitView {
            state: &u,
            cache: &c,
        };
        let s = severity(&ability_id, &content, neutral);
        // tick = ceil(20 * 5 / 100) = 1.0; total = 1.0 * 2 = 2.0
        assert!(
            (s - 2.0).abs() < 1e-6,
            "expected severity 2.0 for 5% hp-dot over 2 rounds on a 20-hp neutral, got {s}"
        );
    }

    /// A pure `Damage{2d6}` ability has severity equal to `dice.expected()` (7.0)
    /// with zero status_cost.
    #[test]
    fn severity_pure_damage_equals_dice_expected() {
        let mut content = empty_content();
        let ability_id = AbilityId::from("spike_2d6");
        let def = make_ability(
            "spike_2d6",
            EffectDef::Damage {
                dice: DiceExpr::new(2, 6, 0),
            },
            vec![],
        );
        insert_ability(&mut content, def);

        let (u, c) = neutral_reference_pair();
        let neutral = UnitView {
            state: &u,
            cache: &c,
        };
        let s = severity(&ability_id, &content, neutral);
        assert!(
            (s - 7.0).abs() < 1e-6,
            "expected 7.0 (2d6 expected value), got {s}"
        );
    }

    // ── status-only ability ───────────────────────────────────────────────────

    /// A `None`-effect ability with a status has severity == `policy::status::value`.
    /// Guards that we delegate to the canonical source-of-truth rather than
    /// re-implementing.
    #[test]
    fn severity_status_only_ability_is_status_cost() {
        let content = ActiveContentData::load_global_for_tests();

        // Find any ability that has no direct damage but applies statuses.
        let (ability_id, def) = content
            .abilities
            .iter()
            .find(|(_, d)| matches!(d.effect, EffectDef::None) && !d.statuses.is_empty())
            .expect("need at least one status-only ability in content");

        let (u, c) = neutral_reference_pair();
        let neutral = UnitView {
            state: &u,
            cache: &c,
        };
        let expected = status::value(def, neutral, &content);
        let actual = severity(ability_id, &content, neutral);
        assert!(
            (actual - expected).abs() < 1e-6,
            "severity should equal status::value for a no-damage ability: \
             expected {expected}, got {actual}"
        );
    }

    // ── non-damage, no-status ─────────────────────────────────────────────────

    /// A `GrantMovement` ability with no statuses → severity == 0.0.
    #[test]
    fn severity_non_damage_no_status_is_zero() {
        let mut content = empty_content();
        let ability_id = AbilityId::from("dash");
        let def = make_ability("dash", EffectDef::GrantMovement { distance: 3 }, vec![]);
        insert_ability(&mut content, def);

        let (u, c) = neutral_reference_pair();
        let neutral = UnitView {
            state: &u,
            cache: &c,
        };
        let s = severity(&ability_id, &content, neutral);
        assert_eq!(
            s, 0.0,
            "non-damage/no-status ability should have severity 0.0"
        );
    }

    // ── unit-independence ─────────────────────────────────────────────────────

    /// Severity is identical regardless of which real actor snapshot it was
    /// computed relative to.  This is the key cache-validity invariant: a single
    /// cached value per `EnvId` is valid for all consumers in the decision cycle.
    #[test]
    fn severity_unit_independent() {
        let mut content = empty_content();
        let ability_id = AbilityId::from("spike_1d6");
        let def = make_ability(
            "spike_1d6",
            EffectDef::Damage {
                dice: DiceExpr::new(1, 6, 0),
            },
            vec![],
        );
        insert_ability(&mut content, def);

        // Two actors with very different stat lines.
        let (gu, gc) = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .max_hp(5)
            .threat(20.0)
            .build_pair();
        let (tu, tc) = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .max_hp(60)
            .threat(2.0)
            .build_pair();
        let glass_cannon = UnitView {
            state: &gu,
            cache: &gc,
        };
        let tank = UnitView {
            state: &tu,
            cache: &tc,
        };

        let s1 = severity(&ability_id, &content, glass_cannon);
        let s2 = severity(&ability_id, &content, tank);
        assert_eq!(
            s1, s2,
            "severity must be unit-independent (pure Damage trap, no statuses): \
             glass_cannon={s1}, tank={s2}"
        );
    }
}
