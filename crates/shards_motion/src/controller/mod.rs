//! Kinematic character controller: the only thing that moves characters.
//!
//! Gameplay writes a [`LocomotionInput`] before `SimSet::Move`
//! ([`intent_to_locomotion`](crate::intent_to_locomotion) fills it from `Intent`, the
//! action machine adds its displacement);
//! [`move_characters`] turns it into motion with shape casts against the world
//! ([`PhysicsWorld`]) and keeps the result in [`MotionState`]. Actions never move a body
//! themselves: a dodge roll is [`LocomotionInput::external`], integrated here with
//! everything else. The numbers come from content ([`CharacterParams`], kind
//! `locomotion`), read again every tick, so editing the file changes the next step.
//!
//! Modes ([`MotionMode`]): **Ground** walks, runs and sprints, follows slopes up to
//! `max_slope_deg`, steps onto obstacles up to `step_height` and snaps down stairs;
//! **Air** falls with gravity and some air control; **Glide** falls slowly and rides
//! updrafts; **Water** floats at the surface; **Mantle** climbs onto a ledge along a
//! path checked before it starts; **Ladder** holds a body whose params give a `ladder_speed`
//! on a ladder volume, with no gravity, and climbs by its movement input (`locomotion.rs`,
//! "ladder"). Characters never collide with each other through
//! casts: after all of them moved, a separation pass pushes overlapping capsules apart.
//!
//! The transform's translation is the character's feet. Writing it from outside (the
//! `teleport` tool, a respawn) puts the character there afresh: no speed, no fall, no
//! climb carried over from where it was.
//!
//! Facing: a body turns toward where it walks, its lock-on, its aim point or the action it runs, at
//! its turn rates. A body whose params say `face: Look` ([`Facing`]) takes its yaw from
//! [`Intent::look`] instead, every tick with no turn rate, whatever else it does (F1a).
//!
//! Stance (F2): a body whose params give a `crouch_height` lowers its capsule while the `crouch` input is
//! held ([`Crouch`], `stance.rs`): the feet stay on the ground and the head stays in the air, a stand-up
//! waits for room, and everything that reads the body's size asks [`CharacterParams::at_depth`].

mod accel;
pub(crate) mod locomotion;
pub(crate) mod mover;
mod params;
mod stance;

use std::collections::HashSet;

use ai_core::{journal, AiAppExt, JournalKind, SimSet};
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::query::QueryFilter;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentAppExt, ContentDb};
use shards_core::input_names::CROUCH;
use shards_core::{Intent, NetRole, NetSet, Predicted};

pub use accel::AccelModel;
pub use params::{CharacterParams, Facing, SprintResume, DEFAULT_EYE_FRACTION, DEFAULT_PITCH_LIMITS_DEG};
pub use stance::{Crouch, Crouched, Stood};
pub(crate) use stance::room_to_stand;

use crate::modifiers::MoveModifiers;
use crate::physics::{Body, Collider, ColliderId, PhysicsPlugin, PhysicsWorld, VolumeEntered, VolumeExited};
use locomotion::{Note, Step};
use mover::{flat, Mover};

/// Makes an entity a character moved by this controller. Its [`Collider`] is the capsule
/// its numbers describe (kept in sync, layers left alone); its transform is at the feet.
/// Add [`Intent`] for it to be driven like every character is
/// (keyboard, agent, brain, replay): [`character`] does.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[reflect(Component, Default)]
#[serde(default)]
#[require(
    Collider = CharacterParams::default().collider(),
    Body = Body::Kinematic,
    LocomotionInput,
    MotionState,
    MoveModifiers
)]
#[component(on_insert = fit_collider)]
pub struct CharacterController {
    /// Content id of the `locomotion` definition ([`CharacterParams`]) to move by. Empty:
    /// the code defaults (tests). An id the content does not have is the same, and says
    /// so once in the journal.
    pub params: String,
}

impl CharacterController {
    /// A controller moving by the `locomotion` definition `params`.
    pub fn new(params: impl Into<String>) -> Self {
        Self { params: params.into() }
    }
}

/// The capsule follows the params from the first moment, so queries between spawn
/// and the first tick already see the right body.
fn fit_collider(mut world: DeferredWorld, ctx: HookContext) {
    let Some(controller) = world.get::<CharacterController>(ctx.entity) else {
        return;
    };
    let want = controller
        .resolve(world.get::<CharacterParams>(ctx.entity), world.get_resource::<ContentDb>())
        .collider();
    if let Some(mut collider) = world.get_mut::<Collider>(ctx.entity) {
        fit(&mut collider, &want);
    }
}

