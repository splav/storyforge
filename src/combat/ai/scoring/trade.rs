//! Trade economics — actor-agnostic unit valuation: [`unit_value`] and its
//! offense / heal / cc per-round projections.
//!
//! # Design invariants
//!
//! 1. **Actor-agnostic.** `unit_value(u)` depends only on `u` and static
//!    content — self, ally and enemy evaluate to the same scalar. This is what
//!    makes `trade_delta` meaningful as a subtraction across sides.
//! 2. **HP-equivalent.** Every channel is normalised to "HP per round", so
//!    contributions add and `value × lifetime` stays on an HP scale.
//! 3. **No internal floor.** A valueless unit returns `0.0`, so summing trash
//!    kills can't silently inflate the delta. [`UNIT_VALUE_FLOOR`] guards only
//!    the `tanh(delta / unit_value(self))` denominator, at that call site.
//!
//! # Intentional omissions
//!
//! - [`lifetime_rounds`] is a constant: tanks gain no value from durability.
//! - Taunt / `forces_targeting` redirect is unpriced — pure tanks score near
//!   the floor, matching `role_value` (Tank 0.3 < Support 1.0).
//! - [`cc_projection`] proxies "peer DPR denied" with `u.threat`; a per-snapshot
//!   average would couple `unit_value` to battle state.

use crate::combat::ai::plan::TurnPlan;
use crate::combat::ai::scoring::horizon::expected_aoo_damage;
use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitView};
use crate::combat::ai::world::tags::AiTags;
use crate::content::abilities::{EffectCalcExt, StatusOn, TargetType};
use crate::content::content_view::ActiveContentData;

/// Expected remaining acting rounds for any unit. MVP2 constant; Phase
/// 2c will replace with `clamp(eff_hp / incoming_dpr, 0.5, 3.0)` once
/// an actor-agnostic `incoming_dpr(u)` proxy is vetted against replay.
const FIXED_LIFETIME_ROUNDS: f32 = 2.0;

/// Lower bound applied **at the denominator call-site** for the Phase 3
/// normaliser `tanh(delta / max(unit_value(self), ε))`. Not applied
/// inside `unit_value` itself — that would silently inflate `trade_delta`
/// when a plan mass-kills trash.
pub const UNIT_VALUE_FLOOR: f32 = 1.0;

/// Amplitude of the plan-level trade modifier. Output of [`trade_score`]
/// lives in `[-TRADE_WEIGHT, +TRADE_WEIGHT]` after the tanh squash.
///
/// Conservative 0.5: the modifier is outside role-composition and applied
/// globally — a "loud" signal. Raise only on replay evidence.
pub const TRADE_WEIGHT: f32 = 0.5;

/// HP-equivalent actor-agnostic value of `u`. See module docs for the
/// contract; the formula is
///
/// ```text
/// unit_value(u) = lifetime_rounds(u) × (offense + heal + cc)
///              + objective_bonus(u)
/// ```
///
/// `objective_bonus` adds `tuning.thresholds.objective_value_bonus` for an
/// `AiTags::OPPONENT_OBJECTIVE` unit (opponent has a `KeepAlive` on it): a
/// permanently-stunned NPC scores 0 otherwise, so AI would ignore it even
/// though killing it loses its side the fight. Returns `0.0`, no floor (see
/// [`UNIT_VALUE_FLOOR`]).
///
/// Consumers (Phase 2): plan-level `trade_delta` sums `unit_value` over
/// killed enemies / lost allies and subtracts `unit_value(self)` when
/// the plan is self-lethal.
pub fn unit_value(u: UnitView<'_>, content: &ActiveContentData) -> f32 {
    if !u.is_alive() {
        return 0.0;
    }
    let life = lifetime_rounds(u);
    let contrib = offense_projection(u) + heal_projection(u, content) + cc_projection(u, content);
    let base = (life * contrib).max(0.0);
    let objective_bonus = if u.cache.tags.contains(AiTags::OPPONENT_OBJECTIVE) {
        content.ai_tuning.thresholds.objective_value_bonus
    } else {
        0.0
    };
    base + objective_bonus
}

