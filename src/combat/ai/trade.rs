//! Trade economics — actor-agnostic unit valuation.
//!
//! MVP2 Phase 1: introduces [`unit_value`] and its three per-round
//! contribution projections (offense / heal / cc). No plan-level
//! `trade_delta` or scorer integration yet; those land in phases 2 and 3.
//!
//! # Design invariants
//!
//! 1. **Actor-agnostic.** `unit_value(u)` depends only on `u` and static
//!    content. No inspecting-actor parameter, no proximity, no relative
//!    threat — self, ally, and enemy all evaluate to the same scalar for
//!    the same unit. This is the property that makes the Phase 2
//!    `trade_delta` meaningful as a subtraction across sides.
//! 2. **HP-equivalent units.** Every channel is normalised to "HP per
//!    round" (damage inflicted / prevented / denied), so contributions
//!    can be added and the final value multiplied by expected lifetime
//!    gives an HP-scale scalar.
//! 3. **Floor at [`UNIT_VALUE_FLOOR`].** Dead / mute / inert units still
//!    return a strictly positive value so the Phase 3 normaliser
//!    `tanh(delta / unit_value(self))` never divides by zero.
//!
//! # Known limitations (MVP2, tracked for Phase 2c)
//!
//! - [`lifetime_rounds`] is a fixed constant, not a dynamic
//!   `clamp(eff_hp / incoming_dpr, …)`. Tanks derive no extra value from
//!   their durability beyond what the constant allocates.
//! - Taunt / `forces_targeting` redirect value is not priced. Pure
//!   tanks score near the floor, consistent with the existing
//!   `role_value` hierarchy (Tank 0.3 < Support 1.0).
//! - [`cc_projection`] uses `u.threat` as the "peer DPR denied by a
//!   stun" proxy — coarse but actor-agnostic. A dynamic per-snapshot
//!   average would couple `unit_value` to battle state.

use crate::combat::ai::scoring::horizon_avg;
use crate::combat::ai::snapshot::UnitSnapshot;
use crate::content::abilities::{StatusOn, TargetType};
use crate::content::content_view::ContentView;

/// Expected remaining acting rounds for any unit. MVP2 constant; Phase
/// 2c will replace with `clamp(eff_hp / incoming_dpr, 0.5, 3.0)` once
/// an actor-agnostic `incoming_dpr(u)` proxy is vetted against replay.
const FIXED_LIFETIME_ROUNDS: f32 = 2.0;

/// Lower bound on `unit_value`. Keeps the Phase 3 normaliser
/// `tanh(delta / max(unit_value(self), ε))` well-defined when the actor
/// has no contribution channels (dead / empty kit).
const UNIT_VALUE_FLOOR: f32 = 1.0;

/// HP-equivalent actor-agnostic value of `u`. See module docs for the
/// contract; the formula is
///
/// ```text
/// unit_value(u) = lifetime_rounds(u) × (offense + heal + cc),  floored.
/// ```
///
/// Consumers (Phase 2): plan-level `trade_delta` sums `unit_value` over
/// killed enemies / lost allies and subtracts `unit_value(self)` when
/// the plan is self-lethal.
pub fn unit_value(u: &UnitSnapshot, content: &ContentView) -> f32 {
    if !u.is_alive() {
        return UNIT_VALUE_FLOOR;
    }
    let life = lifetime_rounds(u);
    let contrib =
        offense_projection(u) + heal_projection(u, content) + cc_projection(u, content);
    (life * contrib).max(UNIT_VALUE_FLOOR)
}

/// Expected acting rounds remaining. See [`FIXED_LIFETIME_ROUNDS`].
fn lifetime_rounds(_u: &UnitSnapshot) -> f32 {
    FIXED_LIFETIME_ROUNDS
}

/// HP/round damage output. Reuses `scoring::horizon_avg` — DPR-correct
/// via `damage_horizon`, falls back to `threat` when the horizon is
/// empty (legacy logs / partial fixtures).
fn offense_projection(u: &UnitSnapshot) -> f32 {
    horizon_avg(u).max(0.0)
}