fn fit(collider: &mut Mut<Collider>, want: &Collider) {
    if collider.shape != want.shape || collider.offset != want.offset {
        collider.shape = want.shape;
        collider.offset = want.offset;
    }
}

/// What the character wants to do this tick. Written by gameplay before
/// `SimSet::Move`; `jump` and `external` are consumed by the controller.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[reflect(Component, Default)]
pub struct LocomotionInput {
    /// Desired movement on the ground plane, length ≤ 1: 0.5 walks, 1 runs.
    pub move_dir: Vec3,
    /// Held: run faster on the ground (needs more than walking input).
    pub sprint: bool,
    /// Pressed this tick. Buffered for `jump_buffer`, then forgotten.
    pub jump: bool,
    /// Held: glide while falling.
    pub glide: bool,
    /// Turn toward this direction; `None` faces where the character moves.
    pub face: Option<Vec3>,
    /// World-space displacement an action asks for this tick (a dodge roll, a lunge).
    pub external: Vec3,
    /// An action owns the body: no walking, jumping, gliding or climbing; only
    /// `external` and gravity move it (a body on a ladder keeps its hold, and hangs).
    pub locked: bool,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MotionMode {
    #[default]
    Ground,
    Air,
    Water,
    Glide,
    Mantle,
    /// On a ladder ([`VolumeKind::Ladder`](crate::VolumeKind::Ladder)): held by it, with no gravity, and
    /// climbing by the movement input at the params' `ladder_speed`. Only a body whose params give that
    /// speed is ever in it. [`MotionState::velocity`] is the climb's: its `y` is the speed up or down.
    Ladder,
}

/// A climb onto a ledge: straight up from `from` to `corner`, then over to `to`.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Mantle {
    pub from: Vec3,
    pub corner: Vec3,
    pub to: Vec3,
    /// Progress, 0..1.
    pub t: f32,
    /// Seconds for the whole climb.
    pub duration: f32,
}

/// How a character is moving: gameplay state, kept in replays.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Component, Default)]
pub struct MotionState {
    pub mode: MotionMode,
    /// m/s. On the ground only the horizontal part (slopes add the vertical one).
    pub velocity: Vec3,
    pub grounded: bool,
    /// The walkable surface stood on (`Y` when not grounded).
    pub ground_normal: Vec3,
    /// Seconds a jump still works after walking off a ledge.
    pub coyote_left: f32,
    /// Seconds the last jump press stays valid.
    pub jump_buffer_left: f32,
    pub mantle: Option<Mantle>,
    /// Highest feet height since leaving the ground; `None` on the ground.
    pub fall_from_y: Option<f32>,
    pub sprinting: bool,
    /// Surface of the water the feet are in.
    pub water_surface: Option<f32>,
    /// `(accel, max_speed)` of the updraft the character is in.
    pub updraft: Option<Vec2>,
    /// Volumes the capsule touches, in collider id order.
    pub volumes: Vec<Entity>,
    /// Where the controller left the feet. A transform found anywhere else was moved by
    /// someone else, and the next tick starts afresh there.
    pub placed: Option<Vec3>,
}

impl Default for MotionState {
    fn default() -> Self {
        Self {
            mode: MotionMode::Ground,
            velocity: Vec3::ZERO,
            grounded: false,
            ground_normal: Vec3::Y,
            coyote_left: 0.0,
            jump_buffer_left: 0.0,
            mantle: None,
            fall_from_y: None,
            sprinting: false,
            water_surface: None,
            updraft: None,
            volumes: Vec::new(),
            placed: None,
        }
    }
}

/// Touched walkable ground after being airborne. `fall_height` counts from the highest
/// point since leaving the ground, gliding included: base fall damage on
/// `impact_speed` (m/s downward), which a glider keeps low from any height.
///
/// `tier` and `jumped` need `landing_tiers` in the [`CharacterParams`]: `tier` is how many of
/// the impact-speed thresholds were reached (0: none), `jumped` whether the fall followed a
/// jump rather than a ledge walked off. Without tiers they stay 0 and `false`. A landing in
/// water deeper than `landing_wade_limit` is not reported at all.
#[derive(Event, Clone, Copy, Debug, PartialEq)]
pub struct Landed {
    pub entity: Entity,
    pub fall_height: f32,
    pub impact_speed: f32,
    pub tier: u8,
    pub jumped: bool,
}

