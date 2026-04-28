//! Plan-level sanity adjustments. Multiplicative penalties for situations the
//! per-factor scoring can't catch — walking through a dangerous corridor with
//! low HP, moving onto a tile with no LoS, centering a self-AoE on yourself,
//! cornering yourself into a 1-neighbour dead-end. Mirrors
//! `utility/sanity.rs` but operates on `TurnPlan` instead of `ActionCandidate`.
//!
//! Applied between `score_plans_with_raw` and `pick_best_plan`: each plan's final score
//! gets multiplied in place by a product of the penalty factors. Floor at
//! `SURVIVAL_FLOOR` keeps even punished plans competitive when all options
//! are bad; retreat lines still beat "rush at 5 HP".

use crate::combat::ai::factors::{aoe_area, PlanFactor, PlanFactorValues};
use crate::combat::ai::planning::adaptation::EvaluationMode;
use crate::combat::ai::planning::scorer::worst_path_danger;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, UnitSnapshot};
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::effects_math::final_damage_f32;
use crate::content::abilities::AoEShape;
use crate::game::hex::{has_los, in_bounds, Hex};
use std::collections::HashSet;

// ── Sanity rule observability ──────────────────────────────────────────────

/// Identifies one sanity rule. One variant per rule in `sanity_adjust_plans`,
/// in the order they fire. Stable codes consumed by offline analyzers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanityRule {
    /// Low-HP actor crossing/resting on dangerous tiles (survival quadratic).
    Survival,
    /// Non-healer abandoning the team's unguarded healer.
    HealerExposure,
    /// Ranged unit ending its turn with no enemy in LoS.
    LosBlindspot,
    /// Final tile has fewer than 2 open neighbours (retreat trap).
    RetreatTrap,
    /// Any Cast step in the plan centers a friendly-fire AoE on the caster.
    SelfAoe,
    /// Move step provokes an opportunity attack (AoO bleed).
    AoOBleed,
    /// Plan repositions to a safer/better tile AND includes a useful cast (synergy bonus).
    SynergyBonus,
}

impl SanityRule {
    /// Short stable code for offline analyzer consumption.
    pub fn code(self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::HealerExposure => "healer_exposure",
            Self::LosBlindspot => "los_blindspot",
            Self::RetreatTrap => "retreat_trap",
            Self::SelfAoe => "self_aoe",
            Self::AoOBleed => "aoo_bleed",
            Self::SynergyBonus => "synergy_bonus",
        }
    }
}

/// Records that one sanity rule fired on one plan and the factor it applied.
/// `multiplier < 1.0` for penalties; `> 1.0` for the synergy bonus.
/// The value is the **clamped** factor that was actually multiplied into the
/// plan score (i.e. post-`max(SURVIVAL_FLOOR)` / post-`max(AOO_RISK_FLOOR)`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SanityHit {
    pub rule: SanityRule,
    pub multiplier: f32,
}


