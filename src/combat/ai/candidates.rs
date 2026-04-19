//! Candidate pool construction: enumerate tiles × abilities × targets, apply
//! budget truncation, and hand a ranked `Vec<ActionCandidate>` to the scoring
//! pipeline. No scoring lives here — only "what could the actor possibly do".

use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::target_priority::target_priority;
use crate::combat::ai::utility::UtilityContext;
use crate::content::abilities::{AoEShape, TargetType};
use crate::core::{AbilityId, ResourceKind};
use crate::game::hex::{has_los, hex_circle, in_bounds, Hex};
use crate::game::pathfinding::ReachableMap;
use bevy::prelude::*;
use hexx::EdgeDirection;
use std::collections::HashSet;

// ── Candidate types ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ActionCandidate {
    pub tile: Hex,
    pub path: Vec<Hex>,
    pub kind: CandidateKind,
}

/// A candidate is either a cast (ability + target) or a pure movement to a
/// defensive tile. MoveOnly integrates "just retreat" into the normal scoring
/// pipeline — top_k, mercy, noise and intent all apply uniformly.
///
/// `target` is `Some` for single-target abilities (SingleEnemy / SingleAlly /
/// Myself). For AoE casts the primary target is the area centred at
/// `target_pos`, so `target` is `None` — area scoring handles damage/CC.
#[derive(Clone)]
pub enum CandidateKind {
    Cast {
        ability: AbilityId,
        target_pos: Hex,
        target: Option<Entity>,
    },
    MoveOnly,
}

