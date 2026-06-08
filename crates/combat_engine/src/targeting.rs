//! Engine-side target enumeration for AoE / single-target abilities.
//!
//! Ported from `src/combat/effects_state.rs` in Phase 2 step 5.  The Bevy
//! `compute_affected_targets` becomes a thin Bevy-adapter call into this
//! function once Phase 2 step 6 wires `Action::Cast` through `step()`.
//!
//! Grid topology is **not** baked in here — `aoe_cells` returns the
//! geometric hex set without bounds-clipping; the `TargetState::unit_at_cell`
//! impl returns `None` for any cell that has no unit (whether out-of-bounds
//! or just empty), which is the equivalent filter.

use hexx::Hex;

use crate::content::{AbilityDef, AoEShape};
use crate::state::Team;

/// What `compute_affected_targets` reports per affected unit.
///
/// Generic over `Id` so each backend supplies its native identifier
/// (`Entity` for Bevy adapters, `UnitId` for the engine impl).
#[derive(Clone, Copy, Debug)]
pub struct TargetRef<Id> {
    pub id: Id,
    pub team: Team,
    /// Carried explicitly: both backends keep dead entries in their world
    /// models (Bevy: `Combatant + HexPositions` for corpses; engine:
    /// `CombatState` tombstones with `hp = 0`).  AoE enumeration filters
    /// by this rather than relying on "present ⇒ alive".
    pub alive: bool,
}

/// Read-only adapter trait for targeting enumeration.
///
/// `TargetState` deliberately omits content access — the caller of
/// `compute_affected_targets` already has the `&AbilityDef` in hand.
pub trait TargetState {
    type Id: Copy + Eq;

    /// Actor's hex position; `None` if the actor is gone.
    fn actor_pos(&self, actor: Self::Id) -> Option<Hex>;

    /// Unit occupying `pos`, if any.  Returns `None` for out-of-bounds
    /// cells, empty cells, or anything not-a-unit.
    fn unit_at_cell(&self, pos: Hex) -> Option<TargetRef<Self::Id>>;

    /// Team of a known unit; `None` if the id is unknown.
    fn team_of(&self, id: Self::Id) -> Option<Team>;
}

/// Geometry of an AoE pattern — the hex cells it occupies before
/// occupancy filtering.  Empty for `AoEShape::None`.
pub fn aoe_cells(aoe: AoEShape, actor_pos: Hex, target_pos: Hex) -> Vec<Hex> {
    match aoe {
        AoEShape::None => Vec::new(),
        AoEShape::Circle { radius } => hex_circle(target_pos, radius),
        AoEShape::Line { length } => hex_line(actor_pos, target_pos, length),
    }
}

/// All hexes within `radius` steps of `center` (inclusive).
fn hex_circle(center: Hex, radius: u32) -> Vec<Hex> {
    center.range(radius).collect()
}

/// `length` hexes starting at `target` and extending in the direction
/// `from → target`.  Empty when `from == target` (direction undefined).
fn hex_line(from: Hex, target: Hex, length: u32) -> Vec<Hex> {
    if from == target {
        return Vec::new();
    }
    let dir = from.main_direction_to(target);
    let step: Hex = dir.into();
    (0..length as i32).map(|i| target + step * i).collect()
}

/// Enumerate every unit an ability touches.
///
/// - **Non-AoE**: returns `[primary_target]`.
/// - **AoE**: walks every cell of `aoe_cells`, collects live units, applies
///   friendly-fire rules:
///   - Actor is included only if `def.friendly_fire`.
///   - Allies (same team as actor) are included only if `def.friendly_fire`.
///   - Enemies are always included.
///
/// Mirrors `combat::effects_state::compute_affected_targets` exactly.
pub fn compute_affected_targets<S: TargetState>(
    actor: S::Id,
    def: &AbilityDef,
    primary_target: S::Id,
    target_pos: Hex,
    state: &S,
) -> Vec<S::Id> {
    if matches!(def.aoe, AoEShape::None) {
        return vec![primary_target];
    }

    let actor_pos = state.actor_pos(actor).unwrap_or(Hex::ZERO);
    let actor_team = match state.team_of(actor) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let cells = aoe_cells(def.aoe, actor_pos, target_pos);
    let mut out = Vec::new();
    for cell in cells {
        let Some(r) = state.unit_at_cell(cell) else {
            continue;
        };
        if !r.alive {
            continue;
        }
        if r.id == actor {
            if def.friendly_fire {
                out.push(r.id);
            }
            continue;
        }
        if !def.friendly_fire && r.team == actor_team {
            continue;
        }
        out.push(r.id);
    }
    out
}
