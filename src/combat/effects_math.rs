//! Pure effect math shared between the live pipeline and the AI sim.
//!
//! No Bevy types. Functions here are the single source of truth for:
//! - final-damage computation (armor, vulnerability, `pierces_armor`, min-1 floor)
//! - AoE cell enumeration (circle / line)
//!
//! Anywhere the live pipeline and `combat::ai::planning::sim` used to derive
//! these independently, they now call through here.

use crate::content::abilities::AoEShape;
use crate::game::hex::{hex_circle, hex_line, Hex};

/// Integer-exact final damage delivered to a target:
///
/// `max(1, raw − (armor unless pierced) + vulnerability)`
///
/// The min-1 floor matches the live pipeline contract: any damage-intent
/// attack that hits leaves at least 1 HP of impact, even vs. heavy armor.
pub fn final_damage_i32(raw: i32, armor: i32, vulnerability: i32, pierces_armor: bool) -> i32 {
    let armor = if pierces_armor { 0 } else { armor };
    (raw - armor + vulnerability).max(1)
}

/// Expected-value variant for the AI sim, which deals in collapsed-dice floats.
/// Same floor semantic — an attack with EV that underruns armor is still
/// expected to land at ≥1 HP, matching what happens in practice.
pub fn final_damage_f32(raw: f32, armor: f32, vulnerability: f32, pierces_armor: bool) -> f32 {
    let armor = if pierces_armor { 0.0 } else { armor };
    (raw - armor + vulnerability).max(1.0)
}

/// Enumerate the hexes an AoE ability touches. `None` returns empty — callers
/// handle single-target cases separately. `actor_pos` is only consulted for
/// line-shaped AoEs (used as the line origin).
pub fn aoe_cells(aoe: AoEShape, actor_pos: Hex, target_pos: Hex) -> Vec<Hex> {
    match aoe {
        AoEShape::None => Vec::new(),
        AoEShape::Circle { radius } => hex_circle(target_pos, radius),
        AoEShape::Line { length } => hex_line(actor_pos, target_pos, length),
    }
}