impl ActionCandidate {
    pub fn ability(&self) -> Option<&AbilityId> {
        match &self.kind {
            CandidateKind::Cast { ability, .. } => Some(ability),
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn target(&self) -> Option<Entity> {
        match &self.kind {
            CandidateKind::Cast { target, .. } => *target,
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn target_pos(&self) -> Option<Hex> {
        match &self.kind {
            CandidateKind::Cast { target_pos, .. } => Some(*target_pos),
            CandidateKind::MoveOnly => None,
        }
    }
    pub fn is_move_only(&self) -> bool {
        matches!(self.kind, CandidateKind::MoveOnly)
    }
}

// ── Public entry ────────────────────────────────────────────────────────────

pub fn generate_candidates(
    actor_pos: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
) -> Vec<ActionCandidate> {
    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active.team).collect();
    let allies: Vec<&UnitSnapshot> = snap.allies_of(active.team).collect();

    let tiles = select_diverse_tiles(actor_pos, active, ctx, snap, maps, reach, &enemies);

    let mut candidates = Vec::new();
    for &tile in &tiles {
        let path = if tile == actor_pos {
            vec![]
        } else {
            match reach.path_to(tile) {
                Some(p) => p,
                None => continue,
            }
        };
        // Needs more movement points to get there than we have.
        if path.len() as i32 > active.movement_points {
            continue;
        }
        emit_casts_from_tile(tile, &path, active, ctx, &enemies, &allies, &mut candidates);
    }

    // MoveOnly: add pure-movement options to safe reachable tiles. These let
    // the AI choose retreat via the normal scoring pipeline (with noise, top_k,
    // mercy) instead of a special-case branch.
    if active.movement_points > 0 {
        add_move_only_candidates(actor_pos, reach, maps, &mut candidates);
    }

    dedup_and_order(candidates, active, snap, ctx)
}

/// Expand all (ability × target_pos × target) triples castable from `tile`.
fn emit_casts_from_tile(
    tile: Hex,
    path: &[Hex],
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    enemies: &[&UnitSnapshot],
    allies: &[&UnitSnapshot],
    out: &mut Vec<ActionCandidate>,
) {
    for ability_id in &ctx.abilities.0 {
        let Some(def) = ctx.content.abilities.get(ability_id) else { continue };

        if !can_afford_snap(def, active) {
            continue;
        }

        let max_range = def.range.max;

        // AoE casts have no single target — area scoring handles damage/CC.
        // Single-target casts point at a living unit at target_pos (dead units
        // are already absent from the snapshot).
        let targets: Vec<(Hex, Option<Entity>)> = match def.aoe {
            AoEShape::None => match def.target_type {
                TargetType::SingleEnemy => enemies
                    .iter()
                    .filter(|e| max_range == 0 || tile.unsigned_distance_to(e.pos) <= max_range)
                    .map(|e| (e.pos, Some(e.entity)))
                    .collect(),
                TargetType::SingleAlly => allies
                    .iter()
                    .filter(|a| max_range == 0 || tile.unsigned_distance_to(a.pos) <= max_range)
                    .map(|a| (a.pos, Some(a.entity)))
                    .collect(),
                // Self-buff / utility: one candidate per caster tile, target = caster.
                TargetType::Myself => vec![(tile, Some(active.entity))],
            },
            AoEShape::Circle { radius } => {
                let mut centers: HashSet<Hex> = HashSet::new();
                for enemy in enemies {
                    for cell in hex_circle(enemy.pos, radius) {
                        if max_range == 0 || tile.unsigned_distance_to(cell) <= max_range {
                            centers.insert(cell);
                        }
                    }
                }
                centers.into_iter().map(|c| (c, None)).collect()
            }
            AoEShape::Line { .. } => {
                let effective_range = if max_range == 0 { 1 } else { max_range };
                let mut results = Vec::new();
                for dir in EdgeDirection::ALL_DIRECTIONS {
                    let step: Hex = dir.into();
                    for d in 1..=effective_range as i32 {
                        let pos = tile + step * d;
                        if !in_bounds(pos) {
                            break;
                        }
                        results.push((pos, None));
                    }
                }
                results
            }
        };

        for (target_pos, target) in targets {
            out.push(ActionCandidate {
                tile,
                path: path.to_vec(),
                kind: CandidateKind::Cast {
                    ability: ability_id.clone(),
                    target_pos,
                    target,
                },
            });
        }
    }
}

/// Dedup, priority-sort, and truncate to `candidate_budget`. The sort and
/// pinning make sure budget-cap doesn't erase a whole "how to reach X" column
/// and leave the AI believing X is untargetable.
fn dedup_and_order(
    mut candidates: Vec<ActionCandidate>,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    ctx: &UtilityContext,
) -> Vec<ActionCandidate> {
    // Deduplicate by (ability, target, target_pos) for Cast, by tile for
    // MoveOnly — keeping the shortest path in each bucket. target_pos is part
    // of the key so distinct AoE placements (target=None) stay separate.
    candidates.sort_by(|a, b| a.path.len().cmp(&b.path.len()));
    let mut seen_cast: HashSet<(AbilityId, Option<Entity>, Hex)> = HashSet::new();
    let mut seen_move: HashSet<Hex> = HashSet::new();
    candidates.retain(|c| match &c.kind {
        CandidateKind::Cast { ability, target, target_pos } => {
            seen_cast.insert((ability.clone(), *target, *target_pos))
        }
        CandidateKind::MoveOnly => seen_move.insert(c.tile),
    });

    // Priority-aware ordering: sort by (target_priority DESC, path_len ASC).
    // High-priority targets survive budget truncation even on crowded fields;
    // within the same target, shortest path wins.
    let priority_of = |c: &ActionCandidate| -> f32 {
        c.target()
            .and_then(|t| snap.unit(t))
            .filter(|u| u.team != active.team) // allies use team-neutral priority
            .map(|u| target_priority(active, u, snap))
            .unwrap_or(0.0)
    };
    candidates.sort_by(|a, b| {
        let pa = priority_of(a);
        let pb = priority_of(b);
        pb.partial_cmp(&pa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.len().cmp(&b.path.len()))
    });

    // Per-target guarantee: make sure the shortest-path candidate for every
    // alive enemy survives truncation.
    let budget = ctx.difficulty.candidate_budget.max(1);
    if candidates.len() <= budget {
        return candidates;
    }

    let mut pinned: Vec<usize> = Vec::new();
    let mut seen_targets: HashSet<Entity> = HashSet::new();
    for (i, c) in candidates.iter().enumerate() {
        if let Some(target) = c.target() {
            if seen_targets.insert(target) {
                pinned.push(i);
            }
        }
    }
    let mut kept: Vec<ActionCandidate> = Vec::with_capacity(budget);
    let mut pinned_set: HashSet<usize> = pinned.iter().copied().collect();
    for &i in &pinned {
        if kept.len() < budget {
            kept.push(candidates[i].clone());
        }
    }
    for (i, c) in candidates.iter().enumerate() {
        if kept.len() >= budget { break; }
        if !pinned_set.remove(&i) {
            kept.push(c.clone());
        }
    }
    kept
}

/// Pick top-3 safe reachable tiles by escape map and add MoveOnly candidates.
fn add_move_only_candidates(
    actor_pos: Hex,
    reach: &ReachableMap,
    maps: &InfluenceMaps,
    out: &mut Vec<ActionCandidate>,
) {
    let mut scored: Vec<(Hex, f32)> = reach
        .destinations
        .iter()
        .filter(|&&h| h != actor_pos)
        .map(|&h| (h, maps.escape.get(h)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (tile, _) in scored.into_iter().take(3) {
        let Some(path) = reach.path_to(tile) else { continue };
        if path.is_empty() {
            continue;
        }
        out.push(ActionCandidate { tile, path, kind: CandidateKind::MoveOnly });
    }
}

// ── Diverse tile selection ──────────────────────────────────────────────────

/// Pick top-N tiles from `reach.destinations` scored by `f`, insert into `out`.
fn pick_top(
    reach: &ReachableMap,
    n: usize,
    out: &mut HashSet<Hex>,
    f: impl Fn(Hex) -> f32,
) {
    let mut scored: Vec<(Hex, f32)> = reach.destinations.iter().map(|&h| (h, f(h))).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (h, _) in scored.into_iter().take(n) {
        out.insert(h);
    }
}

/// Select tiles using multiple strategies to ensure the candidate pool covers
/// offensive, defensive, focus, AoE and kiting positions — not just globally
/// "best" tiles from position_eval.
pub(super) fn select_diverse_tiles(
    actor_pos: Hex,
    active: &UnitSnapshot,
    ctx: &UtilityContext,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    reach: &ReachableMap,
    enemies: &[&UnitSnapshot],
) -> Vec<Hex> {
    let mut tiles: HashSet<Hex> = HashSet::new();

    // 1. Offensive: tiles near wounded / high-threat enemies.
    pick_top(reach, 3, &mut tiles, |h| maps.opportunity.get(h));

    // 2. Safe: lowest danger, near healers.
    pick_top(reach, 3, &mut tiles, |h| maps.escape.get(h));

    // 3. Near priority target: closest tiles to the highest-priority enemy.
    if let Some(priority) = enemies.iter().max_by(|a, b| {
        target_priority(active, a, snap)
            .partial_cmp(&target_priority(active, b, snap))
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        pick_top(reach, 2, &mut tiles, |h| {
            -(h.unsigned_distance_to(priority.pos) as f32)
        });
    }

    // 4. AoE origin: tiles from which AoE hits the most enemies.
    if active.tags.contains(AiTags::HAS_AOE) {
        let aoe_radii: Vec<u32> = ctx.abilities.0.iter()
            .filter_map(|id| ctx.content.abilities.get(id))
            .filter_map(|def| match def.aoe {
                AoEShape::Circle { radius } => Some(radius),
                _ => None,
            })
            .collect();

        if !aoe_radii.is_empty() {
            let enemy_positions: HashSet<Hex> = enemies.iter().map(|e| e.pos).collect();
            pick_top(reach, 2, &mut tiles, |h| {
                aoe_radii.iter().map(|&r| {
                    hex_circle(h, r).iter()
                        .filter(|c| enemy_positions.contains(c))
                        .count() as f32
                }).fold(0.0f32, f32::max)
            });
        }
    }

    // 5. Retreat-with-LOS: safe tiles that maintain line of sight to an enemy (kiting).
    if active.tags.contains(AiTags::RANGED) {
        let occupied: HashSet<Hex> = snap.units.iter().map(|u| u.pos).collect();
        let enemy_positions: Vec<Hex> = enemies.iter().map(|e| e.pos).collect();
        pick_top(reach, 2, &mut tiles, |h| {
            let can_see = enemy_positions.iter().any(|&ep| {
                has_los(h, ep, |mid| occupied.contains(&mid) && mid != h && mid != ep)
            });
            if can_see { maps.escape.get(h) } else { f32::NEG_INFINITY }
        });
    }

    // 6. Support coverage: tiles within heal range of wounded allies, ranked
    // by escape. Without this strategy, "retreat + heal wounded ally" combos
    // only surfaced when the destination tile happened to top the generic
    // escape list. Explicit pass guarantees such tiles enter the candidate
    // pool even when competing escape tiles score higher overall.
    if active.tags.contains(AiTags::CAN_HEAL) {
        let heal_range: u32 = ctx.abilities.0.iter()
            .filter_map(|id| ctx.content.abilities.get(id))
            .filter(|def| matches!(def.target_type, TargetType::SingleAlly))
            .map(|def| def.range.max)
            .max()
            .unwrap_or(0);
        if heal_range > 0 {
            let wounded: Vec<Hex> = snap
                .allies_of(active.team)
                .filter(|u| u.entity != active.entity)
                .filter(|u| u.hp < u.max_hp)
                .map(|u| u.pos)
                .collect();
            for ally_pos in &wounded {
                pick_top(reach, 2, &mut tiles, |h| {
                    if h.unsigned_distance_to(*ally_pos) <= heal_range {
                        maps.escape.get(h)
                    } else {
                        f32::NEG_INFINITY
                    }
                });
            }
        }
    }

    // 7. Always include current position (stay-and-cast).
    tiles.insert(actor_pos);

    // Deterministic order: HashSet iteration is random, which makes candidate
    // truncation non-deterministic when many candidates share the same path
    // length. Sort by (x, y) so runs with the same state produce the same pool.
    let mut sorted: Vec<Hex> = tiles.into_iter().collect();
    sorted.sort_by_key(|h| (h.x, h.y));
    sorted
}

// ── Resource affordability ──────────────────────────────────────────────────

pub(super) fn can_afford_snap(
    def: &crate::content::abilities::AbilityDef,
    unit: &UnitSnapshot,
) -> bool {
    // AP pool gate.
    if unit.action_points < def.cost_ap {
        return false;
    }
    for cost in &def.costs {
        let available = match cost.resource {
            ResourceKind::Hp => unit.hp,
            ResourceKind::Mana => unit.mana.map(|(cur, _)| cur).unwrap_or(0),
            ResourceKind::Rage => unit.rage.map(|(cur, _)| cur).unwrap_or(0),
            ResourceKind::Energy => unit.energy.map(|(cur, _)| cur).unwrap_or(0),
        };
        if available < cost.amount {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::content_view::ContentView;
    use crate::combat::ai::difficulty::DifficultyProfile;
    use crate::combat::ai::influence::InfluenceMap;
    use crate::combat::ai::role::{AiRole, AxisProfile};
    use crate::content::abilities::CasterContext;
    use crate::content::races::CritFailEffect;
    use crate::game::components::{Abilities, Team};
    use crate::game::hex::{hex_from_offset, hex_to_offset};
    

    fn unit(id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(id).expect("valid entity id"),
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
            abilities: vec!["melee_attack".into()],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
            max_attack_range: 1,
            summoner: None,
            reactions_left: 0,
            aoo_expected_damage: None,
        }
    }

    fn snap(units: Vec<UnitSnapshot>) -> BattleSnapshot {
        let active = units[0].entity;
        BattleSnapshot { units, active_unit: active, round: 1 }
    }

    fn empty_maps() -> InfluenceMaps {
        InfluenceMaps {
            danger: InfluenceMap::new(),
            ally_support: InfluenceMap::new(),
            opportunity: InfluenceMap::new(),
            escape: InfluenceMap::new(),
        }
    }

    fn fake_reach(start: Hex) -> ReachableMap {
        use crate::game::pathfinding::reachable_with_paths;
        reachable_with_paths(start, 20, in_bounds, |_| true)
    }

    fn test_ctx<'a>(
        content: &'a ContentView,
        diff: &'a DifficultyProfile,
        abilities: &'a Abilities,
    ) -> UtilityContext<'a> {
        UtilityContext {
            content,
            difficulty: diff,
            caster: &CasterContext { str_mod: 0, int_mod: 0, spell_power: 0, weapon_dice: None },
            abilities,
            opponent_team: Team::Player,
            crit_fail_effect: CritFailEffect::Miss,
            crit_fail_chance: 0.0,
            blocked_tiles: crate::combat::ai::utility::empty_blocked_tiles(),
        }
    }

    #[test]
    fn diverse_tiles_always_includes_current_pos() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        let enemy = unit(1, Team::Player, hex_from_offset(0, 0));
        let s = snap(vec![active.clone(), enemy]);
        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();
        let reach = fake_reach(actor_pos);
        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        assert!(tiles.contains(&actor_pos), "current position must always be included");
    }

    #[test]
    fn diverse_tiles_near_priority_target() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        let mut target = unit(1, Team::Player, hex_from_offset(2, 3));
        target.hp = 3;
        target.threat = 10.0;

        let s = snap(vec![active.clone(), target.clone()]);
        let maps = empty_maps();
        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();

        let target_hex = hex_from_offset(2, 3);
        let reach = fake_reach(actor_pos);

        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        let has_close = tiles.iter().any(|&h| h.unsigned_distance_to(target_hex) <= 1);
        assert!(
            has_close,
            "should include a tile near priority target; got {:?}",
            tiles.iter().map(|h| hex_to_offset(*h)).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn diverse_tiles_includes_offensive_and_safe() {
        let actor_pos = hex_from_offset(4, 3);
        let active = unit(0, Team::Enemy, actor_pos);
        let enemy = unit(1, Team::Player, hex_from_offset(1, 1));

        let s = snap(vec![active.clone(), enemy]);

        let offensive = hex_from_offset(3, 2);
        let safe = hex_from_offset(5, 4);
        let mut maps = empty_maps();
        maps.opportunity.add(offensive, 0.9);
        maps.escape.add(safe, 0.9);

        let content = ContentView::load_global_for_tests();
        let difficulty = DifficultyProfile::default();
        let abilities = Abilities(vec!["melee_attack".into()]);
        let ctx = test_ctx(&content, &difficulty, &abilities);
        let enemies: Vec<&UnitSnapshot> = s.enemies_of(Team::Enemy).collect();
        let reach = fake_reach(actor_pos);

        let tiles = select_diverse_tiles(actor_pos, &active, &ctx, &s, &maps, &reach, &enemies);
        assert!(tiles.contains(&offensive), "offensive tile should be included");
        assert!(tiles.contains(&safe), "safe tile should be included");
    }
}
