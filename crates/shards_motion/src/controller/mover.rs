//! Collision primitives for one character capsule: cast, move-and-slide, ground probe,
//! step-up, ledge probe, depenetration. No game rules here, only geometry.

use bevy::prelude::*;
use parry3d::math::Pose;

use super::params::CharacterParams;
use crate::physics::world::{Geom, StartOverlap};
use crate::physics::{CollisionLayers, LayerMask, Penetration, PhysicsWorld, QueryFilter, Shape, ShapeHit, VolumeKind};

/// Casts per move: a corner of two walls and a floor takes three.
const MAX_SLIDES: usize = 4;
/// Below this a displacement is no displacement (m).
const TINY: f32 = 1e-5;
/// How far past an edge contact the surface probe looks (m).
const EDGE_PROBE: f32 = 0.02;
/// The floor under the feet is looked for from this far above them (m): a ray that starts on a surface
/// misses it.
const FLOOR_RAY_LIFT: f32 = 0.05;
/// A ledge probe needs at least this much room on top (m).
const MIN_LEDGE_ROOM: f32 = 0.1;
/// The nearest the capsule may sit to anything, in skins, before it is pushed back out
/// to a full skin: room for rounding, and for resting on an edge.
const MIN_GAP: f32 = 0.5;

/// Ground under the capsule.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ground {
    /// How far the feet are above their resting height (m).
    pub drop: f32,
    /// The surface stood on (see [`Mover::surface_normal`]).
    pub normal: Vec3,
    pub walkable: bool,
    /// What the probe met: the collider or voxel field stood on (a mover carries what stands on it).
    pub entity: Entity,
}

/// Something a slide ran into.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Touch {
    /// The plane the slide followed (a steep surface flattened into a wall on the ground).
    pub plane: Vec3,
    /// The surface normal as hit.
    pub normal: Vec3,
    /// Contact point on the surface.
    pub point: Vec3,
    pub walkable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Slide {
    pub feet: Vec3,
    pub touches: Vec<Touch>,
}

impl Slide {
    /// A surface too steep to walk on (not a ceiling) that stopped the slide.
    pub fn wall(&self) -> Option<Touch> {
        self.touches.iter().find(|t| !t.walkable && t.normal.y > -0.5).copied()
    }
}

/// One character's capsule against the static world, as of the last physics sync.
pub(crate) struct Mover<'a> {
    physics: &'a PhysicsWorld,
    filter: QueryFilter,
    geom: Geom,
    /// The capsule grown by the minimum gap: the ground probe (see [`Mover::ground`]).
    probe: Geom,
    /// Capsule center above the feet.
    center: f32,
    pub radius: f32,
    pub skin: f32,
    /// `cos(max_slope)`: the lowest normal `y` that is still ground.
    min_ground_y: f32,
}

impl<'a> Mover<'a> {
    /// Characters only collide with what their layers filter for, never with other
    /// characters (the separation pass handles those) and never with themselves.
    pub fn new(physics: &'a PhysicsWorld, params: &CharacterParams, me: Entity, layers: CollisionLayers) -> Self {
        let radius = params.radius.max(0.01);
        let height = params.height.max(2.0 * radius);
        let mask = layers.filters.without(LayerMask::CHARACTER);
        let skin = params.skin();
        let grown = skin * MIN_GAP;
        Self {
            physics,
            filter: QueryFilter::new(mask).excluding(me),
            geom: Geom::new(&Shape::capsule(radius, height)),
            probe: Geom::new(&Shape::capsule(radius + grown, height + 2.0 * grown)),
            center: height * 0.5,
            radius,
            skin,
            min_ground_y: ops::cos(params.max_slope_deg.clamp(0.0, 89.9).to_radians()),
        }
    }

    /// The nearest the capsule may sit to anything before
    /// [`depenetrate`](Self::depenetrate) pushes it back out.
    fn min_gap(&self) -> f32 {
        self.skin * MIN_GAP
    }

    fn pose(&self, feet: Vec3) -> Pose {
        Pose::from_translation(feet + Vec3::Y * self.center)
    }

    pub fn walkable(&self, normal: Vec3) -> bool {
        normal.y >= self.min_ground_y
    }

    /// First hit moving `dist` along unit `dir`. A capsule already inside something
    /// can still move out of it.
    pub fn cast(&self, feet: Vec3, dir: Vec3, dist: f32) -> Option<ShapeHit> {
        self.physics.sweep(&self.geom, &self.pose(feet), dir, dist, &self.filter, StartOverlap::LetOut)
    }

