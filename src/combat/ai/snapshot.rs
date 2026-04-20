use crate::content::content_view::ContentView;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::scoring::{applies_cc, estimate_st_damage};
use crate::content::abilities::{AbilityDef, AoEShape, CasterContext, EffectDef, TargetType};
use crate::core::{AbilityId, ResourceKind, StatusId};
use crate::game::components::{
    AiCombatantQ, Combatant, StatusEffects, Team,
};
use crate::game::hex::Hex;
use crate::game::resources::HexPositions;
use bevy::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;

// ── Tags ──────────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AiTags: u16 {
        const LOW_HP           = 0b0000_0001;
        const CAN_HEAL         = 0b0000_0010;
        const CAN_CC           = 0b0000_0100;
        const HAS_AOE          = 0b0000_1000;
        const IS_STUNNED       = 0b0001_0000;
        const FORCES_TARGETING = 0b0010_0000;
        const RANGED           = 0b0100_0000;
        const MELEE_ONLY       = 0b1000_0000;
    }
}

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BattleSnapshot {
    pub units: Vec<UnitSnapshot>,
    pub round: u32,
    /// O(1) entity → units[index] cache. Private so the invariant "in sync
    /// with `units`" can't be silently broken via struct-literal
    /// construction; callers go through [`BattleSnapshot::new`] or the
    /// serde path (which gives a `None` cache, lazy-built on first
    /// `unit()` call). Sim calls [`BattleSnapshot::invalidate_index`] after
    /// `units.retain` / other shape-changing mutations so the next read
    /// rebuilds. Read through `unit()` — never poke this field directly.
    #[serde(skip)]
    by_entity: RefCell<Option<HashMap<Entity, usize>>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UnitSnapshot {
    #[serde(with = "crate::combat::ai::serde_helpers::entity")]
    pub entity: Entity,
    pub team: Team,
    pub role: AxisProfile,
    #[serde(with = "crate::combat::ai::serde_helpers::hex")]
    pub pos: Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    /// Remaining AP for this turn.
    pub action_points: i32,
    /// Full AP pool (for partial-spend reasoning).
    pub max_ap: i32,
    /// Movement budget remaining right now. Zero means the unit can't walk.
    pub movement_points: i32,
    /// Base speed + status speed_bonus. Used for pathfinding range estimates
    /// and utility scoring; not the live budget (see `movement_points`).
    pub speed: i32,
    pub mana: Option<(i32, i32)>,
    pub rage: Option<(i32, i32)>,
    pub energy: Option<(i32, i32)>,
    pub abilities: Vec<AbilityId>,
    pub threat: f32,
    #[serde(with = "crate::combat::ai::serde_helpers::ai_tags")]
    pub tags: AiTags,
    /// Max range of any offensive (SingleEnemy) ability. Used for reach checks
    /// in intent selection (e.g., "is this enemy killable this turn?").
    pub max_attack_range: u32,
    /// Entity of the summoner, if this unit was summoned.
    #[serde(with = "crate::combat::ai::serde_helpers::entity_opt")]
    pub summoner: Option<Entity>,
    /// Remaining opportunity reactions this round. Zero means no AoO this turn.
    /// (Schema v2+: default `1` on v1 logs — every current unit has `max=1`.)
    #[serde(default = "default_reactions_left")]
    pub reactions_left: i32,
    /// Pre-armor expected damage this unit would inflict via an AoO
    /// (dice.expected + str_mod). `None` if the unit cannot make an opportunity
    /// attack (no melee weapon_attack ability, or no equipped weapon).
    /// Schema v2+: absent on v1 logs → `None`.
    #[serde(default)]
    pub aoo_expected_damage: Option<f32>,
    /// Active status effects on this unit — mirrors the `StatusEffects`
    /// component (minus the `applier` Entity, which isn't needed for AI
    /// reasoning). Sim mutates this list on per-step status applications so
    /// that downstream steps see status-derived bonuses / DoT cleanse / stun.
    /// Schema v3+: absent on older logs → empty vec.
    #[serde(default)]
    pub statuses: Vec<ActiveStatusView>,
}

/// Snapshot-shaped mirror of `ActiveStatus` (components.rs). Drops `applier`
/// since the AI layer never needs to know who put the status on — only the
/// status id, duration, and per-tick DoT damage are consulted.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ActiveStatusView {
    pub id: StatusId,
    pub rounds_remaining: u32,
    pub dot_per_tick: i32,
}

