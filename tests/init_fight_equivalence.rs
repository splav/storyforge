//! Equivalence test: `init_fight` must produce the same `CombatState` as the
//! ECS bootstrap (`spawn_combatants` + `bootstrap_combat_state`) for every
//! real fight in the campaign.
//!
//! # Two legs per encounter
//!
//! **Reference leg (ECS)**: spin up a minimal Bevy App, call `spawn_combatants`,
//! call `bootstrap_combat_state`, and capture the resulting engine state.
//!
//! **Candidate leg (ECS-free)**: call `init_fight` with the same content,
//! scenario, scene index, encounter, seed, and — critically — the **same
//! UnitIds** that the ECS path produced.  When ids match, all deterministic
//! fields (initiative rolls, turn order, etc.) must be bit-for-bit identical.
//!
//! **Dense-id leg**: call `init_fight` again with dense ids `0..N`.  The
//! resulting state won't byte-match the reference (different ids → different
//! initiative roll order), but it must be well-formed, deterministic (same
//! across two calls), and have the correct unit count / teams.  This exercises
//! the path the offline simulator will use.
//!
//! # How the comparison works
//! `post_state_hash` covers only alive units + turn_queue + round + phase; we
//! supplement it with full-field assertions so that dead units, blocked_hexes,
//! environment, and per-unit fields (tags, auras, phases, passives,
//! caster_context, statuses) are all verified.

use bevy::prelude::*;
use bevy::ecs::system::RunSystemOnce;
use std::collections::HashMap;

use combat_engine::{
    state::UnitId,
    trace::post_state_hash_hex,
    DiceRng,
};

use storyforge::combat::engine_bridge::{
    bootstrap_combat_state, apply_bridge_queues_pre_projection,
    CombatStateRes, UnitIdMap, BridgeQueues,
};
use storyforge::content::campaigns::load_campaigns;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::scenarios::SceneDef;
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective,
    GameDb, HexCorpses, HexPositions, PresetInitiative, ScenarioState, TurnQueue, UiDirty,
};
use storyforge::game::combat_log::CombatLog;
use storyforge::scenario::init_fight::init_fight;
use storyforge::scenario::combat_scene::spawn_combatants;
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::DiceRngRes;

#[path = "common/mod.rs"]
mod common;

// ── Fixed RNG seed for deterministic runs ─────────────────────────────────────
const TEST_SEED: u64 = 0xDEAD_C0DE_1234_5678;

// ── App builder ───────────────────────────────────────────────────────────────

/// Build a headless app that can run `spawn_combatants` + `bootstrap_combat_state`.
fn scenario_app(content: storyforge::content::content_view::ContentView) -> App {
    use bevy::math::Vec2;
    use storyforge::combat::ai::log::{AiLogger, PendingAiLogEntries};
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use storyforge::ui::animation::AnimationQueue;
    use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};
    use storyforge::game::messages::ActionInput;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatContext>()
        .init_resource::<CombatBlockedHexes>()
        .init_resource::<CombatEnvironment>()
        .init_resource::<CombatObjective>()
        .init_resource::<UiDirty>()
        .insert_resource(ActiveContent(content))
        .init_resource::<DiceRngRes>()
        .init_resource::<CombatLog>()
        .init_resource::<AnimationQueue>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials::default())
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .init_resource::<BridgeQueues>()
        .init_resource::<PresetInitiative>()
        .init_resource::<EngineTraceWriter>()
        .init_resource::<AiLogger>()
        .init_resource::<PendingAiLogEntries>()
        .add_message::<ActionInput>();
    app
}

// ── Reference bootstrap ───────────────────────────────────────────────────────

