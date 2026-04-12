pub const GRID_COLS: i32 = 8;
pub const GRID_ROWS: i32 = 7;

/// Odd rows are one cell longer (protrude on both sides); even rows have GRID_COLS-1 cells.
pub fn row_cols(r: i32) -> i32 {
    if r & 1 == 0 { GRID_COLS - 1 } else { GRID_COLS }
}

pub fn in_bounds(q: i32, r: i32) -> bool {
    (0..GRID_ROWS).contains(&r) && (0..row_cols(r)).contains(&q)
}

/// Convert even-r offset (col, row) to cube coordinates.
/// Our convention: even rows shift right by 0.5, so q_cube = q - (r + (r & 1)) / 2.
fn to_cube(q: i32, r: i32) -> (i32, i32, i32) {
    let cq = q - (r + (r & 1)) / 2;
    let cr = r;
    (cq, cr, -cq - cr)
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
