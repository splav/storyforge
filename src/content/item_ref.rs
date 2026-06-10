/// Internal runtime representation for items in the stash and loadouts.
///
/// NOTE: Authored `rewards` in TOML use bare ids (resolved by slot type at load time).
/// `ItemRef` is the internal tagged form used at runtime in `CampaignProgress`/`CampaignState`.
use combat_engine::{ArmorId, WeaponId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum ItemRef {
    Weapon(WeaponId),
    Armor(ArmorId),
}

/// Full snapshot of a hero's equipped gear (mirrors `Equipment` in components.rs).
///
/// Used in `CampaignProgress.loadouts` / `CampaignState.loadouts` to persist the
/// mutable per-character overlay over the class default.
///
/// All armor slots are mandatory (no `Default` derive — an empty `ArmorId` is
/// never a valid persisted state).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EquipmentSave {
    pub main_hand: Option<WeaponId>,
    pub off_hand: Option<WeaponId>,
    pub chest: ArmorId,
    pub legs: ArmorId,
    pub feet: ArmorId,
}

impl EquipmentSave {
    /// Snapshot an ECS `Equipment` component into a serialisable save record.
    pub fn from_equipment(eq: &crate::game::components::Equipment) -> Self {
        Self {
            main_hand: eq.main_hand.clone(),
            off_hand: eq.off_hand.clone(),
            chest: eq.chest.clone(),
            legs: eq.legs.clone(),
            feet: eq.feet.clone(),
        }
    }

    /// Materialise the save record back into an ECS `Equipment` component.
    pub fn to_equipment(&self) -> crate::game::components::Equipment {
        crate::game::components::Equipment {
            main_hand: self.main_hand.clone(),
            off_hand: self.off_hand.clone(),
            chest: self.chest.clone(),
            legs: self.legs.clone(),
            feet: self.feet.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ref_weapon_roundtrip() {
        let item = ItemRef::Weapon(WeaponId::from("iron_sword"));
        let s = toml::to_string_pretty(&item).unwrap();
        let parsed: ItemRef = toml::from_str(&s).unwrap();
        assert_eq!(parsed, item);
    }

    #[test]
    fn item_ref_armor_roundtrip() {
        let item = ItemRef::Armor(ArmorId::from("plate_chest"));
        let s = toml::to_string_pretty(&item).unwrap();
        let parsed: ItemRef = toml::from_str(&s).unwrap();
        assert_eq!(parsed, item);
    }
}
