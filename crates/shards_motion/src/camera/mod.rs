//! The third-person camera (E2.5): orbit, wall collision, lock-on, aim assist, shake, FOV.
//! The lock-on rules (C6) are in [`lock`]: data (content kind `lock_on`) and plain functions
//! that the camera and the action module share.
//!
//! **Presentation only.** The simulation and the AI's `observe` never read the camera:
//! what the AI sees comes from its subject's eyes, and a window with this camera plays the
//! same game as a headless run. The one thing the camera writes into the game is the
//! lock-on target it picks, as `Intent.target` between ticks (it is input, so replays
//! record it), and, with a `lock_on` definition, the `lock_switch` presses of a flick or the
//! mouse wheel. It never takes away a target anybody else set: that is gameplay's, and the
//! camera is only where the eye happens to be. With a `lock_on` definition it lets go of none
//! by itself either: the action module checks every lock on every tick.
//!
//! The math is in [`math`] as plain functions (`orbit_position`, `resolve_camera_collision`,
//! `pick_lock_target`, `next_lock_target`, `shake_offset`, `aim_assist`); the systems that
//! drive a camera entity with it need a window and live behind the `render` feature.
//!
//! ```ignore
//! // Follows the entity with `LocalPlayer`:
//! commands.spawn((Camera3d::default(), AiCamera, ThirdPersonCamera::default()));
//! // A hit shakes it (when the player's setting allows):
//! commands.trigger(AddTrauma(0.4));
//! ```

pub mod first_person;
pub mod grading;
pub mod lock;
pub mod math;
pub mod picture;
pub mod shot;
#[cfg(feature = "render")]
mod systems;
pub mod tuning;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentAppExt, ContentDb};
use shards_settings::{AccessibilitySettings, ControlSettings};

pub use math::{
    aim_assist, arm_length, arm_reach, critically_damped_step, damp, fit_room, fit_tilt, lock_framing, lock_holds, next_lock_target,
    orbit_angles, orbit_offset, orbit_position, pick_lock_target, pitch_limits, resolve_camera_collision, shake_offset,
    shake_offset_with, wrap_angle, FlickState, LockCandidate, LockView, ShakeOffset, Side,
};
pub use lock::{
    body_point, in_frustum, lock_candidates, lock_score, pick_lock, signed_bearing, sight_clear, switch_lock, switch_target, LockHooks,
    LockOnDef, LockWeights, Lockable, PickView,
};
pub use first_person::{
    heading_of, look_through, turn_look, view_rotation, BobTuning, CameraMode, EyeFrame, EyeShift, EyeState, FirstPersonCamera, FirstPersonTuning, FromMode,
    LandingTuning, RollTuning, OWN_BODY_LAYER, VIEW_MODEL_LAYER,
};
pub use grading::{grade_drawn_in_full, picture_clock, Grading, GradingBlend, GradingDef, GradingVolume};
#[cfg(feature = "render")]
pub use grading::GradingSet;
pub use picture::{AntiAliasing, BloomComposite, BloomTuning, PictureTuning, SsaoQuality, SsaoTuning, ToneMapping};
pub use tuning::{ArmTuning, AutoCentreTuning, CameraOffsets, CameraTuning, FitTuning, FlickTuning, FramingTuning, TraumaTuning};
pub use shot::{ShotDef, ShotEnded, ShotKey, ShotPoint, ShotStarted, Shots};

