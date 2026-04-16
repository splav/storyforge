use crate::content::settings::GameSettings;
use crate::game::combat_log::CombatLog;
use crate::game::resources::GameDb;
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct ConsoleCursor(pub usize);

/// Prints new CombatLog entries to stdout each frame.
pub fn print_log_system(
    log: Res<CombatLog>,
    names: Query<&Name>,
    db: Res<GameDb>,
    settings: Res<GameSettings>,
    mut cursor: ResMut<ConsoleCursor>,
) {
    let name = |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();
    let new_events = &log.0[cursor.0..];
    for event in new_events {
        let line = event.format(name, &db, settings.crit_fail_die);
        println!("{line}");
    }
    cursor.0 = log.0.len();
}
