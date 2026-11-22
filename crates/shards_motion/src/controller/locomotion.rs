//! One character, one tick: the movement modes and the transitions between them.
//!
//! Velocities change over the tick and positions move by the average of the old and
//! the new velocity: exact for constant acceleration, so a jump peaks at `jump_height`
//! whatever the tick rate.

use bevy::prelude::*;
use shards_core::Gaze;

use super::accel::{air_accelerate, apply_friction, ground_accelerate, limit_air_speed, AccelModel};
use super::mover::{clip, flat, Ladder, Mover, Slide, Touch};
use super::params::{CharacterParams, MANTLE_SLACK};
use super::{LocomotionInput, Mantle, MotionMode, MotionState};
use crate::modifiers::MoveModifiers;

/// Landing is noticed this far above the ground (m).
const LAND_PROBE: f32 = 0.05;
/// A swimmer stands up where the water is this much shallower than `swim_depth` (m).
const SHALLOW_MARGIN: f32 = 0.1;
/// How fast a swimmer drifts to the floating height (1/s of the height error).
const FLOAT_RATE: f32 = 4.0;
/// A ledge is climbed, and a ladder taken hold of, when the input points at it within 60°.
const PUSH_COS: f32 = 0.5;
/// A ladder is taken hold of where something solid stops the body within this far into it (m): the wall
/// it hangs on, or its own collider.
const LADDER_BLOCKED: f32 = 0.05;
/// A body in the air that moves away from a ladder faster than this (m/s) does not take hold of it: a
/// jump off a ladder lets go of it.
const LADDER_LEAVING: f32 = 0.1;
/// A cast that comes up this much short of its length (m) went the whole way: rounding.
const CAST_SLACK: f32 = 1e-4;

/// What happened, for the events the system triggers afterwards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Note {
    Mode(MotionMode, MotionMode),
    Jumped,
    /// A [`Launch`](super::Launch) took the body up: a jump for landing tiers, with no event.
    Launched,
    Landed { fall_height: f32, impact_speed: f32, tier: u8, jumped: bool },
    Mantled { height: f32 },
    /// The body jumped off a ladder (`true`: [`LadderLetGo`](super::LadderLetGo) goes on), or no longer
    /// pushes into one after such a jump (`false`: it comes off).
    LadderLetGo(bool),
    Entered(Entity),
    Exited(Entity),
}

pub(crate) struct Step<'a, 'w> {
    pub mover: &'a Mover<'w>,
    pub params: &'a CharacterParams,
    /// What slows, caps or roots the character this tick.
    pub modifiers: &'a MoveModifiers,
    /// This tick's input, `jump` and `external` included (the component's are consumed).
    pub input: &'a LocomotionInput,
    pub state: &'a mut MotionState,
    pub dt: f32,
    /// Where the character faces, on the ground plane.
    pub facing: Vec3,
    /// The fall this tick continues began with a jump (tracked with landing tiers only).
    pub jumped_flight: bool,
    /// A launch asked for this tick: the height (m) the body should peak at.
    pub launch: Option<f32>,
    /// The body is below its standing height (F2): on the ground it walks no faster than the params'
    /// `crouch_speed` and never sprints.
    pub crouched: bool,
    /// The look the body's yaw is bound to (`face: Look`): what `ladder_by_look` climbs by.
    pub look: Option<Gaze>,
    /// The body jumped off a ladder and has pushed into it ever since
    /// ([`LadderLetGo`](super::LadderLetGo)): it takes hold of none.
    pub let_go: bool,
    pub notes: Vec<Note>,
}

/// The movement input as a ladder reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LadderWish {
    /// Up the ladder, in `ladder_speed`s: 1 climbs at full speed, negative goes down.
    climb: f32,
    /// Along the ladder's face, level: sideways.
    side: Vec3,
    /// How much of the input points into the ladder (negative: away from it), whatever the look.
    into: f32,
}

impl Step<'_, '_> {
    /// Runs the tick and returns the new feet position.
    pub fn run(&mut self, feet: Vec3) -> Vec3 {
        if self.state.placed.is_some_and(|at| at != feet) {
            self.restart(feet);
        }
        if self.input.jump {
            self.state.jump_buffer_left = self.params.jump_buffer;
        }
        // A climb follows a path checked when it started; everything else first gets
        // out of whatever it was pushed into.
        let climbing = self.state.mode == MotionMode::Mantle;
        let feet = if climbing { feet } else { self.mover.depenetrate(feet) };
        let feet = match self.state.mode {
            // A launch asked for by the body's owner: on the ground, or within coyote time of it.
            MotionMode::Ground | MotionMode::Air if self.launches() => self.launch(feet),
            MotionMode::Mantle => self.mantle(feet),
            MotionMode::Ladder => self.ladder(feet),
            MotionMode::Ground => self.ground(feet),
            MotionMode::Air | MotionMode::Glide => self.air(feet),
            MotionMode::Water => self.water(feet),
        };
        let feet = if self.state.mode == MotionMode::Mantle { feet } else { self.mover.depenetrate(feet) };
        let feet = self.surroundings(feet);
        // A jump off a ladder is over once the body stops pushing into the ladder, or is out of the air. The
        // jump's own tick only set it.
        if self.let_go
            && !self.notes.contains(&Note::LadderLetGo(true))
            && !(matches!(self.state.mode, MotionMode::Air | MotionMode::Glide) && self.pushes_into_ladder(feet).is_some())
        {
            self.notes.push(Note::LadderLetGo(false));
        }
        let dt = self.dt;
        let s = &mut *self.state;
        s.jump_buffer_left = (s.jump_buffer_left - dt).max(0.0);
        if s.mode != MotionMode::Ground {
            s.coyote_left = (s.coyote_left - dt).max(0.0);
        }
        feet
    }

