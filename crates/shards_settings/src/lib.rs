//! # shards_settings — player settings (E8.6)
//!
//! What a player sets in a settings screen: **graphics**, **audio**, **controls** and
//! **accessibility**, kept between runs in `settings.toml` by Bevy's own settings framework
//! (`bevy::settings`, one TOML table per group), and changed from outside by the `settings`
//! and `settings_set` tools.
//!
//! ```toml
//! # ~/.config/shards-my-game/settings.toml
//! [accessibility]
//! camera_shake = 0.3
//! flashes = "reduced"
//! fov_deg = 90.0
//!
//! [controls.bindings]
//! "gameplay.jump" = ["Key(KeyJ)", "GamepadButton(South)"]
//! ```
//!
//! - **Groups** ([`groups`]): [`GraphicsSettings`], [`AudioSettings`], [`ControlSettings`],
//!   [`AccessibilitySettings`]. Plain resources of bools, numbers and strings, read with clamping
//!   accessors (`fov()`, [`AccessibilitySettings::flash_level`]) because a hand-edited file may
//!   say anything. A game adds its own with [`SettingsAppExt::add_settings_group`]; a group that
//!   TOML cannot hold (a `u64`, a map keyed by something else than `String`) is refused at
//!   registration, naming the field ([`check_toml_safe`]), and so is one that asks for a file of
//!   its own (`#[settings_group(file = "...")]`): the engine looks after `settings.toml` only.
//! - **Applying**: each module applies the groups it owns, outside the simulation: this crate
//!   puts [`GraphicsSettings`] on the primary window ([`apply_graphics`]); `shards_motion`
//!   turns [`ControlSettings`] into the input map (and `rebind` writes back), and
//!   [`AccessibilitySettings`] and the look speed into the camera; `shards_action` draws
//!   attack warnings by [`AccessibilitySettings::flash_level`]; `shards_audio` plays each bus at
//!   its volume ([`AudioSettings::volume`]: `master` times the bus).
//! - **Per machine, never simulated**: settings are not in snapshots, replays or the replay
//!   hash, and a host does not replicate them. `settings_set` is a control tool, so replays do
//!   not record it either; a change is journaled (`settings: accessibility.camera_shake 1 → 0.3`).
//! - **Persistence** ([`store`]): Bevy's plugin reads the file once, when it is added, and
//!   writes when told to. `shards::run` adds it ([`install`]) **after** the game's builder, because it scans
//!   the type registry once, and only when `EngineConfig.settings` asks for it: `--settings-dir DIR`
//!   keeps `DIR/settings.toml`, the default is the platform's settings folder under
//!   [`SettingsAppId`], `--no-settings`, tests, `--verify` and a stand's clients keep nothing (the
//!   groups are still there, as defaults), and so does a headless game unless `--settings-dir` is
//!   given. Changes are written a second after the last one ([`save_soon`]) and when the game
//!   exits.

mod file;
pub mod groups;
mod recover;
mod registry;
pub mod store;
mod tools;
pub mod toml_safe;
mod window;

use ai_core::AiAppExt;
use bevy::prelude::*;
use serde_json::{json, Value};
use shards_core::EngineAppExt;

pub use bevy::settings::SettingsGroup;
pub use groups::{AccessibilitySettings, AudioSettings, ControlSettings, FlashLevel, GraphicsSettings, ShadowBudget, ShadowQuality};
pub use registry::{watch_settings, SettingsAppExt, SettingsChanged, SettingsRegistry};
pub use file::{stored_entries, watch_store};
pub use store::{flush, flush_checked, install, save_soon, save_soon_in, Installed, SettingsAppId, SettingsStorage};
pub use tools::{SettingsArgs, SettingsSetArgs};
pub use toml_safe::check_toml_safe;
pub use window::apply_graphics;

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The types a game module uses most.
pub mod prelude {
    // `ReflectSettingsGroup` is what `#[reflect(SettingsGroup)]` on a group names.
    pub use crate::{
        AccessibilitySettings, AudioSettings, ControlSettings, FlashLevel, GraphicsSettings, SettingsAppExt, SettingsAppId, SettingsGroup,
    };
    pub use bevy::settings::ReflectSettingsGroup;

    // Bevy's `SettingsGroup` derive names this crate when the game has no `bevy` dependency of its own.
    pub use bevy::settings as bevy_settings;
}

/// The engine's four groups, the registry, the tools and the exit hook that saves. Needs
/// `AiCorePlugin` first; `shards::EnginePlugins` adds it, before the modules that apply a
/// group. Bevy's own settings plugin is not added here: [`install`] adds it after the game's
/// builder returned.
pub struct ShardsSettingsPlugin;

impl Plugin for ShardsSettingsPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<ai_core::AiCorePlugin>(),
            "ShardsSettingsPlugin needs AiCorePlugin added first (shards::EnginePlugins adds both)"
        );
        app.init_resource::<SettingsRegistry>()
            .add_settings_group::<GraphicsSettings>()
            .add_settings_group::<AudioSettings>()
            .add_settings_group::<ControlSettings>()
            .add_settings_group::<AccessibilitySettings>()
            .add_settings_check::<GraphicsSettings>(GraphicsSettings::check)
            .add_settings_check::<AudioSettings>(AudioSettings::check)
            .add_settings_check::<ControlSettings>(ControlSettings::check)
            .add_settings_check::<AccessibilitySettings>(AccessibilitySettings::check)
            .add_engine_module("settings", VERSION)
            .add_ai_status("settings", status)
            // The last thing a game does: what the player changed is on disk.
            .add_exit_hook("settings", flush)
            .add_systems(Update, apply_graphics)
            .add_systems(Last, (watch_settings, watch_store).chain());
        tools::register(app.world_mut());
    }

    fn finish(&self, app: &mut App) {
        // For an app that never starts a store (a test): what the game built is what a reset goes
        // back to. `install` took them already where there is one, before the file was read.
        registry::capture_defaults(app.world_mut());
    }
}

/// `ai/status` → `settings: {store, groups}`.
fn status(world: &World) -> Value {
    let file = world.get_resource::<SettingsStorage>().map(|s| s.file.as_ref().map(|f| f.display().to_string()));
    let groups = world.get_resource::<SettingsRegistry>().map(SettingsRegistry::names).unwrap_or_default();
    json!({"store": file, "groups": groups})
}
