//! Legality parity: `BevyActions` (UI tooltip path) vs `EngineCheckState`
//! (engine `step()` path).
//!
//! For each test case, calls `check_legality` against BOTH adapters and asserts
//! they return identical `Result<LegalAction, IllegalReason>`.
//!
//! **Known intentional asymmetries** (excluded from test cases):
//!
//! 1. `TargetOutOfBounds` — `EngineCheckState::is_in_bounds` always returns
//!    `true`; `BevyActions` calls `in_bounds`. Test cases stay within bounds.
//!
//! 2. `AbilityNotInList` — `EngineCheckState::actor_knows_ability` always
//!    returns `true`; `BevyActions` checks the actor's `Abilities` component.
//!    Test cases only use abilities the actor actually knows.
//!
//! All other `IllegalReason` variants are exercised.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use storyforge::combat::engine_bridge::{
    apply_phase_transitions_system, bootstrap_combat_state, entity_to_uid, process_action_system,
    project_state_to_ecs, CombatStateRes, PendingPhaseTransitions, UnitIdMap,
};
use storyforge::combat::legality_adapter::BevyActions;
use storyforge::combat::ai::world::tags::AbilityTagCache;
use storyforge::combat::DiceRngRes;
use storyforge::combat_engine::{
    check_legality, AbilityDef, AbilityRange, AoEShape, DiceExpr, EffectDef,
    EngineCheckState, IllegalReason, LegalAction, ProposedAction, ResourceKind,
    StatusBonuses, StatusDef as EngineStatusDef, UnitTemplate,
};
use storyforge::combat_engine::content::{ContentView as EngineContentView, Cost, TargetType as EngineTargetType};
use storyforge::combat_engine::state::UnitId;
use storyforge::content::abilities::AbilityDef as BevyAbilityDef;
use storyforge::content::content_view::{ActiveContent, ContentView as BevyContentView};
use storyforge::content::statuses::StatusDef as BevyStatusDef;
use storyforge::core::AbilityId;
use storyforge::game::bundles::CombatantBundle;
use storyforge::game::combat_log::CombatLog;
use storyforge::game::components::{
    ActionPoints, ActiveStatus as EcsActiveStatus, CombatStats, Equipment, Mana,
    StatusEffects, Team, ValidationActorQ, ValidationTargetQ,
};
use storyforge::game::hex::hex_from_offset;
use storyforge::game::messages::ActionInput;
use storyforge::game::resources::{CombatContext, HexPositions, TurnQueue};
use storyforge::ui::animation::AnimationQueue;
use storyforge::ui::hex_grid::{HexGridOffset, HexMaterials, TokenMesh};

// ── IDs ───────────────────────────────────────────────────────────────────────

const ATTACK_ID: &str = "test_attack";
const HEAL_ID: &str = "test_heal";
const MANA_SPELL_ID: &str = "test_mana_spell";
const MANA_BLOCK_STATUS: &str = "mana_block";
const TAUNT_STATUS: &str = "taunt";

// ── Engine-side ContentView wrapper ──────────────────────────────────────────

/// Thin `EngineContentView` wrapper over `&BevyContentView`.
///
/// Mirrors exactly what `EcsContentView` (pub(crate)) does in
/// `src/combat/engine_bridge.rs`, so the engine adapter sees the same content
/// as the Bevy adapter.
struct TestEngineContent<'a>(&'a BevyContentView);

impl<'a> EngineContentView for TestEngineContent<'a> {
    fn status_bonuses(&self, _: &storyforge::combat_engine::StatusId) -> StatusBonuses {
        StatusBonuses::default()
    }

    fn ability_def(&self, id: &storyforge::combat_engine::AbilityId) -> Option<&AbilityDef> {
        self.0.abilities.get(id).map(|a| &a.engine)
    }

    fn status_def(&self, id: &storyforge::combat_engine::StatusId) -> Option<&EngineStatusDef> {
        self.0.statuses.get(id).map(|s| &s.engine)
    }

    fn unit_template(&self, _: &str) -> Option<UnitTemplate> {
        None
    }
}

// ── App fixture ───────────────────────────────────────────────────────────────

