use crate::content::content_view::ContentView;
use crate::combat::ai::role::AxisProfile;
use crate::combat::ai::scoring::{applies_cc, estimate_st_damage};
use crate::content::abilities::{AoEShape, CasterContext, EffectDef, TargetType};
use crate::core::{AbilityId, StatusId};
use crate::game::components::{
    AiCombatantQ, Combatant, StatusEffects, Team,
};
use crate::game::hex::Hex;
use crate::game::resources::HexPositions;
use bevy::prelude::*;

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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BattleSnapshot {
    pub units: Vec<UnitSnapshot>,
    #[serde(with = "crate::combat::ai::serde_helpers::entity")]
    pub active_unit: Entity,
    pub round: u32,
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
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub fn build_snapshot(
    active: Entity,
    round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    positions: &HexPositions,
    roles: &Query<&AxisProfile>,
    content: &ContentView,
) -> BattleSnapshot {
    let units = combatants
        .iter()
        .filter(|c| c.vital.is_alive())
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

    BattleSnapshot {
        units,
        active_unit: active,
        round,
    }
}

// ── Helpers on BattleSnapshot ─────────────────────────────────────────────────

impl BattleSnapshot {
    pub fn active(&self) -> Option<&UnitSnapshot> {
        self.unit(self.active_unit)
    }

    pub fn unit(&self, entity: Entity) -> Option<&UnitSnapshot> {
        self.units.iter().find(|u| u.entity == entity)
    }

    pub fn unit_at(&self, pos: Hex) -> Option<&UnitSnapshot> {
        self.units.iter().find(|u| u.pos == pos)
    }

    pub fn enemies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        let opponent = match team {
            Team::Player => Team::Enemy,
            Team::Enemy => Team::Player,
        };
        self.units.iter().filter(move |u| u.team == opponent)
    }

    pub fn allies_of(&self, team: Team) -> impl Iterator<Item = &UnitSnapshot> {
        self.units.iter().filter(move |u| u.team == team)
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
            let pool = match cost.resource {
                crate::core::ResourceKind::Hp => c.vital.hp,
                crate::core::ResourceKind::Mana => resources.0,
                crate::core::ResourceKind::Rage => resources.1,
                crate::core::ResourceKind::Energy => resources.2,
            };
            pool >= cost.amount
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
