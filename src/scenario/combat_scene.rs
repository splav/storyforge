#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use crate::app_state::CombatPhase;
use crate::content::encounters::VictoryCondition;
use crate::content::scenarios::{active_party, SceneDef};
use crate::combat::ai::intent::AiMemory;
use crate::combat::ai::config::role::infer_profile;
use crate::combat::ai::world::tags::AbilityTagCache;
use crate::game::bundles::{enemy_bundle, hero_bundle};
use crate::game::components::{AuraSource, CombatPath, Combatant, Energy, EnemyPhases, Equipment, Initiative, Mana, Rage, StartingHexPos, TemplateRef, UnitToken, VictoryTarget, KeepAliveTarget, Vital};
use crate::game::combat_log::{CombatEvent, CombatLog};
use crate::game::messages::RestartCombat;
use crate::game::resources::{
    CombatBlockedHexes, CombatContext, CombatObjective, GameDb, HexCorpses, HexPositions,
    PhaseDeadline, PresetInitiative, ScenarioState, SelectionState, TurnQueue,
};
use crate::combat::enemy_popup::PopupCursor;
use crate::ui::animation::AnimationQueue;
use crate::ui::console_log::ConsoleCursor;
use bevy::prelude::*;

#[derive(Component)]
pub struct BattleBackground;

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Спаунит героев и врагов по текущему сценарию/энкаунтеру. Только Commands.
fn spawn_combatants(
    commands: &mut Commands,
    db: &GameDb,
    scenario: &ScenarioState,
    objective: &mut CombatObjective,
    blocked_hexes: &mut CombatBlockedHexes,
    tag_cache: &AbilityTagCache,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let encounter_id = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { encounter_id, .. } => encounter_id,
        _ => return,
    };
    let enc = scen.encounters.get(encounter_id.as_str()).unwrap_or_else(|| {
        panic!(
            "Encounter '{encounter_id}' not found in scenario '{}'",
            scen.id
        )
    });

    objective.0 = enc.victory.clone();
    blocked_hexes.0 = enc.obstacles.clone();
    let content = &scen.content;

    // Pre-compute the set of names that have a KeepAlive condition at any depth
    // inside the victory tree. Used during spawning to insert KeepAliveTarget
    // without a deferred second pass.
    let keep_alive_names = collect_keep_alive_names(&enc.victory);

    let party = active_party(scen, scenario.scene_index);
    for member in &party {
        // Template-based member (e.g. non-acting NPC added via party_add with template field).
        if let Some(template_id) = &member.template {
            let tpl = content.unit_templates.get(template_id).unwrap_or_else(|| {
                panic!("Template '{}' not found for party member '{}'", template_id, member.name)
            });
            let equipment = Equipment {
                main_hand: Some(tpl.equipment.main_hand.clone()),
                off_hand: tpl.equipment.off_hand.clone(),
                chest: tpl.equipment.chest.clone(),
                legs: tpl.equipment.legs.clone(),
                feet: tpl.equipment.feet.clone(),
            };
            let effective = content.effective_stats(&tpl.stats, &equipment);
            let armor = content.equipment_armor(&equipment);
            let role = infer_profile(&tpl.ability_ids, effective.max_hp, armor, content, tag_cache);
            let initial_hp = tpl.initial_pools.get("hp").copied()
                .unwrap_or(effective.max_hp)
                .clamp(1, effective.max_hp);
            // Template-based party member shares the same bundle as class-based
            // heroes — engine `from_ecs` and AI snapshot cache need Abilities,
            // CombatStats, Equipment to fully see the unit. `from_ecs` now
            // tolerates missing Speed/AP/Reactions with warn-and-default, but
            // ability/stats lookup is engine-private — no defaults fit.
            let mut ec = commands.spawn((
                Name::new(member.name.clone()),
                hero_bundle(effective.clone(), armor, tpl.speed, tpl.ability_ids.clone(), equipment),
                StartingHexPos(member.hex_pos),
                role,
                AiMemory::default(),
                TemplateRef(template_id.clone()),
            ));
            // Override Vital.hp from `initial_pools[hp]` — bundle spawns with hp=max_hp.
            ec.insert(Vital { hp: initial_hp, max_hp: effective.max_hp, armor });
            // Pool components — for templates that declare them.
            if tpl.resources.mana_max > 0 { ec.insert(Mana::new(tpl.resources.mana_max)); }
            if tpl.resources.rage_max > 0 { ec.insert(Rage::new(tpl.resources.rage_max)); }
            if tpl.resources.energy_max > 0 { ec.insert(Energy::new(tpl.resources.energy_max)); }
            if keep_alive_names.contains(member.name.as_str()) {
                ec.insert(KeepAliveTarget { marker_color: keep_alive_marker_color(&enc.victory, &member.name) });
            }
            // initial_statuses are applied engine-side in bootstrap_combat_state
            // via CombatState::apply_initial_statuses (reads UnitTemplate from ContentView).
            continue;
        }

        // Class-based member (regular hero).
        let cls = content.classes.get(&member.class_id).unwrap_or_else(|| {
            panic!("Class '{}' not found in classes.toml", member.class_id)
        });
        let equipment = Equipment {
            main_hand: Some(cls.main_hand.clone()),
            off_hand: cls.off_hand.clone(),
            chest: cls.chest.clone(),
            legs: cls.legs.clone(),
            feet: cls.feet.clone(),
        };
        let effective = content.effective_stats(&cls.stats, &equipment);
        let armor = content.equipment_armor(&equipment);
        let role = infer_profile(&cls.abilities, effective.max_hp, armor, content, tag_cache);
        let mut ec = commands.spawn((
            Name::new(member.name.clone()),
            hero_bundle(effective, armor, cls.speed, cls.abilities.clone(), equipment),
            StartingHexPos(member.hex_pos),
            role,
            AiMemory::default(),
        ));
        if cls.rage_max > 0 { ec.insert(Rage::new(cls.rage_max)); }
        if cls.mana_max > 0 { ec.insert(Mana::new(cls.mana_max)); }
        if cls.energy_max > 0 { ec.insert(Energy::new(cls.energy_max)); }
        if let Some(ref p) = member.path { ec.insert(CombatPath(p.clone())); }
        if keep_alive_names.contains(member.name.as_str()) {
            ec.insert(KeepAliveTarget { marker_color: keep_alive_marker_color(&enc.victory, &member.name) });
        }
    }

    for enemy in &enc.enemies {
        let equipment = Equipment {
            main_hand: Some(enemy.main_hand.clone()),
            off_hand: enemy.off_hand.clone(),
            chest: enemy.chest.clone(),
            legs: enemy.legs.clone(),
            feet: enemy.feet.clone(),
        };
        let effective = content.effective_stats(&enemy.stats, &equipment);
        let armor = content.equipment_armor(&equipment);
        let race_name = content.races.get(&enemy.race).map_or("", |r| r.name.as_str());
        let display_name = format!("{} {}", race_name, &enemy.name);
        let role = infer_profile(&enemy.ability_ids, effective.max_hp, armor, content, tag_cache);
        let mut ec = commands.spawn((
            Name::new(display_name.clone()),
            enemy_bundle(effective, armor, enemy.speed, enemy.ability_ids.clone(), equipment),
            StartingHexPos(enemy.hex_pos),
            role,
            AiMemory::default(),
        ));
        if enemy.rage_max > 0 { ec.insert(Rage::new(enemy.rage_max)); }
        if enemy.mana_max > 0 { ec.insert(Mana::new(enemy.mana_max)); }
        if enemy.energy_max > 0 { ec.insert(Energy::new(enemy.energy_max)); }
        if let Some(ref p) = enemy.path { ec.insert(CombatPath(p.clone())); }
        if let VictoryCondition::KillTarget { enemy_name, marker_color, .. } = &enc.victory {
            if &enemy.name == enemy_name {
                ec.insert(VictoryTarget { marker_color: *marker_color });
            }
        }
        // KeepAlive targets may be enemies too (unusual but valid — e.g. "protect boss NPC").
        // Match against both raw enemy.name and the display_name (race + name).
        if keep_alive_names.contains(enemy.name.as_str())
            || keep_alive_names.contains(display_name.as_str())
        {
            ec.insert(KeepAliveTarget { marker_color: keep_alive_marker_color(&enc.victory, &enemy.name) });
        }
        if !enemy.phases.is_empty() {
            ec.insert(EnemyPhases { pending: enemy.phases.clone() });
        }
        if let Some(ref aura) = enemy.aura {
            ec.insert(AuraSource {
                status: aura.status.clone(),
                radius: aura.radius,
                affects: aura.affects,
            });
        }
    }
}