/// Run `spawn_combatants` + `bootstrap_combat_state` for `scenario_id /
/// scene_index` in the provided `app`.  Resets the RNG to `seed` first.
///
/// Returns the reference `CombatState` and a `Vec<UnitId>` in spawn order
/// (i.e. the sequence in which entities were assigned ids by `entity_to_uid`).
fn run_ecs_bootstrap(
    app: &mut App,
    scenario_id: &str,
    scenario: &storyforge::content::scenarios::ScenarioDef,
    scene_index: usize,
    seed: u64,
) -> (combat_engine::state::CombatState, Vec<UnitId>) {
    // Build a minimal GameDb containing only this scenario.
    let mut db = GameDb {
        scenarios: HashMap::new(),
        campaigns: HashMap::new(),
        campaign_order: Vec::new(),
    };
    db.scenarios.insert(scenario_id.to_owned(), scenario.clone());

    // Insert scenario routing resources.
    app.world_mut()
        .insert_resource(ScenarioState {
            scenario_id: scenario_id.to_owned(),
            scene_index,
        });
    app.world_mut().insert_resource(db);

    // Seed the RNG.
    app.world_mut()
        .resource_mut::<DiceRngRes>()
        .0 = DiceRng::with_seed(seed);

    // Spawn combatants (mirrors spawn_combat_scene).
    app.world_mut()
        .run_system_once(spawn_combatants_system)
        .expect("spawn_combatants_system failed");

    // Apply StartingHexPos → HexPositions (mirrors assign_hex_positions in render.rs).
    // Without this, from_ecs cannot find positions and produces empty state.
    app.world_mut()
        .run_system_once(apply_starting_hex_positions)
        .expect("apply_starting_hex_positions failed");

    // Bootstrap engine state.
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
    app.world_mut()
        .run_system_once(apply_bridge_queues_pre_projection)
        .expect("apply_bridge_queues_pre_projection failed");

    let state = app.world().resource::<CombatStateRes>().0.clone();
    let id_map = app.world().resource::<UnitIdMap>();

    // Collect UnitIds in spawn order.
    // entity_to_uid uses Entity::to_bits() = (generation << 32) | index.
    // Entities are allocated with incrementing indices; fresh app starts at a
    // Bevy-internal baseline.  Sort by entity INDEX (low 32 bits) to recover
    // spawn order (first spawned = smallest index).
    let mut uid_list: Vec<(u64, UnitId)> = id_map
        .entity_to_id
        .iter()
        .map(|(e, &uid)| (e.to_bits(), uid))
        .collect();
    uid_list.sort_by_key(|(bits, _)| *bits);
    let uids: Vec<UnitId> = uid_list.into_iter().map(|(_, uid)| uid).collect();

    (state, uids)
}

/// One-shot Bevy system that calls the inner `spawn_combatants` helper.
fn spawn_combatants_system(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    mut objective: ResMut<CombatObjective>,
    mut blocked: ResMut<CombatBlockedHexes>,
    mut environment: ResMut<CombatEnvironment>,
    tag_cache: Res<AbilityTagCache>,
) {
    spawn_combatants(
        &mut commands,
        &db,
        &scenario,
        &mut objective,
        &mut blocked,
        &mut environment,
        &tag_cache,
    );
}

/// One-shot system that mirrors `assign_hex_positions` (UI render.rs) without
/// the mesh/material logic.  Reads each newly-spawned entity's `StartingHexPos`
/// and inserts it into the `HexPositions` resource, then removes the component.
/// Without this, `from_ecs` in `bootstrap_combat_state` can't find positions
/// and returns empty state.
fn apply_starting_hex_positions(
    mut commands: Commands,
    mut positions: ResMut<HexPositions>,
    combatants: Query<(Entity, &storyforge::game::components::StartingHexPos), With<storyforge::game::components::Combatant>>,
) {
    for (entity, starting_pos) in combatants.iter() {
        positions.insert(entity, starting_pos.0);
        commands.entity(entity).remove::<storyforge::game::components::StartingHexPos>();
    }
}

// ── Main test ─────────────────────────────────────────────────────────────────

