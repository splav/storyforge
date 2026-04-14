use crate::content::weapons::WeaponDef;
use crate::core::{modifier, AbilityId, DiceExpr, StatusId, WeaponId};
use crate::game::components::{CombatStats, Equipment};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    SingleEnemy,
    SingleAlly,
    Myself,
}

/// To whom a status is applied when the ability resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusOn {
    /// The ability's resolved target (enemy, ally, or self depending on target_type).
    Target,
    /// Always the actor who used the ability.
    MySelf,
}

#[derive(Debug, Clone)]
pub struct StatusApplication {
    pub status: StatusId,
    pub duration_rounds: u32,
    pub on: StatusOn,
}

#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id: AbilityId,
    pub name: String,
    pub target_type: TargetType,
    /// Max range in hex steps. 0 = ignored (for Myself target_type).
    pub range: u32,
    pub effect: EffectDef,
    pub rage_cost: i32,
    pub mana_cost: i32,
    /// Status effects applied when the ability resolves.
    pub statuses: Vec<StatusApplication>,
}

#[derive(Debug, Clone)]
pub enum EffectDef {
    /// No direct damage or heal — ability only applies statuses.
    None,
    WeaponAttack,
    Damage {
        dice: DiceExpr,
    },
    /// spell_power + intelligence + dice, bypasses armor
    SpellDamage {
        dice: DiceExpr,
    },
    /// spell_power + intelligence + dice, heals target (capped at max_hp)
    Heal {
        dice: DiceExpr,
    },
    /// Grants bonus movement to the actor. Does NOT end the turn.
    GrantMovement {
        distance: i32,
    },
}

// ── Unified effect computation ──────────────────────────────────────────────

/// Context about the caster needed to compute effect values.
pub struct CasterContext {
    pub str_mod: i32,
    pub int_mod: i32,
    pub spell_power: i32,
    pub weapon_dice: Option<DiceExpr>,
}

impl CasterContext {
    pub fn new(
        stats: &CombatStats,
        equip: Option<&Equipment>,
        weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    ) -> Self {
        let weapon_def = equip
            .and_then(|e| e.main_hand.as_ref())
            .and_then(|w| weapons.get(w));
        Self {
            str_mod: modifier(stats.strength),
            int_mod: modifier(stats.intelligence),
            spell_power: weapon_def.map_or(0, |wd| wd.spell_power),
            weapon_dice: weapon_def.map(|wd| wd.dice.clone()),
        }
    }
}

/// Computed parameters for an ability effect.
pub struct EffectCalc {
    pub dice: Option<DiceExpr>,
    pub bonus: i32,
    pub pierces_armor: bool,
    pub is_heal: bool,
}

impl EffectCalc {
    pub fn expected(&self) -> f32 {
        self.dice.as_ref().map_or(0.0, |d| d.expected()) + self.bonus as f32
    }
}

