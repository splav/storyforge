//! Integration test for `combat::forecast::compute_forecast`.
//!
//! Covers:
//! 1. Correct `hp_before`/`hp_after`/`lethal` for a damage cast.
//! 2. `CombatStateRes` is unchanged after the forecast system runs.
//! 3. Forecast is cleared when no ability is selected.
//! 4. Correct `hp_after` for a heal cast.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use combat_engine::{state::RoundPhase, AbilityId, PoolKind};
use storyforge::combat::engine_bridge::{CombatStateRes, UnitIdMap};
use storyforge::combat::forecast::compute_forecast;
use storyforge::content::content_view::ActiveContent;
use storyforge::game::hex::hex_from_offset;
use storyforge::game::resources::{
    ActionForecast, ForecastKind, HexPositions, SelectionState, UiDirty, UiDirtyFlags,
};
use storyforge::ui::hex_grid::HexHover;

use crate::common::engine_unit::EngineUnitBuilder;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Minimal app for `compute_forecast` tests.
fn forecast_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<SelectionState>()
        .init_resource::<HexHover>()
        .init_resource::<ActiveContent>()
        .init_resource::<UiDirty>()
        .init_resource::<ActionForecast>();
    app
}

use combat_engine::content::{AbilityRange, AoEShape, EffectDef, TargetType};
use combat_engine::dice::DiceExpr;

fn damage_ability_def(dice: DiceExpr) -> combat_engine::AbilityDef {
    combat_engine::AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleEnemy,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        effect: EffectDef::Damage { dice },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    }
}

fn heal_ability_def(dice: DiceExpr) -> combat_engine::AbilityDef {
    combat_engine::AbilityDef {
        key: None,
        cost_ap: 1,
        costs: vec![],
        range: AbilityRange { min: 0, max: 5 },
        target_type: TargetType::SingleAlly,
        aoe: AoEShape::None,
        friendly_fire: false,
        requires_los: false,
        effect: EffectDef::Heal { dice },
        statuses: vec![],
        passive: vec![],
        requires_tags: Default::default(),
        excludes_tags: Default::default(),
    }
}

