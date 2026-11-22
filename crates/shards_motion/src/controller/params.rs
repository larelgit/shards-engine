//! [`CharacterParams`]: every number the controller uses, as content (kind `locomotion`,
//! hot-reloadable) that a [`CharacterController`] names.

use std::borrow::Cow;
use std::sync::LazyLock;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentDb, ContentDef, Refs};
use shards_core::{Gaze, Intent};

use super::accel::AccelModel;
use super::{CharacterController, MotionMode};
use crate::physics::{Collider, CollisionLayers, Shape};
use crate::usable::{DEFAULT_USE_RANGE, PROBE_RANGE};

/// How far above `mantle_height` a ledge may stand and still be climbed (m): float noise.
pub(crate) const MANTLE_SLACK: f32 = 1e-3;
/// A jump in water climbs out onto a ledge this close in front of the body (m), unless the
/// params set `climb_out_reach`.
pub(crate) const DEFAULT_CLIMB_OUT_REACH: f32 = 0.3;

/// When a sprint that ran the stamina dry may start again. The rule is a content choice per kind
/// of sprint (`CombatTuning::sprint_resume` on land, [`CharacterParams::swim_fast_resume`] in water).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum SprintResume {
    /// The sprint button must be let go once; then it works again as soon as there is any stamina
    /// (the default, and the only rule before this option existed).
    #[default]
    OnRelease,
    /// Back once the stamina is at least this fraction of its maximum, with the button held or
    /// not; letting go does not end the lock. `Fraction(0.25)`: a lock that opens at a quarter.
    /// `Fraction(0.0)`: no lock at all, the sprint stops at an empty bar and goes on as soon as
    /// there is any (how some action games treat a sprint on land).
    Fraction(f32),
}

impl SprintResume {
    /// Whether an empty bar sets the lock this rule consults (`Fraction(0)` has none).
    pub fn latches(self) -> bool {
        !matches!(self, SprintResume::Fraction(f) if f <= 0.0)
    }
}

/// What a body's yaw follows ([`CharacterParams::face`]).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Facing {
    /// The body turns toward where it walks, its lock-on target, its aim point or the action it
    /// runs, at its turn rates: every character before the first-person view, and the default.
    #[default]
    Move,
    /// The body's yaw is the yaw of [`Intent::look`](shards_core::Intent::look), set every tick
    /// with no turn rate, whatever it walks or runs (an action included), and its actions that
    /// aim steer by the look. A body without a look in its `Intent` turns as `Move` does.
    /// Movement stays in world space (`Intent::move_dir`).
    Look,
}

