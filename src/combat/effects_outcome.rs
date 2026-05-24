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

use crate::content::abilities::{AbilityDef, CasterContext, EffectCalcExt, EffectDef, StatusOn};
use crate::content::races::CritFailEffect;
use combat_engine::{DiceExpr, DiceRng, ResourceKind, StatusId};
use bevy::prelude::Entity;

// ── Dice abstraction ────────────────────────────────────────────────────────

/// Source of dice rolls during outcome computation. Real uses the shared
/// `DiceRng`; sim uses `ExpectedValue` (collapsed to the mean, floored to
/// integer to match the game's integer damage contract).
///
/// Crit-fail is **not** part of this trait: the real backend rolls it
/// explicitly before calling `compute_ability_outcome` and passes the bool
/// in. Sim never crit-fails by construction. Keeping crit-fail out of the
/// trait removes the deadweight `roll_crit_fail` (sim hardcoded `false`)
/// and the deadweight `crit_fail_die` parameter from the sim path.
pub trait DiceSource {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String);
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
}

/// Deterministic expected-value source for the planner. Returns
/// `expr.expected().round() as i32`, empty breakdown. Disadvantage swaps
/// in `expected_disadvantage()` (per-die closed form — see
/// `DiceExpr::expected_disadvantage` for the live-divergence note).
pub struct ExpectedValue;

impl DiceSource for ExpectedValue {
    fn roll_dice(&mut self, expr: &DiceExpr, disadvantage: bool) -> (i32, String) {
        let total = if disadvantage {
            expr.expected_disadvantage().round() as i32
        } else {
            expr.expected().round() as i32
        };
        (total, String::new())
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
    /// Crit-fail state for this cast. A single value replaces the old
    /// `Option<CritFail>` + `mana_overload: bool` pair, which could encode
    /// contradictions (`Some(Miss)` with `mana_overload = true`, etc.).
    /// `Miss` / `SelfStatus` / `SelfDamage` imply `primary == None` with
    /// `affected` / `statuses` empty; `ManaOverload` keeps the primary
    /// payload and tells the backend to double the mana cost; `None`
    /// is the normal resolution path.
    pub crit: CritOutcome,
}

/// Crit-fail outcome. Single source of truth for "what did the crit do?" —
/// collapses the old three-field encoding into one enum so invalid combinations
/// (e.g., ManaOverload + primary-skipping side effect) are unrepresentable.
#[derive(Debug)]
pub enum CritOutcome {
    /// No crit-fail; the rest of the outcome is the normal resolution path.
    None,
    /// Crit-fail with no carried side effect. Primary effects and status
    /// applications are dropped. Backend logs `CriticalMiss`.
    Miss,
    /// Crit-fail applied a status to the caster (BrokenFaith / Exhaustion /
    /// PactControl). Primary effects and status applications are dropped.
    /// Backend logs `CriticalMiss` + `CritFailSideEffect`.
    SelfStatus {
        status: StatusId,
        duration_rounds: u32,
        log_description: String,
    },
    /// Crit-fail dealt self-damage to the caster (CircuitBreach). Primary
    /// effects and status applications are dropped. Backend logs
    /// `CriticalMiss` + `CritFailSideEffect`.
    SelfDamage {
        amount: i32,
        damage_breakdown: String,
        log_description: String,
    },
    /// Crit-fail mapped to ManaOverload: primary effects **still fire**, but
    /// the backend doubles the mana cost (with HP-deficit damage on
    /// underspend) and logs `WillOverload`. Only reachable when the cast has
    /// a non-zero mana cost and the path's crit-fail effect is `ManaOverload`.
    ManaOverload,
}

impl CritOutcome {
    /// Whether primary effects + statuses are suppressed by this crit outcome.
    /// True for `Miss` / `SelfStatus` / `SelfDamage` (they skip primary);
    /// false for `None` (no crit) and `ManaOverload` (primary still fires).
    pub fn skips_primary(&self) -> bool {
        matches!(
            self,
            Self::Miss | Self::SelfStatus { .. } | Self::SelfDamage { .. }
        )
    }