    /// How far along `dir` the capsule may go toward `hit`: it stops a skin away from
    /// the surface measured along the surface normal, so a slanted approach (into a
    /// corner, along a wall) does not end closer than a straight one.
    fn stop(&self, hit: &ShapeHit, dir: Vec3) -> f32 {
        stop_short(hit, dir, self.skin)
    }

    /// How far a move of `dist` looks ahead: a surface met at an angle is a skin away
    /// along its normal but farther along the move (√2 skins at 45°).
    fn reach(&self, dist: f32) -> f32 {
        dist + 2.0 * self.skin
    }

    /// How far the capsule can move along `dir` (up to `dist`), keeping the skin gap.
    pub fn free(&self, feet: Vec3, dir: Vec3, dist: f32) -> f32 {
        self.cast(feet, dir, self.reach(dist)).map_or(dist, |h| self.stop(&h, dir).min(dist))
    }

    /// Move-and-slide: goes as far as it can along `delta`, then along whatever it hit.
    /// `flatten` turns steep surfaces into vertical walls, so walking into a steep slope
    /// never climbs it.
    pub fn slide(&self, feet: Vec3, delta: Vec3, flatten: bool) -> Slide {
        let mut pos = feet;
        let mut rest = delta;
        let mut touches: Vec<Touch> = Vec::new();
        for _ in 0..MAX_SLIDES {
            let len = rest.length();
            if len < TINY {
                break;
            }
            let dir = rest / len;
            let hit = self.cast(pos, dir, self.reach(len));
            let travel = hit.as_ref().map_or(len, |h| self.stop(h, dir));
            // A surface ahead that this move does not reach is no touch.
            let Some(hit) = hit.filter(|_| travel < len) else {
                pos += rest;
                break;
            };
            pos += dir * travel;
            rest = dir * (len - travel);
            let walkable = self.walkable(hit.normal);
            let plane = if flatten && !walkable { flat(hit.normal).try_normalize().unwrap_or(hit.normal) } else { hit.normal };
            touches.push(Touch { plane, normal: hit.normal, point: hit.point, walkable });
            rest = clip(rest, plane);
            // Pushed back into an earlier plane: follow the crease, or stop in a corner.
            let earlier = touches.len() - 1;
            if let Some(prev) = touches[..earlier].iter().map(|t| t.plane).find(|p| rest.dot(*p) < -TINY) {
                rest = match prev.cross(plane).try_normalize() {
                    Some(crease) => crease * rest.dot(crease),
                    None => Vec3::ZERO,
                };
                if touches.iter().any(|t| rest.dot(t.plane) < -TINY) {
                    rest = Vec3::ZERO;
                }
            }
        }
        Slide { feet: pos, touches }
    }

    /// Ground within `max` below the feet. The probe is the capsule grown by the
    /// minimum gap: going down past an edge beside the feet (a stair nosing) it settles
    /// on that edge instead of slipping by nearer than [`depenetrate`](Self::depenetrate)
    /// allows, which would push it back off sideways and undo every step onto it. On a
    /// plain floor or slope it rests where a bare capsule would, a skin above.
    pub fn ground(&self, feet: Vec3, max: f32) -> Option<Ground> {
        let grown = self.min_gap();
        let reach = max + self.skin - grown;
        let hit = self.physics.sweep(&self.probe, &self.pose(feet), Vec3::NEG_Y, reach, &self.filter, StartOverlap::LetOut)?;
        let normal = self.surface_normal(feet, &hit);
        let drop = stop_short(&hit, Vec3::NEG_Y, self.skin - grown);
        Some(Ground { drop, normal, walkable: self.walkable(normal), entity: hit.entity })
    }

    /// The same capsule, its casts passing through `others` as well: what a mover carries or
    /// pushes slides against the world, not against the mover.
    pub fn excluding(&self, others: &[Entity]) -> Self {
        let mut filter = self.filter.clone();
        filter.exclude.extend_from_slice(others);
        Self {
            physics: self.physics,
            filter,
            geom: self.geom,
            probe: self.probe,
            center: self.center,
            radius: self.radius,
            skin: self.skin,
            min_ground_y: self.min_ground_y,
        }
    }

