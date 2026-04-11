use super::console_log::fmt_event;
use super::HudLog;
use crate::game::resources::{CombatLog, GameDb};
use bevy::prelude::*;

const MAX_LINES: usize = 6;

pub fn update_log(
    log: Res<CombatLog>,
    names: Query<&Name>,
    db: Res<GameDb>,
    mut q: Query<&mut Text, With<HudLog>>,
) {
    if !log.is_changed() {
        return;
    }
    let Ok(mut t) = q.single_mut() else { return };

    let lines: String = log
        .0
        .iter()
        .rev()
        .take(MAX_LINES)
        .map(|e| format!("{}\n", fmt_event(e, &names, &db)))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    t.0 = lines;
}
