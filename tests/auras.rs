//! Tests for the passive-aura refresh system (`apply_auras_system`).
//!
//! Each test builds a minimal Bevy world with only the systems we need, spawns
//! source and target entities, seeds their hex positions, runs one update, and
//! checks the resulting `StatusEffects`.

use bevy::prelude::*;

use storyforge::combat::auras::apply_auras_system;
use storyforge::content::encounters::AuraAffects;
use storyforge::core::StatusId;
use storyforge::game::components::{
    ActiveStatus, AuraSource, Combatant, Dead, Faction, StatusEffects, Team,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::HexPositions;

fn aura_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<HexPositions>()
        .add_systems(Update, apply_auras_system);
    app
}

fn set_pos(app: &mut App, e: Entity, col: i32, row: i32) {
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(e, hex_from_offset(col, row));
}

fn status_ids(app: &App, e: Entity) -> Vec<StatusId> {
    app.world()
        .get::<StatusEffects>(e)
        .map(|se| se.0.iter().map(|s| s.id.clone()).collect())
        .unwrap_or_default()
}

fn test_source(team: Team, status: &str, radius: u32, affects: AuraAffects) -> impl Bundle {
    (
        Combatant,
        Faction(team),
        StatusEffects::default(),
        AuraSource {
            status: status.into(),
            radius,
            affects,
        },
    )
}

fn test_target(team: Team) -> impl Bundle {
    (Combatant, Faction(team), StatusEffects::default())
}

#[test]
fn target_in_range_gets_aura_status() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 2, AuraAffects::Enemies))
        .id();
    let target = app.world_mut().spawn(test_target(Team::Player)).id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, target, 1, 0); // distance 1

    app.update();

    assert_eq!(status_ids(&app, target), vec!["disoriented".into()]);
}

#[test]
fn target_out_of_range_is_not_affected() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 1, AuraAffects::Enemies))
        .id();
    let target = app.world_mut().spawn(test_target(Team::Player)).id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, target, 3, 0); // distance 3, radius 1

    app.update();

    assert!(status_ids(&app, target).is_empty());
}

#[test]
fn leaving_radius_removes_aura_status() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 1, AuraAffects::Enemies))
        .id();
    let target = app.world_mut().spawn(test_target(Team::Player)).id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, target, 1, 0);
    app.update();
    assert_eq!(status_ids(&app, target), vec!["disoriented".into()]);

    // Move target out of range.
    set_pos(&mut app, target, 5, 0);
    app.update();

    assert!(
        status_ids(&app, target).is_empty(),
        "aura-applied status must clear once target leaves radius"
    );
}

#[test]
fn dead_source_removes_aura_status() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 2, AuraAffects::Enemies))
        .id();
    let target = app.world_mut().spawn(test_target(Team::Player)).id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, target, 1, 0);
    app.update();
    assert_eq!(status_ids(&app, target), vec!["disoriented".into()]);

    // Kill the source.
    app.world_mut().entity_mut(source).insert(Dead);
    app.update();

    assert!(
        status_ids(&app, target).is_empty(),
        "aura must clear when source dies"
    );
}

#[test]
fn affects_enemies_excludes_allies() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 2, AuraAffects::Enemies))
        .id();
    let ally = app.world_mut().spawn(test_target(Team::Enemy)).id();
    let enemy = app.world_mut().spawn(test_target(Team::Player)).id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, ally, 1, 0);
    set_pos(&mut app, enemy, 2, 0);

    app.update();

    assert!(
        status_ids(&app, ally).is_empty(),
        "affects=enemies must not apply to same-team unit"
    );
    assert_eq!(status_ids(&app, enemy), vec!["disoriented".into()]);
}

#[test]
fn affects_all_skips_source_itself() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 3, AuraAffects::All))
        .id();
    set_pos(&mut app, source, 0, 0);

    app.update();

    assert!(
        status_ids(&app, source).is_empty(),
        "aura must never target its own source"
    );
}

#[test]
fn ability_applied_status_is_not_stomped_and_survives_aura_refresh() {
    let mut app = aura_app();
    let source = app.world_mut()
        .spawn(test_source(Team::Enemy, "disoriented", 2, AuraAffects::Enemies))
        .id();
    // Pre-apply disoriented from a *different* applier (simulating an ability cast).
    let ghost_caster = app.world_mut().spawn_empty().id();
    let target = app
        .world_mut()
        .spawn((
            Combatant,
            Faction(Team::Player),
            StatusEffects(vec![ActiveStatus {
                id: "disoriented".into(),
                rounds_remaining: 5,
                applier: ghost_caster,
                dot_per_tick: 0,
            }]),
        ))
        .id();
    set_pos(&mut app, source, 0, 0);
    set_pos(&mut app, target, 1, 0);

    app.update();

    let se = app.world().get::<StatusEffects>(target).unwrap();
    assert_eq!(se.0.len(), 1, "no duplicate entries for same status id");
    let s = &se.0[0];
    assert_eq!(s.id, "disoriented".into());
    assert_eq!(
        s.applier, ghost_caster,
        "ability-applied status must NOT be overwritten by aura refresh"
    );
    assert_eq!(s.rounds_remaining, 5, "ability duration preserved");
}
