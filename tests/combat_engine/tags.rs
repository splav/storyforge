//! Slice B tag-predicate tests — engine legality, aura subset filter, and
//! serde round-trip for `AuraDef.affects_tags`.
//!
//! All content is synthetic; no real assets are touched.  Covers:
//!
//! - `check_legality`: `WrongTargetTags` for `requires_tags`/`excludes_tags`
//!   on `SingleEnemy` and `SingleAlly`.
//! - `Ground`/`Myself` abilities ignore tag predicates entirely.
//! - Aura `affects_tags` subset filter in `aura_effects_on`.
//! - Empty `affects_tags` behaves like pre-Slice-B (regression-safety).
//! - `AuraDef.affects_tags` round-trips through serde.

use std::collections::BTreeSet;

use hexx::Hex;
use storyforge::combat_engine::{
    check_legality,
    content::{AuraDef, ContentView, StatusBonuses, TeamRelation},
    legality::{ActionState, ActorView, IllegalReason, LegalAction, ProposedAction},
    state::{CombatState, RoundPhase, Team, Unit, UnitId},
    AbilityDef, AbilityId, AbilityRange, StatusDef, StatusId, TagId, TargetType, UnitTemplate,
};

// ── Minimal ContentView stub ──────────────────────────────────────────────────

struct TagContent {
    abilities: Vec<(AbilityId, AbilityDef)>,
    #[allow(dead_code)]
    status_id: StatusId,
    status_def: StatusDef,
}

impl ContentView for TagContent {
    fn ability_def(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }
    fn status_def(&self, _: &StatusId) -> Option<&StatusDef> {
        Some(&self.status_def)
    }
    fn unit_template(&self, _: &str) -> Option<UnitTemplate> {
        None
    }
    fn status_bonuses(&self, _: &StatusId) -> StatusBonuses {
        self.status_def.bonuses
    }
}

// ── EngineCheckState wrapper (mirrors step.rs pattern) ────────────────────────

struct StubState<'a> {
    state: &'a CombatState,
    content: &'a TagContent,
}

impl<'a> ActionState for StubState<'a> {
    type Id = UnitId;