/// Recursively collect all `target_name` strings from any `KeepAlive` node
/// at any depth inside the victory condition tree.
fn collect_keep_alive_names(cond: &VictoryCondition) -> std::collections::HashSet<&str> {
    let mut names = std::collections::HashSet::new();
    walk_victory_names(cond, &mut names);
    names
}

fn walk_victory_names<'a>(cond: &'a VictoryCondition, names: &mut std::collections::HashSet<&'a str>) {
    match cond {
        VictoryCondition::KeepAlive { target_name, .. } => {
            names.insert(target_name.as_str());
        }
        VictoryCondition::AllOf(children) => {
            for child in children {
                walk_victory_names(child, names);
            }
        }
        VictoryCondition::AllEnemiesDead | VictoryCondition::KillTarget { .. } => {}
    }
}

/// Find the `marker_color` for a given `target_name` anywhere in the victory tree.
/// Returns a neutral amber color `[0.9, 0.7, 0.1]` if the name is not found
/// (should not happen in valid data — `validate_scenario` guards this).
fn keep_alive_marker_color(cond: &VictoryCondition, name: &str) -> [f32; 3] {
    fn search(cond: &VictoryCondition, name: &str) -> Option<[f32; 3]> {
        match cond {
            VictoryCondition::KeepAlive { target_name, marker_color } if target_name == name => {
                Some(*marker_color)
            }
            VictoryCondition::AllOf(children) => {
                children.iter().find_map(|c| search(c, name))
            }
            _ => None,
        }
    }
    search(cond, name).unwrap_or([0.9, 0.7, 0.1])
}

