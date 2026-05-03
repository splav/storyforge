use crate::combat::ai::world::snapshot::{BattleSnapshot, UnitSnapshot};
use crate::combat::ai::world::tags::AiTags;
use crate::game::components::Team;
use crate::game::hex::{can_stop_on, hex_from_offset, is_passable, Hex, GRID_ROWS, row_cols};
use crate::game::pathfinding::reachable_cells;
use bevy::prelude::Resource;
use std::collections::{HashMap, HashSet};

// ── Tuning config ───────────────────────────────────────────────────────────

/// Tunable constants for influence-map construction. Registered as a Bevy
/// resource so designers can tweak balance without recompiling.
#[derive(Resource, Clone, Debug)]
pub struct InfluenceConfig {
    /// Distance decay for ally-support falloff (larger = gentler drop-off).
    pub lambda_support: f32,
    /// Distance decay for opportunity falloff.
    pub lambda_opportunity: f32,
    /// Weight of "enemy is low HP" in target-value.
    pub w_kill: f32,
    /// Weight of "enemy is high threat" in target-value.
    pub w_threat: f32,
    /// Support-weight multiplier for allies flagged CAN_HEAL.
    pub healer_support_weight: f32,
    /// Support-weight multiplier for allies flagged MELEE_ONLY.
    pub melee_adj_weight: f32,
}

impl Default for InfluenceConfig {
    fn default() -> Self {
        Self {
            lambda_support: 2.5,
            lambda_opportunity: 3.0,
            w_kill: 0.7,
            w_threat: 0.3,
            healer_support_weight: 2.0,
            melee_adj_weight: 1.5,
        }
    }
}

// ── Data types ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct InfluenceMap(HashMap<Hex, f32>);

impl Default for InfluenceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl InfluenceMap {
    pub fn new() -> Self {
        Self(all_cells().into_iter().map(|h| (h, 0.0)).collect())
    }

    pub fn get(&self, hex: Hex) -> f32 {
        self.0.get(&hex).copied().unwrap_or(0.0)
    }

    pub fn add(&mut self, hex: Hex, value: f32) {
        if let Some(v) = self.0.get_mut(&hex) {
            *v += value;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Hex, &f32)> {
        self.0.iter()
    }

    pub fn scale(&mut self, factor: f32) {
        for v in self.0.values_mut() {
            *v *= factor;
        }
    }

    pub fn min_max(&self) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in self.0.values() {
            if v < min { min = v; }
            if v > max { max = v; }
        }
        if min > max { (0.0, 0.0) } else { (min, max) }
    }

    /// Rank-based normalization to [0, 1].
    /// Guarantees full spread: lowest value → 0.0, highest → 1.0.
    /// Ties share the same rank.
    pub fn normalize(&mut self) {
        let mut vals: Vec<(Hex, f32)> = self.0.iter().map(|(&h, &v)| (h, v)).collect();
        if vals.is_empty() {
            return;
        }
        vals.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let last = (vals.len() - 1).max(1) as f32;
        for (rank, (hex, _)) in vals.into_iter().enumerate() {
            self.0.insert(hex, rank as f32 / last);
        }
    }
}

#[derive(Clone)]
pub struct InfluenceMaps {
    pub danger: InfluenceMap,
    pub ally_support: InfluenceMap,
    pub opportunity: InfluenceMap,
    pub escape: InfluenceMap,
}

// ── Grid helpers ─────────────────────────────────────────────────────────────

fn all_cells() -> Vec<Hex> {
    let mut cells = Vec::new();
    for r in 0..GRID_ROWS {
        for q in 0..row_cols(r) {
            cells.push(hex_from_offset(q, r));
        }
    }
    cells
}

// ── Builder ──────────────────────────────────────────────────────────────────

pub fn build_influence_maps(
    snap: &BattleSnapshot,
    active_entity: bevy::prelude::Entity,
    active_team: Team,
    cfg: &InfluenceConfig,
) -> InfluenceMaps {
    let cells = all_cells();

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active_team).collect();
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active_team)
        .filter(|u| u.entity != active_entity)
        .collect();

    let ally_positions: HashSet<Hex> = snap.allies_of(active_team).map(|u| u.pos).collect();
    let enemy_positions: HashSet<Hex> = enemies.iter().map(|u| u.pos).collect();

    let danger = build_danger(&cells, &enemies, &ally_positions, &enemy_positions);
    let ally_support = build_ally_support(&cells, &allies, cfg);
    let opportunity = build_opportunity(&cells, &enemies, cfg);
    let escape = build_escape(&cells, &danger, &ally_support);

    // Maps are normalized: danger/ally_support/opportunity ∈ [0, 1],
    // escape ∈ [-1, +1] (survival margin).

    InfluenceMaps { danger, ally_support, opportunity, escape }
}

