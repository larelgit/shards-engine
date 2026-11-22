//! # shards_motion — physics, character, input, camera
//!
//! - [`physics`]: colliders with deterministic ids, collision layers, sensor volumes
//!   (water, updrafts, ladders, triggers) and queries (`raycast`, `shapecast`, `overlaps`,
//!   `volumes_at`) on [`PhysicsWorld`], usable from systems and from `&World` (AI tools).
//! - [`controller`]: the kinematic character controller — walk, run, sprint, slopes,
//!   stairs, jump, mantle, swim, glide, ladders — driven by [`LocomotionInput`]. Its numbers are
//!   content (`locomotion`, hot-reloadable).
//! - [`intent`](MotionSystems): `Intent` → [`LocomotionInput`], the one way characters are
//!   steered.
//! - [`input`]: the input action map: keyboard, mouse and gamepad become `Intent`;
//!   contexts, press/hold/toggle, rebinding kept in the `controls` settings (`shards_settings`).
//! - [`control_lock`] (F12): a scene takes movement, actions or the freedom of the look from a body, for a reason
//!   (`lock_controls`, `ControlLocks`): a filter on `Intent` inside the tick, given back before the replay check.
//! - [`camera`]: the third-person camera (orbit, wall collision, lock-on, aim assist,
//!   shake) and the first-person one (at the eye of a body that looks, `Intent::look`, F1b) —
//!   presentation only; its field of view, shake and look speed follow the player's
//!   accessibility and control settings.
//! - use by looking (F5): [`Usable`], the `USE` layer and the pure ray [`use_target`] from the eye along the look; the
//!   press that uses what it meets is the action module's (`ActionSystems::Use`).
//! - AI tools for agents: `move_to` (with `path`), `move`, `stop`, `jump`, `sprint`,
//!   `glide`, and the perception queries `clearance` and `path_to` ([`perception`]);
//!   `input_bindings` and `rebind`; `physics_overlay` with a window.
//!
//! Everything that matters for gameplay runs in `FixedUpdate`:
//!
//! ```text
//! PhysicsSystems::Sync          start of the tick: colliders moved between ticks
//! SimSet::Input … Think         orders walk, brains decide
//! SimSet::Act
//!   MotionSystems::Lock         the control lock takes what is locked out of Intent and holds the look in its cones
//!   MotionSystems::Intent       intent_to_locomotion: Intent → LocomotionInput
//!   …                           the action machine (shards_action) adds external, locked, face
//! SimSet::Move
//!   ControllerSystems::Move     move_characters: casts against PhysicsWorld
//! PhysicsSystems::SyncAfterMove characters' new positions
//! SimSet::Resolve …             hit detection sees post-move positions
//! ```
//!
//! Keyboard, mouse and gamepad are read between ticks (`PreUpdate`) and written to
//! `Intent`, which replays record; the camera runs in `PostUpdate` and nothing in the
//! simulation or the AI's `observe` reads it. The one thing it writes is the lock-on
//! target, into `Intent.target`.
//!
//! Collision math comes from parry3d, which stays an implementation detail: the API
//! speaks glam (`Vec3`, `Quat`, `Dir3`) and our own [`Shape`].
//!
//! ```ignore
//! app.add_plugins(MotionPlugin);
//! commands.spawn((Collider::standing_box(Vec3::new(5.0, 0.5, 5.0)), Transform::default()));
//! commands.spawn(character("hero", Vec3::new(0.0, 1.0, 0.0)));
//! ```

pub mod ai;
pub mod camera;
pub mod control_lock;
pub mod controller;
mod coverage;
pub mod eye;
pub mod input;
mod intent;
mod interp;
mod invariants;
mod modifiers;
pub mod movers;
mod orders;
#[cfg(feature = "render")]
mod overlay;
pub mod perception;
pub mod physics;
mod predict;
mod tools;
mod usable;

use bevy::prelude::*;
use shards_core::EngineAppExt;

pub use invariants::{body_in_solid, launch_sane, motion_finite, BODY_IN_SOLID};

