//! ECS-free `init_fight`: builds engine `CombatState` purely from content/scenario data.
//!
//! This is the deterministic, Bevy-free counterpart to `bootstrap_combat_state` +
//! `spawn_combatants`.  It receives:
//! - A resolved `ContentView` (already merged global → campaign → scenario).
//! - A resolved `ScenarioDef` + `scene_index` (to derive `active_party` and
//!   `active_party_statuses`).
//! - A resolved `EncounterDef`.
//! - A caller-supplied UnitId assignment callback (option C) so the same ids
//!   used by the ECS path can be fed in for the equivalence test, while the
//!   offline simulation feeds dense `0..N` ids.
//!
//! **Step 2+3** contract: `init_fight` produces a `CombatState` that is
//! byte-identical to the ECS bootstrap when fed the same UnitIds and seed.
//! The live bootstrap is NOT modified here (Step 4 will wire them together).

use std::collections::{HashMap, HashSet};

use combat_engine::{
    enum_map::enum_map,
    state::{
        ActiveStatus, CombatState, EffectSource, EnvId, EnvKind, EnvObject, RoundPhase, Team,
        TeamSet, UnitId,
    },
    AuraDef, CasterContext, DiceExpr as EngineDiceExpr, PhaseEntry, PoolKind, RegenRule,
    TeamRelation, PERMANENT_DURATION,
};

use crate::combat::engine_bridge::{build_ecs_content_view, build_unit, UnitBuildInput};
use crate::content::{
    abilities::{CasterContext as BevyCasterContext, EffectDef},
    content_view::{ActiveContent, ContentView},
    encounters::{AuraAffects, EncounterDef, EnemyDef, VictoryCondition},
    races::CritFailEffect,
    scenarios::{active_party, active_party_statuses, PartyMemberDef, ScenarioDef},
};
use combat_engine::modifier;
use combat_engine::AbilityId;

use super::combat_scene::{collect_keep_alive_names, keep_alive_marker_color};

// ── Public types ──────────────────────────────────────────────────────────────

/// One combatant entering the fight — either a party member (class-based or
/// template-based) or an encounter enemy.  Carries all data needed to build the
/// engine `Unit` and `ProjectionMeta` without any ECS access.
#[derive(Debug, Clone)]
pub enum CombatantSource<'a> {
    /// Hero/ally whose stats come from a `ClassDef`.
    ClassHero {
        member: &'a PartyMemberDef,
        uid: UnitId,
        /// Persistent statuses from `active_party_statuses` for this member.
        party_statuses: Vec<String>,
    },
    /// Party member whose stats come from a `UnitTemplateDef` (e.g. a boat NPC).
    TemplateMember {
        member: &'a PartyMemberDef,
        uid: UnitId,
        /// Persistent statuses from `active_party_statuses` for this member.
        party_statuses: Vec<String>,
    },
    /// Enemy from the encounter definition.
    Enemy { def: &'a EnemyDef, uid: UnitId },
}

impl CombatantSource<'_> {
    /// Returns the UnitId assigned to this combatant.
    pub fn uid(&self) -> UnitId {
        match self {
            Self::ClassHero { uid, .. }
            | Self::TemplateMember { uid, .. }
            | Self::Enemy { uid, .. } => *uid,
        }
    }
}