/// How a character moves. Distances in meters, times in seconds, speeds in m/s.
/// Missing fields in a file take the defaults; unknown ones are an error, so a typo
/// (`walk_spead`) is reported with its file and line instead of silently doing nothing.
///
/// Content: `<assets>/content/**/*.locomotion.ron`, one map of ids to definitions
/// (`CharacterController { params: "hero" }` picks `"hero"`):
///
/// ```ron
/// { "hero": (walk_speed: 2.0, run_speed: 5.5, jump_height: 1.3) }
/// ```
///
/// The same struct is also a component: one on a character wins over the content id
/// (tests, one-off creatures).
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Component, Default)]
#[serde(default, deny_unknown_fields)]
pub struct CharacterParams {
    /// Capsule radius.
    pub radius: f32,
    /// Capsule height, feet to the top of the head.
    pub height: f32,
    /// Ground speed at `move_dir` length 0.5; slower inputs scale it down.
    pub walk_speed: f32,
    /// Ground speed at `move_dir` length 1.
    pub run_speed: f32,
    /// Ground speed while `sprint` is held and the input is above walking.
    pub sprint_speed: f32,
    /// How fast speed builds up toward the input (m/s²).
    pub acceleration: f32,
    /// How fast speed drops when the input eases off or stops (m/s²).
    pub deceleration: f32,
    /// Steepest walkable slope (degrees). Steeper surfaces are walls: no walking up
    /// them, and a character standing on one slides down.
    pub max_slope_deg: f32,
    /// Highest obstacle walked onto without jumping (stairs, curbs).
    pub step_height: f32,
    /// How far the character is pulled down to stay on the ground: stairs down,
    /// slopes, small drops.
    pub snap_distance: f32,
    /// Gap kept between the capsule and the world, so every cast starts outside
    /// geometry. Feet rest this far above the ground.
    pub skin_width: f32,
    /// Downward acceleration (m/s²).
    pub gravity: f32,
    /// Apex of a jump above the take-off height.
    pub jump_height: f32,
    /// After walking off a ledge, a jump still works for this long.
    pub coyote_time: f32,
    /// A jump pressed this long before it is possible (e.g. just before landing)
    /// still happens.
    pub jump_buffer: f32,
    /// Fraction of `acceleration` available in the air.
    pub air_control: f32,
    /// Fastest fall.
    pub terminal_speed: f32,
    /// Highest ledge top (above the feet) the character climbs onto.
    pub mantle_height: f32,
    /// How long climbing onto a ledge takes.
    pub mantle_time: f32,
    /// Water deeper than this makes the character swim.
    pub swim_depth: f32,
    /// While swimming, the feet float this far below the surface. Less than
    /// `swim_depth`, so a swimmer reaching the shallows stands up.
    pub float_depth: f32,
    /// Swimming speed; sprint does nothing in water.
    pub swim_speed: f32,
    /// Fastest fall while gliding.
    pub glide_fall_speed: f32,
    /// Horizontal cruising speed of a glide.
    pub glide_speed: f32,
    /// How fast a glide turns and speeds up toward `glide_speed` (m/s²).
    pub glide_accel: f32,
    /// Turning speed (degrees per second); 0 turns instantly.
    pub turn_speed_deg: f32,
    /// Swimming speed while `sprint` is held and the input is not zero (a fast swim). 0: sprint
    /// does nothing in water. It costs [`swim_fast_drain`](Self::swim_fast_drain) stamina per
    /// second (a fighter with `Stamina`), ends when there is none, and shows as
    /// `MotionState::sprinting`.
    #[serde(skip_serializing_if = "is_zero")]
    pub swim_fast_speed: f32,
    /// Stamina per second a fast swim costs. Not scaled by `MoveModifiers::sprint_cost`.
    #[serde(skip_serializing_if = "is_zero")]
    pub swim_fast_drain: f32,
    /// How fast a swimmer speeds up toward its input (m/s²). 0: `acceleration`.
    #[serde(skip_serializing_if = "is_zero")]
    pub swim_acceleration: f32,
    /// How fast a swimmer slows with no input (m/s²). 0: the same as `swim_acceleration`
    /// (or `acceleration`).
    #[serde(skip_serializing_if = "is_zero")]
    pub swim_deceleration: f32,
    /// Turning speed of a swimmer (degrees per second). 0: `turn_speed_deg`.
    #[serde(skip_serializing_if = "is_zero")]
    pub swim_turn_speed_deg: f32,
    /// Turning speed (degrees per second) while the input names a facing: a lock-on, an aim point or a
    /// running action's own facing (`LocomotionInput::face`). 0: `turn_speed_deg`. A raised guard has
    /// its own rate through `MoveModifiers::turn_at`, which wins over this; a swimmer keeps
    /// `swim_turn_speed_deg`.
    #[serde(skip_serializing_if = "is_zero")]
    pub locked_turn_speed_deg: f32,
    /// The body turns toward the movement input while there is one, instead of toward its velocity
    /// (the direction it actually moves). A body pressed against a wall, whose velocity the wall
    /// holds at zero, then still turns to face the wall, which a climb-out that wants a facing needs
    /// (`climb_out_min_facing`). With no input it turns toward its velocity as before.
    #[serde(skip_serializing_if = "is_false")]
    pub turn_toward_input: bool,
    /// Horizontal speed (m/s) a body is slowed to when it starts swimming (a plunge from a run or a
    /// jump keeps no more than this). 0: no brake. Applied once, in the tick the water takes the
    /// body, as a scale of the horizontal velocity.
    #[serde(skip_serializing_if = "is_zero")]
    pub entry_max_speed: f32,
    /// Buoyancy as a critically damped spring toward the float line with this natural frequency
    /// (1/s), stepped in closed form so the result does not depend on the tick rate. A fall into the
    /// water then plunges past the float line and comes back, instead of stopping at `swim_speed`.
    /// 0: the first-order drift to the float line, the entry speed held to `swim_speed`.
    #[serde(skip_serializing_if = "is_zero")]
    pub buoyancy_omega: f32,
    /// Fastest vertical speed in the water (m/s) while `buoyancy_omega` is set, entry included.
    /// 0: `swim_speed`. Set it above `swim_speed` for a fall to plunge past the float line.
    #[serde(skip_serializing_if = "is_zero")]
    pub water_max_vertical_speed: f32,
    /// Drag on a fall in the water while `buoyancy_omega` is set: the downward speed decays as
    /// `exp(-drag * t)`. 0: none.
    #[serde(skip_serializing_if = "is_zero")]
    pub water_fall_drag: f32,
    /// Water climb-out needs the body to face the ledge: the facing direction's dot product with the
    /// direction into the wall (the opposite of its normal) must be at least this (0.5: within 60
    /// degrees). `None`: no test, as before. Land mantles are not tested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climb_out_min_facing: Option<f32>,
    /// How long a water climb-out takes (s). `None`: `mantle_time`, like every climb.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climb_out_time: Option<f32>,
    /// Two-tier climb-out: a swimmer pushing against a ledge climbs onto it by itself when the
    /// ledge top is at most this high **above the water surface** (m, plus the 1 mm of float slack
    /// `mantle_height` has: a ledge exactly this high climbs); a higher ledge (up to
    /// `mantle_height` above the feet) needs a jump. `None`: every ledge `mantle_height` allows
    /// is climbed by pushing, as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climb_out_auto_height: Option<f32>,
    /// Jump climb-out limit **above the water surface** (m): a swimmer that jumps at a ledge
    /// climbs onto it when the ledge top is at most this far over the surface, however deep its
    /// feet float; the probe reaches that high too. `None`: the jump uses `mantle_height` above
    /// the feet like any climb (so only `mantle_height - float_depth` over the surface is
    /// reachable). A game that wants a window like "up to 1.3 m over the water" sets this; the
    /// land climb keeps `mantle_height`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climb_out_max_height: Option<f32>,
    /// Horizontal reach of the water climb-out (m): a swimmer that jumps at a ledge climbs onto
    /// it when the ledge face is within this distance in front of the body. `None`: 0.3 m, as
    /// before. A game whose swimmers climb out of ledges half a meter away sets it to that
    /// distance; the `jump` tool and `clearance` ask the same number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climb_out_reach: Option<f32>,
    /// Fast swim: when it may start again after the stamina ran dry. See [`SprintResume`]; the
    /// land sprint has its own rule in the combat tuning.
    #[serde(skip_serializing_if = "is_on_release")]
    pub swim_fast_resume: SprintResume,
    /// Wading: from this depth of water over the feet (m) the walk slows down. Needs
    /// `wade_factor` below 1.
    #[serde(skip_serializing_if = "is_zero")]
    pub wade_min_depth: f32,
    /// Wading: at this depth and deeper the slowdown is the whole `wade_factor`; between
    /// `wade_min_depth` and here it grows in a straight line from 1.
    #[serde(skip_serializing_if = "is_zero")]
    pub wade_full_depth: f32,
    /// Wading: the speed factor at `wade_full_depth`. 1: wading does not slow (the default).
    #[serde(skip_serializing_if = "is_one")]
    pub wade_factor: f32,
    /// Wading: the depth is measured this far (m) below the feet (`surface - feet + offset`), for
    /// a game whose rules read submersion at a reference point under the soles. The value to use
    /// is the gap from that point to the floor of a standing body: its distance below the capsule
    /// bottom plus `skin_width` (the feet rest a skin above the floor). A point 0.1 m under the
    /// capsule bottom with the default skin is `0.12`. Only wading reads it, not swimming,
    /// floating or landing. 0: the depth over the feet.
    #[serde(skip_serializing_if = "is_zero")]
    pub wade_depth_offset: f32,
    /// Wading: the offset while airborne (with `wade_in_air`), when it differs from
    /// the standing one. A reference point fixed under the capsule bottom is `skin_width`
    /// closer to the feet in the air than for a standing body (the feet rest a skin above the
    /// floor only on the ground), so the in-air value is `wade_depth_offset - skin_width`.
    /// `None`: `wade_depth_offset` in every mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wade_air_depth_offset: Option<f32>,
    /// Wading also slows a body in the air over water (the water is over the feet, not on the
    /// ground). A glide is never slowed: modifiers do not scale it. `false`: only on the ground.
    #[serde(skip_serializing_if = "is_false")]
    pub wade_in_air: bool,
    /// Sprint speed grows with the input like a walk does: `sprint_speed * amount`, and any input
    /// above zero is a sprint (not only above walking). `false`: full sprint speed above 0.5.
    #[serde(skip_serializing_if = "is_false")]
    pub sprint_scales_with_input: bool,
    /// A speed cap (`MoveModifiers::cap` and `unsprinted_cap`) is the speed at full input, and
    /// slower inputs scale with it (a cap of 4.5 on a run of 5.0 makes a half-stick walk 2.25),
    /// like an analog stick against a lower top speed. `false`: the cap is an absolute ceiling.
    #[serde(skip_serializing_if = "is_false")]
    pub caps_scale_with_input: bool,
    /// Horizontal speed changes by the acceleration step per axis (x and z each move toward their
    /// own target by at most the step) instead of along the straight line to the target velocity.
    #[serde(skip_serializing_if = "is_false")]
    pub axis_stepping: bool,
    /// `deceleration` applies only when there is no movement input; with input the speed always
    /// changes at `acceleration`, even toward a slower target. `false`: deceleration whenever the
    /// target is slower than the current speed.
    #[serde(skip_serializing_if = "is_false")]
    pub brake_only_without_input: bool,
    /// Horizontal braking in the air with no input (m/s²). 0: the air keeps its horizontal speed.
    /// `air_control` scales the speed-up with input; this is the slow-down without it.
    #[serde(skip_serializing_if = "is_zero")]
    pub air_deceleration: f32,
    /// Sprint is decided every tick in the air too: pressing sprint in the air starts the sprint
    /// (speed and stamina drain) and letting go stops it, and a sprint that was on at take-off
    /// ends with the button. A glide is no sprint. `false`: the sprint flag is decided on the
    /// ground and in the water only, so a take-off sprint lasts the whole jump.
    #[serde(skip_serializing_if = "is_false")]
    pub sprint_in_air: bool,
    /// Landing tiers after a jump: impact speeds (m/s, ascending). `Landed::tier` is how many of
    /// them the impact reached (`[4.0, 6.5, 13.0]`: 0 below 4, 3 from 13). Empty (with
    /// `landing_drop_tiers`): tier 0 always, and `Landed::jumped` is not tracked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub landing_tiers: Vec<f32>,
    /// The same for a fall that did not follow a jump (a ledge walked off). Empty: `landing_tiers`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub landing_drop_tiers: Vec<f32>,
    /// No `Landed` while the feet stand in water deeper than this (m): the shallows break the
    /// fall. `None`: every landing is reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_wade_limit: Option<f32>,
    /// What the body's yaw follows (F1a): `Move` (the default: where it walks, its lock-on, its aim
    /// point, its action) or `Look` (the yaw of `Intent::look` every tick, with no turn rate, and
    /// its aiming actions steer by it). A first-person character is `Look`. Without an
    /// `Intent::look` a `Look` body turns as a `Move` one does.
    #[serde(skip_serializing_if = "is_move")]
    pub face: Facing,
    /// How far the look may pitch (degrees): `(lowest, highest)`, negative looks down. Readers of
    /// `Intent::look` (`look_dir`, the `look` tool, the view) hold the pitch between them. Within
    /// -90..90.
    #[serde(skip_serializing_if = "pitch_limits_are_default")]
    pub pitch_limits_deg: (f32, f32),
    /// Where the eye is, in meters above the feet (`eye_of`, the point the look starts from). `None`:
    /// 0.9 of `height`, the fraction the lock-on's `eye_height` defaults to. The standing eye: crouched,
    /// the eye keeps its place in the shorter capsule (`crouch_height`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eye_height: Option<f32>,
    /// How far the body reaches to use something it looks at (m, F5): the longest distance from its
    /// eye at which `use_target` counts a `Usable`, unless the thing says its own `use_range`.
    /// Above 0 and at most 100 (how far a use is looked for); the default is 2 m.
    #[serde(skip_serializing_if = "use_range_is_default")]
    pub use_range: f32,
    /// The capsule's height when crouched (F2), in meters from the feet. `None`: the body cannot
    /// crouch (a `crouch` input does nothing, and the `crouch` tool says why). The eye keeps its
    /// place in the capsule: crouched, it is [`eye_above_feet`](Self::eye_above_feet) scaled by
    /// `crouch_height / height`. At least the capsule's diameter and below `height`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crouch_height: Option<f32>,
    /// The highest ground speed while crouched (m/s, a cap on walking and running; a crouched body
    /// never sprints). `None`: crouching does not slow the walk. Not applied in the air: a crouch
    /// jump keeps its speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crouch_speed: Option<f32>,
    /// Seconds a full crouch, and a full stand, take: the capsule (and the eye with it) moves
    /// between the two heights at an even rate. 0: at once.
    #[serde(skip_serializing_if = "is_zero")]
    pub crouch_time: f32,
    /// How fast the body climbs a ladder (m/s; a [`VolumeKind::Ladder`](crate::VolumeKind::Ladder) volume):
    /// up while it pushes into the ladder, down while it pushes away from it, sideways along it. 0 (the
    /// default): the body does not climb, and a ladder volume is nothing to it, so an NPC climbs only when
    /// its `locomotion` says so.
    #[serde(skip_serializing_if = "is_zero")]
    pub ladder_speed: f32,
    /// How fast a jump pushes the body off a ladder (m/s, level, out of the ladder's face). 0: the jump
    /// only lets go. The default is 2.5.
    #[serde(skip_serializing_if = "ladder_jump_is_default")]
    pub ladder_jump_speed: f32,
    /// *Half-Life*'s rule for a body that faces its look (`face: Look`): on a ladder the forward input
    /// goes along the look's pitch, so looking up and pressing forward climbs, looking down and pressing
    /// forward goes down, and at 45 degrees up the climb is about 1.4 times `ladder_speed`. `false` (the
    /// default): the pitch does not count, pushing into the ladder climbs and pushing away goes down.
    #[serde(skip_serializing_if = "is_false")]
    pub ladder_by_look: bool,
    /// How the ground and the air steer the body (F7): `Step` (the default: the velocity steps toward the target at
    /// `acceleration`, `deceleration` and `air_control`) or `Projected(..)` (ground friction and a push along the wished
    /// direction limited by the speed already projected on it, as in *Quake* and *Half-Life*: strafing and air turns).
    /// See [`AccelModel`]. Under `Projected` the ground and the air ignore `acceleration`, `deceleration`,
    /// `air_control` and `brake_only_without_input` (and `axis_stepping`, which still shapes the optional `air_deceleration` drag); every other mode and feature is as it was.
    #[serde(skip_serializing_if = "AccelModel::is_step")]
    pub accel: AccelModel,
}

