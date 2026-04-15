use bevy::math::Vec2;

pub const GRID_COLS: i32 = 8;
pub const GRID_ROWS: i32 = 7;
pub const HEX_SIZE: f32 = 34.0;

/// Pointy-top hex, even rows shift right 0.5 → pixel position for cell (q, r).
pub fn hex_to_pixel(q: i32, r: i32) -> Vec2 {
    let shift = if r & 1 == 0 { 0.5 } else { 0.0 };
    let x = HEX_SIZE * 3.0_f32.sqrt() * (q as f32 + shift);
    let y = HEX_SIZE * 1.5 * r as f32;
    Vec2::new(x, -y)
}

/// Odd rows are one cell longer (protrude on both sides); even rows have GRID_COLS-1 cells.
pub fn row_cols(r: i32) -> i32 {
    if r & 1 == 0 { GRID_COLS - 1 } else { GRID_COLS }
}

pub fn in_bounds(q: i32, r: i32) -> bool {
    (0..GRID_ROWS).contains(&r) && (0..row_cols(r)).contains(&q)
}

/// Convert even-r offset (col, row) to cube coordinates.
/// Our convention: even rows shift right by 0.5, so q_cube = q - (r + (r & 1)) / 2.
pub fn to_cube(q: i32, r: i32) -> (i32, i32, i32) {
    let cq = q - (r + (r & 1)) / 2;
    let cr = r;
    (cq, cr, -cq - cr)
}

/// Convert cube coordinates back to even-r offset (col, row).
pub fn from_cube(cq: i32, cr: i32) -> (i32, i32) {
    let r = cr;
    let q = cq + (r + (r & 1)) / 2;
    (q, r)
}

/// Shortest distance between two cells in hex steps (ignores obstacles).
pub fn hex_distance(q1: i32, r1: i32, q2: i32, r2: i32) -> i32 {
    let (aq, ar, as_) = to_cube(q1, r1);
    let (bq, br, bs) = to_cube(q2, r2);
    (aq - bq).abs().max((ar - br).abs()).max((as_ - bs).abs())
}

#[cfg(test)]
mod distance_tests {
    use super::*;

    #[test]
    fn same_cell_is_zero() {
        assert_eq!(hex_distance(3, 2, 3, 2), 0);
    }

    #[test]
    fn all_neighbors_are_distance_one() {
        for nb in hex_neighbors(2, 2) {
            assert_eq!(hex_distance(2, 2, nb.0, nb.1), 1, "neighbor {nb:?}");
        }
        // Odd row
        for nb in hex_neighbors(2, 3) {
            assert_eq!(hex_distance(2, 3, nb.0, nb.1), 1, "neighbor {nb:?}");
        }
    }

    #[test]
    fn two_steps_away() {
        // Moving two steps in the same row.
        assert_eq!(hex_distance(0, 0, 2, 0), 2);
        // Two rows down.
        assert_eq!(hex_distance(0, 0, 0, 2), 2);
    }
}

/// All in-bounds cells within hex-distance ≤ radius from (cq, cr).
pub fn hex_circle(cq: i32, cr: i32, radius: u32) -> Vec<(i32, i32)> {
    let r = radius as i32;
    let (cx, cy, _) = to_cube(cq, cr);
    let mut cells = Vec::new();
    for dx in -r..=r {
        let lo = (-r).max(-dx - r);
        let hi = r.min(-dx + r);
        for dy in lo..=hi {
            let (oq, or) = from_cube(cx + dx, cy + dy);
            if in_bounds(oq, or) {
                cells.push((oq, or));
            }
        }
    }
    cells
}

/// The 6 unit cube-coordinate directions.
pub const CUBE_DIRS: [(i32, i32, i32); 6] = [
    ( 1,  0, -1), ( 1, -1,  0), ( 0, -1,  1),
    (-1,  0,  1), (-1,  1,  0), ( 0,  1, -1),
];