    /// Someone else moved the body (a teleport, a respawn): the speed, the fall and the
    /// climb belonged to where it was. Standing if there is ground under the feet, else
    /// falling from here, from rest.
    fn restart(&mut self, feet: Vec3) {
        let standing = self.mover.ground(feet, self.params.snap_distance).is_some_and(|g| g.walkable);
        let s = &mut *self.state;
        s.velocity = Vec3::ZERO;
        s.mantle = None;
        s.fall_from_y = None;
        s.coyote_left = 0.0;
        s.grounded = false;
        s.ground_normal = Vec3::Y;
        self.set_mode(if standing { MotionMode::Ground } else { MotionMode::Air });
    }

    fn set_mode(&mut self, to: MotionMode) {
        let from = self.state.mode;
        if from != to {
            self.state.mode = to;
            self.notes.push(Note::Mode(from, to));
        }
    }

    /// Movement input on the ground plane, nothing while an action owns the body.
    fn wish(&self) -> Vec3 {
        if self.input.locked { Vec3::ZERO } else { flat(self.input.move_dir).clamp_length_max(1.0) }
    }

    // --- ground ---------------------------------------------------------------------

    fn ground(&mut self, feet: Vec3) -> Vec3 {
        let p = self.params;
        if self.state.jump_buffer_left > 0.0 && !self.input.locked {
            return self.jump(feet);
        }
        if self.takes_hold(feet) {
            return self.hold(feet);
        }
        let wish = self.wish();
        let (speed, sprinting) = self.ground_speed(wish.length());
        self.state.sprinting = sprinting;
        let target = wish.normalize_or_zero() * speed;
        let v0 = flat(self.state.velocity);
        let mut v = match p.accel {
            AccelModel::Step => {
                let braking = if p.brake_only_without_input {
                    wish == Vec3::ZERO
                } else {
                    target.length_squared() < v0.length_squared()
                };
                let rate = if braking { self.deceleration() } else { self.acceleration() };
                step_toward(v0, target, rate * self.dt, p.axis_stepping)
            }
            // Friction, then a push along the wished direction up to the wished speed (F7).
            AccelModel::Projected { ground_accel, friction, stop_speed, .. } => {
                let slowed = apply_friction(v0, friction, stop_speed, self.dt);
                ground_accelerate(slowed, wish.normalize_or_zero(), speed, ground_accel, self.dt)
            }
        };
        let delta = along_ground((v0 + v) * 0.5 * self.dt, self.state.ground_normal) + self.input.external;
        let (moved, stepped) = self.walk(feet, delta);
        for touch in moved.touches.iter().filter(|t| !t.walkable) {
            v = clip(v, touch.plane);
        }
        self.state.velocity = v;
        if !stepped
            && let Some(wall) = moved.wall()
            && let Some(feet) = self.try_mantle(moved.feet, &wall, p.step_height, true, None, p.mantle_height)
        {
            return feet;
        }
        self.stick(moved.feet)
    }

    /// The speed of a walk on the ground for an input of length `amount`, and whether it is a sprint:
    /// the modifiers', and a crouched body's `crouch_speed` as one more cap that takes the sprint away.
    fn ground_speed(&self, amount: f32) -> (f32, bool) {
        if !self.crouched {
            return self.modifiers.ground_speed(self.params, amount, self.input.sprint);
        }
        let mut crouched = *self.modifiers;
        crouched.forbid_sprint();
        if let Some(cap) = self.params.crouch_speed {
            crouched.cap_at(cap);
        }
        crouched.ground_speed(self.params, amount, self.input.sprint)
    }

    /// Slide along the ground; when something steep blocks the way, also try stepping
    /// over it and keep whichever got further. `true`: the step won.
    fn walk(&self, feet: Vec3, delta: Vec3) -> (Slide, bool) {
        let plain = self.mover.slide(feet, delta, true);
        if plain.touches.iter().all(|t| t.walkable) {
            return (plain, false);
        }
        let want = flat(delta);
        let Some(dir) = want.try_normalize() else {
            return (plain, false);
        };
        let Some(stepped) = self.mover.step_up(feet, want, self.params.step_height) else {
            return (plain, false);
        };
        let progress = |s: &Slide| flat(s.feet - feet).dot(dir);
        if progress(&stepped) > progress(&plain) + 1e-4 { (stepped, true) } else { (plain, false) }
    }

    /// Keeps the feet on walkable ground within snap distance, or starts falling.
    fn stick(&mut self, feet: Vec3) -> Vec3 {
        match self.mover.ground(feet, self.params.snap_distance) {
            Some(ground) if ground.walkable => {
                let s = &mut *self.state;
                s.grounded = true;
                s.ground_normal = ground.normal;
                s.coyote_left = self.params.coyote_time;
                s.velocity.y = 0.0;
                feet - Vec3::Y * ground.drop
            }
            _ => {
                // Off an edge inside a ladder's volume (its top, walked or backed off): onto the ladder, not
                // into the fall. The next ticks climb down by the input that walked it off.
                if self.params.can_climb_ladders() && !self.input.locked && self.mover.ladder(feet).is_some_and(|l| l.front) {
                    self.hold_ladder();
                    return feet;
                }
                let s = &mut *self.state;
                s.grounded = false;
                s.ground_normal = Vec3::Y;
                s.fall_from_y = Some(feet.y);
                s.velocity.y = s.velocity.y.min(0.0);
                self.set_mode(MotionMode::Air);
                feet
            }
        }
    }

