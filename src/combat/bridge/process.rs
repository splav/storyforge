//! `process_action_system` — ActionInput → engine `step()` → translated side effects; dynamic summon spawn.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::combat::ai::config::role::infer_profile;
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::content::content_view::ActiveContent;
use crate::game::bundles::enemy_bundle;
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::components::{
    facing_toward, CombatPath, Energy, Equipment, Faction, Mana, Rage, SummonedBy, UnitToken,
};
use crate::game::hex::LAYOUT;
use crate::game::messages::ActionInput;
use crate::game::resources::HexPositions;
use crate::ui::animation::PendingAnim;
use crate::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

use super::*;
use combat_engine::{action::Action, event::Event, step::step};

// ── VisualAssets SystemParam newtype ──────────────────────────────────────────

/// Bundles rendering-only Bevy resources used by `process_action_system`
/// and `spawn_ecs_entity_from_engine_unit`, to stay within Bevy's 16-param
/// limit on those systems.
#[derive(SystemParam)]
pub struct VisualAssets<'w, 's> {
    pub grid_offset: Res<'w, HexGridOffset>,
    pub tokens: Query<'w, 's, (Entity, &'static UnitToken)>,
    pub mats: Res<'w, HexMaterials>,
    pub token_mesh: Res<'w, TokenMesh>,
    pub tag_cache: Res<'w, AbilityTagCache>,
    // `Option` so headless test harnesses without AssetPlugin can run the bridge
    // schedule — summon figures are purely visual and skipped when absent.
    pub asset_server: Option<Res<'w, AssetServer>>,
}

// ── spawn_ecs_entity_from_engine_unit ────────────────────────────────────────

/// Instantiate a new ECS combatant entity from a unit already present in the
/// engine state.  Called from `translate_cast_events` when `Event::UnitSpawned`
/// arrives; replaces the old `apply_spawn_system` + `SpawnUnit` message path.
///
/// Returns the new `Entity`, or `None` if the template is not in content
/// (should not happen — engine already validated the template before emitting
/// the event, but guards are cheap).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_ecs_entity_from_engine_unit(
    uid: combat_engine::state::UnitId,
    summoner_entity: Entity,
    pos: hexx::Hex,
    template_id: &str,
    team: combat_engine::state::Team,
    commands: &mut Commands,
    id_map: &mut UnitIdMap,
    positions: &mut HexPositions,
    active_content: &crate::content::content_view::ActiveContent,
    tag_cache: &AbilityTagCache,
    mats: &HexMaterials,
    token_mesh: &TokenMesh,
    grid_offset: &HexGridOffset,
    asset_server: Option<&AssetServer>,
    log: &mut CombatLog,
) -> Option<Entity> {
    use crate::game::components::Team as EcsTeam;

    let template = active_content.unit_templates.get(template_id)?;
    let equipment = Equipment {
        main_hand: Some(template.equipment.main_hand.clone()),
        off_hand: template.equipment.off_hand.clone(),
        chest: template.equipment.chest.clone(),
        legs: template.equipment.legs.clone(),
        feet: template.equipment.feet.clone(),
    };
    let effective = active_content.effective_stats(&template.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);
    let race_name = active_content
        .races
        .get(&template.race)
        .map_or("", |r| r.name.as_str());
    let display_name = if race_name.is_empty() {
        template.name.clone()
    } else {
        format!("{} {}", race_name, template.name)
    };
    let ecs_team = match team {
        combat_engine::state::Team::Player => EcsTeam::Player,
        combat_engine::state::Team::Enemy => EcsTeam::Enemy,
    };
    let role = infer_profile(
        &template.ability_ids,
        effective.max_hp,
        armor,
        active_content,
        tag_cache,
    );

    let mut ec = commands.spawn((
        Name::new(display_name.clone()),
        enemy_bundle(
            effective,
            armor,
            0, // magic_resist: spawned units have no magic_resist (template carries none)
            template.speed,
            template.ability_ids.clone(),
            equipment,
        ),
        role,
        AiMemory::default(),
        SummonedBy(summoner_entity),
    ));
    // enemy_bundle forces Team::Enemy — overwrite with actual team.
    ec.insert(Faction(ecs_team));
    if template.resources.rage_max > 0 {
        ec.insert(Rage::new(template.resources.rage_max));
    }
    if template.resources.mana_max > 0 {
        ec.insert(Mana::new(template.resources.mana_max));
    }
    if template.resources.energy_max > 0 {
        ec.insert(Energy::new(template.resources.energy_max));
    }
    if let Some(ref p) = template.path {
        ec.insert(CombatPath(p.clone()));
    }
    // Summons spawn mid-combat: face the enemy side by team; the event-driven
    // turn system corrects this on the unit's first interaction. `Facing` is a
    // property of every combatant (not just sprited ones), so the turn system can
    // mutate it uniformly — keep it out of the sprite-gated block.
    let facing = crate::game::components::Facing::for_team(ecs_team);
    ec.insert(facing);
    let sprite_key = template
        .sprite
        .as_deref()
        .map(|p| crate::game::components::resolve_race(p, &template.race));
    if let Some(ref s) = sprite_key {
        ec.insert(crate::game::components::UnitSprite(s.clone()));
    }
    let new_entity = ec.id();

    positions.insert(new_entity, pos);
    id_map.insert(new_entity, uid);

    let pixel = LAYOUT.hex_to_world_pos(pos) + grid_offset.0;
    let token_material = match ecs_team {
        EcsTeam::Player => mats.token_player.clone(),
        EcsTeam::Enemy => mats.token_enemy.clone(),
    };
    commands
        .spawn((
            UnitToken(new_entity),
            Mesh2d(token_mesh.token.clone()),
            MeshMaterial2d(token_material),
            Transform::from_xyz(pixel.x, pixel.y, 0.15),
        ))
        .with_children(|parent| {
            if let (Some(pattern), Some(srv)) = (sprite_key.as_ref(), asset_server) {
                crate::ui::hex_grid::spawn_figure_child(parent, srv, new_entity, pattern, facing);
            }
        });

    log.push(CombatEvent::Summoned {
        summoner: summoner_entity,
        summon_name: display_name,
    });

    Some(new_entity)
}