/// Сбрасывает все ресурсы боя в начальное состояние.
fn reset_combat_state(
    ctx: &mut CombatContext,
    log: &mut CombatLog,
    cursor: &mut ConsoleCursor,
    popup_cursor: &mut PopupCursor,
    anim_queue: &mut AnimationQueue,
    deadline: &mut PhaseDeadline,
) {
    ctx.round = 0;
    ctx.encounter = None;
    log.0.clear();
    log.push(CombatEvent::CombatStarted);
    cursor.0 = 0;
    popup_cursor.0 = 0;
    anim_queue.0.clear();
    deadline.0 = None;
}

// ── Systems ─────────────────────────────────────────────────────────────────

pub fn spawn_combat_scene(
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    asset_server: Res<AssetServer>,
    windows: Query<&Window>,
    mut ctx: ResMut<CombatContext>,
    mut objective: ResMut<CombatObjective>,
    mut blocked_hexes: ResMut<CombatBlockedHexes>,
    mut log: ResMut<CombatLog>,
    mut cursor: ResMut<ConsoleCursor>,
    mut popup_cursor: ResMut<PopupCursor>,
    mut anim_queue: ResMut<AnimationQueue>,
    mut deadline: ResMut<PhaseDeadline>,
    tag_cache: Res<AbilityTagCache>,
) {
    spawn_combatants(&mut commands, &db, &scenario, &mut objective, &mut blocked_hexes, &tag_cache);
    spawn_background(&mut commands, &db, &scenario, &asset_server, &windows);
    reset_combat_state(&mut ctx, &mut log, &mut cursor, &mut popup_cursor, &mut anim_queue, &mut deadline);
}