    /// Whether a [`Launch`](super::Launch) takes the body up this tick: it stands on the ground, or
    /// walked off it within `coyote_time` (the test a jump makes, without its buffer), and an action
    /// that owns the body does not stop it.
    fn launches(&self) -> bool {
        self.launch.is_some()
            && match self.state.mode {
                MotionMode::Ground => true,
                MotionMode::Air => self.state.coyote_left > 0.0,
                _ => false,
            }
    }

    /// Launches the body up so that it peaks `launch` metres above the feet, then the tick goes on
    /// in the air.
    fn launch(&mut self, feet: Vec3) -> Vec3 {
        let height = self.launch.unwrap_or(0.0).max(0.0);
        let s = &mut *self.state;
        s.velocity.y = (2.0 * self.params.gravity.max(0.0) * height).sqrt();
        s.coyote_left = 0.0;
        s.grounded = false;
        s.ground_normal = Vec3::Y;
        s.fall_from_y = Some(feet.y);
        self.notes.push(Note::Launched);
        self.set_mode(MotionMode::Air);
        self.air(feet)
    }

    fn jump(&mut self, feet: Vec3) -> Vec3 {
        let s = &mut *self.state;
        s.velocity.y = self.params.jump_speed();
        s.jump_buffer_left = 0.0;
        s.coyote_left = 0.0;
        s.grounded = false;
        s.ground_normal = Vec3::Y;
        s.fall_from_y = Some(feet.y);
        self.notes.push(Note::Jumped);
        self.set_mode(MotionMode::Air);
        self.air(feet)
    }

    // --- air and glide ----------------------------------------------------------------

    fn air(&mut self, feet: Vec3) -> Vec3 {
        let p = self.params;
        let locked = self.input.locked;
        if self.state.mode == MotionMode::Air
            && self.state.jump_buffer_left > 0.0
            && self.state.coyote_left > 0.0
            && !locked
        {
            return self.jump(feet);
        }
        if self.takes_hold(feet) {
            return self.hold(feet);
        }
        let glide = self.input.glide && !locked;
        if self.state.mode == MotionMode::Air && glide && self.state.velocity.y < 0.0 {
            self.set_mode(MotionMode::Glide);
        } else if self.state.mode == MotionMode::Glide && !glide {
            self.set_mode(MotionMode::Air);
        }

        let v0 = self.state.velocity;
        let (h0, dt) = (flat(v0), self.dt);
        let wish = self.wish();
        if p.sprint_in_air {
            // Sprint is decided every tick, as on the ground: a glide is none.
            self.state.sprinting = self.state.mode != MotionMode::Glide
                && self.modifiers.ground_speed(p, wish.length(), self.input.sprint).1;
        }
        let (h, vy) = if self.state.mode == MotionMode::Glide {
            let dir = wish.try_normalize().or(h0.try_normalize()).unwrap_or(self.facing);
            let h = approach(h0, dir * p.glide_speed, p.glide_accel * dt);
            let vy = match self.state.updraft {
                Some(updraft) => (v0.y + updraft.x * dt).min(updraft.y),
                None => (v0.y - p.gravity * dt).max(-p.glide_fall_speed),
            };
            (h, vy)
        } else {
            let h = match wish.try_normalize() {
                Some(dir) => {
                    let (speed, _) = self.modifiers.ground_speed(p, wish.length(), self.input.sprint);
                    match p.accel {
                        AccelModel::Step => {
                            step_toward(h0, dir * speed, self.acceleration() * p.air_control * dt, p.axis_stepping)
                        }
                        // The push along the wished direction, to the capped speed; no friction in the air (F7).
                        AccelModel::Projected { air_accel, air_speed_cap, max_air_speed, .. } => {
                            let pushed = air_accelerate(h0, dir, speed, air_accel, air_speed_cap, dt);
                            max_air_speed.map_or(pushed, |max| limit_air_speed(h0, pushed, max))
                        }
                    }
                }
                // No input: the air keeps its speed unless the params brake it. The modifiers' `deceleration` override
                // changes that braking for a `Step` body; under `Projected` it steers nothing (F7).
                None if p.air_deceleration > 0.0 => {
                    let rate = match p.accel {
                        AccelModel::Step => self.modifiers.deceleration.unwrap_or(p.air_deceleration),
                        AccelModel::Projected { .. } => p.air_deceleration,
                    };
                    step_toward(h0, Vec3::ZERO, rate * dt, p.axis_stepping)
                }
                None => h0,
            };
            (h, (v0.y - p.gravity * dt).max(-p.terminal_speed))
        };
        let mut v = h + Vec3::Y * vy;
        let moved = self.mover.slide(feet, (v0 + v) * 0.5 * dt + self.input.external, false);
        for touch in &moved.touches {
            v = clip(v, touch.normal);
        }
        self.state.velocity = v;
        // A lip no higher than a step is no ledge: the fall ends on the tread below it,
        // and the walk steps up.
        if let Some(wall) = moved.wall()
            && let Some(feet) = self.try_mantle(moved.feet, &wall, p.step_height, true, None, p.mantle_height)
        {
            return feet;
        }

        let feet = moved.feet;
        let s = &mut *self.state;
        s.fall_from_y = Some(s.fall_from_y.map_or(feet.y, |top| top.max(feet.y)));
        if vy <= 0.0
            && let Some(ground) = self.mover.ground(feet, LAND_PROBE)
            && ground.walkable
        {
            // The bottom of water deep enough to swim in: a fast fall can pass the whole
            // float depth in one tick. That is a swim, not a landing.
            if self.mover.water(feet).is_some_and(|surface| self.depth(feet, surface) > p.swim_depth) {
                self.enter_water();
                return feet;
            }
            let feet = feet - Vec3::Y * ground.drop;
            let s = &mut *self.state;
            let fall_height = s.fall_from_y.take().map_or(0.0, |top| (top - feet.y).max(0.0));
            s.velocity.y = 0.0;
            s.grounded = true;
            s.ground_normal = ground.normal;
            s.coyote_left = p.coyote_time;
            // Shallows break a fall: no landing is reported where the water is deeper than the limit.
            let wet = p.landing_wade_limit.is_some_and(|limit| {
                self.mover.water(feet).is_some_and(|surface| surface - feet.y > limit)
            });
            if !wet {
                let jumped = self.jumped_flight;
                let tier = p.landing_tier(-vy, jumped);
                self.notes.push(Note::Landed { fall_height, impact_speed: -vy, tier, jumped });
            }
            self.set_mode(MotionMode::Ground);
            return feet;
        }
        feet
    }