// ── process_action_system ──────────────────────────────────────────────────────

/// `Update` system — authoritative action handler via `combat_engine::step()`.
///
/// Reads `ActionInput` messages, calls `step()` against the mirrored
/// `CombatStateRes`, and translates the resulting `Event` stream into Bevy-land
/// side effects (CombatLog entries, Dead markers, movement animations). The
/// engine is the sole owner of `Action::Move` and `Action::Cast`, and the sole
/// writer for hp / rage / mana / statuses; `project_state_to_ecs` (chained
/// immediately after) writes the engine mutations back to ECS components.
///
/// Runs in `CombatStep::Execute`, gated by `CombatPhase::AwaitCommand`.
#[allow(clippy::too_many_arguments)]
pub fn process_action_system(
    mut commands: Commands,
    mut reader: MessageReader<ActionInput>,
    mut id_map: ResMut<UnitIdMap>,
    mut combat_state: ResMut<CombatStateRes>,
    active_content: Res<ActiveContent>,
    mut rng: ResMut<crate::combat::DiceRngRes>,
    mut log: ResMut<CombatLog>,
    mut positions: ResMut<HexPositions>,
    visuals: VisualAssets,
    mut queues: ResMut<BridgeQueues>,
    mut trace_writer: ResMut<crate::combat::ai::log::engine_trace::EngineTraceWriter>,
) {
    for msg in reader.read() {
        match msg {
            ActionInput::Move { actor, path } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for entity {:?} — skipping",
                        actor
                    );
                    continue;
                };

                let action = Action::Move {
                    actor: actor_uid,
                    path: path.clone(),
                };

                let content = build_ecs_content_view(&active_content);

                let action_for_trace = action.clone();
                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection so a crash mid-projection
                        // doesn't corrupt the trace (plan spec §4 wiring note).
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) =
                            trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        // Save interrupted flag before `ctx` is shadowed by TranslateCtx below.
                        let move_was_interrupted = ctx.interrupted;
                        let move_ctx = MoveCtx {
                            actor: *actor,
                            combat_state: &combat_state,
                            grid_offset: &visuals.grid_offset,
                            first_from: None,
                            last_to: None,
                            waypoints: Vec::new(),
                            pending_aoo_target: None,
                        };
                        // Scoped block so ctx's borrow of `log` ends before finalize_move.
                        let (final_from, final_to, final_waypoints, final_actor) = {
                            let mut ctx = TranslateCtx {
                                log: &mut log,
                                id_map: &mut id_map,
                                queues: &mut queues,
                                cast: None,
                                move_: Some(move_ctx),
                            };
                            translate_events(&events, &mut ctx);
                            let mv = ctx.move_.take().unwrap();
                            (mv.first_from, mv.last_to, mv.waypoints, mv.actor)
                        };
                        // Emit aggregated UnitMoved and enqueue animation (ctx dropped above).
                        if let (Some(from), Some(to)) = (final_from, final_to) {
                            log.push(CombatEvent::UnitMoved {
                                actor: final_actor,
                                from,
                                to,
                            });
                        }
                        if !final_waypoints.is_empty() {
                            if let Some((token_entity, _)) =
                                visuals.tokens.iter().find(|(_, t)| t.0 == final_actor)
                            {
                                // Face travel direction before the slide animation.
                                // Skip on pure-vertical moves (equal world-X → tie → Left,
                                // which is meaningless and would flip the sprite oddly).
                                if let (Some(from), Some(to)) = (final_from, final_to) {
                                    use crate::game::hex::LAYOUT;
                                    if LAYOUT.hex_to_world_pos(from).x
                                        != LAYOUT.hex_to_world_pos(to).x
                                    {
                                        queues.animations.push(PendingAnim::Face {
                                            unit: final_actor,
                                            facing: facing_toward(from, to),
                                        });
                                    }
                                }
                                queues.animations.push(PendingAnim::Movement {
                                    token: token_entity,
                                    waypoints: final_waypoints,
                                });
                            }
                        }
                        // AoO on a move can cross a phase threshold; queue for apply system.
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
                            }
                        }
                        // EnvRevealed post-pass: push CombatLog entry with the trap's hex.
                        // Done here (not in translate_one) because resolving the hex requires
                        // reading combat_state, which is not available inside TranslateCtx.
                        for ev in &events {
                            if let Event::EnvRevealed { env_id } = ev {
                                let hex = combat_state
                                    .0
                                    .environment
                                    .iter()
                                    .find(|e| e.id == *env_id)
                                    .map(|e| e.hex)
                                    .unwrap_or(hexx::Hex::ZERO);
                                log.push(CombatEvent::EnvRevealed { hex });
                            }
                        }
                        // If interrupted (AoO / hazard reveal / trap fire), drop the
                        // turn's remaining queued ActionInputs: a bundled Cast planned
                        // from the pre-move position must not fire from the truncated
                        // landing hex. The AI re-plans next frame.
                        if move_was_interrupted {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                    }
                }
            }
            ActionInput::Cast {
                actor,
                ability,
                target,
                target_pos,
            } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for cast actor {:?} — skipping",
                        actor
                    );
                    continue;
                };
                let Some(target_uid) = id_map.get_id(*target) else {
                    warn!(
                        "process_action_system: no UnitId for cast target {:?} — skipping",
                        target
                    );
                    continue;
                };

                let action = Action::Cast {
                    actor: actor_uid,
                    ability: ability.clone(),
                    target: target_uid,
                    target_pos: *target_pos,
                };

                let content = build_ecs_content_view(&active_content);

                let action_for_trace = action.clone();
                match step(&mut combat_state.0, action, &mut rng.0, &content) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) =
                            trace_writer.write_step(&action_for_trace, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        // Face the target before cast visuals (actor_uid != target_uid
                        // guards self-targeted abilities).
                        if actor_uid != target_uid {
                            if let Some(actor_hex) = combat_state.0.unit(actor_uid).map(|u| u.pos) {
                                queues.animations.push(PendingAnim::Face {
                                    unit: *actor,
                                    facing: facing_toward(actor_hex, *target_pos),
                                });
                            }
                        }
                        emit_ability_used(
                            *actor,
                            ability,
                            *target,
                            *target_pos,
                            &active_content,
                            &mut log,
                        );
                        // PRE-pass: handle UnitSpawned BEFORE translate_events so the
                        // summoned entity is registered in `id_map` first.  A summon cast
                        // by the round's last actor wraps the round inside this same
                        // `step(Cast)` and emits `TurnStarted { summon }`; translate_events
                        // resolves that actor via `id_map` to assign `ActiveCombatant`.
                        // If the entity were created afterwards (old post-pass), the
                        // lookup returned `None`, the new actor got no `ActiveCombatant`,
                        // and the turn loop stalled (no driver → game hang).
                        // Needs `&mut Commands` (un-storable in TranslateCtx — same reason
                        // PhaseEntered is handled at the callsite). `InitiativeRolled` for
                        // the summon is logged by translate_events below, which now finds
                        // the entity in `id_map`.
                        for ev in &events {
                            if let Event::UnitSpawned {
                                uid,
                                summoner: summoner_uid,
                                pos,
                                template_id,
                                team,
                            } = ev
                            {
                                let Some(summoner_entity) = id_map.get_entity(*summoner_uid) else {
                                    continue;
                                };
                                spawn_ecs_entity_from_engine_unit(
                                    *uid,
                                    summoner_entity,
                                    *pos,
                                    template_id,
                                    *team,
                                    &mut commands,
                                    &mut id_map,
                                    &mut positions,
                                    &active_content,
                                    &visuals.tag_cache,
                                    &visuals.mats,
                                    &visuals.token_mesh,
                                    &visuals.grid_offset,
                                    visuals.asset_server.as_deref(),
                                    &mut log,
                                );
                            }
                        }
                        {
                            let cast_ctx = CastCtx { _phantom: () };
                            let mut ctx = TranslateCtx {
                                log: &mut log,
                                id_map: &mut id_map,
                                queues: &mut queues,
                                cast: Some(cast_ctx),
                                move_: None,
                            };
                            translate_events(&events, &mut ctx);
                        }
                        // Queue phase transitions from cast events (most common case:
                        // boss crosses HP threshold from a direct damage spell).
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: Cast step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                        // Cast failed validation — engine state is rolled back, so
                        // don't end the turn; let the user retry or end manually.
                    }
                }
            }
            ActionInput::EndTurn { actor } => {
                let Some(actor_uid) = id_map.get_id(*actor) else {
                    warn!(
                        "process_action_system: no UnitId for EndTurn actor {:?} — skipping",
                        actor
                    );
                    continue;
                };

                let content = build_ecs_content_view(&active_content);

                let end_action = Action::EndTurn { actor: actor_uid };
                match step(
                    &mut combat_state.0,
                    end_action.clone(),
                    &mut rng.0,
                    &content,
                ) {
                    Ok((events, ctx)) => {
                        // Write trace BEFORE ECS projection.
                        let hash = combat_engine::trace::post_state_hash_hex(&combat_state.0);
                        if let Err(e) =
                            trace_writer.write_step(&end_action, &events, ctx.rng_calls, hash)
                        {
                            warn!("Engine trace step write failed: {e}");
                        }
                        let mut ctx = TranslateCtx {
                            log: &mut log,
                            id_map: &mut id_map,
                            queues: &mut queues,
                            cast: None,
                            move_: None,
                        };
                        translate_events(&events, &mut ctx);
                        // DoT ticks at end of turn can cross a phase threshold.
                        for ev in &events {
                            if let Event::PhaseEntered {
                                unit, phase_idx, ..
                            } = ev
                            {
                                queues.phases.push((*unit, *phase_idx));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "process_action_system: EndTurn step() error for actor {:?} (uid {:?}): {:?}",
                            actor, actor_uid, e
                        );
                    }
                }
            }
        }
    }
}