/// ECS/UI-only data produced alongside each engine `Unit`.
///
/// Contains information the engine `Unit` intentionally does NOT carry (to
/// avoid field drift) but that the Bevy projection layer (Step 4) needs to
/// reconstruct ECS components.  For Steps 2+3 (equivalence gate only) many
/// fields are populated but not asserted on — the type is designed for Step 4.
#[derive(Debug, Clone)]
pub struct ProjectionMeta {
    pub uid: UnitId,
    pub team: Team,
    pub display_name: String,
    /// Id of the UnitTemplate this unit was built from, if any.
    pub template_id: Option<String>,
    /// Starting hex position (for `StartingHexPos` component).
    pub hex_pos: hexx::Hex,
    /// Ability list (for `Abilities` component / AI snapshot).
    pub abilities: Vec<combat_engine::AbilityId>,
    /// Combat path id (for `CombatPath` component), if any.
    pub path: Option<String>,
    /// Mana pool max, if any (>0 means spawn `Mana` component).
    pub mana_max: i32,
    /// Rage pool max, if any (>0 means spawn `Rage` component).
    pub rage_max: i32,
    /// Energy pool max, if any (>0 means spawn `Energy` component).
    pub energy_max: i32,
    /// Max HP (mirrors pools[Hp].1).
    pub max_hp: i32,
    /// Starting HP (mirrors pools[Hp].0 — may differ from max_hp for templates
    /// with `initial_pools.hp`).
    pub initial_hp: i32,
    /// Armor value (for `Vital` reconstruction).
    pub armor: i32,
    /// Base speed (for `Speed` component).
    pub speed: i32,
    /// Max AP (for `ActionPoints` — always 1 in current content).
    pub max_ap: i32,
    /// Reactions max (for `Reactions` — always 1 in current content).
    pub reactions_max: i32,
    /// `VictoryTarget` marker color, if this unit is a kill target.
    pub victory_target: Option<[f32; 3]>,
    /// `KeepAliveTarget` marker color, if this unit must be kept alive.
    pub keep_alive_target: Option<[f32; 3]>,
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Build the engine `CombatState` from content/scenario data — no ECS, no Bevy.
///
/// # Arguments
/// * `content` — the scenario's merged `ContentView` (global → campaign → scenario).
/// * `scenario` — the fully-resolved `ScenarioDef` (party, scenes, encounters).
/// * `scene_index` — which scene index to enter; drives `active_party` and
///   `active_party_statuses`.
/// * `encounter` — the resolved `EncounterDef` for this scene.
/// * `rng` — seeded `DiceRng`; consumed in the same order as the ECS bootstrap.
/// * `preset` — name → initiative override map (mirrors `PresetInitiative`).
/// * `assign_uid` — callback invoked once per `CombatantSource` in spawn order.
///   For the equivalence test, feed the entity-derived ids the ECS path produced;
///   for the offline sim, feed dense `0..N` counters.
///
/// # Returns
/// `(state, metas)`:
/// - `state` — fully-bootstrapped engine `CombatState` (post initiative roll +
///   reconcile + settle_round_start), byte-identical to the ECS bootstrap when
///   given the same UnitIds and seed.
/// - `metas` — per-unit `ProjectionMeta` in spawn order (party first, enemies
///   second), for Step 4 ECS reconstruction.
///
/// # Determinism contract
/// Given the same inputs the function always produces the same `CombatState`.
/// RNG consumption order matches the ECS bootstrap exactly.
pub fn init_fight(
    content: &ContentView,
    scenario: &ScenarioDef,
    scene_index: usize,
    encounter: &EncounterDef,
    rng: &mut combat_engine::DiceRng,
    preset: &HashMap<String, i32>,
    mut assign_uid: impl FnMut(&CombatantSource<'_>) -> UnitId,
) -> (CombatState, Vec<ProjectionMeta>) {
    // Wrap ContentView in ActiveContent so build_unit and bridge helpers work.
    let active_content = ActiveContent(content.clone());

    // Resolve party + persistent statuses.
    let party = active_party(scenario, scene_index);
    let party_status_map: HashMap<String, Vec<String>> =
        active_party_statuses(scenario, scene_index);

    // Pre-compute keep-alive names from victory condition.
    let keep_alive_names = collect_keep_alive_names(&encounter.victory);

    // Build CombatantSource list in spawn order (party first, then enemies —
    // mirrors spawn_combatants).  UIDs start as 0 and are filled by assign_uid.
    let mut sources: Vec<CombatantSource<'_>> = Vec::new();

    for member in &party {
        let statuses = party_status_map
            .get(&member.name)
            .cloned()
            .unwrap_or_default();
        if member.template.is_some() {
            sources.push(CombatantSource::TemplateMember {
                member,
                uid: UnitId(0),
                party_statuses: statuses,
            });
        } else {
            sources.push(CombatantSource::ClassHero {
                member,
                uid: UnitId(0),
                party_statuses: statuses,
            });
        }
    }
    for enemy in &encounter.enemies {
        sources.push(CombatantSource::Enemy {
            def: enemy,
            uid: UnitId(0),
        });
    }

    // Assign UIDs in order (mirrors ECS spawn order).
    for src in &mut sources {
        let uid = assign_uid(src);
        match src {
            CombatantSource::ClassHero { uid: u, .. }
            | CombatantSource::TemplateMember { uid: u, .. }
            | CombatantSource::Enemy { uid: u, .. } => *u = uid,
        }
    }

    // Build engine units from sources.
    let mut units = Vec::with_capacity(sources.len());
    let mut metas: Vec<ProjectionMeta> = Vec::with_capacity(sources.len());
    for src in &sources {
        let (unit, meta) = build_combatant(src, &active_content, &keep_alive_names, encounter);
        units.push(unit);
        metas.push(meta);
    }

    // Construct initial CombatState (mirrors from_ecs — round=1).
    let mut state = CombatState::new(units, 1, RoundPhase::ActorTurn, 0);

    // apply_initial_statuses from unit templates (mirrors bootstrap_combat_state).
    {
        let content_view = build_ecs_content_view(&active_content);
        state.apply_initial_statuses(&content_view);
    }

    // Static obstacle hexes + environment objects (mirrors bootstrap_combat_state).
    state.blocked_hexes = encounter.obstacles.iter().copied().collect();
    state.environment = encounter
        .environment
        .iter()
        .enumerate()
        .map(|(idx, def)| EnvObject {
            id: EnvId(idx as u32),
            hex: def.hex,
            kind: EnvKind::Hazard,
            ability: def.ability.clone(),
            owner: def.owner,
            revealed_to: TeamSet::EMPTY,
        })
        .collect();

    // Roll initiative + reconcile turn order (mirrors bootstrap_combat_state).
    // Build name → UnitId map for preset lookup.
    let name_to_uid: HashMap<String, UnitId> = metas
        .iter()
        .map(|m| (m.display_name.clone(), m.uid))
        .collect();
    let preset_map: HashMap<UnitId, i32> = preset
        .iter()
        .filter_map(|(name, &val)| name_to_uid.get(name.as_str()).map(|&uid| (uid, val)))
        .collect();

    let _roll_events = state.roll_initiative_for_all(rng, &preset_map);

    state.reconcile_turn_order();
    state.turn_queue.index = 0;

    // settle_round_start (mirrors bootstrap_combat_state).
    if !state.turn_queue.order.is_empty() {
        let content_view = build_ecs_content_view(&active_content);
        let _events = state.settle_round_start(&content_view);
    }

    (state, metas)
}

// ── Unit builders ─────────────────────────────────────────────────────────────

fn build_combatant<'a>(
    src: &CombatantSource<'a>,
    active_content: &ActiveContent,
    keep_alive_names: &HashSet<&str>,
    encounter: &EncounterDef,
) -> (combat_engine::state::Unit, ProjectionMeta) {
    match src {
        CombatantSource::ClassHero {
            member,
            uid,
            party_statuses,
        } => build_class_hero(
            member,
            *uid,
            party_statuses,
            active_content,
            keep_alive_names,
            encounter,
        ),
        CombatantSource::TemplateMember {
            member,
            uid,
            party_statuses,
        } => build_template_member(
            member,
            *uid,
            party_statuses,
            active_content,
            keep_alive_names,
            encounter,
        ),
        CombatantSource::Enemy { def, uid } => {
            build_enemy(def, *uid, active_content, keep_alive_names, encounter)
        }
    }
}

/// Build a class-based hero (mirrors the class-based branch in `spawn_combatants`).
fn build_class_hero(
    member: &PartyMemberDef,
    uid: UnitId,
    party_statuses: &[String],
    active_content: &ActiveContent,
    keep_alive_names: &HashSet<&str>,
    encounter: &EncounterDef,
) -> (combat_engine::state::Unit, ProjectionMeta) {
    use crate::game::components::Equipment;

    let cls = active_content
        .classes
        .get(&member.class_id)
        .unwrap_or_else(|| panic!("Class '{}' not found", member.class_id));

    let equipment = Equipment {
        main_hand: Some(cls.main_hand.clone()),
        off_hand: cls.off_hand.clone(),
        chest: cls.chest.clone(),
        legs: cls.legs.clone(),
        feet: cls.feet.clone(),
    };

    let effective = active_content.effective_stats(&cls.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);
    let mana_bonus = active_content.equipment_mana_bonus(&equipment);

    // Persistent statuses (PERMANENT_DURATION; unit is own applier — mirrors spawn_combatants).
    let statuses_vec: Vec<ActiveStatus> = party_statuses
        .iter()
        .map(|sid| ActiveStatus {
            id: combat_engine::StatusId::from(sid.as_str()),
            rounds_remaining: PERMANENT_DURATION,
            dot_per_tick: 0,
            applier: EffectSource::Unit(uid),
        })
        .collect();

    let bridge_pools = enum_map! {
        PoolKind::Hp     => Some((effective.max_hp, effective.max_hp)),
        PoolKind::Mana   => (cls.mana_max > 0).then_some((cls.mana_max + mana_bonus, cls.mana_max + mana_bonus)),
        PoolKind::Rage   => (cls.rage_max > 0).then_some((0, cls.rage_max)),
        PoolKind::Energy => (cls.energy_max > 0).then_some((cls.energy_max, cls.energy_max)),
        PoolKind::Ap     => Some((1, 1)),
        // MP starts at max (= base speed), same as CombatantBundle.
        PoolKind::Mp     => Some((cls.speed, cls.speed)),
    };
    let bridge_regen = standard_regen();

    let caster_ctx = make_caster_ctx(
        &cls.stats,
        Some(&equipment),
        active_content,
        member.path.as_deref(),
    );
    let aoo_dice = make_aoo_dice(&cls.abilities, &caster_ctx, &cls.stats, active_content);
    let passives = collect_passives(&cls.abilities, active_content);

    let input = UnitBuildInput {
        uid,
        team: Team::Player,
        pos: member.hex_pos,
        armor,
        base_speed: cls.speed,
        reactions_max: 1, // Reactions::default().max = 1
        statuses: statuses_vec,
        pools: bridge_pools,
        regen_per_pool: bridge_regen,
        template_id: None, // class-based heroes carry no template_id
    };

    let mut unit = build_unit(input, active_content);
    unit.caster_context = caster_ctx;
    unit.aoo_dice = aoo_dice;
    unit.passives = passives;

    let keep_alive_color = keep_alive_names
        .contains(member.name.as_str())
        .then(|| keep_alive_marker_color(&encounter.victory, &member.name));

    let meta = ProjectionMeta {
        uid,
        team: Team::Player,
        display_name: member.name.clone(),
        template_id: None,
        hex_pos: member.hex_pos,
        abilities: cls.abilities.clone(),
        path: member.path.clone(),
        mana_max: cls.mana_max,
        rage_max: cls.rage_max,
        energy_max: cls.energy_max,
        max_hp: effective.max_hp,
        initial_hp: effective.max_hp,
        armor,
        speed: cls.speed,
        max_ap: 1,
        reactions_max: 1,
        victory_target: None,
        keep_alive_target: keep_alive_color,
    };

    (unit, meta)
}

/// Build a template-based party member (mirrors the template branch in `spawn_combatants`).
fn build_template_member(
    member: &PartyMemberDef,
    uid: UnitId,
    party_statuses: &[String],
    active_content: &ActiveContent,
    keep_alive_names: &HashSet<&str>,
    encounter: &EncounterDef,
) -> (combat_engine::state::Unit, ProjectionMeta) {
    use crate::game::components::Equipment;

    let template_id = member
        .template
        .as_ref()
        .expect("template-based member must have template");
    let tpl = active_content
        .unit_templates
        .get(template_id)
        .unwrap_or_else(|| {
            panic!(
                "Template '{}' not found for party member '{}'",
                template_id, member.name
            )
        });

    let equipment = Equipment {
        main_hand: Some(tpl.equipment.main_hand.clone()),
        off_hand: tpl.equipment.off_hand.clone(),
        chest: tpl.equipment.chest.clone(),
        legs: tpl.equipment.legs.clone(),
        feet: tpl.equipment.feet.clone(),
    };

    let effective = active_content.effective_stats(&tpl.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);
    let mana_bonus = active_content.equipment_mana_bonus(&equipment);

    // initial_pools.hp override — mirrors spawn_combatants template branch.
    let initial_hp = tpl
        .initial_pools
        .get("hp")
        .copied()
        .unwrap_or(effective.max_hp)
        .clamp(1, effective.max_hp);

    // Persistent statuses (PERMANENT_DURATION; unit is own applier).
    let statuses_vec: Vec<ActiveStatus> = party_statuses
        .iter()
        .map(|sid| ActiveStatus {
            id: combat_engine::StatusId::from(sid.as_str()),
            rounds_remaining: PERMANENT_DURATION,
            dot_per_tick: 0,
            applier: EffectSource::Unit(uid),
        })
        .collect();
    // Note: template initial_statuses are applied engine-side by
    // apply_initial_statuses — not added here (matches spawn_combatants comment).

    let bridge_pools = enum_map! {
        PoolKind::Hp     => Some((initial_hp, effective.max_hp)),
        PoolKind::Mana   => (tpl.resources.mana_max > 0).then_some((tpl.resources.mana_max + mana_bonus, tpl.resources.mana_max + mana_bonus)),
        PoolKind::Rage   => (tpl.resources.rage_max > 0).then_some((0, tpl.resources.rage_max)),
        PoolKind::Energy => (tpl.resources.energy_max > 0).then_some((tpl.resources.energy_max, tpl.resources.energy_max)),
        PoolKind::Ap     => Some((1, 1)),
        PoolKind::Mp     => Some((tpl.speed, tpl.speed)),
    };
    let bridge_regen = standard_regen();

    // Template-based party members do NOT receive a `CombatPath` component in
    // `spawn_combatants` (only class heroes and enemies do), so the ECS
    // `bootstrap_combat_state` reads `crit_fail_outcome = Miss` for them. Pass
    // `None` here to mirror that — sourcing from `tpl.path` would diverge.
    let caster_ctx = make_caster_ctx(&tpl.stats, Some(&equipment), active_content, None);
    let aoo_dice = make_aoo_dice(&tpl.ability_ids, &caster_ctx, &tpl.stats, active_content);
    let passives = collect_passives(&tpl.ability_ids, active_content);

    let input = UnitBuildInput {
        uid,
        team: Team::Player,
        pos: member.hex_pos,
        armor,
        base_speed: tpl.speed,
        reactions_max: 1,
        statuses: statuses_vec,
        pools: bridge_pools,
        regen_per_pool: bridge_regen,
        template_id: Some(template_id.clone()),
    };

    let mut unit = build_unit(input, active_content);
    unit.caster_context = caster_ctx;
    unit.aoo_dice = aoo_dice;
    unit.passives = passives;

    let keep_alive_color = keep_alive_names
        .contains(member.name.as_str())
        .then(|| keep_alive_marker_color(&encounter.victory, &member.name));

    let meta = ProjectionMeta {
        uid,
        team: Team::Player,
        display_name: member.name.clone(),
        template_id: Some(template_id.clone()),
        hex_pos: member.hex_pos,
        abilities: tpl.ability_ids.clone(),
        path: tpl.path.clone(),
        mana_max: tpl.resources.mana_max,
        rage_max: tpl.resources.rage_max,
        energy_max: tpl.resources.energy_max,
        max_hp: effective.max_hp,
        initial_hp,
        armor,
        speed: tpl.speed,
        max_ap: 1,
        reactions_max: 1,
        victory_target: None,
        keep_alive_target: keep_alive_color,
    };

    (unit, meta)
}

/// Build an enemy unit (mirrors the enemy loop in `spawn_combatants`).
fn build_enemy(
    def: &EnemyDef,
    uid: UnitId,
    active_content: &ActiveContent,
    keep_alive_names: &HashSet<&str>,
    encounter: &EncounterDef,
) -> (combat_engine::state::Unit, ProjectionMeta) {
    use crate::game::components::Equipment;

    let equipment = Equipment {
        main_hand: Some(def.main_hand.clone()),
        off_hand: def.off_hand.clone(),
        chest: def.chest.clone(),
        legs: def.legs.clone(),
        feet: def.feet.clone(),
    };

    let effective = active_content.effective_stats(&def.stats, &equipment);
    let armor = active_content.equipment_armor(&equipment);

    let bridge_pools = enum_map! {
        PoolKind::Hp     => Some((effective.max_hp, effective.max_hp)),
        PoolKind::Mana   => (def.mana_max > 0).then_some((def.mana_max, def.mana_max)),
        PoolKind::Rage   => (def.rage_max > 0).then_some((0, def.rage_max)),
        PoolKind::Energy => (def.energy_max > 0).then_some((def.energy_max, def.energy_max)),
        PoolKind::Ap     => Some((1, 1)),
        PoolKind::Mp     => Some((def.speed, def.speed)),
    };
    let bridge_regen = standard_regen();

    let caster_ctx = make_caster_ctx(
        &def.stats,
        Some(&equipment),
        active_content,
        def.path.as_deref(),
    );
    let aoo_dice = make_aoo_dice(&def.ability_ids, &caster_ctx, &def.stats, active_content);
    let passives = collect_passives(&def.ability_ids, active_content);

    // Auras from EnemyDef.aura (mirrors the aura_q loop in bootstrap_combat_state).
    let auras: Vec<AuraDef> = def
        .aura
        .as_ref()
        .map(|aura_src| {
            let applies_to = match aura_src.affects {
                AuraAffects::Enemies => TeamRelation::Enemies,
                AuraAffects::Allies => TeamRelation::Allies,
                AuraAffects::All => TeamRelation::All,
            };
            AuraDef {
                radius: aura_src.radius,
                status_id: aura_src.status.clone(),
                applies_to,
                affects_tags: aura_src.affects_tags.clone(),
            }
        })
        .into_iter()
        .collect();

    // Enemy phases from EnemyDef.phases (mirrors phases_q loop in bootstrap_combat_state).
    let enemy_phases: Vec<PhaseEntry> = def
        .phases
        .iter()
        .map(|phase| {
            let crate::content::encounters::PhaseTrigger::HpBelowPct(pct) = phase.trigger;
            let new_max_hp = phase.stats.as_ref().map(|s| s.max_hp).unwrap_or(0);
            PhaseEntry {
                pct,
                new_max_hp,
                heal_to_full: phase.heal_to_full,
                tags: phase.tags.clone(),
            }
        })
        .collect();

    // Tags from EnemyDef.tags (mirrors tags_q loop in bootstrap_combat_state).
    let tags = def.tags.clone();

    let input = UnitBuildInput {
        uid,
        team: Team::Enemy,
        pos: def.hex_pos,
        armor,
        base_speed: def.speed,
        reactions_max: 1,
        statuses: Vec::new(), // enemies carry no persistent statuses at spawn
        pools: bridge_pools,
        regen_per_pool: bridge_regen,
        template_id: None,
        // EnemyDef is already fully-resolved from a template by the content loader,
        // but the resulting engine Unit carries no template_id (there is no summon
        // path for initial combatants, so apply_initial_statuses won't look them up).
    };

    let mut unit = build_unit(input, active_content);
    unit.caster_context = caster_ctx;
    unit.aoo_dice = aoo_dice;
    unit.auras = auras;
    unit.enemy_phases = enemy_phases;
    unit.tags = tags;
    unit.passives = passives;

    // VictoryTarget color.
    let victory_target = match &encounter.victory {
        VictoryCondition::KillTarget {
            enemy_name,
            marker_color,
            ..
        } if enemy_name == &def.name => Some(*marker_color),
        _ => None,
    };

    // Display name mirrors spawn_combatants: "<race_name> <enemy.name>".
    let race_name = active_content
        .races
        .get(&def.race)
        .map_or("", |r| r.name.as_str());
    let display_name = format!("{} {}", race_name, &def.name);

    let keep_alive_color = (keep_alive_names.contains(def.name.as_str())
        || keep_alive_names.contains(display_name.as_str()))
    .then(|| keep_alive_marker_color(&encounter.victory, &def.name));

    let meta = ProjectionMeta {
        uid,
        team: Team::Enemy,
        display_name,
        template_id: None,
        hex_pos: def.hex_pos,
        abilities: def.ability_ids.clone(),
        path: def.path.clone(),
        mana_max: def.mana_max,
        rage_max: def.rage_max,
        energy_max: def.energy_max,
        max_hp: effective.max_hp,
        initial_hp: effective.max_hp,
        armor,
        speed: def.speed,
        max_ap: 1,
        reactions_max: 1,
        victory_target,
        keep_alive_target: keep_alive_color,
    };

    (unit, meta)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Standard pool regen rules — mirrors `from_ecs`.
fn standard_regen() -> enum_map::EnumMap<PoolKind, RegenRule> {
    enum_map! {
        PoolKind::Hp     => RegenRule::None,
        PoolKind::Mana   => RegenRule::Increment(1),
        PoolKind::Rage   => RegenRule::None,
        PoolKind::Energy => RegenRule::Increment(1),
        PoolKind::Ap     => RegenRule::RefillToMax,
        PoolKind::Mp     => RegenRule::RefillToMax,
    }
}

/// Build a `combat_engine::CasterContext` from content data.
///
/// Mirrors the `caster_context` loop in `bootstrap_combat_state`.
fn make_caster_ctx(
    stats: &crate::game::components::CombatStats,
    equipment: Option<&crate::game::components::Equipment>,
    active_content: &ActiveContent,
    path: Option<&str>,
) -> CasterContext {
    let bevy_ctx = BevyCasterContext::new(stats, equipment, &active_content.weapons);
    let crit_fail_effect = path
        .and_then(|p| active_content.paths.get(p))
        .map_or(CritFailEffect::Miss, |p| p.crit_fail_effect.clone());
    CasterContext {
        str_mod: bevy_ctx.str_mod,
        int_mod: bevy_ctx.int_mod,
        spell_power: bevy_ctx.spell_power,
        weapon_dice: bevy_ctx.weapon_dice,
        crit_fail_outcome: crate::content::to_engine::crit_fail_outcome(&crit_fail_effect),
        dex_mod: modifier(stats.dexterity),
    }
}

/// Build AoO dice for a unit — mirrors the `aoo_q` loop in `bootstrap_combat_state`.
fn make_aoo_dice(
    ability_ids: &[AbilityId],
    caster_ctx: &CasterContext,
    stats: &crate::game::components::CombatStats,
    active_content: &ActiveContent,
) -> Option<EngineDiceExpr> {
    let has_melee = ability_ids.iter().any(|aid| {
        active_content
            .abilities
            .get(aid)
            .is_some_and(|def| matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1)
    });
    if !has_melee {
        return None;
    }
    let core_dice = caster_ctx.weapon_dice?;
    Some(EngineDiceExpr::new(
        core_dice.count,
        core_dice.sides,
        core_dice.bonus + combat_engine::modifier(stats.strength),
    ))
}

/// Collect passive ability ids — mirrors the `passives` loop in `bootstrap_combat_state`.
fn collect_passives(ability_ids: &[AbilityId], active_content: &ActiveContent) -> Vec<AbilityId> {
    ability_ids
        .iter()
        .filter(|aid| {
            active_content
                .abilities
                .get(*aid)
                .is_some_and(|def| !def.passive.is_empty())
        })
        .cloned()
        .collect()
}
