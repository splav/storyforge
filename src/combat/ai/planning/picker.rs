//! Final plan selection: mercy tie-breaker + top-K window + commitment to the
//! first step as an `AiDecision`.

#![allow(clippy::too_many_arguments)]

use crate::combat::ai::candidates::{ActionCandidate, CandidateKind};
use crate::combat::ai::factors::aoe_area;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::intent::TacticalIntent;
use crate::combat::ai::planning::scorer::compute_plan_factors;
use crate::combat::ai::planning::types::{PlanStep, TurnPlan};
use crate::combat::ai::reservations::Reservations;
use crate::combat::ai::scoring::{applies_cc, score_action};
use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{AiDecision, PickMechanics, UtilityContext};
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::DiceRng;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

/// Commit the winning plan's first step (or first two, if they're a
/// Move→Cast bundle) as a single `AiDecision`. The remainder replans on
/// subsequent ticks (or resumes via `ActivePlan` in Phase 4 flow).
pub fn decision_from_plan(plan: &TurnPlan, actor: Entity, actor_pos: Hex) -> AiDecision {
    decision_from_steps(&plan.steps, actor, actor_pos)
}

/// Commit the leading step(s) of a plan **suffix** as an AiDecision. Used
/// when resuming a stored `ActivePlan` from `steps[cursor..]`.
pub fn decision_from_steps(steps: &[PlanStep], actor: Entity, actor_pos: Hex) -> AiDecision {
    match steps {
        [] => AiDecision::EndTurn,
        [PlanStep::Cast { ability, target, target_pos }, ..] => AiDecision::CastInPlace {
            ability: ability.clone(),
            target: *target,
            target_pos: *target_pos,
        },
        // Bundle Move→Cast into a single atomic tick so the existing engine
        // contract (one UseAbility per actor-turn pathfind) is preserved.
        [PlanStep::Move { path }, PlanStep::Cast { ability, target, target_pos }, ..] => {
            if path.is_empty() {
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
            }
        }
        [PlanStep::Move { path }, ..] => {
            let dest = path.last().copied().unwrap_or(actor_pos);
            if path.is_empty() || dest == actor_pos {
                AiDecision::EndTurn
            } else {
                AiDecision::MoveOnlyRetreat { path: path.clone() }
            }
        }
    }
    .fallback_on_actor(actor)
}

/// How many steps from `steps` does `decision_from_steps` commit this tick?
/// Mirrors the match in `decision_from_steps`: Move→Cast bundles consume 2,
/// single-step decisions consume 1, empty consumes 0.
pub fn steps_consumed_by_decision(steps: &[PlanStep]) -> usize {
    match steps {
        [] => 0,
        [PlanStep::Move { .. }, PlanStep::Cast { .. }, ..] => 2,
        _ => 1,
    }
}

/// Can the resumed step still execute given the current world? Returns an
/// error message describing *why* the step is no longer valid — useful both
/// for invalidation branching and for debug logs. Only checks **hard**
/// constraints (state can't change in-flight). Soft quality concerns (did
/// the situation get much better for a different plan?) belong to
/// opportunistic-replan, not validation.
pub fn validate_plan_step(
    step: &PlanStep,
    actor: &UnitSnapshot,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
) -> Result<(), &'static str> {
    match step {
        PlanStep::Cast { ability, target, target_pos } => {
            let def = ctx
                .content
                .abilities
                .get(ability)
                .ok_or("ability no longer in content")?;
            if actor.action_points < def.cost_ap {
                return Err("insufficient AP");
            }
            // Resource affordability mirrors can_afford_snap.
            for cost in &def.costs {
                let pool = match cost.resource {
                    crate::core::ResourceKind::Hp => actor.hp,
                    crate::core::ResourceKind::Mana => actor.mana.map(|(c, _)| c).unwrap_or(0),
                    crate::core::ResourceKind::Rage => actor.rage.map(|(c, _)| c).unwrap_or(0),
                    crate::core::ResourceKind::Energy => actor.energy.map(|(c, _)| c).unwrap_or(0),
                };
                if pool < cost.amount {
                    return Err("insufficient resource");
                }
            }
            // Target liveness: AoE centred on a tile can still fire without a
            // primary unit, but single-target abilities need the entity alive.
            if !matches!(def.aoe, crate::content::abilities::AoEShape::None) {
                // AoE: check range to target_pos rather than an entity.
                if def.range.max > 0 {
                    let dist = actor.pos.unsigned_distance_to(*target_pos);
                    if dist > def.range.max {
                        return Err("AoE target_pos out of range");
                    }
                }
                return Ok(());
            }
            let Some(target_unit) = snap.unit(*target) else {
                return Err("target unit gone");
            };
            if def.range.max > 0 {
                let dist = actor.pos.unsigned_distance_to(target_unit.pos);
                if dist > def.range.max {
                    return Err("target out of range");
                }
            }
            Ok(())
        }
        PlanStep::Move { path } => {
            if path.is_empty() {
                return Err("empty path");
            }
            if actor.movement_points < path.len() as i32 {
                return Err("insufficient MP");
            }
            // Path passability: only *live* enemies block traversal. Corpses
            // are walkable (matches the real movement system's contract).
            let enemy_positions: std::collections::HashSet<Hex> = snap
                .enemies_of(actor.team)
                .map(|u| u.pos)
                .collect();
            for h in path {
                if !crate::game::hex::is_passable(*h, &enemy_positions) {
                    return Err("path blocked");
                }
            }
            // Destination check: must match the real `HexPositions` view,
            // not `snap.units`. The snapshot filters out dead units (for
            // scoring purposes), but their `HexPositions` entries persist so
            // that `movement.rs` still treats those tiles as occupied. If a
            // unit died on the planned destination between plan generation
            // and resume, `snap.units.contains(pos)` returns false but
            // `positions.insert(actor, pos)` still panics on the corpse.
            // `ctx.blocked_tiles` reflects the HexPositions truth minus the
            // actor themself, so it catches both cases.
            let dest = *path.last().expect("non-empty path");
            if ctx.blocked_tiles.contains(&dest) {
                return Err("destination occupied");
            }
            Ok(())
        }
    }
}

