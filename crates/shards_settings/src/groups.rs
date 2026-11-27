//! The engine's settings groups: what a player sets in a settings screen.
//!
//! Each group is a Bevy resource that Bevy's settings framework loads from and saves to
//! `settings.toml` (one TOML table per group). They hold only what TOML holds without
//! surprises: booleans, numbers, strings, string-keyed maps and lists. Choices are strings
//! (`flashes = "reduced"`) with a typed reading next to them ([`FlashLevel`],
//! [`GraphicsSettings::shadow_quality`]), because a hand-edited file may say anything: every
//! reading clamps or falls back, nothing relies on the file being valid.
//!
//! Settings are per machine and never part of the simulation: they are not in snapshots, not
//! in replays and not replicated to other players.

use bevy::prelude::*;
use bevy::settings::{ReflectSettingsGroup, SettingsGroup};

/// How flashing and flickering effects (the pulse of an attack warning) are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlashLevel {
    /// As designed.
    #[default]
    Full,
    /// Slower and shallower: at most 2 pulses a second, half the depth.
    Reduced,
    /// Steady: nothing flickers.
    Off,
}

impl FlashLevel {
    /// The values `AccessibilitySettings::flashes` takes.
    pub const NAMES: [&'static str; 3] = ["full", "reduced", "off"];

    /// Reads `full`, `reduced` or `off` (any case).
    pub fn parse(text: &str) -> Option<FlashLevel> {
        match text.trim().to_ascii_lowercase().as_str() {
            "full" => Some(FlashLevel::Full),
            "reduced" => Some(FlashLevel::Reduced),
            "off" => Some(FlashLevel::Off),
            _ => None,
        }
    }

    /// How the level is written in the file.
    pub fn name(self) -> &'static str {
        match self {
            FlashLevel::Full => "full",
            FlashLevel::Reduced => "reduced",
            FlashLevel::Off => "off",
        }
    }
}

/// Shadow quality, from `GraphicsSettings::shadows`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShadowQuality {
    Off,
    Low,
    Medium,
    #[default]
    High,
}

impl ShadowQuality {
    /// The values `GraphicsSettings::shadows` takes.
    pub const NAMES: [&'static str; 4] = ["off", "low", "medium", "high"];

    /// Reads `off`, `low`, `medium` or `high` (any case).
    pub fn parse(text: &str) -> Option<ShadowQuality> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(ShadowQuality::Off),
            "low" => Some(ShadowQuality::Low),
            "medium" => Some(ShadowQuality::Medium),
            "high" => Some(ShadowQuality::High),
            _ => None,
        }
    }
}

/// The most lights from content that may cast shadows at once ([`GraphicsSettings::shadow_lights`]).
pub const MAX_SHADOW_LIGHTS: u32 = 64;

/// The most shadow views the content lights may take at once ([`GraphicsSettings::shadow_views`]): 64 cube maps.
pub const MAX_SHADOW_VIEWS: u32 = 384;

/// What the shadow budget of the content lights counts ([`GraphicsSettings::shadow_budget`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowBudget {
    /// So many lights, whatever they cost (`graphics.shadow_lights`).
    Lights(usize),
    /// So many shadow views (`graphics.shadow_views`): a point light takes 6 (a cube), a spot 1.
    Views(usize),
}

impl ShadowBudget {
    /// The number, whatever it counts.
    pub fn amount(self) -> usize {
        match self {
            ShadowBudget::Lights(n) | ShadowBudget::Views(n) => n,
        }
    }
}

