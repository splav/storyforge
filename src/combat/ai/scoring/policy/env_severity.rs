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
use crate::combat::ai::world::snapshot::UnitSnapshot;
use crate::content::content_view::ContentView;
use combat_engine::AbilityId;
use combat_engine::EffectDef;

/// Soft avoidance ranking weight for a trap whose trigger ability is `ability`.
///
/// Returns `0.0` if the ability is absent from `content` or has no damage/status
/// effect.  The match over `EffectDef` is exhaustive — a future engine variant
/// becomes a compile error rather than a silent `0.0`.
pub fn severity(ability: &AbilityId, content: &ContentView, neutral_ref: &UnitSnapshot) -> f32 {
    let Some(def) = content.abilities.get(ability) else {
        return 0.0;
    };

    let expected_damage_avg: f32 = match &def.effect {
        EffectDef::Damage { dice } => dice.expected(),
        EffectDef::SpellDamage { dice } => dice.expected(),
        // Non-damage variants contribute zero to the damage component.
        EffectDef::None => 0.0,
        EffectDef::WeaponAttack => 0.0,
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
    use crate::combat::ai::world::snapshot::UnitSnapshot;
    use crate::content::abilities::AbilityDef;
    use crate::content::content_view::ContentView;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::{
        AbilityDef as EngineAbilityDef, AbilityId, AbilityRange, AoEShape, DiceExpr, EffectDef,
        StatusApplication, TargetType,
    };

    fn neutral() -> UnitSnapshot {
        UnitSnapshot::neutral_reference()
    }

    fn empty_content() -> ContentView {
        ContentView::default()
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

    fn insert_ability(content: &mut ContentView, def: AbilityDef) {
        content.abilities.insert(def.id.clone(), def);
    }

    // ── damage = dice.expected ────────────────────────────────────────────────

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

        let s = severity(&ability_id, &content, &neutral());
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
        let content = ContentView::load_global_for_tests();
        let target = neutral();

        // Find any ability that has no direct damage but applies statuses.
        let (ability_id, def) = content
            .abilities
            .iter()
            .find(|(_, d)| matches!(d.effect, EffectDef::None) && !d.statuses.is_empty())
            .expect("need at least one status-only ability in content");

        let expected = status::value(def, &target, &content);
        let actual = severity(ability_id, &content, &target);
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

        let s = severity(&ability_id, &content, &neutral());
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
        let glass_cannon = UnitBuilder::new(1, Team::Player, hex_from_offset(0, 0))
            .max_hp(5)
            .threat(20.0)
            .build();
        let tank = UnitBuilder::new(2, Team::Enemy, hex_from_offset(1, 0))
            .max_hp(60)
            .threat(2.0)
            .build();

        let s1 = severity(&ability_id, &content, &glass_cannon);
        let s2 = severity(&ability_id, &content, &tank);
        assert_eq!(
            s1, s2,
            "severity must be unit-independent (pure Damage trap, no statuses): \
             glass_cannon={s1}, tank={s2}"
        );
    }
}
