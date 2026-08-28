//! Per-game profiles persisted in `%APPDATA%\iFrame\profiles.toml`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameProfile {
    pub target_fps: f64,
    /// "vsync" | "vrr" | "off"
    pub mode: String,
    pub vsync_override: bool,
    #[serde(default)]
    pub auto_attach: bool,
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            target_fps: 60.0,
            mode: "vsync".into(),
            vsync_override: true,
            auto_attach: false,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default)]
    games: HashMap<String, GameProfile>,
}

pub struct Profiles {
    path: PathBuf,
    games: HashMap<String, GameProfile>,
}

impl Profiles {
    pub fn load() -> Self {
        let path = Self::path();
        let games = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<FileFormat>(&s).ok())
            .map(|f| f.games)
            .unwrap_or_default();
        Self { path, games }
    }

    fn path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join("iFrame").join("profiles.toml")
    }

    pub fn get(&self, exe: &str) -> Option<&GameProfile> {
        self.games.get(exe)
    }

    pub fn set(&mut self, exe: &str, profile: GameProfile) {
        self.games.insert(exe.to_string(), profile);
        self.save();
    }

    fn save(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = FileFormat {
            games: self.games.clone(),
        };
        if let Ok(s) = toml::to_string_pretty(&file) {
            let _ = std::fs::write(&self.path, s);
        }
    }
}