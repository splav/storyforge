//! Equivalence test: `init_fight` must produce the same `CombatState` as the
//! ECS bootstrap (`spawn_combatants` + `bootstrap_combat_state`) for every
//! real fight in the campaign.
//!
//! # Identity-keyed comparison (not UnitId-keyed)
//!
//! The two build paths assign UnitIds by different schemes:
//! - **Reference (ECS)** derives ids from Bevy entity allocation order.
//! - **Candidate (`init_fight`)** uses dense `0..N` ids in spawn order.
//!
//! The UnitId *scheme* (C vs B) is a separate, later decision; this test
//! validates **field sourcing**, which must be id-scheme-agnostic. So we match
//! units across the two states by a **stable identity key** — `(team, pos)` —
//! and assert every engine `Unit` field is equal EXCEPT the id itself (and any
//! field whose value is literally an id, e.g. a status applier — those are
//! compared modulo the id remap induced by matching).
//!
//! ## Identity key: `(team, pos)`
//! Each combatant spawns on a distinct hex (`member.hex_pos` / `def.hex_pos`),
//! so `pos` alone is unique within an encounter; `team` is included for
//! robustness. The test panics if the key collides, so the matching is sound.
//!
//! ## Initiative tie-break caveat
//! `roll_initiative_for_all` consumes RNG in ascending-UnitId order, and both
//! schemes assign ids in the same spawn order, so the Nth-spawned unit draws
//! the Nth roll in both paths → per-identity initiative TOTALS are identical.
//! `reconcile_turn_order` breaks initiative ties by ascending UnitId; for units
//! with EQUAL totals a different id scheme MAY legitimately reorder the tied
//! run. We therefore assert turn order matches by identity for all positions
//! whose initiative total is unique, and (for tied groups) only that the SET of
//! identities in each equal-initiative run matches.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use std::collections::HashMap;

use combat_engine::{
    state::{Team, UnitId},
    DiceRng,
};

use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::engine_bridge::{
    apply_bridge_queues_pre_projection, bootstrap_combat_state, BridgeQueues, CombatStateRes,
    UnitIdMap,
};
use storyforge::combat::DiceRngRes;
use storyforge::content::campaigns::load_campaigns;
use storyforge::content::content_view::ActiveContent;
use storyforge::content::scenarios::SceneDef;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::resources::{
    CombatBlockedHexes, CombatContext, CombatEnvironment, CombatObjective, GameDb, HexCorpses,
    HexPositions, PresetInitiative, ScenarioState, TurnQueue, UiDirty,
};
use storyforge::scenario::combat_scene::spawn_combatants;
use storyforge::scenario::init_fight::{init_fight, CombatantSource};

#[path = "common/mod.rs"]
mod common;

// ── Fixed RNG seed for deterministic runs ─────────────────────────────────────
const TEST_SEED: u64 = 0xDEAD_C0DE_1234_5678;

// ── App builder ───────────────────────────────────────────────────────────────