/// Apply sanity multipliers to `scores` in place and return a per-plan
/// breakdown of which rules fired and with what multiplier.
///
/// The outer `Vec` is parallel to `plans`/`scores`. Each inner `Vec` lists
/// the `SanityHit`s for that plan in the order they fired; an empty inner
/// vec means no rules fired for that plan and the score was unchanged.
///
/// **Early-return case (`scores.len() <= 1`):** returns a `Vec` of empty
/// inner vecs sized to `scores.len()` (0 or 1 entries). The single-plan
/// edge case does not run any rule, so the breakdown is empty — but the
/// outer length still matches `scores.len()` so callers can index it safely
/// without a special-case check.
pub fn sanity_adjust_plans(
    scores: &mut [f32],
    plans: &[TurnPlan],
    ctx: &ScoringCtx,
) -> Vec<Vec<SanityHit>> {
    // Pre-allocate one empty inner vec per plan.
    let mut breakdown: Vec<Vec<SanityHit>> = (0..scores.len()).map(|_| Vec::new()).collect();

    if scores.len() <= 1 {
        return breakdown;
    }

    let active = ctx.active;
    let snap = ctx.snap;
    let maps = ctx.maps;
    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    // LoS blockers are LIVE units only. Corpses in the snapshot (dead but
    // still present for replay / death-trigger reasons) do not block sight —
    // line-of-sight is a tactical signal about who-can-see-what, not a
    // tile-occupancy question. Explicit `.is_alive()` keeps the behaviour
    // pinned now that `snap.units` includes dead entries.
    let occupied: HashSet<Hex> = snap
        .units
        .iter()
        .filter(|u| u.is_alive())
        .map(|u| u.pos)
        .collect();
    let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, ctx.world.tuning, maps);
    let current_danger = maps.danger.get(active.pos);

    for (idx, (plan, score)) in plans.iter().zip(scores.iter_mut()).enumerate() {
        if !score.is_finite() {
            continue;
        }
        let mut penalty = 1.0f32;
        let final_pos = plan.final_pos;
        let hits = &mut breakdown[idx];

        // Worst danger the actor touches across moves + resting tile.
        // Shared helper with scorer (`scorer::worst_path_danger`) so the
        // factor and the penalty look at exactly the same signal.
        let max_path_danger = worst_path_danger(plan, maps);

        // 1. Survival: low-HP actor crossing/resting on dangerous tiles.
        // Uses `max_path_danger` rather than just final_pos so "walk through
        // Kael's AoO corridor to land on a safe tile" still eats the penalty.
        let hp_need = ((0.6 - active.hp_pct()) / 0.6).clamp(0.0, 1.0);
        let excess = (max_path_danger - 0.5).max(0.0);
        let surv = ctx.world.tuning.thresholds.low_hp_factor * hp_need * excess * excess;
        if surv > 0.0 {
            let multiplier = (1.0 - surv).max(ctx.world.tuning.thresholds.survival_floor);
            penalty *= multiplier;
            hits.push(SanityHit { rule: SanityRule::Survival, multiplier });
        }

        // 2. Healer exposure: a non-healer abandoning the team's healer.
        if active.role.support < 0.3 {
            for ally in &allies {
                if !ally.tags.contains(AiTags::CAN_HEAL) {
                    continue;
                }
                let was_near = active.pos.unsigned_distance_to(ally.pos) <= 1;
                let will_be_far = final_pos.unsigned_distance_to(ally.pos) > 2;
                if was_near && will_be_far {
                    let other_guard = allies.iter().any(|a| {
                        a.entity != ally.entity && a.pos.unsigned_distance_to(ally.pos) <= 2
                    });
                    if !other_guard {
                        penalty *= 0.5;
                        hits.push(SanityHit { rule: SanityRule::HealerExposure, multiplier: 0.5 });
                    }
                }
            }
        }

        // 3. LOS blindspot: ranged unit ending its turn with no enemy in LOS.
        if active.tags.contains(AiTags::RANGED) && !enemies.is_empty() {
            let can_see_any = enemies.iter().any(|e| {
                has_los(final_pos, e.pos, |mid| {
                    occupied.contains(&mid) && mid != final_pos && mid != e.pos
                })
            });
            if !can_see_any {
                penalty *= 0.3;
                hits.push(SanityHit { rule: SanityRule::LosBlindspot, multiplier: 0.3 });
            }
        }

        // 4. Retreat trap: final tile with fewer than 2 open neighbours
        // (flankable, no room to move next turn).
        let open_neighbors = final_pos
            .all_neighbors()
            .iter()
            .filter(|&&n| in_bounds(n) && !ally_positions.contains(&n))
            .count();
        if open_neighbors < 2 {
            penalty *= 0.5;
            hits.push(SanityHit { rule: SanityRule::RetreatTrap, multiplier: 0.5 });
        }

        // 5. Self-AoE: any Cast step in the plan centers a friendly-fire AoE
        // on a tile that covers the caster's position at that moment.
        if plan_has_self_aoe(plan, ctx) {
            penalty *= 0.5;
            hits.push(SanityHit { rule: SanityRule::SelfAoe, multiplier: 0.5 });
        }

        // 6. AoO bleed: every Move step transition `was_adj && !still_adj`
        // against a melee enemy with reactions provokes an opportunity attack.
        // Sum expected damage per enemy (one AoO per enemy per turn) and
        // apply a multiplicative quadratic penalty with a floor so high-
        // reward plans can still accept the risk.
        //
        // Invariant: **sanity never hard-masks.** The "expected-lethal"
        // case (aoo_dmg ≥ hp) lives in `adaptation::apply_adaptation` —
        // there a plan whose AoO bleed crosses the HP threshold gets its
        // evaluation regime switched to `LastStand` rather than masked
        // out. See `planning/adaptation.rs` for the rationale.
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        if aoo_dmg > 0.0 {
            let ratio = (aoo_dmg / active.hp.max(1) as f32).min(1.0);
            let multiplier = (1.0 - ctx.world.tuning.thresholds.aoo_penalty_k * ratio * ratio).max(ctx.world.tuning.thresholds.aoo_risk_floor);
            penalty *= multiplier;
            hits.push(SanityHit { rule: SanityRule::AoOBleed, multiplier });
        }

        // 7. Synergy bonus: the plan repositions to a safer/better tile AND
        // includes a useful cast. Encourages retreat-and-help combos. Multi-
        // plicative so it doesn't flip sign.
        if final_pos != active.pos {
            let safer_tile = maps.danger.get(final_pos) + 0.05 < current_danger;
            let better_pos = evaluate_position(final_pos, &active.role, ctx.world.tuning, maps) > current_pos_eval;
            if (safer_tile || better_pos) && plan_has_useful_cast(plan, ctx) {
                penalty *= 1.1;
                hits.push(SanityHit { rule: SanityRule::SynergyBonus, multiplier: 1.1 });
            }
        }

        *score *= penalty;
    }

    breakdown
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Sum of expected AoO damage the plan would take across all provoking
/// transitions. For each melee enemy with reactions and a damage estimate,
/// scan the plan's movement path for the first `was_adj && !still_adj`
/// transition (one AoO per enemy per round) and accrue its expected damage
/// against the actor's armor + vulnerability. Returns 0.0 if no provokers
/// are triggered — fast path for typical non-adjacent moves.
/// Visible to the adaptation layer — the `ExpectedSelfLethal` trigger
/// compares this against `active.hp`. Kept in sanity.rs because the
/// non-lethal multiplicative penalty (inside `sanity_adjust_plans`) uses
/// the same number.
pub(crate) fn expected_aoo_damage(
    active: &UnitSnapshot,
    plan: &TurnPlan,
    enemies: &[&UnitSnapshot],
) -> f32 {
    let mut total = 0.0f32;
    let mitigation = (active.armor + active.armor_bonus) as f32;
    let vuln = active.damage_taken_bonus as f32;
    for e in enemies {
        if e.reactions_left <= 0 {
            continue;
        }
        let Some(raw) = e.aoo_expected_damage else { continue };
        // Scan: does the path ever leave adjacency with this enemy?
        let mut prev = active.pos;
        let mut triggered = false;
        for step in &plan.steps {
            let PlanStep::Move { path } = step else { continue };
            for &h in path {
                if prev.unsigned_distance_to(e.pos) == 1
                    && h.unsigned_distance_to(e.pos) != 1
                {
                    triggered = true;
                    break;
                }
                prev = h;
            }
            if triggered {
                break;
            }
        }
        if triggered {
            total += final_damage_f32(raw, mitigation, vuln, /* pierces_armor */ false);
        }
    }
    total
}