/// The pitch limits a body has unless its `locomotion` says (degrees): just short of straight up
/// and down, where the yaw means nothing.
pub const DEFAULT_PITCH_LIMITS_DEG: (f32, f32) = (-89.0, 89.0);

/// How fast a jump pushes a body off a ladder unless its `locomotion` says (m/s).
pub const DEFAULT_LADDER_JUMP_SPEED: f32 = 2.5;

/// The eye of a body without an `eye_height`, as a fraction of its height.
pub const DEFAULT_EYE_FRACTION: f32 = 0.9;

impl Default for CharacterParams {
    fn default() -> Self {
        Self {
            radius: 0.4,
            height: 1.8,
            walk_speed: 2.0,
            run_speed: 5.0,
            sprint_speed: 8.0,
            acceleration: 40.0,
            deceleration: 50.0,
            max_slope_deg: 45.0,
            step_height: 0.35,
            snap_distance: 0.35,
            skin_width: 0.02,
            gravity: 20.0,
            jump_height: 1.2,
            coyote_time: 0.12,
            jump_buffer: 0.12,
            air_control: 0.35,
            terminal_speed: 40.0,
            mantle_height: 1.6,
            mantle_time: 0.4,
            swim_depth: 1.3,
            float_depth: 1.1,
            swim_speed: 3.0,
            glide_fall_speed: 2.0,
            glide_speed: 9.0,
            glide_accel: 8.0,
            turn_speed_deg: 720.0,
            swim_fast_speed: 0.0,
            swim_fast_drain: 0.0,
            swim_acceleration: 0.0,
            swim_deceleration: 0.0,
            swim_turn_speed_deg: 0.0,
            locked_turn_speed_deg: 0.0,
            turn_toward_input: false,
            entry_max_speed: 0.0,
            buoyancy_omega: 0.0,
            water_max_vertical_speed: 0.0,
            water_fall_drag: 0.0,
            climb_out_min_facing: None,
            climb_out_time: None,
            climb_out_auto_height: None,
            climb_out_max_height: None,
            climb_out_reach: None,
            swim_fast_resume: SprintResume::OnRelease,
            wade_min_depth: 0.0,
            wade_full_depth: 0.0,
            wade_factor: 1.0,
            wade_depth_offset: 0.0,
            wade_air_depth_offset: None,
            wade_in_air: false,
            sprint_scales_with_input: false,
            caps_scale_with_input: false,
            axis_stepping: false,
            brake_only_without_input: false,
            air_deceleration: 0.0,
            sprint_in_air: false,
            landing_tiers: Vec::new(),
            landing_drop_tiers: Vec::new(),
            landing_wade_limit: None,
            face: Facing::Move,
            pitch_limits_deg: DEFAULT_PITCH_LIMITS_DEG,
            eye_height: None,
            use_range: DEFAULT_USE_RANGE,
            crouch_height: None,
            crouch_speed: None,
            crouch_time: 0.0,
            ladder_speed: 0.0,
            ladder_jump_speed: DEFAULT_LADDER_JUMP_SPEED,
            ladder_by_look: false,
            accel: AccelModel::Step,
        }
    }
}

