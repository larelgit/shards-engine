//! What the AI sees of motion: the `MotionState` fragment, journal lines for the events
//! worth knowing, and line of sight through physics.

use ai_core::{label, AiAppExt, AiId, AiLineOfSight};
use bevy::prelude::*;

use crate::controller::{Crouch, Crouched, Jumped, Landed, Mantled, ModeChanged, MotionMode, MotionState, Stood};
use crate::physics::{LayerMask, PhysicsWorld, QueryFilter, VolumeEntered, VolumeExited, VolumeKind};

/// What the motion layer tells the AI, and how far its perception queries look.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct MotionAi {
    /// A landing is journaled from this fall height (m): small hops are noise.
    pub landed_min_fall: f32,
    /// `clearance` looks this far in each direction (m).
    pub clearance_range: f32,
    /// A drop deeper than this (m) is a cliff for `clearance` and `path_to`; shallower ones
    /// are stepped down.
    pub safe_drop: f32,
}

impl Default for MotionAi {
    fn default() -> Self {
        Self { landed_min_fall: 2.0, clearance_range: 15.0, safe_drop: 2.0 }
    }
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<MotionAi>()
        // Rays against the physics world; `AiOccluder` boxes still block (games without
        // physics keep working, and so do boxes laid over a physics world).
        .insert_resource(AiLineOfSight(physics_line_of_sight))
        // The key stays the component's name; the fragment reads the stance (a sibling component) too.
        .register_ai_describer("MotionState", describe_body_motion)
        .journal_event::<Landed>(|e, w| {
            let min = w.get_resource::<MotionAi>().map_or(2.0, |m| m.landed_min_fall);
            // A tiered landing is told even from a small fall: the tiers start at a few m/s.
            (e.fall_height >= min || e.tier > 0).then(|| tracked(w, e.entity)).flatten().map(|who| {
                let how = if e.jumped { "jump" } else { "drop" };
                match e.tier {
                    0 => format!("{who} landed from {:.1}m", e.fall_height),
                    tier => format!("{who} landed from {:.1}m at {:.1}m/s, tier {tier} ({how})", e.fall_height, e.impact_speed),
                }
            })
        })
        .journal_event::<Jumped>(|e, w| tracked(w, e.entity).map(|who| format!("{who} jumped")))
        .journal_event::<Crouched>(|e, w| tracked(w, e.entity).map(|who| format!("{who} crouches")))
        .journal_event::<Stood>(|e, w| tracked(w, e.entity).map(|who| format!("{who} stands up")))
        .journal_event::<Mantled>(|e, w| {
            tracked(w, e.entity).map(|who| format!("{who} climbs a {:.1}m ledge", e.height))
        })
        .journal_event::<ModeChanged>(|e, w| {
            let notable = |m: MotionMode| matches!(m, MotionMode::Water | MotionMode::Glide | MotionMode::Ladder);
            (notable(e.from) || notable(e.to))
                .then(|| tracked(w, e.entity))
                .flatten()
                .map(|who| format!("{who} {} → {}", mode_name(e.from), mode_name(e.to)))
        })
        .journal_event::<VolumeEntered>(|e, w| {
            trigger_name(w, e.volume).and_then(|name| Some(format!("{} entered {name}", tracked(w, e.entity)?)))
        })
        .journal_event::<VolumeExited>(|e, w| {
            trigger_name(w, e.volume).and_then(|name| Some(format!("{} left {name}", tracked(w, e.entity)?)))
        });
}

/// `e5` for entities the AI tracks; journal lines about the rest are noise.
fn tracked(world: &World, entity: Entity) -> Option<String> {
    world.get::<AiId>(entity).is_some().then(|| label(world, entity))
}

