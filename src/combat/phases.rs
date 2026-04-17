use crate::combat::ai::role::{self as ai_role, AxisProfile};
use crate::content::content_view::ActiveContent;
use crate::content::encounters::PhaseTrigger;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{Abilities, CombatStats, Dead, EnemyPhases, Vital};
use bevy::prelude::*;

/// Applies the next pending phase of each enemy when its trigger fires.
/// Runs after `apply_effects_system` so damage from the current turn is visible,
/// and before `advance_turn_system` so a transition can revive the boss (by removing
/// `Dead` and refilling HP) *before* the victory check sees its 0 HP.
///
/// At most one phase per enemy per frame — cascading transitions are applied on
/// subsequent ticks, which prevents accidental chains when a huge hit crosses
/// several thresholds.
pub fn phase_transition_system(
    mut commands: Commands,
    mut log: ResMut<CombatLog>,
    content: Res<ActiveContent>,
    mut q: Query<(
        Entity,
        &mut EnemyPhases,
        &mut Vital,
        &mut CombatStats,
        &mut Abilities,
        Option<&mut AxisProfile>,
        &mut Name,
        Has<Dead>,
    )>,
) {
    for (entity, mut phases, mut vital, mut stats, mut abilities, role_opt, mut name, is_dead) in
        &mut q
    {
        let Some(phase) = phases.pending.first() else {
            continue;
        };
        let fires = match &phase.trigger {
            PhaseTrigger::HpBelowPct(pct) => {
                vital.max_hp > 0 && vital.hp * 100 <= vital.max_hp * *pct
            }
        };
        if !fires {
            continue;
        }

        // Capture name before any mutation so the log/popup can show the actual
        // "was → now" transition rather than "now → now".
        let prev_name = name.as_str().to_string();

        // Clone the phase out before mutating its owning component.
        let phase = phase.clone();

        if let Some(new_stats) = &phase.stats {
            *stats = new_stats.clone();
            vital.max_hp = new_stats.max_hp;
            // Keep current hp but clamp; `heal_to_full` overrides below.
            vital.hp = vital.hp.min(vital.max_hp);
        }
        if phase.heal_to_full {
            vital.hp = vital.max_hp;
        }
        if is_dead && vital.hp > 0 {
            commands.entity(entity).remove::<Dead>();
        }
        if let Some(ref new_abilities) = phase.ability_ids {
            abilities.0 = new_abilities.clone();
        }
        if let Some(mut role) = role_opt {
            if let Some(ref role_name) = phase.ai_role {
                if let Some(parsed) = ai_role::parse_role(role_name) {
                    *role = parsed.into();
                }
            } else if phase.stats.is_some() || phase.ability_ids.is_some() {
                // Re-infer when inputs changed and no explicit role was given.
                *role = ai_role::infer_profile(&abilities.0, vital.max_hp, vital.armor, &content);
            }
        }

        let next_name = phase.name.clone().unwrap_or_else(|| prev_name.clone());
        if phase.name.is_some() {
            *name = Name::new(next_name.clone());
        }

        log.push(CombatEvent::PhaseEntered {
            actor: entity,
            prev_name,
            next_name,
            flavor: phase.flavor.clone(),
        });
        phases.pending.remove(0);
    }
}