/// Expected acting rounds remaining. See [`FIXED_LIFETIME_ROUNDS`].
fn lifetime_rounds(_u: UnitView<'_>) -> f32 {
    FIXED_LIFETIME_ROUNDS
}

/// HP/round damage output. DPR-correct via `damage_horizon`, falls back
/// to `threat` when the horizon is empty (legacy logs / partial fixtures).
fn offense_projection(u: UnitView<'_>) -> f32 {
    if u.cache.damage_horizon.is_empty() {
        u.cache.threat.max(0.0)
    } else {
        let n = u.cache.damage_horizon.len() as f32;
        (u.cache.damage_horizon.iter().sum::<f32>() / n.max(1.0)).max(0.0)
    }
}

/// HP/round healing output. **Best single legal heal**, no `× max_ap`
/// scaling.
///
/// Max `expected` over `SingleAlly + Heal` abilities, evaluated against
/// `u.caster_ctx`. Multi-cast scaling is omitted deliberately: over-counting
/// made heavy casters dominate trades beyond their real in-game leverage.
fn heal_projection(u: UnitView<'_>, content: &ActiveContentData) -> f32 {
    u.cache
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .filter(|def| matches!(def.target_type, TargetType::SingleAlly))
        .filter_map(|def| {
            let calc = def.effect.calc(&u.cache.caster_ctx, def.engine.power())?;
            if !calc.is_heal {
                return None;
            }
            Some(calc.expected().max(0.0))
        })
        .fold(0.0f32, f32::max)
}

/// HP/round CC-denial output. **Best single legal CC**, no `× max_ap`
/// scaling.
///
/// Per ability with a `skips_turn` target status: `Σ duration_rounds ×
/// peer_dpr`. `peer_dpr = u.threat` is the actor-agnostic proxy for the
/// average hit the stun prevents; `MySelf` applications are self-buffs, not
/// denial. Multi-cast omitted — same status doesn't stack, and counting
/// unstunned targets would need a snapshot (breaks actor-agnostic).
fn cc_projection(u: UnitView<'_>, content: &ActiveContentData) -> f32 {
    let peer_dpr = u.cache.threat.max(0.0);
    if peer_dpr <= 0.0 {
        return 0.0;
    }
    u.cache
        .abilities
        .iter()
        .filter_map(|id| content.abilities.get(id))
        .map(|def| {
            def.statuses
                .iter()
                .filter(|sa| matches!(sa.on, StatusOn::Target))
                .filter_map(|sa| {
                    let sd = content.statuses.get(&sa.status)?;
                    if !sd.skips_turn {
                        return None;
                    }
                    Some(sa.duration_rounds as f32 * peer_dpr)
                })
                .sum::<f32>()
        })
        .fold(0.0f32, f32::max)
}

// ── Plan-level trade delta ───────────────────────────────────────────────

/// Decomposition of a plan's trade-economy outcome. Every field is in the
/// HP-equivalent scale produced by [`unit_value`], so they sum / subtract
/// directly. `delta` is the headline scalar consumed by Phase 3; the other
/// fields are carried so the log writer can explain a score without recomputing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TradeBreakdown {
    /// Sum of pre-death `unit_value` for enemies the plan kills.
    pub killed_value: f32,
    /// Sum of pre-death `unit_value` for allies the plan kills
    /// (friendly fire). Excludes the actor — their self-loss lands in
    /// [`Self::self_lost`] to avoid double counting.
    pub lost_value: f32,
    /// `unit_value(active)` when the plan is self-lethal AND the actor
    /// is not already in a step's killed list. Zero otherwise.
    pub self_lost: f32,
    /// True if the plan either moves the actor through a lethal AoO
    /// or self-AoEs the actor into a killed-list entry.
    pub self_lethal: bool,
    /// `killed_value − lost_value − self_lost`. The signed headline
    /// number scored by Phase 3.
    pub delta: f32,
}