fn bridge_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<CombatStateRes>()
        .init_resource::<UnitIdMap>()
        .init_resource::<HexPositions>()
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
        .init_resource::<PendingPhaseTransitions>()
        .init_resource::<storyforge::combat::ai::log::engine_trace::EngineTraceWriter>()
        .init_resource::<storyforge::combat::ai::log::AiLogger>()
        .init_resource::<storyforge::combat::ai::log::PendingAiLogEntries>()
        .add_message::<ActionInput>()
        .add_systems(
            Update,
            (
                process_action_system,
                project_state_to_ecs,
                apply_phase_transitions_system,
                storyforge::combat::ai::log::flush_pending_ai_log_system,
            )
                .chain(),
        );
    app
}

fn init_bridge_engine_state(app: &mut App) {
    app.world_mut()
        .run_system_once(bootstrap_combat_state)
        .expect("bootstrap_combat_state failed");
}

fn no_equip() -> Equipment {
    Equipment {
        main_hand: None,
        off_hand: None,
        chest: "".into(),
        legs: "".into(),
        feet: "".into(),
    }
}

fn base_stats() -> CombatStats {
    CombatStats {
        max_hp: 20,
        strength: 0,
        dexterity: 5,
        constitution: 10,
        intelligence: 0,
        wisdom: 10,
        charisma: 10,
    }
}

// ── Content insertion ─────────────────────────────────────────────────────────

fn insert_attack(app: &mut App) {
    let def = BevyAbilityDef {
        id: ATTACK_ID.into(),
        name: "Test Attack".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: AbilityDef {
            key: None,
            cost_ap: 1,
            costs: vec![],
            range: AbilityRange { min: 0, max: 5 },
            target_type: EngineTargetType::SingleEnemy,
            aoe: AoEShape::None,
            friendly_fire: false,
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            statuses: vec![],
        },
    };
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .abilities
        .insert(ATTACK_ID.into(), def);
}

fn insert_heal(app: &mut App) {
    let def = BevyAbilityDef {
        id: HEAL_ID.into(),
        name: "Test Heal".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: AbilityDef {
            key: None,
            cost_ap: 1,
            costs: vec![],
            range: AbilityRange { min: 0, max: 5 },
            target_type: EngineTargetType::SingleAlly,
            aoe: AoEShape::None,
            friendly_fire: false,
            effect: EffectDef::Heal { dice: DiceExpr::new(1, 4, 0) },
            statuses: vec![],
        },
    };
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .abilities
        .insert(HEAL_ID.into(), def);
}

fn insert_mana_spell(app: &mut App) {
    let def = BevyAbilityDef {
        id: MANA_SPELL_ID.into(),
        name: "Test Mana Spell".into(),
        magic_domains: vec![],
        magic_method: String::new(),
        ai_tags_override: None,
        is_move_toggle: false,
        engine: AbilityDef {
            key: None,
            cost_ap: 1,
            costs: vec![Cost { resource: ResourceKind::Mana, amount: 2 }],
            range: AbilityRange { min: 0, max: 5 },
            target_type: EngineTargetType::SingleEnemy,
            aoe: AoEShape::None,
            friendly_fire: false,
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            statuses: vec![],
        },
    };
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .abilities
        .insert(MANA_SPELL_ID.into(), def);
}

fn insert_mana_block_status(app: &mut App) {
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .statuses
        .insert(
            MANA_BLOCK_STATUS.into(),
            BevyStatusDef {
                id: MANA_BLOCK_STATUS.into(),
                name: "Mana Block".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: EngineStatusDef {
                    armor_bonus: 0,
                    damage_taken_bonus: 0,
                    skips_turn: false,
                    forces_targeting: false,
                    blocks_mana_abilities: true,
                    speed_bonus: 0,
                    hp_percent_dot: 0,
                    causes_disadvantage: false,
                },
            },
        );
}

fn insert_taunt_status(app: &mut App) {
    app.world_mut()
        .resource_mut::<ActiveContent>()
        .0
        .statuses
        .insert(
            TAUNT_STATUS.into(),
            BevyStatusDef {
                id: TAUNT_STATUS.into(),
                name: "Taunt".into(),
                dot_dice: None,
                ai_controlled: false,
                buff_class: None,
                engine: EngineStatusDef {
                    armor_bonus: 0,
                    damage_taken_bonus: 0,
                    skips_turn: false,
                    forces_targeting: true,
                    blocks_mana_abilities: false,
                    speed_bonus: 0,
                    hp_percent_dot: 0,
                    causes_disadvantage: false,
                },
            },
        );
}

