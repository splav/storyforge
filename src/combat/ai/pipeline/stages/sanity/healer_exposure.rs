//! Rule 1: non-healer abandoning the team's unguarded healer.
//!
//! Fires once per unguarded healer ally that the active unit was adjacent to
//! but whose destination leaves behind. Each firing produces a separate
//! `SanityHit { rule: HealerExposure, multiplier: 0.5 }`.

use crate::combat::ai::pipeline::stages::sanity::SanityHit;
use crate::combat::ai::world::snapshot::UnitView;
use crate::combat::ai::world::tags::AiTags;
use crate::game::hex::Hex;

use super::SanityRule;

/// Evaluate the HealerExposure rule for one plan.
///
/// Returns one `SanityHit` per unguarded healer that the active unit abandons.
/// Returns an empty `Vec` when the rule does not apply (active is a healer /
/// support, or no healer is being abandoned).
pub(super) fn evaluate(
    active: UnitView<'_>,
    final_pos: Hex,
    allies: &[UnitView<'_>],
) -> Vec<SanityHit> {
    let mut hits = Vec::new();
    if active.cache.role.support >= 0.3 {
        return hits;
    }
    for ally in allies {
        if !ally.cache.tags.contains(AiTags::CAN_HEAL) {
            continue;
        }
        let was_near = active.pos.unsigned_distance_to(ally.pos) <= 1;
        let will_be_far = final_pos.unsigned_distance_to(ally.pos) > 2;
        if was_near && will_be_far {
            let other_guard = allies
                .iter()
                .any(|a| a.entity() != ally.entity() && a.pos.unsigned_distance_to(ally.pos) <= 2);
            if !other_guard {
                hits.push(SanityHit {
                    rule: SanityRule::HealerExposure,
                    multiplier: 0.5,
                });
            }
        }
    }
    hits
}