/// `trigger "goal"` (with its id when the AI sees the volume): only named triggers are
/// journaled. Water and updrafts show as modes and speeds instead.
#[expect(clippy::disallowed_methods, reason = "a volume is a collider (a voxel field never is): its kind, not its box")]
fn trigger_name(world: &World, volume: Entity) -> Option<String> {
    let kind = world.get_resource::<PhysicsWorld>()?.collider(volume)?.volume.clone()?;
    matches!(kind, VolumeKind::Trigger { .. }).then(|| match world.get::<AiId>(volume) {
        Some(id) => format!("{kind} {id}"),
        None => kind.to_string(),
    })
}

fn mode_name(mode: MotionMode) -> &'static str {
    match mode {
        MotionMode::Ground => "ground",
        MotionMode::Air => "air",
        MotionMode::Water => "water",
        MotionMode::Glide => "glide",
        MotionMode::Mantle => "climbing",
        MotionMode::Ladder => "ladder",
    }
}

/// `ground 5.8m/s`, `air ↑3.1m/s`, `water 2.0m/s`, `glide ↓2.0m/s`, `mantle 40%`, `ladder ↑3.5m/s`
/// (`ladder` while it hangs); nothing while standing still. In the air only the vertical speed shows: where the character is
/// going sideways follows from its position.
pub fn describe_motion(s: &MotionState) -> String {
    let speed = crate::controller::mover::flat(s.velocity).length();
    let vertical = if s.velocity.y >= 0.0 { format!("↑{:.1}m/s", s.velocity.y) } else { format!("↓{:.1}m/s", -s.velocity.y) };
    match s.mode {
        MotionMode::Ground if speed < 0.05 => String::new(),
        MotionMode::Ground if s.sprinting => format!("ground {speed:.1}m/s sprint"),
        MotionMode::Ground => format!("ground {speed:.1}m/s"),
        MotionMode::Air => format!("air {vertical}"),
        MotionMode::Glide => format!("glide {vertical}"),
        MotionMode::Water if speed < 0.05 => "water".to_owned(),
        MotionMode::Water => format!("water {speed:.1}m/s"),
        MotionMode::Mantle => format!("mantle {:.0}%", s.mantle.map_or(0.0, |m| m.t * 100.0)),
        MotionMode::Ladder if s.velocity.length() < 0.05 => "ladder".to_owned(),
        MotionMode::Ladder if s.velocity.y.abs() < 0.05 => format!("ladder {speed:.1}m/s"),
        MotionMode::Ladder => format!("ladder {vertical}"),
    }
}

/// The fragment of a body's line about how it moves: [`describe_motion`], and after it the stance while
/// the body is below its standing height (`ground 2.1m/s crouched`, `air ↑3.1m/s crouched`, or just
/// `crouched` standing still).
fn describe_body_motion(e: EntityRef) -> Option<String> {
    let motion = describe_motion(e.get::<MotionState>()?);
    match e.get::<Crouch>().and_then(describe_stance) {
        None => Some(motion),
        Some(stance) if motion.is_empty() => Some(stance.to_owned()),
        Some(stance) => Some(format!("{motion} {stance}")),
    }
}

/// `crouched`, `crouching`, `standing up`, or `crouched, no room to stand` while a ceiling (or, in the
/// air, the floor) holds a body that wants to stand; `None` for a body at its full height.
pub fn describe_stance(c: &Crouch) -> Option<&'static str> {
    match (c.depth > 0.0, c.wanted, c.blocked) {
        (false, ..) => None,
        (true, _, true) => Some("crouched, no room to stand"),
        (true, true, false) if c.depth >= 1.0 => Some("crouched"),
        (true, true, false) => Some("crouching"),
        (true, false, false) => Some("standing up"),
    }
}

