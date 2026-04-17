use crate::content::content_view::ActiveContent;
use crate::content::settings::GameSettings;
use crate::game::combat_log::CombatLog;
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct ConsoleCursor(pub usize);

/// Prints new CombatLog entries to stdout each frame.
pub fn print_log_system(
    log: Res<CombatLog>,
    names: Query<&Name>,
    content: Res<ActiveContent>,
    settings: Res<GameSettings>,
    mut cursor: ResMut<ConsoleCursor>,
) {
    let name = |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();
    let new_events = &log.0[cursor.0..];
    for event in new_events {
        let line = event.format(name, &content, settings.crit_fail_die);
        println!("{line}");
    }
    cursor.0 = log.0.len();
}
