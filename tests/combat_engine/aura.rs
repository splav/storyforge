//! Engine integration tests for aura pure-presence query — Phase 4 step 4c.
//!
//! Covers:
//! - In-range aura gives speed/armor bonus; out-of-range does not.
//! - Dead aura source contributes nothing.
//! - Cross-team filter: ally-only aura doesn't affect enemies; enemy-only doesn't affect allies.
//! - Aura stun: actor adjacent to stun-aura source → AdvanceTurn skips them.
//! - Diff-on-move: target moves into aura radius → AuraStatusGained; moves out → AuraStatusLost.
//! - Aura source moves out of neighbours' radius → multiple AuraStatusLost events.
//! - Aura source dies (via aura_membership_set diff) → coverage lost for in-range targets.

use hexx::Hex;
use storyforge::combat_engine::{
    action::Action,
    content::{AuraDef, ContentView, StatusBonuses, TeamRelation},
    dice::ExpectedValue,
    event::{Event, TurnSkipReason},
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    step::step,
    StatusDef, StatusId,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn uid(n: u64) -> UnitId { UnitId(n) }

fn make_unit(id: UnitId, team: Team, pos: Hex, alive: bool) -> Unit {
    let hp = if alive { 10 } else { 0 };
    crate::common::engine_unit::EngineUnitBuilder::new(id.0)
        .team(team)
        .pos_hex(pos)
        .hp(hp, 10)
        .speed(3)
        .mp(20, 20)
        .build()
}

/// Build a `CombatState` from a unit list; turn-queue set to `order` at index 0.
fn make_state(units: Vec<Unit>, order: Vec<UnitId>) -> CombatState {
    let mut s = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);
    s.set_turn_queue(order.clone(), 0);
    s
}

/// ContentView providing status definitions for a single aura status.
///
/// Aura geometry (radius, applies_to) now lives in `Unit.auras`; this struct
/// only carries data needed for `status_bonuses` / `status_def` lookups.
struct AuraContent {
    radius: u32,
    status_id: StatusId,
    applies_to: TeamRelation,
    stun: bool,
    speed_bonus: i32,
    armor_bonus: i32,
    cached_def: StatusDef,
}

/// Attach one aura to a unit (helper for test construction).
fn with_aura(mut unit: Unit, content: &AuraContent) -> Unit {
    unit.auras = vec![AuraDef {
        radius: content.radius,
        status_id: content.status_id.clone(),
        applies_to: content.applies_to,
        affects_tags: std::collections::BTreeSet::new(),
    }];
    unit
}

impl AuraContent {
    fn build_def(stun: bool, speed_bonus: i32, armor_bonus: i32) -> StatusDef {
        StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: stun,
            bonuses: StatusBonuses { armor_bonus, damage_taken_bonus: 0, speed_bonus },
            hp_percent_dot: 0,
            heal_per_tick: 0,
        }
    }
    fn new(radius: u32, status: &str, applies_to: TeamRelation) -> Self {
        Self {
            radius,
            status_id: StatusId(status.to_string()),
            applies_to,
            stun: false,
            speed_bonus: 0,
            armor_bonus: 0,
            cached_def: Self::build_def(false, 0, 0),
        }
    }
    fn with_stun(mut self) -> Self {
        self.stun = true;
        self.cached_def = Self::build_def(true, self.speed_bonus, self.armor_bonus);
        self
    }
    fn with_speed(mut self, v: i32) -> Self {
        self.speed_bonus = v;
        self.cached_def = Self::build_def(self.stun, v, self.armor_bonus);
        self
    }
    fn with_armor(mut self, v: i32) -> Self {
        self.armor_bonus = v;
        self.cached_def = Self::build_def(self.stun, self.speed_bonus, v);
        self
    }
}

impl ContentView for AuraContent {
    fn status_bonuses(&self, id: &StatusId) -> StatusBonuses {
        if *id == self.status_id {
            StatusBonuses { speed_bonus: self.speed_bonus, armor_bonus: self.armor_bonus, damage_taken_bonus: 0 }
        } else {
            StatusBonuses::default()
        }
    }

    fn ability_def(&self, _: &storyforge::combat_engine::AbilityId)
        -> Option<&storyforge::combat_engine::AbilityDef> { None }

    fn status_def(&self, id: &StatusId) -> Option<&StatusDef> {
        if *id == self.status_id { Some(&self.cached_def) } else { None }
    }

    fn unit_template(&self, _: &str) -> Option<storyforge::combat_engine::UnitTemplate> { None }
}

// ── aura_effects_on query ─────────────────────────────────────────────────────