/// Window and rendering. Applied to the primary window by [`apply_graphics`](crate::apply_graphics)
/// while there is one; a headless game only keeps the values.
#[derive(Resource, Reflect, SettingsGroup, Clone, Debug, PartialEq)]
#[reflect(Resource, SettingsGroup, Default)]
#[settings_group(group = "graphics")]
pub struct GraphicsSettings {
    /// Borderless fullscreen on the current monitor.
    pub fullscreen: bool,
    /// Wait for the display's refresh (no tearing) instead of drawing as fast as possible.
    pub vsync: bool,
    /// Window size when not fullscreen: `auto` keeps the game's own, `1920x1080` sets it
    /// (logical pixels, 320x200 to 16384x16384).
    pub resolution: String,
    /// The resolution the 3D world is drawn at, as a multiple of the window's pixels (0.5 to 2:
    /// below 1 is cheaper and softer, above 1 supersamples). The picture is scaled to the window; the
    /// UI and the HUD stay at the window's own resolution. The `shards` crate applies it
    /// ([`GraphicsSettings::scale`]).
    pub render_scale: f64,
    /// `off`, `low`, `medium` or `high`. The game's lights read it ([`GraphicsSettings::shadow_quality`]).
    pub shadows: String,
    /// How many lights from content (the `light` kind) cast shadows at once, the ones nearest the camera first (0 to 64,
    /// [`GraphicsSettings::shadow_light_budget`]): a light that wants shadows and is farther than that many has none.
    /// Under `shadows = "off"` none does. A cube shadow map is six passes, so this is the knob for a weak GPU.
    pub shadow_lights: u32,
    /// The same budget counted in shadow views instead of lights (1 to 384; 0, the default, counts lights by
    /// `shadow_lights`): a point light's shadow is a cube of 6 views, a spot's is 1, and each view is a pass the render
    /// thread prepares, so twelve views shadow twelve spots or two points. The nearest lights that ask take theirs first;
    /// one that does not fit in what is left has none, and a farther one that fits does. When it is above 0,
    /// `shadow_lights` is not read; `shadows = "off"` is still none ([`GraphicsSettings::shadow_budget`]).
    pub shadow_views: u32,
    /// How much of the foliage and small props the scatter module strews (0 none to 1 all of it,
    /// [`GraphicsSettings::foliage_density`]): fewer instances of the same scatter, never others.
    pub foliage: f64,
    /// Draw bodies between their last two ticks (smooth at any refresh rate) instead of at the last
    /// tick's pose (60 steps a second). Presentation only: the simulation, replays and saves do not
    /// change. Off is the picture without interpolation, for an A/B.
    pub interpolation: bool,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            fullscreen: false,
            vsync: true,
            resolution: "auto".into(),
            render_scale: 1.0,
            shadows: "high".into(),
            shadow_lights: 4,
            shadow_views: 0,
            foliage: 1.0,
            interpolation: true,
        }
    }
}

impl GraphicsSettings {
    /// The window size `resolution` asks for; `None` for `auto` or anything that is not
    /// `<width>x<height>` within 320x200 to 16384x16384.
    pub fn window_size(&self) -> Option<(u32, u32)> {
        let (w, h) = self.resolution.trim().split_once(['x', 'X'])?;
        let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
        ((320..=16384).contains(&w) && (200..=16384).contains(&h)).then_some((w, h))
    }

    /// `render_scale` within 0.5 to 2 (a NaN or a hand-edited 40 does no harm).
    pub fn scale(&self) -> f32 {
        if self.render_scale.is_finite() { self.render_scale.clamp(0.5, 2.0) as f32 } else { 1.0 }
    }

    /// `foliage` within 0 to 1 (a NaN keeps it all, a hand-edited 3 is all of it).
    pub fn foliage_density(&self) -> f32 {
        if self.foliage.is_finite() { self.foliage.clamp(0.0, 1.0) as f32 } else { 1.0 }
    }

    /// `shadows`, the default (`high`) when the file says something else.
    pub fn shadow_quality(&self) -> ShadowQuality {
        ShadowQuality::parse(&self.shadows).unwrap_or_default()
    }

    /// How many lights from content may cast shadows now: `shadow_lights` within 0 to 64, and none under `shadows = "off"`.
    pub fn shadow_light_budget(&self) -> usize {
        if self.shadow_quality() == ShadowQuality::Off { 0 } else { self.shadow_lights.min(MAX_SHADOW_LIGHTS) as usize }
    }

    /// The shadow budget of the content lights now: `shadow_views` views when it is above 0 (at most 384), else
    /// `shadow_lights` lights ([`Self::shadow_light_budget`]); nothing under `shadows = "off"`.
    pub fn shadow_budget(&self) -> ShadowBudget {
        if self.shadow_views == 0 {
            return ShadowBudget::Lights(self.shadow_light_budget());
        }
        let views = if self.shadow_quality() == ShadowQuality::Off { 0 } else { self.shadow_views.min(MAX_SHADOW_VIEWS) as usize };
        ShadowBudget::Views(views)
    }