impl ContentDef for CharacterParams {
    const KIND: &'static str = "locomotion";

    fn validate(&self, _: &str, refs: &mut Refs) {
        if let Err(problems) = self.check() {
            refs.error(problems);
        }
    }
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_on_release(v: &SprintResume) -> bool {
    *v == SprintResume::OnRelease
}

fn is_move(v: &Facing) -> bool {
    *v == Facing::Move
}

fn pitch_limits_are_default(v: &(f32, f32)) -> bool {
    *v == DEFAULT_PITCH_LIMITS_DEG
}

fn use_range_is_default(v: &f32) -> bool {
    *v == DEFAULT_USE_RANGE
}

fn ladder_jump_is_default(v: &f32) -> bool {
    *v == DEFAULT_LADDER_JUMP_SPEED
}

/// What a character without content moves by: the code defaults.
static DEFAULTS: LazyLock<CharacterParams> = LazyLock::new(CharacterParams::default);

impl CharacterController {
    /// The numbers this character moves by: its own [`CharacterParams`] component, else the
    /// `locomotion` definition named by [`params`](Self::params) (read from the live
    /// database, so a hot reload applies on the next tick), else the code defaults.
    pub fn resolve<'a>(&self, own: Option<&'a CharacterParams>, db: Option<&'a ContentDb>) -> &'a CharacterParams {
        own.or_else(|| db.and_then(|db| db.get::<CharacterParams>(&self.params))).unwrap_or(&DEFAULTS)
    }

    /// A named definition the database does not have: the character runs on the code
    /// defaults, which is worth saying once.
    pub(super) fn is_dangling(&self, own: Option<&CharacterParams>, db: Option<&ContentDb>) -> bool {
        own.is_none()
            && !self.params.is_empty()
            && db.is_some_and(|db| db.is_loaded() && db.get::<CharacterParams>(&self.params).is_none())
    }
}

impl CharacterParams {
    /// The body these params describe: a capsule standing on the entity's origin.
    pub fn collider(&self) -> Collider {
        Collider::new(Shape::capsule(self.radius, self.height))
            .with_offset(Vec3::Y * self.height * 0.5)
            .with_layers(CollisionLayers::CHARACTER)
    }

    /// The gap the capsule keeps from the world: `skin_width`, as far as the capsule can carry it.
    pub fn skin(&self) -> f32 {
        self.skin_width.clamp(0.001, self.radius.max(0.01) * 0.5)
    }

    /// Whether a ledge whose standing surface is `height` above the feet can be climbed. The
    /// controller's rule, and the only one: the planner, `clearance` and the `jump` tool
    /// ask [`can_climb`](Self::can_climb), which turns the height they see into this one.
    pub fn can_mantle(&self, height: f32) -> bool {
        height <= self.mantle_height + MANTLE_SLACK
    }