fn default_reactions_left() -> i32 { 1 }

/// Re-aggregate `armor_bonus` / `damage_taken_bonus` on `unit` from its
/// current `statuses` list. Call after sim mutates the status list
/// (apply/cleanse/removal) so downstream damage math sees fresh aggregates
/// instead of the stale snapshot-time value.
///
/// **Not** recomputed: `speed` — base speed isn't tracked separately on the
/// snapshot (only `base + aggregate` is stored), so deriving the new speed
/// mid-plan would require knowing what the aggregate was at snapshot time.
/// Speed-affecting statuses applied mid-plan therefore don't re-flow into
/// the planner's pathing; accept that limitation for now.
pub fn refresh_status_aggregates(unit: &mut UnitSnapshot, content: &ContentView) {
    let (armor_bonus, damage_taken_bonus) = unit
        .statuses
        .iter()
        .filter_map(|s| content.statuses.get(&s.id))
        .fold((0, 0), |(a, v), sd| (a + sd.armor_bonus, v + sd.damage_taken_bonus));
    unit.armor_bonus = armor_bonus;
    unit.damage_taken_bonus = damage_taken_bonus;
}

impl UnitSnapshot {
    /// `hp > 0`. Snapshot keeps dead units (for death-triggered effects,
    /// resurrection, and honest replay logs); accessors like `enemies_of`
    /// filter them out by default. Use this directly when a call site needs
    /// both "alive?" and "pos occupied?" (the classic case — movement
    /// stop-blockers — counts corpses even though they're not enemies).
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Effective HP: raw HP plus base and status armor — the real damage
    /// budget needed to drop this unit.
    pub fn eff_hp(&self) -> i32 {
        self.hp + self.armor + self.armor_bonus
    }

    /// Effective max HP, clamped ≥ 1 to protect against division.
    pub fn eff_max_hp(&self) -> i32 {
        (self.max_hp + self.armor + self.armor_bonus).max(1)
    }

    /// Current HP as a fraction of max HP, clamped ≥ 1 to avoid div-by-zero.
    /// Use for threshold checks like "below 30% HP triggers LOW_HP".
    pub fn hp_pct(&self) -> f32 {
        self.hp as f32 / self.max_hp.max(1) as f32
    }

    /// Killability signal: `1 − eff_hp / eff_max_hp`. A 1.0 unit is dead,
    /// 0.0 is at full effective HP. Used by `target_priority` (scoring the
    /// focus factor) and by the planner's target enumeration (picking the
    /// top-K most-finishable enemies for Cast candidates).
    pub fn killability(&self) -> f32 {
        let eff_max = self.eff_max_hp() as f32;
        if eff_max <= 0.0 {
            return 0.0;
        }
        1.0 - (self.eff_hp() as f32 / eff_max)
    }

    /// Current amount in the spendable pool for `kind`. `Option` resources
    /// (mana/rage/energy) yield 0 when absent.
    pub fn resource_amount(&self, kind: ResourceKind) -> i32 {
        pool_amount(
            kind,
            self.hp,
            self.mana.map(|(c, _)| c).unwrap_or(0),
            self.rage.map(|(c, _)| c).unwrap_or(0),
            self.energy.map(|(c, _)| c).unwrap_or(0),
        )
    }

    /// True iff the unit has enough AP and every resource cost to cast `def`.
    pub fn can_afford(&self, def: &AbilityDef) -> bool {
        self.action_points >= def.cost_ap
            && def
                .costs
                .iter()
                .all(|c| self.resource_amount(c.resource) >= c.amount)
    }
}

