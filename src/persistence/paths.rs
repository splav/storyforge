use directories::ProjectDirs;
use std::{fs, io, path::PathBuf};

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "Storyforge";
const APPLICATION: &str = "Storyforge";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl AppPaths {
    pub fn detect() -> io::Result<Self> {
        let proj = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no standard user dirs"))?;

        let config_dir = proj.config_dir().to_path_buf();
        let data_dir = proj.data_local_dir().to_path_buf();
        let cache_dir = proj.cache_dir().to_path_buf();
        let state_dir = proj
            .state_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("state"));

        for dir in [&config_dir, &data_dir, &cache_dir, &state_dir] {
            fs::create_dir_all(dir)?;
        }

        Ok(Self {
            config_dir,
            data_dir,
            cache_dir,
            state_dir,
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn saves_dir(&self) -> PathBuf {
        self.data_dir.join("saves")
    }
}