    /// [`can_mantle`](Self::can_mantle) for a floor `rise` meters above the floor (or the
    /// water, when `swimming`) a character stands in. On the ground the feet rest a skin above
    /// the floor and the climb stands them a skin above the ledge, so the skins cancel; a
    /// swimmer floats at a height of its own and the second skin counts.
    pub fn can_climb(&self, rise: f32, swimming: bool) -> bool {
        self.can_mantle(if swimming { rise + self.skin() } else { rise })
    }

    /// Whether a swimmer's **jump** climbs onto a floor `rise` meters above the height its feet
    /// float at: [`climb_out_max_height`](Self::climb_out_max_height) above the surface when set
    /// (the feet float `float_depth` below it), else the ordinary [`can_climb`](Self::can_climb).
    /// The controller's rule for the jump, and the one the `jump` tool and `clearance` ask.
    pub fn can_jump_out(&self, rise: f32) -> bool {
        match self.climb_out_max_height {
            Some(limit) => rise - self.float_depth <= limit + MANTLE_SLACK,
            None => self.can_climb(rise, true),
        }
    }

    /// Initial upward speed that peaks at `jump_height`.
    pub fn jump_speed(&self) -> f32 {
        (2.0 * self.gravity.max(0.0) * self.jump_height.max(0.0)).sqrt()
    }

    /// Ground speed for an input of length `amount` (0..1), and whether it is a sprint.
    pub fn ground_speed(&self, amount: f32, sprint: bool) -> (f32, bool) {
        let amount = amount.clamp(0.0, 1.0);
        if self.sprint_scales_with_input {
            if sprint && amount > 0.0 {
                return (self.sprint_speed * amount, true);
            }
        } else if sprint && amount > 0.5 {
            return (self.sprint_speed, true);
        }
        if amount <= 0.5 {
            (self.walk_speed * amount * 2.0, false)
        } else {
            (self.walk_speed + (self.run_speed - self.walk_speed) * (amount - 0.5) * 2.0, false)
        }
    }

    /// The speed factor of wading in water `depth` meters over the feet: 1 up to
    /// `wade_min_depth`, `wade_factor` from `wade_full_depth`, a straight line between.
    pub fn wade_scale(&self, depth: f32) -> f32 {
        if self.wade_factor == 1.0 || depth <= self.wade_min_depth {
            return 1.0;
        }
        let span = self.wade_full_depth - self.wade_min_depth;
        let k = if span > 1e-6 { ((depth - self.wade_min_depth) / span).clamp(0.0, 1.0) } else { 1.0 };
        1.0 + (self.wade_factor - 1.0) * k
    }

    /// Whether water over the feet slows a body in `mode`: on the ground, and in the air with
    /// [`wade_in_air`](Self::wade_in_air). A swimmer is not a wader, and a glide is not scaled.
    pub fn wades_in(&self, mode: MotionMode) -> bool {
        self.wade_factor != 1.0
            && (mode == MotionMode::Ground
                || (self.wade_in_air && mode == MotionMode::Air))
    }

    /// The depth wading reads for feet at height `feet_y` in water with this `surface`: the water
    /// over the feet plus [`wade_depth_offset`](Self::wade_depth_offset), never below 0.
    pub fn wade_depth(&self, surface: f32, feet_y: f32) -> f32 {
        self.wade_depth_for(MotionMode::Ground, surface, feet_y)
    }

    /// [`wade_depth`](Self::wade_depth) for a body in `mode`: an airborne or gliding body uses
    /// [`wade_air_depth_offset`](Self::wade_air_depth_offset) when the params have one.
    pub fn wade_depth_for(&self, mode: MotionMode, surface: f32, feet_y: f32) -> f32 {
        let offset = match mode {
            MotionMode::Air | MotionMode::Glide => self.wade_air_depth_offset.unwrap_or(self.wade_depth_offset),
            _ => self.wade_depth_offset,
        };
        if offset == 0.0 { (surface - feet_y).max(0.0) } else { (surface - feet_y + offset).max(0.0) }
    }

    /// How far in front of the body a water climb-out finds a ledge (m):
    /// [`climb_out_reach`](Self::climb_out_reach), else 0.3.
    pub fn climb_reach(&self) -> f32 {
        self.climb_out_reach.unwrap_or(DEFAULT_CLIMB_OUT_REACH)
    }

    /// Where the eye is, in meters above the feet: [`eye_height`](Self::eye_height), else 0.9 of
    /// the body's height. The standing eye; `eye_above_feet_at` is the eye at a depth of crouch, and `eye_of`
    /// adds that to the body's feet.
    pub fn eye_above_feet(&self) -> f32 {
        self.eye_height.unwrap_or(DEFAULT_EYE_FRACTION * self.height)
    }

    /// Whether the body climbs ladders: its `locomotion` gives a [`ladder_speed`](Self::ladder_speed).
    pub fn can_climb_ladders(&self) -> bool {
        self.ladder_speed > 0.0
    }

    /// Whether the body can crouch: its `locomotion` gives a [`crouch_height`](Self::crouch_height).
    pub fn can_crouch(&self) -> bool {
        self.crouch_height.is_some()
    }

    /// The capsule's height when `depth` of the way into a crouch (0 standing, 1 crouched; held
    /// between): [`height`](Self::height) for a body that cannot crouch, whatever the depth.
    pub fn height_at(&self, depth: f32) -> f32 {
        match self.crouch_height {
            Some(crouched) if depth > 0.0 => self.height + (crouched - self.height) * depth.min(1.0),
            _ => self.height,
        }
    }

    /// [`eye_above_feet`](Self::eye_above_feet) when `depth` of the way into a crouch: the eye keeps
    /// its place in the capsule, so it is scaled with [`height_at`](Self::height_at).
    pub fn eye_above_feet_at(&self, depth: f32) -> f32 {
        let standing = self.eye_above_feet();
        if depth > 0.0 && self.can_crouch() { standing * (self.height_at(depth) / self.height) } else { standing }
    }