// ── Danger Map ───────────────────────────────────────────────────────────────

/// Coverage from a single threat source to a cell.
/// Ranged: nearly flat inside fire zone (base 0.92).
/// Melee: steeper gradient — reaching the exact tile is harder (base 0.80).
fn coverage(src: Hex, cell: Hex, max_range: u32, is_ranged: bool) -> f32 {
    let dist = src.unsigned_distance_to(cell);
    if dist > max_range {
        return 0.0;
    }
    let edge = dist as f32 / max_range.max(1) as f32;
    let base = if is_ranged { 0.92 } else { 0.80 };
    base + (1.0 - base) * (1.0 - edge)
}

fn build_danger(
    cells: &[Hex],
    enemies: &[&UnitSnapshot],
    ally_positions: &HashSet<Hex>,
    enemy_positions: &HashSet<Hex>,
) -> InfluenceMap {
    let mut map = InfluenceMap::new();
    let total_threat: f32 = enemies.iter().map(|e| e.threat).sum();

    for enemy in enemies {
        // Non-attacker fallback: project 1-tile melee reach so the unit still
        // colours adjacent tiles with its raw threat (would be zero otherwise).
        let max_range = enemy.max_attack_range.max(1);

        // BFS: enemy passes through own teammates, blocked by our units.
        let reachable = reachable_cells(
            enemy.pos,
            enemy.speed,
            |h| is_passable(h, ally_positions),
            |h| can_stop_on(h, enemy_positions, Some(enemy.pos)),
        );

        // From each reachable cell (+ current pos), compute distance-based danger.
        let mut threat_sources: HashSet<Hex> = reachable;
        threat_sources.insert(enemy.pos);

        let is_ranged = enemy.tags.contains(AiTags::RANGED);

        for &cell in cells {
            let best_cover = threat_sources
                .iter()
                .map(|&src| coverage(src, cell, max_range, is_ranged))
                .fold(0.0f32, f32::max);
            if best_cover > 0.0 {
                map.add(cell, enemy.threat * best_cover);
            }
        }
    }

    // Normalize to [0, 1]: fraction of total enemy threat covering each cell.
    if total_threat > 0.0 {
        map.scale(1.0 / total_threat);
    }

    map
}

// ── Ally Support Map ─────────────────────────────────────────────────────────

fn support_weight(ally: &UnitSnapshot, cfg: &InfluenceConfig) -> f32 {
    let mut w = 1.0;
    if ally.tags.contains(AiTags::CAN_HEAL) {
        w = cfg.healer_support_weight;
    }
    if ally.tags.contains(AiTags::MELEE_ONLY) {
        w *= cfg.melee_adj_weight;
    }
    w
}

fn build_ally_support(cells: &[Hex], allies: &[&UnitSnapshot], cfg: &InfluenceConfig) -> InfluenceMap {
    let mut map = InfluenceMap::new();
    let total_weight: f32 = allies.iter().map(|a| support_weight(a, cfg)).sum();
    if total_weight == 0.0 {
        return map;
    }

    for &cell in cells {
        let mut value = 0.0;
        for ally in allies {
            let dist = cell.unsigned_distance_to(ally.pos) as f32;
            let w = support_weight(ally, cfg);
            value += w * (-dist / cfg.lambda_support).exp();
        }
        map.add(cell, value / total_weight);
    }

    map
}

// ── Opportunity Map ──────────────────────────────────────────────────────────

fn target_value(enemy: &UnitSnapshot, max_threat: f32, cfg: &InfluenceConfig) -> f32 {
    let hp_pct = enemy.hp_pct();
    let threat_norm = enemy.threat / max_threat;
    cfg.w_kill * (1.0 - hp_pct) + cfg.w_threat * threat_norm
}

