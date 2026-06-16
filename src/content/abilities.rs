use crate::content::weapons::WeaponDef;
use crate::game::components::{CombatStats, Equipment};
use combat_engine::{modifier, AbilityId, DiceExpr, ResourceKind, StatusId, WeaponId};
use serde::Deserialize;

pub use combat_engine::TargetType;

pub use combat_engine::StatusOn;

pub use combat_engine::StatusApplication;

pub use combat_engine::AbilityRange;

pub use combat_engine::AoEShape;

// EffectDef and PassiveTrigger are re-exported from the engine.
pub use combat_engine::EffectDef;
use combat_engine::PassiveTrigger as EngineTrigger;

/// Extension trait that adds bridge-side effect computation to `EffectDef`.
/// Requires `CasterContext`, which is a bridge/game-layer type.
///
/// `power` is the ability-level multiplier (`AbilityDef::power()`).  For callers
/// that have an `AbilityDef` (bridge type), pass `def.engine.power()`.
pub trait EffectCalcExt {
    fn calc(&self, ctx: &CasterContext, power: f32) -> Option<EffectCalc>;
}

impl EffectCalcExt for EffectDef {
    fn calc(&self, ctx: &CasterContext, power: f32) -> Option<EffectCalc> {
        match self {
            EffectDef::WeaponAttack { ranged } => Some(EffectCalc {
                dice: if *ranged {
                    ctx.ranged_dice
                } else {
                    ctx.weapon_dice
                },
                bonus: if *ranged { ctx.dex_mod } else { ctx.str_mod },
                power,
                pierces_armor: false,
                magic: false,
                is_heal: false,
            }),
            EffectDef::Damage { dice } => Some(EffectCalc {
                dice: Some(*dice),
                bonus: ctx.str_mod,
                power: 1.0,
                pierces_armor: false,
                magic: false,
                is_heal: false,
            }),
            EffectDef::SpellDamage { dice } => {
                // bonus = int_mod + round(power × spell_power) — matches engine formula.
                let sp_scaled = (power * ctx.spell_power as f32).round() as i32;
                Some(EffectCalc {
                    dice: Some(*dice),
                    bonus: ctx.int_mod + sp_scaled,
                    power: 1.0,
                    // magic=true routes mitigation to magic_resist (not armor);
                    // pierces_armor only suppresses ALL mitigation, so it stays false.
                    pierces_armor: false,
                    magic: true,
                    is_heal: false,
                })
            }
            EffectDef::Heal { dice } => {
                // bonus = int_mod + round(power × spell_power) — matches engine formula.
                let sp_scaled = (power * ctx.spell_power as f32).round() as i32;
                Some(EffectCalc {
                    dice: Some(*dice),
                    bonus: ctx.int_mod + sp_scaled,
                    power: 1.0,
                    pierces_armor: false,
                    magic: false,
                    is_heal: true,
                })
            }
            EffectDef::None
            | EffectDef::GrantMovement { .. }
            | EffectDef::RestoreResources
            | EffectDef::Summon { .. }
            | EffectDef::RevealEnvInRange { .. } => None,
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
    /// Melee weapon dice (first non-ranged weapon found in main/off-hand).
    pub weapon_dice: Option<DiceExpr>,
    /// Ranged weapon dice (first ranged weapon found in main/off-hand).
    #[serde(default)]
    pub ranged_dice: Option<DiceExpr>,
    /// Dexterity modifier for ranged attacks and initiative.
    #[serde(default)]
    pub dex_mod: i32,
}

impl CasterContext {
    pub fn new(
        stats: &CombatStats,
        equip: Option<&Equipment>,
        weapons: &std::collections::HashMap<WeaponId, WeaponDef>,
    ) -> Self {
        // Single source of truth for the melee/ranged dice-source rule.
        // melee_dice  = first of [main_hand, off_hand] that is NOT ranged → its dice.
        // ranged_dice = first of [main_hand, off_hand] that IS ranged → its dice.
        let slots: [Option<&WeaponId>; 2] = equip.map_or([None, None], |e| {
            [e.main_hand.as_ref(), e.off_hand.as_ref()]
        });
        let mut weapon_dice: Option<DiceExpr> = None;
        let mut ranged_dice: Option<DiceExpr> = None;
        let mut spell_power = 0i32;
        for slot in slots.into_iter().flatten() {
            if let Some(wd) = weapons.get(slot) {
                if wd.ranged {
                    if ranged_dice.is_none() {
                        ranged_dice = Some(wd.dice);
                    }
                } else {
                    if weapon_dice.is_none() {
                        weapon_dice = Some(wd.dice);
                        // spell_power comes from the first melee weapon (original behaviour)
                        spell_power = wd.spell_power;
                    }
                }
            }
        }
        Self {
            str_mod: modifier(stats.strength),
            int_mod: modifier(stats.intelligence),
            spell_power,
            weapon_dice,
            ranged_dice,
            dex_mod: modifier(stats.dexterity),
        }
    }
}

/// Computed parameters for an ability effect.
pub struct EffectCalc {
    pub dice: Option<DiceExpr>,
    pub bonus: i32,
    pub power: f32,
    pub pierces_armor: bool,
    /// True for spell damage — engine uses magic_resist instead of armor.
    pub magic: bool,
    pub is_heal: bool,
}

impl EffectCalc {
    pub fn expected(&self) -> f32 {
        self.dice
            .as_ref()
            .map_or(0.0, |d| d.expected() * self.power)
            + self.bonus as f32
    }
}

// ── TOML loading ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AbilityFile {
    abilities: Vec<AbilityRecord>,
}

fn default_range() -> u32 {
    1
}
fn default_cost_ap() -> i32 {
    1
}

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
    #[serde(default)]
    requires_los: bool,
    /// Passive trigger list. Use `passive = ["turn_start"]` (list form).
    #[serde(default)]
    passive: Vec<String>,
    /// Tags the target must have (ALL required).  `SingleEnemy`/`SingleAlly` only.
    #[serde(default)]
    requires_tags: Vec<String>,
    /// Tags the target must NOT have.  `SingleEnemy`/`SingleAlly` only.
    #[serde(default)]
    excludes_tags: Vec<String>,
    /// `weapon_attack` only: `true` → uses ranged_dice + dex_mod.
    #[serde(default)]
    ranged: bool,
    /// Per-ability power multiplier. `None` (omitted in TOML) means 1.0.
    /// For `weapon_attack`: scales dice. For magical abilities: scales `spell_power`.
    #[serde(default)]
    power: Option<f32>,
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
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read {path}: {e}"));
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
                "environment" => TargetType::Environment,
                other => panic!("{path}: unknown target_type '{other}'"),
            };
            let need_dice = |id: &str, count: Option<u32>, sides: Option<u32>| {
                DiceExpr::new(
                    count.unwrap_or_else(|| panic!("ability '{id}' missing dice_count")),
                    sides.unwrap_or_else(|| panic!("ability '{id}' missing dice_sides")),
                    0,
                )
            };
            let aoe = match r.aoe.as_str() {
                "" | "none" => AoEShape::None,
                "circle" => AoEShape::Circle { radius: r.aoe_size },
                "line" => AoEShape::Line { length: r.aoe_size },
                other => panic!("{path}: ability '{}' unknown aoe '{other}'", r.id),
            };
            let (effect, is_move_toggle) = match r.effect.as_str() {
                "" | "none" => (EffectDef::None, false),
                "weapon_attack" => (EffectDef::WeaponAttack { ranged: r.ranged }, false),
                "damage" => (
                    EffectDef::Damage {
                        dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                    },
                    false,
                ),
                "spell_damage" => (
                    EffectDef::SpellDamage {
                        dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                    },
                    false,
                ),
                "heal" => (
                    EffectDef::Heal {
                        dice: need_dice(&r.id, r.dice_count, r.dice_sides),
                    },
                    false,
                ),
                "grant_movement" => (
                    EffectDef::GrantMovement {
                        distance: r.distance,
                    },
                    false,
                ),
                "restore_resources" => (EffectDef::RestoreResources, false),
                // UI-only sentinel: no engine effect; flag set on AbilityDef.
                "toggle_move_mode" => (EffectDef::None, true),
                "summon" => (
                    EffectDef::Summon {
                        template_id: r.summon_template.clone().unwrap_or_else(|| {
                            panic!(
                                "{path}: ability '{}' effect=summon missing summon_template",
                                r.id
                            )
                        }),
                        max_active: r.summon_max_active,
                    },
                    false,
                ),
                // "reveal_env" is the canonical token; "reveal_env_in_range" accepted
                // as a legacy alias.  Radius is sourced from the aoe shape.
                "reveal_env" | "reveal_env_in_range" => {
                    let range = match aoe {
                        AoEShape::Circle { radius } => radius as i32,
                        _ => 0,
                    };
                    (EffectDef::RevealEnvInRange { range }, false)
                }
                other => panic!("{path}: unknown effect '{other}'"),
            };
            let passive: Vec<EngineTrigger> = r
                .passive
                .iter()
                .map(|tok| match tok.as_str() {
                    "turn_start" => EngineTrigger::TurnStart,
                    "on_move" => EngineTrigger::OnMove,
                    other => panic!(
                        "{path}: ability '{}' unknown passive trigger '{other}'",
                        r.id
                    ),
                })
                .collect();
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
                    ResourceCost {
                        resource,
                        amount: c.amount,
                    }
                })
                .collect();
            let is_magical = !r.magic_domains.is_empty() || !r.magic_method.is_empty();
            if is_magical {
                let has_mana_cost = costs
                    .iter()
                    .any(|c| c.resource == ResourceKind::Mana && c.amount > 0);
                assert!(
                    has_mana_cost,
                    "{path}: magical ability '{}' must have a mana cost",
                    r.id
                );
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
                    range: AbilityRange {
                        min: r.min_range,
                        max: r.range,
                    },
                    target_type,
                    aoe,
                    friendly_fire: r.friendly_fire,
                    effect,
                    statuses,
                    requires_los: r.requires_los,
                    passive,
                    requires_tags: r
                        .requires_tags
                        .iter()
                        .map(|s| combat_engine::TagId::from(s.as_str()))
                        .collect(),
                    excludes_tags: r
                        .excludes_tags
                        .iter()
                        .map(|s| combat_engine::TagId::from(s.as_str()))
                        .collect(),
                    power: r.power,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(
        str_mod: i32,
        int_mod: i32,
        spell_power: i32,
        weapon_dice: Option<DiceExpr>,
    ) -> CasterContext {
        CasterContext {
            str_mod,
            int_mod,
            spell_power,
            weapon_dice,
            dex_mod: 0,
            ranged_dice: None,
        }
    }

    // ── calc() returns correct bonus and flags per effect type ────────────

    #[test]
    fn weapon_attack_uses_str_and_weapon_dice() {
        let weapon = DiceExpr::new(2, 6, 0);
        let c = ctx(4, 0, 0, Some(weapon));
        let calc = EffectDef::WeaponAttack { ranged: false }
            .calc(&c, 1.0)
            .unwrap();
        assert_eq!(calc.bonus, 4);
        assert_eq!(calc.dice.unwrap().count, 2);
        assert!(!calc.pierces_armor);
        assert!(!calc.is_heal);
    }

    #[test]
    fn damage_uses_str_and_own_dice() {
        let c = ctx(3, 5, 2, Some(DiceExpr::new(99, 99, 0)));
        let dice = DiceExpr::new(1, 8, 0);
        let calc = EffectDef::Damage { dice }.calc(&c, 1.0).unwrap();
        assert_eq!(calc.bonus, 3, "should use str_mod, not int_mod");
        assert_eq!(
            calc.dice.as_ref().unwrap().sides,
            8,
            "should use ability dice, not weapon dice"
        );
        assert!(!calc.pierces_armor);
    }

    #[test]
    fn spell_damage_uses_int_plus_spell_power_and_is_magic() {
        let c = ctx(4, 3, 1, None);
        let dice = DiceExpr::new(2, 6, 0);
        let calc = EffectDef::SpellDamage { dice }.calc(&c, 1.0).unwrap();
        assert_eq!(calc.bonus, 4, "int_mod(3) + spell_power(1)");
        // SpellDamage uses magic_resist (not armor): magic=true, pierces_armor=false.
        assert!(calc.magic, "spell damage must be flagged magic");
        assert!(
            !calc.pierces_armor,
            "spell damage does not pierce — uses magic_resist instead"
        );
        assert!(!calc.is_heal);
    }

    #[test]
    fn heal_uses_int_plus_spell_power_and_is_heal() {
        let c = ctx(4, 2, 1, None);
        let dice = DiceExpr::new(1, 6, 0);
        let calc = EffectDef::Heal { dice }.calc(&c, 1.0).unwrap();
        assert_eq!(calc.bonus, 3, "int_mod(2) + spell_power(1)");
        assert!(!calc.pierces_armor);
        assert!(calc.is_heal);
    }

    #[test]
    fn none_and_grant_movement_return_none() {
        let c = ctx(0, 0, 0, None);
        assert!(EffectDef::None.calc(&c, 1.0).is_none());
        assert!(EffectDef::GrantMovement { distance: 3 }
            .calc(&c, 1.0)
            .is_none());
    }

    // ── expected() ───────────────────────────────────────────────────────

    #[test]
    fn expected_combines_dice_and_bonus() {
        let c = ctx(2, 0, 0, None);
        let dice = DiceExpr::new(2, 6, 0); // E[2d6] = 7.0
        let calc = EffectDef::Damage { dice }.calc(&c, 1.0).unwrap();
        let expected = calc.expected();
        assert!(
            (expected - 9.0).abs() < 0.01,
            "E[2d6]+2 = 9.0, got {expected}"
        );
    }

    #[test]
    fn expected_without_dice_is_bonus_only() {
        let c = ctx(3, 0, 0, None); // no weapon dice
        let calc = EffectDef::WeaponAttack { ranged: false }
            .calc(&c, 1.0)
            .unwrap();
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

    // ── CasterContext::new dice-source rule ──────────────────────────────────

    fn base_stats() -> CombatStats {
        CombatStats {
            strength: 10, // modifier = 0
            dexterity: 10,
            intelligence: 10,
            ..Default::default()
        }
    }

    fn make_weapon(id: &str, dice: DiceExpr, ranged: bool) -> (WeaponId, WeaponDef) {
        let wid = WeaponId::from(id);
        let def = WeaponDef {
            id: wid.clone(),
            name: id.to_string(),
            hand: crate::content::weapons::HandType::MainHand,
            dice,
            ranged,
            spell_power: 0,
            stats: Default::default(),
            image: None,
        };
        (wid, def)
    }

    /// bow (ranged) in main + dagger (melee) in off-hand →
    /// weapon_dice = dagger, ranged_dice = bow.
    #[test]
    fn caster_ctx_new_bow_plus_dagger_routes_both_channels() {
        let bow_dice = DiceExpr::new(1, 8, 0);
        let dagger_dice = DiceExpr::new(1, 4, 0);
        let (bow_id, bow_def) = make_weapon("bow", bow_dice, true);
        let (dagger_id, dagger_def) = make_weapon("dagger", dagger_dice, false);

        let weapons: std::collections::HashMap<WeaponId, WeaponDef> =
            [(bow_id.clone(), bow_def), (dagger_id.clone(), dagger_def)].into();

        let equip = Equipment {
            main_hand: Some(bow_id),
            off_hand: Some(dagger_id),
            chest: combat_engine::ArmorId::from(""),
            legs: combat_engine::ArmorId::from(""),
            feet: combat_engine::ArmorId::from(""),
        };
        let ctx = CasterContext::new(&base_stats(), Some(&equip), &weapons);
        assert_eq!(ctx.weapon_dice, Some(dagger_dice), "melee = dagger");
        assert_eq!(ctx.ranged_dice, Some(bow_dice), "ranged = bow");
    }

    /// Two-handed melee only → ranged_dice = None.
    #[test]
    fn caster_ctx_new_melee_only_has_no_ranged_dice() {
        let sword_dice = DiceExpr::new(1, 8, 0);
        let (sword_id, sword_def) = make_weapon("sword", sword_dice, false);
        let weapons: std::collections::HashMap<WeaponId, WeaponDef> =
            [(sword_id.clone(), sword_def)].into();

        let equip = Equipment {
            main_hand: Some(sword_id),
            off_hand: None,
            chest: combat_engine::ArmorId::from(""),
            legs: combat_engine::ArmorId::from(""),
            feet: combat_engine::ArmorId::from(""),
        };
        let ctx = CasterContext::new(&base_stats(), Some(&equip), &weapons);
        assert_eq!(ctx.weapon_dice, Some(sword_dice), "melee = sword");
        assert_eq!(
            ctx.ranged_dice, None,
            "no ranged weapon → ranged_dice = None"
        );
    }

    /// Ranged main + empty off-hand → weapon_dice = None.
    #[test]
    fn caster_ctx_new_ranged_only_has_no_melee_dice() {
        let bow_dice = DiceExpr::new(1, 8, 0);
        let (bow_id, bow_def) = make_weapon("bow", bow_dice, true);
        let weapons: std::collections::HashMap<WeaponId, WeaponDef> =
            [(bow_id.clone(), bow_def)].into();

        let equip = Equipment {
            main_hand: Some(bow_id),
            off_hand: None,
            chest: combat_engine::ArmorId::from(""),
            legs: combat_engine::ArmorId::from(""),
            feet: combat_engine::ArmorId::from(""),
        };
        let ctx = CasterContext::new(&base_stats(), Some(&equip), &weapons);
        assert_eq!(
            ctx.weapon_dice, None,
            "no melee weapon → weapon_dice = None"
        );
        assert_eq!(ctx.ranged_dice, Some(bow_dice), "ranged = bow");
    }

    // ── EffectCalc::expected power math ──────────────────────────────────────

    /// WeaponAttack power=0.5: expected = dice.expected() * 0.5 + bonus.
    /// E[2d6] = 7.0. power=0.5. str_mod=3 (bonus). expected = 7.0*0.5 + 3 = 6.5.
    #[test]
    fn effect_calc_expected_scales_by_power() {
        let weapon = DiceExpr::new(2, 6, 0); // E = 7.0
        let c = CasterContext {
            str_mod: 3,
            weapon_dice: Some(weapon),
            ..Default::default()
        };
        let calc = EffectDef::WeaponAttack { ranged: false }
            .calc(&c, 0.5)
            .unwrap();
        assert!(
            (calc.expected() - 6.5).abs() < 0.01,
            "E[2d6]*0.5 + 3 = 6.5, got {}",
            calc.expected()
        );
    }

    /// Ranged WeaponAttack: calc uses ranged_dice + dex_mod, ignores str/weapon.
    #[test]
    fn ranged_weapon_attack_uses_dex_mod_and_ranged_dice() {
        let bow = DiceExpr::new(1, 8, 0);
        let c = CasterContext {
            str_mod: 99, // must NOT be used
            dex_mod: 4,
            weapon_dice: None,
            ranged_dice: Some(bow),
            ..Default::default()
        };
        let calc = EffectDef::WeaponAttack { ranged: true }
            .calc(&c, 1.0)
            .unwrap();
        assert_eq!(calc.bonus, 4, "bonus = dex_mod");
        assert_eq!(calc.dice.unwrap().sides, 8, "dice = ranged (1d8)");
    }
}