/// Build a headless app that can run `spawn_combatants` + `bootstrap_combat_state`.
fn scenario_app(content: storyforge::content::content_view::ContentView) -> App {
    use bevy::math::Vec2;
    use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
    use storyforge::combat::ai::log::{AiLogger, PendingAiLogEntries};
    use storyforge::game::messages::ActionInput;
    use storyforge::ui::animation::AnimationQueue;
    use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

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
    db.scenarios
        .insert(scenario_id.to_owned(), scenario.clone());

    // Insert scenario routing resources.
    app.world_mut().insert_resource(ScenarioState {
        scenario_id: scenario_id.to_owned(),
        scene_index,
    });
    app.world_mut().insert_resource(db);

    // Seed the RNG.
    app.world_mut().resource_mut::<DiceRngRes>().0 = DiceRng::with_seed(seed);

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
/// Hero loadouts are intentionally not applied on the offline equivalence-test path:
/// `init_fight` always uses class defaults, so both paths must match without overrides.
fn spawn_combatants_system(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    mut objective: ResMut<CombatObjective>,
    mut blocked: ResMut<CombatBlockedHexes>,
    mut environment: ResMut<CombatEnvironment>,
    tag_cache: Res<AbilityTagCache>,
) {
    let empty_loadouts = std::collections::HashMap::new();
    spawn_combatants(
        &mut commands,
        &db,
        &scenario,
        &mut objective,
        &mut blocked,
        &mut environment,
        &tag_cache,
        &empty_loadouts,
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
    combatants: Query<
        (Entity, &storyforge::game::components::StartingHexPos),
        With<storyforge::game::components::Combatant>,
    >,
) {
    for (entity, starting_pos) in combatants.iter() {
        positions.insert(entity, starting_pos.0);
        commands
            .entity(entity)
            .remove::<storyforge::game::components::StartingHexPos>();
    }
}

// ── Identity key ────────────────────────────────────────────────────────────

/// Stable, id-scheme-agnostic identity for a unit within one encounter.
/// `pos` is unique per combatant (each spawns on a distinct hex); the team tag
/// is folded in for robustness. `Team` is neither `Hash` nor `Ord`, so we
/// encode it as a `u8` tag to make the key usable in hash maps/sets.
/// See module docs for the uniqueness argument.
type Identity = (u8, hexx::Hex);

fn team_tag(team: Team) -> u8 {
    match team {
        Team::Player => 0,
        Team::Enemy => 1,
    }
}

fn identity_of(u: &combat_engine::state::Unit) -> Identity {
    (team_tag(u.team), u.pos)
}

/// Identity of a `CombatantSource` — same `(team_tag, hex_pos)` key as
/// `identity_of`, derived from content before the engine `Unit` exists.
fn source_identity(src: &CombatantSource<'_>) -> Identity {
    match src {
        CombatantSource::ClassHero { member, .. }
        | CombatantSource::TemplateMember { member, .. } => {
            (team_tag(Team::Player), member.hex_pos)
        }
        CombatantSource::Enemy { def, .. } => (team_tag(Team::Enemy), def.hex_pos),
    }
}

/// Build `Identity → UnitId` for a state, panicking if the key collides
/// (which would make the matching unsound).
fn identity_index(
    state: &combat_engine::state::CombatState,
    label: &str,
) -> HashMap<Identity, UnitId> {
    let mut map = HashMap::new();
    for u in state.units() {
        if map.insert(identity_of(u), u.id).is_some() {
            panic!(
                "{label}: identity key {:?} collides — two units share (team, pos); \
                 the identity-matching assumption is violated",
                identity_of(u)
            );
        }
    }
    map
}

// ── Main test ─────────────────────────────────────────────────────────────────

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
            let (ref_state, _spawn_order_uids) =
                run_ecs_bootstrap(&mut app, scenario_id, scenario, scene_index, TEST_SEED);

            // ── Candidate leg: init_fight with dense ids ──────────────────────
            //
            // Why not naive 0..N in spawn order?  `roll_initiative_for_all`
            // consumes RNG in ascending-UnitId order, so per-identity initiative
            // totals only match the reference if the candidate's ascending-uid
            // order maps to the SAME identities as the reference's.  The ECS
            // path's entity-derived ids do NOT follow party-then-enemy spawn
            // order (Bevy allocates enemies' entity bits below the party's), so
            // naive party-first dense ids would roll for a different identity at
            // each RNG draw and permute the totals.
            //
            // The id SCHEME is a separate later decision; this test only
            // validates field sourcing, which must be id-scheme-agnostic.  To
            // isolate field sourcing from the roll-order coupling we still feed
            // dense `0..N` ids, but assign their VALUES by the reference's
            // ascending-uid rank of each unit's identity.  This makes the RNG
            // consumption order identical without making init_fight reproduce
            // Bevy's actual UnitIds.
            let ident_to_dense: HashMap<Identity, u64> = {
                let mut by_uid: Vec<_> = ref_state
                    .units()
                    .iter()
                    .map(|u| (u.id, identity_of(u)))
                    .collect();
                by_uid.sort_by_key(|(id, _)| id.0);
                by_uid
                    .into_iter()
                    .enumerate()
                    .map(|(rank, (_, ident))| (ident, rank as u64))
                    .collect()
            };

            let mut rng = DiceRng::with_seed(TEST_SEED);
            let (cand_state, _metas) = init_fight(
                &scenario.content,
                scenario,
                scene_index,
                encounter,
                &mut rng,
                &HashMap::new(), // no preset
                |src| {
                    let ident = source_identity(src);
                    UnitId(ident_to_dense[&ident])
                },
            );

            let errs = compare_states(&ref_state, &cand_state);
            if !errs.is_empty() {
                failures.push(format!(
                    "FAIL  {case_name}\n{}",
                    errs.iter()
                        .map(|e| format!("      {e}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
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

// ── State-level identity-keyed comparison ──────────────────────────────────────

fn compare_states(
    ref_state: &combat_engine::state::CombatState,
    cand_state: &combat_engine::state::CombatState,
) -> Vec<String> {
    let mut errs: Vec<String> = Vec::new();

    let ref_idx = identity_index(ref_state, "ref");
    let cand_idx = identity_index(cand_state, "cand");

    // ── Unit count + identity set ─────────────────────────────────────────────
    if ref_idx.len() != cand_idx.len() {
        errs.push(format!(
            "unit count: ref={} cand={}",
            ref_idx.len(),
            cand_idx.len()
        ));
    }
    let ref_ids: std::collections::HashSet<&Identity> = ref_idx.keys().collect();
    let cand_ids: std::collections::HashSet<&Identity> = cand_idx.keys().collect();
    if ref_ids != cand_ids {
        errs.push(format!(
            "identity sets differ:\n    ref-only:  {:?}\n    cand-only: {:?}",
            ref_ids.difference(&cand_ids).collect::<Vec<_>>(),
            cand_ids.difference(&ref_ids).collect::<Vec<_>>(),
        ));
        // Can't field-compare if identity sets differ.
        return errs;
    }

    // Reference UnitId → candidate UnitId, induced by the identity match.
    // Used to remap id-valued fields (status appliers) before comparison.
    let ref_to_cand: HashMap<UnitId, UnitId> = ref_idx
        .iter()
        .map(|(ident, &ru)| (ru, *cand_idx.get(ident).unwrap()))
        .collect();

    // ── Per-unit field comparison ─────────────────────────────────────────────
    for (ident, &ruid) in &ref_idx {
        let cuid = *cand_idx.get(ident).unwrap();
        let ru = ref_state.unit(ruid).unwrap();
        let cu = cand_state.unit(cuid).unwrap();
        compare_units(ru, cu, *ident, &ref_to_cand, &mut errs);
    }

    // ── next_synthetic_uid ────────────────────────────────────────────────────
    if ref_state.next_synthetic_uid() != cand_state.next_synthetic_uid() {
        errs.push(format!(
            "next_synthetic_uid: ref={} cand={}",
            ref_state.next_synthetic_uid(),
            cand_state.next_synthetic_uid(),
        ));
    }

    // ── blocked_hexes ─────────────────────────────────────────────────────────
    if ref_state.blocked_hexes != cand_state.blocked_hexes {
        errs.push(format!(
            "blocked_hexes mismatch:\n    ref:  {:?}\n    cand: {:?}",
            ref_state.blocked_hexes, cand_state.blocked_hexes,
        ));
    }

    // ── environment (id-keyed by index; ids are positional, not unit ids) ─────
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
                errs.push(format!("environment[{i}] mismatch: ref={re:?} cand={ce:?}"));
            }
        }
    }

    // ── Turn order (compared as identities, with tie-break caveat) ────────────
    compare_turn_order(ref_state, cand_state, &mut errs);

    errs
}

/// Compare `turn_queue.order` by identity. For runs of EQUAL initiative the
/// tie-break is by UnitId, so a different id scheme may reorder *tied* units;
/// we only require that each equal-initiative run contains the same identity
/// SET. Positions with a unique total must match exactly (by identity).
fn compare_turn_order(
    ref_state: &combat_engine::state::CombatState,
    cand_state: &combat_engine::state::CombatState,
    errs: &mut Vec<String>,
) {
    let ref_seq = order_as_identities(ref_state);
    let cand_seq = order_as_identities(cand_state);

    if ref_seq.len() != cand_seq.len() {
        errs.push(format!(
            "turn_queue length: ref={} cand={}",
            ref_seq.len(),
            cand_seq.len()
        ));
        return;
    }

    // Group consecutive entries by initiative total and compare each run as a set.
    let mut i = 0;
    while i < ref_seq.len() {
        let (_, init) = ref_seq[i];
        let mut j = i;
        while j < ref_seq.len() && ref_seq[j].1 == init {
            j += 1;
        }
        let ref_run: std::collections::HashSet<Identity> =
            ref_seq[i..j].iter().map(|(id, _)| *id).collect();
        let cand_run: std::collections::HashSet<Identity> =
            cand_seq[i..j].iter().map(|(id, _)| *id).collect();
        // Both initiative totals AND the identity set within the run must match.
        let cand_inits: std::collections::BTreeSet<i32> =
            cand_seq[i..j].iter().map(|(_, v)| *v).collect();
        if cand_inits.len() != 1 || !cand_inits.contains(&init) {
            errs.push(format!(
                "turn_queue[{i}..{j}]: initiative totals differ\n    ref:  {:?}\n    cand: {:?}",
                ref_seq[i..j].iter().map(|(_, v)| *v).collect::<Vec<_>>(),
                cand_seq[i..j].iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            ));
        }
        if ref_run != cand_run {
            errs.push(format!(
                "turn_queue[{i}..{j}] (init={init}): identity set differs\n    ref:  {ref_run:?}\n    cand: {cand_run:?}",
            ));
        }
        i = j;
    }
}

/// Map each queued UnitId → (identity, initiative total) in `state`.
fn order_as_identities(state: &combat_engine::state::CombatState) -> Vec<(Identity, i32)> {
    state
        .turn_queue
        .order
        .iter()
        .map(|uid| {
            let u = state.unit(*uid).unwrap();
            (identity_of(u), u.initiative.unwrap_or(i32::MIN))
        })
        .collect()
}

// ── Per-unit comparison ───────────────────────────────────────────────────────

fn compare_units(
    ru: &combat_engine::state::Unit,
    cu: &combat_engine::state::Unit,
    ident: Identity,
    ref_to_cand: &HashMap<UnitId, UnitId>,
    errs: &mut Vec<String>,
) {
    macro_rules! cmp {
        ($field:ident) => {
            if ru.$field != cu.$field {
                errs.push(format!(
                    "unit {:?}: .{} differs\n        ref:  {:?}\n        cand: {:?}",
                    ident,
                    stringify!($field),
                    ru.$field,
                    cu.$field
                ));
            }
        };
    }

    // NB: `id` and `summoner` are intentionally NOT compared (purely id-valued).
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

    // statuses: order-sensitive (both are Vec), but the `applier` field is a
    // UnitId — remap the reference applier through the identity match before
    // comparing, so different id schemes don't spuriously diverge.
    let ref_statuses: Vec<combat_engine::state::ActiveStatus> = ru
        .statuses
        .iter()
        .cloned()
        .map(|mut s| {
            if let combat_engine::state::EffectSource::Unit(uid) = s.applier {
                if let Some(&mapped) = ref_to_cand.get(&uid) {
                    s.applier = combat_engine::state::EffectSource::Unit(mapped);
                }
            }
            s
        })
        .collect();
    if ref_statuses != cu.statuses {
        errs.push(format!(
            "unit {:?}: .statuses differ (applier id-remapped)\n        ref:  {:?}\n        cand: {:?}",
            ident, ref_statuses, cu.statuses,
        ));
    }

    // auras: order-sensitive.
    if ru.auras != cu.auras {
        errs.push(format!(
            "unit {:?}: .auras differ\n        ref:  {:?}\n        cand: {:?}",
            ident, ru.auras, cu.auras,
        ));
    }

    // enemy_phases: order-sensitive.
    if ru.enemy_phases != cu.enemy_phases {
        errs.push(format!(
            "unit {:?}: .enemy_phases differ\n        ref:  {:?}\n        cand: {:?}",
            ident, ru.enemy_phases, cu.enemy_phases,
        ));
    }
}
