use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{fs, io};

use crate::content::item_ref::{EquipmentSave, ItemRef};
use crate::persistence::paths::AppPaths;

pub const SLOT_COUNT: u8 = 3;

/// Versioned on-disk format. Adding a new variant + migration keeps old saves readable.
#[derive(Serialize, Deserialize)]
#[serde(tag = "version")]
enum SaveSlotFile {
    #[serde(rename = "1")]
    V1(SlotProfileV1),
}

/// A slot is a user profile: one save per campaign + the last campaign played.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct SlotProfileV1 {
    pub last_campaign: Option<String>,
    pub campaigns: HashMap<String, CampaignProgress>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CampaignProgress {
    pub scenario_index: usize,
    pub scenario_id: String,
    pub scene_index: usize,
    pub saved_at: u64,
    /// Campaign-wide flags (from victories, story choices, objectives).
    /// `#[serde(default)]` lets old saves (without this field) load as an empty list.
    #[serde(default)]
    pub flags: Vec<String>,
    /// Flat party-wide item stash (gear not currently equipped).
    #[serde(default)]
    pub stash: Vec<ItemRef>,
    /// Per-hero equipment overrides keyed by the hero's stable slug id.
    /// Overrides the class default on combat spawn. Missing entry → class default.
    #[serde(default)]
    pub loadouts: HashMap<String, EquipmentSave>,
}

fn slot_path(paths: &AppPaths, slot: u8) -> std::path::PathBuf {
    paths.saves_dir().join(format!("slot_{slot}.toml"))
}

pub fn exists(paths: &AppPaths, slot: u8) -> bool {
    slot_path(paths, slot).is_file()
}

pub fn load(paths: &AppPaths, slot: u8) -> Option<SlotProfileV1> {
    let path = slot_path(paths, slot);
    if !path.is_file() {
        return None;
    }
    match fs::read_to_string(&path) {
        Ok(src) => match toml::from_str::<SaveSlotFile>(&src) {
            Ok(SaveSlotFile::V1(v1)) => {
                info!(
                    "loaded slot {slot} from {} (campaigns={}, last={:?})",
                    path.display(),
                    v1.campaigns.len(),
                    v1.last_campaign,
                );
                Some(v1)
            }
            Err(e) => {
                let bak = path.with_extension("toml.bak");
                if let Err(re) = fs::rename(&path, &bak) {
                    warn!("slot {slot} parse failed ({e}); backup rename also failed: {re}");
                } else {
                    warn!(
                        "slot {slot} parse failed ({e}); backed up to {}",
                        bak.display()
                    );
                }
                None
            }
        },
        Err(e) => {
            warn!("cannot read slot {slot}: {e}");
            None
        }
    }
}