/// On a character in the air after a jump, while its params have `landing_tiers`: what tells
/// a landing after a jump from one after a fall. Put by the controller on the jump and taken
/// off when the feet touch ground, water or a ledge again; never there with no tiers.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Component, Default)]
pub struct JumpedFlight;

/// One-shot: the controller launches the body up so that it peaks `height` metres above the
/// take-off, with the body's own gravity (`v = sqrt(2 * gravity * height)`), then removes this.
/// The body must stand on the ground (or be within its coyote time of it, as a jump asks); a body
/// in the air or in water is not launched and the component goes all the same. It launches even
/// while an action owns the body (`LocomotionInput::locked`): an action's leap is the one that asks.
/// Put by whoever owns the body's state (the action machine for a `leap`), in `SimSet::Act`, so
/// that the controller reads it in the same tick. A launch is a jump for landing tiers
/// ([`JumpedFlight`]) but not a [`Jumped`] event: no one pressed jump.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct Launch {
    pub height: f32,
}

/// The body passes through other characters: the separation pass skips every pair it is a member
/// of. Put on and taken off by whoever owns the body's state (the action machine for the
/// `ghost` windows of an action). It does not change what the body collides with in the world.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Component, Default)]
pub struct Ghost;

#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeChanged {
    pub entity: Entity,
    pub from: MotionMode,
    pub to: MotionMode,
}

#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jumped {
    pub entity: Entity,
}

/// Started climbing onto a ledge `height` meters above the feet.
#[derive(Event, Clone, Copy, Debug, PartialEq)]
pub struct Mantled {
    pub entity: Entity,
    pub height: f32,
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerSystems {
    /// Inside `SimSet::Move`: [`move_characters`].
    Move,
    /// Inside `NetSet::Predict`, last: [`move_characters`] for a client's own body
    /// ([`Predicted`](shards_core::Predicted)).
    PredictMove,
}

/// The character controller (adds [`PhysicsPlugin`] if it is missing).
pub struct ControllerPlugin;

impl Plugin for ControllerPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<PhysicsPlugin>() {
            app.add_plugins(PhysicsPlugin);
        }
        app.add_content::<CharacterParams>()
            .register_type::<CharacterParams>()
            .register_type::<SprintResume>()
            .register_type::<Facing>()
            .register_type::<AccelModel>()
            .register_type::<CharacterController>()
            .register_type::<LocomotionInput>()
            .register_type::<MotionMode>()
            .register_type::<Mantle>()
            .register_type::<JumpedFlight>()
            // Like the glide latch: the own body's on a client.
            .net_predicted::<JumpedFlight>()
            .register_type::<Launch>()
            .register_type::<Ghost>()
            .register_type::<Crouch>()
            // The own body's on a client: its `crouch` input runs there.
            .net_predicted::<Crouch>()
            // The host's `Ghost` does not reach a client's own body: the client puts and takes it itself.
            .net_predicted::<Ghost>()
            .register_type::<MotionState>()
            // A client moves its own body itself and is corrected toward the host's.
            .net_predicted::<Transform>()
            .net_predicted::<MotionState>()
            .net_predicted::<LocomotionInput>()
            .configure_sets(FixedUpdate, ControllerSystems::Move.in_set(SimSet::Move))
            .configure_sets(
                FixedUpdate,
                ControllerSystems::PredictMove.in_set(NetSet::Predict).after(crate::MotionSystems::PredictIntent),
            )
            .add_systems(
                FixedUpdate,
                (
                    move_characters::<()>.in_set(ControllerSystems::Move),
                    move_characters::<With<Predicted>>.in_set(ControllerSystems::PredictMove),
                ),
            );
        crate::modifiers::plugin(app);
        crate::predict::plugin(app);
    }
}

/// `commands.spawn(character("hero", feet))`: a character moving by the `locomotion`
/// definition `params` (empty: code defaults), standing at `feet`, driven through
/// `Intent`.
pub fn character(params: &str, feet: Vec3) -> (CharacterController, Intent, Transform) {
    (CharacterController::new(params), Intent::default(), Transform::from_translation(feet))
}