    /// True iff this cast should double its mana cost (ManaOverload branch).
    pub fn is_mana_overload(&self) -> bool {
        matches!(self, Self::ManaOverload)
    }
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
/// `crit_failed` is decided by the caller — real backend rolls
/// `1..=settings.crit_fail_die` against `1`, sim always passes `false`. The
/// chosen `CritOutcome` variant is the single authority on what happened —
/// `ManaOverload` keeps the normal payload with a cost-doubling flag,
/// `Miss` / `SelfStatus` / `SelfDamage` suppress the payload entirely,
/// `None` is the no-crit path. `crit_fail_effect` is only consulted when
/// `crit_failed` is true; sim callers may pass any placeholder.
#[allow(clippy::too_many_arguments)]
pub fn compute_ability_outcome<R: DiceSource>(
    actor: Entity,
    def: &AbilityDef,
    affected: Vec<Entity>,
    caster_ctx: &CasterContext,
    disadvantage: bool,
    crit_failed: bool,
    crit_fail_effect: &CritFailEffect,
    rng: &mut R,
) -> AbilityOutcome {
    let mana_cost: i32 = def
        .costs
        .iter()
        .filter(|c| matches!(c.resource, ResourceKind::Mana))
        .map(|c| c.amount)
        .sum();

    let crit = if crit_failed {
        map_crit_fail(crit_fail_effect, mana_cost)
    } else {
        CritOutcome::None
    };

    if crit.skips_primary() {
        return AbilityOutcome {
            affected: Vec::new(),
            primary: OutcomePrimary::None,
            statuses: Vec::new(),
            breakdown: String::new(),
            crit,
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
                EffectDef::Summon { template_id, max_active } => OutcomePrimary::Summon {
                    template: template_id.clone(),
                    max_active: *max_active,
                },
                // `None` (pure-status ability) both land here.
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
        crit,
    }
}

/// Map a path's `CritFailEffect` to the concrete `CritOutcome` variant that
/// backends consume. `ManaOverload` only applies when there's a mana cost to
/// double — otherwise it degrades to a plain `Miss`. Strings come from the
/// live pipeline's log copy so both backends render identical text.
fn map_crit_fail(effect: &CritFailEffect, mana_cost: i32) -> CritOutcome {
    match effect {
        CritFailEffect::Miss => CritOutcome::Miss,
        CritFailEffect::ManaOverload => {
            if mana_cost > 0 {
                CritOutcome::ManaOverload
            } else {
                CritOutcome::Miss
            }
        }
        CritFailEffect::BrokenFaith => CritOutcome::SelfStatus {
            status: StatusId::from("broken_faith"),
            duration_rounds: 1,
            log_description: "Сломленная вера — магия заблокирована на 1 ход".into(),
        },
        CritFailEffect::CircuitBreach => {
            let amount = (mana_cost + 1) / 2;
            CritOutcome::SelfDamage {
                amount,
                damage_breakdown: format!("разгерметизация: {mana_cost}/2={amount}"),
                log_description: format!("Разгерметизация контура — {amount} урона себе"),
            }
        }
        CritFailEffect::Exhaustion => CritOutcome::SelfStatus {
            status: StatusId::from("exhaustion"),
            duration_rounds: 2,
            log_description: "Телесный откат — истощение на 2 хода".into(),
        },
        CritFailEffect::PactControl => CritOutcome::SelfStatus {
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
    use combat_engine::AbilityId;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid entity id")
    }

    /// `DiceSource` double that returns a fixed dice roll. Crit-fail used to
    /// be a method on the trait; it now lives at the call site, so tests
    /// just pass `crit_failed: bool` straight to `compute_ability_outcome`.
    struct MockDice {
        roll: i32,
    }
    impl DiceSource for MockDice {
        fn roll_dice(&mut self, _expr: &DiceExpr, _dis: bool) -> (i32, String) {
            (self.roll, format!("mock({})", self.roll))
        }
    }

    fn ctx() -> CasterContext {
        CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None }
    }

    fn damage_ability(mana_cost: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("strike"),
            name: "Strike".into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
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
                key: None,
            },
        }
    }

    #[test]
    fn crit_fail_miss_variant_skips_primary() {
        let def = damage_ability(0);
        let mut dice = MockDice { roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, true, &CritFailEffect::Miss, &mut dice,
        );
        assert!(matches!(outcome.crit, CritOutcome::Miss));
        assert!(outcome.crit.skips_primary());
        assert!(!outcome.crit.is_mana_overload());
        assert!(outcome.affected.is_empty());
        assert!(matches!(outcome.primary, OutcomePrimary::None));
    }

    #[test]
    fn crit_fail_broken_faith_produces_self_status() {
        let def = damage_ability(0);
        let mut dice = MockDice { roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, true, &CritFailEffect::BrokenFaith, &mut dice,
        );
        match outcome.crit {
            CritOutcome::SelfStatus { status, duration_rounds, .. } => {
                assert_eq!(status.0, "broken_faith");
                assert_eq!(duration_rounds, 1);
            }
            other => panic!("expected SelfStatus, got {other:?}"),
        }
    }

    #[test]
    fn crit_fail_circuit_breach_self_damage_uses_mana_cost() {
        // mana_cost=5 → self_damage = (5+1)/2 = 3.
        let def = damage_ability(5);
        let mut dice = MockDice { roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, true, &CritFailEffect::CircuitBreach, &mut dice,
        );
        match outcome.crit {
            CritOutcome::SelfDamage { amount, .. } => assert_eq!(amount, 3),
            other => panic!("expected SelfDamage, got {other:?}"),
        }
    }

    #[test]
    fn crit_fail_mana_overload_with_mana_cost_fires_effects_and_flags_overload() {
        let def = damage_ability(5);
        let mut dice = MockDice { roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, true, &CritFailEffect::ManaOverload, &mut dice,
        );
        assert!(matches!(outcome.crit, CritOutcome::ManaOverload));
        assert!(outcome.crit.is_mana_overload());
        assert!(!outcome.crit.skips_primary(), "overload keeps the primary payload");
        assert!(matches!(outcome.primary, OutcomePrimary::Damage { .. }));
    }

    #[test]
    fn crit_fail_mana_overload_without_mana_cost_falls_back_to_miss() {
        // No mana cost → overload doesn't apply → treat as plain miss.
        let def = damage_ability(0);
        let mut dice = MockDice { roll: 6 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, true, &CritFailEffect::ManaOverload, &mut dice,
        );
        assert!(matches!(outcome.crit, CritOutcome::Miss));
        assert!(!outcome.crit.is_mana_overload());
        assert!(matches!(outcome.primary, OutcomePrimary::None));
    }

    #[test]
    fn no_crit_fail_fires_primary_effect() {
        let def = damage_ability(0);
        let mut dice = MockDice { roll: 5 };
        let outcome = compute_ability_outcome(
            ent(1), &def, vec![ent(2)], &ctx(),
            false, false, &CritFailEffect::Miss, &mut dice,
        );
        assert!(matches!(outcome.crit, CritOutcome::None));
        assert!(!outcome.crit.skips_primary());
        assert!(!outcome.crit.is_mana_overload());
        match outcome.primary {
            OutcomePrimary::Damage { raw, .. } => assert_eq!(raw, 5),
            _ => panic!("expected Damage"),
        }
        assert_eq!(outcome.affected, vec![ent(2)]);
    }
}
