// SPDX-License-Identifier: GPL-3.0-only
//! Settings specific to the drop-down behaviour (cosmic-term's own settings such as
//! font and colors are read from cosmic-term's config, see `config.rs`).

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const ID: &str = "dev.norman.CosmicQuakeTerm";
pub const VERSION: u64 = 1;

pub const DEFAULT_ANIMATION_MS: u16 = 300;
pub const MIN_ANIMATION_MS: u16 = 50;
pub const MAX_ANIMATION_MS: u16 = 2000;
/// Range of the persisted zoom (steps relative to the configured font size).
pub const MIN_ZOOM: i8 = -8;
pub const MAX_ZOOM: i8 = 30;
pub const MIN_PERCENT: u16 = 10;
pub const MAX_PERCENT: u16 = 100;

/// Stored in `~/.config/cosmic/dev.norman.CosmicQuakeTerm/v1/`.
#[derive(Clone, CosmicConfigEntry, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuakeConfig {
    /// Height as a percentage of the output height.
    pub height_percent: u16,
    /// Width as a percentage of the output width; the terminal is centered.
    pub width_percent: u16,
    /// Slide animation duration in milliseconds; 0 turns the animation off.
    pub animation_ms: u16,
    /// Name of the output to show the terminal on; empty = the active output.
    pub monitor: String,
    /// New tabs and splits start in the working directory of the focused terminal.
    pub inherit_cwd: bool,
    /// Font zoom in steps relative to the font size from cosmic-term's settings.
    pub zoom: i8,
}

impl Default for QuakeConfig {
    fn default() -> Self {
        Self {
            height_percent: 40,
            width_percent: 100,
            animation_ms: DEFAULT_ANIMATION_MS,
            monitor: String::new(),
            inherit_cwd: true,
            zoom: 0,
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

/// Values given on the command line; `None` leaves the stored value untouched.
#[derive(Default)]
pub struct Update {
    pub height: Option<u16>,
    pub width: Option<u16>,
    pub animation_ms: Option<u16>,
    pub monitor: Option<String>,
    pub inherit_cwd: Option<bool>,
}

/// Parse the argument of `--animation`: `on`, `off` or a duration in milliseconds.
pub fn parse_animation(value: &str) -> Option<u16> {
    match value {
        "on" => Some(DEFAULT_ANIMATION_MS),
        "off" => Some(0),
        ms => ms
            .parse::<u32>()
            .ok()
            .map(|ms| ms.min(u32::from(MAX_ANIMATION_MS)) as u16),
    }
}

/// Persist values from the command line; a running daemon picks them up live.
pub fn save(update: Update) -> Result<QuakeConfig, String> {
    let (handler, mut config) = load();
    let handler = handler.ok_or("cannot open the config store")?;
    if let Some(height) = update.height {
        config
            .set_height_percent(&handler, clamp_percent(height))
            .map_err(|e| e.to_string())?;
    }
    if let Some(width) = update.width {
        config
            .set_width_percent(&handler, clamp_percent(width))
            .map_err(|e| e.to_string())?;
    }
    if let Some(ms) = update.animation_ms {
        config
            .set_animation_ms(&handler, ms.min(MAX_ANIMATION_MS))
            .map_err(|e| e.to_string())?;
    }
    if let Some(monitor) = update.monitor {
        config
            .set_monitor(&handler, monitor)
            .map_err(|e| e.to_string())?;
    }
    if let Some(inherit) = update.inherit_cwd {
        config
            .set_inherit_cwd(&handler, inherit)
            .map_err(|e| e.to_string())?;
    }
    Ok(config)
}

/// Parse `on` / `off` (also accepts `true`/`false`, `yes`/`no`, `1`/`0`).
pub fn parse_switch(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Some(true),
        "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
}