/// Low-level resource-pool lookup. The one place that knows the
/// `ResourceKind` match arms; everybody else — `UnitSnapshot` methods,
/// `compute_tags` during snapshot construction, scarcity scoring — funnels
/// through this so the four-arm match doesn't replicate across the crate.
pub(crate) fn pool_amount(
    kind: ResourceKind,
    hp: i32,
    mana: i32,
    rage: i32,
    energy: i32,
) -> i32 {
    match kind {
        ResourceKind::Hp => hp,
        ResourceKind::Mana => mana,
        ResourceKind::Rage => rage,
        ResourceKind::Energy => energy,
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub fn build_snapshot(
    round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    positions: &HexPositions,
    roles: &Query<&AxisProfile>,
    content: &ContentView,
) -> BattleSnapshot {
    // Dead combatants stay in the snapshot (hp=0 marker). Downstream
    // accessors like `enemies_of` / `allies_of` filter them out; death-
    // aware code (resurrection, on-kill triggers, replay) reads them via
    // `all_enemies_of` / `dead_units`.
    let units = combatants
        .iter()
        .filter_map(|c| {
            let pos = positions.get(&c.entity)?;
            let role = roles.get(c.entity).copied().unwrap_or_default();
            let caster_ctx = CasterContext::new(c.stats, Some(c.equipment), &content.weapons);
            let threat = estimate_st_damage(&caster_ctx, c.abilities, content);

            let tags = compute_tags(&c, statuses_q, content);

            // Single pass over status effects — aggregates every per-snapshot
            // bonus at once (speed, armor, damage-taken). Keep this fold as
            // the only place each bonus is read from statuses.
            let StatusBonuses { speed_bonus, armor_bonus, damage_taken_bonus } =
                status_bonuses(c.entity, statuses_q, content);

            let max_attack_range: u32 = c
                .abilities
                .0
                .iter()
                .filter_map(|id| content.abilities.get(id))
                .filter(|def| matches!(def.target_type, TargetType::SingleEnemy))
                .map(|def| def.range.max)
                .max()
                .unwrap_or(0);

            // AoO provoker data: has melee weapon_attack + equipped weapon.
            // Mirrors `movement.rs` provoker selection.
            let has_melee_weapon_attack = c.abilities.0.iter().any(|id| {
                content.abilities.get(id).is_some_and(|def| {
                    matches!(def.effect, EffectDef::WeaponAttack) && def.range.max == 1
                })
            });
            let aoo_expected_damage =
                if has_melee_weapon_attack {
                    caster_ctx
                        .weapon_dice
                        .as_ref()
                        .map(|d| d.expected() + caster_ctx.str_mod as f32)
                } else {
                    None
                };
            let reactions_left = c.reactions.map(|r| r.remaining as i32).unwrap_or(0);

            let statuses = statuses_q
                .get(c.entity)
                .map(|se| {
                    se.0.iter()
                        .map(|s| ActiveStatusView {
                            id: s.id.clone(),
                            rounds_remaining: s.rounds_remaining,
                            dot_per_tick: s.dot_per_tick,
                        })
                        .collect()
                })
                .unwrap_or_default();

            Some(UnitSnapshot {
                entity: c.entity,
                team: c.faction.0,
                role,
                pos,
                hp: c.vital.hp,
                max_hp: c.vital.max_hp,
                armor: c.vital.armor,
                armor_bonus,
                damage_taken_bonus,
                action_points: c.ap.action_points,
                max_ap: c.ap.max_ap,
                movement_points: c.ap.movement_points,
                speed: c.speed.0 + speed_bonus,
                mana: c.mana.map(|m| (m.current, m.max)),
                rage: c.rage.map(|r| (r.current, r.max)),
                energy: c.energy.map(|e| (e.current, e.max)),
                abilities: c.abilities.0.clone(),
                threat,
                tags,
                max_attack_range,
                summoner: c.summoned_by.map(|s| s.0),
                reactions_left,
                aoo_expected_damage,
                statuses,
            })
        })
        .collect();

    BattleSnapshot::new(units, round)
}

// ── Helpers on BattleSnapshot ─────────────────────────────────────────────────

impl BattleSnapshot {
    /// Construct a snapshot with its entity index eagerly built. The index
    /// backs `unit()` — O(1) instead of the linear scan the old literal
    /// construction produced. Use this everywhere (prod + tests);
    /// `#[derive(Default)]` / serde-deserialized snapshots get an empty
    /// cache that lazy-builds on first `unit()` call.
    pub fn new(units: Vec<UnitSnapshot>, round: u32) -> Self {
        let map = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.entity, i))
            .collect();
        Self {
            units,
            round,
            by_entity: RefCell::new(Some(map)),
        }
    }

    /// Discard the entity index. Call after any mutation of `units` that
    /// changes length or order (e.g. sim's `retain` for killed units). The
    /// next `unit()` call rebuilds lazily.
    pub fn invalidate_index(&mut self) {
        *self.by_entity.borrow_mut() = None;
    }

    /// O(1) lookup by entity. Transparently lazy-builds the index when
    /// missing (fresh deserialized snapshot or post-`invalidate_index`).
    pub fn unit(&self, entity: Entity) -> Option<&UnitSnapshot> {
        // Scope the RefCell borrow so the resulting `&UnitSnapshot` doesn't
        // alias it.
        let idx = {
            let mut slot = self.by_entity.borrow_mut();
            if slot.is_none() {
                *slot = Some(
                    self.units
                        .iter()
                        .enumerate()
                        .map(|(i, u)| (u.entity, i))
                        .collect(),
                );
            }
            slot.as_ref().expect("just filled").get(&entity).copied()?
        };
        self.units.get(idx)
    }

    /// Position lookup stays linear — there's no per-tile index, and
    /// callers use it sparingly (mostly in sim's `compute_affected_targets`).
    pub fn unit_at(&self, pos: Hex) -> Option<&UnitSnapshot> {
        self.units.iter().find(|u| u.pos == pos)
    }

    /// Live enemies of `team`. Dead units on the opposing team stay in
    /// `units` (kept for resurrection / death triggers / replay fidelity)
    /// but are filtered here because every tactical caller wants the
    /// "who can I actually fight" view. For the raw list including
    /// corpses, use `all_enemies_of`.
    pub fn enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units
            .iter()
            .filter(move |u| u.team == opponent && u.is_alive())
    }

    /// Live allies of `team` (mirrors `enemies_of` contract).
    pub fn allies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        self.units
            .iter()
            .filter(move |u| u.team == team && u.is_alive())
    }

    /// Enemies of `team` **including corpses**. Used by death-aware code
    /// (resurrection targeting, on-kill effect resolution, replay) that
    /// needs to see the entity even after it died.
    pub fn all_enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units.iter().filter(move |u| u.team == opponent)
    }

    /// Dead opposing-team units only. Empty on live-only snapshots; the
    /// resurrection/death-trigger call sites poll this.
    pub fn dead_enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = opponent_team(team);
        self.units
            .iter()
            .filter(move |u| u.team == opponent && !u.is_alive())
    }

    /// Every dead unit in the snapshot regardless of team.
    pub fn dead_units(&self) -> impl Iterator<Item = &UnitSnapshot> {
        self.units.iter().filter(|u| !u.is_alive())
    }
}