    /// These params for a body `depth` of the way into a crouch: the capsule's height and the eye at
    /// that depth, everything else as it is. What every reader of a body's size asks (the
    /// controller's casts, its collider, the movers that carry it, `clearance`). Borrowed (no copy)
    /// for a standing body or one that cannot crouch.
    pub fn at_depth(&self, depth: f32) -> Cow<'_, CharacterParams> {
        if depth <= 0.0 || !self.can_crouch() {
            return Cow::Borrowed(self);
        }
        Cow::Owned(CharacterParams { height: self.height_at(depth), eye_height: Some(self.eye_above_feet_at(depth)), ..self.clone() })
    }

    /// [`pitch_limits_deg`](Self::pitch_limits_deg) in radians, `(lowest, highest)`.
    pub fn pitch_limits(&self) -> (f32, f32) {
        (self.pitch_limits_deg.0.to_radians(), self.pitch_limits_deg.1.to_radians())
    }

    /// `look` with its pitch held between this body's limits.
    pub fn clamp_look(&self, look: Gaze) -> Gaze {
        let (lowest, highest) = self.pitch_limits();
        look.with_pitch_between(lowest, highest)
    }

    /// The look this body's yaw is bound to this tick: `intent.look` (finite, its pitch clamped)
    /// when the body has [`face: Look`](Facing::Look), else `None` (it turns by the movement,
    /// its lock-on, its aim point or its action, as every character did before the look).
    pub fn bound_look(&self, intent: &Intent) -> Option<Gaze> {
        if self.face != Facing::Look {
            return None;
        }
        intent.look.filter(|l| l.is_finite()).map(|l| self.clamp_look(l))
    }

    /// Whether landings are told apart (any tier thresholds): the controller then tracks
    /// whether a fall followed a jump.
    pub fn tracks_landings(&self) -> bool {
        !self.landing_tiers.is_empty() || !self.landing_drop_tiers.is_empty()
    }

    /// The landing tier of an impact at `impact_speed` m/s: how many thresholds it reached.
    pub fn landing_tier(&self, impact_speed: f32, jumped: bool) -> u8 {
        let tiers =
            if !jumped && !self.landing_drop_tiers.is_empty() { &self.landing_drop_tiers } else { &self.landing_tiers };
        tiers.iter().filter(|t| impact_speed >= **t).count().min(u8::MAX as usize) as u8
    }

