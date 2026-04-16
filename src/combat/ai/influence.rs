use crate::combat::ai::snapshot::{BattleSnapshot, UnitSnapshot, AiTags};
use crate::content::abilities::TargetType;
use crate::game::components::Team;
use crate::game::hex::{hex_circle, hex_from_offset, in_bounds, Hex, GRID_ROWS, row_cols};
use crate::game::pathfinding::reachable_cells;
use crate::game::resources::GameDb;
use std::collections::{HashMap, HashSet};

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

    pub fn min_max(&self) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in self.0.values() {
            if v < min { min = v; }
            if v > max { max = v; }
        }
        if min > max { (0.0, 0.0) } else { (min, max) }
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
    active_team: Team,
    db: &GameDb,
) -> InfluenceMaps {
    let cells = all_cells();

    let enemies: Vec<&UnitSnapshot> = snap.enemies_of(active_team).collect();
    let allies: Vec<&UnitSnapshot> = snap
        .allies_of(active_team)
        .filter(|u| u.entity != snap.active_unit)
        .collect();

    let ally_positions: HashSet<Hex> = snap.allies_of(active_team).map(|u| u.pos).collect();
    let enemy_positions: HashSet<Hex> = enemies.iter().map(|u| u.pos).collect();

    let danger = build_danger(&cells, &enemies, &ally_positions, &enemy_positions, db);
    let ally_support = build_ally_support(&cells, &allies);
    let opportunity = build_opportunity(&cells, &enemies);
    let escape = build_escape(&cells, &danger, &allies);

    InfluenceMaps { danger, ally_support, opportunity, escape }
}

// ── Danger Map ───────────────────────────────────────────────────────────────

fn build_danger(
    cells: &[Hex],
    enemies: &[&UnitSnapshot],
    ally_positions: &HashSet<Hex>,
    enemy_positions: &HashSet<Hex>,
    db: &GameDb,
) -> InfluenceMap {
    let mut map = InfluenceMap::new();

    for enemy in enemies {
        let max_range = max_attack_range(enemy, db);

        // BFS: enemy passes through own teammates, blocked by our units.
        let reachable = reachable_cells(
            enemy.pos,
            enemy.speed,
            |h| in_bounds(h) && !ally_positions.contains(&h),
            |h| !enemy_positions.contains(&h) || h == enemy.pos,
        );

        // From each reachable cell (+ current pos), expand by attack range.
        let mut threat_sources: HashSet<Hex> = reachable;
        threat_sources.insert(enemy.pos);

        let mut threatened: HashSet<Hex> = HashSet::new();
        for src in &threat_sources {
            for cell in hex_circle(*src, max_range) {
                threatened.insert(cell);
            }
        }

        for &cell in cells {
            if threatened.contains(&cell) {
                map.add(cell, enemy.threat);
            }
        }
    }

    map
}

fn max_attack_range(unit: &UnitSnapshot, db: &GameDb) -> u32 {
    unit.abilities
        .iter()
        .filter_map(|id| db.abilities.get(id))
        .filter(|def| def.target_type == TargetType::SingleEnemy)
        .map(|def| def.range.max)
        .max()
        .unwrap_or(1)
}

// ── Ally Support Map ─────────────────────────────────────────────────────────

fn build_ally_support(cells: &[Hex], allies: &[&UnitSnapshot]) -> InfluenceMap {
    let mut map = InfluenceMap::new();

    for &cell in cells {
        for ally in allies {
            let dist = cell.unsigned_distance_to(ally.pos) as f32;
            map.add(cell, 1.0 / (1.0 + dist));

            if ally.tags.contains(AiTags::CAN_HEAL) {
                map.add(cell, 2.0 / (1.0 + dist));
            }

            if ally.tags.contains(AiTags::MELEE_ONLY) && dist <= 1.0 {
                map.add(cell, 0.5);
            }
        }
    }

    map
}

// ── Opportunity Map ──────────────────────────────────────────────────────────

fn build_opportunity(cells: &[Hex], enemies: &[&UnitSnapshot]) -> InfluenceMap {
    let mut map = InfluenceMap::new();

    for &cell in cells {
        for enemy in enemies {
            let dist = cell.unsigned_distance_to(enemy.pos) as f32;
            let hp_pct = enemy.hp as f32 / enemy.max_hp.max(1) as f32;

            // Low-HP targets are more attractive.
            map.add(cell, (1.0 - hp_pct) / (1.0 + dist));

            // High-threat targets are worth approaching.
            map.add(cell, enemy.threat * 0.1 / (1.0 + dist));
        }
    }

    map
}

// ── Escape Map ───────────────────────────────────────────────────────────────

fn build_escape(
    cells: &[Hex],
    danger: &InfluenceMap,
    allies: &[&UnitSnapshot],
) -> InfluenceMap {
    let mut map = InfluenceMap::new();

    for &cell in cells {
        // Invert danger.
        map.add(cell, -danger.get(cell));

        // Bonus for proximity to healers.
        for ally in allies {
            if ally.tags.contains(AiTags::CAN_HEAL) {
                let dist = cell.unsigned_distance_to(ally.pos) as f32;
                map.add(cell, 1.5 / (1.0 + dist));
            }
        }
    }

    map
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::ai::snapshot::AiTags;
    use crate::game::hex::hex_from_offset;

    fn all_cell_count() -> usize {
        (0..GRID_ROWS).map(|r| row_cols(r) as usize).sum()
    }

    fn unit(entity_id: u32, team: Team, pos: Hex) -> UnitSnapshot {
        use crate::combat::ai::role::AiRole;
        UnitSnapshot {
            entity: bevy::prelude::Entity::from_raw_u32(entity_id)
                .expect("valid entity id"),
            team,
            role: AiRole::Bruiser,
            pos,
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action: true,
            movement: true,
            speed: 2,
            mana: None,
            rage: None,
            energy: None,
            abilities: vec!["melee_attack".into()],
            statuses: vec![],
            threat: 5.0,
            tags: AiTags::MELEE_ONLY,
        }
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
        let db = GameDb::default();
        // Enemy at center with speed=2, melee_attack range=1.
        let enemy = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();

        let map = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new(), &db);

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
        let map = build_ally_support(&cells, &[&ally]);

        let near = hex_from_offset(4, 2); // distance 1
        let far = hex_from_offset(0, 0);  // far away
        assert!(map.get(near) > map.get(far));
    }

    #[test]
    fn ally_support_healer_bonus() {
        let mut healer = unit(0, Team::Enemy, hex_from_offset(4, 3));
        healer.tags = AiTags::CAN_HEAL;

        let fighter = unit(1, Team::Enemy, hex_from_offset(4, 3));

        let cells = all_cells();
        let with_healer = build_ally_support(&cells, &[&healer]);
        let without_healer = build_ally_support(&cells, &[&fighter]);

        let near = hex_from_offset(4, 2);
        assert!(
            with_healer.get(near) > without_healer.get(near),
            "healer should provide extra support"
        );
    }

    #[test]
    fn escape_inversely_correlated_with_danger() {
        let db = GameDb::default();
        let enemy = unit(0, Team::Enemy, hex_from_offset(4, 3));
        let cells = all_cells();

        let danger = build_danger(&cells, &[&enemy], &HashSet::new(), &HashSet::new(), &db);
        let escape = build_escape(&cells, &danger, &[]);

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
}