type CharacterQuery<'w, 's, F = ()> = Query<
    'w,
    's,
    (
        Entity,
        &'static ColliderId,
        &'static CharacterController,
        Option<&'static CharacterParams>,
        &'static mut LocomotionInput,
        &'static mut MotionState,
        &'static mut Transform,
        &'static mut Collider,
        Option<&'static MoveModifiers>,
        Has<JumpedFlight>,
        Option<&'static Launch>,
        Has<Ghost>,
        Option<&'static Intent>,
        Option<&'static mut Crouch>,
    ),
    F,
>;

/// Where a character ended up, for the separation pass.
struct Placed {
    entity: Entity,
    feet: Vec3,
    radius: f32,
    height: f32,
    /// Climbing: follows its checked path, others give way.
    pinned: bool,
    /// Passes through other characters ([`Ghost`]).
    ghost: bool,
    params: CharacterParams,
}

/// Moves every character one tick, in [`ColliderId`] order, then pushes overlapping
/// characters apart.
///
/// `F` picks the characters: `()` in the simulation, `With<Predicted>` where a client
/// predicts its own body (it is pushed apart from no one there: the host does that).
pub fn move_characters<F: QueryFilter + 'static>(
    time: Res<Time>,
    physics: Res<PhysicsWorld>,
    db: Option<Res<ContentDb>>,
    role: Option<Res<NetRole>>,
    mut commands: Commands,
    mut characters: CharacterQuery<F>,
    mut dangling: Local<HashSet<String>>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let db = db.as_deref();
    let mut order: Vec<(ColliderId, Entity)> = characters.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();
    let mut placed = Vec::with_capacity(order.len());
    for &(_, entity) in &order {
        let Ok((_, _, controller, own, mut input, mut state, mut tf, mut collider, modifiers, flight, launch, ghost, intent, mut stance)) =
            characters.get_mut(entity)
        else {
            continue;
        };
        let modifiers = modifiers.unwrap_or(&MoveModifiers::NEUTRAL);
        if controller.is_dangling(own, db) && dangling.insert(controller.params.clone()) {
            let text = format!(
                "{entity}: locomotion `{}` is not defined in content, so its character moves by the engine defaults; \
                 define it in content/**/*.locomotion.ron or set CharacterController::params to one that exists",
                controller.params
            );
            commands.queue(move |world: &mut World| {
                journal::emit(world, JournalKind::Warn, text);
            });
        }
        let params = controller.resolve(own, db);
        if launch.is_some() {
            // Taken or not, a launch is for this tick only.
            commands.entity(entity).try_remove::<Launch>();
        }
        let mut now = sanitized(&input);
        // A root takes the body from the walker the way an action does; `external` and gravity stay.
        if modifiers.rooted {
            now.locked = true;
        }
        if input.jump || input.external != Vec3::ZERO {
            input.jump = false;
            input.external = Vec3::ZERO;
        }
        if !tf.translation.is_finite() {
            warn!("{entity}: character at {} cannot move; teleport it somewhere real", tf.translation);
            continue;
        }
        // The stance (F2): the crouch input moves the capsule's height, and in the air its bottom (the feet)
        // with it. The controller's casts, the body's collider and the separation pass all use the size it
        // has now. A body that cannot crouch and never did goes through none of this.
        let before = stance.as_deref().copied().unwrap_or_default();
        let mut settled = None;
        if params.can_crouch() || before != Crouch::default() {
            let held = intent.is_some_and(|intent| intent.is_held(CROUCH));
            let s = stance::settle(&physics, params, (entity, collider.layers), state.mode, tf.translation, before, held, modifiers.rooted, dt);
            // A body someone else moved this tick restarts afresh in `Step::run`, from where it was put: the
            // feet shift and the controller's own record of them move together, or it would take its own
            // shift for a teleport.
            if s.lift != 0.0 && state.placed == Some(tf.translation) {
                tf.translation.y += s.lift;
                state.placed = Some(tf.translation);
            }
            match (stance.as_deref_mut(), s.crouch) {
                (Some(kept), now) if *kept != now => *kept = now,
                (None, now) if now != Crouch::default() => {
                    commands.entity(entity).try_insert(now);
                }
                _ => {}
            }
            settled = Some(s);
        }
        let depth = settled.map_or(0.0, |s| s.crouch.depth);
        let sized = params.at_depth(depth);
        let params: &CharacterParams = &sized;
        // The collider is the host's to size: a client predicts the stance (`Crouch`, the feet) and leaves the shape the
        // host replicates, or the prediction would change a component that is not predicted and the desync check would see it.
        if role.as_deref().is_none_or(|role| role.simulates()) {
            fit(&mut collider, &params.collider());
        }
        let mover = Mover::new(&physics, params, entity, collider.layers);
        let facing = flat(*tf.forward()).try_normalize().unwrap_or(Vec3::NEG_Z);
        let mut step = Step {
            mover: &mover,
            params,
            modifiers,
            input: &now,
            state: &mut state,
            dt,
            facing,
            jumped_flight: flight,
            launch: launch.map(|l| l.height),
            crouched: depth > 0.0,
            look: intent.and_then(|intent| params.bound_look(intent)),
            notes: Vec::new(),
        };
        let feet = step.run(tf.translation);
        let notes = std::mem::take(&mut step.notes);
        if tf.translation != feet {
            tf.translation = feet;
        }
        if let Some(s) = settled {
            if s.crouched {
                commands.trigger(Crouched { entity });
            }
            if s.stood {
                commands.trigger(Stood { entity });
            }
        }
        match intent.and_then(|intent| params.bound_look(intent)) {
            // A body that faces its look has its yaw, whatever it walks, runs or climbs: no turn rate.
            Some(gaze) => face_exactly(&mut tf, gaze.yaw),
            None => {
                let look = look_dir(&now, &state, params.turn_toward_input);
                let turn_speed = turn_rate(params, modifiers, state.mode, now.face.is_some() && look.is_some());
                turn(&mut tf, look, turn_speed, dt);
            }
        }
        let tracks = params.tracks_landings();
        for note in notes {
            trigger(&mut commands, entity, note, tracks);
        }
        let pinned = state.mode == MotionMode::Mantle;
        let (radius, height) = (mover.radius, params.height.max(2.0 * mover.radius));
        placed.push(Placed { entity, feet, radius, height, pinned, ghost, params: params.clone() });
    }
    separate(&physics, &mut characters, &placed);
    for body in &placed {
        if let Ok((.., mut state, tf, _, _, _, _, _, _, _)) = characters.get_mut(body.entity) {
            state.placed = Some(tf.translation);
        }
    }
}

