// SPDX-License-Identifier: GPL-3.0-only
//! Settings specific to the drop-down behaviour (cosmic-term's own settings such as
//! font and colors are read from cosmic-term's config, see `config.rs`).

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const ID: &str = "dev.norman.CosmicQuakeTerm";
pub const VERSION: u64 = 1;

pub const MIN_PERCENT: u16 = 10;
pub const MAX_PERCENT: u16 = 100;

/// Stored in `~/.config/cosmic/dev.norman.CosmicQuakeTerm/v1/`.
#[derive(Clone, CosmicConfigEntry, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuakeConfig {
    /// Height as a percentage of the output height.
    pub height_percent: u16,
    /// Width as a percentage of the output width; the terminal is centered.
    pub width_percent: u16,
}

impl Default for QuakeConfig {
    fn default() -> Self {
        Self {
            height_percent: 40,
            width_percent: 100,
        }
    }
}

pub fn clamp_percent(value: u16) -> u16 {
    value.clamp(MIN_PERCENT, MAX_PERCENT)
}

impl QuakeConfig {
    /// Pixel size of the terminal for an output of `output` logical pixels.
    /// Returns `None` for the width when it should simply span the whole output.
    pub fn target_size(&self, output: (f32, f32)) -> (Option<u32>, u32) {
        let height = (output.1 * f32::from(clamp_percent(self.height_percent)) / 100.0).round();
        let width_percent = clamp_percent(self.width_percent);
        let width = (width_percent < MAX_PERCENT)
            .then(|| (output.0 * f32::from(width_percent) / 100.0).round() as u32);
        (width, (height as u32).max(1))
    }
}

/// Load the config, falling back to defaults for anything missing or invalid.
pub fn load() -> (Option<cosmic_config::Config>, QuakeConfig) {
    match cosmic_config::Config::new(ID, VERSION) {
        Ok(handler) => {
            let config = match QuakeConfig::get_entry(&handler) {
                Ok(config) => config,
                Err((errors, config)) => {
                    log::info!("errors loading quake config: {errors:?}");
                    config
                }
            };
            (Some(handler), config)
        }
        Err(err) => {
            log::error!("failed to create quake config handler: {err}");
            (None, QuakeConfig::default())
        }
    }
}

/// Persist a value from the command line; a running daemon picks it up live.
pub fn save(height: Option<u16>, width: Option<u16>) -> Result<QuakeConfig, String> {
    let (handler, mut config) = load();
    let handler = handler.ok_or("cannot open the config store")?;
    if let Some(height) = height {
        config
            .set_height_percent(&handler, clamp_percent(height))
            .map_err(|e| e.to_string())?;
    }
    if let Some(width) = width {
        config
            .set_width_percent(&handler, clamp_percent(width))
            .map_err(|e| e.to_string())?;
    }
    Ok(config)
}
