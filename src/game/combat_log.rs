use crate::core::StatusId;
use bevy::prelude::*;

use super::resources::GameDb;

// ── Combat events ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CombatEvent {
    CombatStarted,
    RoundStarted {
        round: u32,
    },
    InitiativeRolled {
        actor: Entity,
        dex_mod: i32,
        roll: i32,
        total: i32,
    },
    TurnStarted {
        actor: Entity,
    },
    AbilityUsed {
        actor: Entity,
        ability_name: String,
        target: Entity,
        cost_str: String,
    },
    DamageResult {
        target: Entity,
        formula: String,
        armor_reduced: i32,
        final_damage: i32,
    },
    HealResult {
        target: Entity,
        formula: String,
        amount: i32,
    },
    StatusApplied {
        target: Entity,
        status: StatusId,
    },
    StatusExpired {
        target: Entity,
        status: StatusId,
    },
    TurnSkipped {
        actor: Entity,
    },
    TurnEnded {
        actor: Entity,
    },
    UnitMoved {
        actor: Entity,
        from: (i32, i32),
        to: (i32, i32),
    },
    RageGained {
        actor: Entity,
        current: i32,
        max: i32,
    },
    ManaChanged {
        actor: Entity,
        current: i32,
        max: i32,
    },
    CombatEnded {
        victory: bool,
    },
    UnitDied {
        entity: Entity,
    },
}

impl CombatEvent {
    pub fn format(&self, name: impl Fn(Entity) -> String, db: &GameDb) -> String {
        match self {
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
}

// ── Combat log resource ─────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct CombatLog(pub Vec<CombatEvent>);

impl CombatLog {
    pub fn push(&mut self, event: CombatEvent) {
        self.0.push(event);
    }
}
