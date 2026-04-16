use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{ActionCandidate, CandidateKind};
use crate::content::abilities::{AoEShape, TargetType};
use crate::game::hex::{hex_circle, hex_line};
use crate::game::resources::GameDb;
use std::collections::HashSet;

/// Remove candidates that violate hard constraints.
/// Applied before scoring to prune obviously bad choices.
pub fn filter_candidates(
    candidates: &mut Vec<ActionCandidate>,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    maps: &InfluenceMaps,
    db: &GameDb,
) {
    // Forced targeting: if any enemy taunts, only allow Cast candidates on them.
    // MoveOnly is exempt — taunt doesn't restrict movement.
    let forced: Vec<_> = snap
        .enemies_of(active.team)
        .filter(|u| u.tags.contains(AiTags::FORCES_TARGETING))
        .map(|u| u.entity)
        .collect();

    if !forced.is_empty() {
        candidates.retain(|c| match &c.kind {
            CandidateKind::Cast { target, .. } => forced.contains(target),
            CandidateKind::MoveOnly => true,
        });
    }

    let ally_positions: HashSet<_> = snap.allies_of(active.team).map(|u| u.pos).collect();

    candidates.retain(|c| {
        // Don't walk into death: reject if LOW_HP and the candidate MOVES to
        // a high-danger tile. Applies to both Cast and MoveOnly.
        if active.tags.contains(AiTags::LOW_HP)
            && c.tile != active.pos
            && maps.danger.get(c.tile) > 0.7
        {
            return false;
        }

        // MoveOnly bypasses the remaining ability-specific constraints.
        let (ability, target, target_pos) = match &c.kind {
            CandidateKind::Cast { ability, target, target_pos } => (ability, *target, *target_pos),
            CandidateKind::MoveOnly => return true,
        };

        let Some(def) = db.abilities.get(ability) else {
            return false;
        };

        // Team safety: SingleAlly must target ally, SingleEnemy must target
        // enemy. Defence in depth — engine validation doesn't enforce this.
        match def.target_type {
            TargetType::SingleAlly => {
                let Some(target_unit) = snap.unit(target) else { return false };
                if target_unit.team != active.team { return false; }
            }
            TargetType::SingleEnemy => {
                if let Some(target_unit) = snap.unit(target) {
                    if target_unit.team == active.team { return false; }
                }
            }
            _ => {}
        }

        // Don't AoE allies/self: reject if friendly fire would hit caster or
        // hit more allies than extra enemies justify.
        if def.aoe != AoEShape::None {
            let area: Vec<_> = match def.aoe {
                AoEShape::Circle { radius } => hex_circle(target_pos, radius),
                AoEShape::Line { length } => hex_line(c.tile, target_pos, length),
                AoEShape::None => vec![],
            };
            let area_set: HashSet<_> = area.into_iter().collect();

            if def.friendly_fire {
                let allies_hit = ally_positions.iter().filter(|p| area_set.contains(p)).count();
                let enemies_hit = snap
                    .enemies_of(active.team)
                    .filter(|u| area_set.contains(&u.pos))
                    .count();
                if allies_hit > 0 && enemies_hit < allies_hit * 2 {
                    return false;
                }
            }
        }

        // Don't waste CC on already-stunned target.
        let applies_cc = def
            .statuses
            .iter()
            .any(|sa| db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn));
        if applies_cc {
            if let Some(target_unit) = snap.unit(target) {
                if target_unit.tags.contains(AiTags::IS_STUNNED) {
                    return false;
                }
            }
        }

        // Don't overheal: reject heal on target above 90% HP.
        if def.target_type == TargetType::SingleAlly {
            if let Some(target_unit) = snap.unit(target) {
                let hp_pct = target_unit.hp as f32 / target_unit.max_hp.max(1) as f32;
                if hp_pct > 0.9 {
                    return false;
                }
            }
        }

        true
    });
}
