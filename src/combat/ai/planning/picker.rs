//! Final plan selection: mercy tie-breaker + top-K window + commitment to the
//! first step as an `AiDecision`.

#![allow(clippy::too_many_arguments)]

/// Raw mechanics output from `pick_best_plan`. The outer layer converts pool
/// indices into human-readable labels for debug output.
pub struct PickMechanics {
    pub top_k: usize,
    pub window: f32,
    pub mercy_margin: f32,
    pub mercy_applied: bool,
    /// `(plan_index, final_score)` in pool order.
    pub pool: Vec<(usize, f32)>,
    pub chosen_pos: usize,
}

use crate::combat::ai::factors::aoe_area;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::scorer::compute_plan_factors;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::{applies_cc, score_action};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{AiDecision, UtilityContext};
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::DiceRng;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

/// Commit the winning plan's first step (or first two, if they're a
/// Move→Cast bundle) as a single `AiDecision`, along with how many steps
/// of the plan the decision consumed. The remainder of the plan is
/// discarded — every AI tick re-plans from scratch.
///
/// Bundling rules (`consumed` follows the match arm):
/// - Empty plan → `EndTurn`, 0 steps.
/// - `[Cast, ..]` → `CastInPlace`, 1 step.
/// - `[Move, Cast, ..]` → `MoveAndCast` (or `CastInPlace` if the move path
///   is empty), 2 steps. One atomic tick preserves the engine contract
///   (one `UseAbility` per actor-turn pathfind).
/// - `[Move, ..]` → `MoveOnlyRetreat` (or `EndTurn` when the path is a no-op),
///   1 step.
pub fn commit_plan(plan: &TurnPlan, actor_pos: Hex) -> (AiDecision, usize) {
    match plan.steps.as_slice() {
        [] => (AiDecision::EndTurn, 0),
        [PlanStep::Cast { ability, target, target_pos }, ..] => (
            AiDecision::CastInPlace {
                ability: ability.clone(),
                target: *target,
                target_pos: *target_pos,
            },
            1,
        ),
        [PlanStep::Move { path }, PlanStep::Cast { ability, target, target_pos }, ..] => {
            let decision = if path.is_empty() {
                AiDecision::CastInPlace {
                    ability: ability.clone(),
                    target: *target,
                    target_pos: *target_pos,
                }
            } else {
                AiDecision::MoveAndCast {
                    path: path.clone(),
                    ability: ability.clone(),
                    target: *target,
                    target_pos: *target_pos,
                }
            };
            (decision, 2)
        }
        [PlanStep::Move { path }, ..] => {
            let dest = path.last().copied().unwrap_or(actor_pos);
            let decision = if path.is_empty() || dest == actor_pos {
                AiDecision::EndTurn
            } else {
                AiDecision::MoveOnlyRetreat { path: path.clone() }
            };
            (decision, 1)
        }
    }
}

/// Mercy cruelty for a plan: how harsh does this plan feel? Kill dominates;
/// CC caps at 0.5 regardless of magnitude.
fn mercy_cruelty(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
) -> f32 {
    let f = compute_plan_factors(plan, active, intent, ctx, snap, maps, reservations);
    // factors: [dmg, kill, cc, heal, pos, risk, focus, intent, scarcity]
    f[1] + (f[2] * 0.1).min(0.5)
}

/// Pick the winning plan. Mirrors `pick_best_candidate` — window-bounded top-K
/// sampling with a mercy tie-breaker applied only inside the near-best window.
pub fn pick_best_plan(
    scored: &[f32],
    plans: &[TurnPlan],
    active: &UnitSnapshot,
    intent: &TacticalIntent,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reservations: &Reservations,
    rng: &mut DiceRng,
) -> (usize, PickMechanics) {
    let top_k_req = ctx.difficulty.top_k_choice();
    let m = ctx.difficulty.mercy_margin();
    let window = (ctx.difficulty.score_noise() * 2.0).max(0.05);

    if scored.is_empty() {
        return (
            0,
            PickMechanics {
                top_k: top_k_req,
                window,
                mercy_margin: m,
                mercy_applied: false,
                pool: vec![],
                chosen_pos: 0,
            },
        );
    }

    let mut ranked: Vec<(usize, f32)> = scored.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let best_score = ranked[0].1;
    let mut mercy_applied = false;
    if m > 0.0 && best_score.is_finite() {
        let mercy_end = ranked
            .iter()
            .position(|(_, s)| !s.is_finite() || *s < best_score - m)
            .unwrap_or(ranked.len());
        if mercy_end > 1 {
            let mut windowed: Vec<(usize, f32)> = ranked[..mercy_end]
                .iter()
                .map(|&(i, s)| {
                    let cruel =
                        mercy_cruelty(&plans[i], active, intent, ctx, snap, maps, reservations);
                    (i, s - m * cruel)
                })
                .collect();
            windowed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (slot, item) in windowed.into_iter().enumerate() {
                ranked[slot] = item;
            }
            mercy_applied = true;
        }
    }

    let k = top_k_req.max(1).min(ranked.len());
    let best_after = ranked[0].1;
    let pool: Vec<(usize, f32)> = ranked
        .iter()
        .take(k)
        .filter(|(_, s)| s.is_finite() && *s >= best_after - window)
        .map(|&(i, s)| (i, s))
        .collect();

    if pool.is_empty() {
        return (
            ranked[0].0,
            PickMechanics {
                top_k: k,
                window,
                mercy_margin: m,
                mercy_applied,
                pool: vec![(ranked[0].0, ranked[0].1)],
                chosen_pos: 0,
            },
        );
    }
    let chosen_pos = if pool.len() == 1 {
        0
    } else {
        (rng.roll_d(pool.len() as u32) as usize).saturating_sub(1)
    };
    (
        pool[chosen_pos].0,
        PickMechanics {
            top_k: k,
            window,
            mercy_margin: m,
            mercy_applied,
            pool,
            chosen_pos,
        },
    )
}