impl EffectDef {
    /// Compute effect parameters from caster context.
    /// Returns None for effects without damage/heal (None, GrantMovement).
    pub fn calc(&self, ctx: &CasterContext) -> Option<EffectCalc> {
        match self {
            EffectDef::WeaponAttack => Some(EffectCalc {
                dice: ctx.weapon_dice.clone(),
                bonus: ctx.str_mod,
                pierces_armor: false,
                is_heal: false,
            }),
            EffectDef::Damage { dice } => Some(EffectCalc {
                dice: Some(dice.clone()),
                bonus: ctx.str_mod,
                pierces_armor: false,
                is_heal: false,
            }),
            EffectDef::SpellDamage { dice } => Some(EffectCalc {
                dice: Some(dice.clone()),
                bonus: ctx.int_mod + ctx.spell_power,
                pierces_armor: true,
                is_heal: false,
            }),
            EffectDef::Heal { dice } => Some(EffectCalc {
                dice: Some(dice.clone()),
                bonus: ctx.int_mod + ctx.spell_power,
                pierces_armor: false,
                is_heal: true,
            }),
            EffectDef::None | EffectDef::GrantMovement { .. } => None,
        }
    }
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

#[derive(Deserialize)]
struct AbilityRecord {
    id: String,
    name: String,
    target_type: String,
    #[serde(default)]
    effect: String,
    #[serde(default)]
    range: u32,
    dice_count: Option<u32>,
    dice_sides: Option<u32>,
    #[serde(default)]
    distance: i32,
    #[serde(default)]
    rage_cost: i32,
    #[serde(default)]
    mana_cost: i32,
    #[serde(default)]
    statuses: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id: String,
    on: String,
    duration: u32,
}

const ABILITIES_PATH: &str = "assets/data/abilities.toml";

pub fn load_abilities() -> Vec<AbilityDef> {
    let src = std::fs::read_to_string(ABILITIES_PATH)
        .unwrap_or_else(|e| panic!("Cannot read {ABILITIES_PATH}: {e}"));
    let file: AbilityFile =
        toml::from_str(&src).unwrap_or_else(|e| panic!("Cannot parse {ABILITIES_PATH}: {e}"));

    file.abilities
        .into_iter()
        .map(|r| {
            let target_type = match r.target_type.as_str() {
                "single_enemy" => TargetType::SingleEnemy,
                "single_ally" => TargetType::SingleAlly,
                "myself" => TargetType::Myself,
                other => panic!("{ABILITIES_PATH}: unknown target_type '{other}'"),
            };
            let need_dice = |id: &str, count: Option<u32>, sides: Option<u32>| {
                DiceExpr::new(
                    count.unwrap_or_else(|| panic!("ability '{id}' missing dice_count")),
                    sides.unwrap_or_else(|| panic!("ability '{id}' missing dice_sides")),
                    0,
                )
            };
            let effect = match r.effect.as_str() {
                "" | "none" => EffectDef::None,
                "weapon_attack" => EffectDef::WeaponAttack,
                "damage" => EffectDef::Damage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "spell_damage" => EffectDef::SpellDamage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "heal" => EffectDef::Heal {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                },
                "grant_movement" => EffectDef::GrantMovement {
                    distance: r.distance,
                },
                other => panic!("{ABILITIES_PATH}: unknown effect '{other}'"),
            };
            let statuses = r
                .statuses
                .into_iter()
                .map(|s| {
                    let on = match s.on.as_str() {
                        "target" => StatusOn::Target,
                        "self" => StatusOn::MySelf,
                        other => panic!("{ABILITIES_PATH}: unknown status 'on' value '{other}'"),
                    };
                    StatusApplication {
                        status: StatusId::from(s.id.as_str()),
                        duration_rounds: s.duration,
                        on,
                    }
                })
                .collect();
            AbilityDef {
                id: AbilityId::from(r.id.as_str()),
                name: r.name,
                target_type,
                range: r.range,
                effect,
                rage_cost: r.rage_cost,
                mana_cost: r.mana_cost,
                statuses,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(str_mod: i32, int_mod: i32, spell_power: i32, weapon_dice: Option<DiceExpr>) -> CasterContext {
        CasterContext { str_mod, int_mod, spell_power, weapon_dice }
    }

    // ── calc() returns correct bonus and flags per effect type ────────────

    #[test]
    fn weapon_attack_uses_str_and_weapon_dice() {
        let weapon = DiceExpr::new(2, 6, 0);
        let c = ctx(4, 0, 0, Some(weapon.clone()));
        let calc = EffectDef::WeaponAttack.calc(&c).unwrap();
        assert_eq!(calc.bonus, 4);
        assert_eq!(calc.dice.unwrap().count, 2);
        assert!(!calc.pierces_armor);
        assert!(!calc.is_heal);
    }

    #[test]
    fn damage_uses_str_and_own_dice() {
        let c = ctx(3, 5, 2, Some(DiceExpr::new(99, 99, 0)));
        let dice = DiceExpr::new(1, 8, 0);
        let calc = EffectDef::Damage { dice }.calc(&c).unwrap();
        assert_eq!(calc.bonus, 3, "should use str_mod, not int_mod");
        assert_eq!(calc.dice.as_ref().unwrap().sides, 8, "should use ability dice, not weapon dice");
        assert!(!calc.pierces_armor);
    }

    #[test]
    fn spell_damage_uses_int_plus_spell_power_and_pierces() {
        let c = ctx(4, 3, 1, None);
        let dice = DiceExpr::new(2, 6, 0);
        let calc = EffectDef::SpellDamage { dice }.calc(&c).unwrap();
        assert_eq!(calc.bonus, 4, "int_mod(3) + spell_power(1)");
        assert!(calc.pierces_armor);
        assert!(!calc.is_heal);
    }

    #[test]
    fn heal_uses_int_plus_spell_power_and_is_heal() {
        let c = ctx(4, 2, 1, None);
        let dice = DiceExpr::new(1, 6, 0);
        let calc = EffectDef::Heal { dice }.calc(&c).unwrap();
        assert_eq!(calc.bonus, 3, "int_mod(2) + spell_power(1)");
        assert!(!calc.pierces_armor);
        assert!(calc.is_heal);
    }

    #[test]
    fn none_and_grant_movement_return_none() {
        let c = ctx(0, 0, 0, None);
        assert!(EffectDef::None.calc(&c).is_none());
        assert!(EffectDef::GrantMovement { distance: 3 }.calc(&c).is_none());
    }

    // ── expected() ───────────────────────────────────────────────────────

    #[test]
    fn expected_combines_dice_and_bonus() {
        let c = ctx(2, 0, 0, None);
        let dice = DiceExpr::new(2, 6, 0); // E[2d6] = 7.0
        let calc = EffectDef::Damage { dice }.calc(&c).unwrap();
        let expected = calc.expected();
        assert!((expected - 9.0).abs() < 0.01, "E[2d6]+2 = 9.0, got {expected}");
    }

    #[test]
    fn expected_without_dice_is_bonus_only() {
        let c = ctx(3, 0, 0, None); // no weapon dice
        let calc = EffectDef::WeaponAttack.calc(&c).unwrap();
        assert!((calc.expected() - 3.0).abs() < 0.01);
    }
}