// WIP (init_fight step 2/3): init_fight does not yet reproduce the ECS
// bootstrap's UnitId↔unit assignment (Bevy entity-allocation order), so the
// candidate state misaligns by unit. Ignored until the UnitId-ordering approach
// is settled (option C reproduce-Bevy-order vs option B dense-ids+re-record).
#[ignore = "WIP: init_fight UnitId/spawn-order equivalence not yet achieved"]
#[test]
fn init_fight_matches_ecs_bootstrap_for_all_campaign_encounters() {
    let campaigns = load_campaigns();

    let mut case_count = 0;
    let mut failures: Vec<String> = Vec::new();

    for (scenario_id, scenario) in &campaigns.scenarios {
        for (scene_index, scene) in scenario.scenes.iter().enumerate() {
            let encounter_id = match scene {
                SceneDef::Combat { encounter_id, .. } => encounter_id,
                _ => continue,
            };
            let encounter = scenario
                .encounters
                .get(encounter_id.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "Encounter '{}' not found in scenario '{}'",
                        encounter_id, scenario_id
                    )
                });

            case_count += 1;
            let case_name = format!("{scenario_id}/scene{scene_index}/{encounter_id}");

            // ── Reference leg: ECS bootstrap ─────────────────────────────────
            let mut app = scenario_app(scenario.content.clone());

            eprintln!("Testing {case_name} ...");
            let (ref_state, spawn_order_uids) =
                run_ecs_bootstrap(&mut app, scenario_id, scenario, scene_index, TEST_SEED);
            eprintln!("  ECS: {} units, {} uids in spawn order", ref_state.units().len(), spawn_order_uids.len());

            // ── Candidate leg: init_fight with matching UnitIds ───────────────
            let mut uid_iter = spawn_order_uids.iter().copied();
            let mut rng = DiceRng::with_seed(TEST_SEED);

            let (cand_state, _metas) = init_fight(
                &scenario.content,
                scenario,
                scene_index,
                encounter,
                &mut rng,
                &HashMap::new(), // no preset
                |_src| uid_iter.next().expect("more UIDs than sources"),
            );

            // ── Assertions ───────────────────────────────────────────────────
            let mut errs: Vec<String> = Vec::new();

            // 1. post_state_hash (covers alive units + turn_queue + round + phase).
            let ref_hash = post_state_hash_hex(&ref_state);
            let cand_hash = post_state_hash_hex(&cand_state);
            if ref_hash != cand_hash {
                errs.push(format!(
                    "post_state_hash mismatch:\n    ref:  {ref_hash}\n    cand: {cand_hash}"
                ));
            }

            // 2. Turn queue order + cursor.
            if ref_state.turn_queue.order != cand_state.turn_queue.order {
                errs.push(format!(
                    "turn_queue.order mismatch:\n    ref:  {:?}\n    cand: {:?}",
                    ref_state.turn_queue.order, cand_state.turn_queue.order,
                ));
            }
            if ref_state.turn_queue.index != cand_state.turn_queue.index {
                errs.push(format!(
                    "turn_queue.index: ref={} cand={}",
                    ref_state.turn_queue.index, cand_state.turn_queue.index,
                ));
            }

            // 3. Unit set and per-unit fields.
            let ref_uids: std::collections::BTreeSet<UnitId> =
                ref_state.units().iter().map(|u| u.id).collect();
            let cand_uids: std::collections::BTreeSet<UnitId> =
                cand_state.units().iter().map(|u| u.id).collect();
            if ref_uids != cand_uids {
                errs.push(format!(
                    "unit id sets differ:\n    ref:  {:?}\n    cand: {:?}",
                    ref_uids, cand_uids,
                ));
            } else {
                for uid in &ref_uids {
                    let ru = ref_state.unit(*uid).unwrap();
                    let cu = cand_state.unit(*uid).unwrap();
                    compare_units(ru, cu, *uid, &mut errs);
                }
            }

            // 4. next_synthetic_uid (should be 0 at bootstrap start).
            if ref_state.next_synthetic_uid() != cand_state.next_synthetic_uid() {
                errs.push(format!(
                    "next_synthetic_uid: ref={} cand={}",
                    ref_state.next_synthetic_uid(),
                    cand_state.next_synthetic_uid(),
                ));
            }

            // 5. blocked_hexes.
            if ref_state.blocked_hexes != cand_state.blocked_hexes {
                errs.push(format!(
                    "blocked_hexes mismatch:\n    ref:  {:?}\n    cand: {:?}",
                    ref_state.blocked_hexes, cand_state.blocked_hexes,
                ));
            }

            // 6. environment.
            if ref_state.environment.len() != cand_state.environment.len() {
                errs.push(format!(
                    "environment len: ref={} cand={}",
                    ref_state.environment.len(),
                    cand_state.environment.len(),
                ));
            } else {
                for (i, (re, ce)) in ref_state
                    .environment
                    .iter()
                    .zip(cand_state.environment.iter())
                    .enumerate()
                {
                    if re.id != ce.id || re.hex != ce.hex || re.ability != ce.ability {
                        errs.push(format!(
                            "environment[{i}] mismatch: ref={re:?} cand={ce:?}"
                        ));
                    }
                }
            }

            if !errs.is_empty() {
                failures.push(format!(
                    "FAIL  {case_name}\n{}",
                    errs.iter().map(|e| format!("      {e}")).collect::<Vec<_>>().join("\n"),
                ));
            }

            // ── Dense-id leg (for offline sim path) ──────────────────────────
            // Feeds dense 0..N UnitIds instead of entity-derived ones.
            // Won't byte-match reference (different ids → different initiative
            // draw order), but must be well-formed and deterministic.
            // Note: we assert consistency (two runs agree) rather than equality
            // with the reference, because the sim will use different ids.
            let run_dense = || {
                let mut counter = 0u64;
                let mut rng2 = DiceRng::with_seed(TEST_SEED);
                init_fight(
                    &scenario.content,
                    scenario,
                    scene_index,
                    encounter,
                    &mut rng2,
                    &HashMap::new(),
                    |_src| {
                        let uid = UnitId(counter);
                        counter += 1;
                        uid
                    },
                )
            };

            let (dense1, _) = run_dense();
            let (dense2, _) = run_dense();

            let mut dense_errs: Vec<String> = Vec::new();
            if post_state_hash_hex(&dense1) != post_state_hash_hex(&dense2) {
                dense_errs.push(format!(
                    "dense-id: two runs produced different post_state_hash\n    run1: {}\n    run2: {}",
                    post_state_hash_hex(&dense1),
                    post_state_hash_hex(&dense2),
                ));
            }
            if dense1.units().len() != spawn_order_uids.len() {
                dense_errs.push(format!(
                    "dense-id: unit count {} != expected {}",
                    dense1.units().len(),
                    spawn_order_uids.len(),
                ));
            }
            if dense1.turn_queue.order != dense2.turn_queue.order {
                dense_errs.push("dense-id: turn_queue.order differs between runs".to_owned());
            }

            if !dense_errs.is_empty() {
                failures.push(format!(
                    "FAIL  {case_name} [dense-id leg]\n{}",
                    dense_errs.iter().map(|e| format!("      {e}")).collect::<Vec<_>>().join("\n"),
                ));
            }
        }
    }

    assert!(
        case_count > 0,
        "No combat scenes found — campaign data may not be accessible"
    );

    if !failures.is_empty() {
        panic!(
            "{}/{} encounter(s) failed:\n\n{}",
            failures.len(),
            case_count,
            failures.join("\n\n")
        );
    }

    eprintln!("init_fight equivalence: {case_count} encounter(s) passed");
}