/// A camera that orbits a character. Put it on the camera entity; `yaw` and `pitch` are
/// where the player looks (`input` turns the move axes by `yaw`).
#[derive(Component, Reflect, Clone, Debug)]
#[reflect(Component, Default)]
pub struct ThirdPersonCamera {
    /// The character to follow. [`Entity::PLACEHOLDER`] (the default): the entity with
    /// `LocalPlayer`, whichever it is now.
    pub target: Entity,
    /// Turn about Y, radians (0: behind a character facing -Z).
    pub yaw: f32,
    /// Tilt, radians; positive looks down from above.
    pub pitch: f32,
    /// How far the view is turned up from the focus (radians) to keep a locked target in frame
    /// with the player (`FramingTuning::fit`, R4); 0 without a fit. The camera stands where `yaw`
    /// and `pitch` put it either way.
    pub tilt: f32,
    /// Distance from the focus point (m) when nothing is in the way.
    pub distance: f32,
    /// Height of the focus point above the character's feet (m).
    pub height: f32,
    /// The focus point as smoothed so far.
    pub focus: Vec3,
    /// How quickly the focus catches up with the character (1/s), unless `CameraTuning::follow_rate` says.
    pub follow_rate: f32,
    /// The arm's nominal length as smoothed so far (m; 0 until the first frame). It follows
    /// `distance` at `CameraTuning::arm.follow_rate`. A game that needs the reach of a swimming
    /// camera reads this, not the length a wall left.
    pub arm_length: f32,
    /// How far out the camera stood on the last frame (m; 0 until the first frame): `arm_length`,
    /// or less where a wall pulled it in, growing back at `CameraTuning::arm.follow_rate`.
    pub arm_reach: f32,
    /// How long the look has been left alone (real seconds): free look with no look input. Any
    /// look input, a lock, a jump of the focus and a followed body placed rather than walked (a
    /// teleport) start it over. The auto-centre (K2) waits on it.
    pub look_idle_s: f32,
    /// Where the followed body was drawn on the last frame (`None` until the first). The
    /// auto-centre (K2) measures the body's walk from it, not from the focus, which goes on
    /// catching up after the body stops.
    pub subject_seen: Option<Vec3>,
}

impl Default for ThirdPersonCamera {
    fn default() -> Self {
        Self {
            target: Entity::PLACEHOLDER,
            yaw: 0.0,
            pitch: 0.35,
            tilt: 0.0,
            distance: 5.5,
            height: 1.5,
            focus: Vec3::ZERO,
            follow_rate: 14.0,
            arm_length: 0.0,
            arm_reach: 0.0,
            look_idle_s: 0.0,
            subject_seen: None,
        }
    }
}

impl ThirdPersonCamera {
    /// A camera following `target`.
    pub fn following(target: Entity) -> Self {
        Self { target, ..default() }
    }
}

/// How the camera feels. `fov_deg`, `shake`, `shake_intensity`, `sensitivity`, `aim_sensitivity`,
/// `invert_y` and `auto_centre` are the player's choices: they follow the accessibility and control settings (E8.6, saved
/// in `settings.toml`) whenever those change. The rest is the game's.
#[derive(Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[serde(default)]
pub struct CameraSettings {
    /// Vertical field of view, degrees.
    pub fov_deg: f32,
    /// Look speed multiplier.
    pub sensitivity: f32,
    /// Look speed multiplier while the game says the player aims ([`CameraOffsets::aiming`]).
    pub aim_sensitivity: f32,
    /// Free look turns back behind the followed body when the player leaves it alone and walks
    /// (K2, [`AutoCentreTuning`]).
    pub auto_centre: bool,
    /// Looking up moves the camera up instead of down.
    pub invert_y: bool,
    /// Whether hits shake the camera.
    pub shake: bool,
    /// How hard they do (0..1).
    pub shake_intensity: f32,
    /// How much the look bends toward a target near the crosshair (0 off, 1 full).
    pub aim_assist: f32,
    /// A lock-on picks and holds targets this close (m), when the game has no `lock_on`
    /// definition (with one, its `range` decides).
    pub lock_range: f32,
    /// Half the width of the cone in front of the camera a lock-on picks from (degrees): always
    /// without a `lock_on` definition, and with one whose `frustum` is off.
    pub lock_cone_deg: f32,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self {
            fov_deg: 70.0,
            sensitivity: 1.0,
            aim_sensitivity: 1.0,
            auto_centre: false,
            invert_y: false,
            shake: true,
            shake_intensity: 1.0,
            aim_assist: 0.35,
            lock_range: 20.0,
            lock_cone_deg: 55.0,
        }
    }
}

/// Marks what a lock-on can pick: enemies and the like (the game or the action module
/// adds it, and removes it when they die, which releases the lock).
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component, Default)]
pub struct LockOnTarget;

/// Marks a lock-on target that is a point on another body (a body part, R1/R3), not a body: the
/// camera frames it at its own position (its centre), where it frames a body at its chest.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component, Default)]
pub struct LockPoint;

/// The camera's current shake: `trauma` in 0..1 decays on its own.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub struct CameraShake {
    pub trauma: f32,
}

