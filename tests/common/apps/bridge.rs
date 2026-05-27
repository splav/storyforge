//! Bridge-layer test harness — Bevy App + engine + ECS projection setup.
//!
//! # HARNESS INVARIANT
//! `bridge_app()` and `projector_only_app()` do NOT spawn entities.
//! Tests can rely on Entity ids being stable across runs (first spawn = generation 0/index 0).
//!
//! # Usage
//! ```ignore
//! use common::bridge::{bridge_app, spawn_caster, spawn_target, bootstrap, insert_ability};
//!
//! let mut app = bridge_app();
//! let caster = spawn_caster(&mut app, hex_from_offset(0, 0), vec!["my_ability".into()]);
//! let target = spawn_target(&mut app, hex_from_offset(1, 0));
//! bootstrap(&mut app);
//! insert_ability(&mut app, my_def);
//! ```
//!
//! Used by: tests/combat_engine/bridge_smoke.rs (and future bridge-layer suites).

#![allow(dead_code)]

use bevy::math::Vec2;
use bevy::prelude::*;

use storyforge::combat::{
    DiceRngRes,
    engine_bridge::{
        apply_bridge_queues_pre_projection, apply_bridge_queues_post_projection,
        bootstrap_combat_state, entity_to_uid,
        process_action_system, project_state_to_ecs,
        BridgeQueues, CombatStateRes, UnitIdMap,
    },
};
use storyforge::combat::ai::log::AiLogger;
use storyforge::combat::ai::log::engine_trace::EngineTraceWriter;
use storyforge::combat::ai::log::PendingAiLogEntries;
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::content::abilities::{AbilityDef, AbilityRange, AoEShape, EffectDef};
use storyforge::content::content_view::{ActiveContent, ContentView};
use storyforge::content::statuses::StatusDef;
use storyforge::content::weapons::{HandType, WeaponDef};
use combat_engine::{AbilityId, DiceExpr, WeaponId};
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{CombatStats, Equipment, Team};
use storyforge::game::hex::Hex;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{CombatContext, HexCorpses, HexPositions, TurnQueue};
use storyforge::ui::animation::AnimationQueue;
use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

use combat_engine::state::Unit;

// ─── App builders ─────────────────────────────────────────────────────────────

/// Build a full bridge App: process_action + projector + phase_transitions + ai_log.
///
/// Mirrors the structure of `bridge_smoke::bridge_app` but does NOT spawn
/// entities — tests must spawn explicitly via [`spawn_unit`] / [`spawn_caster`] /
/// [`spawn_target`], then call [`bootstrap`] before the first `app.update()`.
pub fn bridge_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<TurnQueue>()
        .init_resource::<CombatContext>()
        .init_resource::<ActiveContent>()
        .init_resource::<DiceRngRes>()
        .init_resource::<CombatLog>()
        .init_resource::<AnimationQueue>()
        .insert_resource(HexGridOffset(Vec2::ZERO))
        .insert_resource(AbilityTagCache::default())
        .insert_resource(HexMaterials {
            empty: Handle::default(),
            player: Handle::default(),
            enemy: Handle::default(),
            dead: Handle::default(),
            in_range: Handle::default(),
            in_range_dim: Handle::default(),
            move_range: Handle::default(),
            border_active: Handle::default(),
            border_target: Handle::default(),
            border_in_range: Handle::default(),
            border_in_range_dim: Handle::default(),
            border_move: Handle::default(),
            aoe_preview: Handle::default(),
            border_aoe: Handle::default(),
            token_player: Handle::default(),
            token_enemy: Handle::default(),
            token_dead: Handle::default(),
        })
        .insert_resource(TokenMesh {
            token: Handle::default(),
            ring: Handle::default(),
        })
        .init_resource::<BridgeQueues>()
        .init_resource::<EngineTraceWriter>()
        .init_resource::<AiLogger>()
        .init_resource::<PendingAiLogEntries>()
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (
                process_action_system,
                apply_bridge_queues_pre_projection,
                project_state_to_ecs,
                apply_bridge_queues_post_projection,
                storyforge::combat::ai::log::flush_pending_ai_log_system,
            )
                .chain(),
        );
    app
}

/// Projector-only App: only `project_state_to_ecs` in PostUpdate.
///
/// Used to test the projector in isolation: seed `CombatStateRes` manually,
/// then run `app.update()` — the projector writes to ECS without any mirror
/// system clobbering the state first.
pub fn projector_only_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
        .init_resource::<HexCorpses>()
        .init_resource::<CombatContext>()
        .add_message::<ActionInput>()
        .add_systems(PostUpdate, project_state_to_ecs);
    app
}

// ─── Bootstrap ────────────────────────────────────────────────────────────────

