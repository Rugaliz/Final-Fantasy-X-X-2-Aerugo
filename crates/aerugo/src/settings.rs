//! User settings, stored as TOML in the user folder.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SETTINGS_FILE: &str = "settings.toml";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: WindowSettings,
    pub graphics: GraphicsSettings,
    pub ui: UiSettings,
    pub game: GameSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    /// fifo, fifo-relaxed, mailbox or immediate.
    pub present_mode: String,
    /// Frame rate cap in Hz; 0 = uncapped.
    pub frame_limit: u32,
    /// Prefer the GPU whose name contains this text; empty = automatic.
    pub gpu: String,
    pub validation: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Zoom applied on top of the display scale.
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceLanguage {
    English,
    Japanese,
}

impl VoiceLanguage {
    pub fn label(self) -> &'static str {
        match self {
            VoiceLanguage::English => "English",
            VoiceLanguage::Japanese => "Japanese",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// Install folder chosen by the user; empty = automatic discovery.
    pub dir: Option<PathBuf>,
    pub voice: VoiceLanguage,
    /// Text language code (us, jp, de, fr, it, es, kr, cn, ch).
    pub text_language: String,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { width: 1600, height: 900, fullscreen: false }
    }
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self { present_mode: "fifo".into(), frame_limit: 0, gpu: String::new(), validation: false }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

impl Default for GameSettings {
    fn default() -> Self {
        Self { dir: None, voice: VoiceLanguage::English, text_language: "us".into() }
    }
}

impl Settings {
    /// Load settings. A missing file gives defaults; an unreadable one is kept aside as `.bad` and replaced
    /// by defaults so a broken file never prevents start-up.
    pub fn load(path: &Path) -> Settings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                log::warn!("cannot read {}: {e}; using defaults", path.display());
                return Settings::default();
            }
        };
        match toml::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                let bad = path.with_extension("toml.bad");
                log::warn!("invalid settings in {} ({e}); moved to {}", path.display(), bad.display());
                std::fs::rename(path, &bad).ok();
                Settings::default()
            }
        }
    }

    /// Write atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aerugo-settings-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("rt");
        let path = dir.join(SETTINGS_FILE);
        let mut s = Settings::default();
        s.window.fullscreen = true;
        s.graphics.frame_limit = 240;
        s.graphics.present_mode = "mailbox".into();
        s.game.voice = VoiceLanguage::Japanese;
        s.game.dir = Some(PathBuf::from("/games/ffx"));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = toml::from_str("[graphics]\nframe_limit = 144\n").unwrap();
        assert_eq!(s.graphics.frame_limit, 144);
        assert_eq!(s.graphics.present_mode, "fifo");
        assert_eq!(s.window, WindowSettings::default());
    }

    #[test]
    fn broken_file_is_set_aside() {
        let dir = temp_dir("bad");
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(path.with_extension("toml.bad").exists());
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
