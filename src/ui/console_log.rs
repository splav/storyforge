use bevy::prelude::*;
use crate::game::resources::{CombatEvent, CombatLog};

#[derive(Resource, Default)]
pub struct ConsoleCursor(pub usize);

/// Prints new CombatLog entries to stdout each frame.
pub fn print_log_system(
    log: Res<CombatLog>,
    names: Query<&Name>,
    mut cursor: ResMut<ConsoleCursor>,
) {
    let new_events = &log.0[cursor.0..];
    for event in new_events {
        let line = fmt_event(event, &names);
        println!("{line}");
    }
    cursor.0 = log.0.len();
}

pub fn fmt_event(event: &CombatEvent, names: &Query<&Name>) -> String {
    let name = |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();
    match event {
        CombatEvent::CombatStarted =>
            "=== Бой начался ===".into(),
        CombatEvent::RoundStarted { round } =>
            format!("--- Раунд {round} ---"),
        CombatEvent::TurnStarted { actor } =>
            format!("  ▶ Ход: {}", name(*actor)),
        CombatEvent::TurnEnded { actor } =>
            format!("  ○ {} завершил ход", name(*actor)),
        CombatEvent::AbilityUsed { actor, ability_name, target } =>
            format!("  {} использует «{}» → {}", name(*actor), ability_name, name(*target)),
        CombatEvent::DamageResult { target, formula, armor_reduced, final_damage } => {
            let armor_part = if *armor_reduced > 0 {
                format!(", броня -{}", armor_reduced)
            } else {
                String::new()
            };
            format!("    урон: {}{} → -{} HP ({})", formula, armor_part, final_damage, name(*target))
        }
        CombatEvent::HealResult { target, formula, amount } =>
            format!("    лечение: {} → +{} HP ({})", formula, amount, name(*target)),
        CombatEvent::StatusApplied { target, status } =>
            format!("    {} получает статус «{}»", name(*target), status),
        CombatEvent::Missed { actor, target } =>
            format!("  {} промахнулся по {}", name(*actor), name(*target)),
        CombatEvent::UnitDied { entity } =>
            format!("  ✗ {} погиб", name(*entity)),
        CombatEvent::CombatEnded { victory } =>
            if *victory { "=== ПОБЕДА ===" .into() } else { "=== ПОРАЖЕНИЕ ===" .into() },
    }
}