fn plan_has_self_aoe(plan: &TurnPlan, ctx: &ScoringCtx) -> bool {
    let content = ctx.world.content;
    plan.walk_with_caster(ctx.active.pos).any(|(_, step, caster_pos)| {
        let PlanStep::Cast { ability, target_pos, .. } = step else { return false };
        let Some(def) = content.abilities.get(ability) else { return false };
        if !def.friendly_fire || def.aoe == AoEShape::None {
            return false;
        }
        // Route through the shared helper so a new `AoEShape` variant lands
        // here automatically — the inline match we used to have silently
        // missed anything beyond Circle/Line.
        aoe_area(def, *target_pos, caster_pos).contains(&caster_pos)
    })
}

fn plan_has_useful_cast(plan: &TurnPlan, ctx: &ScoringCtx) -> bool {
    let content = ctx.world.content;
    let caster = &ctx.active.caster_ctx;
    plan.steps.iter().any(|s| {
        if let PlanStep::Cast { ability, .. } = s {
            content.abilities.get(ability).is_some_and(|def| {
                def.effect.calc(caster).is_some() || !def.statuses.is_empty()
            })
        } else {
            false
        }
    })
}

// ── ProtectSelf mask ───────────────────────────────────────────────────────

/// A plan is **defensive** iff its `self_survival` factor is at or above
/// `epsilon`. The `self_survival` axis captures cumulative defensive value
/// across the plan (self-heal, armor-buff, and danger-exit), making the
/// threshold independent of step-level tile/target-type heuristics.
pub fn plan_is_defensive(self_survival: f32, epsilon: f32) -> bool {
    self_survival >= epsilon
}