    fn ability_def(&self, id: &AbilityId) -> Option<storyforge::combat_engine::AbilityDef> {
        self.content.ability_def(id).cloned()
    }
    fn status_def(&self, id: &StatusId) -> Option<StatusDef> {
        self.content.status_def(id).copied()
    }
    fn actor_view(&self, actor: UnitId) -> Option<ActorView> {
        let u = self.state.unit(actor)?;
        use storyforge::combat_engine::PoolKind;
        Some(ActorView {
            pos: u.pos,
            team: u.team,
            hp: u.hp(),
            ap: u.pools[PoolKind::Ap].map(|(c, _)| c).unwrap_or(0),
            pools: storyforge::combat_engine::enum_map::enum_map! {
                PoolKind::Hp     => None,
                PoolKind::Mana   => None,
                PoolKind::Rage   => None,
                PoolKind::Energy => None,
                PoolKind::Ap     => None,
                PoolKind::Mp     => None,
            },
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            is_alive: u.is_alive(),
        })
    }
    fn actor_knows_ability(&self, _: UnitId, _: &AbilityId) -> bool {
        true
    }
    fn is_target_alive(&self, target: UnitId) -> Option<bool> {
        self.state.unit(target).map(|u| u.is_alive())
    }
    fn target_team(&self, target: UnitId) -> Option<Team> {
        self.state.unit(target).map(|u| u.team)
    }
    fn taunters_for(&self, _: Team) -> Vec<UnitId> {
        vec![]
    }
    fn is_in_bounds(&self, _: Hex) -> bool {
        true
    }
    fn has_tags(
        &self,
        target: UnitId,
        requires: &BTreeSet<TagId>,
        excludes: &BTreeSet<TagId>,
    ) -> bool {
        self.state
            .unit(target)
            .is_some_and(|u| requires.is_subset(&u.tags) && excludes.is_disjoint(&u.tags))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn tag(s: &str) -> TagId {
    TagId::from(s)
}
fn tags(v: &[&str]) -> BTreeSet<TagId> {
    v.iter().map(|s| tag(s)).collect()
}
fn aid(s: &str) -> AbilityId {
    AbilityId::from(s)
}
fn uid(n: u64) -> UnitId {
    UnitId(n)
}

fn unit_with_tags(id: u64, team: Team, t: BTreeSet<TagId>) -> Unit {
    let mut u = crate::common::engine_unit::EngineUnitBuilder::new(id)
        .team(team)
        .build();
    u.tags = t;
    u
}

fn enemy_ability(requires: BTreeSet<TagId>, excludes: BTreeSet<TagId>) -> AbilityDef {
    AbilityDef {
        target_type: TargetType::SingleEnemy,
        requires_tags: requires,
        excludes_tags: excludes,
        ..AbilityDef::default()
    }
}

fn ally_ability(requires: BTreeSet<TagId>, excludes: BTreeSet<TagId>) -> AbilityDef {
    AbilityDef {
        target_type: TargetType::SingleAlly,
        requires_tags: requires,
        excludes_tags: excludes,
        ..AbilityDef::default()
    }
}

fn make_content(id: &str, def: AbilityDef) -> TagContent {
    TagContent {
        abilities: vec![(aid(id), def)],
        status_id: StatusId::from("aura_s"),
        status_def: StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: false,
            bonuses: StatusBonuses::default(),
            hp_percent_dot: 0,
            heal_per_tick: 0,
            ..Default::default()
        },
    }
}

fn propose<'a>(
    actor: UnitId,
    ability: &'a AbilityId,
    target: UnitId,
) -> ProposedAction<'a, UnitId> {
    ProposedAction {
        actor,
        ability,
        target,
        target_pos: Hex::ZERO,
    }
}

fn make_state_two(actor: Unit, target: Unit) -> CombatState {
    let a_id = actor.id;
    let t_id = target.id;
    let mut s = CombatState::new(vec![actor, target], 1, RoundPhase::ActorTurn, 0);
    s.set_turn_queue(vec![a_id, t_id], 0);
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Engine legality: requires_tags / excludes_tags
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn requires_tags_met_is_legal() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, tags(&["symbiote"]));
    let state = make_state_two(actor, target);
    let ab_id = aid("atk");
    let content = make_content("atk", enemy_ability(tags(&["symbiote"]), BTreeSet::new()));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Ok(LegalAction {
            disadvantage: false
        })
    );
}

#[test]
fn requires_tags_missing_returns_wrong_target_tags() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, BTreeSet::new()); // no tags
    let state = make_state_two(actor, target);
    let ab_id = aid("atk");
    let content = make_content("atk", enemy_ability(tags(&["symbiote"]), BTreeSet::new()));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Err(IllegalReason::WrongTargetTags)
    );
}

#[test]
fn excludes_tags_present_returns_wrong_target_tags() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, tags(&["living"]));
    let state = make_state_two(actor, target);
    let ab_id = aid("atk");
    let content = make_content("atk", enemy_ability(BTreeSet::new(), tags(&["living"])));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Err(IllegalReason::WrongTargetTags)
    );
}

#[test]
fn excludes_tags_absent_is_legal() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, tags(&["undead"]));
    let state = make_state_two(actor, target);
    let ab_id = aid("atk");
    // excludes "living" but target has "undead" only → legal
    let content = make_content("atk", enemy_ability(BTreeSet::new(), tags(&["living"])));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Ok(LegalAction {
            disadvantage: false
        })
    );
}

#[test]
fn single_ally_with_requires_tags_legal() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let ally = unit_with_tags(2, Team::Player, tags(&["blessed"]));
    let state = make_state_two(actor, ally);
    let ab_id = aid("heal");
    let content = make_content("heal", ally_ability(tags(&["blessed"]), BTreeSet::new()));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Ok(LegalAction {
            disadvantage: false
        })
    );
}

