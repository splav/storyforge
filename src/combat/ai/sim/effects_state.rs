//! Read-only abstraction over battle state for effect resolution.
//!
//! The live pipeline (`combat::resolution`) and the AI sim
//! (`combat::ai::planning::sim`) both need to answer the same few questions
//! when resolving an ability: *where* is the actor standing, *which* units
//! occupy which cells, *what* team is each unit on, *is* each unit alive.
//! They historically answered these questions against different sources
//! (Bevy queries vs. `BattleSnapshot`), and drift crept in every time one
//! side changed its filtering rules.
//!
//! `TargetState` is the single question-set; backends implement it via thin
//! adapters at their call sites. Shared helpers like
//! `compute_affected_targets` consume the trait and live once.

use crate::content::abilities::{AbilityDef, AoEShape};
use combat_engine::aoe_cells;
use crate::game::components::Team;
use crate::game::hex::Hex;
use bevy::prelude::Entity;

/// A unit reference as seen by the targeting layer. `alive` is carried
/// explicitly because both backends keep dead entities in their world
/// models — Bevy keeps `Combatant` + `HexPositions` for corpses, and the
/// AI snapshot now retains hp=0 entries so resurrection / death triggers /
/// replay fidelity all work. AoE enumeration must filter by `alive`
/// explicitly rather than assuming "present ⇒ alive".
#[derive(Clone, Copy, Debug)]
pub struct TargetRef {
    pub entity: Entity,
    pub team: Team,
    pub alive: bool,
}

pub trait TargetState {
    fn actor_pos(&self, actor: Entity) -> Option<Hex>;
    fn unit_at_cell(&self, pos: Hex) -> Option<TargetRef>;
    fn team_of(&self, entity: Entity) -> Option<Team>;
}

/// Enumerate every unit an ability touches, unifying single-target and AoE
/// cases. Mirrors the filtering rules the two backends used to maintain
/// independently:
///
/// - Non-AoE: just `[primary_target]`.
/// - AoE: walk every cell, collect live units, apply friendly-fire rules.
///
/// Friendly-fire semantics match `resolve_action_system` + sim's legacy
/// `collect_aoe`: actor is included only if `friendly_fire`; allies are
/// included only if `friendly_fire`; enemies always included.
pub fn compute_affected_targets<S: TargetState>(
    actor: Entity,
    def: &AbilityDef,
    primary_target: Entity,
    target_pos: Hex,
    state: &S,
) -> Vec<Entity> {
    if matches!(def.aoe, AoEShape::None) {
        return vec![primary_target];
    }

    let actor_pos = state.actor_pos(actor).unwrap_or(Hex::ZERO);
    let actor_team = match state.team_of(actor) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let cells = aoe_cells(def.aoe, actor_pos, target_pos);
    let mut out = Vec::new();
    for cell in cells {
        let Some(r) = state.unit_at_cell(cell) else { continue };
        if !r.alive {
            continue;
        }
        if r.entity == actor {
            if def.friendly_fire {
                out.push(r.entity);
            }
            continue;
        }
        if !def.friendly_fire && r.team == actor_team {
            continue;
        }
        out.push(r.entity);
    }
    out
}