    /// Why these values are not allowed, if they are not (what `settings_set` refuses).
    pub fn check(&self) -> Result<(), String> {
        if self.resolution.trim() != "auto" && self.window_size().is_none() {
            return Err(format!(
                "graphics.resolution `{}` is not `auto` or `<width>x<height>` (320x200 to 16384x16384), e.g. `1920x1080`",
                self.resolution
            ));
        }
        range("graphics.render_scale", self.render_scale, 0.5, 2.0)?;
        range("graphics.foliage", self.foliage, 0.0, 1.0)?;
        if self.shadow_lights > MAX_SHADOW_LIGHTS {
            return Err(format!("graphics.shadow_lights {} is out of range: use a whole number from 0 to {MAX_SHADOW_LIGHTS}", self.shadow_lights));
        }
        if self.shadow_views > MAX_SHADOW_VIEWS {
            return Err(format!(
                "graphics.shadow_views {} is out of range: use a whole number from 0 (count lights by shadow_lights) to {MAX_SHADOW_VIEWS}",
                self.shadow_views
            ));
        }
        if ShadowQuality::parse(&self.shadows).is_none() {
            return Err(format!("graphics.shadows `{}` is not one of {}", self.shadows, ShadowQuality::NAMES.join(", ")));
        }
        Ok(())
    }
}

/// Volumes, 0 (silent) to 1 (full), of the audio module's buses; `master` scales the others.
#[derive(Resource, Reflect, SettingsGroup, Clone, Debug, PartialEq)]
#[reflect(Resource, SettingsGroup, Default)]
#[settings_group(group = "audio")]
pub struct AudioSettings {
    pub master: f64,
    pub music: f64,
    pub sfx: f64,
    pub voice: f64,
    /// Menu sounds.
    pub ui: f64,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { master: 1.0, music: 0.8, sfx: 1.0, voice: 1.0, ui: 1.0 }
    }
}

impl AudioSettings {
    /// The volume of `channel` (`music`, `sfx`, `voice`, `ui`; anything else is the master
    /// alone) with the master applied, each clamped to 0..1.
    pub fn volume(&self, channel: &str) -> f32 {
        let unit = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) as f32 } else { 0.0 };
        let part = match channel {
            "music" => self.music,
            "sfx" => self.sfx,
            "voice" => self.voice,
            "ui" => self.ui,
            _ => 1.0,
        };
        unit(self.master) * unit(part)
    }

    /// Why these values are not allowed, if they are not.
    pub fn check(&self) -> Result<(), String> {
        for (name, value) in [("master", self.master), ("music", self.music), ("sfx", self.sfx), ("voice", self.voice), ("ui", self.ui)] {
            range(&format!("audio.{name}"), value, 0.0, 1.0)?;
        }
        Ok(())
    }
}

/// Controls the player changed (E2's `rebind`), as strings.
///
/// `bindings` and `hold_or_toggle` hold only what differs from the game's default controls,
/// so new default actions still reach an old file. The motion module turns them into the
/// input map (and back when `rebind` changes it).
#[derive(Resource, Reflect, SettingsGroup, Clone, Debug, PartialEq)]
#[reflect(Resource, SettingsGroup, Default)]
#[settings_group(group = "controls")]
pub struct ControlSettings {
    /// `"context.action"` to its bindings in slot order, written like a content file
    /// (`"gameplay.jump" = ["Key(KeyJ)", "GamepadButton(South)"]`). An empty list unbinds.
    pub bindings: std::collections::BTreeMap<String, Vec<String>>,
    /// `"context.action"` to `press`, `hold`, `toggle` or `press_hold`.
    pub hold_or_toggle: std::collections::BTreeMap<String, String>,
    /// Look speed multiplier (0.05 to 5).
    pub mouse_sensitivity: f64,
    /// Look speed multiplier while the game says the player aims (0.05 to 5): a ranged attack
    /// or a zoom turns the view by this instead of `mouse_sensitivity` (K1).
    pub aim_sensitivity: f64,
    /// Free look turns back behind the player's body when the look is left alone and the body
    /// walks (K2). Off by default.
    pub camera_auto_centre: bool,
    /// Looking up moves the camera up instead of down.
    pub invert_y: bool,
    /// Attacks may correct their direction toward the lock-on target (the attack assist). Off:
    /// the controls hold `assist_off` for the player's body, and an attack with an assist does not
    /// track the target.
    pub attack_assist: bool,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            bindings: Default::default(),
            hold_or_toggle: Default::default(),
            mouse_sensitivity: 1.0,
            aim_sensitivity: 1.0,
            camera_auto_centre: false,
            invert_y: false,
            attack_assist: true,
        }
    }
}

impl ControlSettings {
    /// The look speed multiplier within 0.05 to 5.
    pub fn sensitivity(&self) -> f32 {
        if self.mouse_sensitivity.is_finite() { self.mouse_sensitivity.clamp(0.05, 5.0) as f32 } else { 1.0 }
    }

