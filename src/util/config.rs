//! Game configuration loaded from config.toml.
//!
//! config.toml is machine-specific (contains the local RA2 install path)
//! and is gitignored. A config.toml.example template is provided in the repo.
//!
//! ## Dependency rules
//! - config.rs is part of util/ â€” no dependencies on game modules.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Default config file name â€” looked up in the current working directory.
const CONFIG_FILE_NAME: &str = "config.toml";

/// Top-level game configuration, deserialized from config.toml.
///
/// Add new sections here as features are implemented (audio, game speed, etc.).
#[derive(Debug, Deserialize)]
pub struct GameConfig {
    /// File system paths (RA2 install directory).
    pub paths: PathsConfig,
    /// Graphics/window settings (all optional — sensible defaults provided).
    #[serde(default)]
    pub graphics: GraphicsConfig,
    /// Deterministic simulation settings.
    #[serde(default)]
    pub gameplay: GameplayConfig,
}

/// Paths to external resources (the player's RA2 installation).
#[derive(Debug, Deserialize)]
pub struct PathsConfig {
    /// Path to the user's RA2 installation directory.
    /// MIX files (ra2.mix, language.mix, theme.mix) are loaded from here.
    /// Example: "C:/Program Files/EA Games/Command and Conquer Red Alert II"
    pub ra2_dir: PathBuf,
}

/// Graphics and window settings.
///
/// Every field has a sensible default so `[graphics]` can be omitted entirely.
#[derive(Debug, Deserialize)]
pub struct GraphicsConfig {
    /// Window width in pixels.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Window height in pixels.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Whether to enable vertical sync (reduces tearing, caps framerate).
    #[serde(default = "default_true")]
    pub vsync: bool,
    /// Enable Catmull-Rom bicubic upscaling (renders at half resolution, upscales to window).
    #[serde(default)]
    pub upscale: bool,
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            vsync: true,
            upscale: false,
        }
    }
}

impl GraphicsConfig {
    /// Render width: half of window width when upscaling, otherwise full window width.
    pub fn render_width(&self) -> u32 {
        if self.upscale {
            self.width / 2
        } else {
            self.width
        }
    }

    /// Render height: half of window height when upscaling, otherwise full window height.
    pub fn render_height(&self) -> u32 {
        if self.upscale {
            self.height / 2
        } else {
            self.height
        }
    }
}

/// Deterministic simulation and command scheduling settings.
#[derive(Debug, Deserialize)]
pub struct GameplayConfig {
    /// Fixed simulation tick rate (Hz).
    #[serde(default = "default_sim_tick_hz")]
    pub sim_tick_hz: u32,
    /// Input delay in ticks for lockstep-style command execution.
    #[serde(default = "default_input_delay_ticks")]
    pub input_delay_ticks: u32,
}

impl Default for GameplayConfig {
    fn default() -> Self {
        Self {
            sim_tick_hz: default_sim_tick_hz(),
            input_delay_ticks: default_input_delay_ticks(),
        }
    }
}

fn default_width() -> u32 {
    1024
}

fn default_height() -> u32 {
    768
}

fn default_true() -> bool {
    true
}

fn default_sim_tick_hz() -> u32 {
    15
}

fn default_input_delay_ticks() -> u32 {
    2
}

impl GameConfig {
    /// Load configuration from config.toml in the current working directory.
    ///
    /// Returns a descriptive error if the file is missing or malformed.
    pub fn load() -> Result<Self> {
        Self::load_from(Path::new(CONFIG_FILE_NAME))
    }

    /// Load configuration from a specific file path.
    ///
    /// Useful for testing or when config is stored in a non-default location.
    pub fn load_from(path: &Path) -> Result<Self> {
        let contents: String = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: GameConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        log::info!("Loaded config from {}", path.display());
        log::info!("RA2 directory: {}", config.paths.ra2_dir.display());

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_config() {
        let toml_str = r#"
[paths]
ra2_dir = "C:/Westwood/RA2"
"#;
        let config: GameConfig = toml::from_str(toml_str).expect("Failed to parse test config");
        assert_eq!(config.graphics.width, 1024);
        assert_eq!(config.graphics.height, 768);
        assert!(config.graphics.vsync);
        assert!(!config.graphics.upscale);
        assert_eq!(config.gameplay.sim_tick_hz, 15);
        assert_eq!(config.gameplay.input_delay_ticks, 2);
    }
}
