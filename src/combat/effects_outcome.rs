//! Shared ability-resolution core: compute what an ability *does* against the
//! current state, without mutating anything.
//!
//! The live pipeline and the AI sim both describe the same ability the same
//! way; they diverge only in *how* they apply the outcome. This module owns
//! the "what happens" decision — affected targets, raw damage / heal numbers,
//! status applications — and the backends just consume the resulting
//! `AbilityOutcome`.
//!
//! Not handled here (yet): AP / resource cost spending, crit-fail rolls and
//! side effects, combat log events, end-of-turn emission. Those are
//! backend-specific glue around the outcome.

use crate::content::abilities::{AbilityDef, CasterContext, EffectDef, StatusOn};
use crate::content::races::CritFailEffect;
use crate::core::{DiceExpr, DiceRng, ResourceKind, StatusId};
use bevy::prelude::Entity;

// ── Dice abstraction ────────────────────────────────────────────────────────

/// Source of dice rolls during outcome computation. Real uses the shared
/// `DiceRng`; sim uses `ExpectedValue` (collapsed to the mean, floored to
/// integer to match the game's integer damage contract).
pub trait DiceSource {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String);
    /// Critical failure check. Real rolls `1..=die` and returns `true` on 1;
    /// sim treats abilities as always succeeding (returns `false`) — matches
    /// the planner's greedy-replan assumption.
    fn roll_crit_fail(&mut self, crit_fail_die: u32) -> bool;
}

/// Wraps `&mut DiceRng` for the live pipeline. Rolls integers through the
/// usual RNG; `disadvantage` → `roll_dice_disadvantage`.
pub struct RngDice<'a>(pub &'a mut DiceRng);

impl DiceSource for RngDice<'_> {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String) {
        if disadvantage {
            self.0.roll_dice_disadvantage(expr)
        } else {
            self.0.roll_dice(expr)
        }
    }
    fn roll_crit_fail(&mut self, die: u32) -> bool {
        self.0.roll_d(die) == 1
    }
}

/// Deterministic expected-value source for the planner. Returns
/// `expr.expected().round() as i32`, empty breakdown. Disadvantage is ignored
/// at this layer — sim treats it as the same EV (exact disadvantage EV needs
/// pairwise min integration, overkill for planning).
pub struct ExpectedValue;

impl DiceSource for ExpectedValue {
    fn roll_dice(&mut self, expr: &DiceExpr, _disadvantage: bool) -> (i32, String) {
        (expr.expected().round() as i32, String::new())
    }
    fn roll_crit_fail(&mut self, _die: u32) -> bool {
        false
    }
}

// ── Outcome data model ──────────────────────────────────────────────────────

/// Per-ability resolution summary. The `primary` branch describes the single
/// direct effect (damage / heal / movement / etc.). Status applications are
/// collected independently — an ability can have both (e.g., poison dart
/// does damage AND applies poison status).
#[derive(Debug)]
pub struct AbilityOutcome {
    pub affected: Vec<Entity>,
    pub primary: OutcomePrimary,
    pub statuses: Vec<StatusApply>,
    /// Dice breakdown for combat-log display (e.g., `"1d6+3 = 7"`). Empty when
    /// `primary` is non-dice (`GrantMovement`, `RestoreResources`, `Summon`,
    /// `None`). Carried here so real backend can log without re-rolling.
    pub breakdown: String,
    /// Critical-failure side effect. `Some` ⇒ primary effects are skipped
    /// (`primary` is `None`, `affected` / `statuses` are empty); backend
    /// applies the carried side effect and logs `CriticalMiss`.
    pub crit_fail: Option<CritFail>,
    /// `true` iff a crit-fail mapped to `ManaOverload` **and** the ability has
    /// a mana cost — primary effects still fire, but the backend must double
    /// the mana cost (with HP deficit damage on underspend) and log
    /// `WillOverload`.
    pub mana_overload: bool,
}

/// Crit-fail side effect applied to the actor. Compute-side mapping lives in
/// `compute_ability_outcome`; backends just consume the variant.
#[derive(Debug)]
pub enum CritFail {
    /// No side effect — only the `CriticalMiss` combat-log entry.
    Miss,
    /// Apply a status to the actor (BrokenFaith / Exhaustion / PactControl).
    SelfStatus {
        status: StatusId,
        duration_rounds: u32,
        log_description: String,
    },
    /// Deal self-damage to the actor (CircuitBreach).
    SelfDamage {
        amount: i32,
        damage_breakdown: String,
        log_description: String,
    },
}

#[derive(Debug)]
pub enum OutcomePrimary {
    /// Uniform raw damage to every `affected` target. Mitigation (armor +
    /// status vulnerability + min-1 floor) is applied at the *backend* to
    /// match each backend's state shape — live reads Bevy components, sim
    /// reads snapshot aggregates.
    Damage { raw: i32, pierces_armor: bool },
    /// Uniform heal amount to every `affected` target. Real backend neutralises
    /// target DoTs first (handled in `apply_effects_system`); sim currently
    /// just adds to HP (drift #2, addressed in a later stage).
    Heal { amount: i32 },
    GrantMovement { distance: i32 },
    RestoreResources,
    Summon { template: String, max_active: Option<u32> },
    /// `ToggleMoveMode` or an ability whose only effect is status application.
    None,
}