/// Populate the `ActiveContent` resource with a single engine ability def.
fn insert_engine_ability(app: &mut App, id: &str, def: combat_engine::AbilityDef) {
    use storyforge::content::abilities::AbilityDef;
    let bevy_def = AbilityDef {
        id: AbilityId::from(id),
        name: id.to_string(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: def,
    };
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .abilities
        .insert(AbilityId::from(id), bevy_def);
}

/// Build a two-unit `CombatState` and install it + `UnitIdMap` + `HexPositions`
/// into `app`. Returns `(actor_entity, target_entity)`.
fn setup_combat(app: &mut App, target_hp: i32) -> (Entity, Entity) {
    use combat_engine::state::{CombatState, Team};

    let actor_uid = combat_engine::state::UnitId(1);
    let target_uid = combat_engine::state::UnitId(2);

    let actor_pos = hex_from_offset(0, 0);
    let target_pos = hex_from_offset(1, 0);

    let actor = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos(0, 0)
        .hp_full(20)
        .ap(4, 4)
        .mp(6, 6)
        .build();
    let mut target = EngineUnitBuilder::new(2)
        .team(Team::Enemy)
        .pos(1, 0)
        .hp_full(20)
        .ap(4, 4)
        .mp(6, 6)
        .build();
    target.pools[PoolKind::Hp] = Some((target_hp, 20));

    let state = CombatState::new(vec![actor, target], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    // Spawn minimal ECS entities and wire UnitIdMap + HexPositions.
    let actor_entity = app.world_mut().spawn_empty().id();
    let target_entity = app.world_mut().spawn_empty().id();

    {
        let mut id_map = app.world_mut().resource_mut::<UnitIdMap>();
        id_map.insert(actor_entity, actor_uid);
        id_map.insert(target_entity, target_uid);
    }
    {
        let mut positions = app.world_mut().resource_mut::<HexPositions>();
        positions.insert(actor_entity, actor_pos);
        positions.insert(target_entity, target_pos);
    }

    (actor_entity, target_entity)
}

fn run_forecast(app: &mut App) {
    app.world_mut()
        .run_system_once(compute_forecast)
        .expect("compute_forecast failed");
}

// ── 1. Damage forecast: hp_before/hp_after/lethal ────────────────────────────

/// 1d4 under `ExpectedValue` → amount=3. Target hp=20 → hp_after=17, not lethal.
/// When target hp=3 → lethal=true, hp_after=0.
#[test]
fn forecast_damage_amounts_and_lethal() {
    let dice = DiceExpr::new(1, 4, 0); // expected = 3

    // ── surviving case ──
    {
        let mut app = forecast_app();
        let (actor_entity, target_entity) = setup_combat(&mut app, 20);
        insert_engine_ability(&mut app, "strike", damage_ability_def(dice));

        app.world_mut()
            .resource_mut::<SelectionState>()
            .selected_actor = Some(actor_entity);
        app.world_mut()
            .resource_mut::<SelectionState>()
            .selected_ability = Some(AbilityId::from("strike"));
        app.world_mut().resource_mut::<HexHover>().0 = Some(hex_from_offset(1, 0));
        app.world_mut().resource_mut::<UiDirty>().0 = UiDirtyFlags::FORECAST;

        run_forecast(&mut app);

        let forecast = app.world().resource::<ActionForecast>();
        let entry = forecast
            .entries
            .iter()
            .find(|e| e.entity == target_entity)
            .expect("forecast entry for target");

        assert_eq!(entry.kind, ForecastKind::Damage);
        assert_eq!(entry.amount, 3, "expected damage = 3 (1d4 mean)");
        assert_eq!(entry.hp_before, 20);
        assert_eq!(entry.hp_after, 17);
        assert!(!entry.lethal, "hp=20 survives 3 damage");
        assert_eq!(forecast.crit_fail_pct, 5);
    }

    // ── lethal case ──
    {
        let mut app = forecast_app();
        let (actor_entity, target_entity) = setup_combat(&mut app, 3); // hp == expected dmg
        insert_engine_ability(&mut app, "strike", damage_ability_def(dice));

        app.world_mut()
            .resource_mut::<SelectionState>()
            .selected_actor = Some(actor_entity);
        app.world_mut()
            .resource_mut::<SelectionState>()
            .selected_ability = Some(AbilityId::from("strike"));
        app.world_mut().resource_mut::<HexHover>().0 = Some(hex_from_offset(1, 0));
        app.world_mut().resource_mut::<UiDirty>().0 = UiDirtyFlags::FORECAST;

        run_forecast(&mut app);

        let forecast = app.world().resource::<ActionForecast>();
        let entry = forecast
            .entries
            .iter()
            .find(|e| e.entity == target_entity)
            .expect("forecast entry for target (lethal)");

        assert!(entry.lethal, "hp=3 should be lethal under 3 damage");
        assert_eq!(entry.hp_after, 0, "hp_after clamped at 0");
    }
}

// ── 2. CombatStateRes unchanged after forecast ────────────────────────────────

/// Running `compute_forecast` must not mutate `CombatStateRes`.
#[test]
fn forecast_does_not_mutate_combat_state() {
    let dice = DiceExpr::new(1, 4, 0);
    let mut app = forecast_app();
    let (actor_entity, _target_entity) = setup_combat(&mut app, 20);
    insert_engine_ability(&mut app, "strike", damage_ability_def(dice));

    // Snapshot hp before.
    let hp_before: i32 = {
        let state = &app.world().resource::<CombatStateRes>().0;
        state.unit(combat_engine::state::UnitId(2)).unwrap().pools[PoolKind::Hp]
            .map(|(c, _)| c)
            .unwrap_or(0)
    };

    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_actor = Some(actor_entity);
    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_ability = Some(AbilityId::from("strike"));
    app.world_mut().resource_mut::<HexHover>().0 = Some(hex_from_offset(1, 0));
    app.world_mut().resource_mut::<UiDirty>().0 = UiDirtyFlags::FORECAST;

    run_forecast(&mut app);

    let hp_after: i32 = {
        let state = &app.world().resource::<CombatStateRes>().0;
        state.unit(combat_engine::state::UnitId(2)).unwrap().pools[PoolKind::Hp]
            .map(|(c, _)| c)
            .unwrap_or(0)
    };

    assert_eq!(
        hp_before, hp_after,
        "compute_forecast must not mutate CombatStateRes"
    );
}

// ── 3. Forecast cleared when no ability selected ──────────────────────────────

/// If `selected_ability` is None, `ActionForecast` is cleared.
#[test]
fn forecast_cleared_when_no_ability() {
    let mut app = forecast_app();
    let (actor_entity, _) = setup_combat(&mut app, 20);

    // Pre-populate forecast to verify it gets cleared.
    {
        use storyforge::game::resources::ForecastEntry;
        let mut forecast = app.world_mut().resource_mut::<ActionForecast>();
        forecast.entries.push(ForecastEntry {
            entity: actor_entity,
            kind: ForecastKind::Damage,
            amount: 5,
            hp_before: 20,
            hp_after: 15,
            lethal: false,
            statuses: vec![],
        });
        forecast.crit_fail_pct = 5;
    }

    // No ability selected.
    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_actor = Some(actor_entity);
    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_ability = None;
    app.world_mut().resource_mut::<HexHover>().0 = Some(hex_from_offset(1, 0));
    app.world_mut().resource_mut::<UiDirty>().0 = UiDirtyFlags::FORECAST;

    run_forecast(&mut app);

    let forecast = app.world().resource::<ActionForecast>();
    assert!(
        forecast.is_empty(),
        "forecast must be cleared when no ability is selected"
    );
    assert_eq!(forecast.crit_fail_pct, 0);
}

// ── 4. Heal forecast ─────────────────────────────────────────────────────────

/// `+4 heal` on a target with hp=10, max_hp=20 → hp_after=14, not lethal.
#[test]
fn forecast_heal_amounts() {
    use combat_engine::state::Team;

    let dice = DiceExpr::new(1, 6, 0); // expected mean = 4 (round(3.5)=4)
    let mut app = forecast_app();

    // For a heal we need actor and target on the SAME team.
    let actor_uid = combat_engine::state::UnitId(1);
    let target_uid = combat_engine::state::UnitId(2);

    let actor = EngineUnitBuilder::new(1)
        .team(Team::Player)
        .pos(0, 0)
        .hp_full(20)
        .ap(4, 4)
        .mp(6, 6)
        .build();
    let mut target = EngineUnitBuilder::new(2)
        .team(Team::Player)
        .pos(1, 0)
        .hp_full(20)
        .ap(4, 4)
        .mp(6, 6)
        .build();
    target.pools[PoolKind::Hp] = Some((10, 20));

    let state =
        combat_engine::state::CombatState::new(vec![actor, target], 1, RoundPhase::ActorTurn, 0);
    app.world_mut().resource_mut::<CombatStateRes>().0 = state;

    let actor_entity = app.world_mut().spawn_empty().id();
    let target_entity = app.world_mut().spawn_empty().id();
    {
        let mut id_map = app.world_mut().resource_mut::<UnitIdMap>();
        id_map.insert(actor_entity, actor_uid);
        id_map.insert(target_entity, target_uid);
    }
    {
        let mut positions = app.world_mut().resource_mut::<HexPositions>();
        positions.insert(actor_entity, hex_from_offset(0, 0));
        positions.insert(target_entity, hex_from_offset(1, 0));
    }
    insert_engine_ability(&mut app, "heal", heal_ability_def(dice));

    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_actor = Some(actor_entity);
    app.world_mut()
        .resource_mut::<SelectionState>()
        .selected_ability = Some(AbilityId::from("heal"));
    app.world_mut().resource_mut::<HexHover>().0 = Some(hex_from_offset(1, 0));
    app.world_mut().resource_mut::<UiDirty>().0 = UiDirtyFlags::FORECAST;

    run_forecast(&mut app);

    let forecast = app.world().resource::<ActionForecast>();
    let entry = forecast
        .entries
        .iter()
        .find(|e| e.entity == target_entity)
        .expect("forecast entry for heal target");

    assert_eq!(entry.kind, ForecastKind::Heal);
    assert_eq!(entry.hp_before, 10);
    // 1d6 ExpectedValue = round(3.5) = 4
    assert_eq!(entry.amount, 4, "expected heal = 4 (1d6 mean)");
    assert_eq!(entry.hp_after, 14);
    assert!(!entry.lethal);
}
