use crate::content::content_view::ContentView;
use bevy::prelude::*;

use combat_engine::{
    event::{TurnSkipReason, TurnEndCause},
    effect::SpawnBlockedReason,
    content::CritFailOutcome,
    StatusId,
};

// ── Mirror types ─────────────────────────────────────────────────────────────

/// Mirror of engine `TurnSkipReason` for the ECS side.
/// ECS-only doc-alias; mirrors engine enum verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSkipReasonEcs {
    Dead,
    Stunned,
}

impl From<&TurnSkipReason> for TurnSkipReasonEcs {
    fn from(r: &TurnSkipReason) -> Self {
        match r {
            TurnSkipReason::Dead => TurnSkipReasonEcs::Dead,
            TurnSkipReason::Stunned => TurnSkipReasonEcs::Stunned,
        }
    }
}

/// Mirror of engine `TurnEndCause` for the ECS / CombatLog side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndCauseEcs {
    Manual,
    DeathOfActor,
    ResourcesExhausted,
}

impl From<&TurnEndCause> for TurnEndCauseEcs {
    fn from(c: &TurnEndCause) -> Self {
        match c {
            TurnEndCause::Manual => TurnEndCauseEcs::Manual,
            TurnEndCause::DeathOfActor => TurnEndCauseEcs::DeathOfActor,
            TurnEndCause::ResourcesExhausted => TurnEndCauseEcs::ResourcesExhausted,
        }
    }
}

/// Mirror of engine `SpawnBlockedReason` for the ECS side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnBlockedReasonEcs {
    TemplateMissing,
    MaxActiveReached,
    NoFreePosition,
}

impl From<&SpawnBlockedReason> for SpawnBlockedReasonEcs {
    fn from(r: &SpawnBlockedReason) -> Self {
        match r {
            SpawnBlockedReason::TemplateMissing => SpawnBlockedReasonEcs::TemplateMissing,
            SpawnBlockedReason::MaxActiveReached => SpawnBlockedReasonEcs::MaxActiveReached,
            SpawnBlockedReason::NoFreePosition => SpawnBlockedReasonEcs::NoFreePosition,
        }
    }
}

/// Mirror of engine `CritFailOutcome` for the ECS side.
/// `ApplyStatus` carries `StatusId` so the formatter can resolve the status name.
#[derive(Debug, Clone, PartialEq)]
pub enum CritFailOutcomeEcs {
    Miss,
    DoubleCost,
    SelfDamage,
    ApplyStatus(StatusId),
}

impl From<&CritFailOutcome> for CritFailOutcomeEcs {
    fn from(o: &CritFailOutcome) -> Self {
        match o {
            CritFailOutcome::Miss => CritFailOutcomeEcs::Miss,
            CritFailOutcome::DoubleCost => CritFailOutcomeEcs::DoubleCost,
            CritFailOutcome::SelfDamage(_) => CritFailOutcomeEcs::SelfDamage,
            CritFailOutcome::ApplyStatus(id) => CritFailOutcomeEcs::ApplyStatus(id.clone()),
        }
    }
}

// ── Localizer trait ───────────────────────────────────────────────────────────

/// Hook for future i18n.  Default implementation is `BuiltinRu`.
pub trait Localizer {
    fn locale(&self) -> &'static str;
}

/// Default built-in Russian locale — wraps current hardcoded strings.
pub struct BuiltinRu;

impl Localizer for BuiltinRu {
    fn locale(&self) -> &'static str {
        "ru"
    }
}

// ── CombatEvent enum ──────────────────────────────────────────────────────────