/// Compute the trade-economy breakdown for a plan.
///
/// **Commit-prefix only** (`plan.committed_step_count()`): tail-step kills are
/// lookahead the next tick re-plans from scratch, so crediting them here would
/// give undiscounted credit for hypothetical futures.
///
/// Victims are valued from the *pre-step* snapshot, so a unit sim records as
/// killed still carries its alive `unit_value`. Enemy kills → `killed_value`,
/// ally/self kills → `lost_value`.
///
/// Self-lethal via movement AoO uses [`expected_aoo_damage`]. The risky move is
/// always step 0 in a valid prefix (`[]`/`[Cast]`/`[Move]`/`[Move, Cast]`), so
/// `aoo_dmg >= active.hp` against plan-start HP is exact — no self-heal runs
/// first. Counted into `self_lost` only if the actor isn't already a sim kill
/// (else `lost_value` already charged it).
pub fn trade_delta(
    plan: &TurnPlan,
    active: UnitView<'_>,
    initial_snap: &BattleSnapshot,
    content: &ActiveContentData,
) -> TradeBreakdown {
    let prefix_len = plan.committed_step_count();
    let mut killed_value = 0.0f32;
    let mut lost_value = 0.0f32;
    let mut self_in_killed = false;

    // Only the committed prefix contributes (tail steps are lookahead);
    // `outcomes.len()` can be shorter than `steps.len()` for deserialized
    // plans, so clamp at both ends.
    let scan_len = prefix_len.min(plan.outcomes.len());
    for (k, outcome) in plan.outcomes.iter().take(scan_len).enumerate() {
        if outcome.killed.is_empty() {
            continue;
        }
        let pre = plan.pre_step_snapshot(k, initial_snap);
        for &e in &outcome.killed {
            let Some(victim) = pre.unit(e) else { continue };
            let v = unit_value(victim, content);
            if victim.entity() == active.entity() {
                self_in_killed = true;
                lost_value += v;
            } else if victim.team == active.team {
                lost_value += v;
            } else {
                killed_value += v;
            }
        }
    }

    // Self-lethal via AoO on movement. In a valid commit prefix the only Move
    // firing this tick is step 0; prefixes without a Move scan no transition
    // and return 0 — gated by `prefix_is_move_shaped`.
    let enemies: Vec<UnitView<'_>> = initial_snap.enemies_of(active.team).collect();
    let aoo_dmg = if prefix_is_move_shaped(plan, prefix_len) {
        expected_aoo_damage(active, plan, &enemies)
    } else {
        0.0
    };
    let self_lethal_aoo = aoo_dmg >= active.hp() as f32 && active.hp() > 0;

    let self_lost = if self_in_killed {
        0.0
    } else if self_lethal_aoo {
        unit_value(active, content)
    } else {
        0.0
    };

    let self_lethal = self_in_killed || self_lethal_aoo;
    let delta = killed_value - lost_value - self_lost;

    TradeBreakdown {
        killed_value,
        lost_value,
        self_lost,
        self_lethal,
        delta,
    }
}

/// Does the committed prefix contain a Move step? Cheap predicate over
/// the first `prefix_len` steps; used to gate the AoO scan so a
/// commit-prefix of `[Cast]` doesn't borrow AoO risk from a lookahead
/// move in step 2.
fn prefix_is_move_shaped(plan: &TurnPlan, prefix_len: usize) -> bool {
    plan.steps
        .iter()
        .take(prefix_len)
        .any(|s| matches!(s, crate::combat::ai::plan::PlanStep::Move { .. }))
}