#[test]
fn single_ally_missing_required_tag_is_wrong() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let ally = unit_with_tags(2, Team::Player, BTreeSet::new());
    let state = make_state_two(actor, ally);
    let ab_id = aid("heal");
    let content = make_content("heal", ally_ability(tags(&["blessed"]), BTreeSet::new()));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Err(IllegalReason::WrongTargetTags)
    );
}

#[test]
fn ground_ability_ignores_tags() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, BTreeSet::new());
    let state = make_state_two(actor, target);
    let ab_id = aid("ground");
    let def = AbilityDef {
        target_type: TargetType::Ground,
        requires_tags: tags(&["symbiote"]),
        ..AbilityDef::default()
    };
    let content = make_content("ground", def);
    let s = StubState {
        state: &state,
        content: &content,
    };
    // Ground skips tag check entirely — should be Ok
    assert!(check_legality(propose(uid(1), &ab_id, uid(2)), &s).is_ok());
}

#[test]
fn myself_ability_ignores_tags() {
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, BTreeSet::new());
    let state = make_state_two(actor, target);
    let ab_id = aid("self_cast");
    let def = AbilityDef {
        target_type: TargetType::Myself,
        requires_tags: tags(&["symbiote"]),
        range: AbilityRange { min: 0, max: 0 },
        ..AbilityDef::default()
    };
    let content = make_content("self_cast", def);
    let s = StubState {
        state: &state,
        content: &content,
    };
    // Myself cast on actor itself — passes regardless of requires_tags
    let actor_proposal = ProposedAction {
        actor: uid(1),
        ability: &ab_id,
        target: uid(1),
        target_pos: Hex::ZERO,
    };
    assert!(check_legality(actor_proposal, &s).is_ok());
}