/// Flat enum for ECS-side log entries.
///
/// Variants marked *engine mirror* carry data lifted directly from engine `Event`.
/// Variants marked *ECS-only* are produced by bridge or Bevy systems with no
/// direct engine counterpart.
#[derive(Debug, Clone, PartialEq)]
pub enum CombatEvent {
    /// *ECS-only* — emitted once when a combat scenario starts.
    CombatStarted,
    /// *engine mirror* — `Event::RoundStarted`.
    RoundStarted {
        round: u32,
    },
    /// *ECS-only* — emitted by `turn_order.rs` after initiative rolls.
    InitiativeRolled {
        actor: Entity,
        dex_mod: i32,
        roll: i32,
        total: i32,
    },
    /// *engine mirror* — `Event::TurnStarted`.
    TurnStarted {
        actor: Entity,
    },
    /// *ECS-only* — ability-use summary emitted by bridge before damage/heal events.
    AbilityUsed {
        actor: Entity,
        ability_name: String,
        target: Entity,
        target_pos: hexx::Hex,
        is_aoe: bool,
        cost_str: String,
    },
    /// *engine mirror* — `Event::UnitDamaged`.
    DamageResult {
        target: Entity,
        raw: i32,
        armor_reduced: i32,
        final_damage: i32,
    },
    /// *engine mirror* — `Event::UnitHealed`.
    HealResult {
        target: Entity,
        amount: i32,
    },
    /// *engine mirror* — `Event::StatusApplied`.
    StatusApplied {
        target: Entity,
        status: StatusId,
    },
    /// *engine mirror* — `Event::StatusRemoved`.
    StatusExpired {
        target: Entity,
        status: StatusId,
    },
    /// *engine mirror* — `Event::TurnSkipped`. Carries reason so formatter can
    /// suppress `Dead` turns (D1).
    TurnSkipped {
        actor: Entity,
        reason: TurnSkipReasonEcs,
    },
    /// *engine mirror* — `Event::TurnEnded`.
    TurnEnded {
        actor: Entity,
        cause: TurnEndCauseEcs,
    },
    /// *engine mirror* — `Event::UnitMoved` (bridge aggregates per-step moves).
    UnitMoved {
        actor: Entity,
        from: hexx::Hex,
        to: hexx::Hex,
    },
    /// *ECS-only* — AoO summary built by bridge from `ReactionFired + UnitDamaged`.
    OpportunityAttack {
        attacker: Entity,
        target: Entity,
        damage: i32,
        killed: bool,
    },
    /// *engine mirror* — `Event::DotDamaged` (fused DoT tick + damage).
    DotDamaged {
        target: Entity,
        source: Entity,
        source_status: StatusId,
        raw: f32,
        mitigation: i32,
        pierces: bool,
        amount: i32,
    },
    /// *ECS-only* — emitted once when combat ends.
    CombatEnded {
        victory: bool,
    },
    /// *engine mirror* — `Event::CritFailed { outcome: Miss }`.
    CriticalMiss {
        actor: Entity,
    },
    /// *engine mirror* — `Event::CritFailed { outcome: DoubleCost | SelfDamage | ApplyStatus }`.
    CritFailSideEffect {
        actor: Entity,
        outcome: CritFailOutcomeEcs,
    },
    /// *engine mirror* — `Event::UnitDied`.
    UnitDied {
        entity: Entity,
    },
    /// *ECS-only* — boss phase transition data built by bridge from `Event::PhaseEntered`.
    PhaseEntered {
        actor: Entity,
        prev_name: String,
        next_name: String,
        flavor: Option<String>,
    },
    /// *ECS-only* — summon success emitted by bridge after `Event::UnitSpawned`.
    Summoned {
        summoner: Entity,
        summon_name: String,
    },
    /// *engine mirror* — `Event::SpawnBlocked`. Carries structured reason.
    SummonBlocked {
        summoner: Entity,
        reason: SpawnBlockedReasonEcs,
    },
    /// *engine mirror* — `Event::PoolChanged`. Unified pool-mutation event (C4+C6).
    PoolChanged {
        actor: Entity,
        pool: combat_engine::PoolKind,
        current: i32,
        max: i32,
        cause: combat_engine::PoolChangeCause,
    },
}

// ── Formatter ─────────────────────────────────────────────────────────────────