    /// The look speed multiplier while aiming, within 0.05 to 5.
    pub fn aim_sensitivity(&self) -> f32 {
        if self.aim_sensitivity.is_finite() { self.aim_sensitivity.clamp(0.05, 5.0) as f32 } else { 1.0 }
    }

    /// Why these values are not allowed, if they are not.
    pub fn check(&self) -> Result<(), String> {
        range("controls.mouse_sensitivity", self.mouse_sensitivity, 0.05, 5.0)?;
        range("controls.aim_sensitivity", self.aim_sensitivity, 0.05, 5.0)?;
        for (key, mode) in &self.hold_or_toggle {
            if !matches!(mode.trim().to_ascii_lowercase().as_str(), "press" | "hold" | "toggle" | "press_hold") {
                return Err(format!("controls.hold_or_toggle.{key} `{mode}` is not one of press, hold, toggle, press_hold"));
            }
        }
        for key in self.bindings.keys().chain(self.hold_or_toggle.keys()) {
            if !key.contains('.') {
                return Err(format!("`{key}` is not `context.action` (for example `gameplay.jump`)"));
            }
        }
        Ok(())
    }
}

/// Things that make the game easier on the eyes and hands.
#[derive(Resource, Reflect, SettingsGroup, Clone, Debug, PartialEq)]
#[reflect(Resource, SettingsGroup, Default)]
#[settings_group(group = "accessibility")]
pub struct AccessibilitySettings {
    /// How hard hits shake the camera, 0 (never) to 1 (as designed).
    pub camera_shake: f64,
    /// Motion blur strength, 0 (none) to 1. Applied to cameras that carry Bevy's `MotionBlur`.
    pub motion_blur: f64,
    /// Flashing and flickering: `full`, `reduced` or `off` ([`FlashLevel`]).
    pub flashes: String,
    /// Text size multiplier (0.5 to 3), for the game's UI.
    pub text_scale: f64,
    /// Vertical field of view in degrees (30 to 120).
    pub fov_deg: f64,
    /// Every held button (`block`, `sprint`, ...) works as a toggle: press once to hold, again
    /// to let go.
    pub hold_to_toggle: bool,
    /// How long a hit stop (the brief hold of the picture, and of the clock when playing alone,
    /// when a blow lands) lasts, as a share of what the game designed: 0 (never) to 1.
    pub hit_stop: f64,
    /// A hit stop only for the blows the local player takes part in, not for the fights around them.
    pub hit_stop_local_only: bool,
    /// Lines for the sounds that have them (a voice's words), shown by the game's UI.
    pub subtitles: bool,
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            camera_shake: 1.0,
            motion_blur: 0.5,
            flashes: "full".into(),
            text_scale: 1.0,
            fov_deg: 70.0,
            hold_to_toggle: false,
            hit_stop: 1.0,
            hit_stop_local_only: false,
            subtitles: true,
        }
    }
}

impl AccessibilitySettings {
    /// `camera_shake` within 0..1.
    pub fn shake(&self) -> f32 {
        unit(self.camera_shake, 1.0)
    }

    /// `hit_stop` within 0..1 (0: no hit stop at all).
    pub fn hit_stop_scale(&self) -> f32 {
        unit(self.hit_stop, 1.0)
    }

    /// `motion_blur` within 0..1.
    pub fn blur(&self) -> f32 {
        unit(self.motion_blur, 0.0)
    }

    /// `flashes`, [`FlashLevel::Full`] when the file says something else.
    pub fn flash_level(&self) -> FlashLevel {
        FlashLevel::parse(&self.flashes).unwrap_or_default()
    }

    /// `text_scale` within 0.5 to 3.
    pub fn text(&self) -> f32 {
        if self.text_scale.is_finite() { self.text_scale.clamp(0.5, 3.0) as f32 } else { 1.0 }
    }

    /// `fov_deg` within 30 to 120.
    pub fn fov(&self) -> f32 {
        if self.fov_deg.is_finite() { self.fov_deg.clamp(30.0, 120.0) as f32 } else { 70.0 }
    }