    // --- water ----------------------------------------------------------------------

    fn water(&mut self, feet: Vec3) -> Vec3 {
        let p = self.params;
        let Some(surface) = self.state.water_surface else {
            self.state.fall_from_y = Some(feet.y);
            self.set_mode(MotionMode::Air);
            return self.air(feet);
        };
        // A ladder out of the water is taken hold of like any other.
        if self.takes_hold(feet) {
            return self.hold(feet);
        }
        // Jump in water: climb out where there is a ledge ahead.
        if self.state.jump_buffer_left > 0.0 && !self.input.locked {
            let dir = self.wish().try_normalize().unwrap_or(self.facing);
            // A surface-relative window when the params have one, else the ordinary reach.
            let reach = p.climb_out_max_height.map_or(p.mantle_height, |limit| {
                (surface + limit + self.mover.skin - feet.y).max(0.0)
            });
            if let Some(wall) = self.wall_ahead(feet, dir)
                && let Some(feet) = self.try_mantle(feet, &wall, 0.0, false, None, reach)
            {
                self.state.jump_buffer_left = 0.0;
                return feet;
            }
        }
        let v0 = self.state.velocity;
        let wish = self.wish();
        let fast = p.swim_fast_speed > 0.0
            && self.input.sprint
            && !self.modifiers.no_sprint
            && wish != Vec3::ZERO;
        let top = self.modifiers.swim_speed(if fast { p.swim_fast_speed } else { p.swim_speed });
        let rate = swim_rate(p, wish != Vec3::ZERO);
        let h = step_toward(flat(v0), wish * top, rate * self.dt, p.axis_stepping);
        let float_y = surface - p.float_depth;
        let (mut v, travel) = if p.buoyancy_omega > 0.0 {
            let (vy, dy) = buoyancy(p, feet.y - float_y, v0.y, self.dt);
            (h + Vec3::Y * vy, (flat(v0) + h) * 0.5 * self.dt + Vec3::Y * dy)
        } else {
            let vy = ((float_y - feet.y) * FLOAT_RATE).clamp(-p.swim_speed, p.swim_speed);
            let v = h + Vec3::Y * vy;
            (v, (v0 + v) * 0.5 * self.dt)
        };
        let moved = self.mover.slide(feet, travel + self.input.external, false);
        for touch in &moved.touches {
            v = clip(v, touch.normal);
        }
        self.state.velocity = v;
        self.state.sprinting = fast;
        // Pushing against a ledge climbs it by itself up to the auto height above the surface
        // (when the params have one); a higher ledge needs the jump above.
        let auto = p.climb_out_auto_height.map(|height| surface + height);
        if let Some(wall) = moved.wall()
            && let Some(feet) = self.try_mantle(moved.feet, &wall, 0.0, true, auto, p.mantle_height)
        {
            return feet;
        }
        moved.feet
    }