/// HP/round healing output. Best per-AP heal scaled to `max_ap`.
///
/// Scans `u.abilities` for `SingleAlly + Heal`, evaluates each against
/// `u.caster_ctx` (spell power / int mod), picks the ability with the
/// highest `expected / cost_ap`, then multiplies by `max_ap` so a
/// 2-AP actor with a 1-AP heal can fire twice. Returns `0.0` when the
/// unit has no heal kit.
fn heal_projection(u: &UnitSnapshot, content: &ContentView) -> f32 {
    let best_per_ap: f32 = u
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| matches!(def.target_type, TargetType::SingleAlly))
        .filter_map(|def| {
            let calc = def.effect.calc(&u.caster_ctx)?;
            if !calc.is_heal {
                return None;
            }
            let cost_ap = def.cost_ap.max(1) as f32;
            Some(calc.expected().max(0.0) / cost_ap)
        })
        .fold(0.0f32, f32::max);
    best_per_ap * u.max_ap.max(0) as f32
}

/// HP/round CC-denial output. Best per-AP CC ability scaled to `max_ap`.
///
/// For each ability applying at least one `skips_turn` status on the
/// target, the denial value is `Σ duration_rounds × peer_dpr` over the
/// ability's target-side statuses. `peer_dpr = u.threat` is the
/// actor-agnostic proxy for "how hard is the average enemy hit the stun
/// prevents". Non-CC abilities and `MySelf`-only status applications
/// don't contribute.
fn cc_projection(u: &UnitSnapshot, content: &ContentView) -> f32 {
    let peer_dpr = u.threat.max(0.0);
    if peer_dpr <= 0.0 {
        return 0.0;
    }
    let best_per_ap: f32 = u
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter_map(|def| {
            let denial: f32 = def
                .statuses
                .iter()
                .filter(|sa| matches!(sa.on, StatusOn::Target))
                .filter_map(|sa| {
                    let sd = content.statuses.get(&sa.status)?;
                    if !sd.skips_turn {
                        return None;
                    }
                    Some(sa.duration_rounds as f32 * peer_dpr)
                })
                .sum();
            if denial <= 0.0 {
                return None;
            }
            let cost_ap = def.cost_ap.max(1) as f32;
            Some(denial / cost_ap)
        })
        .fold(0.0f32, f32::max);
    best_per_ap * u.max_ap.max(0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::role::AiRole;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost, StatusApplication,
    };
    use crate::content::statuses::StatusDef;
    use crate::core::{AbilityId, DiceExpr, ResourceKind, StatusId};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Tests build a minimal `ContentView` with exactly the abilities / statuses
    // the case exercises. Keeps assertions readable — a regression pins a
    // specific formula branch rather than entangling global test content.
    fn content() -> ContentView {
        ContentView {
            abilities: HashMap::new(),
            keyed_abilities: Vec::new(),
            statuses: HashMap::new(),
            weapons: HashMap::new(),
            armor: HashMap::new(),
            classes: HashMap::new(),
            unit_templates: HashMap::new(),
            races: HashMap::new(),
            factions: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    fn heal_ability(id: &str, cost_ap: i32, heal_dice: DiceExpr) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleAlly,
            range: AbilityRange { min: 0, max: 3 },
            effect: EffectDef::Heal { dice: heal_dice },
            costs: Vec::new(),
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn stun_ability(id: &str, cost_ap: i32, duration: u32, status_id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::None,
            costs: vec![ResourceCost { resource: ResourceKind::Rage, amount: 0 }],
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: vec![StatusApplication {
                status: StatusId::from(status_id),
                duration_rounds: duration,
                on: StatusOn::Target,
            }],
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn stun_status(id: &str) -> StatusDef {
        StatusDef {
            id: StatusId::from(id),
            name: id.into(),
            armor_bonus: 0,
            damage_taken_bonus: 0,
            skips_turn: true,
            forces_targeting: false,
            dot_dice: None,
            blocks_mana_abilities: false,
            speed_bonus: 0,
            hp_percent_dot: 0,
            ai_controlled: false,
            causes_disadvantage: false,
        }
    }

    // ── unit_value ──────────────────────────────────────────────────────────

    /// Dead unit must still return the floor — Phase 3 normaliser depends on
    /// the guarantee `unit_value ≥ ε > 0` to avoid division-by-zero when the
    /// actor's own value is queried mid-plan.
    #[test]
    fn dead_unit_returns_floor() {
        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(0)
            .build();
        let c = content();
        assert_eq!(unit_value(&u, &c), UNIT_VALUE_FLOOR);
    }

    /// Unit with no kit still ≥ floor — inert but alive, we don't want it
    /// zero-priced either (a NPC guard soaking damage still costs *something*).
    #[test]
    fn empty_kit_returns_floor() {
        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(0.0)
            .build();
        let c = content();
        assert_eq!(unit_value(&u, &c), UNIT_VALUE_FLOOR);
    }

    /// A healer with meaningful heal output should out-price a comparable
    /// bruiser with pure melee threat at matching AP. Pins the contribution
    /// ordering Support > Melee that MVP2 must respect.
    #[test]
    fn healer_outranks_bruiser_of_same_threat() {
        let mut c = content();
        let heal = heal_ability("heal", 1, DiceExpr::new(2, 6, 2)); // EV ≈ 9
        c.abilities.insert(heal.id.clone(), heal.clone());

        let bruiser = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .ai_role(AiRole::Bruiser)
            .threat(5.0)
            .ap(2)
            .build();

        let healer = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .ai_role(AiRole::Support)
            .threat(5.0)
            .ap(2)
            .abilities(vec![heal.id.clone()])
            .build();

        let vb = unit_value(&bruiser, &c);
        let vh = unit_value(&healer, &c);
        assert!(
            vh > vb,
            "healer {vh} should outrank bruiser {vb} of matching threat",
        );
    }

    /// A unit with a low peak threat but a flat horizon (sustained fighter)
    /// should out-price one with the same peak that burned its pool (burst).
    /// Pins that offense_projection tracks `horizon_avg`, not peak `threat`.
    #[test]
    fn sustained_offense_outranks_burst_exhausted() {
        let c = content();
        let sustained = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(6.0)
            .build();
        // Same horizon as threat → horizon_avg = 6.0.
        let mut sustained = sustained;
        sustained.damage_horizon = vec![6.0, 6.0, 6.0, 6.0, 6.0];

        let burst = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .threat(6.0)
            .build();
        // Burned pool → horizon_avg = 12/5 = 2.4.
        let mut burst = burst;
        burst.damage_horizon = vec![6.0, 6.0, 0.0, 0.0, 0.0];

        assert!(
            unit_value(&sustained, &c) > unit_value(&burst, &c),
            "sustained fighter must out-price burst caster with same peak",
        );
    }

    /// Controller with a stun ability should out-price a controller without
    /// one, holding everything else constant. Isolates the cc_projection
    /// branch.
    #[test]
    fn cc_kit_adds_value() {
        let mut c = content();
        let stun_id = "stunned";
        c.statuses.insert(StatusId::from(stun_id), stun_status(stun_id));
        let ab = stun_ability("hammer", 1, 2, stun_id);
        c.abilities.insert(ab.id.clone(), ab.clone());

        let with_cc = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .ap(1)
            .abilities(vec![ab.id.clone()])
            .build();
        let no_cc = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .ap(1)
            .build();

        assert!(
            unit_value(&with_cc, &c) > unit_value(&no_cc, &c),
            "CC-capable unit should out-price a peer without CC",
        );
    }

    /// Self-targeted status applications (on=MySelf) must NOT contribute to
    /// cc_projection — those are self-buffs, not opponent denial. Pins the
    /// `StatusOn::Target` filter.
    #[test]
    fn self_buff_status_does_not_count_as_cc() {
        let mut c = content();
        let buff_id = "focused";
        c.statuses.insert(StatusId::from(buff_id), stun_status(buff_id));
        // Same shape as stun_ability but on=MySelf.
        let mut ab = stun_ability("meditate", 1, 2, buff_id);
        ab.statuses[0].on = StatusOn::MySelf;
        c.abilities.insert(ab.id.clone(), ab.clone());

        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .abilities(vec![ab.id.clone()])
            .build();
        // No-CC peer for direct comparison.
        let peer = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();

        assert_eq!(
            unit_value(&u, &c),
            unit_value(&peer, &c),
            "self-buff must not add CC value",
        );
    }

    /// Zero-threat unit has `cc_projection = 0` regardless of kit — peer_dpr
    /// proxy is threat-based, so a threat-less unit denies nothing. Pins the
    /// early-exit guard.
    #[test]
    fn zero_threat_zeroes_cc_channel() {
        let mut c = content();
        let stun_id = "stunned";
        c.statuses.insert(StatusId::from(stun_id), stun_status(stun_id));
        let ab = stun_ability("hammer", 1, 2, stun_id);
        c.abilities.insert(ab.id.clone(), ab.clone());

        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(0.0)
            .abilities(vec![ab.id.clone()])
            .build();

        // threat=0 ⇒ offense also 0 ⇒ pure-CC unit falls to floor.
        assert_eq!(unit_value(&u, &c), UNIT_VALUE_FLOOR);
    }
}
