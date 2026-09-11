//! Per-game profiles persisted in `%APPDATA%\iFrame\profiles.toml`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameProfile {
    pub target_fps: f64,
    /// "vsync" | "vrr" | "off"
    pub mode: String,
    pub vsync_override: bool,
    #[serde(default)]
    pub auto_attach: bool,
    /// Force FRAME_LATENCY_WAITABLE_OBJECT on this game's swap chains
    /// (per-game opt-in, applied by the hook at swap chain creation).
    #[serde(default)]
    pub force_waitable: bool,
    /// Manual monitor refresh override in Hz (0 = trust the DWM hint).
    #[serde(default)]
    pub refresh_hz: f64,
    /// NVIDIA Reflex low latency mode: "off" | "on" | "boost"
    #[serde(default = "default_reflex_mode")]
    pub reflex_mode: String,
    /// In-game D3D11 overlay HUD enabled
    #[serde(default)]
    pub overlay_enabled: bool,
}

fn default_reflex_mode() -> String {
    "off".into()
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            target_fps: 60.0,
            mode: "vsync".into(),
            vsync_override: true,
            auto_attach: false,
            force_waitable: false,
            refresh_hz: 0.0,
            reflex_mode: "off".into(),
            overlay_enabled: false,
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
    /// Unwritten changes. The UI re-publishes settings on every drag tick, so
    /// disk writes are debounced to at most one per second (`flush_due`).
    dirty: bool,
    last_save: Instant,
}

impl Profiles {
    pub fn load() -> Self {
        let path = Self::path();
        let games = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<FileFormat>(&s).ok())
            .map(|f| f.games)
            .unwrap_or_default();
        Self {
            path,
            games,
            dirty: false,
            last_save: Instant::now(),
        }
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
        self.dirty = true;
        self.flush_due();
    }

    pub fn remove(&mut self, exe: &str) -> bool {
        if self.games.remove(exe).is_some() {
            self.dirty = true;
            self.flush();
            true
        } else {
            false
        }
    }

    pub fn list(&self) -> Vec<(String, GameProfile)> {
        let mut list: Vec<_> = self
            .games
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    pub fn all_games(&self) -> &HashMap<String, GameProfile> {
        &self.games
    }

    /// Export all profiles to a formatted TOML string.
    pub fn export_toml(&self) -> Result<String, String> {
        let file = FileFormat {
            games: self.games.clone(),
        };
        toml::to_string_pretty(&file).map_err(|e| format!("Failed to serialize TOML: {e}"))
    }

    /// Import profiles from a TOML string, merging with existing profiles.
    /// Returns the number of imported/updated profiles.
    pub fn import_toml(&mut self, toml_str: &str) -> Result<usize, String> {
        let parsed: FileFormat =
            toml::from_str(toml_str).map_err(|e| format!("Failed to parse TOML: {e}"))?;
        let count = parsed.games.len();
        for (exe, profile) in parsed.games {
            self.games.insert(exe, profile);
        }
        self.dirty = true;
        self.flush();
        Ok(count)
    }

    /// Write pending changes if the debounce interval has elapsed.
    pub fn flush_due(&mut self) {
        if self.dirty && self.last_save.elapsed() >= Duration::from_secs(1) {
            self.save();
        }
    }

    /// Write pending changes now (app shutdown or profile deletion/import).
    pub fn flush(&mut self) {
        if self.dirty {
            self.save();
        }
    }

    fn save(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = FileFormat {
            games: self.games.clone(),
        };
        if let Ok(s) = toml::to_string_pretty(&file) {
            let _ = std::fs::write(&self.path, s);
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }
}