/// Run `bootstrap_combat_state` once after all units are spawned.
///
/// `bridge_app()` has no state machine, so `OnEnter(AwaitCommand)` cannot fire.
/// Call this after your spawn block and any direct ECS mutations, but before
/// the first `app.update()` that runs `process_action_system`.
pub fn bootstrap(app: &mut App) {
    use bevy::ecs::system::RunSystemOnce;
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
}

// ─── Scripted RNG ─────────────────────────────────────────────────────────────

/// Script the next d20 draw to 11 (non-1, non-20 — no crit-fail, no crit).
///
/// Use ONCE before a single Cast input. The scripted queue holds exactly one
/// value; if the test triggers a second d20 draw, `DiceRng` will panic by
/// design — this surfaces hidden RNG draws that the test author must account for.
pub fn script_no_crit_fail(app: &mut App) {
    app.world_mut()
        .resource_mut::<DiceRngRes>()
        .script(&[11]);
}

/// Script the next d20 draw to an arbitrary value.
///
/// Use before a Cast input when you need to control the crit-fail check.
/// `d20 == 1` triggers crit-fail; any other value does not.
pub fn script_d20(app: &mut App, value: i32) {
    app.world_mut()
        .resource_mut::<DiceRngRes>()
        .script(&[value]);
}

// ─── Stats / equipment presets ────────────────────────────────────────────────