// ── Parity test ───────────────────────────────────────────────────────────────

type CaseResult = (
    &'static str,
    Result<LegalAction, IllegalReason>,
    Result<LegalAction, IllegalReason>,
);

/// Legality parity: `BevyActions` (UI path) vs `EngineCheckState` (engine path).
///
/// 12 canonical cases; each exercises a distinct rule branch.
#[test]
fn legality_parity_bevy_vs_engine() {
    let mut app = bridge_app();

    // ── Insert content ────────────────────────────────────────────────────────
    insert_attack(&mut app);
    insert_heal(&mut app);
    insert_mana_spell(&mut app);
    insert_mana_block_status(&mut app);
    insert_taunt_status(&mut app);

    // ── Spawn positions ───────────────────────────────────────────────────────
    let actor_pos = hex_from_offset(0, 0);
    let enemy_pos = hex_from_offset(1, 0);
    let ally_pos = hex_from_offset(0, 1);
    let taunter_pos = hex_from_offset(2, 0);
    let dead_enemy_pos = hex_from_offset(3, 0);
    let actor_no_ap_pos = hex_from_offset(0, 2);
    let actor_no_mana_pos = hex_from_offset(0, 3);
    let actor_mana_blocked_pos = hex_from_offset(0, 4);
    // far_pos: in bounds but distance > range max (5).
    // Grid is 7 rows × max 8 cols; hex_from_offset(6, 0) is in bounds (row 0,
    // col 6 < GRID_COLS-1=7) and distance 6 from (0,0) > range max 5.
    let far_pos = hex_from_offset(6, 0);

    // ── Spawn units ───────────────────────────────────────────────────────────

    // actor: Player with all three abilities and mana (3 current / 5 max).
    let actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            base_stats(),
            0,
            6,
            vec![ATTACK_ID.into(), HEAL_ID.into(), MANA_SPELL_ID.into()],
            no_equip(),
        ))
        .id();
    app.world_mut().entity_mut(actor).insert(Mana { current: 3, max: 5 });

    // enemy: Enemy, alive.
    let enemy = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();

    // ally: Player, alive.
    let ally = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Player, base_stats(), 0, 6, vec![], no_equip()))
        .id();

    // taunter: Enemy alive with taunt status.
    let taunter = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();
    app.world_mut()
        .entity_mut(taunter)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(EcsActiveStatus {
            id: TAUNT_STATUS.into(),
            rounds_remaining: 3,
            dot_per_tick: 0,
            applier: taunter,
        });

    // dead_enemy: Enemy with hp=0.
    let dead_enemy = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();
    app.world_mut()
        .entity_mut(dead_enemy)
        .get_mut::<storyforge::game::components::Vital>()
        .unwrap()
        .hp = 0;

    // actor_no_ap: Player, AP=0, mana=5.
    let actor_no_ap = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            base_stats(),
            0,
            6,
            vec![ATTACK_ID.into(), MANA_SPELL_ID.into()],
            no_equip(),
        ))
        .id();
    app.world_mut()
        .entity_mut(actor_no_ap)
        .get_mut::<ActionPoints>()
        .unwrap()
        .action_points = 0;
    app.world_mut().entity_mut(actor_no_ap).insert(Mana { current: 5, max: 5 });

    // actor_no_mana: Player, mana=0/5.
    let actor_no_mana = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            base_stats(),
            0,
            6,
            vec![MANA_SPELL_ID.into()],
            no_equip(),
        ))
        .id();
    app.world_mut().entity_mut(actor_no_mana).insert(Mana { current: 0, max: 5 });

    // actor_mana_blocked: Player, mana=5, carries mana_block status.
    let actor_mana_blocked = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            base_stats(),
            0,
            6,
            vec![MANA_SPELL_ID.into()],
            no_equip(),
        ))
        .id();
    app.world_mut().entity_mut(actor_mana_blocked).insert(Mana { current: 5, max: 5 });
    app.world_mut()
        .entity_mut(actor_mana_blocked)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(EcsActiveStatus {
            id: MANA_BLOCK_STATUS.into(),
            rounds_remaining: 2,
            dot_per_tick: 0,
            applier: actor_mana_blocked,
        });

    // ── Register hex positions ────────────────────────────────────────────────
    {
        let mut pos = app.world_mut().resource_mut::<HexPositions>();
        pos.insert(actor, actor_pos);
        pos.insert(enemy, enemy_pos);
        pos.insert(ally, ally_pos);
        pos.insert(taunter, taunter_pos);
        pos.insert(dead_enemy, dead_enemy_pos);
        pos.insert(actor_no_ap, actor_no_ap_pos);
        pos.insert(actor_no_mana, actor_no_mana_pos);
        pos.insert(actor_mana_blocked, actor_mana_blocked_pos);
    }

    // ── Sync engine state from ECS ────────────────────────────────────────────
    init_bridge_engine_state(&mut app);

    // ── UnitId aliases ────────────────────────────────────────────────────────
    let actor_uid = entity_to_uid(actor);
    let enemy_uid = entity_to_uid(enemy);
    let ally_uid = entity_to_uid(ally);
    let taunter_uid = entity_to_uid(taunter);
    let dead_enemy_uid = entity_to_uid(dead_enemy);
    let actor_no_ap_uid = entity_to_uid(actor_no_ap);
    let actor_no_mana_uid = entity_to_uid(actor_no_mana);
    let actor_mana_blocked_uid = entity_to_uid(actor_mana_blocked);

    // ── Run parity check inside a one-shot Bevy system ────────────────────────
    //
    // `run_system_once` requires `'static` closures; we can't borrow local
    // `AbilityId`s from outside. Instead, build the `ProposedAction`s (which
    // borrow the ability ids) INSIDE the closure where the ids live on the
    // stack.  All Entity / UnitId values are `Copy` and captured by value.

    let results: Vec<CaseResult> = app
        .world_mut()
        .run_system_once(
            move |
                content: Res<ActiveContent>,
                positions: Res<HexPositions>,
                actor_q: Query<ValidationActorQ>,
                target_q: Query<ValidationTargetQ>,
                combat_state: Res<CombatStateRes>,
            | -> Vec<CaseResult> {
                let bevy_adapter = BevyActions {
                    content: &content,
                    positions: &positions,
                    actors: &actor_q,
                    targets: &target_q,
                };
                let engine_content = TestEngineContent(&content.0);
                let engine_adapter = EngineCheckState {
                    state: &combat_state.0,
                    content: &engine_content,
                };

                // Ability ids live on the closure stack — ProposedAction borrows are valid.
                let attack_id: AbilityId = ATTACK_ID.into();
                let heal_id: AbilityId = HEAL_ID.into();
                let mana_id: AbilityId = MANA_SPELL_ID.into();

                // Each case: (name, bevy result, engine result).
                // Cases 11-12 use taunter scenario — taunter is alive with forces_targeting.
                let run = |b: ProposedAction<Entity>, e: ProposedAction<UnitId>| -> (Result<LegalAction, IllegalReason>, Result<LegalAction, IllegalReason>) {
                    (check_legality(b, &bevy_adapter), check_legality(e, &engine_adapter))
                };

                vec![
                    // 1. Happy-path SingleEnemy in range — Ok.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("attack_in_range", br, er)
                    },
                    // 2. Out of range — OutOfRange.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: enemy, target_pos: far_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: enemy_uid, target_pos: far_pos },
                        );
                        ("attack_out_of_range", br, er)
                    },
                    // 3. Dead target — TargetDead.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: dead_enemy, target_pos: dead_enemy_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: dead_enemy_uid, target_pos: dead_enemy_pos },
                        );
                        ("attack_dead_target", br, er)
                    },
                    // 4. SingleEnemy at own-team ally — WrongTargetTeam.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: ally, target_pos: ally_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: ally_uid, target_pos: ally_pos },
                        );
                        ("attack_wrong_team", br, er)
                    },
                    // 5. AP=0 — NotEnoughAp.
                    {
                        let (br, er) = run(
                            ProposedAction { actor: actor_no_ap, ability: &attack_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_no_ap_uid, ability: &attack_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("attack_no_ap", br, er)
                    },
                    // 6. Mana spell with sufficient mana (3 >= cost 2) — Ok.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &mana_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_uid, ability: &mana_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("mana_spell_sufficient_mana", br, er)
                    },
                    // 7. Mana spell with mana=0 — InsufficientResource(Mana).
                    {
                        let (br, er) = run(
                            ProposedAction { actor: actor_no_mana, ability: &mana_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_no_mana_uid, ability: &mana_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("mana_spell_no_mana", br, er)
                    },
                    // 8. Mana spell with mana_block status — BlockedByStatus.
                    {
                        let (br, er) = run(
                            ProposedAction { actor: actor_mana_blocked, ability: &mana_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_mana_blocked_uid, ability: &mana_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("mana_spell_blocked_by_status", br, er)
                    },
                    // 9. SingleAlly heal on teammate — Ok.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &heal_id, target: ally, target_pos: ally_pos },
                            ProposedAction { actor: actor_uid, ability: &heal_id, target: ally_uid, target_pos: ally_pos },
                        );
                        ("heal_on_ally_legal", br, er)
                    },
                    // 10. SingleAlly heal on enemy — WrongTargetTeam.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &heal_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_uid, ability: &heal_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("heal_on_enemy_wrong_team", br, er)
                    },
                    // 11. Taunter alive: attack non-taunter enemy — TauntForcesTarget.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: enemy, target_pos: enemy_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: enemy_uid, target_pos: enemy_pos },
                        );
                        ("taunt_forces_non_taunter_target", br, er)
                    },
                    // 12. Taunter alive: attack the taunter — Ok (constraint satisfied).
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: taunter, target_pos: taunter_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: taunter_uid, target_pos: taunter_pos },
                        );
                        ("taunt_attack_taunter_legal", br, er)
                    },
                ]
            },
        )
        .expect("run_system_once failed");

    // ── Assert parity ─────────────────────────────────────────────────────────
    let mut divergences: Vec<String> = Vec::new();
    for (name, bevy_result, engine_result) in &results {
        if bevy_result != engine_result {
            divergences.push(format!(
                "  '{name}': bevy={bevy_result:?}  engine={engine_result:?}"
            ));
        }
    }
    assert!(
        divergences.is_empty(),
        "legality divergence between BevyActions (UI) and EngineCheckState (step()):\n{}",
        divergences.join("\n")
    );
}