#[test]
fn in_range_gives_speed_bonus() {
    // src at (0,0), tgt at (1,0) — dist=1, radius=2 → in range.
    let src = uid(1);
    let tgt = uid(2);
    let content = AuraContent::new(2, "slow", TeamRelation::Enemies).with_speed(-1);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Player, Hex::new(1, 0), true),
        ],
        vec![tgt, src],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.speed_bonus, -1);
}

#[test]
fn out_of_range_gives_no_bonus() {
    // src at (0,0), tgt at (3,0) — dist=3, radius=2 → out of range.
    let src = uid(1);
    let tgt = uid(2);
    let content = AuraContent::new(2, "slow", TeamRelation::Enemies).with_speed(-1);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Player, Hex::new(3, 0), true),
        ],
        vec![tgt, src],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.speed_bonus, 0, "out-of-range: no bonus expected");
}

#[test]
fn dead_source_contributes_nothing() {
    let src = uid(1);
    let tgt = uid(2);
    // src is dead (hp=0).
    let content = AuraContent::new(5, "slow", TeamRelation::Enemies).with_speed(-2);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, false), &content), // dead
            make_unit(tgt, Team::Player, Hex::new(1, 0), true),
        ],
        vec![tgt, src],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.speed_bonus, 0, "dead source must not contribute");
}

/// Parametrize team-relation filter: (src_team, tgt_team, relation, should_apply).
#[test]
fn ally_only_aura_does_not_affect_enemy() {
    let src = uid(1); // Player
    let tgt = uid(2); // Enemy
    let content = AuraContent::new(5, "bless", TeamRelation::Allies).with_armor(2);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Player, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Enemy, Hex::new(1, 0), true),
        ],
        vec![src, tgt],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.armor_bonus, 0, "ally-only aura must not affect enemies");
}

#[test]
fn ally_only_aura_affects_ally() {
    let src = uid(1); // Player
    let tgt = uid(2); // Player (same team)
    let content = AuraContent::new(5, "bless", TeamRelation::Allies).with_armor(2);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Player, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Player, Hex::new(1, 0), true),
        ],
        vec![src, tgt],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.armor_bonus, 2, "ally-only aura must affect same-team units");
}

#[test]
fn enemy_aura_does_not_affect_same_team() {
    let src = uid(1); // Enemy
    let tgt = uid(2); // Enemy (same team)
    let content = AuraContent::new(5, "curse", TeamRelation::Enemies).with_speed(-1);
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Enemy, Hex::new(1, 0), true),
        ],
        vec![src, tgt],
    );
    let fx = state.aura_effects_on(tgt, &content);
    assert_eq!(fx.speed_bonus, 0, "enemy-targeted aura must not affect same-team unit");
}

// ── aura-stun: skip via AdvanceTurn ──────────────────────────────────────────

#[test]
fn aura_stun_causes_skip_on_advance_turn() {
    // Queue: [src(Player, aura-emitter), tgt(Player, stunned by src's ally-aura)].
    // src ends turn → AdvanceTurn → engine sees tgt is stunned by aura → TurnSkipped.
    let src = uid(1);
    let tgt = uid(2);
    // src emits ally-stun-aura radius=2 → tgt (ally, adjacent) gets stunned.
    let content = AuraContent::new(2, "stun_aura", TeamRelation::Allies).with_stun();
    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Player, Hex::ZERO, true), &content),
            make_unit(tgt, Team::Player, Hex::new(1, 0), true), // adjacent, in radius=2
        ],
        vec![src, tgt],
    );

    let mut state = state;
    let mut rng = ExpectedValue;
    let (events, _ctx) = step(
        &mut state,
        Action::EndTurn { actor: src },
        &mut rng,
        &content,
    )
    .expect("EndTurn must succeed");

    let skipped = events.iter().any(|e| matches!(
        e,
        Event::TurnSkipped { actor, reason: TurnSkipReason::Stunned } if *actor == tgt
    ));
    assert!(skipped, "aura-stunned actor must be skipped; events: {:#?}", events);
}

// ── diff-on-move: AuraStatusGained / AuraStatusLost ──────────────────────────

#[test]
fn aura_gained_when_mover_enters_radius() {
    // src (Enemy) at (0,0), radius=2.  mover (Player) starts at (-5,0), moves to (-2,0).
    // Before: dist=5 > 2 → not in set.  After: dist=2 ≤ 2 → in set → Gained.
    let src = uid(1);
    let mover = uid(2);
    let content = AuraContent::new(2, "curse", TeamRelation::Enemies);

    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(mover, Team::Player, Hex::new(-5, 0), true),
        ],
        vec![mover, src],
    );

    let path = vec![Hex::new(-4, 0), Hex::new(-3, 0), Hex::new(-2, 0)];
    let mut state = state;
    let mut rng = ExpectedValue;
    let (events, _ctx) = step(&mut state, Action::Move { actor: mover, path }, &mut rng, &content)
        .expect("Move must succeed");

    let gained = events.iter().any(|e| matches!(
        e,
        Event::AuraStatusGained { target, source, status_id }
        if *target == mover && *source == src && *status_id == StatusId("curse".to_string())
    ));
    assert!(gained, "AuraStatusGained expected when mover enters radius; events: {:#?}", events);
}

