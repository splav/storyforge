use crate::content::weapons::WeaponDef;
use crate::core::{modifier, AbilityId, DiceExpr, ResourceKind, StatusId, WeaponId};
use crate::game::components::{CombatStats, Equipment};
use serde::Deserialize;

pub use combat_engine::TargetType;

pub use combat_engine::StatusOn;

pub use combat_engine::StatusApplication;

pub use combat_engine::AbilityRange;

pub use combat_engine::AoEShape;






// EffectDef is re-exported from the engine — the canonical source of truth.
pub use combat_engine::EffectDef;

/// Extension trait that adds bridge-side effect computation to `EffectDef`.
/// Requires `CasterContext`, which is a bridge/game-layer type.
pub trait EffectCalcExt {
    fn calc(&self, ctx: &CasterContext) -> Option<EffectCalc>;
}

impl EffectCalcExt for EffectDef {
    fn calc(&self, ctx: &CasterContext) -> Option<EffectCalc> {
        match self {
            EffectDef::WeaponAttack => Some(EffectCalc {
                dice: ctx.weapon_dice.clone(),
                bonus: ctx.str_mod,
                pierces_armor: false,
                is_heal: false,
            }),
            EffectDef::Damage { dice } => Some(EffectCalc {
                dice: Some(*dice),
                bonus: ctx.str_mod,
                pierces_armor: false,
                is_heal: false,
            }),
            EffectDef::SpellDamage { dice } => Some(EffectCalc {
                dice: Some(*dice),
                bonus: ctx.int_mod + ctx.spell_power,
                pierces_armor: true,
                is_heal: false,
            }),
            EffectDef::Heal { dice } => Some(EffectCalc {
                dice: Some(*dice),
                bonus: ctx.int_mod + ctx.spell_power,
                pierces_armor: false,
                is_heal: true,
            }),
            EffectDef::None
            | EffectDef::GrantMovement { .. }
            | EffectDef::RestoreResources
            | EffectDef::Summon { .. } => None,
        }
    }
}

impl From<&AbilityDef> for combat_engine::AbilityDef {
    fn from(def: &AbilityDef) -> Self {
        def.engine.clone()
    }
}

pub use combat_engine::Cost as ResourceCost;

/// Bridge ability definition.
///
/// Gameplay fields live in `engine` (the `combat_engine::AbilityDef`); this
/// struct adds metadata fields that the engine doesn't need.
/// `Deref` makes all engine fields directly accessible: `def.cost_ap`, `def.aoe`, etc.
#[derive(Debug, Clone)]
pub struct AbilityDef {
    // ── metadata (bridge-only) ────────────────────────────────────────────
    pub id: AbilityId,
    pub name: String,
    /// Magic domains this ability belongs to (empty for non-magical abilities).
    pub magic_domains: Vec<String>,
    /// Magic method (empty string for non-magical abilities).
    pub magic_method: String,
    /// Optional override for AI semantic tags (replaces derived, not appends).
    /// `Some(vec![])` = explicitly empty tag set. `None` = use derived tags.
    /// Tag-name strings are validated (panic on unknown) in `tags::cache::build_caches`.
    /// Stored raw here to avoid content layer depending on the AI layer.
    pub ai_tags_override: Option<Vec<String>>,
    /// UI sentinel: this ability toggles move mode instead of going through the
    /// resolution pipeline. Set when TOML has `effect = "toggle_move_mode"`.
    pub is_move_toggle: bool,
    // ── gameplay (engine) ─────────────────────────────────────────────────
    /// All gameplay fields. Access via Deref: `def.cost_ap`, `def.effect`, etc.
    pub engine: combat_engine::AbilityDef,
}

impl std::ops::Deref for AbilityDef {
    type Target = combat_engine::AbilityDef;
    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}

impl std::ops::DerefMut for AbilityDef {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.engine
    }
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
    #[serde(default)]
    ai_tags_override: Option<Vec<String>>,
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
                "ground" => TargetType::Ground,
                other => panic!("{path}: unknown target_type '{other}'"),
            };
            let need_dice = |id: &str, count: Option<u32>, sides: Option<u32>| {
                DiceExpr::new(
                    count.unwrap_or_else(|| panic!("ability '{id}' missing dice_count")),
                    sides.unwrap_or_else(|| panic!("ability '{id}' missing dice_sides")),
                    0,
                )
            };
            let (effect, is_move_toggle) = match r.effect.as_str() {
                "" | "none" => (EffectDef::None, false),
                "weapon_attack" => (EffectDef::WeaponAttack, false),
                "damage" => (EffectDef::Damage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                }, false),
                "spell_damage" => (EffectDef::SpellDamage {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                }, false),
                "heal" => (EffectDef::Heal {
                    dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                }, false),
                "grant_movement" => (EffectDef::GrantMovement {
                    distance: r.distance,
                }, false),
                "restore_resources" => (EffectDef::RestoreResources, false),
                // UI-only sentinel: no engine effect; flag set on AbilityDef.
                "toggle_move_mode" => (EffectDef::None, true),
                "summon" => (EffectDef::Summon {
                    template_id: r.summon_template.clone().unwrap_or_else(|| {
                        panic!("{path}: ability '{}' effect=summon missing summon_template", r.id)
                    }),
                    max_active: r.summon_max_active,
                }, false),
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
                magic_domains: r.magic_domains,
                magic_method: r.magic_method,
                ai_tags_override: r.ai_tags_override,
                is_move_toggle,
                engine: combat_engine::AbilityDef {
                    key: r.key,
                    cost_ap: r.cost_ap,
                    costs,
                    range: AbilityRange { min: r.min_range, max: r.range },
                    target_type,
                    aoe,
                    friendly_fire: r.friendly_fire,
                    effect,
                    statuses,
                },
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

    // ── ai_tags_override parse tests ──────────────────────────────────────

    #[test]
    fn parse_abilities_default_override_is_none() {
        // TOML without ai_tags_override field → None (via #[serde(default)])
        let src = r#"
[[abilities]]
id          = "melee_attack"
name        = "Strike"
target_type = "single_enemy"
effect      = "weapon_attack"
range       = 1
"#;
        let defs = parse_abilities("test", src);
        assert_eq!(defs.len(), 1);
        assert!(
            defs[0].ai_tags_override.is_none(),
            "missing ai_tags_override must deserialise as None"
        );
    }

    #[test]
    fn parse_abilities_with_override_field_round_trip() {
        // TOML with ai_tags_override → Some(vec![...])
        let src = r#"
[[abilities]]
id               = "rush"
name             = "Rush"
target_type      = "myself"
effect           = "grant_movement"
distance         = 2
range            = 0
ai_tags_override = ["mobility"]
"#;
        let defs = parse_abilities("test", src);
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs[0].ai_tags_override,
            Some(vec!["mobility".to_string()]),
            "ai_tags_override must round-trip through TOML deserialization"
        );
    }
}