/// The input with anything non-finite dropped: a NaN direction from a buggy brain must
/// not poison the position (and the collision queries behind it).
fn sanitized(input: &LocomotionInput) -> LocomotionInput {
    let finite = |v: Vec3| if v.is_finite() { v } else { Vec3::ZERO };
    LocomotionInput {
        move_dir: finite(input.move_dir),
        face: input.face.filter(|f| f.is_finite()),
        external: finite(input.external),
        ..input.clone()
    }
}

/// `tracks`: the params tell landings apart, so the flight after a jump is marked.
fn trigger(commands: &mut Commands, entity: Entity, note: Note, tracks: bool) {
    match note {
        Note::Mode(from, to) => {
            // Ground, water, a ledge and a ladder end the flight, whether or not a landing was reported.
            if tracks && matches!(to, MotionMode::Ground | MotionMode::Water | MotionMode::Mantle | MotionMode::Ladder) {
                commands.entity(entity).try_remove::<JumpedFlight>();
            }
            commands.trigger(ModeChanged { entity, from, to })
        }
        Note::Launched => {
            if tracks {
                commands.entity(entity).try_insert(JumpedFlight);
            }
        }
        Note::Jumped => {
            if tracks {
                commands.entity(entity).try_insert(JumpedFlight);
            }
            commands.trigger(Jumped { entity })
        }
        Note::Landed { fall_height, impact_speed, tier, jumped } => {
            commands.trigger(Landed { entity, fall_height, impact_speed, tier, jumped })
        }
        Note::Mantled { height } => commands.trigger(Mantled { entity, height }),
        Note::Entered(volume) => commands.trigger(VolumeEntered { entity, volume }),
        Note::Exited(volume) => commands.trigger(VolumeExited { entity, volume }),
    }
}