    /// Water, updrafts and volume events at the new position; water changes the mode.
    fn surroundings(&mut self, mut feet: Vec3) -> Vec3 {
        let p = self.params;
        let around = self.mover.surroundings(feet);
        for volume in &self.state.volumes {
            if !around.volumes.contains(volume) {
                self.notes.push(Note::Exited(*volume));
            }
        }
        for volume in &around.volumes {
            if !self.state.volumes.contains(volume) {
                self.notes.push(Note::Entered(*volume));
            }
        }
        self.state.volumes = around.volumes;
        self.state.water_surface = around.water;
        self.state.updraft = around.updraft.map(|(accel, max)| Vec2::new(accel, max));

        let deep = |feet: Vec3, surface: f32| self.depth(feet, surface);
        match (self.state.mode, around.water) {
            (MotionMode::Ground, Some(surface)) if deep(feet, surface) > p.swim_depth => self.enter_water(),
            (MotionMode::Air | MotionMode::Glide, Some(surface))
                if feet.y <= surface - p.float_depth && deep(feet, surface) > p.swim_depth =>
            {
                self.enter_water()
            }
            (MotionMode::Water, None) => {
                self.state.fall_from_y = Some(feet.y);
                self.set_mode(MotionMode::Air);
            }
            (MotionMode::Water, Some(surface)) if deep(feet, surface) < p.swim_depth - SHALLOW_MARGIN => {
                // Shallow enough to stand: down onto the bottom, or fall to it.
                let reach = p.snap_distance + (p.swim_depth - p.float_depth);
                match self.mover.ground(feet, reach) {
                    Some(ground) if ground.walkable => {
                        feet -= Vec3::Y * ground.drop;
                        let s = &mut *self.state;
                        s.grounded = true;
                        s.ground_normal = ground.normal;
                        s.velocity.y = 0.0;
                        self.set_mode(MotionMode::Ground);
                    }
                    _ => {
                        self.state.fall_from_y = Some(feet.y);
                        self.set_mode(MotionMode::Air);
                    }
                }
            }
            _ => {}
        }
        feet
    }

    /// How deep the water with this surface is at the feet; anything a meter past
    /// `swim_depth` counts as that.
    fn depth(&self, feet: Vec3, surface: f32) -> f32 {
        self.mover.depth(feet, surface, self.params.swim_depth + 1.0)
    }

    fn enter_water(&mut self) {
        let p = self.params;
        let s = &mut *self.state;
        s.grounded = false;
        s.ground_normal = Vec3::Y;
        s.fall_from_y = None;
        s.coyote_left = 0.0;
        s.sprinting = false;
        // With a spring under the swimmer the plunge keeps its speed (up to the water's own limit);
        // without one the fall stops at the swim speed, as before.
        let fastest = if p.buoyancy_omega > 0.0 { water_vertical_limit(p) } else { p.swim_speed };
        s.velocity.y = s.velocity.y.max(-fastest);
        // The brake on the way in: the horizontal speed is scaled down to the cap, once.
        if p.entry_max_speed > 0.0 {
            let across = flat(s.velocity);
            let speed = across.length();
            if speed > p.entry_max_speed {
                let scaled = across * (p.entry_max_speed / speed);
                s.velocity.x = scaled.x;
                s.velocity.z = scaled.z;
            }
        }
        self.set_mode(MotionMode::Water);
    }

    // --- ladder ---------------------------------------------------------------------
    //
    // A ladder is a volume with a face ([`VolumeKind::Ladder`](crate::VolumeKind::Ladder)). Nothing is kept
    // about it between ticks but the mode: the ladder is the one the capsule touches now.
    //
    // - **Taking hold.** A body whose params climb pushes into the ladder (within 60 degrees) from its front
    //   and something solid stops it: on the ground, in the air (a fall is caught; a body flying away from
    //   the ladder is not) or swimming. A body that walks off an edge inside the volume (the ladder's top,
    //   forward or backward) takes hold too, whatever it pushes (`stick`).
    // - **Climbing.** No gravity. The part of the input into the ladder climbs, the part away from it goes
    //   down, the rest slides sideways; no input hangs. `ladder_by_look` tilts the forward input by the
    //   look's pitch first, as *Half-Life* does.
    // - **The top.** Where the way in is open and there is floor one radius in, the body walks in level,
    //   still held, until its axis is over the floor, and stands. A ladder that leads nowhere stops the
    //   climb where the hold would end.
    // - **The foot.** A body that is not climbing up stands on ground it reaches.
    // - **Letting go.** A jump pushes the body off, out of the face, and the body takes hold of no ladder
    //   while it goes on pushing into this one ([`LadderLetGo`](super::LadderLetGo), the one thing kept
    //   beside the mode): air control must not bring it straight back. Leaving the volume (sideways, or down
    //   past its foot) lets go too, and the fall starts there.

    /// The movement input as a ladder with the face `out` reads it.
    fn ladder_wish(&self, out: Vec3) -> LadderWish {
        let wish = self.wish();
        // Half-Life's rule: the forward part of the input goes along the look's pitch.
        let (level, lift) = match self.look.filter(|_| self.params.ladder_by_look) {
            Some(look) => {
                let forward = look.flat_dir();
                let ahead = wish.dot(forward);
                let (sin, cos) = ops::sin_cos(look.pitch);
                (wish + forward * (ahead * (cos - 1.0)), ahead * sin)
            }
            None => (wish, 0.0),
        };
        let away = level.dot(out);
        LadderWish { climb: lift - away, side: level - out * away, into: -wish.dot(out) }
    }

    /// The face of the ladder the body at `feet` pushes into: it touches one from its front and its input
    /// points into it within 60 degrees.
    fn pushes_into_ladder(&self, feet: Vec3) -> Option<Vec3> {
        let Ladder { out, .. } = self.mover.ladder(feet).filter(|l| l.front)?;
        (self.wish().normalize_or_zero().dot(-out) >= PUSH_COS).then_some(out)
    }

    /// Whether the body takes hold of a ladder at `feet` this tick (see "Taking hold" above).
    fn takes_hold(&self, feet: Vec3) -> bool {
        if !self.params.can_climb_ladders() || self.input.locked || self.let_go {
            return false;
        }
        let Some(out) = self.pushes_into_ladder(feet) else {
            return false;
        };
        match self.state.mode {
            // From the ground the push must climb: a look that sends it down has nowhere to go.
            MotionMode::Ground if self.ladder_wish(out).climb <= 0.0 => return false,
            MotionMode::Air | MotionMode::Glide if flat(self.state.velocity).dot(out) > LADDER_LEAVING => return false,
            _ => {}
        }
        self.mover.free(feet, -out, LADDER_BLOCKED) < LADDER_BLOCKED - CAST_SLACK
    }