/// Bridge-tier stats preset: max_hp=20, strength=5 (str_mod=2).
///
/// Distinct from `common::base_stats` (max_hp=10) — bridge tests often assert
/// on HP values after damage, so the larger pool avoids accidental kills.
pub fn bridge_stats() -> CombatStats {
    CombatStats {
        max_hp: 20,
        strength: 5,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

/// Default equipment with a short_sword + cloth armor. Mirrors the legacy
/// test_equipment() used by some bridge tests (notably the engine_trace
/// record/replay smoke test and the projector_writes_engine_mutation_to_ecs
/// Phase B-D transplant flow).
pub fn default_equipment() -> Equipment {
    Equipment {
        main_hand: Some("short_sword".into()),
        off_hand: None,
        chest: "mage_robe".into(),
        legs: "cloth_pants".into(),
        feet: "cloth_shoes".into(),
    }
}

/// Empty equipment — for casters / targets that don't need a weapon.
pub fn no_equipment() -> Equipment {
    Equipment {
        main_hand: None,
        off_hand: None,
        chest: "".into(),
        legs: "".into(),
        feet: "".into(),
    }
}

// ─── Entity spawners ──────────────────────────────────────────────────────────

/// Spawn a combatant, register its position in `HexPositions`, and return the
/// `Entity`. Does NOT call [`bootstrap`] — call that after all spawning is done.
pub fn spawn_unit(
    app: &mut App,
    team: Team,
    stats: CombatStats,
    armor: i32,
    speed: i32,
    abilities: Vec<AbilityId>,
    equipment: Equipment,
    pos: Hex,
) -> Entity {
    let entity = app
        .world_mut()
        .spawn(CombatantBundle::new(team, stats, armor, speed, abilities, equipment))
        .id();
    app.world_mut()
        .resource_mut::<HexPositions>()
        .insert(entity, pos);
    entity
}

/// Convenience: spawn a Player unit at `pos` with [`bridge_stats`],
/// [`no_equipment`], and the given abilities.
pub fn spawn_caster(app: &mut App, pos: Hex, abilities: Vec<AbilityId>) -> Entity {
    spawn_unit(app, Team::Player, bridge_stats(), 0, 6, abilities, no_equipment(), pos)
}

/// Like [`spawn_caster`] but with explicit `speed` — used by movement tests
/// that need a non-default speed (e.g., speed=1 to test bonus-movement exhaustion).
pub fn spawn_caster_with_speed(
    app: &mut App,
    pos: Hex,
    abilities: Vec<AbilityId>,
    speed: i32,
) -> Entity {
    spawn_unit(app, Team::Player, bridge_stats(), 0, speed, abilities, no_equipment(), pos)
}

/// Convenience: spawn an Enemy unit at `pos` with [`bridge_stats`],
/// no abilities, and [`no_equipment`].
pub fn spawn_target(app: &mut App, pos: Hex) -> Entity {
    spawn_unit(app, Team::Enemy, bridge_stats(), 0, 6, vec![], no_equipment(), pos)
}

/// Spawn an Enemy with a weapon in `main_hand` — used by AoO-flavored tests
/// where the enemy must have a melee weapon to provoke or react.
pub fn spawn_enemy_with_weapon(
    app: &mut App,
    pos: Hex,
    abilities: Vec<AbilityId>,
    weapon_id: WeaponId,
) -> Entity {
    let equipment = Equipment {
        main_hand: Some(weapon_id),
        off_hand: None,
        chest: "".into(),
        legs: "".into(),
        feet: "".into(),
    };
    spawn_unit(app, Team::Enemy, bridge_stats(), 0, 6, abilities, equipment, pos)
}

// ─── Content injection ────────────────────────────────────────────────────────

/// Inject a single `AbilityDef` into `ActiveContent`.
///
/// Call before [`bootstrap`] if the ability must be visible to the engine during
/// bootstrap, or after if only needed for the cast path.
pub fn insert_ability(app: &mut App, def: AbilityDef) {
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .abilities
        .insert(def.id.clone(), def);
}

// ─── Melee content builder ────────────────────────────────────────────────────

/// Builder for a synthetic melee `ContentView` (weapon + WeaponAttack ability +
/// optional status definitions) used by AoO-flavoured tests.
///
/// ```ignore
/// let cv = melee_content(&ability_id, &weapon_id)
///     .with_status(stun_def)
///     .into_view();
/// app.insert_resource(ActiveContent(cv));
/// ```
pub struct MeleeContent {
    pub ability_id: AbilityId,
    pub weapon_id: WeaponId,
    pub statuses: Vec<StatusDef>,
}

/// Start building a `MeleeContent`. Creates a 1d6 `WeaponAttack` ability + sword.
pub fn melee_content(ability_id: &AbilityId, weapon_id: &WeaponId) -> MeleeContent {
    MeleeContent {
        ability_id: ability_id.clone(),
        weapon_id: weapon_id.clone(),
        statuses: vec![],
    }
}

impl MeleeContent {
    /// Add a `StatusDef` to the content view (for stun-filter tests, etc.).
    pub fn with_status(mut self, status: StatusDef) -> Self {
        self.statuses.push(status);
        self
    }

    /// Consume the builder and produce a `ContentView` ready for `ActiveContent`.
    pub fn into_view(self) -> ContentView {
        let sword = WeaponDef {
            id: self.weapon_id.clone(),
            name: "Test Sword".into(),
            hand: HandType::MainHand,
            dice: DiceExpr::new(1, 6, 0),
            spell_power: 0,
            armor: 0,
            max_hp: 0,
            strength: 0,
            dexterity: 0,
            constitution: 0,
            intelligence: 0,
            wisdom: 0,
            charisma: 0,
        };
        let ability = AbilityDef {
            id: self.ability_id.clone(),
            name: "Test Attack".into(),
            magic_domains: vec![],
            magic_method: String::new(),
            ai_tags_override: None,
            is_move_toggle: false,
            engine: combat_engine::AbilityDef {
                target_type: storyforge::content::abilities::TargetType::SingleEnemy,
                range: AbilityRange::MELEE,
                effect: EffectDef::WeaponAttack,
                costs: vec![],
                cost_ap: 1,
                aoe: AoEShape::None,
                friendly_fire: false,
                statuses: vec![],
                key: None,
                requires_los: false,
            },
        };
        let mut cv = ContentView::default();
        cv.abilities.insert(self.ability_id.clone(), ability);
        cv.weapons.insert(self.weapon_id.clone(), sword);
        for status in self.statuses {
            cv.statuses.insert(status.id.clone(), status);
        }
        cv
    }
}

// ─── Input messages ───────────────────────────────────────────────────────────

/// Write an `ActionInput::Move` message for `actor` along `path`.
/// Saves the 3-line `resource_mut::<Messages<ActionInput>>().write(...)` ceremony.
pub fn write_move(app: &mut App, actor: Entity, path: Vec<Hex>) {
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Move { actor, path });
}

/// Write an `ActionInput::Cast` message. `target_pos` is the hex; `target_entity`
/// is the targeted unit (or the caster for self-cast).
pub fn write_cast(
    app: &mut App,
    actor: Entity,
    ability: AbilityId,
    target: Entity,
    target_pos: Hex,
) {
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<ActionInput>>()
        .write(ActionInput::Cast {
            actor,
            ability,
            target,
            target_pos,
        });
}

// ─── Engine state mutation ────────────────────────────────────────────────────

/// Mutate an engine `Unit` in `CombatStateRes` for a given Bevy `Entity`.
///
/// Converts `Entity → UnitId` via `entity_to_uid` (deterministic bit-cast,
/// no UnitIdMap lookup). Panics if the unit is not found in the engine state
/// (which means [`bootstrap`] was not called or the entity was not spawned
/// before bootstrap).
pub fn with_engine_unit<F>(app: &mut App, entity: Entity, f: F)
where
    F: FnOnce(&mut Unit),
{
    let uid = entity_to_uid(entity);
    let mut state = app.world_mut().resource_mut::<CombatStateRes>();
    let unit = state
        .0
        .unit_mut(uid)
        .unwrap_or_else(|| panic!("with_engine_unit: entity {entity:?} not found in CombatStateRes — was bootstrap() called?"));
    f(unit);
}