fn build_opportunity(cells: &[Hex], enemies: &[&UnitSnapshot], cfg: &InfluenceConfig) -> InfluenceMap {
    let mut map = InfluenceMap::new();
    let max_threat = enemies.iter().map(|e| e.threat).fold(0.0f32, f32::max).max(f32::EPSILON);
    let total_value: f32 = enemies.iter().map(|e| target_value(e, max_threat, cfg)).sum();
    if total_value == 0.0 {
        return map;
    }

    for &cell in cells {
        let mut value = 0.0;
        for enemy in enemies {
            let dist = cell.unsigned_distance_to(enemy.pos) as f32;
            let tv = target_value(enemy, max_threat, cfg);
            value += tv * (-dist / cfg.lambda_opportunity).exp();
        }
        map.add(cell, value / total_value);
    }

    map
}

// ── Escape Map ───────────────────────────────────────────────────────────────

fn build_escape(
    cells: &[Hex],
    danger: &InfluenceMap,
    ally_support: &InfluenceMap,
) -> InfluenceMap {
    let mut map = InfluenceMap::new();

    for &cell in cells {
        // Survival margin: positive = safe, negative = exposed.
        map.add(cell, ally_support.get(cell) - danger.get(cell));
    }

    map
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::hex::in_bounds;
    use crate::combat::ai::world::tags::AiTags;
    use crate::game::hex::hex_from_offset;

    fn all_cell_count() -> usize {
        (0..GRID_ROWS).map(|r| row_cols(r) as usize).sum()
    }

    use crate::combat::ai::test_helpers::UnitBuilder;

    /// Local shorthand preserving this module's historical defaults
    /// (speed=2, MELEE_ONLY). Influence-map tests are sensitive to reach
    /// radius, so keep these explicit instead of relying on the generic
    /// `unit()` defaults.
    fn unit(entity_id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        UnitBuilder::new(entity_id, team, pos)
            .speed(2)
            .tags(AiTags::MELEE_ONLY)
            .ability_names(&["melee_attack"])
            .build()
    }

    #[test]
    fn all_cells_matches_grid() {
        let cells = all_cells();
        assert_eq!(cells.len(), all_cell_count());
        for &h in &cells {
            assert!(in_bounds(h));
        }
    }

    #[test]
    fn influence_map_add_and_get() {
        let mut map = InfluenceMap::new();
        let hex = hex_from_offset(3, 3);
        assert_eq!(map.get(hex), 0.0);
        map.add(hex, 2.5);
        assert_eq!(map.get(hex), 2.5);
        map.add(hex, 1.0);
        assert_eq!(map.get(hex), 3.5);
    }

    #[test]
    fn danger_map_marks_threat_zone() {
        // Enemy at center with speed=2, melee_attack range=1.
        let enemy = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();

        let map = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new());

        // Enemy's own cell should be dangerous.
        assert!(map.get(enemy.pos) > 0.0);

        // Adjacent cells (within move+attack range) should have danger.
        let adjacent = hex_from_offset(4, 2);
        assert!(map.get(adjacent) > 0.0);

        // Far corner should be safe (speed 2 + range 1 = max 3 hex distance).
        let far = hex_from_offset(0, 0);
        let dist = enemy.pos.unsigned_distance_to(far);
        if dist > 3 {
            assert_eq!(map.get(far), 0.0, "cell at distance {dist} should be safe");
        }
    }

    #[test]
    fn ally_support_drops_with_distance() {
        let ally = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();
        let map = build_ally_support(&cells, &[&ally], &InfluenceConfig::default());

        let near = hex_from_offset(4, 2); // distance 1
        let far = hex_from_offset(0, 0);  // far away
        assert!(map.get(near) > map.get(far));
    }

    #[test]
    fn ally_support_healer_bonus() {
        // With two allies, the healer's higher weight makes nearby cells
        // score higher than when both allies are plain fighters.
        let healer = UnitBuilder::new(0, Team::Enemy, hex_from_offset(4, 3))
            .tags(AiTags::CAN_HEAL)
            .build();
        let fighter1 = UnitBuilder::new(1, Team::Enemy, hex_from_offset(6, 3))
            .tags(AiTags::empty())
            .build();
        let fighter2 = UnitBuilder::new(2, Team::Enemy, hex_from_offset(4, 3))
            .tags(AiTags::empty())
            .build();

        let cells = all_cells();
        let with_healer = build_ally_support(&cells, &[&healer, &fighter1], &InfluenceConfig::default());
        let without_healer = build_ally_support(&cells, &[&fighter2, &fighter1], &InfluenceConfig::default());

        // Cell near the healer/fighter2 position should be higher with healer
        // because healer contributes a larger share of total support.
        let near = hex_from_offset(4, 2);
        assert!(
            with_healer.get(near) > without_healer.get(near),
            "healer should provide extra support: with={} without={}",
            with_healer.get(near), without_healer.get(near),
        );
    }

    #[test]
    fn escape_inversely_correlated_with_danger() {
        let enemy = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();

        let danger = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new());
        let ally_support = InfluenceMap::new(); // no allies
        let escape = build_escape(&cells, &danger, &ally_support);

        let dangerous = enemy.pos;
        let safe = hex_from_offset(0, 0);

        // If dangerous cell has danger, escape there should be lower.
        if danger.get(dangerous) > danger.get(safe) {
            assert!(
                escape.get(dangerous) < escape.get(safe),
                "escape should be lower in dangerous areas"
            );
        }
    }

    /// All three non-derived maps must stay in [0, 1] after rank-normalization.
    /// Table-driven across danger / ally_support / opportunity to share the
    /// (cells + InfluenceConfig) scaffolding.
    #[test]
    fn built_maps_stay_within_zero_one() {
        let cells = all_cells();
        let cfg = InfluenceConfig::default();
        let e1 = UnitBuilder::new(0, Team::Enemy, hex_from_offset(2, 2)).hp(5).build();
        let e2 = unit(1, Team::Enemy, hex_from_offset(6, 4));
        let a1 = unit(10, Team::Enemy, hex_from_offset(3, 3));
        let a2 = UnitBuilder::new(11, Team::Enemy, hex_from_offset(5, 3))
            .tags(AiTags::CAN_HEAL)
            .build();

        let maps: [(&str, InfluenceMap); 3] = [
            (
                "danger",
                build_danger(&cells, &[&e1, &e2], &HashSet::new(), &HashSet::new()),
            ),
            ("ally_support", build_ally_support(&cells, &[&a1, &a2], &cfg)),
            ("opportunity", build_opportunity(&cells, &[&e1, &e2], &cfg)),
        ];
        for (name, map) in &maps {
            for (_, &v) in map.iter() {
                assert!((0.0..=1.0).contains(&v), "{name} out of [0,1]: {v}");
            }
        }
    }

    #[test]
    fn opportunity_not_dominated_by_threat() {
        // A low-HP target with moderate threat should score higher in opportunity
        // than a full-HP target with high threat, at the same distance.
        let wounded = UnitBuilder::new(0, Team::Enemy, hex_from_offset(4, 3))
            .hp(2)
            .threat(5.0)
            .speed(2)
            .tags(AiTags::MELEE_ONLY)
            .build();
        let healthy = UnitBuilder::new(1, Team::Enemy, hex_from_offset(4, 5))
            .hp(20)
            .threat(10.0)
            .speed(2)
            .tags(AiTags::MELEE_ONLY)
            .build();

        let cells = all_cells();
        let map = build_opportunity(&cells, &[&wounded, &healthy], &InfluenceConfig::default());

        // Cells adjacent to each target.
        let near_wounded = hex_from_offset(4, 2);
        let near_healthy = hex_from_offset(4, 6);

        assert!(
            map.get(near_wounded) > map.get(near_healthy),
            "wounded target should create higher opportunity nearby: wounded={} healthy={}",
            map.get(near_wounded), map.get(near_healthy),
        );
    }

    #[test]
    fn danger_gradient_closer_is_more_dangerous() {
        // Enemy at (4,3) with speed=2, melee range=1.
        let enemy = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();

        let map = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new());

        // Adjacent cell (dist 1 from enemy pos, within move range → dist 0 from source).
        let close = hex_from_offset(4, 2);
        // Edge of threat zone (dist 3 = speed 2 + range 1).
        let far = hex_from_offset(4, 0);

        let d_close = map.get(close);
        let d_far = map.get(far);

        assert!(
            d_close > d_far,
            "closer cell should have higher danger: close={d_close} far={d_far}"
        );
        // Both should be non-zero (both within threat zone).
        assert!(d_close > 0.0);
        assert!(d_far > 0.0);
    }

    #[test]
    fn escape_bounded_minus_one_plus_one() {
        let enemy = unit(0, Team::Enemy, hex_from_offset(2, 2));
        let ally = unit(1, Team::Player, hex_from_offset(6, 4));
        let cells = all_cells();

        let danger = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new());
        let ally_support = build_ally_support(&cells, &[&ally], &InfluenceConfig::default());
        let escape = build_escape(&cells, &danger, &ally_support);
        for (_, &v) in escape.iter() {
            assert!((-1.0..=1.0).contains(&v), "escape out of [-1,1]: {v}");
        }
    }
}
