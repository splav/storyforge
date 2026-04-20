use crate::content::weapons::WeaponDef;
use crate::core::{modifier, AbilityId, DiceExpr, ResourceKind, StatusId, WeaponId};
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

/// Range constraints for an ability.
#[derive(Debug, Clone, Copy)]
pub struct AbilityRange {
    /// Minimum comfortable range. Below this the attack is at disadvantage.
    pub min: u32,
    /// Maximum range in hex steps. 0 = self-only.
    pub max: u32,
}

impl AbilityRange {
    pub const SELF_ONLY: Self = Self { min: 0, max: 0 };
    pub const MELEE: Self = Self { min: 0, max: 1 };
}

/// Area-of-effect pattern. `None` = single-target (default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AoEShape {
    #[default]
    None,
    /// All cells within hex-distance ≤ radius from the target point.
    Circle { radius: u32 },
    /// Line of `length` cells from caster through target direction.
    Line { length: u32 },
}

#[derive(Debug, Clone)]
pub struct ResourceCost {
    pub resource: ResourceKind,
    pub amount: i32,
}

#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id: AbilityId,
    pub name: String,
    pub target_type: TargetType,
    pub range: AbilityRange,
    pub effect: EffectDef,
    pub costs: Vec<ResourceCost>,
    /// Action-point cost. Default 1. Zero = reaction / free utility.
    pub cost_ap: i32,
    pub aoe: AoEShape,
    /// If true, AoE damages allies too (e.g. fireball).
    pub friendly_fire: bool,
    /// Status effects applied when the ability resolves.
    pub statuses: Vec<StatusApplication>,
    /// Magic domains this ability belongs to (empty for non-magical abilities).
    pub magic_domains: Vec<String>,
    /// Magic method (empty string for non-magical abilities).
    pub magic_method: String,
    /// Custom hotkey (e.g. "M", "R"). Abilities with keys are universal (available to all).
    pub key: Option<String>,
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
    /// Restores HP and all resources (mana, rage, energy) by 1.
    RestoreResources,
    /// UI-only: toggles move mode. Does not go through resolution pipeline.
    ToggleMoveMode,
    /// Instantiates a new combatant from a unit template at a free hex near the caster.
    /// `max_active` caps concurrent summons from one caster; `None` = unlimited.
    Summon {
        template: String,
        max_active: Option<u32>,
    },
}

// ── Unified effect computation ──────────────────────────────────────────────

/// Context about the caster needed to compute effect values.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
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
            EffectDef::None
            | EffectDef::GrantMovement { .. }
            | EffectDef::RestoreResources
            | EffectDef::ToggleMoveMode
            | EffectDef::Summon { .. } => None,
        }
    }
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

fn default_range() -> u32 { 1 }
fn default_cost_ap() -> i32 { 1 }

#[derive(Deserialize)]
struct AbilityRecord {
    id: String,
    name: String,
    target_type: String,
    #[serde(default)]
    effect: String,
    #[serde(default = "default_range")]
    range: u32,
    #[serde(default)]
    min_range: u32,
    dice_count: Option<u32>,
    dice_sides: Option<u32>,
    #[serde(default)]
    distance: i32,
    #[serde(default)]
    costs: Vec<CostRecord>,
    #[serde(default = "default_cost_ap")]
    cost_ap: i32,
    #[serde(default)]
    aoe: String,
    #[serde(default)]
    aoe_size: u32,
    #[serde(default)]
    friendly_fire: bool,
    #[serde(default)]
    statuses: Vec<StatusRecord>,
    #[serde(default)]
    magic_domains: Vec<String>,
    #[serde(default)]
    magic_method: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    summon_template: Option<String>,
    #[serde(default)]
    summon_max_active: Option<u32>,
}

#[derive(Deserialize)]
struct StatusRecord {
    id: String,
    on: String,
    duration: u32,
}

#[derive(Deserialize)]
struct CostRecord {
    resource: String,
    amount: i32,
}

pub const ABILITIES_FILE: &str = "abilities.toml";

/// Reads the global layer's `abilities.toml`. Returns empty vec if missing.
pub fn load_abilities() -> Vec<AbilityDef> {
    let path = format!("assets/data/{ABILITIES_FILE}");
    if !std::path::Path::new(&path).is_file() {
        return Vec::new();
    }
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
    parse_abilities(&path, &src)
}

/// Parses a `abilities.toml` body. `path` is for error messages only.
pub fn parse_abilities(path: &str, src: &str) -> Vec<AbilityDef> {
    let file: AbilityFile =
        toml::from_str(src).unwrap_or_else(|e| panic!("Cannot parse {path}: {e}"));

    file.abilities
        .into_iter()
        .map(|r| {
            let target_type = match r.target_type.as_str() {
                "single_enemy" => TargetType::SingleEnemy,
                "single_ally" => TargetType::SingleAlly,
                "myself" => TargetType::Myself,
                other => panic!("{path}: unknown target_type '{other}'"),
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
                "restore_resources" => EffectDef::RestoreResources,
                "toggle_move_mode" => EffectDef::ToggleMoveMode,
                "summon" => EffectDef::Summon {
                    template: r.summon_template.clone().unwrap_or_else(|| {
                        panic!("{path}: ability '{}' effect=summon missing summon_template", r.id)
                    }),
                    max_active: r.summon_max_active,
                },
                other => panic!("{path}: unknown effect '{other}'"),
            };
            let statuses = r
                .statuses
                .into_iter()
                .map(|s| {
                    let on = match s.on.as_str() {
                        "target" => StatusOn::Target,
                        "self" => StatusOn::MySelf,
                        other => panic!("{path}: unknown status 'on' value '{other}'"),
                    };
                    StatusApplication {
                        status: StatusId::from(s.id.as_str()),
                        duration_rounds: s.duration,
                        on,
                    }
                })
                .collect();
            let aoe = match r.aoe.as_str() {
                "" | "none" => AoEShape::None,
                "circle" => AoEShape::Circle { radius: r.aoe_size },
                "line" => AoEShape::Line { length: r.aoe_size },
                other => panic!("{path}: ability '{}' unknown aoe '{other}'", r.id),
            };
            let costs: Vec<ResourceCost> = r
                .costs
                .into_iter()
                .map(|c| {
                    let resource = match c.resource.as_str() {
                        "hp" => ResourceKind::Hp,
                        "mana" => ResourceKind::Mana,
                        "rage" => ResourceKind::Rage,
                        "energy" => ResourceKind::Energy,
                        other => panic!("{path}: ability '{}' unknown resource '{other}'", r.id),
                    };
                    ResourceCost { resource, amount: c.amount }
                })
                .collect();
            let is_magical = !r.magic_domains.is_empty() || !r.magic_method.is_empty();
            if is_magical {
                let has_mana_cost = costs.iter().any(|c| c.resource == ResourceKind::Mana && c.amount > 0);
                assert!(has_mana_cost, "{path}: magical ability '{}' must have a mana cost", r.id);
            }

            AbilityDef {
                id: AbilityId::from(r.id.as_str()),
                name: r.name,
                target_type,
                range: AbilityRange { min: r.min_range, max: r.range },
                effect,
                costs,
                cost_ap: r.cost_ap,
                aoe,
                friendly_fire: r.friendly_fire,
                statuses,
                magic_domains: r.magic_domains,
                magic_method: r.magic_method,
                key: r.key,
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