pub fn save(paths: &AppPaths, slot: u8, data: &SlotProfileV1) -> io::Result<()> {
    fs::create_dir_all(paths.saves_dir())?;
    let file = SaveSlotFile::V1(data.clone());
    let text =
        toml::to_string_pretty(&file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = slot_path(paths, slot);
    fs::write(&path, text)?;
    info!(
        "saved slot {slot} to {} (campaigns={}, last={:?})",
        path.display(),
        data.campaigns.len(),
        data.last_campaign,
    );
    Ok(())
}

pub fn delete(paths: &AppPaths, slot: u8) -> io::Result<()> {
    let path = slot_path(paths, slot);
    if path.exists() {
        fs::remove_file(&path)?;
        info!("deleted slot {slot} at {}", path.display());
    }
    Ok(())
}

/// Write a campaign's current position into the slot, updating `last_campaign`.
/// Reads existing profile to preserve other campaigns' progress.
pub fn record_progress(
    paths: &AppPaths,
    slot: u8,
    campaign: &crate::game::resources::CampaignState,
    scenario_id: &str,
    scene_index: usize,
) -> io::Result<()> {
    let mut profile = load(paths, slot).unwrap_or_default();
    profile.campaigns.insert(
        campaign.campaign_id.clone(),
        CampaignProgress {
            scenario_index: campaign.scenario_index,
            scenario_id: scenario_id.to_string(),
            scene_index,
            saved_at: now_unix(),
            flags: campaign.flags.iter().cloned().collect(),
            stash: campaign.stash.clone(),
            loadouts: campaign.loadouts.clone(),
        },
    );
    profile.last_campaign = Some(campaign.campaign_id.clone());
    save(paths, slot, &profile)
}

/// Drop a campaign's record from the slot (on completion or explicit delete).
/// Clears `last_campaign` if it pointed to this one.
pub fn clear_campaign(paths: &AppPaths, slot: u8, campaign_id: &str) -> io::Result<()> {
    let Some(mut profile) = load(paths, slot) else {
        return Ok(());
    };
    profile.campaigns.remove(campaign_id);
    if profile.last_campaign.as_deref() == Some(campaign_id) {
        profile.last_campaign = None;
    }
    if profile.campaigns.is_empty() && profile.last_campaign.is_none() {
        delete(paths, slot)
    } else {
        save(paths, slot, &profile)
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(scenario_index: usize, flags: Vec<&str>) -> CampaignProgress {
        CampaignProgress {
            scenario_index,
            scenario_id: "scen_x".into(),
            scene_index: 0,
            saved_at: 0,
            flags: flags.into_iter().map(str::to_string).collect(),
            stash: Vec::new(),
            loadouts: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn roundtrip_preserves_data() {
        let mut campaigns = HashMap::new();
        campaigns.insert(
            "camp_a".to_string(),
            CampaignProgress {
                scenario_index: 2,
                scenario_id: "scen_x".into(),
                scene_index: 3,
                saved_at: 1_700_000_000,
                flags: vec!["flag_a".into(), "flag_b".into()],
                stash: Vec::new(),
                loadouts: std::collections::HashMap::new(),
            },
        );
        let original = SlotProfileV1 {
            last_campaign: Some("camp_a".into()),
            campaigns,
        };
        let text = toml::to_string_pretty(&SaveSlotFile::V1(original.clone())).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        assert_eq!(parsed.last_campaign, original.last_campaign);
        assert_eq!(parsed.campaigns.len(), 1);
        let pr = parsed.campaigns.get("camp_a").unwrap();
        assert_eq!(pr.scenario_index, 2);
        assert_eq!(pr.scenario_id, "scen_x");
        assert_eq!(pr.scene_index, 3);
        assert_eq!(pr.saved_at, 1_700_000_000);
        assert_eq!(pr.flags, vec!["flag_a", "flag_b"]);
    }

    #[test]
    fn empty_profile_roundtrip() {
        let original = SlotProfileV1::default();
        let text = toml::to_string_pretty(&SaveSlotFile::V1(original)).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        assert!(parsed.last_campaign.is_none());
        assert!(parsed.campaigns.is_empty());
    }

    /// Flags round-trip: non-empty set survives serialize → deserialize.
    #[test]
    fn flags_roundtrip_nonempty() {
        let mut campaigns = HashMap::new();
        campaigns.insert(
            "c".to_string(),
            progress(1, vec!["found_token", "kael_found"]),
        );
        let slot = SlotProfileV1 {
            last_campaign: Some("c".into()),
            campaigns,
        };
        let text = toml::to_string_pretty(&SaveSlotFile::V1(slot)).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        let pr = parsed.campaigns.get("c").unwrap();
        assert_eq!(pr.flags, vec!["found_token", "kael_found"]);
    }

    /// Flags round-trip: empty vec stays empty.
    #[test]
    fn flags_roundtrip_empty() {
        let mut campaigns = HashMap::new();
        campaigns.insert("c".to_string(), progress(0, vec![]));
        let slot = SlotProfileV1 {
            last_campaign: None,
            campaigns,
        };
        let text = toml::to_string_pretty(&SaveSlotFile::V1(slot)).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        let pr = parsed.campaigns.get("c").unwrap();
        assert!(pr.flags.is_empty());
    }

    /// Old save without `flags` field → deserializes as empty vec (serde(default)).
    #[test]
    fn old_save_without_flags_defaults_to_empty() {
        // Manually construct TOML that lacks the `flags` key entirely.
        let toml_src = r#"
version = "1"

[campaigns.camp_a]
scenario_index = 0
scenario_id = "s1"
scene_index = 0
saved_at = 0
"#;
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(toml_src).unwrap();
        let pr = parsed.campaigns.get("camp_a").unwrap();
        assert!(
            pr.flags.is_empty(),
            "old save without flags field should default to empty, got {:?}",
            pr.flags
        );
    }

    /// Stash and loadouts round-trip through TOML serialize → deserialize.
    #[test]
    fn stash_and_loadouts_roundtrip() {
        use crate::content::item_ref::{EquipmentSave, ItemRef};
        use combat_engine::{ArmorId, WeaponId};

        let mut loadouts = std::collections::HashMap::new();
        loadouts.insert(
            "aldric".to_string(),
            EquipmentSave {
                main_hand: Some(WeaponId::from("long_sword")),
                off_hand: None,
                chest: ArmorId::from("plate_chest"),
                legs: ArmorId::from("plate_legs"),
                feet: ArmorId::from("plate_feet"),
            },
        );
        let mut campaigns = HashMap::new();
        campaigns.insert(
            "c".to_string(),
            CampaignProgress {
                scenario_index: 0,
                scenario_id: "s1".into(),
                scene_index: 0,
                saved_at: 0,
                flags: Vec::new(),
                stash: vec![
                    ItemRef::Weapon(WeaponId::from("long_sword")),
                    ItemRef::Armor(ArmorId::from("plate_chest")),
                ],
                loadouts,
            },
        );
        let slot = SlotProfileV1 { last_campaign: Some("c".into()), campaigns };
        let text = toml::to_string_pretty(&SaveSlotFile::V1(slot)).unwrap();
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(&text).unwrap();
        let pr = parsed.campaigns.get("c").unwrap();
        assert_eq!(pr.stash.len(), 2);
        assert_eq!(pr.stash[0], ItemRef::Weapon(WeaponId::from("long_sword")));
        assert_eq!(pr.stash[1], ItemRef::Armor(ArmorId::from("plate_chest")));
        let ald = pr.loadouts.get("aldric").expect("aldric loadout must survive roundtrip");
        assert_eq!(ald.main_hand, Some(WeaponId::from("long_sword")));
        assert!(ald.off_hand.is_none());
        assert_eq!(ald.chest, ArmorId::from("plate_chest"));
    }

    /// Old save without `stash`/`loadouts` keys → both default to empty.
    #[test]
    fn old_save_without_stash_loadouts_defaults_to_empty() {
        let toml_src = r#"
version = "1"

[campaigns.camp_a]
scenario_index = 0
scenario_id = "s1"
scene_index = 0
saved_at = 0
"#;
        let SaveSlotFile::V1(parsed) = toml::from_str::<SaveSlotFile>(toml_src).unwrap();
        let pr = parsed.campaigns.get("camp_a").unwrap();
        assert!(
            pr.stash.is_empty(),
            "old save without stash should default to empty, got {:?}",
            pr.stash
        );
        assert!(
            pr.loadouts.is_empty(),
            "old save without loadouts should default to empty, got {:?}",
            pr.loadouts
        );
    }

    /// `PartyRecord` with explicit id preserves it; without id → slug derived from name.
    #[test]
    fn party_record_id_explicit_and_derived() {
        use crate::content::scenarios::parse_scenario_body;

        let with_id = r#"
name = "test"
[[party]]
id      = "aldric"
name    = "Aldric"
race    = "human"
class   = "warrior"
hex_col = 0
hex_row = 0
[[scenes]]
type = "story"
"#;
        let scen = parse_scenario_body("s1", "test.toml", with_id);
        assert_eq!(scen.party[0].id, "aldric");
        assert_eq!(scen.party[0].name, "Aldric");

        let without_id = r#"
name = "test"
[[party]]
name    = "Lyra"
race    = "human"
class   = "mage"
hex_col = 0
hex_row = 0
[[scenes]]
type = "story"
"#;
        let scen2 = parse_scenario_body("s1", "test.toml", without_id);
        assert_eq!(scen2.party[0].id, "lyra", "id should be lowercased ASCII of name");
        assert_eq!(scen2.party[0].name, "Lyra");
    }
}
