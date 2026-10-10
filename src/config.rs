//! User-facing settings, loaded once from a TOML file next to this DLL.
//!
//! No setting names a resolution. The fixes read the size of the viewport
//! each movie is drawn into, every time it is drawn.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

use crate::utils::{log, log_warn};

const FILE_NAME: &str = "red_dead_redemption_fix.toml";

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub super_enable: bool,
    pub hud: HudConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self { super_enable: true, hud: HudConfig::default() }
    }
}

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct HudConfig {
    /// Keeps the HUD inside a centered 16:9 region on screens wider than
    /// 16:9: the Flash movies
    /// ([`crate::fixes::hud::center_movies`]),
    /// the text-size options
    /// ([`crate::fixes::hud::follow_text_scale`]),
    /// the minimap ring
    /// ([`crate::fixes::radar::center_radar`]).
    pub enable: bool,
}

impl Default for HudConfig {
    fn default() -> Self {
        Self { enable: true }
    }
}

/// Reads `red_dead_redemption_fix.toml` from the same directory as
/// `this_module`, then logs whichever config ends up in effect, regardless of
/// which path below produced it. That includes the disabled-mod case:
/// otherwise, disabling the mod would be the one path that never shows what
/// the file actually contained.
pub fn load(this_module: HMODULE) -> Config {
    let config = load_inner(this_module);
    log(&format!("config: {config:?}"));
    config
}

/// `this_module` must be *this DLL's own* handle (`DllMain`'s first
/// parameter), not the game's. A relative filename would depend on the
/// process's current working directory -- for a launched game, that's its
/// install root, not the `scripts` folder this DLL and its config actually
/// live in.
///
/// Falls back to defaults on any error -- a missing or malformed config should
/// degrade gracefully, not stop every fix in the mod from applying.
fn load_inner(this_module: HMODULE) -> Config {
    let Some(dir) = module_dir(this_module) else {
        log_warn("config: could not resolve this DLL's own directory, using defaults");
        return Config::default();
    };
    let path = dir.join(FILE_NAME);

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            log(&format!("config: {} not found ({e}), using defaults", path.display()));
            return Config::default();
        }
    };

    match toml::from_str(&text) {
        Ok(config) => {
            log(&format!("config: loaded {}", path.display()));
            config
        }
        Err(e) => {
            log_warn(&format!("config: {} failed to parse ({e}), using defaults", path.display()));
            Config::default()
        }
    }
}

/// The directory holding `module`'s file.
pub fn module_dir(module: HMODULE) -> Option<PathBuf> {
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    if len == 0 {
        return None;
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
        .parent()
        .map(|p| p.to_path_buf())
}