impl CombatEvent {
    /// Format this event as a log line, or `None` if the event should be silent.
    ///
    /// `name` maps an `Entity` to a display name.
    /// `content` is used to resolve `StatusId` → human name.
    /// `crit_fail_die` is the die size used for crit-fail rolls (for display).
    pub fn format<L: Localizer>(
        &self,
        name: impl Fn(Entity) -> String,
        content: &ContentView,
        crit_fail_die: u32,
        _loc: &L,
    ) -> Option<String> {
        let line = match self {
            CombatEvent::CombatStarted => "=== Бой начался ===".into(),
            CombatEvent::RoundStarted { round } => format!("--- Раунд {round} ---"),
            CombatEvent::InitiativeRolled { actor, dex_mod, roll, total } => {
                let mod_str = if *dex_mod >= 0 {
                    format!("+{dex_mod}")
                } else {
                    format!("{dex_mod}")
                };
                format!("  инициатива {}: d20({roll}) {mod_str} = {total}", name(*actor))
            }
            CombatEvent::TurnStarted { actor } => format!("  ▶ Ход: {}", name(*actor)),
            CombatEvent::TurnSkipped { actor, reason } => match reason {
                // D1: dead units silently skip every round.
                TurnSkipReasonEcs::Dead => return None,
                TurnSkipReasonEcs::Stunned => {
                    format!("  ○ {} пропускает ход [оглушён]", name(*actor))
                }
            },
            CombatEvent::TurnEnded { actor, .. } => format!("  ○ {} завершил ход", name(*actor)),
            CombatEvent::OpportunityAttack { attacker, target, damage, killed } => {
                let tail = if *killed { " ✗" } else { "" };
                format!(
                    "  ⚔ AoO: {} → {} (-{} HP){}",
                    name(*attacker),
                    name(*target),
                    damage,
                    tail
                )
            }
            CombatEvent::UnitMoved { actor, from, to } => {
                let [fc, fr] = crate::game::hex::hex_to_offset(*from);
                let [tc, tr] = crate::game::hex::hex_to_offset(*to);
                format!(
                    "  ↦ {} переместился ({},{}) → ({},{})",
                    name(*actor),
                    fc,
                    fr,
                    tc,
                    tr
                )
            }
            CombatEvent::PoolChanged { actor, pool, current, max, cause } => {
                use combat_engine::{PoolKind, PoolChangeCause};
                let pool_name = match pool {
                    // Hp pool changes are not shown in the combat log; HP
                    // events surface via UnitDamaged/UnitHealed entries.
                    PoolKind::Hp     => return None,
                    PoolKind::Mana   => "мана",
                    PoolKind::Rage   => "ярость",
                    PoolKind::Energy => "энергия",
                    PoolKind::Ap     => return None, // AP/MP changes are silent
                    PoolKind::Mp     => return None,
                };
                let icon = match cause {
                    PoolChangeCause::Regen | PoolChangeCause::Refill | PoolChangeCause::Gained => "⚡",
                    PoolChangeCause::Spent   => "✦",
                    PoolChangeCause::MaxChanged => "✦",
                };
                format!("  {icon} {}: {pool_name} {current}/{max}", name(*actor))
            }
            CombatEvent::AbilityUsed {
                actor,
                ability_name,
                target,
                target_pos,
                is_aoe,
                cost_str,
            } => {
                let costs = if cost_str.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", cost_str)
                };
                let target_label = if *is_aoe {
                    let [q, r] = crate::game::hex::hex_to_offset(*target_pos);
                    format!("({},{})", q, r)
                } else {
                    name(*target)
                };
                format!(
                    "  {} использует «{}» → {}{}",
                    name(*actor),
                    ability_name,
                    target_label,
                    costs,
                )
            }
            CombatEvent::DamageResult { target, raw, armor_reduced, final_damage } => {
                let armor_part = if *armor_reduced > 0 {
                    format!(", броня -{armor_reduced}")
                } else {
                    String::new()
                };
                format!(
                    "    урон: {raw}{armor_part} → -{final_damage} HP ({})",
                    name(*target)
                )
            }
            CombatEvent::HealResult { target, amount } => {
                format!("    лечение: +{} HP ({})", amount, name(*target))
            }
            CombatEvent::StatusApplied { target, status } => {
                let sname = content
                    .statuses
                    .get(status)
                    .map_or(status.0.as_str(), |s| s.name.as_str());
                format!("    {} получает статус «{}»", name(*target), sname)
            }
            CombatEvent::StatusExpired { target, status } => {
                let sname = content
                    .statuses
                    .get(status)
                    .map_or(status.0.as_str(), |s| s.name.as_str());
                format!("    статус «{}» спал с {}", sname, name(*target))
            }
            CombatEvent::DotDamaged { target, source_status, amount, .. } => {
                let sname = content
                    .statuses
                    .get(source_status)
                    .map_or(source_status.0.as_str(), |s| s.name.as_str());
                format!("    «{}» наносит {} урона ({})", sname, amount, name(*target))
            }
            CombatEvent::CriticalMiss { actor } => {
                format!(
                    "  ✗ {}: критическая неудача (d{crit_fail_die}=1) — промах!",
                    name(*actor)
                )
            }
            CombatEvent::CritFailSideEffect { actor, outcome } => {
                let desc = match outcome {
                    CritFailOutcomeEcs::DoubleCost => "перегрузка воли (двойная цена)".into(),
                    CritFailOutcomeEcs::SelfDamage => "пробой цепи (урон по себе)".into(),
                    CritFailOutcomeEcs::ApplyStatus(id) => {
                        let sname = content
                            .statuses
                            .get(id)
                            .map_or(id.0.as_str(), |s| s.name.as_str());
                        format!("побочный эффект: {sname}")
                    }
                    CritFailOutcomeEcs::Miss => "критическая неудача".into(),
                };
                format!("  ⚠ {}: {}", name(*actor), desc)
            }
            CombatEvent::UnitDied { entity } => format!("  ✗ {} погиб", name(*entity)),
            CombatEvent::PhaseEntered { prev_name, next_name, flavor, .. } => {
                let head = format!("  ✦ {prev_name} → {next_name}");
                match flavor {
                    Some(f) if !f.is_empty() => format!("{head}\n    {f}"),
                    _ => head,
                }
            }
            CombatEvent::Summoned { summoner, summon_name } => {
                format!("  ✧ {} призывает {}", name(*summoner), summon_name)
            }
            CombatEvent::SummonBlocked { summoner, reason } => {
                let reason_text = match reason {
                    SpawnBlockedReasonEcs::TemplateMissing => "шаблон не найден",
                    SpawnBlockedReasonEcs::MaxActiveReached => "лимит призванных достигнут",
                    SpawnBlockedReasonEcs::NoFreePosition => "рядом нет свободной клетки",
                };
                format!("  ⚠ {} пытается призвать — {reason_text}", name(*summoner))
            }
            CombatEvent::CombatEnded { victory } => {
                if *victory {
                    "=== ПОБЕДА ===".into()
                } else {
                    "=== ПОРАЖЕНИЕ ===".into()
                }
            }
        };
        Some(line)
    }
}

// ── Combat log resource ────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct CombatLog(pub Vec<CombatEvent>);

impl CombatLog {
    pub fn push(&mut self, event: CombatEvent) {
        self.0.push(event);
    }
}
