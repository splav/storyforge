pub use hexx::Hex;
use hexx::{HexLayout, HexOrientation, OffsetHexMode};

use bevy::math::Vec2;
use std::sync::LazyLock;

pub const GRID_COLS: i32 = 8;
pub const GRID_ROWS: i32 = 7;
pub const HEX_SIZE: f32 = 34.0;

const OFFSET_MODE: OffsetHexMode = OffsetHexMode::Even;
const ORIENTATION: HexOrientation = HexOrientation::Pointy;

/// Cached HexLayout: pointy-top, Y inverted, origin-shifted to match even-r grid.
pub static LAYOUT: LazyLock<HexLayout> = LazyLock::new(|| {
    let mut l = HexLayout::pointy()
        .with_hex_size(HEX_SIZE)
        .with_origin(Vec2::new(HEX_SIZE * 3.0_f32.sqrt() * 0.5, 0.0));
    l.invert_y();
    l
});

/// Convert even-r offset (col, row) to axial Hex.
pub fn hex_from_offset(col: i32, row: i32) -> Hex {
    Hex::from_offset_coordinates([col, row], OFFSET_MODE, ORIENTATION)
}

/// Convert axial Hex to even-r offset (col, row).
pub fn hex_to_offset(hex: Hex) -> [i32; 2] {
    hex.to_offset_coordinates(OFFSET_MODE, ORIENTATION)
}

/// Odd rows are one cell longer (protrude on both sides); even rows have GRID_COLS-1 cells.
pub fn row_cols(r: i32) -> i32 {
    if r & 1 == 0 { GRID_COLS - 1 } else { GRID_COLS }
}

pub fn in_bounds(hex: Hex) -> bool {
    let [q, r] = hex_to_offset(hex);
    (0..GRID_ROWS).contains(&r) && (0..row_cols(r)).contains(&q)
}

/// All in-bounds cells within hex-distance ≤ radius from center.
pub fn hex_circle(center: Hex, radius: u32) -> Vec<Hex> {
    center
        .range(radius)
        .filter(|&h| in_bounds(h))
        .collect()
}

/// Line of `length` cells starting at `target` and extending in the direction
/// `from → target`. Returns up to `length` in-bounds cells.
pub fn hex_line(from: Hex, target: Hex, length: u32) -> Vec<Hex> {
    if from == target {
        return Vec::new();
    }
    let dir = from.main_direction_to(target);
    let step: Hex = dir.into();
    (0..length as i32)
        .map(|i| target + step * i)
        .take_while(|&h| in_bounds(h))
        .collect()
}

/// Returns `true` if there is an unobstructed line of sight from `from` to `to`.
/// `blocks_los(hex)` should return `true` for cells that block vision.
/// Only intermediate cells are checked — `from` and `to` themselves are not.
pub fn has_los(from: Hex, to: Hex, blocks_los: impl Fn(Hex) -> bool) -> bool {
    if from == to {
        return true;
    }
    let cells: Vec<Hex> = from.line_to(to).collect();
    // Skip first (from) and last (to)
    cells[1..cells.len() - 1].iter().all(|h| !blocks_los(*h))
}

/// All in-bounds cells within hex-distance ≤ `radius` that have LOS from `from`.
pub fn visible_cells(
    from: Hex,
    radius: u32,
    blocks_los: impl Fn(Hex) -> bool,
) -> Vec<Hex> {
    hex_circle(from, radius)
        .into_iter()
        .filter(|&h| has_los(from, h, &blocks_los))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_radius_0_is_center_only() {
        let c = hex_from_offset(3, 3);
        let cells = hex_circle(c, 0);
        assert_eq!(cells, vec![c]);
    }

    #[test]
    fn circle_radius_1_has_7_cells_in_center() {
        let c = hex_from_offset(3, 3);
        let cells = hex_circle(c, 1);
        assert_eq!(cells.len(), 7);
        assert!(cells.contains(&c));
        for &h in &cells {
            assert!(c.unsigned_distance_to(h) <= 1, "{h:?} too far");
        }
    }

    #[test]
    fn circle_clips_to_bounds() {
        let c = hex_from_offset(0, 0);
        let cells = hex_circle(c, 1);
        assert!(cells.len() < 7);
        for &h in &cells {
            assert!(in_bounds(h));
        }
    }

    #[test]
    fn offset_roundtrips() {
        for r in 0..GRID_ROWS {
            for q in 0..row_cols(r) {
                let hex = hex_from_offset(q, r);
                let [oq, or] = hex_to_offset(hex);
                assert_eq!((oq, or), (q, r), "roundtrip ({q},{r})");
            }
        }
    }

    #[test]
    fn line_adjacent_produces_two_cells() {
        let from = hex_from_offset(3, 3);
        let target = hex_from_offset(3, 2);
        let cells = hex_line(from, target, 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], target);
        assert_eq!(from.unsigned_distance_to(cells[1]), 2);
    }

    #[test]
    fn line_non_adjacent_normalizes_direction() {
        let from = hex_from_offset(3, 3);
        let target = hex_from_offset(3, 1);
        let cells = hex_line(from, target, 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], target);
        assert_eq!(from.unsigned_distance_to(cells[1]), 3);
    }

    #[test]
    fn line_same_cell_returns_empty() {
        let c = hex_from_offset(3, 3);
        let cells = hex_line(c, c, 2);
        assert!(cells.is_empty());
    }

    #[test]
    fn pixel_roundtrip() {
        for r in 0..GRID_ROWS {
            for q in 0..row_cols(r) {
                let hex = hex_from_offset(q, r);
                let pixel = LAYOUT.hex_to_world_pos(hex);
                let back = LAYOUT.world_pos_to_hex(pixel);
                assert_eq!(back, hex, "pixel roundtrip ({q},{r})");
            }
        }
    }

    // ── LOS tests ────────────────────────────────────────────────────────

    #[test]
    fn los_to_self_is_true() {
        let c = hex_from_offset(3, 3);
        assert!(has_los(c, c, |_| false));
    }

    #[test]
    fn los_to_neighbor_no_blockers() {
        let a = hex_from_offset(3, 3);
        let b = hex_from_offset(4, 3);
        assert!(has_los(a, b, |_| false));
    }

    #[test]
    fn los_blocked_by_intermediate_cell() {
        let a = hex_from_offset(1, 0);
        let c = hex_from_offset(3, 0);
        let blocker = hex_from_offset(2, 0);
        assert!(!has_los(a, c, |h| h == blocker));
    }

    #[test]
    fn los_not_blocked_by_endpoints() {
        let a = hex_from_offset(1, 0);
        let b = hex_from_offset(3, 0);
        // Blocking only endpoints should not affect LOS
        assert!(has_los(a, b, |h| h == a || h == b));
    }

    #[test]
    fn visible_cells_no_blockers_equals_circle() {
        let c = hex_from_offset(3, 3);
        let circle = hex_circle(c, 2);
        let visible = visible_cells(c, 2, |_| false);
        assert_eq!(visible.len(), circle.len());
    }

    #[test]
    fn visible_cells_with_blocker_hides_cells_behind() {
        let c = hex_from_offset(3, 3);
        // Blocker is at distance 1 — it's visible but cells behind it are not
        let blocker = hex_from_offset(4, 3);
        let circle = hex_circle(c, 2);
        let visible = visible_cells(c, 2, |h| h == blocker);
        assert!(visible.len() < circle.len());
        // The blocker itself is visible (it's a target, not intermediate)
        assert!(visible.contains(&blocker));
    }
}
