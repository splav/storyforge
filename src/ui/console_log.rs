use crate::game::resources::{CombatEvent, CombatLog, GameDb};
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct ConsoleCursor(pub usize);

/// Prints new CombatLog entries to stdout each frame.
pub fn print_log_system(
    log: Res<CombatLog>,
    names: Query<&Name>,
    db: Res<GameDb>,
    mut cursor: ResMut<ConsoleCursor>,
) {
    let new_events = &log.0[cursor.0..];
    for event in new_events {
        let line = fmt_event(event, &names, &db);
        println!("{line}");
    }
    cursor.0 = log.0.len();
}

pub fn fmt_event(event: &CombatEvent, names: &Query<&Name>, db: &GameDb) -> String {
    let name = |e: Entity| names.get(e).map(|n| n.as_str()).unwrap_or("?").to_string();
    match event {
        CombatEvent::CombatStarted => "=== Бой начался ===".into(),
        CombatEvent::RoundStarted { round } => format!("--- Раунд {round} ---"),
        CombatEvent::InitiativeRolled {
            actor,
            dex_mod,
            roll,
            total,
        } => {
            let mod_str = if *dex_mod >= 0 {
                format!("+{dex_mod}")
            } else {
                format!("{dex_mod}")
            };
            format!(
                "  инициатива {}: d20({roll}) {mod_str} = {total}",
                name(*actor)
            )
        }
        CombatEvent::TurnStarted { actor } => format!("  ▶ Ход: {}", name(*actor)),
        CombatEvent::TurnSkipped { actor } => {
            format!("  ○ {} пропускает ход [оглушён]", name(*actor))
        }
        CombatEvent::TurnEnded { actor } => format!("  ○ {} завершил ход", name(*actor)),
        CombatEvent::UnitMoved { actor, from, to } => {
            format!(
                "  ↦ {} переместился ({},{}) → ({},{})",
                name(*actor),
                from.0,
                from.1,
                to.0,
                to.1
            )
        }
        CombatEvent::RageGained {
            actor,
            current,
            max,
        } => format!("  ⚡ {}: ярость {}/{}", name(*actor), current, max),
        CombatEvent::ManaChanged {
            actor,
            current,
            max,
        } => format!("  ✦ {}: мана {}/{}", name(*actor), current, max),
        CombatEvent::AbilityUsed {
            actor,
            ability_name,
            target,
            cost_str,
        } => {
            let costs = if cost_str.is_empty() {
                String::new()
            } else {
                format!(" [{}]", cost_str)
            };
            format!(
                "  {} использует «{}» → {}{}",
                name(*actor),
                ability_name,
                name(*target),
                costs,
            )
        }
        CombatEvent::DamageResult {
            target,
            formula,
            armor_reduced,
            final_damage,
        } => {
            let armor_part = if *armor_reduced > 0 {
                format!(", броня -{}", armor_reduced)
            } else {
                String::new()
            };
            format!(
                "    урон: {}{} → -{} HP ({})",
                formula,
                armor_part,
                final_damage,
                name(*target)
            )
        }
        CombatEvent::HealResult {
            target,
            formula,
            amount,
        } => format!(
            "    лечение: {} → +{} HP ({})",
            formula,
            amount,
            name(*target)
        ),
        CombatEvent::StatusApplied { target, status } => {
            let sname = db
                .statuses
                .get(status)
                .map_or(status.0.as_str(), |s| s.name.as_str());
            format!("    {} получает статус «{}»", name(*target), sname)
        }
        CombatEvent::StatusExpired { target, status } => {
            let sname = db
                .statuses
                .get(status)
                .map_or(status.0.as_str(), |s| s.name.as_str());
            format!("    статус «{}» спал с {}", sname, name(*target))
        }
        CombatEvent::Missed { actor, target } => {
            format!("  {} промахнулся по {}", name(*actor), name(*target))
        }
        CombatEvent::UnitDied { entity } => format!("  ✗ {} погиб", name(*entity)),
        CombatEvent::CombatEnded { victory } => {
            if *victory {
                "=== ПОБЕДА ===".into()
            } else {
                "=== ПОРАЖЕНИЕ ===".into()
            }
        }
    }
}