/// Mask non-defensive plans to `-∞` under `ProtectSelf` intent — contract
/// enforcement. A plan opt-out from the ProtectSelf contract is expressed
/// via `EvaluationMode != Default` (set upstream in `apply_adaptation`
/// when the contract is globally unsatisfiable → `ProtectSelfNoDefensive`
/// switches every plan's mode to `LastStand`). Plans in non-Default mode
/// are left alone by this mask.
///
/// Returns true if at least one plan was observed to be defensive. The
/// "no defensive plan at all" case is now handled by ADAPTATION one step
/// upstream — by the time this function runs, that case has already
/// switched all plans to `LastStand` mode, so every plan will skip the
/// mask. The return value is retained for callers that want to observe
/// contract satisfiability, but no longer triggers a LastStand rescore
/// inside this function.
pub fn apply_protect_self_mask(
    scores: &mut [f32],
    raw: &[PlanFactorValues],
    modes: &[EvaluationMode],
    epsilon: f32,
) -> bool {
    debug_assert_eq!(raw.len(), modes.len());
    let mut any_defensive = false;
    for (i, f) in raw.iter().enumerate() {
        // Plans that adaptation moved to a non-Default mode have opted
        // out of the ProtectSelf contract; the mask does not apply to
        // them.
        if !matches!(modes.get(i), Some(EvaluationMode::Default)) {
            continue;
        }
        if plan_is_defensive(f.get_plan(PlanFactor::SelfSurvival), epsilon) {
            any_defensive = true;
        } else if i < scores.len() {
            scores[i] = f32::NEG_INFINITY;
        }
    }
    any_defensive
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::test_helpers::{ent, UnitBuilder};
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;

    /// Sanity-suite defaults: max_hp=30, speed=4, aoo=(5.0, 1 reaction).
    /// Mirrors the pre-builder `fn unit(id, team, pos, hp)` factory.
    fn unit(id: u32, team: Team, pos: Hex, hp: i32) -> UnitSnapshot {
        UnitBuilder::new(id, team, pos)
            .hp(hp)
            .max_hp(30)
            .speed(4)
            .aoo(5.0, 1)
            .build()
    }

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        }
    }

    // Even-r geometry reminder (verified empirically):
    //   Neighbors of (0,0): (1,0),(-1,0),(0,1),(0,-1),(1,1),(1,-1).
    //   (-1,0) is adjacent to (0,0) but NOT to (1,0) — "leaves adjacency".
    //   (1,1) is adjacent to BOTH (0,0) and (1,0) — "stays adjacent".

    /// Shape of the expected `expected_aoo_damage` result for a row.
    #[derive(Clone, Copy, Debug)]
    enum Aoo {
        /// Exactly zero — no provoker fired.
        Zero,
        /// Strictly positive — at least one provoker fired.
        Positive,
        /// Within an inclusive `[lo, hi]` band around a known EV.
        Near(f32, f32),
        /// Lethal — result must be ≥ the actor's own HP (pins the
        /// sanity-adjust mask precondition).
        AtLeastActorHp,
    }

    /// Table-driven AoO cases. Each row pins a distinct invariant; the
    /// `name` column is formatted into every failure message so a broken
    /// row stays as diagnostic as an individually-named test.
    #[test]
    fn expected_aoo_damage_matrix() {
        fn default_enemy() -> UnitSnapshot {
            unit(2, Team::Player, hex_from_offset(1, 0), 20)
        }
        fn ranged_enemy() -> UnitSnapshot {
            let mut e = default_enemy();
            e.max_attack_range = 5;
            e.aoo_expected_damage = None;
            e
        }
        fn hybrid_enemy() -> UnitSnapshot {
            // Melee + ranged: max_attack_range>1 но melee-AoO есть.
            // Regression pin for the dropped `max_attack_range != 1` guard.
            let mut e = default_enemy();
            e.max_attack_range = 3;
            e
        }
        fn no_react_enemy() -> UnitSnapshot {
            let mut e = default_enemy();
            e.reactions_left = 0;
            e
        }
        fn second_enemy() -> UnitSnapshot {
            unit(3, Team::Player, hex_from_offset(0, 1), 20)
        }

        struct Row {
            name: &'static str,
            actor_hp: i32,
            actor_armor: i32,
            enemies: Vec<UnitSnapshot>,
            path: Vec<Hex>,
            expected: Aoo,
        }
        let rows: Vec<Row> = vec![
            Row {
                name: "leaves adjacency provokes",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "stays adjacent does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(1, 1)],
                expected: Aoo::Zero,
            },
            Row {
                name: "ranged enemy does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![ranged_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                name: "hybrid melee+ranged provokes (regression: dropped max_range guard)",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![hybrid_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Positive,
            },
            Row {
                name: "enemy with 0 reactions does not provoke",
                actor_hp: 20, actor_armor: 0,
                enemies: vec![no_react_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Zero,
            },
            Row {
                // Between two melees; (0,-1) leaves adjacency with both → 5 × 2.
                name: "multi-enemy damage sums",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy(), second_enemy()],
                path: vec![hex_from_offset(0, -1)],
                expected: Aoo::Near(9.5, 10.5),
            },
            Row {
                // Leaves → re-enters → leaves: one reaction per enemy, not three.
                name: "one AoO per enemy even with multiple transitions",
                actor_hp: 30, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![
                    hex_from_offset(-1, 0),
                    hex_from_offset(0, 0),
                    hex_from_offset(-1, 0),
                ],
                expected: Aoo::Near(4.5, 5.5),
            },
            Row {
                // Armor 10 vs raw 5.0 — final_damage floors at 1.
                name: "armor clamps expected damage at the 1-HP floor",
                actor_hp: 20, actor_armor: 10,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::Near(0.99, 1.01),
            },
            Row {
                // Precondition `expected_aoo_damage ≥ actor_hp` — the
                // input signal ADAPTATION uses to trigger
                // `ExpectedSelfLethal`. Sanity no longer reads this for
                // a hard mask (it stays in soft-bleed territory only),
                // but the helper itself must still report correctly so
                // adaptation can act on it.
                name: "expected-lethal AoO reaches actor HP threshold",
                actor_hp: 3, actor_armor: 0,
                enemies: vec![default_enemy()],
                path: vec![hex_from_offset(-1, 0)],
                expected: Aoo::AtLeastActorHp,
            },
        ];

        for row in &rows {
            let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), row.actor_hp);
            actor.armor = row.actor_armor;
            let enemy_refs: Vec<&UnitSnapshot> = row.enemies.iter().collect();
            let plan = move_plan(row.path.clone());
            let dmg = expected_aoo_damage(&actor, &plan, &enemy_refs);
            let name = row.name;
            match row.expected {
                Aoo::Zero => assert_eq!(dmg, 0.0, "[{name}] expected 0, got {dmg}"),
                Aoo::Positive => assert!(dmg > 0.0, "[{name}] expected > 0, got {dmg}"),
                Aoo::Near(lo, hi) => assert!(
                    (lo..=hi).contains(&dmg),
                    "[{name}] expected in [{lo}, {hi}], got {dmg}",
                ),
                Aoo::AtLeastActorHp => assert!(
                    dmg >= row.actor_hp as f32,
                    "[{name}] expected ≥ hp({}), got {dmg}", row.actor_hp,
                ),
            }
        }
    }

    // ── plan_has_self_aoe: routed through shared `aoe_area` ─────────────
    //
    // Smoke test: a friendly-fire Circle AoE centred on the caster's tile
    // must be detected as self-AoE. The inline match that used to live here
    // covered Circle/Line only; `aoe_area` (shared with every other AoE
    // caller) automatically picks up new `AoEShape` variants, so adding
    // e.g. Cone later will be covered without a code change here.

    use crate::combat::ai::test_helpers::empty_content;
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, EffectDef, TargetType,
    };
    use crate::core::{AbilityId, DiceExpr};

    fn fireball_def(radius: u32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("fireball"),
            name: "fireball".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 5 },
            effect: EffectDef::SpellDamage { dice: DiceExpr::new(1, 6, 0) },
            costs: Vec::new(),
            cost_ap: 1,
            aoe: AoEShape::Circle { radius },
            friendly_fire: true,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
            ai_tags_override: None,
        }
    }

    #[test]
    fn plan_has_self_aoe_detects_friendly_fire_circle_on_caster() {
        use crate::combat::ai::reservations::Reservations;
        use crate::combat::ai::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_maps, make_scoring_ctx, make_test_ctx};
        use crate::content::abilities::CasterContext;
        let actor_pos = hex_from_offset(0, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 20);
        let _caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let _abilities = crate::game::components::Abilities(Vec::new());
        let mut content = empty_content();
        let def = fireball_def(1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::hard();
        let utility = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone()], 1);
        let maps = empty_maps();
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&utility, &snap, &maps, &reservations, &actor);

        // Single-cast fireball centred on `target_pos`, fired from `actor_pos`.
        let cast_fireball_at = |target_pos: Hex| TurnPlan {
            steps: vec![PlanStep::Cast {
                ability: AbilityId::from("fireball"),
                target: ent(99),
                target_pos,
            }],
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: vec![Default::default()],
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };

        assert!(
            plan_has_self_aoe(&cast_fireball_at(actor_pos), &ctx),
            "friendly-fire circle on caster tile must be flagged as self-AoE",
        );
        assert!(
            !plan_has_self_aoe(&cast_fireball_at(hex_from_offset(5, 5)), &ctx),
            "AoE centred far from the caster must not be flagged",
        );
    }

    /// Verify that `sanity_adjust_plans` returns a correct per-plan breakdown
    /// when multiple rules fire on the same plan.
    ///
    /// Setup: two plans so the early-exit (len ≤ 1) is bypassed.
    /// - Plan 0 (noop): actor stays in place — no rules should fire.
    /// - Plan 1 (Survival + AoO bleed): actor at low HP moves away from an
    ///   adjacent melee enemy through a high-danger tile. This triggers both
    ///   `Survival` (low HP + dangerous path) and `AoOBleed` (leaves
    ///   adjacency with an enemy that has reactions).
    ///
    /// Grid note: the even-r grid has rows 0–6; row 0 (even) has columns 0–6.
    /// Actor at (3,0), enemy at (4,0), destination (2,0) — all in-bounds.
    /// Moving from (3,0) to (2,0) leaves adjacency with enemy at (4,0).
    ///
    /// We assert:
    /// - Plan 0 breakdown is empty (no rules fired).
    /// - Plan 1 breakdown contains `Survival` and `AoOBleed`.
    /// - Both penalty multipliers are < 1.0.
    #[test]
    fn breakdown_reports_survival_and_aoo_bleed() {
        use crate::combat::ai::reservations::Reservations;
        use crate::combat::ai::snapshot::BattleSnapshot;
        use crate::combat::ai::test_helpers::{empty_content, empty_maps, make_scoring_ctx, make_test_ctx};

        // Actor: very low HP (5/30 → hp_pct ≈ 0.17, well below the 0.6 threshold).
        let actor_pos = hex_from_offset(3, 0);
        let actor = unit(1, Team::Enemy, actor_pos, 5); // low HP triggers survival rule
        // Enemy adjacent to actor at (4,0) with reactions — will provoke AoO
        // when the actor moves to (2,0), leaving adjacency.
        let enemy_pos = hex_from_offset(4, 0);
        let enemy = unit(2, Team::Player, enemy_pos, 20);

        let content = empty_content();
        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::hard();
        let world = make_test_ctx(&content, &difficulty);
        let snap = BattleSnapshot::new(vec![actor.clone(), enemy], 1);
        let mut maps = empty_maps();
        // Add danger to the destination tile so the survival rule fires.
        // Actor hp_pct ≈ 0.17 → hp_need ≈ 0.72; excess = (danger - 0.5).max(0).
        // With danger=0.9: excess=0.4; surv = 1.2 × 0.72 × 0.16 = 0.138 > 0.
        let dest = hex_from_offset(2, 0);
        maps.danger.add(dest, 0.9);
        let reservations = Reservations::default();
        let ctx = make_scoring_ctx(&world, &snap, &maps, &reservations, &actor);

        // Plan 0: actor stays in place (no steps, final_pos = actor_pos).
        // (3,0) has ≥2 open neighbours so RetreatTrap doesn't fire.
        let noop_plan = TurnPlan {
            steps: Vec::new(),
            final_pos: actor_pos,
            residual_ap: 0,
            residual_mp: 4,
            outcomes: Vec::new(),
            partial_score: 0.0,
            sim_snapshots: Vec::new(),
            annotation: Default::default(),
        };
        // Plan 1: move from (3,0) to (2,0) — leaves adjacency with enemy at
        // (4,0), triggering AoOBleed; path is through the high-danger tile,
        // triggering Survival.
        let dest = hex_from_offset(2, 0);
        let aoo_plan = move_plan(vec![dest]);

        let plans = vec![noop_plan, aoo_plan];
        let mut scores = vec![1.0f32, 1.0f32];

        let breakdown = sanity_adjust_plans(&mut scores, &plans, &ctx);

        assert_eq!(breakdown.len(), 2, "breakdown length == plans length");

        // Plan 0 should have no hits (noop, no danger, no adjacency change).
        assert!(
            breakdown[0].is_empty(),
            "noop plan must produce no sanity hits, got: {:?}",
            breakdown[0],
        );

        // Plan 1 must contain Survival and AoOBleed (in order).
        let hits = &breakdown[1];
        let rules: Vec<SanityRule> = hits.iter().map(|h| h.rule).collect();
        assert!(
            rules.contains(&SanityRule::Survival),
            "expected Survival hit in breakdown, got: {:?}", rules,
        );
        assert!(
            rules.contains(&SanityRule::AoOBleed),
            "expected AoOBleed hit in breakdown, got: {:?}", rules,
        );

        // Both are penalties — multipliers < 1.0.
        for hit in hits {
            if hit.rule == SanityRule::Survival || hit.rule == SanityRule::AoOBleed {
                assert!(
                    hit.multiplier < 1.0,
                    "{:?} is a penalty — multiplier must be < 1.0, got {}",
                    hit.rule, hit.multiplier,
                );
            }
        }
    }
}