pub use ai::{describe_motion, physics_line_of_sight, MotionAi};
pub use control_lock::{
    control_line, control_lock_sane, lock_controls, unlock_controls, within_cone, ControlLock, ControlLocks, ControlsLocked, ControlsUnlocked,
    LockStash, LookLock, CONTROL_LOCK_SANE,
};
pub use camera::{CameraSettings, FirstPersonCamera, LockHooks, LockOnDef, LockOnTarget, LockPoint, ThirdPersonCamera};
pub use eye::{aim_point_of, eye_of, look_dir, params_of};
pub use controller::{
    character, CharacterController, CharacterParams, ControllerPlugin, ControllerSystems, Crouch, Crouched, Facing, Ghost, Jumped,
    JumpedFlight, LadderLetGo, Landed, Launch, SprintResume, Stood,
    LocomotionInput, Mantle, Mantled, ModeChanged, MotionMode, MotionState,
};
pub use controller::AccelModel;
pub use input::{BindMode, Binding, ControlState, ControlSystems, InputContext, InputContexts, InputMap, InputSettings};
pub use intent::{intent_to_locomotion, GlideUntilLanded, MotionSystems};
pub use interp::{adopt_visuals, shown_translation, InterpSystems, Interpolated, NotInterpolated, Pose, visual_offset};
pub use modifiers::{ModifierSystems, MoveModifiers, StanceSpeed};
pub use movers::{
    mover_go, mover_state, Blocked, Mover, MoverArrived, MoverBlocked, MoverDef, MoverInfo, MoverKey, MoverLoop, MoverPart, MoverStarted, MoverState,
    MoverSystems, MOVER_SANE,
};
pub use orders::{end_stalled_orders, OrderEnd, OrderEnded, OrderProgress, STALL_PROGRESS, STALL_SECONDS};
pub use physics::{
    current_at, edge_distance, Body, Collider, ColliderId, ColliderIds, ColliderInfo, CollisionLayers, Current, LayerMask, Penetration, PhysicsPlugin,
    PhysicsSystems, PhysicsWorld, QueryFilter, RayHit, Shape, ShapeHit, SpatialQuery, Volume, VolumeEntered,
    VolumeExited, VolumeKind, VoxelCollider, VoxelField,
};
pub use tools::TargetArg;
pub use usable::{use_point, use_probe, use_target, Usable, UseHit, UseProbe, UseTarget, DEFAULT_USE_RANGE, PROBE_RANGE};

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The types a game module uses most (`Shape`, `Body` and `Volume` stay out: games name
/// their own, import them from the crate root).
pub mod prelude {
    pub use crate::{
        character, CameraSettings, CharacterController, CharacterParams, Collider, CollisionLayers, ControlState,
        FirstPersonCamera, InputContexts, InputMap, LayerMask, LocomotionInput, LockOnTarget, MotionAi, MotionMode, MotionPlugin,
        MotionState, MotionSystems, PhysicsWorld, QueryFilter, SpatialQuery, ThirdPersonCamera,
    };
}

/// Physics, the character controller, `Intent` steering, the input map, the camera (with
/// the `render` feature) and the motion tools. Needs `AiCorePlugin` and `CorePlugin`
/// (`shards::EnginePlugins` adds all three); content kinds register with the content
/// database whether or not it is there.
pub struct MotionPlugin;

impl Plugin for MotionPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<PhysicsPlugin>() {
            app.add_plugins(PhysicsPlugin);
        }
        if !app.is_plugin_added::<ControllerPlugin>() {
            app.add_plugins(ControllerPlugin);
        }
        // The controls and the camera follow what the player set (E8.6).
        if !app.is_plugin_added::<shards_settings::ShardsSettingsPlugin>() {
            app.add_plugins(shards_settings::ShardsSettingsPlugin);
        }
        app.add_engine_module("motion", VERSION);
        app.add_engine_invariant("motion.finite", motion_finite);
        app.add_engine_invariant("motion.launch_sane", launch_sane);
        // On since the fuzz runs on the voxel arena (A2c; see its doc comment).
        app.add_engine_invariant(BODY_IN_SOLID, body_in_solid);
        ai::plugin(app);
        movers::plugin(app);
        usable::plugin(app);
        coverage::plugin(app);
        intent::plugin(app);
        control_lock::plugin(app);
        orders::plugin(app);
        tools::plugin(app);
        perception::plugin(app);
        input::plugin(app);
        camera::plugin(app);
        interp::plugin(app);
        #[cfg(feature = "render")]
        overlay::plugin(app);
    }
}
