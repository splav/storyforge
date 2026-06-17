pub mod paths;
pub mod save_repo;
pub mod settings_repo;

pub use paths::AppPaths;

use bevy::prelude::*;

#[derive(Resource, Debug, Clone)]
pub struct PersistencePaths(pub AppPaths);

/// Returns `None` if the OS doesn't expose standard user dirs (extremely rare on desktop).
pub fn detect_paths() -> Option<AppPaths> {
    match AppPaths::detect() {
        Ok(paths) => {
            info!(
                "persistence paths: config={} data={} cache={} state={}",
                paths.config_dir.display(),
                paths.data_dir.display(),
                paths.cache_dir.display(),
                paths.state_dir.display(),
            );
            Some(paths)
        }
        Err(e) => {
            warn!("could not resolve user dirs, persistence disabled: {e}");
            None
        }
    }
}

pub struct PersistencePlugin {
    pub paths: Option<AppPaths>,
}

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        if let Some(paths) = self.paths.clone() {
            app.insert_resource(PersistencePaths(paths));
        }
    }
}