/// Nearest unit cube direction from `(fq,fr)` toward `(tq,tr)`.
/// Returns `None` if same cell.
pub fn cube_direction(fq: i32, fr: i32, tq: i32, tr: i32) -> Option<(i32, i32, i32)> {
    let (ax, ay, az) = to_cube(fq, fr);
    let (bx, by, bz) = to_cube(tq, tr);
    let dx = bx - ax;
    let dy = by - ay;
    let dz = bz - az;
    if dx == 0 && dy == 0 {
        return None;
    }
    // Pick the unit direction with smallest angular distance.
    CUBE_DIRS
        .iter()
        .copied()
        .max_by_key(|&(ux, uy, uz)| dx * ux + dy * uy + dz * uz) // dot product
}

/// Line of `length` cells starting at `(tq,tr)` and extending in the direction
/// `(fq,fr) → (tq,tr)`. Returns up to `length` in-bounds cells.
pub fn hex_line(fq: i32, fr: i32, tq: i32, tr: i32, length: u32) -> Vec<(i32, i32)> {
    let Some((ux, uy, _)) = cube_direction(fq, fr, tq, tr) else {
        return Vec::new();
    };
    let (sx, sy, _) = to_cube(tq, tr);
    let mut cells = Vec::new();
    for i in 0..length as i32 {
        let (oq, or) = from_cube(sx + ux * i, sy + uy * i);
        if in_bounds(oq, or) {
            cells.push((oq, or));
        }
    }
    cells
}

/// 6 neighbors for even-r offset (even rows shift right by 0.5).
pub fn hex_neighbors(q: i32, r: i32) -> [(i32, i32); 6] {
    if r & 1 == 0 {
        [
            (q - 1, r),
            (q + 1, r),
            (q,     r - 1),
            (q + 1, r - 1),
            (q,     r + 1),
            (q + 1, r + 1),
        ]
    } else {
        [
            (q - 1, r),
            (q + 1, r),
            (q - 1, r - 1),
            (q,     r - 1),
            (q - 1, r + 1),
            (q,     r + 1),
        ]
    }
}

#[cfg(test)]
mod area_tests {
    use super::*;

    #[test]
    fn circle_radius_0_is_center_only() {
        let cells = hex_circle(3, 3, 0);
        assert_eq!(cells, vec![(3, 3)]);
    }

    #[test]
    fn circle_radius_1_has_7_cells_in_center() {
        let cells = hex_circle(3, 3, 1);
        assert_eq!(cells.len(), 7);
        assert!(cells.contains(&(3, 3)));
        for &(q, r) in &cells {
            assert!(hex_distance(3, 3, q, r) <= 1, "({q},{r}) too far");
        }
    }

    #[test]
    fn circle_clips_to_bounds() {
        let cells = hex_circle(0, 0, 1);
        assert!(cells.len() < 7);
        for &(q, r) in &cells {
            assert!(in_bounds(q, r));
        }
    }

    #[test]
    fn from_cube_roundtrips_to_cube() {
        for r in 0..GRID_ROWS {
            for q in 0..row_cols(r) {
                let (cx, cy, _) = to_cube(q, r);
                assert_eq!(from_cube(cx, cy), (q, r), "roundtrip ({q},{r})");
            }
        }
    }

    #[test]
    fn line_adjacent_produces_two_cells() {
        // From (3,3) through (3,2): line starts AT (3,2) and extends 1 more step.
        let cells = hex_line(3, 3, 3, 2, 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], (3, 2)); // starts at target
        assert_eq!(hex_distance(3, 3, cells[1].0, cells[1].1), 2);
    }

    #[test]
    fn line_non_adjacent_normalizes_direction() {
        // From (3,3) through (3,1) (distance 2) — direction normalized to unit step.
        // Line of 2 starts at (3,1) and extends 1 more in same direction.
        let cells = hex_line(3, 3, 3, 1, 2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0], (3, 1)); // starts at target
        assert_eq!(hex_distance(3, 3, cells[1].0, cells[1].1), 3);
    }

    #[test]
    fn line_same_cell_returns_empty() {
        let cells = hex_line(3, 3, 3, 3, 2);
        assert!(cells.is_empty());
    }

    #[test]
    fn cube_direction_picks_nearest() {
        // Straight up: (3,3) → (3,1) should give a valid unit direction.
        let dir = cube_direction(3, 3, 3, 1);
        assert!(dir.is_some());
        let (dx, dy, dz) = dir.unwrap();
        assert_eq!(dx.abs() + dy.abs() + dz.abs(), 2); // unit cube vector
    }
}