/// The turning speed (degrees per second) for this tick, by who asks for the facing: a swimmer's own
/// rate in the water, else a stance's ([`MoveModifiers::turn_speed_deg`], a raised guard), else the
/// locked rate when the input names a facing (`faced`: a lock-on, an aim point, an action's facing),
/// else the free rate. A rate the params leave at 0 falls back to the next one.
fn turn_rate(params: &CharacterParams, modifiers: &MoveModifiers, mode: MotionMode, faced: bool) -> f32 {
    if mode == MotionMode::Water && params.swim_turn_speed_deg > 0.0 {
        params.swim_turn_speed_deg
    } else if let Some(stance) = modifiers.turn_speed_deg {
        stance
    } else if faced && params.locked_turn_speed_deg > 0.0 {
        params.locked_turn_speed_deg
    } else {
        params.turn_speed_deg
    }
}

/// Where to face: the input's `face`, else the climb, else (`toward_input`) the movement input, else the
/// way the character moves.
fn look_dir(input: &LocomotionInput, state: &MotionState, toward_input: bool) -> Option<Vec3> {
    if let Some(face) = input.face.and_then(|f| flat(f).try_normalize()) {
        return Some(face);
    }
    if let Some(climb) = state.mantle {
        return flat(climb.to - climb.from).try_normalize();
    }
    // Asked to, a body with a movement input turns to it, even when a wall holds its velocity at zero.
    if toward_input && !input.locked {
        let wish = flat(input.move_dir);
        if wish.length() > 0.1 {
            return Some(wish.normalize());
        }
    }
    let moving = flat(state.velocity);
    (!input.locked && moving.length() > 0.1).then(|| moving.normalize())
}

/// Turns the transform to face `yaw` (radians, as `Gaze::yaw`: 0 faces -Z) at once, upright.
fn face_exactly(tf: &mut Mut<Transform>, yaw: f32) {
    let want = Quat::from_rotation_y(yaw);
    if tf.rotation != want {
        tf.rotation = want;
    }
}

/// Turns the transform about Y toward `dir` at `speed_deg` per second (0: at once).
fn turn(tf: &mut Mut<Transform>, dir: Option<Vec3>, speed_deg: f32, dt: f32) {
    let Some(dir) = dir else {
        return;
    };
    let forward = tf.forward();
    let yaw = |d: Vec3| ops::atan2(-d.x, -d.z);
    let (current, target) = (yaw(*forward), yaw(dir));
    let mut delta = target - current;
    delta = (delta + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    if delta.abs() < 1e-5 && forward.y.abs() < 1e-5 {
        return;
    }
    let max = if speed_deg > 0.0 { speed_deg.to_radians() * dt } else { f32::INFINITY };
    tf.rotation = Quat::from_rotation_y(current + delta.clamp(-max, max));
}

/// Overlapping capsules (sharing some height) are pushed apart on the ground plane,
/// half each, in a fixed pair order; the pushes go through move-and-slide so nobody
/// ends up inside a wall.
fn separate<F: QueryFilter>(physics: &PhysicsWorld, characters: &mut CharacterQuery<F>, placed: &[Placed]) {
    let mut push = vec![Vec3::ZERO; placed.len()];
    for i in 0..placed.len() {
        for j in (i + 1)..placed.len() {
            let (a, b) = (&placed[i], &placed[j]);
            if a.ghost || b.ghost {
                continue;
            }
            if a.feet.y >= b.feet.y + b.height || b.feet.y >= a.feet.y + a.height {
                continue;
            }
            let d = flat(b.feet - a.feet);
            let dist = d.length();
            let overlap = a.radius + b.radius - dist;
            if overlap <= 0.0 {
                continue;
            }
            let n = if dist > 1e-5 { d / dist } else { Vec3::X };
            let (wa, wb) = match (a.pinned, b.pinned) {
                (true, true) => continue,
                (true, false) => (0.0, 1.0),
                (false, true) => (1.0, 0.0),
                (false, false) => (0.5, 0.5),
            };
            push[i] -= n * overlap * wa;
            push[j] += n * overlap * wb;
        }
    }
    for (body, push) in placed.iter().zip(push) {
        if push == Vec3::ZERO {
            continue;
        }
        let Ok((.., mut tf, collider, _, _, _, _, _, _)) = characters.get_mut(body.entity) else {
            continue;
        };
        let mover = Mover::new(physics, &body.params, body.entity, collider.layers);
        let feet = mover.depenetrate(mover.slide(body.feet, push, true).feet);
        if tf.translation != feet {
            tf.translation = feet;
        }
    }
}
