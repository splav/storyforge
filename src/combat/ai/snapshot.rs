use crate::combat::ai::role::AiRole;
use crate::combat::ai::scoring::estimate_st_damage;
use crate::content::abilities::{AoEShape, CasterContext, EffectDef, TargetType};
use crate::core::AbilityId;
use crate::game::components::{
    AiCombatantQ, Combatant, StatusEffects, Team,
};
use crate::game::hex::Hex;
use crate::game::resources::{GameDb, HexPositions};
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

pub struct BattleSnapshot {
    pub units: Vec<UnitSnapshot>,
    pub active_unit: Entity,
    pub round: u32,
}

#[derive(Clone)]
pub struct UnitSnapshot {
    pub entity: Entity,
    pub team: Team,
    pub role: AiRole,
    pub pos: Hex,
    pub hp: i32,
    pub max_hp: i32,
    pub armor: i32,
    pub armor_bonus: i32,
    pub damage_taken_bonus: i32,
    pub action: bool,
    pub movement: bool,
    pub speed: i32,
    pub mana: Option<(i32, i32)>,
    pub rage: Option<(i32, i32)>,
    pub energy: Option<(i32, i32)>,
    pub abilities: Vec<AbilityId>,
    pub statuses: Vec<StatusSnap>,
    pub threat: f32,
    pub tags: AiTags,
}

#[derive(Debug, Clone)]
pub struct StatusSnap {
    pub id: crate::core::StatusId,
    pub rounds_remaining: u32,
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub fn build_snapshot(
    active: Entity,
    round: u32,
    combatants: &Query<AiCombatantQ, With<Combatant>>,
    statuses_q: &Query<&StatusEffects>,
    positions: &HexPositions,
    roles: &Query<&AiRole>,
    db: &GameDb,
) -> BattleSnapshot {
    let units = combatants
        .iter()
        .filter(|c| c.vital.is_alive())
        .filter_map(|c| {
            let pos = positions.get(&c.entity)?;
            let role = roles.get(c.entity).copied().unwrap_or(AiRole::Bruiser);
            let caster_ctx = CasterContext::new(c.stats, Some(c.equipment), &db.weapons);
            let threat = estimate_st_damage(&caster_ctx, c.abilities, db);

            let status_snaps: Vec<StatusSnap> = statuses_q
                .get(c.entity)
                .map(|se| {
                    se.0.iter()
                        .map(|s| StatusSnap {
                            id: s.id.clone(),
                            rounds_remaining: s.rounds_remaining,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let tags = compute_tags(c.entity, &c, &status_snaps, db);

            let speed = {
                let base = c.speed.0;
                let bonus: i32 = statuses_q
                    .get(c.entity)
                    .map(|se| {
                        se.0.iter()
                            .filter_map(|s| db.statuses.get(&s.id))
                            .map(|sd| sd.speed_bonus)
                            .sum()
                    })
                    .unwrap_or(0);
                base + bonus
            };

            Some(UnitSnapshot {
                entity: c.entity,
                team: c.faction.0,
                role,
                pos,
                hp: c.vital.hp,
                max_hp: c.vital.max_hp,
                armor: c.vital.armor,
                armor_bonus: status_armor_bonus(c.entity, statuses_q, db),
                damage_taken_bonus: status_dmg_taken_bonus(c.entity, statuses_q, db),
                action: c.ap.action,
                movement: c.ap.movement,
                speed,
                mana: c.mana.map(|m| (m.current, m.max)),
                rage: c.rage.map(|r| (r.current, r.max)),
                energy: c.energy.map(|e| (e.current, e.max)),
                abilities: c.abilities.0.clone(),
                statuses: status_snaps,
                threat,
                tags,
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
    entity: Entity,
    c: &AiCombatantQItem,
    statuses: &[StatusSnap],
    db: &GameDb,
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
        let Some(def) = db.abilities.get(id) else { continue };
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

            if def.statuses.iter().any(|sa| {
                db.statuses.get(&sa.status).is_some_and(|sd| sd.skips_turn)
            }) {
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
    let _ = entity; // used above via c
    for snap in statuses {
        if let Some(sd) = db.statuses.get(&snap.id) {
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

fn status_armor_bonus(
    entity: Entity,
    statuses_q: &Query<&StatusEffects>,
    db: &GameDb,
) -> i32 {
    statuses_q
        .get(entity)
        .map(|se| {
            se.0.iter()
                .filter_map(|s| db.statuses.get(&s.id))
                .map(|sd| sd.armor_bonus)
                .sum()
        })
        .unwrap_or(0)
}

fn status_dmg_taken_bonus(
    entity: Entity,
    statuses_q: &Query<&StatusEffects>,
    db: &GameDb,
) -> i32 {
    statuses_q
        .get(entity)
        .map(|se| {
            se.0.iter()
                .filter_map(|s| db.statuses.get(&s.id))
                .map(|sd| sd.damage_taken_bonus)
                .sum()
        })
        .unwrap_or(0)
}
