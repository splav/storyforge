//! `compute_forecast` system — dry-run a pending ability cast and populate
//! `ActionForecast` with expected outcomes (damage/heal amounts, HP before/after,
//! lethal flag, applied statuses).
//!
//! Gated on `UiDirtyFlags::FORECAST`; never mutates `CombatStateRes` or advances RNG.

use bevy::prelude::*;
use combat_engine::{action::Action, event::Event, preview::preview_action, PoolKind, StatusId};

use crate::combat::bridge::{build_ecs_content_view, CombatStateRes, UnitIdMap};
use crate::content::content_view::ActiveContent;
use crate::game::resources::{
    ActionForecast, ForecastEntry, ForecastKind, HexPositions, SelectionState, UiDirty,
    UiDirtyFlags,
};
use crate::ui::hex_grid::HexHover;

/// System: recompute `ActionForecast` when `UiDirtyFlags::FORECAST` is set.
///
/// Reads the current selection + hover, calls `preview_action`, and folds events
/// into `ActionForecast`. Never touches `CombatStateRes` mutably or the RNG.
#[allow(clippy::too_many_arguments)]
pub fn compute_forecast(
    dirty: Res<UiDirty>,
    sel: Res<SelectionState>,
    hover: Res<HexHover>,
    positions: Res<HexPositions>,
    combat_state: Res<CombatStateRes>,
    id_map: Res<UnitIdMap>,
    active_content: Res<ActiveContent>,
    mut forecast: ResMut<ActionForecast>,
) {
    if !dirty.0.contains(UiDirtyFlags::FORECAST) {
        return;
    }

    // Resolve actor UnitId.
    let (Some(actor_entity), Some(ability_id)) = (sel.selected_actor, sel.selected_ability.clone())
    else {
        forecast.clear();
        return;
    };
    let Some(actor_uid) = id_map.get_id(actor_entity) else {
        forecast.clear();
        return;
    };

    // Resolve target from hovered hex.
    let Some(hovered_hex) = hover.0 else {
        forecast.clear();
        return;
    };
    let Some(target_entity) = positions.entity_at(hovered_hex) else {
        forecast.clear();
        return;
    };
    let Some(target_uid) = id_map.get_id(target_entity) else {
        forecast.clear();
        return;
    };

    let action = Action::Cast {
        actor: actor_uid,
        ability: ability_id,
        target: target_uid,
        target_pos: hovered_hex,
    };

    let content = build_ecs_content_view(&active_content);
    let events = match preview_action(&combat_state.0, action, &content) {
        Ok(evs) => evs,
        Err(_) => {
            forecast.clear();
            return;
        }
    };

    // Fold events into per-unit forecast entries.
    // We build a map keyed by UnitId to accumulate damage, heal, death, statuses.
    use std::collections::HashMap;
    struct UnitAccum {
        entity: Entity,
        kind: Option<ForecastKind>,
        amount: i32,
        hp_before: i32,
        max_hp: i32,
        lethal: bool,
        statuses: Vec<StatusId>,
    }

    use combat_engine::state::UnitId;
    let mut accum: HashMap<UnitId, UnitAccum> = HashMap::new();

    for ev in &events {
        match ev {
            Event::UnitDamaged { target, amount, .. } => {
                let Some(entity) = id_map.get_entity(*target) else {
                    continue;
                };
                let (hp_before, max_hp) = combat_state
                    .0
                    .unit(*target)
                    .and_then(|u| u.pools[PoolKind::Hp])
                    .unwrap_or((0, 0));
                let entry = accum.entry(*target).or_insert(UnitAccum {
                    entity,
                    kind: Some(ForecastKind::Damage),
                    amount: 0,
                    hp_before,
                    max_hp,
                    lethal: false,
                    statuses: Vec::new(),
                });
                entry.kind = Some(ForecastKind::Damage);
                entry.amount += amount;
            }
            Event::DotDamaged { target, amount, .. } => {
                let Some(entity) = id_map.get_entity(*target) else {
                    continue;
                };
                let (hp_before, max_hp) = combat_state
                    .0
                    .unit(*target)
                    .and_then(|u| u.pools[PoolKind::Hp])
                    .unwrap_or((0, 0));
                let entry = accum.entry(*target).or_insert(UnitAccum {
                    entity,
                    kind: Some(ForecastKind::Damage),
                    amount: 0,
                    hp_before,
                    max_hp,
                    lethal: false,
                    statuses: Vec::new(),
                });
                entry.kind = Some(ForecastKind::Damage);
                entry.amount += amount;
            }
            Event::UnitHealed { target, amount } => {
                let Some(entity) = id_map.get_entity(*target) else {
                    continue;
                };
                let (hp_before, max_hp) = combat_state
                    .0
                    .unit(*target)
                    .and_then(|u| u.pools[PoolKind::Hp])
                    .unwrap_or((0, 0));
                let entry = accum.entry(*target).or_insert(UnitAccum {
                    entity,
                    kind: Some(ForecastKind::Heal),
                    amount: 0,
                    hp_before,
                    max_hp,
                    lethal: false,
                    statuses: Vec::new(),
                });
                // Healing may mix with damage on the same target in complex spells;
                // last event wins for kind (preview currently won't produce this, but
                // be robust).
                entry.kind = Some(ForecastKind::Heal);
                entry.amount += amount;
            }
            Event::UnitDied { unit } => {
                if let Some(entry) = accum.get_mut(unit) {
                    entry.lethal = true;
                } else {
                    // Death without a preceding damage event in the preview slice —
                    // create a minimal entry so lethal is surfaced.
                    let Some(entity) = id_map.get_entity(*unit) else {
                        continue;
                    };
                    let (hp_before, max_hp) = combat_state
                        .0
                        .unit(*unit)
                        .and_then(|u| u.pools[PoolKind::Hp])
                        .unwrap_or((0, 0));
                    accum.insert(
                        *unit,
                        UnitAccum {
                            entity,
                            kind: None,
                            amount: 0,
                            hp_before,
                            max_hp,
                            lethal: true,
                            statuses: Vec::new(),
                        },
                    );
                }
            }
            Event::StatusApplied { target, status } => {
                let Some(entity) = id_map.get_entity(*target) else {
                    continue;
                };
                let (hp_before, max_hp) = combat_state
                    .0
                    .unit(*target)
                    .and_then(|u| u.pools[PoolKind::Hp])
                    .unwrap_or((0, 0));
                let entry = accum.entry(*target).or_insert(UnitAccum {
                    entity,
                    kind: None,
                    amount: 0,
                    hp_before,
                    max_hp,
                    lethal: false,
                    statuses: Vec::new(),
                });
                entry.statuses.push(status.clone());
            }
            _ => {}
        }
    }

    forecast.entries = accum
        .into_values()
        .filter_map(|a| {
            // Only include entries that have a meaningful outcome to show.
            if a.kind.is_none() && !a.lethal && a.statuses.is_empty() {
                return None;
            }
            let kind = a.kind.unwrap_or(ForecastKind::Damage);
            let hp_after = match kind {
                ForecastKind::Damage => (a.hp_before - a.amount).max(0),
                ForecastKind::Heal => (a.hp_before + a.amount).min(a.max_hp),
            };
            Some(ForecastEntry {
                entity: a.entity,
                kind,
                amount: a.amount,
                hp_before: a.hp_before,
                hp_after,
                lethal: a.lethal,
                statuses: a.statuses,
            })
        })
        .collect();

    // Crit-fail: d20 roll, flat 5% chance.
    forecast.crit_fail_pct = if forecast.entries.is_empty() { 0 } else { 5 };
}