    /// Everything wrong with these numbers, so content errors name the field.
    pub fn check(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        let positive = [
            ("radius", self.radius),
            ("height", self.height),
            ("gravity", self.gravity),
            ("skin_width", self.skin_width),
            ("mantle_time", self.mantle_time),
            ("terminal_speed", self.terminal_speed),
        ];
        for (name, v) in positive {
            if !(v > 0.0 && v.is_finite()) {
                errors.push(format!("{name} must be > 0 (is {v})"));
            }
        }
        if self.height < 2.0 * self.radius {
            errors.push(format!("height {} is less than the capsule's diameter {}", self.height, 2.0 * self.radius));
        }
        if !(0.0..90.0).contains(&self.max_slope_deg) {
            errors.push(format!("max_slope_deg must be in 0..90 (is {})", self.max_slope_deg));
        }
        if self.float_depth >= self.swim_depth {
            errors.push(format!(
                "float_depth {} must be less than swim_depth {}, or swimmers never stand up in the shallows",
                self.float_depth, self.swim_depth
            ));
        }
        if self.mantle_height < self.step_height {
            errors.push(format!("mantle_height {} is below step_height {}", self.mantle_height, self.step_height));
        }
        if self.skin_width >= self.radius * 0.5 {
            errors.push(format!("skin_width {} is too large for radius {}", self.skin_width, self.radius));
        }
        let non_negative = [
            ("swim_fast_speed", self.swim_fast_speed),
            ("swim_fast_drain", self.swim_fast_drain),
            ("swim_acceleration", self.swim_acceleration),
            ("swim_deceleration", self.swim_deceleration),
            ("swim_turn_speed_deg", self.swim_turn_speed_deg),
            ("locked_turn_speed_deg", self.locked_turn_speed_deg),
            ("entry_max_speed", self.entry_max_speed),
            ("buoyancy_omega", self.buoyancy_omega),
            ("water_max_vertical_speed", self.water_max_vertical_speed),
            ("water_fall_drag", self.water_fall_drag),
            ("wade_min_depth", self.wade_min_depth),
            ("wade_full_depth", self.wade_full_depth),
            ("wade_factor", self.wade_factor),
            ("wade_depth_offset", self.wade_depth_offset),
            ("air_deceleration", self.air_deceleration),
            ("ladder_speed", self.ladder_speed),
            ("ladder_jump_speed", self.ladder_jump_speed),
        ];
        for (name, v) in non_negative {
            if !(v >= 0.0 && v.is_finite()) {
                errors.push(format!("{name} must be >= 0 (is {v})"));
            }
        }
        if self.wade_factor != 1.0 && self.wade_full_depth < self.wade_min_depth {
            errors.push(format!(
                "wade_full_depth {} is below wade_min_depth {}",
                self.wade_full_depth, self.wade_min_depth
            ));
        }
        if self.wade_factor > 1.0 {
            errors.push(format!("wade_factor {} is above 1, wading would speed the body up", self.wade_factor));
        }
        if self.swim_fast_speed > 0.0 && self.swim_fast_speed < self.swim_speed {
            errors.push(format!("swim_fast_speed {} is below swim_speed {}", self.swim_fast_speed, self.swim_speed));
        }
        if self.swim_fast_drain > 0.0 && self.swim_fast_speed <= 0.0 {
            errors.push("swim_fast_drain is set but swim_fast_speed is 0, so the drain never applies".to_string());
        }
        if self.climb_out_auto_height.is_some_and(|h| h > self.mantle_height) {
            errors.push(format!(
                "climb_out_auto_height {:?} is above mantle_height {}, the second tier could never apply",
                self.climb_out_auto_height, self.mantle_height
            ));
        }
        if let (Some(auto), Some(max)) = (self.climb_out_auto_height, self.climb_out_max_height)
            && auto > max
        {
            errors.push(format!(
                "climb_out_auto_height {auto} is above climb_out_max_height {max}, the push would climb higher than the jump"
            ));
        }
        if let SprintResume::Fraction(f) = self.swim_fast_resume
            && !(0.0..=1.0).contains(&f)
        {
            errors.push(format!("swim_fast_resume Fraction({f}) must be between 0 and 1"));
        }
        for (name, tiers) in [("landing_tiers", &self.landing_tiers), ("landing_drop_tiers", &self.landing_drop_tiers)] {
            if tiers.iter().any(|t| !(*t > 0.0 && t.is_finite())) || tiers.windows(2).any(|w| w[0] >= w[1]) {
                errors.push(format!("{name} must be speeds > 0 in ascending order (is {tiers:?})"));
            }
        }
        for (name, v) in [
            ("climb_out_auto_height", self.climb_out_auto_height),
            ("climb_out_max_height", self.climb_out_max_height),
            ("climb_out_reach", self.climb_out_reach),
            ("wade_air_depth_offset", self.wade_air_depth_offset),
            ("landing_wade_limit", self.landing_wade_limit),
        ] {
            if v.is_some_and(|v| !(v >= 0.0 && v.is_finite())) {
                errors.push(format!("{name} must be >= 0 (is {v:?})"));
            }
        }
        if self.climb_out_time.is_some_and(|t| !(t > 0.0 && t.is_finite())) {
            errors.push(format!("climb_out_time must be > 0 (is {:?})", self.climb_out_time));
        }
        if self.climb_out_min_facing.is_some_and(|f| !(-1.0..=1.0).contains(&f)) {
            errors.push(format!("climb_out_min_facing must be between -1 and 1 (is {:?})", self.climb_out_min_facing));
        }
        let (lowest, highest) = self.pitch_limits_deg;
        if !(lowest.is_finite() && highest.is_finite() && -90.0 <= lowest && lowest < highest && highest <= 90.0) {
            errors.push(format!(
                "pitch_limits_deg must be (lowest, highest) with -90 <= lowest < highest <= 90 (is ({lowest}, {highest}))"
            ));
        }
        if let Some(eye) = self.eye_height
            && !(eye > 0.0 && eye <= self.height)
        {
            errors.push(format!("eye_height must be above 0 and at most height {} (is {eye})", self.height));
        }
        if !(self.use_range > 0.0 && self.use_range <= PROBE_RANGE) {
            errors.push(format!("use_range must be a number of metres above 0 and at most {PROBE_RANGE} (is {})", self.use_range));
        }
        match self.crouch_height {
            Some(crouched) => {
                if !(crouched.is_finite() && crouched >= 2.0 * self.radius && crouched < self.height) {
                    errors.push(format!(
                        "crouch_height must be at least the capsule's diameter {} and below height {} (is {crouched})",
                        2.0 * self.radius,
                        self.height
                    ));
                }
            }
            None if self.crouch_speed.is_some() || self.crouch_time > 0.0 => {
                errors.push("crouch_speed and crouch_time are set but crouch_height is not, so the body never crouches".to_string());
            }
            None => {}
        }
        if self.crouch_speed.is_some_and(|v| !(v > 0.0 && v.is_finite())) {
            errors.push(format!("crouch_speed must be > 0 (is {:?}); leave it out to keep the walk's speed", self.crouch_speed));
        }
        if !(self.crouch_time >= 0.0 && self.crouch_time.is_finite()) {
            errors.push(format!("crouch_time must be >= 0 (is {})", self.crouch_time));
        }
        errors.extend(self.accel.problems());
        if self.buoyancy_omega <= 0.0 && (self.water_max_vertical_speed > 0.0 || self.water_fall_drag > 0.0) {
            errors.push("water_max_vertical_speed and water_fall_drag are set but buoyancy_omega is 0, so they never apply".to_string());
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        CharacterParams::default().check().unwrap();
    }

    #[test]
    fn validation_names_the_field() {
        let p = CharacterParams { float_depth: 2.0, radius: -1.0, ..default() };
        let err = p.check().unwrap_err();
        assert!(err.contains("radius must be > 0") && err.contains("float_depth"), "{err}");
    }

    #[test]
    fn one_rule_says_what_can_be_climbed() {
        let p = CharacterParams::default();
        assert!(p.can_mantle(1.6) && !p.can_mantle(1.61), "mantle_height is the limit, with float noise to spare");
        assert!(p.can_climb(1.6, false), "on the ground the skins cancel");
        assert!(!p.can_climb(1.6, true) && p.can_climb(1.57, true), "a swimmer's second skin counts");
    }

    #[test]
    fn the_look_options_default_to_nothing_and_leave_the_written_form_alone() {
        let p = CharacterParams::default();
        assert_eq!((p.face, p.pitch_limits_deg, p.eye_height), (Facing::Move, (-89.0, 89.0), None));
        // A definition that never mentions them writes none of them back: its content hash is what it was.
        let written = ron::to_string(&p).unwrap();
        assert!(!written.contains("face") && !written.contains("pitch_limits_deg") && !written.contains("eye_height"), "{written}");
        let set: CharacterParams = ron::from_str("(height: 2.0, face: Look, pitch_limits_deg: (-60.0, 80.0), eye_height: Some(1.7))").unwrap();
        assert_eq!((set.face, set.pitch_limits_deg, set.eye_height), (Facing::Look, (-60.0, 80.0), Some(1.7)));
        let again = ron::to_string(&set).unwrap();
        assert!(again.contains("face:Look") && again.contains("pitch_limits_deg:(-60.0,80.0)") && again.contains("eye_height:Some(1.7)"), "{again}");
        assert_eq!(ron::from_str::<CharacterParams>(&again).unwrap(), set);
    }

    #[test]
    fn the_crouch_options_default_to_nothing_and_leave_the_written_form_alone() {
        let p = CharacterParams::default();
        assert_eq!((p.crouch_height, p.crouch_speed, p.crouch_time, p.can_crouch()), (None, None, 0.0, false));
        let written = ron::to_string(&p).unwrap();
        assert!(!written.contains("crouch"), "a definition that never mentions them writes none of them back: {written}");
        let set: CharacterParams = ron::from_str("(crouch_height: Some(1.0), crouch_speed: Some(1.5), crouch_time: 0.25)").unwrap();
        assert_eq!((set.crouch_height, set.crouch_speed, set.crouch_time, set.can_crouch()), (Some(1.0), Some(1.5), 0.25, true));
        let again = ron::to_string(&set).unwrap();
        assert!(again.contains("crouch_height:Some(1.0)") && again.contains("crouch_speed:Some(1.5)") && again.contains("crouch_time:0.25"), "{again}");
        assert_eq!(ron::from_str::<CharacterParams>(&again).unwrap(), set);
    }

    #[test]
    fn the_height_and_the_eye_at_a_depth_of_crouch() {
        let p = CharacterParams { crouch_height: Some(1.0), eye_height: Some(1.6), ..default() };
        assert_eq!((p.height_at(0.0), p.height_at(1.0), p.height_at(2.0), p.height_at(-1.0)), (1.8, 1.0, 1.0, 1.8), "held between standing and crouched");
        assert!((p.height_at(0.5) - 1.4).abs() < 1e-6);
        // The eye keeps its place in the capsule.
        assert!((p.eye_above_feet_at(0.0) - 1.6).abs() < 1e-6 && (p.eye_above_feet_at(1.0) - 1.6 / 1.8).abs() < 1e-6);
        assert!((p.eye_above_feet_at(0.5) - 1.6 * 1.4 / 1.8).abs() < 1e-6);
        let sized = p.at_depth(0.5);
        assert!(matches!(sized, Cow::Owned(_)) && (sized.height - 1.4).abs() < 1e-6 && (sized.eye_above_feet() - 1.6 * 1.4 / 1.8).abs() < 1e-6);
        assert_eq!(sized.crouch_height, Some(1.0), "the rest is as it was");
        assert!(matches!(p.at_depth(0.0), Cow::Borrowed(_)), "standing: no copy");
        // A body that cannot crouch is the same at any depth.
        let plain = CharacterParams::default();
        assert!(matches!(plain.at_depth(1.0), Cow::Borrowed(_)) && plain.height_at(1.0) == 1.8 && plain.eye_above_feet_at(1.0) == plain.eye_above_feet());
    }

    #[test]
    fn bad_crouch_numbers_are_named() {
        let bad = |p: CharacterParams| p.check().unwrap_err();
        // The height: not the diameter's worth, not below the standing one, not a number.
        for height in [0.7, 1.8, 2.5, f32::NAN] {
            let err = bad(CharacterParams { crouch_height: Some(height), ..default() });
            assert!(err.contains("crouch_height must be at least the capsule's diameter 0.8 and below height 1.8"), "{height}: {err}");
        }
        CharacterParams { crouch_height: Some(0.8), ..default() }.check().unwrap();
        let err = bad(CharacterParams { crouch_height: Some(1.0), crouch_speed: Some(0.0), crouch_time: -1.0, ..default() });
        assert!(err.contains("crouch_speed must be > 0") && err.contains("crouch_time must be >= 0"), "{err}");
        // A speed or a time with no height to crouch to is a typo: it would never apply.
        let err = bad(CharacterParams { crouch_speed: Some(1.0), ..default() });
        assert!(err.contains("crouch_speed and crouch_time are set but crouch_height is not"), "{err}");
        CharacterParams { crouch_height: Some(1.0), crouch_speed: Some(1.0), crouch_time: 0.2, ..default() }.check().unwrap();
    }

    #[test]
    fn the_eye_is_nine_tenths_of_the_height_unless_content_says() {
        let tall = CharacterParams { height: 2.0, ..default() };
        assert!((tall.eye_above_feet() - 1.8).abs() < 1e-6);
        assert_eq!(CharacterParams { eye_height: Some(1.5), ..tall }.eye_above_feet(), 1.5);
    }

    #[test]
    fn a_look_is_bound_only_for_a_body_that_faces_it() {
        let intent = Intent { look: Some(Gaze::from_degrees(30.0, 80.0)), ..default() };
        assert_eq!(CharacterParams::default().bound_look(&intent), None, "a body that faces its movement ignores it");
        let first_person = CharacterParams { face: Facing::Look, pitch_limits_deg: (-60.0, 45.0), ..default() };
        let bound = first_person.bound_look(&intent).expect("bound");
        assert_eq!(bound.yaw, 30.0_f32.to_radians());
        assert!((bound.pitch - 45.0_f32.to_radians()).abs() < 1e-6, "the pitch is held to the limit");
        assert_eq!(first_person.bound_look(&Intent::default()), None, "no look in the intent: it turns as a Move body does");
        let bad = Intent { look: Some(Gaze::new(f32::NAN, 0.0)), ..default() };
        assert_eq!(first_person.bound_look(&bad), None, "a look that is not a number binds nothing");
    }

    #[test]
    fn bad_pitch_limits_and_eyes_are_named() {
        for limits in [(-91.0, 89.0), (-89.0, 91.0), (10.0, 10.0), (20.0, -20.0), (f32::NAN, 0.0)] {
            let err = CharacterParams { pitch_limits_deg: limits, ..default() }.check().unwrap_err();
            assert!(err.contains("pitch_limits_deg"), "{limits:?}: {err}");
        }
        CharacterParams { pitch_limits_deg: (-90.0, 90.0), ..default() }.check().unwrap();
        for eye in [0.0, -1.0, 1.9, f32::NAN] {
            let err = CharacterParams { eye_height: Some(eye), ..default() }.check().unwrap_err();
            assert!(err.contains("eye_height"), "{eye}: {err}");
        }
        CharacterParams { eye_height: Some(1.8), ..default() }.check().unwrap();
    }

    #[test]
    fn the_use_range_defaults_to_two_metres_and_stays_out_of_the_written_form() {
        let p = CharacterParams::default();
        assert_eq!(p.use_range, 2.0);
        assert!(!ron::to_string(&p).unwrap().contains("use_range"), "a definition that never mentions it keeps its hash");
        let set: CharacterParams = ron::from_str("(use_range: 3.5)").unwrap();
        assert_eq!(set.use_range, 3.5);
        assert!(ron::to_string(&set).unwrap().contains("use_range:3.5"));
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY, 100.5] {
            let err = CharacterParams { use_range: bad, ..default() }.check().unwrap_err();
            assert!(err.contains("use_range must be a number of metres above 0 and at most 100"), "{bad}: {err}");
        }
    }

    #[test]
    fn input_length_picks_walk_run_or_sprint() {
        let p = CharacterParams::default();
        assert_eq!(p.ground_speed(0.5, false), (p.walk_speed, false));
        assert_eq!(p.ground_speed(0.25, false), (p.walk_speed * 0.5, false));
        assert_eq!(p.ground_speed(1.0, false), (p.run_speed, false));
        assert_eq!(p.ground_speed(1.0, true), (p.sprint_speed, true));
        assert_eq!(p.ground_speed(0.4, true), (p.walk_speed * 0.8, false), "no sprint at walking pace");
    }
}