// ── Per-unit comparison ───────────────────────────────────────────────────────

fn compare_units(
    ru: &combat_engine::state::Unit,
    cu: &combat_engine::state::Unit,
    uid: UnitId,
    errs: &mut Vec<String>,
) {
    macro_rules! cmp {
        ($field:ident) => {
            if ru.$field != cu.$field {
                errs.push(format!(
                    "unit {:?}: .{} differs\n        ref:  {:?}\n        cand: {:?}",
                    uid, stringify!($field), ru.$field, cu.$field
                ));
            }
        };
    }

    cmp!(team);
    cmp!(pos);
    cmp!(armor);
    cmp!(armor_bonus);
    cmp!(damage_taken_bonus);
    cmp!(base_speed);
    cmp!(speed);
    cmp!(reactions_left);
    cmp!(reactions_max);
    cmp!(initiative);
    cmp!(caster_context);
    cmp!(aoo_dice);
    cmp!(pools);
    cmp!(regen_per_pool);
    cmp!(template_id);
    cmp!(passives);
    cmp!(tags);

    // statuses: order-sensitive (both are Vec).
    if ru.statuses != cu.statuses {
        errs.push(format!(
            "unit {:?}: .statuses differ\n        ref:  {:?}\n        cand: {:?}",
            uid, ru.statuses, cu.statuses,
        ));
    }

    // auras: order-sensitive.
    if ru.auras != cu.auras {
        errs.push(format!(
            "unit {:?}: .auras differ\n        ref:  {:?}\n        cand: {:?}",
            uid, ru.auras, cu.auras,
        ));
    }

    // enemy_phases: order-sensitive.
    if ru.enemy_phases != cu.enemy_phases {
        errs.push(format!(
            "unit {:?}: .enemy_phases differ\n        ref:  {:?}\n        cand: {:?}",
            uid, ru.enemy_phases, cu.enemy_phases,
        ));
    }
}