/// Multi-taunter: two live enemies both carrying `forces_targeting`.
///
/// With the old `taunter_for` (returns first match), clicking the second
/// taunter returned `TauntForcesTarget` — a player deadlock.  This test
/// enforces the fix: both taunters are legal targets; a non-taunter is not.
#[test]
fn multi_taunter_both_are_legal_targets() {
    let mut app = bridge_app();

    insert_attack(&mut app);
    insert_taunt_status(&mut app);

    let actor_pos   = hex_from_offset(0, 0);
    let taunter1_pos = hex_from_offset(1, 0);
    let taunter2_pos = hex_from_offset(2, 0);
    let bystander_pos = hex_from_offset(3, 0);

    // actor: Player, full AP.
    let actor = app
        .world_mut()
        .spawn(CombatantBundle::new(
            Team::Player,
            base_stats(),
            0,
            6,
            vec![ATTACK_ID.into()],
            no_equip(),
        ))
        .id();

    // taunter1: Enemy with taunt status.
    let taunter1 = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();
    app.world_mut()
        .entity_mut(taunter1)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(EcsActiveStatus {
            id: TAUNT_STATUS.into(),
            rounds_remaining: 2,
            dot_per_tick: 0,
            applier: taunter1,
        });

    // taunter2: Enemy with taunt status.
    let taunter2 = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();
    app.world_mut()
        .entity_mut(taunter2)
        .get_mut::<StatusEffects>()
        .unwrap()
        .0
        .push(EcsActiveStatus {
            id: TAUNT_STATUS.into(),
            rounds_remaining: 2,
            dot_per_tick: 0,
            applier: taunter2,
        });

    // bystander: Enemy, no taunt status.
    let bystander = app
        .world_mut()
        .spawn(CombatantBundle::new(Team::Enemy, base_stats(), 0, 6, vec![], no_equip()))
        .id();

    {
        let mut pos = app.world_mut().resource_mut::<HexPositions>();
        pos.insert(actor,    actor_pos);
        pos.insert(taunter1, taunter1_pos);
        pos.insert(taunter2, taunter2_pos);
        pos.insert(bystander, bystander_pos);
    }

    init_bridge_engine_state(&mut app);

    let actor_uid    = entity_to_uid(actor);
    let taunter1_uid = entity_to_uid(taunter1);
    let taunter2_uid = entity_to_uid(taunter2);
    let bystander_uid = entity_to_uid(bystander);

    let results: Vec<CaseResult> = app
        .world_mut()
        .run_system_once(
            move |
                content: Res<ActiveContent>,
                positions: Res<HexPositions>,
                actor_q: Query<ValidationActorQ>,
                target_q: Query<ValidationTargetQ>,
                combat_state: Res<CombatStateRes>,
            | -> Vec<CaseResult> {
                let bevy_adapter = BevyActions {
                    content: &content,
                    positions: &positions,
                    actors: &actor_q,
                    targets: &target_q,
                };
                let engine_content = TestEngineContent(&content.0);
                let engine_adapter = EngineCheckState {
                    state: &combat_state.0,
                    content: &engine_content,
                };

                let attack_id: AbilityId = ATTACK_ID.into();

                let run = |b: ProposedAction<Entity>, e: ProposedAction<UnitId>| {
                    (check_legality(b, &bevy_adapter), check_legality(e, &engine_adapter))
                };

                vec![
                    // Attacking the first taunter must be legal.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: taunter1, target_pos: taunter1_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: taunter1_uid, target_pos: taunter1_pos },
                        );
                        ("multi_taunt_attack_taunter1", br, er)
                    },
                    // Attacking the second taunter must also be legal.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: taunter2, target_pos: taunter2_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: taunter2_uid, target_pos: taunter2_pos },
                        );
                        ("multi_taunt_attack_taunter2", br, er)
                    },
                    // Attacking the non-taunter bystander must be illegal.
                    {
                        let (br, er) = run(
                            ProposedAction { actor, ability: &attack_id, target: bystander, target_pos: bystander_pos },
                            ProposedAction { actor: actor_uid, ability: &attack_id, target: bystander_uid, target_pos: bystander_pos },
                        );
                        ("multi_taunt_attack_bystander_blocked", br, er)
                    },
                ]
            },
        )
        .expect("run_system_once failed");

    // Parity: Bevy and engine must agree on every case.
    let mut divergences: Vec<String> = Vec::new();
    for (name, bevy_result, engine_result) in &results {
        if bevy_result != engine_result {
            divergences.push(format!(
                "  '{name}': bevy={bevy_result:?}  engine={engine_result:?}"
            ));
        }
    }
    assert!(
        divergences.is_empty(),
        "legality divergence between BevyActions and EngineCheckState:\n{}",
        divergences.join("\n")
    );

    // Correctness: assert expected outcomes for each case.
    let by_name = |name: &str| {
        results.iter().find(|(n, _, _)| *n == name).unwrap()
    };

    let (_, bt1, et1) = by_name("multi_taunt_attack_taunter1");
    assert!(bt1.is_ok(), "taunter1 should be a legal target: {bt1:?}");
    assert!(et1.is_ok(), "taunter1 should be a legal target (engine): {et1:?}");

    let (_, bt2, et2) = by_name("multi_taunt_attack_taunter2");
    assert!(bt2.is_ok(), "taunter2 should be a legal target: {bt2:?}");
    assert!(et2.is_ok(), "taunter2 should be a legal target (engine): {et2:?}");

    let (_, bb, eb) = by_name("multi_taunt_attack_bystander_blocked");
    assert_eq!(*bb, Err(IllegalReason::TauntForcesTarget), "bystander should be blocked by taunt: {bb:?}");
    assert_eq!(*eb, Err(IllegalReason::TauntForcesTarget), "bystander should be blocked by taunt (engine): {eb:?}");
}