impl CameraShake {
    /// A hit of `amount` (0..1): trauma adds up, to at most 1.
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount.max(0.0)).min(1.0);
    }
}

/// `commands.trigger(AddTrauma(0.4))`: shakes the camera if its setting is on.
#[derive(Event, Clone, Copy, Debug, PartialEq)]
pub struct AddTrauma(pub f32);

/// The camera's systems, for ordering: a game system that writes [`CameraOffsets`] runs
/// `.before(CameraSystems::Orbit)` (in `PostUpdate`) to be seen on the same frame. The set is
/// empty without the `render` feature.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraSystems {
    /// `orbit_camera`: reads the offsets, moves the camera.
    Orbit,
}

pub(crate) fn plugin(app: &mut App) {
    app.register_type::<ThirdPersonCamera>()
        .register_type::<CameraSettings>()
        .register_type::<CameraTuning>()
        .register_type::<CameraOffsets>()
        .register_type::<LockOnTarget>()
        .register_type::<LockPoint>()
        .register_type::<LockOnDef>()
        .register_type::<FirstPersonCamera>()
        .add_content::<CameraTuning>()
        .add_content::<LockOnDef>()
        .init_resource::<LockHooks>()
        .init_resource::<CameraSettings>()
        .init_resource::<CameraTuning>()
        .init_resource::<CameraOffsets>()
        .init_resource::<CameraShake>()
        .init_resource::<first_person::CameraModeOverride>()
        .add_observer(|trigger: On<AddTrauma>, settings: Res<CameraSettings>, mut shake: ResMut<CameraShake>| {
            if settings.shake {
                shake.add_trauma(trigger.event().0 * settings.shake_intensity.clamp(0.0, 1.0));
            }
        })
        .add_systems(PreUpdate, (apply_player_settings, resolve_tuning))
        .add_systems(PreUpdate, first_person::apply_mode.after(resolve_tuning));
    first_person::plugin(app);
    shot::plugin(app);
    grading::plugin(app);
    #[cfg(feature = "render")]
    systems::plugin(app);
}

/// `PreUpdate`: what the player set (E8.6) becomes how the camera feels. The field of view and
/// the shake come from the accessibility settings (`camera_shake` 0 is no shake at all, anything
/// above it is how hard), the look speeds (free and aiming), the inversion and the auto-centre from the controls.
/// A game that wants other defaults inserts its own settings resources before the settings store
/// starts.
/// Presentation only: nothing the simulation reads.
fn apply_player_settings(
    accessibility: Option<Res<AccessibilitySettings>>,
    controls: Option<Res<ControlSettings>>,
    mut camera: ResMut<CameraSettings>,
) {
    let (Some(accessibility), Some(controls)) = (accessibility, controls) else { return };
    if !accessibility.is_changed() && !controls.is_changed() {
        return;
    }
    let shake = accessibility.shake();
    let next = CameraSettings {
        fov_deg: accessibility.fov(),
        shake: shake > 0.0,
        shake_intensity: shake,
        sensitivity: controls.sensitivity(),
        aim_sensitivity: controls.aim_sensitivity(),
        auto_centre: controls.camera_auto_centre,
        invert_y: controls.invert_y,
        ..camera.clone()
    };
    if *camera != next {
        *camera = next;
    }
}

/// `PreUpdate`: the `default` definition of the `camera` kind becomes the tuning in force once
/// content is loaded and whenever it changes (a hot reload). With no such definition the
/// tuning stays what the game set from code (the code defaults, unless it changed them); a
/// definition that is removed puts the code defaults back.
fn resolve_tuning(db: Option<Res<ContentDb>>, mut tuning: ResMut<CameraTuning>, mut from_content: Local<bool>) {
    let Some(db) = db else { return };
    if !db.is_loaded() || !db.is_changed() {
        return;
    }
    match db.get::<CameraTuning>("default") {
        // A first load keeps a definition with errors in the database (hot reload refuses one):
        // an invalid tuning is not taken, so the camera keeps what it has.
        Some(def) if !def.problems().is_empty() => {}
        Some(def) => {
            *from_content = true;
            if *tuning != *def {
                *tuning = def.clone();
            }
        }
        None if *from_content => {
            *from_content = false;
            *tuning = CameraTuning::default();
        }
        None => {}
    }
}
