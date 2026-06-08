//! Engine-side targeting tests — `aoe_cells` geometry + `compute_affected_targets`.
//!
//! Mirrors the Bevy-side `combat::effects_state` tests now that the function
//! lives in `combat_engine::targeting`.

use std::collections::HashMap;

use storyforge::combat_engine::{
    content::{AbilityDef, AbilityRange, AoEShape, Cost, TargetType},
    state::Team,
    targeting::{aoe_cells, compute_affected_targets, TargetRef, TargetState},
};
use storyforge::game::hex::hex_from_offset;

// ── Helpers ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
struct Uid(u32);

/// Hand-rolled stub: a lookup table of (pos → TargetRef) plus actor position.
struct StubState {
    actor: Uid,
    actor_pos: hexx::Hex,
    actor_team: Team,
    units: HashMap<hexx::Hex, TargetRef<Uid>>,
}

impl TargetState for StubState {
    type Id = Uid;

    fn actor_pos(&self, actor: Uid) -> Option<hexx::Hex> {
        (actor == self.actor).then_some(self.actor_pos)
    }

    fn unit_at_cell(&self, pos: hexx::Hex) -> Option<TargetRef<Uid>> {
        self.units.get(&pos).copied()
    }

    fn team_of(&self, id: Uid) -> Option<Team> {
        if id == self.actor {
            return Some(self.actor_team);
        }
        self.units.values().find(|r| r.id == id).map(|r| r.team)
    }
}

fn ability(aoe: AoEShape, target_type: TargetType, friendly_fire: bool) -> AbilityDef {
    use storyforge::combat_engine::{EffectDef, StatusApplication};
    AbilityDef {
        key: None,
        cost_ap: 1,
        costs: Vec::<Cost>::new(),
        range: AbilityRange { min: 0, max: 6 },
        target_type,
        aoe,
        friendly_fire,
        effect: EffectDef::None,
        statuses: Vec::<StatusApplication>::new(),
        requires_los: false,
        passive: vec![],
requires_tags: Default::default(),
excludes_tags: Default::default()
    }
}

// ── aoe_cells geometry ───────────────────────────────────────────────────────

#[test]
fn aoe_cells_none_returns_empty() {
    let cells = aoe_cells(AoEShape::None, hex_from_offset(0, 0), hex_from_offset(2, 2));
    assert!(cells.is_empty());
}

#[test]
fn aoe_cells_circle_radius_0_is_center_only() {
    let cells = aoe_cells(
        AoEShape::Circle { radius: 0 },
        hex_from_offset(0, 0),
        hex_from_offset(3, 3),
    );
    assert_eq!(cells, vec![hex_from_offset(3, 3)]);
}

#[test]
fn aoe_cells_circle_radius_1_has_7_cells() {
    let cells = aoe_cells(
        AoEShape::Circle { radius: 1 },
        hex_from_offset(0, 0),
        hex_from_offset(3, 3),
    );
    assert_eq!(cells.len(), 7); // center + 6 neighbors
}

#[test]
fn aoe_cells_line_same_cell_returns_empty() {
    let here = hex_from_offset(1, 1);
    let cells = aoe_cells(AoEShape::Line { length: 5 }, here, here);
    assert!(cells.is_empty());
}

// ── compute_affected_targets ─────────────────────────────────────────────────

#[test]
fn non_aoe_returns_primary_target_only() {
    let state = StubState {
        actor: Uid(1),
        actor_pos: hex_from_offset(0, 0),
        actor_team: Team::Player,
        units: HashMap::new(),
    };
    let def = ability(AoEShape::None, TargetType::SingleEnemy, false);

    let out = compute_affected_targets(Uid(1), &def, Uid(99), hex_from_offset(2, 0), &state);
    assert_eq!(out, vec![Uid(99)]);
}

#[test]
fn aoe_circle_collects_enemies_in_radius_excludes_allies_when_no_friendly_fire() {
    let target_pos = hex_from_offset(3, 0);
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    let mut units = HashMap::new();
    // Enemy at center → included.
    units.insert(target_pos, TargetRef { id: Uid(10), team: Team::Enemy, alive: true });
    // Enemy at first neighbor → included.
    units.insert(neighbors[0], TargetRef { id: Uid(11), team: Team::Enemy, alive: true });
    // Ally at second neighbor → excluded (friendly_fire = false).
    units.insert(neighbors[1], TargetRef { id: Uid(12), team: Team::Player, alive: true });
    // Dead enemy at third neighbor → excluded.
    units.insert(neighbors[2], TargetRef { id: Uid(13), team: Team::Enemy, alive: false });

    let state = StubState {
        actor: Uid(1),
        actor_pos: hex_from_offset(0, 0),
        actor_team: Team::Player,
        units,
    };
    let def = ability(AoEShape::Circle { radius: 1 }, TargetType::Ground, /* friendly_fire */ false);

    let mut out = compute_affected_targets(Uid(1), &def, Uid(10), target_pos, &state);
    out.sort_by_key(|u| u.0);
    assert_eq!(out, vec![Uid(10), Uid(11)]);
}

#[test]
fn aoe_circle_with_friendly_fire_includes_allies_and_actor() {
    let target_pos = hex_from_offset(3, 0);
    let actor_pos = hex_from_offset(3, 0); // actor stands at target → in AoE
    let neighbors: Vec<hexx::Hex> = target_pos.all_neighbors().to_vec();

    let mut units = HashMap::new();
    // Actor at center.
    units.insert(actor_pos, TargetRef { id: Uid(1), team: Team::Player, alive: true });
    // Ally adjacent.
    units.insert(neighbors[0], TargetRef { id: Uid(11), team: Team::Player, alive: true });
    // Enemy adjacent.
    units.insert(neighbors[1], TargetRef { id: Uid(12), team: Team::Enemy, alive: true });

    let state = StubState {
        actor: Uid(1),
        actor_pos,
        actor_team: Team::Player,
        units,
    };
    let def = ability(AoEShape::Circle { radius: 1 }, TargetType::Ground, /* friendly_fire */ true);

    let mut out = compute_affected_targets(Uid(1), &def, Uid(1), target_pos, &state);
    out.sort_by_key(|u| u.0);
    assert_eq!(out, vec![Uid(1), Uid(11), Uid(12)]);
}

#[test]
fn aoe_returns_empty_when_actor_team_unknown() {
    let state = StubState {
        actor: Uid(99), // not the actor stored in the state
        actor_pos: hex_from_offset(0, 0),
        actor_team: Team::Player,
        units: HashMap::new(),
    };
    let def = ability(AoEShape::Circle { radius: 1 }, TargetType::Ground, false);

    let out = compute_affected_targets(Uid(99), &def, Uid(0), hex_from_offset(2, 0), &state);
    assert!(out.is_empty(), "no actor_team → no AoE enumeration");
}