/// Record reservations for the **committed** prefix of the winning plan so
/// subsequent AI units this round coordinate (avoid overkill, duplicate CC,
/// tile collisions). Only the first `consumed` steps — the ones this tick
/// actually emits as an `AiDecision` — are recorded. Future plan steps stay
/// invisible to the reservation layer until they themselves commit on a later
/// tick; this trades a slightly weaker coordination signal for freedom from
/// ghost reservations when plans get invalidated mid-flight.
///
/// `consumed` comes from `steps_consumed_by_decision` and matches the match
/// arm in `decision_from_steps` (1 for a solo cast/move, 2 for a Move→Cast
/// bundle).
pub fn record_committed_reservations(
    plan: &TurnPlan,
    consumed: usize,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    reservations: &mut Reservations,
    actor_pos: Hex,
) {
    let mut caster_tile = actor_pos;
    for step in plan.steps.iter().take(consumed) {
        match step {
            PlanStep::Move { path } => {
                if let Some(&dest) = path.last() {
                    caster_tile = dest;
                }
            }
            PlanStep::Cast { ability, target, target_pos } => {
                let Some(def) = ctx.content.abilities.get(ability) else { continue };
                let is_cc = applies_cc(def, ctx.content);
                let hits: Vec<Entity> = if def.aoe == AoEShape::None {
                    vec![*target]
                } else {
                    let area = aoe_area(def, *target_pos, caster_tile);
                    snap.enemies_of(active.team)
                        .filter(|e| area.contains(&e.pos))
                        .map(|e| e.entity)
                        .collect()
                };
                for ent in hits {
                    if let Some(target_unit) = snap.unit(ent) {
                        if def.target_type != TargetType::SingleAlly {
                            let dmg = score_action(def, target_unit, ctx.caster, ctx.content);
                            if dmg > 0.0 {
                                reservations.reserve_damage(ent, dmg);
                            }
                        }
                        if is_cc {
                            reservations.reserve_cc(ent);
                        }
                    }
                }
            }
        }
    }

    // Reserve the tile we'll actually stop on this tick (end of the committed
    // prefix), not the plan's eventual `final_pos` — same no-ghost principle.
    if caster_tile != actor_pos {
        reservations.reserve_tile(caster_tile);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
    use crate::content::abilities::{
        AbilityDef, AbilityRange, AoEShape, CasterContext, EffectDef, TargetType,
    };
    use crate::content::content_view::ContentView;
    use crate::content::races::CritFailEffect;
    use crate::core::{AbilityId, DiceExpr};
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::hex_from_offset;
    use std::collections::HashMap;

    fn ent(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("valid")
    }

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: ent(id),
            team,
            role: AxisProfile::from(AiRole::Bruiser),
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 1,
            max_ap: 1,
            movement_points: 3,
            speed: 3,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec![],
            threat: 5.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        }
    }

    fn empty_content() -> ContentView {
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

    fn ability(id: &str, range: u32, cost_ap: i32) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from(id),
            name: id.to_string(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: range },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
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

    fn make_ctx<'a>(
        content: &'a ContentView,
        difficulty: &'a DifficultyProfile,
        caster: &'a CasterContext,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        UtilityContext {
            content,
            difficulty,
            caster,
            abilities,
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
            blocked_tiles: crate::combat::ai::utility::empty_blocked_tiles(),
        }
    }

    // ── commit_plan: (decision, consumed) shape for each plan arm ──────────

    fn plan_from(steps: Vec<PlanStep>) -> TurnPlan {
        TurnPlan {
            steps,
            final_pos: hex_from_offset(0, 0),
            residual_ap: 0,
            residual_mp: 0,
            outcomes: Vec::new(),
            partial_score: 0.0,
        }
    }

    #[test]
    fn commit_empty_plan_ends_turn() {
        let (decision, consumed) = commit_plan(&plan_from(vec![]), hex_from_offset(0, 0));
        assert!(matches!(decision, AiDecision::EndTurn));
        assert_eq!(consumed, 0);
    }

    #[test]
    fn commit_solo_cast_consumes_one() {
        let plan = plan_from(vec![PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target: ent(1),
            target_pos: hex_from_offset(0, 0),
        }]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert!(matches!(decision, AiDecision::CastInPlace { .. }));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn commit_move_cast_bundles_into_two() {
        let plan = plan_from(vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(2),
                target_pos: hex_from_offset(2, 0),
            },
        ]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        match decision {
            AiDecision::MoveAndCast { path, ability, target, .. } => {
                assert_eq!(path.len(), 1);
                assert_eq!(ability.0, "strike");
                assert_eq!(target, ent(2));
            }
            other => panic!("expected MoveAndCast, got {:?}", std::mem::discriminant(&other)),
        }
        assert_eq!(consumed, 2);
    }

    #[test]
    fn commit_solo_move_consumes_one() {
        let plan = plan_from(vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }]);
        let (decision, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert!(matches!(decision, AiDecision::MoveOnlyRetreat { .. }));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn commit_move_move_keeps_first_only_no_bundle() {
        let plan = plan_from(vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Move { path: vec![hex_from_offset(2, 0)] },
        ]);
        let (_, consumed) = commit_plan(&plan, hex_from_offset(0, 0));
        assert_eq!(consumed, 1, "Move→Move does not bundle");
    }
}
