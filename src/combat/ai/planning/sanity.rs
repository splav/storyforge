//! Plan-level sanity adjustments. Multiplicative penalties for situations the
//! per-factor scoring can't catch — walking through a dangerous corridor with
//! low HP, moving onto a tile with no LoS, centering a self-AoE on yourself,
//! cornering yourself into a 1-neighbour dead-end. Mirrors
//! `utility/sanity.rs` but operates on `TurnPlan` instead of `ActionCandidate`.
//!
//! Applied between `score_plans` and `pick_best_plan`: each plan's final score
//! gets multiplied in place by a product of the penalty factors. Floor at
//! `SURVIVAL_FLOOR` keeps even punished plans competitive when all options
//! are bad; retreat lines still beat "rush at 5 HP".

#![allow(clippy::too_many_arguments)]

use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::position_eval::evaluate_position;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{AoEShape, TargetType};
use crate::content::content_view::ContentView;
use crate::game::hex::{has_los, hex_circle, hex_line, in_bounds, Hex};
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

pub fn sanity_adjust_plans(
    scores: &mut [f32],
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    ctx: &UtilityContext,
) {
    if scores.len() <= 1 {
        return;
    }

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active.team)
        .filter(|u| u.entity != active.entity)
        .collect();
    let occupied: HashSet<Hex> = snap.units.iter().map(|u| u.pos).collect();
    let ally_positions: HashSet<Hex> = allies.iter().map(|a| a.pos).collect();
    let current_pos_eval = evaluate_position(active.pos, &active.role, maps);
    let current_danger = maps.danger.get(active.pos);

    for (plan, score) in plans.iter().zip(scores.iter_mut()) {
        if !score.is_finite() {
            continue;
        }
        let mut penalty = 1.0f32;
        let final_pos = plan.final_pos;

        // Worst danger the actor touches across moves + resting tile. Matches
        // scorer's path_danger_max so the factor and the penalty look at the
        // same signal.
        let max_path_danger = worst_path_danger(active.pos, plan, maps);

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
        if plan_has_self_aoe(active, plan, ctx) {
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
            let net = (raw - mitigation + vuln).max(1.0);
            total += net;
        }
    }
    total
}

fn worst_path_danger(start: Hex, plan: &TurnPlan, maps: &InfluenceMaps) -> f32 {
    let mut max_d = maps.danger.get(plan.final_pos);
    let mut pos = start;
    for step in &plan.steps {
        if let PlanStep::Move { path } = step {
            for &h in path {
                let d = maps.danger.get(h);
                if d > max_d {
                    max_d = d;
                }
                pos = h;
            }
        }
    }
    let _ = pos;
    max_d
}

fn plan_has_self_aoe(active: &UnitSnapshot, plan: &TurnPlan, ctx: &UtilityContext) -> bool {
    let mut caster_pos = active.pos;
    for step in &plan.steps {
        match step {
            PlanStep::Move { path } => {
                if let Some(&dest) = path.last() {
                    caster_pos = dest;
                }
            }
            PlanStep::Cast { ability, target_pos, .. } => {
                let Some(def) = ctx.content.abilities.get(ability) else { continue };
                if !def.friendly_fire || def.aoe == AoEShape::None {
                    continue;
                }
                let area: HashSet<Hex> = match def.aoe {
                    AoEShape::Circle { radius } => hex_circle(*target_pos, radius).into_iter().collect(),
                    AoEShape::Line { length } => {
                        hex_line(caster_pos, *target_pos, length).into_iter().collect()
                    }
                    AoEShape::None => HashSet::new(),
                };
                if area.contains(&caster_pos) {
                    return true;
                }
            }
        }
    }
    false
}