#[test]
fn aura_lost_when_mover_leaves_radius() {
    // src (Enemy) at (0,0), radius=2.  mover starts at (1,0) (in range), moves to (3,0).
    // After: dist=3 > 2 → AuraStatusLost.
    let src = uid(1);
    let mover = uid(2);
    let content = AuraContent::new(2, "curse", TeamRelation::Enemies);

    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(mover, Team::Player, Hex::new(1, 0), true),
        ],
        vec![mover, src],
    );

    let path = vec![Hex::new(2, 0), Hex::new(3, 0)];
    let mut state = state;
    let mut rng = ExpectedValue;
    let (events, _ctx) = step(&mut state, Action::Move { actor: mover, path }, &mut rng, &content)
        .expect("Move must succeed");

    let lost = events.iter().any(|e| matches!(
        e,
        Event::AuraStatusLost { target, source, status_id }
        if *target == mover && *source == src && *status_id == StatusId("curse".to_string())
    ));
    assert!(lost, "AuraStatusLost expected when mover leaves radius; events: {:#?}", events);
}

#[test]
fn source_moves_out_emits_lost_for_multiple_targets() {
    // src (Enemy) at (0,0), radius=2.
    // Two Player targets at (1,0) and (0,1) — both in radius.
    // src moves to (0,5) — both targets exit range → two AuraStatusLost.
    let src = uid(1);
    let tgt1 = uid(2);
    let tgt2 = uid(3);
    let content = AuraContent::new(2, "debuff", TeamRelation::Enemies);

    let state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(tgt1, Team::Player, Hex::new(1, 0), true),
            make_unit(tgt2, Team::Player, Hex::new(0, 1), true),
        ],
        vec![src, tgt1, tgt2],
    );

    // src moves north along y-axis away from both targets.
    let path = vec![
        Hex::new(0, 2),
        Hex::new(0, 3),
        Hex::new(0, 4),
        Hex::new(0, 5),
    ];
    // Note: (0,2) is 2 hexes from tgt2 at (0,1) — still in range. (0,3) is 3 → out of range.
    // So: after step to (0,2) — tgt2 still in range (dist=1); after (0,3) — out. tgt1 at (1,0)
    // exits radius once src crosses dist>2 from (1,0). This path moves along y so
    // dist((0,y),(1,0)) grows. hexx uses cube coords; let's verify the step emits ≥2 lost events.

    let mut state = state;
    let mut rng = ExpectedValue;
    let (events, _ctx) = step(&mut state, Action::Move { actor: src, path }, &mut rng, &content)
        .expect("Move must succeed");

    let lost_count = events
        .iter()
        .filter(|e| matches!(e, Event::AuraStatusLost { source, .. } if *source == src))
        .count();
    // Each target should emit exactly one AuraStatusLost as src leaves their radius.
    assert!(
        lost_count >= 2,
        "expected ≥2 AuraStatusLost (one per target); got {}; events: {:#?}",
        lost_count,
        events
    );
}

#[test]
fn source_death_removes_coverage_from_membership_set() {
    // Tests aura_membership_set diff directly (the mechanism used by step for Death effects).
    // Before: src alive → two targets covered. After: src dead → set empty.
    let src = uid(1);
    let tgt1 = uid(2);
    let tgt2 = uid(3);
    let content = AuraContent::new(2, "debuff", TeamRelation::Enemies);

    let mut state = make_state(
        vec![
            with_aura(make_unit(src, Team::Enemy, Hex::ZERO, true), &content),
            make_unit(tgt1, Team::Player, Hex::new(1, 0), true),
            make_unit(tgt2, Team::Player, Hex::new(-1, 0), true),
        ],
        vec![src, tgt1, tgt2],
    );

    // Snapshot before death.
    let before = state.aura_membership_set(&content);
    assert_eq!(before.len(), 2, "both targets in membership set before death");

    // Kill source — set pool HP to 0.
    {
        let u = state.unit_mut(src).unwrap();
        u.pools[combat_engine::PoolKind::Hp].as_mut().unwrap().0 = 0;
    }

    // Snapshot after death.
    let after = state.aura_membership_set(&content);
    assert!(after.is_empty(), "no coverage after source death");

    // The diff (lost) covers both targets.
    let lost: Vec<_> = before.difference(&after).collect();
    assert_eq!(lost.len(), 2, "two AuraStatusLost entries expected (one per target)");
}