#[derive(Debug)]
pub struct StatusApply {
    pub target: Entity,
    pub status: StatusId,
    pub duration_rounds: u32,
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Resolve an ability against pre-computed affected targets, producing the
/// full outcome. Caller computes `affected` via `compute_affected_targets`
/// — split out so the Bevy backend can release its target-enumeration query
/// before acquiring the rng / caster's mutable components.
///
/// Crit-fail is rolled here through `rng.roll_crit_fail`: real rolls a real
/// die, sim (via `ExpectedValue`) always returns `false`. A crit that maps
/// to `ManaOverload` fires primary effects with the `mana_overload` flag
/// set; any other crit variant skips primary effects and surfaces a
/// `CritFail` side effect on `outcome.crit_fail`.
#[allow(clippy::too_many_arguments)]
pub fn compute_ability_outcome<R: DiceSource>(
    actor: Entity,
    def: &AbilityDef,
    affected: Vec<Entity>,
    caster_ctx: &CasterContext,
    disadvantage: bool,
    crit_fail_die: u32,
    crit_fail_effect: &CritFailEffect,
    rng: &mut R,
) -> AbilityOutcome {
    let crit_failed = rng.roll_crit_fail(crit_fail_die);
    let mana_cost: i32 = def
        .costs
        .iter()
        .filter(|c| matches!(c.resource, ResourceKind::Mana))
        .map(|c| c.amount)
        .sum();
    let mana_overload = crit_failed
        && matches!(crit_fail_effect, CritFailEffect::ManaOverload)
        && mana_cost > 0;
    let skip_effects = crit_failed && !mana_overload;

    if skip_effects {
        let crit = map_crit_fail(crit_fail_effect, mana_cost);
        return AbilityOutcome {
            affected: Vec::new(),
            primary: OutcomePrimary::None,
            statuses: Vec::new(),
            breakdown: String::new(),
            crit_fail: Some(crit),
            mana_overload: false,
        };
    }

    let (primary, breakdown) = match def.effect.calc(caster_ctx) {
        Some(calc) => {
            let (roll_total, dice_str) = match &calc.dice {
                Some(d) => rng.roll_dice(d, disadvantage),
                None => (0, String::new()),
            };
            let raw = roll_total + calc.bonus;
            let breakdown = effect_breakdown(&dice_str, calc.bonus, raw);
            let primary = if calc.is_heal {
                OutcomePrimary::Heal { amount: raw }
            } else {
                OutcomePrimary::Damage {
                    raw,
                    pierces_armor: calc.pierces_armor,
                }
            };
            (primary, breakdown)
        }
        None => {
            let p = match &def.effect {
                EffectDef::GrantMovement { distance } => OutcomePrimary::GrantMovement {
                    distance: *distance,
                },
                EffectDef::RestoreResources => OutcomePrimary::RestoreResources,
                EffectDef::Summon { template, max_active } => OutcomePrimary::Summon {
                    template: template.clone(),
                    max_active: *max_active,
                },
                // `None` (pure-status ability) and `ToggleMoveMode` both land here.
                _ => OutcomePrimary::None,
            };
            (p, String::new())
        }
    };

    // Status applications: unified enumeration — `Target` expands over affected,
    // `MySelf` lands once on the actor. Matches live pipeline and sim both.
    let mut statuses = Vec::new();
    for sa in &def.statuses {
        match sa.on {
            StatusOn::Target => {
                for &t in &affected {
                    statuses.push(StatusApply {
                        target: t,
                        status: sa.status.clone(),
                        duration_rounds: sa.duration_rounds,
                    });
                }
            }
            StatusOn::MySelf => {
                statuses.push(StatusApply {
                    target: actor,
                    status: sa.status.clone(),
                    duration_rounds: sa.duration_rounds,
                });
            }
        }
    }

    AbilityOutcome {
        affected,
        primary,
        statuses,
        breakdown,
        crit_fail: None,
        mana_overload,
    }
}

/// Map a path's `CritFailEffect` to the concrete side-effect variant that
/// backends apply. Strings used here come from the live pipeline's log
/// copy — moved into shared code so both backends read identical text.
fn map_crit_fail(effect: &CritFailEffect, mana_cost: i32) -> CritFail {
    match effect {
        CritFailEffect::Miss | CritFailEffect::ManaOverload => CritFail::Miss,
        CritFailEffect::BrokenFaith => CritFail::SelfStatus {
            status: StatusId::from("broken_faith"),
            duration_rounds: 1,
            log_description: "Сломленная вера — магия заблокирована на 1 ход".into(),
        },
        CritFailEffect::CircuitBreach => {
            let amount = (mana_cost + 1) / 2;
            CritFail::SelfDamage {
                amount,
                damage_breakdown: format!("разгерметизация: {mana_cost}/2={amount}"),
                log_description: format!("Разгерметизация контура — {amount} урона себе"),
            }
        }
        CritFailEffect::Exhaustion => CritFail::SelfStatus {
            status: StatusId::from("exhaustion"),
            duration_rounds: 2,
            log_description: "Телесный откат — истощение на 2 хода".into(),
        },
        CritFailEffect::PactControl => CritFail::SelfStatus {
            status: StatusId::from("pact_control"),
            duration_rounds: 1,
            log_description: "Власть договора — AI управляет на 1 ход".into(),
        },
    }
}

/// Format a dice roll for the combat log. Matches the legacy
/// `resolution::effect_breakdown` exactly so log output is byte-identical.
fn effect_breakdown(dice_str: &str, bonus: i32, total: i32) -> String {
    if dice_str.is_empty() {
        return format!("{total}");
    }
    if bonus == 0 {
        format!("{dice_str} = {total}")
    } else {
        format!("{dice_str} + {bonus} = {total}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::abilities::{AbilityRange, AoEShape, ResourceCost, TargetType};
    use crate::core::AbilityId;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    /// `DiceSource` double that forces crit-fail and returns a fixed dice roll.
    struct MockDice {
        crit_fail: bool,
        roll: i32,
    }
    impl DiceSource for MockDice {
        fn roll_dice(&mut self, _expr: &DiceExpr, _dis: bool) -> (i32, String) {
            (self.roll, format!("mock({})", self.roll))
        }
        fn roll_crit_fail(&mut self, _die: u32) -> bool {
            self.crit_fail
        }
    }

    fn ctx() -> CasterContext {
        CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None }
    }

    fn damage_ability(mana_cost: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("strike"),
            name: "Strike".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs: if mana_cost > 0 {
                vec![ResourceCost { resource: ResourceKind::Mana, amount: mana_cost }]
            } else {
                Vec::new()
            },
            cost_ap: 1,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    #[test]
    fn crit_fail_miss_variant_skips_primary() {
        let def = damage_ability(0);
        let mut dice = MockDice { crit_fail: true, roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::Miss, &mut dice,
        );
        assert!(matches!(outcome.crit_fail, Some(CritFail::Miss)));
        assert!(outcome.affected.is_empty());
        assert!(matches!(outcome.primary, OutcomePrimary::None));
        assert!(!outcome.mana_overload);
    }

    #[test]
    fn crit_fail_broken_faith_produces_self_status() {
        let def = damage_ability(0);
        let mut dice = MockDice { crit_fail: true, roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::BrokenFaith, &mut dice,
        );
        match outcome.crit_fail {
            Some(CritFail::SelfStatus { status, duration_rounds, .. }) => {
                assert_eq!(status.0, "broken_faith");
                assert_eq!(duration_rounds, 1);
            }
            _ => panic!("expected SelfStatus, got {:?}", outcome.crit_fail),
        }
    }

    #[test]
    fn crit_fail_circuit_breach_self_damage_uses_mana_cost() {
        // mana_cost=5 → self_damage = (5+1)/2 = 3.
        let def = damage_ability(5);
        let mut dice = MockDice { crit_fail: true, roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::CircuitBreach, &mut dice,
        );
        match outcome.crit_fail {
            Some(CritFail::SelfDamage { amount, .. }) => assert_eq!(amount, 3),
            _ => panic!("expected SelfDamage, got {:?}", outcome.crit_fail),
        }
    }

    #[test]
    fn crit_fail_mana_overload_with_mana_cost_fires_effects_and_flags_overload() {
        let def = damage_ability(5);
        let mut dice = MockDice { crit_fail: true, roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::ManaOverload, &mut dice,
        );
        assert!(outcome.crit_fail.is_none(), "overload doesn't skip effects");
        assert!(outcome.mana_overload, "overload flag set");
        assert!(matches!(outcome.primary, OutcomePrimary::Damage { .. }));
    }

    #[test]
    fn crit_fail_mana_overload_without_mana_cost_falls_back_to_miss() {
        // No mana cost → overload doesn't apply → treat as plain miss.
        let def = damage_ability(0);
        let mut dice = MockDice { crit_fail: true, roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::ManaOverload, &mut dice,
        );
        assert!(matches!(outcome.crit_fail, Some(CritFail::Miss)));
        assert!(!outcome.mana_overload);
        assert!(matches!(outcome.primary, OutcomePrimary::None));
    }

    #[test]
    fn no_crit_fail_fires_primary_effect() {
        let def = damage_ability(0);
        let mut dice = MockDice { crit_fail: false, roll: 5 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, 20, &CritFailEffect::Miss, &mut dice,
        );
        assert!(outcome.crit_fail.is_none());
        assert!(!outcome.mana_overload);
        match outcome.primary {
            OutcomePrimary::Damage { raw, .. } => assert_eq!(raw, 5),
            _ => panic!("expected Damage"),
        }
        assert_eq!(outcome.affected, vec![ent(2)]);
    }
}