    /// The body is on a ladder from now on: at rest, with no fall behind it and no jump waiting.
    fn hold_ladder(&mut self) {
        let s = &mut *self.state;
        s.velocity = Vec3::ZERO;
        s.grounded = false;
        s.ground_normal = Vec3::Y;
        s.fall_from_y = None;
        s.coyote_left = 0.0;
        s.jump_buffer_left = 0.0;
        s.sprinting = false;
        self.set_mode(MotionMode::Ladder);
    }

    /// Takes hold and climbs the rest of the tick.
    fn hold(&mut self, feet: Vec3) -> Vec3 {
        self.hold_ladder();
        self.ladder(feet)
    }

    fn ladder(&mut self, feet: Vec3) -> Vec3 {
        let p = self.params;
        let Some(Ladder { out, .. }) = self.mover.ladder(feet).filter(|_| p.can_climb_ladders()) else {
            // Off the ladder's side or past its foot, or the ladder is gone: the fall starts here.
            self.state.fall_from_y = Some(feet.y);
            self.set_mode(MotionMode::Air);
            return self.air(feet);
        };
        if self.state.jump_buffer_left > 0.0 && !self.input.locked {
            let s = &mut *self.state;
            s.velocity = out * p.ladder_jump_speed;
            s.jump_buffer_left = 0.0;
            s.fall_from_y = Some(feet.y);
            self.notes.push(Note::Jumped);
            self.notes.push(Note::LadderLetGo(true));
            self.let_go = true;
            self.set_mode(MotionMode::Air);
            return self.air(feet);
        }
        let wish = self.ladder_wish(out);
        let speed = p.ladder_speed * self.modifiers.factor;
        // The top: the way in is open and there is floor there. In, level, until the floor is under the axis.
        if wish.into > 0.0 && self.floor_ahead(feet, out) {
            let mut v = (wish.side - out * wish.into) * speed;
            let moved = self.mover.slide(feet, v * self.dt + self.input.external, false);
            for touch in &moved.touches {
                v = clip(v, touch.normal);
            }
            self.state.velocity = v;
            if self.mover.floor_under(moved.feet, p.snap_distance)
                && let Some(feet) = self.stand(moved.feet, p.snap_distance)
            {
                return feet;
            }
            return moved.feet;
        }
        let mut v = Vec3::Y * (wish.climb * speed) + wish.side * speed;
        let delta = v * self.dt + self.input.external;
        let mut moved = self.mover.slide(feet, delta, false);
        // A ladder that ends in the air: no higher than a hold on it reaches.
        if v.y > 0.0 && self.mover.ladder(moved.feet).is_none() {
            v.y = 0.0;
            moved = self.mover.slide(feet, flat(delta), false);
        }
        for touch in &moved.touches {
            v = clip(v, touch.normal);
        }
        self.state.velocity = v;
        // The foot: a body that is not climbing up stands on the ground it reaches.
        if wish.climb <= 0.0
            && let Some(feet) = self.stand(moved.feet, LAND_PROBE)
        {
            return feet;
        }
        moved.feet
    }

    /// Whether a body on a ladder with the face `out` can walk off it into the ladder's side: nothing
    /// stops the capsule for one radius that way, and it would have walkable ground under it there. True
    /// at a ladder's top once the feet are over the floor it leads to; never against the wall it hangs on.
    fn floor_ahead(&self, feet: Vec3, out: Vec3) -> bool {
        let reach = self.mover.radius;
        self.mover.free(feet, -out, reach) >= reach - CAST_SLACK
            && self.mover.ground(feet - out * reach, self.params.snap_distance).is_some_and(|g| g.walkable)
    }

    /// Stands the body on walkable ground within `reach` under the feet, off a ladder: no landing is
    /// reported, a climb is no fall. `None` when there is none.
    fn stand(&mut self, feet: Vec3, reach: f32) -> Option<Vec3> {
        let ground = self.mover.ground(feet, reach).filter(|g| g.walkable)?;
        let s = &mut *self.state;
        s.grounded = true;
        s.ground_normal = ground.normal;
        s.coyote_left = self.params.coyote_time;
        s.velocity.y = 0.0;
        s.fall_from_y = None;
        self.set_mode(MotionMode::Ground);
        Some(feet - Vec3::Y * ground.drop)
    }

    // --- mantle ---------------------------------------------------------------------

    fn mantle(&mut self, feet: Vec3) -> Vec3 {
        let Some(mut climb) = self.state.mantle else {
            self.set_mode(MotionMode::Ground);
            return self.stick(feet);
        };
        climb.t = (climb.t + self.dt / climb.duration.max(1e-3)).min(1.0);
        let pos = climb.position();
        if climb.t >= 1.0 {
            self.state.mantle = None;
            self.state.velocity = Vec3::ZERO;
            self.set_mode(MotionMode::Ground);
            return self.stick(pos);
        }
        self.state.velocity = (pos - feet) / self.dt;
        self.state.mantle = Some(climb);
        pos
    }