// Internal: assign `actor` to any AiDecision that needs it (none here, but
// this keeps the chain consistent if we add `FallbackEndTurn { actor }`-style
// variants later). Today this is an identity no-op.
trait DecisionActor {
    fn fallback_on_actor(self, actor: Entity) -> Self;
}
impl DecisionActor for AiDecision {
    fn fallback_on_actor(self, _actor: Entity) -> Self {
        self
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

/// Adapter: synthesize an `ActionCandidate` that represents the plan's
/// **committed** first-tick action (matches what `decision_from_plan` emits).
/// Used only for debug output compatibility — the existing debug formatter
/// walks over candidates.
pub fn plan_to_candidate(plan: &TurnPlan, actor_pos: Hex) -> ActionCandidate {
    match plan.steps.as_slice() {
        [] => ActionCandidate {
            tile: actor_pos,
            path: Vec::new(),
            kind: CandidateKind::MoveOnly,
        },
        [PlanStep::Cast { ability, target, target_pos }, ..] => ActionCandidate {
            tile: actor_pos,
            path: Vec::new(),
            kind: CandidateKind::Cast {
                ability: ability.clone(),
                target_pos: *target_pos,
                target: Some(*target),
            },
        },
        [PlanStep::Move { path }, rest @ ..] => {
            let tile = *path.last().unwrap_or(&actor_pos);
            match rest.first() {
                Some(PlanStep::Cast { ability, target, target_pos }) => ActionCandidate {
                    tile,
                    path: path.clone(),
                    kind: CandidateKind::Cast {
                        ability: ability.clone(),
                        target_pos: *target_pos,
                        target: Some(*target),
                    },
                },
                _ => ActionCandidate {
                    tile,
                    path: path.clone(),
                    kind: CandidateKind::MoveOnly,
                },
            }
        }
    }
}

/// Record reservations for every cast in the winning plan so subsequent AI
/// units this round coordinate (avoid overkill, duplicate CC, tile
/// collisions). Mirrors `pick::record_reservation` but walks the full plan.
pub fn record_plan_reservation(
    plan: &TurnPlan,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    reservations: &mut Reservations,
    actor_pos: Hex,
) {
    let mut caster_tile = actor_pos;
    for step in &plan.steps {
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

    if plan.final_pos != actor_pos {
        reservations.reserve_tile(plan.final_pos);
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

    // ── steps_consumed_by_decision ─────────────────────────────────────────

    #[test]
    fn consumed_empty_is_zero() {
        assert_eq!(steps_consumed_by_decision(&[]), 0);
    }

    #[test]
    fn consumed_solo_cast_is_one() {
        let steps = vec![PlanStep::Cast {
            ability: AbilityId::from("strike"),
            target: ent(1),
            target_pos: hex_from_offset(0, 0),
        }];
        assert_eq!(steps_consumed_by_decision(&steps), 1);
    }

    #[test]
    fn consumed_move_cast_bundle_is_two() {
        let steps = vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(1),
                target_pos: hex_from_offset(2, 0),
            },
        ];
        assert_eq!(steps_consumed_by_decision(&steps), 2);
    }

    #[test]
    fn consumed_solo_move_is_one() {
        let steps = vec![PlanStep::Move { path: vec![hex_from_offset(1, 0)] }];
        assert_eq!(steps_consumed_by_decision(&steps), 1);
    }

    #[test]
    fn consumed_move_move_is_one_no_bundle() {
        // Only Move→Cast bundles; Move→Move commits one at a time.
        let steps = vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Move { path: vec![hex_from_offset(2, 0)] },
        ];
        assert_eq!(steps_consumed_by_decision(&steps), 1);
    }

    // ── validate_plan_step happy paths + failures ──────────────────────────

    #[test]
    fn validate_cast_ok_when_target_alive_and_in_range() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let target = unit(2, Team::Player, hex_from_offset(1, 0));
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), target],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Cast {
            ability: def.id,
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        assert!(validate_plan_step(&step, &actor, &snap, &ctx).is_ok());
    }

    #[test]
    fn validate_cast_fails_when_target_gone() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let target_id = ent(2);

        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        // Snapshot has no target unit → validation must fail.
        let snap = BattleSnapshot {
            units: vec![actor.clone()],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Cast {
            ability: def.id,
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("target unit gone"),
        );
    }

    #[test]
    fn validate_cast_fails_when_target_out_of_range() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let target = unit(2, Team::Player, hex_from_offset(5, 0));
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), target.clone()],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Cast {
            ability: def.id,
            target: target_id,
            target_pos: target.pos,
        };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("target out of range"),
        );
    }

    #[test]
    fn validate_cast_fails_when_ap_depleted() {
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        actor.action_points = 0; // spent
        let target = unit(2, Team::Player, hex_from_offset(1, 0));
        let target_id = target.entity;

        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def.clone());

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![def.id.clone()]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), target],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Cast {
            ability: def.id,
            target: target_id,
            target_pos: hex_from_offset(1, 0),
        };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("insufficient AP"),
        );
    }

    #[test]
    fn validate_move_ok_when_path_clear_and_mp_enough() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone()],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Move { path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)] };
        assert!(validate_plan_step(&step, &actor, &snap, &ctx).is_ok());
    }

    #[test]
    fn validate_move_fails_when_destination_occupied_by_blocker() {
        // Destination (2,0) blocked via ctx.blocked_tiles — mirrors real
        // `HexPositions` including corpses of dead units.
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![]);

        // Build a custom blocker set containing the destination.
        let mut blocked: std::collections::HashSet<Hex> = std::collections::HashSet::new();
        blocked.insert(hex_from_offset(2, 0));
        let ctx = UtilityContext {
            content: &content,
            difficulty: &difficulty,
            caster: &caster,
            abilities: &abilities,
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
            blocked_tiles: &blocked,
        };

        let snap = BattleSnapshot {
            units: vec![actor.clone()],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Move { path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)] };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("destination occupied"),
        );
    }

    #[test]
    fn validate_move_fails_when_path_blocked_by_enemy() {
        let actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        let blocker = unit(2, Team::Player, hex_from_offset(1, 0));
        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone(), blocker],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Move { path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)] };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("path blocked"),
        );
    }

    #[test]
    fn validate_move_fails_when_mp_insufficient() {
        let mut actor = unit(1, Team::Enemy, hex_from_offset(0, 0));
        actor.movement_points = 1; // only 1 MP

        let mut content = empty_content();
        let def = ability("strike", 1, 1);
        content.abilities.insert(def.id.clone(), def);

        let difficulty = DifficultyProfile::normal();
        let caster = CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None };
        let abilities = Abilities(vec![]);
        let ctx = make_ctx(&content, &difficulty, &caster, &abilities);

        let snap = BattleSnapshot {
            units: vec![actor.clone()],
            active_unit: actor.entity,
            round: 1,
        };

        let step = PlanStep::Move { path: vec![hex_from_offset(1, 0), hex_from_offset(2, 0)] };
        assert_eq!(
            validate_plan_step(&step, &actor, &snap, &ctx),
            Err("insufficient MP"),
        );
    }

    // ── decision_from_steps suffix behavior ────────────────────────────────

    #[test]
    fn decision_from_steps_bundles_move_cast_suffix() {
        let actor = ent(1);
        let actor_pos = hex_from_offset(0, 0);
        let steps = vec![
            PlanStep::Move { path: vec![hex_from_offset(1, 0)] },
            PlanStep::Cast {
                ability: AbilityId::from("strike"),
                target: ent(2),
                target_pos: hex_from_offset(2, 0),
            },
        ];
        match decision_from_steps(&steps, actor, actor_pos) {
            AiDecision::MoveAndCast { path, ability, target, .. } => {
                assert_eq!(path.len(), 1);
                assert_eq!(ability.0, "strike");
                assert_eq!(target, ent(2));
            }
            other => panic!("expected MoveAndCast, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
