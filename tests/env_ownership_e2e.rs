//! E2E tests for per-trap ownership + per-team reveal + AI hazard avoidance (T10).
//!
//! Covers four equivalence classes and one parity check:
//!
//! 1. `enemy_owned_trap_visible_to_enemy_ai_not_player`
//!    — ownership + T3 snapshot filter.
//! 2. `enemy_ai_soft_avoids_own_visible_trap_when_alternative_exists`
//!    — hazard_costs wiring in `reach_from` (T9 via snapshot).
//! 3. `player_steps_on_hidden_enemy_trap_and_it_fires`
//!    — engine-level firing is visibility-agnostic.
//! 4. `neutral_trap_fires_on_both_teams`
//!    — `owner=None` trap fires regardless of which team steps on it.
//! 5. `ai_sim_and_prod_hazard_costs_agree`
//!    — same team-filtered snapshot yields identical `hazard_costs`
//!    for the same actor (parity by construction).

use std::collections::HashMap;

use storyforge::combat::ai::test_helpers::{snapshot_from, UnitBuilder};
use storyforge::combat::ai::plan::reach::reach_from as ai_reach_from;
use storyforge::combat_engine::{
    AbilityId,
    DiceExpr, StatusBonuses, StatusDef, StatusId,
    action::Action,
    content::{AbilityDef, AbilityRange, AoEShape, ContentView, EffectDef, TargetType},
    dice::ExpectedValue,
    event::Event,
    state::{
        CombatState, EnvId, EnvKind, EnvObject, RoundPhase, Team, TeamSet, Unit, UnitId,
    },
    step::step,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::components::Team as AppTeam;

// ── Engine-test harness (inline, mirrors tests/combat_engine/trap.rs) ────────

struct Stub(HashMap<AbilityId, AbilityDef>, StatusDef);
impl Stub {
    fn damage(n: u32) -> Self {
        Self(
            HashMap::from([(
                AbilityId::from("trap"),
                AbilityDef {
                    key: None,
                    cost_ap: 0,
                    costs: vec![],
                    range: AbilityRange { min: 0, max: 0 },
                    target_type: TargetType::SingleEnemy,
                    aoe: AoEShape::None,
                    friendly_fire: false,
                    effect: EffectDef::Damage { dice: DiceExpr::new(n, 1, 0) },
                    statuses: vec![],
                    requires_los: false,
                    passive: vec![],
                },
            )]),
            StatusDef {
                causes_disadvantage: false,
                blocks_mana_abilities: false,
                forces_targeting: false,
                skips_turn: false,
                hp_percent_dot: 0,
                bonuses: StatusBonuses { armor_bonus: 0, damage_taken_bonus: 0, speed_bonus: 0 },
            },
        )
    }
}
impl ContentView for Stub {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> { self.0.get(id) }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> { Some(&self.1) }
    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

/// Simple engine unit for step-level tests. Mirrors the pattern in
/// `tests/combat_engine/trap.rs::unit()`, but without `EngineUnitBuilder`
/// (unavailable in a standalone test binary).
fn eng_unit(id: u64, team: Team, col: i32, hp: i32) -> Unit {
    use storyforge::combat_engine::{PoolKind, RegenRule, enum_map::enum_map};
    Unit::new(
        UnitId(id),
        team,
        hex_from_offset(col, 0),
        0,  // armor
        0,  // armor_bonus
        0,  // damage_taken_bonus
        4,  // base_speed
        4,  // speed
        1,  // reactions_left
        1,  // reactions_max
        vec![],
        None,
        None,               // initiative
        Default::default(), // caster_context
        None,               // aoo_dice
        vec![],             // auras
        vec![],             // enemy_phases
        enum_map! {
            PoolKind::Hp     => Some((hp, 20)),
            PoolKind::Mana   => None,
            PoolKind::Rage   => None,
            PoolKind::Energy => None,
            PoolKind::Ap     => Some((2, 2)),
            PoolKind::Mp     => Some((4, 4)),
        },
        enum_map! {
            PoolKind::Hp     => RegenRule::None,
            PoolKind::Mana   => RegenRule::Increment(1),
            PoolKind::Rage   => RegenRule::None,
            PoolKind::Energy => RegenRule::Increment(1),
            PoolKind::Ap     => RegenRule::RefillToMax,
            PoolKind::Mp     => RegenRule::RefillToMax,
        },
        None,               // template_id
    )
}

fn hp(s: &CombatState, id: u64) -> i32 {
    s.unit(UnitId(id)).map(|u| u.hp()).unwrap_or(-1)
}

fn neutral_trap(id: u32, col: i32) -> EnvObject {
    EnvObject {
        id: EnvId(id),
        hex: hex_from_offset(col, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: None,
        revealed_to: TeamSet::EMPTY,
    }
}

// ── Test 1: snapshot filter (T3 + ownership) ─────────────────────────────────

/// Enemy-owned trap (empty `revealed_to`) is visible to the enemy AI snapshot
/// but absent from the player snapshot.
///
/// Exercises: `EnvObject::visible_to` + T3 snapshot filter.
#[test]
fn enemy_owned_trap_visible_to_enemy_ai_not_player() {
    use storyforge::combat_engine::state::EnvId;

    let enemy_actor = UnitBuilder::new(1, AppTeam::Enemy, hex_from_offset(0, 0)).build();
    let player_actor = UnitBuilder::new(2, AppTeam::Player, hex_from_offset(5, 0)).build();
    let trap_id = EnvId(42);

    // Build enemy snapshot — trap owned by Enemy is visible to enemy.
    let mut enemy_snap = snapshot_from(vec![enemy_actor.clone(), player_actor.clone()], 1);
    enemy_snap.state.environment.push(EnvObject {
        id: trap_id,
        hex: hex_from_offset(2, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: Some(Team::Enemy),
        revealed_to: TeamSet::EMPTY,
    });

    // Simulate what build_snapshot does: retain only visible-to-ai-team objects.
    // Enemy actor's team is Enemy → retain visible_to(Enemy).
    enemy_snap.state.environment.retain(|e| e.visible_to(AppTeam::Enemy));
    assert!(
        enemy_snap.state.environment.iter().any(|e| e.id == trap_id),
        "enemy-owned trap must be visible in enemy snapshot"
    );

    // Build player snapshot with the same trap but retain visible-to-Player.
    let mut player_snap = snapshot_from(vec![enemy_actor, player_actor], 1);
    player_snap.state.environment.push(EnvObject {
        id: trap_id,
        hex: hex_from_offset(2, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: Some(Team::Enemy),
        revealed_to: TeamSet::EMPTY,
    });
    player_snap.state.environment.retain(|e| e.visible_to(AppTeam::Player));
    assert!(
        !player_snap.state.environment.iter().any(|e| e.id == trap_id),
        "enemy-owned trap must be absent from player snapshot"
    );
}

// ── Test 2: AI soft-avoidance of own visible trap (T9 + T3) ──────────────────

/// Enemy actor's own `owner=Enemy` trap is visible in the enemy AI snapshot;
/// with a high severity and an equal-length alternative route the planned path
/// avoids the trap hex.
///
/// Grid (mirrors pathfinding::hazard_cost_reroutes_equal_length_path):
///   actor at (3,3), trap at (4,3), clean-pred at (4,2), goal at (5,2).
#[test]
fn enemy_ai_soft_avoids_own_visible_trap_when_alternative_exists() {
    use storyforge::combat_engine::state::EnvId;

    let actor = UnitBuilder::new(1, AppTeam::Enemy, hex_from_offset(3, 3))
        .movement_points(4)
        .build();

    let mut snap = snapshot_from(vec![actor.clone()], 1);
    let trap_hex = hex_from_offset(4, 3);
    let trap_id = EnvId(1);

    // Enemy-owned trap: visible to enemy, not to player.
    snap.state.environment.push(EnvObject {
        id: trap_id,
        hex: trap_hex,
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: Some(Team::Enemy),
        revealed_to: TeamSet::EMPTY,
    });
    // T3 filter: retain only objects visible to the actor's team (Enemy).
    snap.state.environment.retain(|e| e.visible_to(AppTeam::Enemy));
    // Severity > 0 → hazard_costs will route around.
    snap.cache.env_severity.insert(trap_id, 50.0);

    let actor_view = snap.unit(actor.entity).unwrap();
    let reach = ai_reach_from(&snap, actor_view);

    let goal = hex_from_offset(5, 2);
    let path = reach.path_to(goal).expect("goal must be reachable");

    assert!(
        !path.contains(&trap_hex),
        "AI path must avoid own visible trap; got {path:?}"
    );
    assert!(
        path.contains(&hex_from_offset(4, 2)),
        "AI path routes through clean predecessor; got {path:?}"
    );
}

// ── Test 3: hidden trap fires regardless of visibility ────────────────────────

/// A player unit stepping onto an `owner=Enemy`, `revealed_to=EMPTY` trap
/// still triggers the trap (firing is visibility-agnostic — reads full
/// `CombatState.environment`, not the filtered AI snapshot).
#[test]
fn player_steps_on_hidden_enemy_trap_and_it_fires() {
    let content = Stub::damage(3);
    let player = eng_unit(1, Team::Player, 0, 10);

    let mut state = CombatState::new(vec![player], 1, RoundPhase::ActorTurn, 0);
    // Enemy-owned trap that the player has NOT discovered.
    state.environment = vec![EnvObject {
        id: EnvId(5),
        hex: hex_from_offset(1, 0),
        kind: EnvKind::Hazard,
        ability: AbilityId::from("trap"),
        owner: Some(Team::Enemy),
        revealed_to: TeamSet::EMPTY,
    }];

    let path = vec![hex_from_offset(1, 0)];
    let (events, _) = step(
        &mut state,
        Action::Move { actor: UnitId(1), path },
        &mut ExpectedValue,
        &content,
    )
    .expect("move must succeed");

    // Trap fires: damage applied.
    assert_eq!(hp(&state, 1), 7, "hidden enemy trap deals 3 damage");
    // HazardTriggered event emitted.
    assert!(
        events.iter().any(|e| matches!(e, Event::HazardTriggered { victim, .. } if *victim == UnitId(1))),
        "HazardTriggered must be emitted even though trap was not visible to player"
    );
    // Trap removed after firing.
    assert!(state.environment.is_empty(), "one-shot trap removed after firing");
}

// ── Test 4: neutral trap fires on both teams ──────────────────────────────────

/// A neutral (`owner=None`) trap fires when stepped on by either team.
#[test]
fn neutral_trap_fires_on_both_teams() {
    let content = Stub::damage(2);

    for team in [Team::Player, Team::Enemy] {
        let mover = eng_unit(1, team, 0, 10);
        let mut state = CombatState::new(vec![mover], 1, RoundPhase::ActorTurn, 0);
        state.environment = vec![neutral_trap(1, 1)];

        let (events, _) = step(
            &mut state,
            Action::Move { actor: UnitId(1), path: vec![hex_from_offset(1, 0)] },
            &mut ExpectedValue,
            &content,
        )
        .expect("move must succeed");

        assert_eq!(hp(&state, 1), 8, "neutral trap deals 2 damage ({team:?})");
        assert!(
            events.iter().any(|e| matches!(e, Event::HazardTriggered { victim, .. } if *victim == UnitId(1))),
            "HazardTriggered for {team:?}"
        );
        assert!(state.environment.is_empty(), "trap removed after firing ({team:?})");
    }
}

// ── Test 5: AI-sim and prod hazard_costs agree (parity) ──────────────────────

/// Property/example test: the same team-filtered snapshot yields identical
/// `hazard_costs` for the same actor regardless of how the snapshot is reached.
///
/// Parity holds by construction: both `BattleSnapshot.state.environment` (the
/// input) and `BattleSnapshot.cache.env_severity` (the lookup table) are
/// serialised inside the snapshot.  Two independently-built snapshots with
/// the same contents must produce the same `hazard_costs` in `reach_from`.
///
/// We verify over two different snapshots (varying unit stats) that the
/// resulting paths to a common goal are identical — confirming the per-unit
/// stats do NOT influence hazard_costs.
#[test]
fn ai_sim_and_prod_hazard_costs_agree() {
    use storyforge::combat_engine::state::EnvId;

    let trap_hex = hex_from_offset(4, 3);
    let trap_id = EnvId(11);
    let severity = 25.0_f32;
    let goal = hex_from_offset(5, 2);

    // Two actors with different stats (HP, armor) — severity is unit-independent.
    let actor_a = UnitBuilder::new(1, AppTeam::Enemy, hex_from_offset(3, 3))
        .movement_points(4)
        .hp(10)
        .build();
    let actor_b = UnitBuilder::new(1, AppTeam::Enemy, hex_from_offset(3, 3))
        .movement_points(4)
        .hp(3)
        .armor(5)
        .build();

    let make_snap = |actor: storyforge::combat::ai::world::snapshot::UnitSnapshot| {
        let mut s = snapshot_from(vec![actor], 1);
        s.state.environment.push(EnvObject {
            id: trap_id,
            hex: trap_hex,
            kind: EnvKind::Hazard,
            ability: AbilityId::from("trap"),
            owner: Some(Team::Enemy),
            revealed_to: TeamSet::EMPTY,
        });
        s.state.environment.retain(|e| e.visible_to(AppTeam::Enemy));
        s.cache.env_severity.insert(trap_id, severity);
        s
    };

    let snap_a = make_snap(actor_a.clone());
    let snap_b = make_snap(actor_b.clone());

    let view_a = snap_a.unit(actor_a.entity).unwrap();
    let view_b = snap_b.unit(actor_b.entity).unwrap();

    let path_a = ai_reach_from(&snap_a, view_a).path_to(goal).expect("a reaches goal");
    let path_b = ai_reach_from(&snap_b, view_b).path_to(goal).expect("b reaches goal");

    assert_eq!(
        path_a, path_b,
        "hazard_costs is unit-independent: paths must be identical regardless of actor stats"
    );
    // Both avoid the trap hex (confirming severity was applied in both cases).
    assert!(
        !path_a.contains(&trap_hex),
        "both actors avoid the trap hex; path_a: {path_a:?}"
    );
}