/// Line of sight for `observe`: nothing solid on the world layer between the points, and
/// no [`ai_core::AiOccluder`] either. `ignore` (the viewer and the target) never blocks.
pub fn physics_line_of_sight(world: &World, from: Vec3, to: Vec3, ignore: &[Entity]) -> bool {
    if let Some(physics) = world.get_resource::<PhysicsWorld>() {
        let delta = to - from;
        let distance = delta.length();
        if let Ok(dir) = Dir3::new(delta) {
            let filter = ignore.iter().fold(QueryFilter::new(LayerMask::WORLD), |f, e| f.excluding(*e));
            // A hit at the very end is the target's own surface, not something in the way.
            if physics.raycast(from, dir, distance, &filter).is_some_and(|hit| hit.distance < distance - 0.01) {
                return false;
            }
        }
    }
    (AiLineOfSight::default().0)(world, from, to, ignore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_is_short_and_silent_when_idle() {
        assert_eq!(describe_motion(&MotionState::default()), "");
        let s = |mode, velocity| MotionState { mode, velocity, ..default() };
        assert_eq!(describe_motion(&s(MotionMode::Ground, Vec3::new(5.8, 0.0, 0.0))), "ground 5.8m/s");
        assert_eq!(describe_motion(&s(MotionMode::Air, Vec3::new(4.0, 3.1, 0.0))), "air ↑3.1m/s");
        assert_eq!(describe_motion(&s(MotionMode::Glide, Vec3::new(9.0, -2.0, 0.0))), "glide ↓2.0m/s");
        assert_eq!(describe_motion(&s(MotionMode::Water, Vec3::ZERO)), "water");
        assert_eq!(describe_motion(&s(MotionMode::Water, Vec3::X * 2.0)), "water 2.0m/s");
        let climbing = MotionState {
            mode: MotionMode::Mantle,
            mantle: Some(crate::controller::Mantle { from: Vec3::ZERO, corner: Vec3::Y, to: Vec3::ONE, t: 0.4, duration: 1.0 }),
            ..default()
        };
        assert_eq!(describe_motion(&climbing), "mantle 40%");
        assert_eq!(describe_motion(&s(MotionMode::Ladder, Vec3::ZERO)), "ladder", "hanging on one");
        assert_eq!(describe_motion(&s(MotionMode::Ladder, Vec3::Y * 3.5)), "ladder ↑3.5m/s");
        assert_eq!(describe_motion(&s(MotionMode::Ladder, Vec3::Y * -3.5)), "ladder ↓3.5m/s");
        assert_eq!(describe_motion(&s(MotionMode::Ladder, Vec3::Z * 3.5)), "ladder 3.5m/s", "sideways along it");
        assert_eq!(mode_name(MotionMode::Ladder), "ladder");
    }

    #[test]
    fn the_stance_follows_the_motion_in_the_bodys_line() {
        let line = |motion: MotionState, crouch: Option<Crouch>| {
            let mut world = World::new();
            let id = {
                let mut e = world.spawn(motion);
                if let Some(c) = crouch {
                    e.insert(c);
                }
                e.id()
            };
            describe_body_motion(world.entity(id))
        };
        let walking = MotionState { mode: MotionMode::Ground, velocity: Vec3::X * 2.1, ..default() };
        let down = Crouch { depth: 1.0, wanted: true, blocked: false };
        assert_eq!(line(walking.clone(), None).as_deref(), Some("ground 2.1m/s"));
        assert_eq!(line(walking.clone(), Some(Crouch::default())).as_deref(), Some("ground 2.1m/s"), "a body that stood up says nothing");
        assert_eq!(line(walking, Some(down)).as_deref(), Some("ground 2.1m/s crouched"));
        assert_eq!(line(MotionState::default(), Some(down)).as_deref(), Some("crouched"), "standing still, only the stance");
        assert_eq!(describe_stance(&Crouch { depth: 0.4, wanted: true, blocked: false }), Some("crouching"));
        assert_eq!(describe_stance(&Crouch { depth: 0.4, wanted: false, blocked: false }), Some("standing up"));
        assert_eq!(describe_stance(&Crouch { depth: 1.0, wanted: false, blocked: true }), Some("crouched, no room to stand"));
    }
}
