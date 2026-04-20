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

use crate::combat::ai::factors::aoe_area;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::scorer::worst_path_danger;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, UnitSnapshot};
use crate::combat::ai::utility::ScoringCtx;
use crate::combat::effects_math::final_damage_f32;
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ContentView;
use crate::game::hex::{has_los, in_bounds, Hex};
use std::collections::HashSet;

/// Minimum multiplier applied by survival quadratic. Keeps low-HP-in-danger
/// plans comparable when every option is bad.
const SURVIVAL_FLOOR: f32 = 0.25;
/// Amplifies the HP × danger² product. Same value the old sanity used.
const LOW_HP_FACTOR: f32 = 1.2;
/// AoO-penalty shape constant. `k * (expected/hp)^2` eats into the multiplier;
/// k=2 gives `1 - 0.5 = 0.5x` when AoO projects to half HP, close to
/// `SURVIVAL_FLOOR` at 70%. Tunable alongside LOW_HP.
const AOO_PENALTY_K: f32 = 2.0;
/// Floor for the AoO-risk (non-lethal) multiplier. Same reasoning as
/// SURVIVAL_FLOOR: keep the plan comparable when every option bleeds.
const AOO_RISK_FLOOR: f32 = 0.25;

pub fn sanity_adjust_plans(scores: &mut [f32], plans: &[TurnPlan], ctx: &ScoringCtx) {
    if scores.len() <= 1 {
        return;
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
    let current_pos_eval = evaluate_position(active.pos, &active.role, maps);
    let current_danger = maps.danger.get(active.pos);

    for (plan, score) in plans.iter().zip(scores.iter_mut()) {
        if !score.is_finite() {
            continue;
        }
        let mut penalty = 1.0f32;
        let final_pos = plan.final_pos;

        // Worst danger the actor touches across moves + resting tile.
        // Shared helper with scorer (`scorer::worst_path_danger`) so the
        // factor and the penalty look at exactly the same signal.
        let max_path_danger = worst_path_danger(plan, maps);

        // 1. Survival: low-HP actor crossing/resting on dangerous tiles.
        // Uses `max_path_danger` rather than just final_pos so "walk through
        // Kael's AoO corridor to land on a safe tile" still eats the penalty.
        let hp_need = ((0.6 - active.hp_pct()) / 0.6).clamp(0.0, 1.0);
        let excess = (max_path_danger - 0.5).max(0.0);
        let surv = LOW_HP_FACTOR * hp_need * excess * excess;
        if surv > 0.0 {
            penalty *= (1.0 - surv).max(SURVIVAL_FLOOR);
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
        }

        // 5. Self-AoE: any Cast step in the plan centers a friendly-fire AoE
        // on a tile that covers the caster's position at that moment.
        if plan_has_self_aoe(plan, ctx) {
            penalty *= 0.5;
        }

        // 6. AoO exposure: every Move step transition `was_adj && !still_adj`
        // against a melee enemy with reactions provokes an opportunity attack.
        // Sum expected damage per enemy (one AoO per enemy per turn); if the
        // sum is lethal against current HP, mask the plan to −∞. Non-lethal
        // case: multiplicative quadratic penalty with a floor — gradient so a
        // high-reward plan (finish a target) can still accept the risk.
        let aoo_dmg = expected_aoo_damage(active, plan, &enemies);
        if aoo_dmg >= active.hp as f32 && active.hp > 0 {
            *score = f32::NEG_INFINITY;
            continue;
        }
        if aoo_dmg > 0.0 {
            let ratio = (aoo_dmg / active.hp.max(1) as f32).min(1.0);
            let factor = (1.0 - AOO_PENALTY_K * ratio * ratio).max(AOO_RISK_FLOOR);
            penalty *= factor;
        }

        // 7. Synergy bonus: the plan repositions to a safer/better tile AND
        // includes a useful cast. Encourages retreat-and-help combos. Multi-
        // plicative so it doesn't flip sign.
        if final_pos != active.pos {
            let safer_tile = maps.danger.get(final_pos) + 0.05 < current_danger;
            let better_pos = evaluate_position(final_pos, &active.role, maps) > current_pos_eval;
            if (safer_tile || better_pos) && plan_has_useful_cast(plan, ctx) {
                penalty *= 1.1;
            }
        }

        *score *= penalty;
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Sum of expected AoO damage the plan would take across all provoking
/// transitions. For each melee enemy with reactions and a damage estimate,
/// scan the plan's movement path for the first `was_adj && !still_adj`
/// transition (one AoO per enemy per round) and accrue its expected damage
/// against the actor's armor + vulnerability. Returns 0.0 if no provokers
/// are triggered — fast path for typical non-adjacent moves.
fn expected_aoo_damage(
    active: &UnitSnapshot,
    plan: &TurnPlan,
    enemies: &[&UnitSnapshot],
) -> f32 {
    let mut total = 0.0f32;
    let mitigation = (active.armor + active.armor_bonus) as f32;
    let vuln = active.damage_taken_bonus as f32;
    for e in enemies {
        if e.reactions_left <= 0 || e.max_attack_range != 1 {
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
    let content = ctx.utility.world.content;
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
    let content = ctx.utility.world.content;
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

/// A plan is **defensive** iff its *first* step is defensive. Rationale: the
/// first step is what gets committed this tick; subsequent steps are
/// opportunistic and will be re-validated next tick from the resulting state.
/// Judging the whole plan by its first step matches "what actually executes
/// now" and doesn't reward filler-offensive suffixes hiding behind a safe
/// opener.
///
/// Step-level defense:
/// - **Move**: destination strictly safer than current tile by
///   `defensive_margin`.
/// - **Cast** on self/ally: always defensive (heals, buffs, self-regen).
/// - **Cast** on enemy: only defensive if the cast fires from a tile safer
///   than the actor's current position — i.e. the plan repositioned before
///   casting.
///
/// Empty plans (seed "skip turn") are defensive by default: doing nothing
/// preserves state; if current tile is dangerous, retreat plans will beat it
/// on position/risk factors anyway.
pub fn plan_is_defensive(
    plan: &TurnPlan,
    actor: &UnitSnapshot,
    content: &ContentView,
    maps: &InfluenceMaps,
    defensive_margin: f32,
) -> bool {
    let Some(first) = plan.steps.first() else { return true };
    let current_danger = maps.danger.get(actor.pos);
    match first {
        PlanStep::Move { path } => {
            let Some(&dest) = path.last() else { return true };
            maps.danger.get(dest) + defensive_margin < current_danger
        }
        PlanStep::Cast { ability, .. } => {
            let Some(def) = content.abilities.get(ability) else { return false };
            // Any ally/self cast = defensive. First-step Cast has caster_pos
            // == actor.pos (no preceding move), so the "cast from safer
            // tile" branch doesn't apply here by definition; it's covered by
            // plans that lead with a Move instead.
            matches!(
                def.target_type,
                TargetType::SingleAlly | TargetType::Myself,
            )
        }
    }
}

/// Mask non-defensive plans to `-∞` under `ProtectSelf` intent. Returns true
/// if at least one defensive plan survived — the caller can detect the
/// "no safe option" case and rescore under `LastStand` instead.
pub fn apply_protect_self_mask(
    scores: &mut [f32],
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    content: &ContentView,
    maps: &InfluenceMaps,
    defensive_margin: f32,
) -> bool {
    let mut any_defensive = false;
    for (i, p) in plans.iter().enumerate() {
        if plan_is_defensive(p, active, content, maps, defensive_margin) {
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
                // Precondition for `sanity_adjust_plans` lethal mask.
                name: "lethal AoO reaches actor HP threshold",
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
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = crate::game::components::Abilities(Vec::new());
        let mut content = empty_content();
        let def = fireball_def(1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = crate::combat::ai::difficulty::DifficultyProfile::normal();
        let utility = make_test_ctx(&content, &difficulty, &caster, &abilities);
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
}