    /// Climbs onto the obstacle behind `wall` if its top is more than `min_height` and at
    /// most `reach` above the feet (the params' `mantle_height`, or a surface-relative limit
    /// turned into a height over the feet) and the capsule fits up there. `push`: only
    /// when the input points at the wall; `top_limit`: and only if the ledge top (world height)
    /// is no higher than that (plus `MANTLE_SLACK`, like the reach check, so a ledge exactly at a
    /// content limit climbs whatever the last bit of an `f32` says).
    fn try_mantle(
        &mut self,
        feet: Vec3,
        wall: &Touch,
        min_height: f32,
        push: bool,
        top_limit: Option<f32>,
        reach: f32,
    ) -> Option<Vec3> {
        if self.input.locked {
            return None;
        }
        let p = self.params;
        let dir = (-flat(wall.normal)).try_normalize()?;
        // A swimmer must face the ledge it climbs onto: the body turns at the swim rate, so a push
        // from the side or a jump with the back turned does not climb.
        if self.state.mode == MotionMode::Water
            && let Some(min) = p.climb_out_min_facing
            && self.facing.dot(dir) < min
        {
            return None;
        }
        if push && self.wish().normalize_or_zero().dot(dir) < PUSH_COS {
            return None;
        }
        let m = self.mover;
        let top = m.obstacle_top(feet, wall.point, dir, reach)?;
        let rest = top + m.skin;
        let height = rest - feet.y;
        if height <= min_height || height > reach + MANTLE_SLACK || top_limit.is_some_and(|limit| top > limit + MANTLE_SLACK) {
            return None;
        }
        // Prefer standing fully on top; a narrow top takes a shorter step.
        let rise = (height + 0.1).min(reach + 0.1);
        // A swimmer that reaches farther than a hand's breadth (`climb_out_reach`) first closes
        // the gap to the wall, then steps over as usual.
        let gap = if self.state.mode == MotionMode::Water && p.climb_out_reach.is_some() {
            (flat(wall.point - feet).dot(dir) - m.radius).max(0.0)
        } else {
            0.0
        };
        let (to, corner) = [2.0, 1.0, 0.5].into_iter().find_map(|reach| {
            let to = m.ledge(feet, dir, rise, gap + m.radius * reach)?;
            if (to.y - rest).abs() > 0.05 {
                return None;
            }
            Some((to, m.climb_corner(feet, to)?))
        })?;
        // A climb out of the water has its own length when the params give one.
        let duration = if self.state.mode == MotionMode::Water { p.climb_out_time.unwrap_or(p.mantle_time) } else { p.mantle_time };
        let s = &mut *self.state;
        s.mantle = Some(Mantle { from: feet, corner, to, t: 0.0, duration });
        s.velocity = Vec3::ZERO;
        s.grounded = false;
        s.coyote_left = 0.0;
        s.fall_from_y = None;
        self.notes.push(Note::Mantled { height: to.y - feet.y });
        self.set_mode(MotionMode::Mantle);
        Some(feet)
    }

    /// Speed-up rate of the ground and air: the modifiers' override, else the params'.
    fn acceleration(&self) -> f32 {
        self.modifiers.acceleration.unwrap_or(self.params.acceleration)
    }

    /// Slow-down rate on the ground: the modifiers' override, else the params'.
    fn deceleration(&self) -> f32 {
        self.modifiers.deceleration.unwrap_or(self.params.deceleration)
    }

    /// A steep surface right in front (within a hand's reach), as a slide would touch it.
    fn wall_ahead(&self, feet: Vec3, dir: Vec3) -> Option<Touch> {
        let hit = self.mover.cast(feet, dir, self.mover.skin + self.params.climb_reach())?;
        (!self.mover.walkable(hit.normal)).then_some(Touch {
            plane: hit.normal,
            normal: hit.normal,
            point: hit.point,
            walkable: false,
        })
    }
}

impl Mantle {
    /// Where the climb is at `t`: straight up to `corner`, then over to `to`, at an even
    /// speed along the path.
    pub fn position(&self) -> Vec3 {
        let up = (self.corner.y - self.from.y).max(0.0);
        let over = (self.to - self.corner).length();
        let total = up + over;
        if total <= f32::EPSILON {
            return self.to;
        }
        let along = self.t.clamp(0.0, 1.0) * total;
        if along <= up {
            self.from + Vec3::Y * along
        } else {
            self.corner + (self.to - self.corner) * ((along - up) / over.max(f32::EPSILON))
        }
    }
}

/// The fastest vertical speed in the water while a spring holds the swimmer.
fn water_vertical_limit(p: &CharacterParams) -> f32 {
    if p.water_max_vertical_speed > 0.0 { p.water_max_vertical_speed } else { p.swim_speed }
}

/// One tick of the buoyancy spring in closed form: a critically damped oscillator toward the float
/// line, `x'' = -w^2 x - 2 w x'`, whose exact solution over `dt` is
/// `x(dt) = (x0 + (v0 + w x0) dt) e^(-w dt)`. `offset`: the feet's height over the float line,
/// `v0` the vertical speed. Returns the new vertical speed and the vertical travel of the tick.
/// No tick rate changes where the swimmer ends up, and nothing is integrated step by step. A fall
/// in the water is first slowed by `water_fall_drag` (`exp(-drag dt)`), and the speed and the
/// travel stay within the water's vertical limit.
fn buoyancy(p: &CharacterParams, offset: f32, v0: f32, dt: f32) -> (f32, f32) {
    let w = p.buoyancy_omega;
    let limit = water_vertical_limit(p);
    let v0 = if v0 < 0.0 && p.water_fall_drag > 0.0 { v0 * ops::exp(-p.water_fall_drag * dt) } else { v0 };
    let v0 = v0.clamp(-limit, limit);
    let decay = ops::exp(-w * dt);
    let push = v0 + w * offset;
    let next = (offset + push * dt) * decay;
    let speed = ((v0 - w * push * dt) * decay).clamp(-limit, limit);
    (speed, (next - offset).clamp(-limit * dt, limit * dt))
}