    /// The deepest of `of` that the capsule at `feet` sits nearer to than the minimum gap (the gap
    /// [`depenetrate`](Self::depenetrate) restores): `depth` is how far short of a full skin it
    /// is, `normal` the way out. `None` when it is clear of all of them.
    pub fn inside(&self, feet: Vec3, of: &[Entity]) -> Option<Penetration> {
        let near = self.physics.closer_than(&self.geom, &self.pose(feet), &self.filter, self.skin);
        // A rounding's worth nearer than the gap is still clear.
        let tolerance = self.skin * 1e-3;
        near.into_iter().filter(|p| of.contains(&p.entity) && self.skin - p.depth < self.min_gap() - tolerance).fold(
            None,
            |best: Option<Penetration>, p| match best {
                Some(b) if b.depth >= p.depth => Some(b),
                _ => Some(p),
            },
        )
    }

    /// The surface the capsule rests on. Resting on an edge (a stair nosing, a ledge)
    /// gives the round bottom a tilted contact normal although the top it stands on is
    /// flat; a short ray just past the contact finds that top. Steep slopes stay steep.
    fn surface_normal(&self, feet: Vec3, hit: &ShapeHit) -> Vec3 {
        if self.walkable(hit.normal) {
            return hit.normal;
        }
        let center = feet + Vec3::Y * self.center;
        let Some(out) = flat(hit.point - center).try_normalize() else {
            return hit.normal;
        };
        let origin = hit.point + out * EDGE_PROBE + Vec3::Y * 0.05;
        match self.physics.raycast(origin, Dir3::NEG_Y, 0.1, &self.filter) {
            Some(ray) if self.walkable(ray.normal) && (ray.point.y - hit.point.y).abs() < 0.03 => ray.normal,
            _ => hit.normal,
        }
    }

    /// Steps over something up to `height` tall: up, along `delta` (horizontal), down
    /// onto walkable ground no lower than the start. Ground level with the start is
    /// right at the bottom of that search, so it looks a skin deeper than it goes.
    pub fn step_up(&self, feet: Vec3, delta: Vec3, height: f32) -> Option<Slide> {
        let rise = self.free(feet, Vec3::Y, height);
        if rise < TINY * 100.0 {
            return None;
        }
        let moved = self.slide(feet + Vec3::Y * rise, delta, true);
        let ground = self.ground(moved.feet, rise + self.skin)?;
        ground.walkable.then(|| Slide { feet: moved.feet - Vec3::Y * ground.drop.min(rise), touches: moved.touches })
    }

    /// A walkable top at most `max_rise` above the feet, reached by going up, then
    /// `forward` along `dir` (or as far as there is room), then down. The capsule fits
    /// all along that path and on top. Returns the feet position on top.
    pub fn ledge(&self, feet: Vec3, dir: Vec3, max_rise: f32, forward: f32) -> Option<Vec3> {
        let rise = self.free(feet, Vec3::Y, max_rise);
        let raised = feet + Vec3::Y * rise;
        let ahead = self.free(raised, dir, forward);
        if ahead < MIN_LEDGE_ROOM {
            return None;
        }
        let above = raised + dir * ahead;
        let ground = self.ground(above, rise)?;
        ground.walkable.then(|| above - Vec3::Y * ground.drop)
    }

    /// Height of the top of the obstacle touched at `contact`, just past the contact
    /// along `dir`, if it is walkable and at most `max_rise` above the feet (a taller
    /// one, or one with something above it, returns `None`).
    pub fn obstacle_top(&self, feet: Vec3, contact: Vec3, dir: Vec3, max_rise: f32) -> Option<f32> {
        let start = feet.y + max_rise + 0.1;
        let origin = Vec3::new(contact.x, start, contact.z) + dir * EDGE_PROBE;
        let hit = self.physics.raycast(origin, Dir3::NEG_Y, max_rise + 1.0, &self.filter)?;
        (hit.distance > 0.0 && self.walkable(hit.normal)).then_some(hit.point.y)
    }

    /// The corner of a two-segment climb `feet → corner → top` (straight up, then over),
    /// if the capsule fits along it. `top` came from [`ledge`](Self::ledge), which
    /// already cleared the way up.
    pub fn climb_corner(&self, feet: Vec3, top: Vec3) -> Option<Vec3> {
        // One skin above the resting height: the capsule passes over the edge, not along it.
        let corner = Vec3::new(feet.x, top.y + self.skin, feet.z);
        if self.free(feet, Vec3::Y, corner.y - feet.y) < corner.y - feet.y - 1e-3 {
            return None;
        }
        let over = flat(top - corner);
        let len = over.length();
        if len > TINY && self.free(corner, over / len, len) < len - 1e-3 {
            return None;
        }
        Some(corner)
    }

