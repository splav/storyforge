use bevy::prelude::*;
use crate::game::resources::CombatLog;
use super::HudLog;
use super::console_log::fmt_event;

const MAX_LINES: usize = 6;

pub fn update_log(
    log: Res<CombatLog>,
    names: Query<&Name>,
    mut q: Query<&mut Text, With<HudLog>>,
) {
    if !log.is_changed() { return; }
    let Ok(mut t) = q.single_mut() else { return };

    let lines: String = log.0
        .iter()
        .rev()
        .take(MAX_LINES)
        .map(|e| format!("{}\n", fmt_event(e, &names)))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    t.0 = lines;
}