fn opponent_team(team: Team) -> Team {
    match team {
        Team::Player => Team::Enemy,
        Team::Enemy => Team::Player,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

use crate::game::components::AiCombatantQItem;

fn compute_tags(
    c: &AiCombatantQItem,
    statuses_q: &Query<&StatusEffects>,
    content: &ContentView,
) -> AiTags {
    let mut tags = AiTags::empty();

    // LOW_HP: below 30%
    let hp_pct = c.vital.hp as f32 / c.vital.max_hp.max(1) as f32;
    if hp_pct < 0.3 {
        tags |= AiTags::LOW_HP;
    }

    // Ability-derived tags
    let mut max_range: u32 = 0;
    let mut has_min_range_2 = false;

    let resources = (
        c.mana.map(|m| m.current).unwrap_or(0),
        c.rage.map(|r| r.current).unwrap_or(0),
        c.energy.map(|e| e.current).unwrap_or(0),
    );

    for id in &c.abilities.0 {
        let Some(def) = content.abilities.get(id) else { continue };
        if def.range.max > max_range {
            max_range = def.range.max;
        }
        if def.range.min >= 2 {
            has_min_range_2 = true;
        }

        let can_afford = def.costs.iter().all(|cost| {
            pool_amount(cost.resource, c.vital.hp, resources.0, resources.1, resources.2)
                >= cost.amount
        });

        if can_afford {
            if def.target_type == TargetType::SingleAlly
                && matches!(def.effect, EffectDef::Heal { .. })
            {
                tags |= AiTags::CAN_HEAL;
            }

            if applies_cc(def, content) {
                tags |= AiTags::CAN_CC;
            }

            if def.aoe != AoEShape::None {
                tags |= AiTags::HAS_AOE;
            }
        }
    }

    if max_range <= 1 {
        tags |= AiTags::MELEE_ONLY;
    }
    if has_min_range_2 {
        tags |= AiTags::RANGED;
    }

    // Status-derived tags
    if let Ok(se) = statuses_q.get(c.entity) {
        for s in &se.0 {
            let Some(sd) = content.statuses.get(&s.id) else { continue };
            if sd.skips_turn {
                tags |= AiTags::IS_STUNNED;
            }
            if sd.forces_targeting {
                tags |= AiTags::FORCES_TARGETING;
            }
        }
    }

    tags
}

#[derive(Default)]
struct StatusBonuses {
    speed_bonus: i32,
    armor_bonus: i32,
    damage_taken_bonus: i32,
}

/// Aggregate every status-derived bonus a snapshot needs in a single pass over
/// the unit's `StatusEffects`. Before this helper we iterated the status list
/// three times per unit (once per bonus field).
fn status_bonuses(
    entity: Entity,
    statuses_q: &Query<&StatusEffects>,
    content: &ContentView,
) -> StatusBonuses {
    let Ok(se) = statuses_q.get(entity) else {
        return StatusBonuses::default();
    };
    se.0.iter()
        .filter_map(|s| content.statuses.get(&s.id))
        .fold(StatusBonuses::default(), |mut acc, sd| {
            acc.speed_bonus += sd.speed_bonus;
            acc.armor_bonus += sd.armor_bonus;
            acc.damage_taken_bonus += sd.damage_taken_bonus;
            acc
        })
}

#[cfg(test)]
mod affordability_tests {
    use super::*;
    use crate::combat::ai::role::AiRole;
    use crate::content::abilities::{AbilityRange, AoEShape, EffectDef, ResourceCost};
    use crate::core::DiceExpr;
    use crate::game::hex::hex_from_offset;

    fn base_unit() -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw_u32(1).expect("valid"),
            team: Team::Enemy,
            role: AxisProfile::from(AiRole::Bruiser),
            pos: hex_from_offset(0, 0),
            hp: 20,
            max_hp: 20,
            armor: 0,
            armor_bonus: 0,
            damage_taken_bonus: 0,
            action_points: 2,
            max_ap: 2,
            movement_points: 3,
            speed: 3,
            mana: Some((5, 10)),
            rage: Some((3, 10)),
            energy: Some((4, 10)),
            abilities: Vec::new(),
            threat: 0.0,
            tags: AiTags::empty(),
            max_attack_range: 1,
            summoner: None,
            reactions_left: 1,
            aoo_expected_damage: None,
            statuses: Vec::new(),
        }
    }

    fn def(cost_ap: i32, costs: Vec<ResourceCost>) -> AbilityDef {
        AbilityDef {
            id: AbilityId::from("x"),
            name: "x".into(),
            target_type: TargetType::SingleEnemy,
            range: AbilityRange { min: 0, max: 1 },
            effect: EffectDef::Damage { dice: DiceExpr::new(1, 6, 0) },
            costs,
            cost_ap,
            aoe: AoEShape::None,
            friendly_fire: false,
            statuses: Vec::new(),
            magic_domains: Vec::new(),
            magic_method: String::new(),
            key: None,
        }
    }

    fn cost(kind: ResourceKind, amount: i32) -> ResourceCost {
        ResourceCost { resource: kind, amount }
    }

    #[test]
    fn can_afford_covers_ap_and_all_resource_kinds() {
        let u = base_unit();
        // (name, ap_cost, costs, expected can_afford)
        let cases: Vec<(&str, i32, Vec<ResourceCost>, bool)> = vec![
            ("free ability",        1, vec![],                              true),
            ("AP shortage",         3, vec![],                              false),
            ("mana ok",             1, vec![cost(ResourceKind::Mana, 5)],   true),
            ("mana short",          1, vec![cost(ResourceKind::Mana, 6)],   false),
            ("rage ok",             1, vec![cost(ResourceKind::Rage, 3)],   true),
            ("rage short",          1, vec![cost(ResourceKind::Rage, 4)],   false),
            ("energy ok",           1, vec![cost(ResourceKind::Energy, 4)], true),
            ("energy short",        1, vec![cost(ResourceKind::Energy, 5)], false),
            ("hp ok",               1, vec![cost(ResourceKind::Hp, 20)],    true),
            ("hp short",            1, vec![cost(ResourceKind::Hp, 21)],    false),
            ("two costs both ok",   1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 3)], true),
            ("two costs one short", 1, vec![cost(ResourceKind::Mana, 5), cost(ResourceKind::Rage, 4)], false),
        ];
        for (name, ap_cost, costs, want) in cases {
            let d = def(ap_cost, costs);
            assert_eq!(u.can_afford(&d), want, "{name}");
        }
    }

    #[test]
    fn resource_amount_treats_absent_option_pools_as_zero() {
        let mut u = base_unit();
        u.mana = None;
        u.rage = None;
        u.energy = None;
        assert_eq!(u.resource_amount(ResourceKind::Mana), 0);
        assert_eq!(u.resource_amount(ResourceKind::Rage), 0);
        assert_eq!(u.resource_amount(ResourceKind::Energy), 0);
        assert_eq!(u.resource_amount(ResourceKind::Hp), u.hp);
        // Any positive cost on an absent pool fails.
        let d = def(1, vec![cost(ResourceKind::Mana, 1)]);
        assert!(!u.can_afford(&d));
    }

    /// Dead units stay in `units` (hp=0 marker); the default-facing
    /// `enemies_of` / `allies_of` accessors hide them, while the explicit
    /// `all_enemies_of` / `dead_units` surface them for resurrection / on-kill /
    /// replay call sites. Pins the new contract.
    #[test]
    fn dead_units_stay_in_snapshot_and_are_filtered_by_default() {
        let alive = base_unit();
        let mut corpse = base_unit();
        corpse.entity = Entity::from_raw_u32(2).expect("valid");
        corpse.team = Team::Player;
        corpse.hp = 0;
        let snap = BattleSnapshot::new(vec![alive.clone(), corpse.clone()], 1);

        assert!(snap.unit(corpse.entity).is_some(), "corpse must stay in units");
        assert_eq!(
            snap.unit(corpse.entity).map(|u| u.is_alive()),
            Some(false),
            "corpse must report is_alive = false",
        );

        // Default accessors hide the dead.
        assert_eq!(snap.enemies_of(Team::Enemy).count(), 0, "default enemies_of hides dead");
        assert_eq!(snap.allies_of(Team::Enemy).count(), 1, "alive ally still visible");

        // Explicit "all" + "dead" variants surface them.
        assert_eq!(snap.all_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_enemies_of(Team::Enemy).count(), 1);
        assert_eq!(snap.dead_units().count(), 1);
    }
}