fn spawn_background(
    commands: &mut Commands,
    db: &GameDb,
    scenario: &ScenarioState,
    asset_server: &AssetServer,
    windows: &Query<&Window>,
) {
    let scen = db.scenarios.get(&scenario.scenario_id).unwrap();
    let location = match &scen.scenes[scenario.scene_index] {
        SceneDef::Combat { location: Some(loc), .. } => loc,
        _ => return,
    };
    let rel_path = format!("images/battle_backgrounds/{location}.png");
    if !std::path::Path::new("assets").join(&rel_path).exists() {
        warn!("battle background not found: {rel_path}");
        return;
    }
    let handle: Handle<Image> = asset_server.load(&rel_path);
    let size = windows.single().ok().map(|w| Vec2::new(w.width(), w.height()));
    commands.spawn((
        BattleBackground,
        Sprite {
            image: handle,
            custom_size: size,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));
}

pub fn despawn_combatants(
    mut commands: Commands,
    combatants: Query<Entity, With<Combatant>>,
    tokens: Query<Entity, With<UnitToken>>,
    backgrounds: Query<Entity, With<BattleBackground>>,
    mut positions: ResMut<HexPositions>,
    mut corpses: ResMut<HexCorpses>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut sel: ResMut<SelectionState>,
    mut anim_queue: ResMut<AnimationQueue>,
    popups: Query<Entity, With<crate::ui::animation::EnemyActionPopup>>,
) {
    for entity in combatants.iter().chain(tokens.iter()).chain(popups.iter()).chain(backgrounds.iter()) {
        commands.entity(entity).despawn();
    }
    positions.clear();
    corpses.clear();
    queue.order.clear();
    queue.index = 0;
    ctx.encounter = None;
    sel.clear();
    anim_queue.0.clear();
    // Engine mirror teardown (CombatStateRes / UnitIdMap / PendingPhaseTransitions)
    // is owned by CombatPipelinePlugin via reset_engine_mirrors_on_exit_combat,
    // which runs on the same OnExit(AppState::Combat) trigger as this system.
}

// ── Restart combat ──────────────────────────────────────────────────────────

/// Сохраняет инициативу в `PresetInitiative`, полностью пересоздаёт сцену.
/// `build_turn_order` подхватит сохранённые значения вместо бросков кубика.
pub fn restart_combat_system(
    mut reader: MessageReader<RestartCombat>,
    mut commands: Commands,
    db: Res<GameDb>,
    scenario: Res<ScenarioState>,
    tag_cache: Res<AbilityTagCache>,
    combatants: Query<(Entity, &Name, &Initiative), With<Combatant>>,
    cleanup: Query<Entity, Or<(With<UnitToken>, With<crate::ui::animation::EnemyActionPopup>)>>,
    mut preset: ResMut<PresetInitiative>,
    mut positions: ResMut<HexPositions>,
    mut corpses: ResMut<HexCorpses>,
    mut queue: ResMut<TurnQueue>,
    mut ctx: ResMut<CombatContext>,
    mut objective: ResMut<CombatObjective>,
    mut reset_bundle: (
        ResMut<CombatBlockedHexes>,
        ResMut<CombatLog>,
        ResMut<ConsoleCursor>,
        ResMut<PopupCursor>,
        ResMut<AnimationQueue>,
        ResMut<PhaseDeadline>,
    ),
    mut sel: ResMut<SelectionState>,
    mut next_phase: ResMut<NextState<CombatPhase>>,
) {
    if reader.read().next().is_none() {
        return;
    }

    let (blocked_hexes, log, cursor, popup_cursor, anim_queue, deadline) = &mut reset_bundle;

    // 1. Save initiative by name.
    preset.0.clear();
    for (_, name, init) in &combatants {
        preset.0.insert(name.as_str().to_string(), init.0);
    }

    // 2. Despawn combatants, tokens, popups.
    for (entity, _, _) in &combatants {
        commands.entity(entity).despawn();
    }
    for entity in &cleanup {
        commands.entity(entity).despawn();
    }
    positions.clear();
    corpses.clear();
    queue.order.clear();
    queue.index = 0;
    sel.clear();

    // Engine mirror teardown (CombatStateRes / UnitIdMap / PendingPhaseTransitions)
    // is owned by CombatPipelinePlugin's reset_engine_mirrors_on_restart, which
    // reads the same RestartCombat message via its own independent reader.

    // 3. Spawn fresh combatants + reset state.
    spawn_combatants(&mut commands, &db, &scenario, &mut objective, blocked_hexes, &tag_cache);
    reset_combat_state(&mut ctx, log, cursor, popup_cursor, anim_queue, deadline);

    // 4. → StartRound, где assign_hex_positions создаст токены,
    //    а build_turn_order возьмёт инициативу из PresetInitiative.
    next_phase.set(CombatPhase::StartRound);
}
