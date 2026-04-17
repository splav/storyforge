use crate::content::encounters::AuraAffects;
use crate::core::StatusId;
use crate::game::components::{
    ActiveStatus, AuraSource, Dead, Faction, StatusEffects, Team,
};
use crate::game::resources::HexPositions;
use bevy::prelude::*;
use hexx::Hex;
use std::collections::HashSet;

/// Reapplies passive auras from alive `AuraSource` units and cleans up stale ones.
///
/// Contract per run:
/// * Every target in range + matching `affects` of a live source ends up with that
///   aura's status, applier = source.
/// * Any status previously applied by an aura source that is now dead, out of range,
///   or has stopped matching `affects` is removed from its target.
/// * Statuses applied by non-aura means (abilities, etc.) are never touched —
///   even if they share the same id as an aura's status, we don't stomp them;
///   the aura will re-cover once the ability-applied entry expires.
pub fn apply_auras_system(
    positions: Res<HexPositions>,
    sources_q: Query<(Entity, &AuraSource, &Faction, Has<Dead>)>,
    mut targets: Query<(Entity, Option<&Faction>, &mut StatusEffects), Without<Dead>>,
) {
    // Snapshot of every entity that currently carries an AuraSource component.
    // Used to recognise "aura-applied" statuses (by their applier field).
    let any_source: HashSet<Entity> = sources_q.iter().map(|(e, _, _, _)| e).collect();
    if any_source.is_empty() {
        return;
    }

    // Snapshot of live sources with resolved positions.
    let live: Vec<(Entity, AuraSource, Team, Hex)> = sources_q
        .iter()
        .filter_map(|(e, aura, fac, dead)| {
            if dead {
                return None;
            }
            let pos = positions.get(&e)?;
            Some((e, aura.clone(), fac.0, pos))
        })
        .collect();

    for (target_e, target_fac, mut se) in targets.iter_mut() {
        let target_pos = positions.get(&target_e);

        // Decide which aura statuses this target should currently carry, and from which source.
        // If multiple sources apply the same status id, the last one wins — acceptable because
        // the status itself is the same and on any source loss the next refresh converges.
        let mut should_have: Vec<(StatusId, Entity)> = Vec::new();
        if let Some(target_pos) = target_pos {
            for (src_e, aura, src_team, src_pos) in &live {
                if src_pos.unsigned_distance_to(target_pos) > aura.radius {
                    continue;
                }
                let matches = match aura.affects {
                    AuraAffects::Enemies => target_fac.is_some_and(|f| f.0 != *src_team),
                    AuraAffects::Allies => {
                        target_fac.is_some_and(|f| f.0 == *src_team) && target_e != *src_e
                    }
                    AuraAffects::All => target_e != *src_e,
                };
                if !matches {
                    continue;
                }
                // Replace any prior entry for the same status id — last source wins.
                should_have.retain(|(id, _)| id != &aura.status);
                should_have.push((aura.status.clone(), *src_e));
            }
        }

        // 1. Clean up stale aura applications: any status whose applier is an AuraSource
        //    entity but is no longer in `should_have` with that same applier.
        se.0.retain(|s| {
            if !any_source.contains(&s.applier) {
                return true; // non-aura application — leave alone
            }
            should_have
                .iter()
                .any(|(id, applier)| id == &s.id && *applier == s.applier)
        });

        // 2. Add new aura applications that are missing. If the target already has this
        //    status id from some *other* (non-aura) applier, leave that alone — we don't
        //    stomp ability-applied statuses. The aura will re-cover later.
        for (status_id, src_e) in should_have {
            let has_from_this_source = se
                .0
                .iter()
                .any(|s| s.id == status_id && s.applier == src_e);
            if has_from_this_source {
                continue;
            }
            let id_taken = se.0.iter().any(|s| s.id == status_id);
            if id_taken {
                continue;
            }
            se.0.push(ActiveStatus {
                id: status_id,
                rounds_remaining: 1,
                applier: src_e,
                dot_per_tick: 0,
            });
        }
    }
}