    /// Why these values are not allowed, if they are not.
    pub fn check(&self) -> Result<(), String> {
        range("accessibility.camera_shake", self.camera_shake, 0.0, 1.0)?;
        range("accessibility.motion_blur", self.motion_blur, 0.0, 1.0)?;
        range("accessibility.hit_stop", self.hit_stop, 0.0, 1.0)?;
        range("accessibility.text_scale", self.text_scale, 0.5, 3.0)?;
        range("accessibility.fov_deg", self.fov_deg, 30.0, 120.0)?;
        if FlashLevel::parse(&self.flashes).is_none() {
            return Err(format!("accessibility.flashes `{}` is not one of {}", self.flashes, FlashLevel::NAMES.join(", ")));
        }
        Ok(())
    }
}

fn unit(value: f64, fallback: f32) -> f32 {
    if value.is_finite() { value.clamp(0.0, 1.0) as f32 } else { fallback }
}

/// `Ok` when `value` is a number within `lo..=hi`.
fn range(name: &str, value: f64, lo: f64, hi: f64) -> Result<(), String> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} {value} is out of range: use a number from {lo} to {hi}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_clamp_whatever_the_file_says() {
        let g = GraphicsSettings { resolution: "1920x1080".into(), render_scale: 40.0, shadows: "ultra".into(), ..default() };
        assert_eq!(g.window_size(), Some((1920, 1080)));
        assert_eq!(g.scale(), 2.0);
        assert_eq!(g.shadow_quality(), ShadowQuality::High, "an unknown word is the default");
        assert_eq!(GraphicsSettings { resolution: "10x10".into(), ..default() }.window_size(), None);
        assert_eq!(GraphicsSettings { resolution: "auto".into(), ..default() }.window_size(), None);
        assert_eq!(GraphicsSettings::default().shadow_light_budget(), 4, "four shadow lights by default");
        assert_eq!(GraphicsSettings { shadow_lights: 1000, ..default() }.shadow_light_budget(), 64, "a hand-edited 1000 is the most");
        assert_eq!(GraphicsSettings { shadows: "off".into(), ..default() }.shadow_light_budget(), 0, "no shadows is no shadow lights");
        assert_eq!(GraphicsSettings::default().shadow_budget(), ShadowBudget::Lights(4), "lights are counted until views are set");
        assert_eq!(GraphicsSettings { shadow_views: 12, shadow_lights: 1, ..default() }.shadow_budget(), ShadowBudget::Views(12), "views replace lights");
        assert_eq!(GraphicsSettings { shadow_views: 5000, ..default() }.shadow_budget(), ShadowBudget::Views(384));
        assert_eq!(GraphicsSettings { shadow_views: 12, shadows: "off".into(), ..default() }.shadow_budget(), ShadowBudget::Views(0));
        let a = AccessibilitySettings { camera_shake: f64::NAN, fov_deg: 400.0, flashes: "strobe".into(), ..default() };
        assert_eq!((a.shake(), a.fov(), a.flash_level()), (1.0, 120.0, FlashLevel::Full));
        let audio = AudioSettings { master: 0.5, music: 0.5, ..default() };
        assert_eq!(audio.volume("music"), 0.25);
        assert_eq!(audio.volume("other"), 0.5);
    }

    #[test]
    fn checks_name_the_setting_and_what_is_allowed() {
        let bad = AccessibilitySettings { fov_deg: 200.0, ..default() }.check().unwrap_err();
        assert!(bad.contains("accessibility.fov_deg 200") && bad.contains("30 to 120"), "{bad}");
        let bad = AccessibilitySettings { flashes: "blink".into(), ..default() }.check().unwrap_err();
        assert!(bad.contains("full, reduced, off"), "{bad}");
        let bad = GraphicsSettings { resolution: "wide".into(), ..default() }.check().unwrap_err();
        assert!(bad.contains("1920x1080"), "{bad}");
        let bad = GraphicsSettings { shadow_lights: 65, ..default() }.check().unwrap_err();
        assert!(bad.contains("graphics.shadow_lights 65") && bad.contains("0 to 64"), "{bad}");
        let bad = GraphicsSettings { shadow_views: 385, ..default() }.check().unwrap_err();
        assert!(bad.contains("graphics.shadow_views 385") && bad.contains("to 384"), "{bad}");
        let mut controls = ControlSettings::default();
        controls.hold_or_toggle.insert("gameplay.sprint".into(), "tap".into());
        assert!(controls.check().unwrap_err().contains("press, hold, toggle"));
        for ok in [GraphicsSettings::default().check(), AudioSettings::default().check(), ControlSettings::default().check(), AccessibilitySettings::default().check()] {
            assert_eq!(ok, Ok(()));
        }
    }
}
