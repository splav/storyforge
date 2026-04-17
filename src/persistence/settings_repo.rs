use bevy::prelude::*;
use std::{fs, io};

use crate::content::settings::{GameSettings, SettingsFile, load_bundled_settings};
use crate::persistence::paths::AppPaths;

/// Layered load: user config overrides bundled defaults.
/// On parse error: rename user file to `.bak` and fall back to bundled.
pub fn load_layered(paths: Option<&AppPaths>) -> GameSettings {
    let bundled = load_bundled_settings();
    let Some(paths) = paths else { return bundled };
    let file_path = paths.settings_file();
    if !file_path.exists() {
        info!("no user settings at {}, using bundled defaults", file_path.display());
        return bundled;
    }
    match fs::read_to_string(&file_path) {
        Ok(src) => match toml::from_str::<SettingsFile>(&src) {
            Ok(parsed) => {
                info!("loaded user settings from {}", file_path.display());
                GameSettings::from_file(parsed)
            }
            Err(e) => {
                let bak = file_path.with_extension("toml.bak");
                if let Err(re) = fs::rename(&file_path, &bak) {
                    warn!("settings parse failed ({e}); backup rename also failed: {re}");
                } else {
                    warn!(
                        "settings parse failed ({e}); backed up to {} and using defaults",
                        bak.display()
                    );
                }
                bundled
            }
        },
        Err(e) => {
            warn!("cannot read user settings: {e}; using defaults");
            bundled
        }
    }
}

/// Persist current settings to the user config file.
pub fn save(paths: &AppPaths, settings: &GameSettings) -> io::Result<()> {
    let file = settings.to_file();
    let text = toml::to_string_pretty(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = paths.settings_file();
    fs::write(&path, text)?;
    info!("saved user settings to {}", path.display());
    Ok(())
}
