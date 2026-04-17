use crate::content::content_view::ContentView;
use crate::combat::ai::factors::aoe_area;
use crate::combat::ai::influence::InfluenceMaps;
use crate::combat::ai::scoring::applies_cc;
use crate::combat::ai::snapshot::{AiTags, BattleSnapshot, UnitSnapshot};
use crate::combat::ai::utility::{ActionCandidate, CandidateKind};
use crate::content::abilities::{AoEShape, TargetType};
use std::collections::HashSet;

/// Remove candidates that violate hard constraints.
/// Applied before scoring to prune obviously bad choices.
pub fn filter_candidates(
    candidates: &mut Vec<ActionCandidate>,
    active: &UnitSnapshot,
    snap: &BattleSnapshot,
    _maps: &InfluenceMaps,
    content: &ContentView,
) {
    // Forced targeting: taunt constrains ENEMY-targeted Cast only. Heals,
    // self-buffs and movement are untouched — a taunt says "you can't hit
    // anyone but me", not "you can't heal your own ally".
    let forced: Vec<_> = snap
        .enemies_of(active.team)
        .filter(|u| u.tags.contains(AiTags::FORCES_TARGETING))
        .map(|u| u.entity)
        .collect();

    if !forced.is_empty() {
        candidates.retain(|c| match &c.kind {
            CandidateKind::Cast { ability, target, .. } => {
                let Some(def) = content.abilities.get(ability) else { return false };
                match def.target_type {
                    // Single-target enemy ability must aim at the taunter.
                    TargetType::SingleEnemy => match target {
                        Some(t) => forced.contains(t),
                        // AoE (target=None) hits an area — reject under taunt
                        // regardless of whether the taunter is in the area.
                        None => false,
                    },
                    // SingleAlly / Myself: taunt doesn't restrict supporting own side.
                    _ => true,
                }
            }
            CandidateKind::MoveOnly => true,
        });
    }

    let ally_positions: HashSet<_> = snap.allies_of(active.team).map(|u| u.pos).collect();

    candidates.retain(|c| {
        // Survival risk is now a scoring penalty in sanity_adjust (quadratic
        // gradient) — hard filter here cut retreat candidates even when all
        // reachable tiles were dangerous, leaving the AI with no option to flee.

        // MoveOnly bypasses the remaining ability-specific constraints.
        let (ability, target, target_pos) = match &c.kind {
            CandidateKind::Cast { ability, target, target_pos } => (ability, *target, *target_pos),
            CandidateKind::MoveOnly => return true,
        };

        let Some(def) = content.abilities.get(ability) else {
            return false;
        };

        // Team safety: SingleAlly must target ally, SingleEnemy must target
        // enemy. Defence in depth — engine validation doesn't enforce this.
        // AoE (target=None) has no single target — team check is skipped here
        // and handled via the friendly_fire/ally-hit rule below.
        match def.target_type {
            TargetType::SingleAlly => match target.and_then(|t| snap.unit(t)) {
                Some(u) if u.team == active.team => {}
                _ => return false,
            },
            TargetType::SingleEnemy => {
                if let Some(target_unit) = target.and_then(|t| snap.unit(t)) {
                    if target_unit.team == active.team { return false; }
                }
            }
            _ => {}
        }

        // Don't AoE allies/self: reject if friendly fire would hit caster or
        // hit more allies than extra enemies justify.
        if def.aoe != AoEShape::None && def.friendly_fire {
            let area = aoe_area(def, target_pos, c.tile);
            let allies_hit = ally_positions.iter().filter(|p| area.contains(p)).count();
            let enemies_hit = snap
                .enemies_of(active.team)
                .filter(|u| area.contains(&u.pos))
                .count();
            if allies_hit > 0 && enemies_hit < allies_hit * 2 {
                return false;
            }
        }

        // Don't waste single-target CC on already-stunned target. AoE CC keeps
        // its pool — dropping an AoE because one enemy is stunned is wrong.
        if applies_cc(def, content) && def.aoe == AoEShape::None {
            if let Some(target_unit) = target.and_then(|t| snap.unit(t)) {
                if target_unit.tags.contains(AiTags::IS_STUNNED) {
                    return false;
                }
            }
        }

        // Don't overheal: reject heal on target above 90% HP.
        if def.target_type == TargetType::SingleAlly {
            if let Some(target_unit) = target.and_then(|t| snap.unit(t)) {
                if target_unit.hp_pct() > 0.9 {
                    return false;
                }
            }
        }

        true
    });
}