fn plan_has_useful_cast(plan: &TurnPlan, ctx: &UtilityContext) -> bool {
    plan.steps.iter().any(|s| {
        if let PlanStep::Cast { ability, .. } = s {
            ctx.content.abilities.get(ability).is_some_and(|def| {
                def.effect.calc(ctx.caster).is_some() || !def.statuses.is_empty()
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
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::AiTags;
    use crate::game::components::Team;
    use crate::game::hex::hex_from_offset;
    use bevy::prelude::Entity;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    fn unit(id: u32, team: Team, pos: Hex, hp: i32) -> UnitSnapshot {
        UnitSnapshot {
            entity: ent(id),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp,
            max_hp: 30,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 1,
            max_ap: 1,
            movement_points: 4,
            speed: 4,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 1,
            aoo_expected_damage: Some(5.0),
            statuses: Vec::new(),
        }
    }

    fn move_plan(path: Vec<Hex>) -> TurnPlan {
        TurnPlan {
            steps: vec![PlanStep::Move { path: path.clone() }],
            final_pos: *path.last().unwrap(),
            residual_ap: 1,
            residual_mp: 0,
            outcomes: vec![],
            partial_score: 0.0,
        }
    }

    // Even-r geometry reminder (verified empirically):
    //   Neighbors of (0,0): (1,0),(-1,0),(0,1),(0,-1),(1,1),(1,-1).
    //   Neighbors of (1,0) include (0,0),(2,0),(1,1),(1,-1).
    //   (-1,0) is adjacent to (0,0) but NOT to (1,0) — used to "leave adjacency".
    //   (1,1) is adjacent to BOTH (0,0) and (1,0) — used for "stay adjacent".

    #[test]
    fn aoo_triggered_on_leaving_adjacency() {
        // Actor (0,0) adjacent to enemy (1,0). Move to (-1,0): dist 1→2 → AoO.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20);
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let plan = move_plan(vec![hex_from_offset(-1, 0)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert!(dmg > 0.0, "leaving adjacency should provoke");
    }

    #[test]
    fn aoo_not_triggered_when_staying_adjacent() {
        // Actor (0,0), enemy (1,0). (1,1) is adjacent to both → no transition.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20);
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let plan = move_plan(vec![hex_from_offset(1, 1)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert_eq!(dmg, 0.0, "stepping while remaining adjacent must not provoke");
    }

    #[test]
    fn ranged_enemy_does_not_provoke() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20);
        let mut enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        enemy.max_attack_range = 5;
        enemy.aoo_expected_damage = None;
        let plan = move_plan(vec![hex_from_offset(-1, 0)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert_eq!(dmg, 0.0, "ranged enemy must not trigger AoO");
    }

    #[test]
    fn no_reactions_left_does_not_provoke() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20);
        let mut enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        enemy.reactions_left = 0;
        let plan = move_plan(vec![hex_from_offset(-1, 0)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert_eq!(dmg, 0.0, "enemy with 0 reactions must not trigger");
    }

    #[test]
    fn multi_enemy_damage_sums() {
        // Actor (0,0) between two melees at (1,0) and (0,1). Move to (0,-1):
        // dist to (1,0) = 2, dist to (0,1) = 2 — leaves adjacency with both.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 30);
        let e1 = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let e2 = unit(3, Team::Player, hex_from_offset(0, 1), 20);
        let plan = move_plan(vec![hex_from_offset(0, -1)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&e1, &e2]);
        // Both provokers deal 5.0 each (no armor) → 10.0 total.
        assert!((9.5..=10.5).contains(&dmg), "expected ~10 total damage, got {dmg}");
    }

    #[test]
    fn one_aoo_per_enemy_even_with_multiple_transitions() {
        // Path: (0,0)→(-1,0)[leaves]→(0,0)[re-enters]→(-1,0)[leaves again].
        // Enemy only has one reaction per round — count the first trigger.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 30);
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let plan = move_plan(vec![
            hex_from_offset(-1, 0),
            hex_from_offset(0, 0),
            hex_from_offset(-1, 0),
        ]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert!((4.5..=5.5).contains(&dmg), "expected single AoO (~5), got {dmg}");
    }

    #[test]
    fn armor_reduces_expected_damage_with_floor_of_one() {
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 20);
        actor.armor = 10; // much higher than 5.0 raw → floor at 1.0.
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let plan = move_plan(vec![hex_from_offset(-1, 0)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert!((0.99..=1.01).contains(&dmg), "expected floored to 1.0, got {dmg}");
    }

    #[test]
    fn lethal_aoo_damage_reaches_hp_threshold() {
        // Precondition for sanity_adjust_plans' lethal masking: dmg ≥ hp.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0), 3);
        let enemy = unit(2, Team::Player, hex_from_offset(1, 0), 20);
        let plan = move_plan(vec![hex_from_offset(-1, 0)]);
        let dmg = expected_aoo_damage(&actor, &plan, &[&enemy]);
        assert!(
            dmg >= actor.hp as f32,
            "expected lethal damage ({dmg}) ≥ hp ({})",
            actor.hp,
        );
    }
}
