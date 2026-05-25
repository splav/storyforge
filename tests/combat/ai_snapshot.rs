/// Regression test: `build_snapshot` must include dead units (hp=0 markers).
///
/// Before the `HexPositions` → `HexCorpses` split, dead entities were removed
/// from `HexPositions` by the projector, causing `build_snapshot`'s
/// `positions.get(&c.entity)?` guard to silently drop them. AI accessors like
/// `dead_units()` and `all_enemies_of()` rely on the dead-unit rows being present.
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use crate::common::{fixtures::*, apps::engine::*};
use storyforge::combat::ai::config::difficulty::DifficultyProfile;
use storyforge::combat::ai::config::role::AxisProfile;
use storyforge::combat::ai::world::snapshot::build_snapshot;
use storyforge::combat::engine_bridge::{CombatStateRes, UnitIdMap};
use storyforge::game::components::{AiCombatantQ, Combatant, Dead, StatusEffects};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::hex_map::HexMap;
use storyforge::game::resources::{HexCorpses, HexPositions};

fn spawn_at(app: &mut App, pos: hexx::Hex, bundle: impl Bundle, name: &'static str) -> Entity {
    let e = app.world_mut().spawn((Name::new(name), bundle)).id();
    app.world_mut().resource_mut::<HexPositions>().insert(e, pos);
    e
}

/// `build_snapshot` must include dead combatants so that death-aware AI
/// accessors (`dead_units`, `all_enemies_of`) can find them.
#[test]
fn build_snapshot_includes_dead_combatant() {
    let mut app = movement_app();

    // Spawn a living and a dead enemy.
    let living = spawn_at(&mut app, hex_from_offset(3, 3), test_enemy(base_stats()), "Living");
    let dead   = spawn_at(&mut app, hex_from_offset(4, 3), test_enemy(base_stats()), "Dead");

    // Mark `dead` as dead: insert Dead component, move to HexCorpses, clear from HexPositions.
    app.world_mut().entity_mut(dead).insert(Dead);
    app.world_mut().get_mut::<storyforge::game::components::Vital>(dead).unwrap().hp = 0;
    {
        let pos = app.world().resource::<HexPositions>().get(&dead).unwrap();
        app.world_mut().resource_mut::<HexCorpses>().insert(dead, pos);
        app.world_mut().resource_mut::<HexPositions>().remove(&dead);
    }

    init_engine_state(&mut app);
    // DifficultyProfile is not in movement_app's default resources (it's injected
    // by the game plugin at startup). Insert a default for this test.
    app.insert_resource(DifficultyProfile::default());

    // Run build_snapshot as a one-shot system; return the entity set it produced.
    #[allow(clippy::type_complexity)]
    fn snapshot_system(
        combatants: Query<AiCombatantQ, With<Combatant>>,
        statuses:   Query<&StatusEffects>,
        hex_map:    HexMap,
        roles:      Query<&AxisProfile>,
        content:    Res<storyforge::content::content_view::ActiveContent>,
        difficulty: Res<DifficultyProfile>,
        state_res:  Res<CombatStateRes>,
        id_map:     Res<UnitIdMap>,
    ) -> Vec<Entity> {
        let snap = build_snapshot(
            1, &combatants, &statuses, &hex_map, &roles,
            &content, &difficulty,
            state_res.0.clone(),
            &id_map,
        );
        snap.cache.units.iter().map(|u| u.entity).collect()
    }

    let entities_in_cache: Vec<Entity> = app
        .world_mut()
        .run_system_once(snapshot_system)
        .expect("snapshot_system failed");

    assert!(
        entities_in_cache.contains(&living),
        "living entity must be in AiCache",
    );
    assert!(
        entities_in_cache.contains(&dead),
        "dead entity must be in AiCache (hp=0 marker for death-aware accessors)",
    );
}
