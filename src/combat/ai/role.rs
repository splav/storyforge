use crate::content::abilities::{AoEShape, EffectDef, TargetType};
use crate::core::AbilityId;
use crate::game::resources::GameDb;
use bevy::prelude::*;

/// Tactical AI role — drives weight profiles in influence maps and utility scoring.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRole {
    /// Melee fighter: holds zone, finishes targets.
    Bruiser,
    /// Ranged physical: seeks distance and LOS.
    Archer,
    /// Ranged magic: AoE, control, spell damage.
    Mage,
    /// Healer / buffer / debuffer: protects allies.
    Support,
    /// Fast striker: focuses vulnerable or dangerous targets.
    Assassin,
}

/// Infer AI role from ability set and unit stats.
/// Priority: heal → AoE/spell → ranged physical → fast melee → default melee.
pub fn infer_role(abilities: &[AbilityId], speed: i32, db: &GameDb) -> AiRole {
    let mut has_heal_ally = false;
    let mut has_aoe_or_spell = false;
    let mut has_ranged_physical = false;

    for id in abilities {
        let Some(def) = db.abilities.get(id) else { continue };

        if def.target_type == TargetType::SingleAlly && matches!(def.effect, EffectDef::Heal { .. }) {
            has_heal_ally = true;
        }

        if def.aoe != AoEShape::None || matches!(def.effect, EffectDef::SpellDamage { .. }) {
            has_aoe_or_spell = true;
        }

        if def.target_type == TargetType::SingleEnemy
            && matches!(def.effect, EffectDef::Damage { .. })
            && def.range.min >= 2
        {
            has_ranged_physical = true;
        }
    }

    if has_heal_ally {
        return AiRole::Support;
    }
    if has_aoe_or_spell {
        return AiRole::Mage;
    }
    if has_ranged_physical {
        return AiRole::Archer;
    }
    if speed >= 5 {
        return AiRole::Assassin;
    }
    AiRole::Bruiser
}

/// Parse an optional TOML string into an AiRole.
pub fn parse_role(s: &str) -> Option<AiRole> {
    match s {
        "bruiser" => Some(AiRole::Bruiser),
        "archer" => Some(AiRole::Archer),
        "mage" => Some(AiRole::Mage),
        "support" => Some(AiRole::Support),
        "assassin" => Some(AiRole::Assassin),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AbilityId;

    fn db() -> GameDb {
        GameDb::default()
    }

    fn ids(names: &[&str]) -> Vec<AbilityId> {
        names.iter().map(|s| AbilityId::from(*s)).collect()
    }

    #[test]
    fn melee_only_is_bruiser() {
        let db = db();
        assert_eq!(infer_role(&ids(&["melee_attack"]), 3, &db), AiRole::Bruiser);
    }

    #[test]
    fn heal_ability_is_support() {
        let db = db();
        assert_eq!(infer_role(&ids(&["melee_attack", "heal"]), 3, &db), AiRole::Support);
    }

    #[test]
    fn aoe_spell_is_mage() {
        let db = db();
        assert_eq!(infer_role(&ids(&["melee_attack", "fireball"]), 4, &db), AiRole::Mage);
    }

    #[test]
    fn ranged_physical_is_archer() {
        let db = db();
        assert_eq!(infer_role(&ids(&["melee_attack", "bow_shot"]), 4, &db), AiRole::Archer);
    }

    #[test]
    fn fast_melee_is_assassin() {
        let db = db();
        assert_eq!(infer_role(&ids(&["melee_attack"]), 5, &db), AiRole::Assassin);
    }

    #[test]
    fn parse_role_valid() {
        assert_eq!(parse_role("mage"), Some(AiRole::Mage));
        assert_eq!(parse_role("bruiser"), Some(AiRole::Bruiser));
    }

    #[test]
    fn parse_role_invalid() {
        assert_eq!(parse_role("unknown"), None);
    }
}