/// Post-normalisation scoring contribution of a [`TradeBreakdown`]:
///
/// ```text
/// tanh(delta / max(actor_value, UNIT_VALUE_FLOOR)) × TRADE_WEIGHT
/// ```
///
/// Single source of truth for `modifiers::trade_bonus` and the log writer,
/// so the JSONL trade column reconciles with what ranking used.
pub fn trade_score(br: &TradeBreakdown, actor_value: f32) -> f32 {
    let denom = actor_value.max(UNIT_VALUE_FLOOR);
    (br.delta / denom).tanh() * TRADE_WEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::config::role::AxisProfile;
    use crate::combat::ai::test_helpers::snapshot_from;
    use crate::combat::ai::test_helpers::UnitBuilder;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, ResourceCost, StatusApplication,
    };
    use crate::content::statuses::StatusDef;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use combat_engine::{AbilityId, DiceExpr, ResourceKind, StatusId};
    use std::collections::HashMap;

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Minimal `ActiveContentData` with only the abilities/statuses each case
    // exercises, so a regression pins one formula branch, not global content.
    fn content() -> ActiveContentData {
        ActiveContentData {
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
            ..ActiveContentData::default()
        }
    }

    fn heal_ability(id: &str, cost_ap: i32, heal_dice: DiceExpr) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleAlly,
                range: AbilityRange { min: 0, max: 3 },
                effect: EffectDef::Heal { dice: heal_dice },
                costs: Vec::new(),
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: Vec::new(),
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        }
    }

    fn stun_ability(id: &str, cost_ap: i32, duration: u32, status_id: &str) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.into(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: TargetType::SingleEnemy,
                range: AbilityRange { min: 0, max: 1 },
                effect: EffectDef::None,
                costs: vec![ResourceCost {
                    resource: ResourceKind::Rage,
                    amount: 0,
                }],
                cost_ap,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![StatusApplication {
                    status: StatusId::from(status_id),
                    duration_rounds: duration,
                    on: StatusOn::Target,
                }],
                key: None,
                requires_los: false,
                passive: vec![],
                requires_tags: Default::default(),
                excludes_tags: Default::default(),
                power: None,
            },
        }
    }

    fn stun_status(id: &str) -> StatusDef {
        StatusDef {
            id: StatusId::from(id),
            name: id.into(),
            dot_dice: None,
            ai_controlled: false,
            buff_class: None,
            engine: combat_engine::StatusDef {
                bonuses: combat_engine::StatusBonuses::default(),
                skips_turn: true,
                forces_targeting: false,
                blocks_mana_abilities: false,
                hp_percent_dot: 0,
                heal_per_tick: 0,
                causes_disadvantage: false,
                ..Default::default()
            },
        }
    }

    // ── unit_value ──────────────────────────────────────────────────────────

    /// Dead unit returns zero — the Phase 3 normaliser applies its own
    /// floor at the denominator. Summing zero-valued trash kills must
    /// therefore not silently inflate `trade_delta`.
    #[test]
    fn dead_unit_returns_zero() {
        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(0)
            .build();
        let c = content();
        let s = snapshot_from(vec![u.clone()], 1);
        let v = s.unit(u.entity).unwrap();
        assert_eq!(unit_value(v, &c), 0.0);
    }

    /// Alive unit with no kit and zero threat → zero value. Rationale same
    /// as dead-unit: no hidden floor that mass-kill accounting can exploit.
    #[test]
    fn empty_kit_zero_threat_returns_zero() {
        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(0.0)
            .build();
        let c = content();
        let s = snapshot_from(vec![u.clone()], 1);
        let v = s.unit(u.entity).unwrap();
        assert_eq!(unit_value(v, &c), 0.0);
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
            .role(AxisProfile {
                tank: 0.5,
                melee: 0.5,
                ..Default::default()
            })
            .threat(5.0)
            .ap(2)
            .build();

        let healer = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .role(AxisProfile {
                support: 1.0,
                ..Default::default()
            })
            .threat(5.0)
            .ap(2)
            .abilities(vec![heal.id.clone()])
            .build();

        let s = snapshot_from(vec![bruiser.clone(), healer.clone()], 1);
        let vb = unit_value(s.unit(bruiser.entity).unwrap(), &c);
        let vh = unit_value(s.unit(healer.entity).unwrap(), &c);
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
            .damage_horizon(vec![6.0, 6.0, 6.0, 6.0, 6.0])
            .build();

        let burst = UnitBuilder::new(2, Team::Enemy, hex_from_offset(0, 0))
            .threat(6.0)
            .damage_horizon(vec![6.0, 6.0, 0.0, 0.0, 0.0])
            .build();

        let s = snapshot_from(vec![sustained.clone(), burst.clone()], 1);
        assert!(
            unit_value(s.unit(sustained.entity).unwrap(), &c)
                > unit_value(s.unit(burst.entity).unwrap(), &c),
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
        c.statuses
            .insert(StatusId::from(stun_id), stun_status(stun_id));
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

        let s = snapshot_from(vec![with_cc.clone(), no_cc.clone()], 1);
        assert!(
            unit_value(s.unit(with_cc.entity).unwrap(), &c)
                > unit_value(s.unit(no_cc.entity).unwrap(), &c),
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
        c.statuses
            .insert(StatusId::from(buff_id), stun_status(buff_id));
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

        let s = snapshot_from(vec![u.clone(), peer.clone()], 1);
        assert_eq!(
            unit_value(s.unit(u.entity).unwrap(), &c),
            unit_value(s.unit(peer.entity).unwrap(), &c),
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
        c.statuses
            .insert(StatusId::from(stun_id), stun_status(stun_id));
        let ab = stun_ability("hammer", 1, 2, stun_id);
        c.abilities.insert(ab.id.clone(), ab.clone());

        let u = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(0.0)
            .abilities(vec![ab.id.clone()])
            .build();

        // threat=0 ⇒ offense also 0 ⇒ pure-CC unit values at zero.
        let s = snapshot_from(vec![u.clone()], 1);
        assert_eq!(unit_value(s.unit(u.entity).unwrap(), &c), 0.0);
    }

    // ── trade_delta ─────────────────────────────────────────────────────────
    //
    // Plans leave `sim_snapshots` empty — `pre_step_snapshot` then falls back to
    // `initial_snap`, the same path deserialized plans use. No sim wiring needed.

    use crate::combat::ai::plan::{PlanStep, StepOutcome, TurnPlan};
    use crate::combat::ai::test_helpers::ent;
    use bevy::prelude::Entity;

    /// Plan with a single `Move` step and a prescribed `killed` outcome.
    /// AoO-relevant: the Move step is what `expected_aoo_damage` scans.
    fn move_plan_killing(path: Vec<crate::game::hex::Hex>, killed: Vec<Entity>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![StepOutcome {
                killed,
                ..Default::default()
            }],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    /// Stationary cast plan — a commit-prefix-valid `[Cast]` with a
    /// fabricated outcome vector. The Cast step is a no-op marker; what
    /// we're pinning is trade_delta's victim classification, not the
    /// cast resolution.
    fn static_kill_plan(pos: crate::game::hex::Hex, killed: Vec<Entity>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("_fixture"),
                target: ent(0xDEAD),
                target_pos: pos,
            }],
            final_pos: pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![StepOutcome {
                killed,
                ..Default::default()
            }],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    /// Killing an enemy yields a positive delta equal to their `unit_value`.
    /// No allies in the plan → `lost_value = 0`; no movement → `self_lost = 0`.
    #[test]
    fn enemy_kill_credits_killed_value() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let victim = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .role(AxisProfile {
                support: 1.0,
                ..Default::default()
            })
            .threat(4.0)
            .build();
        let snap = snapshot_from(vec![actor.clone(), victim.clone()], 1);
        let plan = static_kill_plan(actor.pos, vec![victim.entity]);
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        let expected = unit_value(snap.unit(victim.entity).unwrap(), &c);

        assert_eq!(br.killed_value, expected);
        assert_eq!(br.lost_value, 0.0);
        assert_eq!(br.self_lost, 0.0);
        assert!(!br.self_lethal);
        assert_eq!(br.delta, expected);
    }

    /// AoE that kills a weak enemy AND a valuable ally should produce a
    /// negative delta dominated by the ally loss. Pins the friendly-fire
    /// accounting path (ally → `lost_value`, not `killed_value`).
    #[test]
    fn aoe_killing_ally_and_rat_nets_negative() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let rat = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .threat(1.0)
            .build();
        let ally_controller = UnitBuilder::new(3, Team::Enemy, hex_from_offset(1, 0))
            .role(AxisProfile {
                ranged: 0.7,
                control: 0.3,
                ..Default::default()
            })
            .threat(8.0)
            .build();
        let snap = snapshot_from(vec![actor.clone(), rat.clone(), ally_controller.clone()], 1);
        let plan = static_kill_plan(actor.pos, vec![rat.entity, ally_controller.entity]);
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        assert!(br.killed_value > 0.0);
        assert!(br.lost_value > br.killed_value, "ally value must dominate");
        assert!(br.delta < 0.0);
    }

    /// Self-lethal move with no kill → `delta = −unit_value(self)`.
    /// Pins the `expected_aoo_damage ≥ hp` path and the guard that
    /// `lost_value` stays zero when the actor isn't in a killed list.
    #[test]
    fn self_lethal_move_no_kill_equals_minus_self() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .threat(5.0)
            .build();
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let snap = snapshot_from(vec![actor.clone(), enemy], 1);
        let plan = move_plan_killing(vec![hex_from_offset(-1, 0)], Vec::new());
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        let actor_view = snap.unit(actor.entity).unwrap();
        assert!(br.self_lethal);
        assert_eq!(br.killed_value, 0.0);
        assert_eq!(br.lost_value, 0.0);
        assert_eq!(br.self_lost, unit_value(actor_view, &c));
        assert_eq!(br.delta, -unit_value(actor_view, &c));
    }

    /// Self-AoE putting the actor in the killed list must charge the
    /// loss exactly once — via `lost_value`, not `self_lost`. Pins the
    /// `self_in_killed` guard that prevents double counting when an
    /// already-dead actor would also be AoO-lethal.
    #[test]
    fn self_in_killed_list_suppresses_self_lost_path() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .hp(3)
            .threat(5.0)
            .build();
        // Adjacent enemy with a lethal AoO — would normally trigger the
        // AoO-lethal path AS WELL AS the sim-kill path, double-charging.
        let enemy = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .aoo(5.0, 1)
            .build();
        let snap = snapshot_from(vec![actor.clone(), enemy], 1);
        // Plan moves away (triggers AoO by the provoker above) AND the sim
        // outcome declares the actor dead. Under double-counting we'd lose
        // 2×unit_value(actor); the guard caps it at 1×.
        let plan = move_plan_killing(vec![hex_from_offset(-1, 0)], vec![actor.entity]);
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        let actor_view = snap.unit(actor.entity).unwrap();
        assert!(br.self_lethal);
        assert_eq!(br.self_lost, 0.0, "must not double-charge");
        assert_eq!(br.lost_value, unit_value(actor_view, &c));
        assert_eq!(br.delta, -unit_value(actor_view, &c));
    }

    /// Empty plan (no steps, no outcomes) is neutral: zero deltas, not
    /// self-lethal. The baseline case every other branch contrasts against.
    #[test]
    fn empty_plan_is_neutral() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let plan = TurnPlan {
            steps: Vec::new(),
            final_pos: actor.pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        assert_eq!(br, TradeBreakdown::default());
    }

    /// Unknown victim entity (not in the snapshot) must be silently
    /// skipped — a robustness guard for deserialized plans or mid-sim
    /// state drift. Pins the `pre.unit(e)` `Some`-only accumulation.
    #[test]
    fn unknown_victim_is_skipped() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let snap = snapshot_from(vec![actor.clone()], 1);
        let plan = static_kill_plan(actor.pos, vec![ent(99)]);
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        assert_eq!(br, TradeBreakdown::default());
    }

    /// Multi-step plan whose commit prefix is `[Cast]` — step 1's Cast
    /// fires, step 2's kill is lookahead that the next tick will replan.
    /// Pins the architectural invariant that trade_delta is
    /// commit-prefix-only: un-discounted credit for step-2 kills would
    /// double-count what should live under the existing step-discount
    /// regime.
    #[test]
    fn tail_step_kill_is_not_credited() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let tail_victim = UnitBuilder::new(2, Team::Player, hex_from_offset(3, 0))
            .role(AxisProfile {
                support: 1.0,
                ..Default::default()
            })
            .build();
        let snap = snapshot_from(vec![actor.clone(), tail_victim.clone()], 1);

        // Prefix `[Cast]`; step-2 Cast has the kill. Step-2 is lookahead
        // and must not contribute to trade_delta under any prefix.
        let plan = TurnPlan {
            steps: vec![
                PlanStep::Cast {
                    ability: AbilityId::from("_first"),
                    target: ent(0xAAAA),
                    target_pos: hex_from_offset(1, 0),
                },
                PlanStep::Cast {
                    ability: AbilityId::from("_tail"),
                    target: tail_victim.entity,
                    target_pos: tail_victim.pos,
                },
            ],
            final_pos: actor.pos,
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![
                StepOutcome::default(),
                StepOutcome {
                    killed: vec![tail_victim.entity],
                    ..Default::default()
                },
            ],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        assert_eq!(br.killed_value, 0.0, "step-2 kill must not be credited");
        assert_eq!(br, TradeBreakdown::default());
    }

    /// Bundled `[Move, Cast]` prefix: BOTH steps count toward
    /// trade_delta because both fire this tick. Pins the upper bound of
    /// the commit-prefix scan.
    #[test]
    fn move_then_cast_prefix_counts_both_steps() {
        let actor = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(5.0)
            .build();
        let victim = UnitBuilder::new(2, Team::Player, hex_from_offset(2, 0))
            .role(AxisProfile {
                support: 1.0,
                ..Default::default()
            })
            .threat(4.0)
            .build();
        let snap = snapshot_from(vec![actor.clone(), victim.clone()], 1);

        let plan = TurnPlan {
            steps: vec![
                PlanStep::Move {
                    path: vec![hex_from_offset(1, 0)],
                },
                PlanStep::Cast {
                    ability: AbilityId::from("_cast"),
                    target: victim.entity,
                    target_pos: victim.pos,
                },
            ],
            final_pos: hex_from_offset(1, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: vec![
                StepOutcome::default(),
                StepOutcome {
                    killed: vec![victim.entity],
                    ..Default::default()
                },
            ],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        let c = content();

        let br = trade_delta(&plan, snap.unit(actor.entity).unwrap(), &snap, &c);
        assert_eq!(
            br.killed_value,
            unit_value(snap.unit(victim.entity).unwrap(), &c)
        );
        assert!(!br.self_lethal);
    }

    // Keep `ent` live even if test churn drops it from the test list.
    #[test]
    fn _ent_helper_available() {
        let _ = ent(42);
    }

    // ── OPPONENT_OBJECTIVE bonus ──────────────────────────────────────────────

    /// A unit with `AiTags::OPPONENT_OBJECTIVE` but no offensive kit must have
    /// `unit_value` ≥ that of an active melee hero. This is the central knob:
    /// the bonus must be large enough to outweigh typical hero values so AI
    /// prioritizes killing the KeepAlive target in trade economy.
    #[test]
    fn objective_tag_outranks_active_hero_with_zero_kit() {
        let c = content();

        // Active melee hero: threat=8, no mana/rage. unit_value ≈ 2.0 × 8 = 16.
        let hero = UnitBuilder::new(1, Team::Enemy, hex_from_offset(0, 0))
            .threat(8.0)
            .build();

        // Stunned NPC: no abilities, threat=0 — kit value is 0.
        // Gets OPPONENT_OBJECTIVE tag (normally set from KeepAliveTarget component).
        let npc = UnitBuilder::new(2, Team::Player, hex_from_offset(1, 0))
            .threat(0.0)
            .tags(AiTags::OPPONENT_OBJECTIVE)
            .build();

        let s = snapshot_from(vec![hero.clone(), npc.clone()], 1);
        let vh = unit_value(s.unit(hero.entity).unwrap(), &c);
        let vn = unit_value(s.unit(npc.entity).unwrap(), &c);

        assert!(
            vn >= vh,
            "OPPONENT_OBJECTIVE NPC value {vn} should be >= active hero value {vh}",
        );
        // Sanity: hero is worth something (formula hasn't broken).
        assert!(vh > 0.0, "active hero must have non-zero unit_value");
        // Sanity: bonus is actually applied (NPC kit contributes 0).
        assert!(
            vn >= c.ai_tuning.thresholds.objective_value_bonus,
            "NPC value {vn} must be at least objective_value_bonus {}",
            c.ai_tuning.thresholds.objective_value_bonus,
        );
    }
}