#[test]
fn empty_tags_on_both_sides_is_always_legal() {
    // Regression: empty requires/excludes with untagged target → legal (no regression)
    let actor = unit_with_tags(1, Team::Player, BTreeSet::new());
    let target = unit_with_tags(2, Team::Enemy, BTreeSet::new());
    let state = make_state_two(actor, target);
    let ab_id = aid("atk");
    let content = make_content("atk", enemy_ability(BTreeSet::new(), BTreeSet::new()));
    let s = StubState {
        state: &state,
        content: &content,
    };
    assert_eq!(
        check_legality(propose(uid(1), &ab_id, uid(2)), &s),
        Ok(LegalAction {
            disadvantage: false
        })
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Aura affects_tags subset filter
// ─────────────────────────────────────────────────────────────────────────────

fn aura_content_with_tags(_affects_tags: BTreeSet<TagId>) -> TagContent {
    let status_id = StatusId::from("aura_s");
    TagContent {
        abilities: vec![],
        status_id: status_id.clone(),
        status_def: StatusDef {
            causes_disadvantage: false,
            blocks_mana_abilities: false,
            forces_targeting: false,
            skips_turn: false,
            bonuses: StatusBonuses {
                runtime: storyforge::combat_engine::RuntimeStatsDelta(
                    storyforge::combat_engine::RuntimeStats {
                        armor: 0,
                        magic_resist: 0,
                        base_speed: 5,
                    },
                ),
            },
            hp_percent_dot: 0,
            heal_per_tick: 0,
            ..Default::default()
        },
    }
    // We build the unit with an AuraDef that has affects_tags; TagContent
    // only provides status lookups.
}

fn unit_with_aura(id: u64, team: Team, radius: u32, affects_tags: BTreeSet<TagId>) -> Unit {
    let mut u = crate::common::engine_unit::EngineUnitBuilder::new(id)
        .team(team)
        .speed(6)
        .build();
    u.auras = vec![AuraDef {
        radius,
        status_id: StatusId::from("aura_s"),
        applies_to: TeamRelation::All,
        affects_tags,
    }];
    u
}

#[test]
fn aura_with_affects_tags_applies_only_to_tagged_target() {
    let source = unit_with_aura(1, Team::Player, 3, tags(&["symbiote"]));
    let tagged = {
        let mut u = crate::common::engine_unit::EngineUnitBuilder::new(2)
            .team(Team::Enemy)
            .build();
        u.tags = tags(&["symbiote"]);
        u
    };
    let untagged = crate::common::engine_unit::EngineUnitBuilder::new(3)
        .team(Team::Enemy)
        .build(); // no tags

    let mut state = CombatState::new(vec![source, tagged, untagged], 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![uid(1), uid(2), uid(3)], 0);
    let content = aura_content_with_tags(tags(&["symbiote"]));

    // Tagged target receives aura speed bonus
    let fx_tagged = state.aura_effects_on(uid(2), &content);
    assert_eq!(
        fx_tagged.runtime.0.base_speed, 5,
        "tagged target should receive speed bonus"
    );

    // Untagged target receives nothing
    let fx_untagged = state.aura_effects_on(uid(3), &content);
    assert_eq!(
        fx_untagged.runtime.0.base_speed, 0,
        "untagged target should not receive bonus"
    );
}

#[test]
fn aura_empty_affects_tags_applies_to_all_targets() {
    // Regression: existing tests unaffected — empty affects_tags means no filter
    let source = unit_with_aura(1, Team::Player, 3, BTreeSet::new());
    let target = crate::common::engine_unit::EngineUnitBuilder::new(2)
        .team(Team::Enemy)
        .build();

    let mut state = CombatState::new(vec![source, target], 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![uid(1), uid(2)], 0);
    let content = aura_content_with_tags(BTreeSet::new());

    let fx = state.aura_effects_on(uid(2), &content);
    assert_eq!(
        fx.runtime.0.base_speed, 5,
        "empty affects_tags should apply to all targets"
    );
}

#[test]
fn aura_membership_set_respects_affects_tags() {
    let source = unit_with_aura(1, Team::Player, 3, tags(&["symbiote"]));
    let tagged = {
        let mut u = crate::common::engine_unit::EngineUnitBuilder::new(2)
            .team(Team::Enemy)
            .build();
        u.tags = tags(&["symbiote"]);
        u
    };
    let untagged = crate::common::engine_unit::EngineUnitBuilder::new(3)
        .team(Team::Enemy)
        .build();

    let mut state = CombatState::new(vec![source, tagged, untagged], 1, RoundPhase::ActorTurn, 0);
    state.set_turn_queue(vec![uid(1), uid(2), uid(3)], 0);
    let content = aura_content_with_tags(tags(&["symbiote"]));

    let membership = state.aura_membership_set(&content);
    let status = StatusId::from("aura_s");

    assert!(
        membership.contains(&(uid(2), uid(1), status.clone())),
        "tagged target in membership"
    );
    assert!(
        !membership.contains(&(uid(3), uid(1), status)),
        "untagged target not in membership"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AuraDef serde round-trip for affects_tags
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn aura_def_affects_tags_roundtrip_nonempty() {
    let aura = AuraDef {
        radius: 2,
        status_id: StatusId::from("test_s"),
        applies_to: TeamRelation::Enemies,
        affects_tags: tags(&["undead", "beast"]),
    };
    let json = serde_json::to_string(&aura).unwrap();
    let back: AuraDef = serde_json::from_str(&json).unwrap();
    assert_eq!(back, aura);
}

#[test]
fn aura_def_affects_tags_roundtrip_empty() {
    let aura = AuraDef {
        radius: 1,
        status_id: StatusId::from("s"),
        applies_to: TeamRelation::All,
        affects_tags: BTreeSet::new(),
    };
    let json = serde_json::to_string(&aura).unwrap();
    let back: AuraDef = serde_json::from_str(&json).unwrap();
    assert_eq!(back, aura);
}

#[test]
fn aura_def_affects_tags_serde_default_for_missing_key() {
    // Pre-v48 wire shape has no affects_tags key → deserializes to empty.
    let json = r#"{"radius":1,"status_id":"s","applies_to":"all"}"#;
    let back: AuraDef = serde_json::from_str(json).unwrap();
    assert!(back.affects_tags.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// TOML parse: synthetic ability with requires_tags / excludes_tags
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn toml_ability_requires_tags_parses_to_engine_def() {
    let toml = r#"
[[abilities]]
id = "symbiote_strike"
name = "Symbiote Strike"
target_type = "single_enemy"
requires_tags = ["symbiote"]
excludes_tags = ["undead"]
"#;
    let defs = storyforge::content::abilities::parse_abilities("test.toml", toml);
    assert_eq!(defs.len(), 1);
    let def = &defs[0];
    assert!(def.engine.requires_tags.contains(&TagId::from("symbiote")));
    assert!(def.engine.excludes_tags.contains(&TagId::from("undead")));
}

#[test]
fn toml_ability_missing_tag_fields_defaults_to_empty() {
    let toml = r#"
[[abilities]]
id = "basic_attack"
name = "Basic Attack"
target_type = "single_enemy"
"#;
    let defs = storyforge::content::abilities::parse_abilities("test.toml", toml);
    assert_eq!(defs.len(), 1);
    assert!(defs[0].engine.requires_tags.is_empty());
    assert!(defs[0].engine.excludes_tags.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// TOML parse: enemy tags and aura affects_tags
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn toml_enemy_tags_parse_correctly() {
    use storyforge::content::encounters::load_encounters_from_str;

    let toml = r#"
[[encounters]]
id = "test_enc"
name = "Test"

[encounters.victory]
type = "all_enemies_dead"

[[encounters.enemies]]
name = "Symbiote"
race = "alien"
hex_col = 0
hex_row = 0
speed = 4
ability_ids = ["basic_attack"]
tags = ["symbiote", "living"]

[encounters.enemies.stats]
max_hp = 10
strength = 5
dexterity = 5
constitution = 5
intelligence = 5
wisdom = 5
charisma = 5

[encounters.enemies.equipment]
main_hand = "unarmed"
chest = "cloth"
legs = "cloth"
feet = "cloth"
"#;
    let encs = load_encounters_from_str(
        "test_scenario",
        "test.toml",
        toml,
        &std::collections::HashMap::new(),
    );
    let enc = encs.iter().find(|e| e.id == "test_enc").unwrap();
    let enemy = &enc.enemies[0];
    assert!(enemy.tags.contains(&TagId::from("symbiote")));
    assert!(enemy.tags.contains(&TagId::from("living")));
}

#[test]
fn toml_aura_affects_tags_parse_correctly() {
    use storyforge::content::encounters::load_encounters_from_str;

    let toml = r#"
[[encounters]]
id = "test_enc"
name = "Test"

[encounters.victory]
type = "all_enemies_dead"

[[encounters.enemies]]
name = "Aura Boss"
race = "alien"
hex_col = 0
hex_row = 0
speed = 4
ability_ids = ["basic_attack"]

[encounters.enemies.stats]
max_hp = 20
strength = 5
dexterity = 5
constitution = 5
intelligence = 5
wisdom = 5
charisma = 5

[encounters.enemies.equipment]
main_hand = "unarmed"
chest = "cloth"
legs = "cloth"
feet = "cloth"

[encounters.enemies.aura]
status = "slowed"
radius = 3
affects = "enemies"
affects_tags = ["symbiote"]
"#;
    let encs = load_encounters_from_str(
        "test_scenario",
        "test.toml",
        toml,
        &std::collections::HashMap::new(),
    );
    let enc = encs.iter().find(|e| e.id == "test_enc").unwrap();
    let enemy = &enc.enemies[0];
    let aura = enemy.aura.as_ref().unwrap();
    assert!(aura.affects_tags.contains(&TagId::from("symbiote")));
}