    /// Pushes the capsule out of anything it sinks into or sits nearer to than the
    /// minimum gap (spawned inside a wall, moved by something else, rounding), back to a
    /// full skin away; nearest first.
    pub fn depenetrate(&self, mut feet: Vec3) -> Vec3 {
        for _ in 0..4 {
            let near = self.physics.closer_than(&self.geom, &self.pose(feet), &self.filter, self.skin);
            // The deepest, the first of equals in id order: deterministic.
            let deepest = near.iter().fold(None, |best: Option<&Penetration>, p| match best {
                Some(b) if b.depth >= p.depth => Some(b),
                _ => Some(p),
            });
            match deepest {
                // `depth` is how far short of a full skin it sits.
                Some(p) if self.skin - p.depth < self.min_gap() => feet += p.normal * p.depth,
                _ => break,
            }
        }
        feet
    }

    /// Water surface at the feet (the highest one), updraft and every volume the
    /// capsule touches, in id order.
    pub fn surroundings(&self, feet: Vec3) -> Surroundings {
        let mut out = Surroundings::default();
        for volume in self.physics.volumes_touching(&self.geom, &self.pose(feet)) {
            out.volumes.push(volume.entity);
            if let Some(VolumeKind::Updraft { accel, max_speed }) = volume.volume
                && out.updraft.is_none()
            {
                out.updraft = Some((accel, max_speed));
            }
        }
        out.water = self.water(feet);
        out
    }

    /// The ladder the capsule at `feet` touches: of several, the first in id order the body is in
    /// front of, else the first.
    pub fn ladder(&self, feet: Vec3) -> Option<Ladder> {
        let pose = self.pose(feet);
        let center = feet + Vec3::Y * self.center;
        let mut first = None;
        for face in self.physics.ladders_touching(&self.geom, &pose) {
            let ladder = Ladder { out: face.out, front: (center - face.center).dot(face.out) > 0.0 };
            if ladder.front {
                return Some(ladder);
            }
            first.get_or_insert(ladder);
        }
        first
    }

    /// Whether the capsule's axis stands over walkable ground no more than `max` below the feet: a
    /// ray, so an edge beside the feet that the round bottom would rest on does not count.
    pub fn floor_under(&self, feet: Vec3, max: f32) -> bool {
        let origin = feet + Vec3::Y * FLOOR_RAY_LIFT;
        self.physics.raycast(origin, Dir3::NEG_Y, max + FLOOR_RAY_LIFT, &self.filter).is_some_and(|hit| self.walkable(hit.normal))
    }

    /// Surface of the water the feet are in (the highest one).
    pub fn water(&self, feet: Vec3) -> Option<f32> {
        self.physics.water_surface_at(feet + Vec3::Y * 0.05)
    }

    /// Height of the feet resting position below the water surface: how deep the water
    /// is where the character is. Anything deeper than `max` counts as `max`.
    pub fn depth(&self, feet: Vec3, surface: f32, max: f32) -> f32 {
        let start = Vec3::new(feet.x, feet.y.min(surface), feet.z);
        let below = surface - start.y;
        let probe = (max - below).max(0.0);
        match self.ground(start, probe) {
            Some(ground) => below + ground.drop,
            None => max.max(below),
        }
    }
}

/// A ladder as the body touching it meets it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ladder {
    /// Out of the ladder, toward the side it is climbed from: level, of length 1.
    pub out: Vec3,
    /// The body's middle is on that side of the ladder's own middle: it can take hold of it.
    pub front: bool,
}

/// What volumes say about a position.
#[derive(Clone, Debug, Default)]
pub(crate) struct Surroundings {
    pub volumes: Vec<Entity>,
    pub water: Option<f32>,
    pub updraft: Option<(f32, f32)>,
}

/// How far along `dir` a shape may go toward `hit` to end `gap` away from it, measured
/// along the surface normal.
fn stop_short(hit: &ShapeHit, dir: Vec3, gap: f32) -> f32 {
    let closing = dir.dot(-hit.normal).max(1e-3);
    (hit.distance - gap / closing).max(0.0)
}

/// `v` on the ground plane.
pub(crate) fn flat(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z)
}

/// Removes the part of `v` that goes into a surface with normal `n`.
pub(crate) fn clip(v: Vec3, n: Vec3) -> Vec3 {
    let into = v.dot(n);
    if into < 0.0 { v - n * into } else { v }
}