/// How fast a swimmer changes its speed: toward an input (`has_input`) or to rest.
fn swim_rate(p: &CharacterParams, has_input: bool) -> f32 {
    let speed_up = if p.swim_acceleration > 0.0 { p.swim_acceleration } else { p.acceleration };
    if has_input || p.swim_deceleration <= 0.0 { speed_up } else { p.swim_deceleration }
}

/// `approach`, or (`per_axis`) each horizontal axis toward its own target by at most `max_step`
/// (`y` stays as it is).
fn step_toward(v: Vec3, target: Vec3, max_step: f32, per_axis: bool) -> Vec3 {
    if per_axis {
        Vec3::new(move_toward(v.x, target.x, max_step), v.y, move_toward(v.z, target.z, max_step))
    } else {
        approach(v, target, max_step)
    }
}

fn move_toward(from: f32, to: f32, max_step: f32) -> f32 {
    if (to - from).abs() <= max_step { to } else { from + (to - from).signum() * max_step }
}

/// Moves `v` toward `target` by at most `max_step`.
fn approach(v: Vec3, target: Vec3, max_step: f32) -> Vec3 {
    let diff = target - v;
    let len = diff.length();
    if len <= max_step || len <= f32::EPSILON { target } else { v + diff * (max_step / len) }
}

/// A horizontal displacement along the ground plane: same horizontal distance, up or
/// down the slope.
fn along_ground(d: Vec3, normal: Vec3) -> Vec3 {
    if normal.y < 1e-3 {
        return d;
    }
    Vec3::new(d.x, -(normal.x * d.x + normal.z * d.z) / normal.y, d.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approach_stops_at_target() {
        assert_eq!(approach(Vec3::ZERO, Vec3::X * 5.0, 1.0), Vec3::X);
        assert_eq!(approach(Vec3::X * 4.5, Vec3::X * 5.0, 1.0), Vec3::X * 5.0);
    }

    #[test]
    fn slope_projection_keeps_horizontal_distance() {
        let normal = Vec3::new(-1.0, 1.0, 0.0).normalize(); // 45° rising toward +x
        let d = along_ground(Vec3::X, normal);
        assert!((d.y - 1.0).abs() < 1e-5 && d.x == 1.0, "{d}");
    }

    #[test]
    fn mantle_path_goes_up_then_over() {
        let m = Mantle {
            from: Vec3::ZERO,
            corner: Vec3::Y * 1.0,
            to: Vec3::new(1.0, 1.0, 0.0),
            t: 0.25,
            duration: 1.0,
        };
        assert!((m.position() - Vec3::Y * 0.5).length() < 1e-6);
        assert!((Mantle { t: 0.75, ..m }.position() - Vec3::new(0.5, 1.0, 0.0)).length() < 1e-6);
        assert_eq!(Mantle { t: 1.0, ..m }.position(), m.to);
    }

    #[test]
    fn the_buoyancy_spring_is_the_same_whatever_the_tick_rate() {
        let p = CharacterParams { buoyancy_omega: 4.0, water_max_vertical_speed: 50.0, ..default() };
        // One second at 60, 30 and 240 ticks per second from a plunge: the same place and speed.
        let run = |ticks: u32| {
            let dt = 1.0 / ticks as f32;
            let (mut x, mut v) = (-1.5f32, -9.0f32);
            for _ in 0..ticks {
                let (nv, dy) = buoyancy(&p, x, v, dt);
                x += dy;
                v = nv;
            }
            (x, v)
        };
        let (x60, v60) = run(60);
        for ticks in [30, 240] {
            let (x, v) = run(ticks);
            assert!((x - x60).abs() < 2e-4 && (v - v60).abs() < 2e-3, "{ticks}: {x} {v} vs {x60} {v60}");
        }
        // And it is the exact solution: x(t) = (x0 + (v0 + w x0) t) e^(-w t) at t = 1 s.
        let exact = (-1.5 + (-9.0 + 4.0 * -1.5)) * ops::exp(-4.0);
        assert!((x60 - exact).abs() < 2e-4, "{x60} vs {exact}");
    }

    #[test]
    fn the_buoyancy_limit_clamps_speed_and_travel() {
        let p = CharacterParams { buoyancy_omega: 4.0, water_max_vertical_speed: 7.0, ..default() };
        let (v, dy) = buoyancy(&p, -3.0, -30.0, 1.0 / 60.0);
        assert!(v >= -7.0 - 1e-5 && dy >= -7.0 / 60.0 - 1e-6, "{v} {dy}");
        // A fall is slowed by the drag before the spring acts.
        let drag = CharacterParams { water_fall_drag: 3.0, ..p.clone() };
        let (slowed, _) = buoyancy(&drag, 0.0, -5.0, 0.1);
        let (plain, _) = buoyancy(&p, 0.0, -5.0, 0.1);
        assert!(slowed.abs() < plain.abs(), "{slowed} {plain}");
    }
}
