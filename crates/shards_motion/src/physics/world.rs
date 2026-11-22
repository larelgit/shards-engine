//! [`PhysicsWorld`]: world-space colliders and the queries on them. The only place
//! that talks to parry; everything public is in our own types (glam `Vec3`/`Quat`).

use std::sync::Arc;

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use parry3d::math::Pose;
use parry3d::query::{self, Ray, ShapeCastOptions};
use parry3d::shape::{Ball, Capsule, Cone, Cuboid, Cylinder, Segment, Shape as ParryShape};

use super::broad::{Cells, Grid};
use super::field::VoxelField;
use super::{Body, ColliderData, ColliderId, CollisionLayers, LayerMask, QueryFilter, Shape, VolumeKind};

/// What a ray hit first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    /// Along the ray from its origin; 0 when the origin is inside a collider.
    pub distance: f32,
    pub point: Vec3,
    /// Surface normal at `point`, facing the ray's origin.
    pub normal: Vec3,
}

/// Where a moving shape first touched something.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeHit {
    pub entity: Entity,
    /// How far the shape travels before touching; 0 when it starts overlapping.
    pub distance: f32,
    /// Contact point on the collider that was hit.
    pub point: Vec3,
    /// Surface normal of the collider that was hit, facing the moving shape.
    pub normal: Vec3,
}

/// A collider a shape sinks into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Penetration {
    pub entity: Entity,
    /// Push the shape this way (out of the collider)...
    pub normal: Vec3,
    /// ...this far (m) to separate them.
    pub depth: f32,
}

/// A ladder volume as a climber meets it ([`PhysicsWorld::ladders_touching`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LadderFace {
    /// Out of the ladder, toward the side it is climbed from: level, of length 1.
    pub out: Vec3,
    /// The middle of the ladder's volume.
    pub center: Vec3,
}

/// One collider in world space, as of the last sync.
#[derive(Clone, Debug)]
pub struct ColliderInfo {
    pub entity: Entity,
    pub id: ColliderId,
    pub shape: Shape,
    /// World position of the shape's center (entity position + rotated offset).
    pub position: Vec3,
    pub rotation: Quat,
    /// The collider's bounding box (outside the crate: [`bounds`](Self::bounds)).
    pub(crate) aabb_min: Vec3,
    pub(crate) aabb_max: Vec3,
    pub layers: CollisionLayers,
    pub body: Body,
    /// Set for sensors.
    pub volume: Option<VolumeKind>,
    geom: Geom,
}

impl ColliderInfo {
    fn pose(&self) -> Pose {
        Pose::from_parts(self.position, self.rotation)
    }

    fn passes(&self, filter: &QueryFilter) -> bool {
        (filter.volumes || self.volume.is_none())
            && filter.mask.intersects(self.layers.memberships)
            && !filter.exclude.contains(&self.entity)
    }

    /// Whether `point` is inside the shape.
    pub fn contains(&self, point: Vec3) -> bool {
        aabb_contains(self.aabb_min, self.aabb_max, point) && self.geom.as_dyn().contains_point(&self.pose(), point)
    }

    /// The collider's box (least and greatest corner), not the solid: a [`VoxelField`] is no
    /// collider and has no box, so code that reads the top or the bottom of what a query hit
    /// asks [`PhysicsWorld::extent_at`], [`top_under`](PhysicsWorld::top_under) or
    /// [`solid_span`](PhysicsWorld::solid_span) instead, which answer this box's numbers for a
    /// collider and the cells' for a field. Disallowed by `clippy.toml`: a caller says why with
    /// `#[expect(clippy::disallowed_methods, reason = "…")]` (`docs/manual.md`, "Movement").
    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.aabb_min, self.aabb_max)
    }
}

/// Our [`Shape`] as a parry shape. Small value types: kept by value, no allocation.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Geom {
    Cuboid(Cuboid),
    Ball(Ball),
    Capsule(Capsule),
    Cylinder(Cylinder),
    Cone(Cone),
    /// The core of a capsule (see [`Geom::split`]).
    Segment(Segment),
}

impl Geom {
    /// Negative or NaN sizes become 0, so a bad value in content cannot crash a query.
    pub(crate) fn new(shape: &Shape) -> Self {
        let len = |v: f32| if v >= 0.0 && v.is_finite() { v } else { 0.0 };
        match *shape {
            Shape::Cuboid { half } => Geom::Cuboid(Cuboid::new(Vec3::new(len(half.x), len(half.y), len(half.z)))),
            Shape::Sphere { radius } => Geom::Ball(Ball::new(len(radius))),
            Shape::Capsule { half_height, radius } => Geom::Capsule(Capsule::new_y(len(half_height), len(radius))),
            Shape::Cylinder { half_height, radius } => Geom::Cylinder(Cylinder::new(len(half_height), len(radius))),
            Shape::Cone { half_height, radius } => Geom::Cone(Cone::new(len(half_height), len(radius))),
        }
    }

    fn as_dyn(&self) -> &dyn ParryShape {
        match self {
            Geom::Cuboid(s) => s,
            Geom::Ball(s) => s,
            Geom::Capsule(s) => s,
            Geom::Cylinder(s) => s,
            Geom::Cone(s) => s,
            Geom::Segment(s) => s,
        }
    }

    /// A rounded shape as its core plus a radius. GJK's closest points between shapes
    /// that touch are ill-defined (any normal will do), so contacts are measured
    /// between the cores, which stay a radius apart.
    fn split(&self) -> (Geom, f32) {
        match self {
            Geom::Capsule(c) => (Geom::Segment(c.segment), c.radius),
            Geom::Ball(b) => (Geom::Ball(Ball::new(0.0)), b.radius),
            other => (*other, 0.0),
        }
    }

    fn aabb(&self, pose: &Pose) -> (Vec3, Vec3) {
        let aabb = self.as_dyn().compute_aabb(pose);
        (aabb.mins, aabb.maxs)
    }
}

/// Signed distance between two posed shapes (negative: how deep they overlap), the
/// unit normal from the first toward the second, and the closest point on the second.
/// `None` when they are farther apart than `prediction`.
fn separation(p1: &Pose, g1: &Geom, p2: &Pose, g2: &Geom, prediction: f32) -> Option<(f32, Vec3, Vec3)> {
    let ((c1, r1), (c2, r2)) = (g1.split(), g2.split());
    let contact = query::contact(p1, c1.as_dyn(), p2, c2.as_dyn(), prediction + r1 + r2).ok()??;
    let normal = contact.normal1.try_normalize()?;
    Some((contact.dist - r1 - r2, normal, contact.point2 - normal * r2))
}

/// GJK may overestimate a time of impact by this much (m), so later candidates are
/// still cast a little beyond the best refined hit so far.
const REFINE_SLACK: f32 = 0.01;

/// GJK's time of impact is only good to a few millimeters (its tolerance is relative).
/// Newton steps on the signed distance along the cast make it exact to float precision
/// and give the true surface normal. Grazing hits keep GJK's answer.
/// Only pairs with a rounded shape: two sharp shapes that touch have no well-defined
/// contact normal. `None`: the true impact lies beyond `max`.
fn refine(geom: &Geom, pose: &Pose, dir: Vec3, max: f32, c: &ColliderInfo, hit: ShapeHit) -> Option<ShapeHit> {
    if hit.distance <= 0.0 || geom.split().1 + c.geom.split().1 <= 0.0 {
        return Some(hit);
    }
    let other = c.pose();
    let mut best = hit;
    let mut t = hit.distance;
    for _ in 0..3 {
        let at = Pose::from_parts(pose.translation + dir * t, pose.rotation);
        let Some((dist, normal, point)) = separation(&at, geom, &other, &c.geom, 0.1) else {
            break;
        };
        let closing = dir.dot(normal);
        if closing < 0.05 {
            break;
        }
        best = ShapeHit { entity: c.entity, distance: t, point, normal: -normal };
        let next = (t + dist / closing).max(0.0);
        if next > max {
            return None;
        }
        if (next - t).abs() < 1e-6 {
            break;
        }
        t = next;
        best.distance = t;
    }
    Some(best)
}

/// One collider of a [`PhysicsWorld::sweep`]: where `geom` moving along `dir` first touches `c`
/// within `reach`, refined (within `max`). `None`: it does not, or it starts touching `c` and
/// the motion leads away (with [`StartOverlap::LetOut`]).
fn sweep_one(
    geom: &Geom,
    pose: &Pose,
    dir: Vec3,
    max: f32,
    reach: f32,
    start: StartOverlap,
    c: &ColliderInfo,
) -> Option<ShapeHit> {
    let options = ShapeCastOptions {
        max_time_of_impact: reach,
        target_distance: 0.0,
        stop_at_penetration: start == StartOverlap::Hit,
        compute_impact_geometry_on_penetration: true,
    };
    let other = c.pose();
    let Ok(Some(hit)) = query::cast_shapes(pose, dir, geom.as_dyn(), &other, Vec3::ZERO, c.geom.as_dyn(), options) else {
        return None;
    };
    let rough = ShapeHit {
        entity: c.entity,
        distance: hit.time_of_impact.max(0.0),
        point: other * hit.witness2,
        normal: -(pose.rotation * hit.normal1).normalize_or(Vec3::Y),
    };
    // Touching at the start and sliding along (a body resting against a wall) is
    // no hit. Parry tells that from GJK's normal, which is noise for shapes that
    // touch and reports a hit just past the start; the cores' normal is clean.
    if start == StartOverlap::LetOut && rough.distance < REFINE_SLACK && !approaches(geom, pose, dir, c) {
        return None;
    }
    refine(geom, pose, dir, max, c, rough)
}

/// Whether moving along `dir` takes `geom` nearer to `c`, by more than rounding. By
/// convexity a shape that does not draw nearer at the start never meets it.
fn approaches(geom: &Geom, pose: &Pose, dir: Vec3, c: &ColliderInfo) -> bool {
    separation(pose, geom, &c.pose(), &c.geom, REFINE_SLACK).is_none_or(|(_, normal, _)| dir.dot(normal) > 1e-4)
}

/// Whether `geom` cast along `dir` can meet `c` within `reach`: nearer than that at the start
/// and drawing nearer. The distance between convex shapes under a translation is convex in
/// the distance moved, so a shape farther than `reach` or not drawing nearer never meets it.
fn may_meet(geom: &Geom, pose: &Pose, dir: Vec3, reach: f32, c: &ColliderInfo) -> bool {
    separation(pose, geom, &c.pose(), &c.geom, reach).is_some_and(|(_, normal, _)| dir.dot(normal) > 1e-4)
}

/// How a cast treats a shape that already overlaps something at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartOverlap {
    /// Report it as a hit at distance 0 (public queries).
    Hit,
    /// Ignore it if the motion leads out of the overlap (or along it), so a body that
    /// ended up inside a wall can still walk away from it (the character controller).
    LetOut,
}

/// Fewer colliders than this are scanned, not looked up in the grid: reading cells costs more.
const SCAN_BELOW: usize = 32;

/// How many grid cells a query may read before testing every collider is cheaper: a cell costs
/// a hash lookup, a collider a few comparisons.
fn cell_budget(colliders: usize) -> u64 {
    colliders as u64 / 8 + 16
}

/// A collider and where the broad phase keeps it.
#[derive(Clone, Debug)]
struct Slot {
    info: ColliderInfo,
    cells: Cells,
}

/// Every collider in world space, ordered by [`ColliderId`], plus the queries.
/// Kept current by [`sync_physics`](super::sync_physics); usable from `&World`.
///
/// Queries ask the broad phase (`broad.rs`) which colliders are near and test those in id order,
/// as a scan of every collider would: the answers, and their order, are the scan's.
#[derive(Resource, Default, Debug)]
pub struct PhysicsWorld {
    /// Colliders where the grid finds them; a slot stays while its collider lives. Freed ones
    /// (in `free`) are reused.
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// `(id, slot)` sorted by id: the order of every iteration and every result. New colliders
    /// have the highest id, so they go at the end.
    order: Vec<(ColliderId, u32)>,
    ids: HashMap<Entity, ColliderId>,
    grid: Grid,
    /// Voxel fields, by [`ColliderId`]: every query asks them after the colliders. Empty in a
    /// world without one, where every query runs the colliders' code alone.
    fields: Vec<FieldSlot>,
    /// Tests every collider, as before the broad phase: what tests compare it with.
    #[cfg(test)]
    scan_all: bool,
}

/// A [`VoxelField`] in the world and who it is in query results.
#[derive(Clone, Debug)]
struct FieldSlot {
    entity: Entity,
    id: ColliderId,
    field: Arc<VoxelField>,
}

/// A shape cast as [`PhysicsWorld::sweep`] runs it: the shape, where it starts, where it goes, its
/// box at the start and the box it sweeps.
struct Cast<'a> {
    geom: &'a Geom,
    pose: &'a Pose,
    dir: Vec3,
    max: f32,
    start: StartOverlap,
    from: (Vec3, Vec3),
    swept: (Vec3, Vec3),
}

impl FieldSlot {
    fn passes(&self, filter: &QueryFilter) -> bool {
        filter.mask.intersects(self.field.layers().memberships) && !filter.exclude.contains(&self.entity)
    }

    /// The cells `min..max` (max exclusive) as one box collider, for the code that tests
    /// colliders one by one. One cell is the cuboid a collider at its centre would be, to the bit.
    fn cells(&self, min: IVec3, max: IVec3) -> ColliderInfo {
        let f = &self.field;
        let half = (max - min).as_vec3() * f.cell() * 0.5;
        let position = if max - min == IVec3::ONE { f.center(min) } else { f.origin() + (min + max).as_vec3() * 0.5 * f.cell() };
        ColliderInfo {
            entity: self.entity,
            id: self.id,
            shape: Shape::Cuboid { half },
            position,
            rotation: Quat::IDENTITY,
            aabb_min: position - half,
            aabb_max: position + half,
            layers: f.layers(),
            body: Body::Static,
            volume: None,
            geom: Geom::Cuboid(Cuboid::new(half)),
        }
    }

    fn cell(&self, c: IVec3) -> ColliderInfo {
        self.cells(c, c + IVec3::ONE)
    }

    /// Whether the shape overlaps solid cell `c`.
    fn touches(&self, geom: &Geom, pose: &Pose, c: IVec3) -> bool {
        let cell = self.cell(c);
        query::intersection_test(pose, geom.as_dyn(), &cell.pose(), cell.geom.as_dyn()).is_ok_and(|t| t.intersecting)
    }

    /// The solid cells under the box `lo..=hi`, in `(x, y, z)` order (x outermost).
    fn solid_in(&self, lo: Vec3, hi: Vec3) -> impl Iterator<Item = IVec3> + '_ {
        let range = self.field.cells_in(lo, hi);
        range
            .into_iter()
            .flat_map(|(a, b)| (a.x..=b.x).flat_map(move |x| (a.y..=b.y).flat_map(move |y| (a.z..=b.z).map(move |z| IVec3::new(x, y, z)))))
            .filter(|c| self.field.solid(*c))
    }

    /// The field's part of a [`PhysicsWorld::sweep`], folded into `best` and `reach` as a collider
    /// would be.
    ///
    /// The candidates are the solid cells under the swept box, each tested as the box it is, in
    /// `(x, y, z)` order: what a world of one collider per cell (ids in that order) tests, to the
    /// bit. When the swept box covers more than one cell and the field merges casts (the default,
    /// [`VoxelField::with_cast_merge`]), the cells outside the start box are merged into column
    /// runs (per column, the run of cells
    /// contiguous in y within the swept box; equal runs of neighbouring columns into
    /// rectangles, along x, then z): the union of the boxes is the union of the cells, so the
    /// first touch is the same up to rounding, and a cast that covers many cells tests a few
    /// boxes instead of each of them. The cells under the start box stay single, so a body
    /// that starts touching or inside the solid is let out of (or stopped by) each cell as with
    /// a collider per cell.
    ///
    /// Tie-break: candidates are tried in `(x, y, z)` order of their least cell (x first, as
    /// integers) and only a strictly nearer hit replaces the best, so among hits at the same
    /// distance (the same `f32`) the lowest cell wins; a hit of a field replaces a collider's
    /// only when strictly nearer, and of two fields the lower id is asked first.
    fn sweep(&self, cast: &Cast, best: &mut Option<ShapeHit>, reach: &mut f32) {
        let Some((a, b)) = self.field.cells_in(cast.swept.0, cast.swept.1) else {
            return;
        };
        let inside = self.field.cells_in(cast.from.0, cast.from.1);
        let in_start = |c: IVec3| inside.is_some_and(|(p, q)| c.cmpge(p).all() && c.cmple(q).all());
        // Casts at one candidate; whether it was hit (nearer than the best or not).
        let test = |c: &ColliderInfo, best: &mut Option<ShapeHit>, reach: &mut f32| {
            let hit = sweep_one(cast.geom, cast.pose, cast.dir, cast.max, *reach, cast.start, c);
            if let Some(hit) = hit
                && best.is_none_or(|b| hit.distance < b.distance)
            {
                *reach = (hit.distance + REFINE_SLACK).min(cast.max);
                *best = Some(hit);
            }
            hit.is_some()
        };
        // The solid cells of `lo..=hi` one by one, as cuboid colliders would be.
        let cells = |lo: IVec3, hi: IVec3, best: &mut Option<ShapeHit>, reach: &mut f32| {
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        let c = IVec3::new(x, y, z);
                        if !self.field.solid(c) {
                            continue;
                        }
                        let cell = self.cell(c);
                        if aabb_overlap(cast.swept.0, cast.swept.1, cell.aabb_min, cell.aabb_max) {
                            test(&cell, best, reach);
                        }
                    }
                }
            }
        };
        if !self.field.merges_casts() || a == b {
            cells(a, b, best, reach);
            return;
        }
        for (min, max) in self.merged(a, b, in_start) {
            let merged = self.cells(min, max);
            // parry's cast misses a box now and then that it is about to touch (a capsule
            // over a box's edge, 13 mm away: `a_merged_box_parry_misses_is_cast_cell_by_cell`)
            // and finds the same contact on the cells. A box the cast missed but whose
            // shape is within reach and drawing nearer is cast cell by cell, as the merge off
            // casts it; by convexity, a box that is farther than the reach or that the shape
            // does not draw nearer to is not met at all.
            if !test(&merged, best, reach) && max - min != IVec3::ONE && may_meet(cast.geom, cast.pose, cast.dir, *reach, &merged) {
                cells(min, max - IVec3::ONE, best, reach);
            }
        }
    }

    /// The candidates of a merged [`sweep`](Self::sweep) over cells `a..=b`: the solid cells
    /// under the start box one by one, the others as column runs merged into rectangles, all
    /// in `(x, y, z)` order of their least cell. Boxes as `min..max`, max exclusive.
    fn merged(&self, a: IVec3, b: IVec3, in_start: impl Fn(IVec3) -> bool) -> Vec<(IVec3, IVec3)> {
        let (w, d) = ((b.x - a.x + 1) as usize, (b.z - a.z + 1) as usize);
        let mut out: Vec<(IVec3, IVec3)> = Vec::new();
        // Runs `y0..y1` per column `(x - a.x) * d + (z - a.z)`, and whether a box took each.
        let mut runs: Vec<Vec<(i32, i32, bool)>> = vec![Vec::new(); w * d];
        for x in a.x..=b.x {
            for z in a.z..=b.z {
                let col = &mut runs[(x - a.x) as usize * d + (z - a.z) as usize];
                let mut open: Option<i32> = None;
                for y in a.y..=b.y + 1 {
                    let c = IVec3::new(x, y, z);
                    let solid = y <= b.y && self.field.solid(c);
                    let single = solid && in_start(c);
                    if single {
                        out.push((c, c + IVec3::ONE));
                    }
                    match (solid && !single, open) {
                        (true, None) => open = Some(y),
                        (false, Some(y0)) => {
                            col.push((y0, y, false));
                            open = None;
                        }
                        _ => {}
                    }
                }
            }
        }
        let at = |x: i32, z: i32| (x - a.x) as usize * d + (z - a.z) as usize;
        let free = |runs: &Vec<Vec<(i32, i32, bool)>>, x: i32, z: i32, r: (i32, i32)| {
            runs[at(x, z)].iter().position(|&(y0, y1, used)| !used && (y0, y1) == r)
        };
        for z in a.z..=b.z {
            for x in a.x..=b.x {
                for k in 0..runs[at(x, z)].len() {
                    let (y0, y1, used) = runs[at(x, z)][k];
                    if used {
                        continue;
                    }
                    let r = (y0, y1);
                    let mut x1 = x + 1;
                    while x1 <= b.x && free(&runs, x1, z, r).is_some() {
                        x1 += 1;
                    }
                    let mut z1 = z + 1;
                    while z1 <= b.z && (x..x1).all(|xx| free(&runs, xx, z1, r).is_some()) {
                        z1 += 1;
                    }
                    for zz in z..z1 {
                        for xx in x..x1 {
                            if let Some(k) = free(&runs, xx, zz, r) {
                                runs[at(xx, zz)][k].2 = true;
                            }
                        }
                    }
                    out.push((IVec3::new(x, y0, z), IVec3::new(x1, y1, z1)));
                }
            }
        }
        out.sort_unstable_by_key(|(min, _)| (min.x, min.y, min.z));
        out
    }
}

impl PhysicsWorld {
    /// How many colliders: the voxel fields are not counted ([`fields`](Self::fields) lists
    /// them), so a world of one field is [`is_empty`](Self::is_empty).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// No collider (there may be voxel fields).
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// How many colliders are wider than the broad phase's widest cell (a sea box kilometres
    /// across): every query that reaches volumes or colliders near a point reads all of them.
    pub fn wide_colliders(&self) -> usize {
        self.grid.wide_len()
    }

    /// All colliders in id order (not the voxel fields: [`fields`](Self::fields)).
    pub fn colliders(&self) -> impl Iterator<Item = &ColliderInfo> {
        self.order.iter().map(|(_, slot)| &self.slots[*slot as usize].info)
    }

    /// The collider of `entity`. A voxel field has none: code that reads the solid under a hit
    /// asks [`extent_at`](Self::extent_at), [`top_under`](Self::top_under) or
    /// [`solid_span`](Self::solid_span). Disallowed by `clippy.toml`: a caller says why with
    /// `#[expect(clippy::disallowed_methods, reason = "…")]`.
    pub fn collider(&self, entity: Entity) -> Option<&ColliderInfo> {
        self.info(entity)
    }

    /// The collider with id `id` (a voxel field's id names none). Disallowed by `clippy.toml`,
    /// as [`collider`](Self::collider) is.
    pub fn by_id(&self, id: ColliderId) -> Option<&ColliderInfo> {
        self.info_by_id(id)
    }

    fn info_by_id(&self, id: ColliderId) -> Option<&ColliderInfo> {
        self.find(id).ok().map(|i| &self.slots[self.order[i].1 as usize].info)
    }

    /// Where `id` is in `order`, or where it would go.
    fn find(&self, id: ColliderId) -> Result<usize, usize> {
        self.order.binary_search_by_key(&id, |(id, _)| *id)
    }

    /// A world of the colliders `keep` keeps and the voxel fields `keep_field` keeps, in the same
    /// order: for rays and shapes that only kept colliders and fields can reach, it answers as
    /// this world does (strewing a tile asks the static solids near it, not the bodies that walk
    /// there). A field is shared, not copied.
    pub fn subset(&self, keep: impl Fn(&ColliderInfo) -> bool, keep_field: impl Fn(&VoxelField) -> bool) -> PhysicsWorld {
        let mut world = PhysicsWorld::default();
        for c in self.colliders().filter(|c| keep(c)) {
            world.ids.insert(c.entity, c.id);
            world.put(c.clone());
        }
        world.fields = self.fields.iter().filter(|f| keep_field(&f.field)).cloned().collect();
        world
    }

    /// The voxel fields, in id order: the entity that carries each, its id and its cells.
    pub fn fields(&self) -> impl Iterator<Item = (Entity, ColliderId, &VoxelField)> {
        self.fields.iter().map(|f| (f.entity, f.id, &*f.field))
    }

    /// The voxel field of `entity`, if it carries one.
    pub fn field(&self, entity: Entity) -> Option<&VoxelField> {
        self.field_slot(entity).map(|f| &*f.field)
    }

    fn field_slot(&self, entity: Entity) -> Option<&FieldSlot> {
        self.fields.iter().find(|f| f.entity == entity)
    }

    /// The collider of `entity`, for the hooks (which say what a field answers instead).
    fn info(&self, entity: Entity) -> Option<&ColliderInfo> {
        self.ids.get(&entity).and_then(|id| self.info_by_id(*id))
    }

    // --- what readers used to take from a collider's box ----------------------------

    /// Bottom and top (world y) of the solid of `entity` that holds `point`: a collider's box
    /// (whatever `point` is); for a voxel field, the run of solid cells (contiguous in y) that
    /// holds the cell of `point`, tried at `point`, then 0.1 mm below it, then 0.1 mm above it
    /// (a ray that starts inside a solid stops on or in a cell), never the whole column: under a
    /// plateau with a tunnel through it, a point in the tunnel's floor gets the floor's run and
    /// a point in the plateau the plateau's. `None`: no such solid, or no solid cell there.
    pub fn extent_at(&self, entity: Entity, point: Vec3) -> Option<(f32, f32)> {
        if let Some(c) = self.info(entity) {
            return Some((c.aabb_min.y, c.aabb_max.y));
        }
        let f = &self.field_slot(entity)?.field;
        [point, point - Vec3::Y * 1e-4, point + Vec3::Y * 1e-4].into_iter().find_map(|p| f.run_extent(f.cell_at(p)))
    }

    /// The tallest top (world y) among the parts of `entity` that `shape` at `pos`, turned by
    /// `rot`, overlaps: a collider's box top (the shape is not tested again); for a voxel field,
    /// the top of the run of each solid cell the shape overlaps, the tallest. A capsule standing
    /// in a tunnel overlaps its floor and walls, not the plateau above it, so the plateau is no
    /// blocker. `None`: no such solid, or the shape overlaps no solid cell of it.
    pub fn top_under(&self, entity: Entity, shape: &Shape, pos: Vec3, rot: Quat) -> Option<f32> {
        if let Some(c) = self.info(entity) {
            return Some(c.aabb_max.y);
        }
        let f = self.field_slot(entity)?;
        let (geom, pose) = (Geom::new(shape), Pose::from_parts(pos, rot.normalize()));
        let (lo, hi) = geom.aabb(&pose);
        f.solid_in(lo, hi).filter(|c| f.touches(&geom, &pose, *c)).filter_map(|c| Some(f.field.run_extent(c)?.1)).reduce(f32::max)
    }

    /// The lowest bottom and the highest top (world y) of every solid on `mask`: the boxes of
    /// the colliders that are not volumes (static or not) and the solid cells of the fields.
    /// `None` when there is none.
    pub fn solid_span(&self, mask: LayerMask) -> Option<(f32, f32)> {
        let solids = self.colliders().filter(|c| c.volume.is_none() && c.layers.memberships.intersects(mask));
        let (mut top, mut bottom) = solids.fold((f32::MIN, f32::MAX), |(t, b), c| (t.max(c.aabb_max.y), b.min(c.aabb_min.y)));
        for f in self.fields.iter().filter(|f| f.field.layers().memberships.intersects(mask)) {
            if let Some((low, high)) = f.field.solid_span() {
                (top, bottom) = (top.max(high), bottom.min(low));
            }
        }
        (top >= bottom).then_some((bottom, top))
    }

    /// Whether there is any solid on `mask`: a collider that is not a volume, or a field with a
    /// solid cell: whether [`solid_span`](Self::solid_span) has an answer, stopping at the first.
    pub fn has_solid(&self, mask: LayerMask) -> bool {
        self.colliders().any(|c| c.volume.is_none() && c.layers.memberships.intersects(mask))
            || self.fields.iter().any(|f| f.field.layers().memberships.intersects(mask) && f.field.solid_cells() > 0)
    }

    /// The colliders a query has to test, in id order: those the grid finds near it that pass
    /// `keep` (the query's own bounding-box test), or every collider. `gather` reads the grid and
    /// says whether it did (see [`Grid::near_box`]).
    fn near(&self, gather: impl FnOnce(&Grid, u64, &mut Vec<u32>) -> bool, keep: impl Fn(&ColliderInfo) -> bool) -> Near<'_> {
        #[cfg(test)]
        if self.scan_all {
            return Near { slots: &self.slots, which: Which::All(self.order.iter()) };
        }
        if self.order.len() >= SCAN_BELOW {
            let mut found = Vec::new();
            if gather(&self.grid, cell_budget(self.order.len()), &mut found) {
                let mut near: Vec<(ColliderId, u32)> = found
                    .into_iter()
                    .filter_map(|slot| {
                        let info = &self.slots[slot as usize].info;
                        keep(info).then_some((info.id, slot))
                    })
                    .collect();
                near.sort_unstable_by_key(|(id, _)| *id);
                near.dedup_by_key(|(id, _)| *id);
                return Near { slots: &self.slots, which: Which::Found(near.into_iter()) };
            }
        }
        Near { slots: &self.slots, which: Which::All(self.order.iter()) }
    }

    /// [`near`](Self::near) a box: what overlaps `lo..hi`.
    fn near_box(&self, lo: Vec3, hi: Vec3) -> Near<'_> {
        self.near(|grid, budget, out| grid.near_box(lo, hi, budget, out), |c| aabb_overlap(lo, hi, c.aabb_min, c.aabb_max))
    }

    // --- maintenance (sync and hooks) ---------------------------------------------

    /// Puts `info` where its id goes, in place of a collider with that id, and in the grid.
    fn put(&mut self, info: ColliderInfo) {
        let cells = Cells::of(info.aabb_min, info.aabb_max);
        match self.find(info.id) {
            Ok(i) => {
                let slot = self.order[i].1;
                let old = self.slots[slot as usize].cells;
                if old != cells {
                    self.grid.remove(slot, old);
                    self.grid.insert(slot, cells);
                }
                self.slots[slot as usize] = Slot { info, cells };
            }
            Err(i) => {
                let id = info.id;
                let slot = match self.free.pop() {
                    Some(slot) => {
                        self.slots[slot as usize] = Slot { info, cells };
                        slot
                    }
                    None => {
                        self.slots.push(Slot { info, cells });
                        (self.slots.len() - 1) as u32
                    }
                };
                self.grid.insert(slot, cells);
                self.order.insert(i, (id, slot));
            }
        }
    }

    pub(crate) fn upsert(&mut self, (entity, id, collider, tf, body, volume): ColliderData) {
        if let Some(old) = self.ids.get(&entity).copied()
            && old != *id
        {
            self.remove(entity);
        }
        let geom = Geom::new(&collider.shape);
        let rotation = tf.rotation.normalize();
        let position = tf.translation + rotation * collider.offset;
        let (aabb_min, aabb_max) = geom.aabb(&Pose::from_parts(position, rotation));
        // A NaN or infinite pose would answer every ray in the world (a hit at distance
        // 0): left out until its transform is real again.
        if !(position.is_finite() && rotation.is_finite() && aabb_min.is_finite() && aabb_max.is_finite()) {
            warn!(
                "{entity}: collider at {} (rotation {}, offset {}) is not finite, so physics ignores it; \
                 give it a real transform",
                tf.translation, tf.rotation, collider.offset
            );
            self.remove(entity);
            return;
        }
        let info = ColliderInfo {
            entity,
            id: *id,
            shape: collider.shape,
            position,
            rotation,
            aabb_min,
            aabb_max,
            layers: collider.layers,
            body: body.copied().unwrap_or_default(),
            volume: volume.map(|v| v.kind.clone()),
            geom,
        };
        self.put(info);
        self.ids.insert(entity, *id);
    }

    pub(crate) fn remove(&mut self, entity: Entity) {
        if let Some(id) = self.ids.remove(&entity)
            && let Ok(i) = self.find(id)
        {
            let (_, slot) = self.order.remove(i);
            self.grid.remove(slot, self.slots[slot as usize].cells);
            self.free.push(slot);
        }
    }

    pub(crate) fn clear_volume(&mut self, entity: Entity) {
        if let Some(id) = self.ids.get(&entity).copied()
            && let Ok(i) = self.find(id)
        {
            self.slots[self.order[i].1 as usize].info.volume = None;
        }
    }

    /// Puts the voxel field of `entity` (id `id`) in its place in id order, in place of the one it
    /// had (a replaced component, a new id).
    pub(crate) fn upsert_field(&mut self, entity: Entity, id: ColliderId, field: &Arc<VoxelField>) {
        self.remove_field(entity);
        let at = self.fields.partition_point(|f| f.id < id);
        self.fields.insert(at, FieldSlot { entity, id, field: field.clone() });
    }

    pub(crate) fn remove_field(&mut self, entity: Entity) {
        self.fields.retain(|f| f.entity != entity);
    }

    pub(crate) fn refresh_kinematic<'a>(&mut self, mut read: impl FnMut(Entity) -> Option<ColliderData<'a>>) {
        let moving: Vec<Entity> = self.colliders().filter(|c| c.body == Body::Kinematic).map(|c| c.entity).collect();
        for entity in moving {
            if let Some(data) = read(entity) {
                self.upsert(data);
            }
        }
    }

    // --- queries ------------------------------------------------------------------

    /// The first collider along the ray within `max` meters.
    pub fn raycast(&self, origin: Vec3, dir: Dir3, max: f32, filter: &QueryFilter) -> Option<RayHit> {
        let ray = Ray::new(origin, *dir);
        let mut best: Option<RayHit> = None;
        let mut reach = max;
        let near = self.near(
            |grid, budget, out| grid.near_ray(origin, *dir, max, budget, out),
            |c| ray_hits_aabb(origin, *dir, max, c.aabb_min, c.aabb_max),
        );
        for c in near.filter(|c| c.passes(filter)) {
            if !ray_hits_aabb(origin, *dir, reach, c.aabb_min, c.aabb_max) {
                continue;
            }
            let Some(hit) = c.geom.as_dyn().cast_ray_and_get_normal(&c.pose(), &ray, reach, true) else {
                continue;
            };
            // Strict: on a tie the lower id (seen first) wins.
            if best.is_none_or(|b| hit.time_of_impact < b.distance) {
                reach = hit.time_of_impact;
                best = Some(RayHit {
                    entity: c.entity,
                    distance: hit.time_of_impact,
                    point: origin + *dir * hit.time_of_impact,
                    normal: hit.normal,
                });
            }
        }
        // Fields after the colliders: a field's hit wins only when strictly nearer; of two
        // fields, the lower id is asked first.
        for f in self.fields.iter().filter(|f| f.passes(filter)) {
            if let Some(hit) = f.field.raycast(origin, *dir, reach)
                && best.is_none_or(|b| hit.distance < b.distance)
            {
                reach = hit.distance;
                best = Some(RayHit { entity: f.entity, distance: hit.distance, point: origin + *dir * hit.distance, normal: hit.normal });
            }
        }
        best
    }

    /// Moves `shape` (centered at `origin`, turned by `rot`) along `dir` and reports the
    /// first collider it touches within `max` meters. A shape that starts overlapping
    /// something hits it at distance 0.
    pub fn shapecast(
        &self,
        shape: &Shape,
        origin: Vec3,
        rot: Quat,
        dir: Dir3,
        max: f32,
        filter: &QueryFilter,
    ) -> Option<ShapeHit> {
        let pose = Pose::from_parts(origin, rot.normalize());
        self.sweep(&Geom::new(shape), &pose, *dir, max, filter, StartOverlap::Hit)
    }

    /// Shape cast with a ready parry shape and pose; `dir` must be unit length.
    pub(crate) fn sweep(
        &self,
        geom: &Geom,
        pose: &Pose,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
        start: StartOverlap,
    ) -> Option<ShapeHit> {
        let (lo, hi) = geom.aabb(pose);
        let travel = dir * max.max(0.0);
        let (sweep_min, sweep_max) = (lo.min(lo + travel), hi.max(hi + travel));
        let mut best: Option<ShapeHit> = None;
        let mut reach = max;
        for c in self.near_box(sweep_min, sweep_max).filter(|c| c.passes(filter)) {
            if !aabb_overlap(sweep_min, sweep_max, c.aabb_min, c.aabb_max) {
                continue;
            }
            let Some(hit) = sweep_one(geom, pose, dir, max, reach, start, c) else {
                continue;
            };
            // Strict: on a tie the lower id (seen first) wins.
            if best.is_none_or(|b| hit.distance < b.distance) {
                reach = (hit.distance + REFINE_SLACK).min(max);
                best = Some(hit);
            }
        }
        // Fields after the colliders, a field only when strictly nearer (see `FieldSlot::sweep`).
        for f in self.fields.iter().filter(|f| f.passes(filter)) {
            let cast = Cast { geom, pose, dir, max, start, from: (lo, hi), swept: (sweep_min, sweep_max) };
            f.sweep(&cast, &mut best, &mut reach);
        }
        best
    }

    /// Entities whose colliders overlap `shape` at `pos`, in id order.
    pub fn overlaps(&self, shape: &Shape, pos: Vec3, rot: Quat, filter: &QueryFilter) -> Vec<Entity> {
        let geom = Geom::new(shape);
        let pose = Pose::from_parts(pos, rot.normalize());
        if self.fields.is_empty() {
            return self.overlapping(&geom, &pose, filter).map(|c| c.entity).collect();
        }
        let mut found: Vec<(ColliderId, Entity)> = self.overlapping(&geom, &pose, filter).map(|c| (c.id, c.entity)).collect();
        let (lo, hi) = geom.aabb(&pose);
        for f in self.fields.iter().filter(|f| f.passes(filter)) {
            if f.solid_in(lo, hi).any(|c| f.touches(&geom, &pose, c)) {
                let at = found.partition_point(|(id, _)| *id <= f.id);
                found.insert(at, (f.id, f.entity));
            }
        }
        found.into_iter().map(|(_, e)| e).collect()
    }

    pub(crate) fn overlapping<'a>(
        &'a self,
        geom: &'a Geom,
        pose: &'a Pose,
        filter: &'a QueryFilter,
    ) -> impl Iterator<Item = &'a ColliderInfo> + 'a {
        let (lo, hi) = geom.aabb(pose);
        self.near_box(lo, hi).filter(move |c| {
            c.passes(filter)
                && aabb_overlap(lo, hi, c.aabb_min, c.aabb_max)
                && query::intersection_test(pose, geom.as_dyn(), &c.pose(), c.geom.as_dyn())
                    .is_ok_and(|t| t.intersecting)
        })
    }

    /// Colliders `shape` sinks into, and how to push it out of each, in id order.
    pub fn penetrations(&self, shape: &Shape, pos: Vec3, rot: Quat, filter: &QueryFilter) -> Vec<Penetration> {
        let geom = Geom::new(shape);
        let pose = Pose::from_parts(pos, rot.normalize());
        self.closer_than(&geom, &pose, filter, 0.0)
    }

    /// Colliders nearer to the shape than `margin` (penetrating ones included), with
    /// how far to push the shape to put it `margin` away from each, in id order.
    pub(crate) fn closer_than(&self, geom: &Geom, pose: &Pose, filter: &QueryFilter, margin: f32) -> Vec<Penetration> {
        let (lo, hi) = geom.aabb(pose);
        let (lo, hi) = (lo - Vec3::splat(margin), hi + Vec3::splat(margin));
        let near = |c: &ColliderInfo| {
            let (dist, normal, _) = separation(pose, geom, &c.pose(), &c.geom, margin)?;
            (dist < margin).then_some(Penetration { entity: c.entity, normal: -normal, depth: margin - dist })
        };
        let colliders = self.near_box(lo, hi).filter(|c| c.passes(filter) && aabb_overlap(lo, hi, c.aabb_min, c.aabb_max));
        if self.fields.is_empty() {
            return colliders.filter_map(near).collect();
        }
        // One penetration per solid cell, in (x, y, z) order, where the field's id puts it.
        let mut out: Vec<(ColliderId, Penetration)> = colliders.filter_map(|c| Some((c.id, near(c)?))).collect();
        for f in self.fields.iter().filter(|f| f.passes(filter)) {
            let at = out.partition_point(|(id, _)| *id <= f.id);
            let cells: Vec<(ColliderId, Penetration)> = f.solid_in(lo, hi).filter_map(|c| Some((f.id, near(&f.cell(c))?))).collect();
            out.splice(at..at, cells);
        }
        out.into_iter().map(|(_, p)| p).collect()
    }

    /// Volumes containing `point`, in id order. `filter` narrows them by layer and
    /// exclusions (its `volumes` flag is implied).
    pub fn volumes_at(&self, point: Vec3, filter: Option<&QueryFilter>) -> Vec<(Entity, VolumeKind)> {
        self.volumes_containing(point, filter).filter_map(|c| Some((c.entity, c.volume.clone()?))).collect()
    }

    /// The surface (the top) of the highest water volume containing `point`; `None` where there is
    /// no water. The controller asks it 5 cm above the feet; a landing search (a leap that must
    /// not come down in deep water) asks it at the landing.
    #[expect(clippy::disallowed_methods, reason = "water volumes are colliders (a voxel field is never a volume): the surface is the box's top")]
    pub fn water_surface_at(&self, point: Vec3) -> Option<f32> {
        self.volumes_containing(point, None)
            .filter(|volume| matches!(volume.volume, Some(VolumeKind::Water)))
            .map(|volume| volume.bounds().1.y)
            .reduce(f32::max)
    }

    pub(crate) fn volumes_containing<'a>(
        &'a self,
        point: Vec3,
        filter: Option<&'a QueryFilter>,
    ) -> impl Iterator<Item = &'a ColliderInfo> + 'a {
        self.near_box(point, point).filter(move |c| {
            c.volume.is_some()
                && filter.is_none_or(|f| f.mask.intersects(c.layers.memberships) && !f.exclude.contains(&c.entity))
                && c.contains(point)
        })
    }

    /// Volumes overlapping a shape, in id order.
    pub(crate) fn volumes_touching<'a>(&'a self, geom: &'a Geom, pose: &'a Pose) -> impl Iterator<Item = &'a ColliderInfo> + 'a {
        let (lo, hi) = geom.aabb(pose);
        self.near_box(lo, hi).filter(move |c| {
            c.volume.is_some()
                && aabb_overlap(lo, hi, c.aabb_min, c.aabb_max)
                && query::intersection_test(pose, geom.as_dyn(), &c.pose(), c.geom.as_dyn())
                    .is_ok_and(|t| t.intersecting)
        })
    }

    /// The ladder volumes a shape touches, in id order, each with its face turned into the world. A
    /// ladder whose face has no level part there (it points up or down) cannot be climbed and is left out.
    pub(crate) fn ladders_touching<'a>(&'a self, geom: &'a Geom, pose: &'a Pose) -> impl Iterator<Item = LadderFace> + 'a {
        self.volumes_touching(geom, pose).filter_map(|c| {
            let Some(VolumeKind::Ladder { face }) = c.volume else { return None };
            let turned = c.rotation * face;
            let out = Vec3::new(turned.x, 0.0, turned.z).try_normalize()?;
            Some(LadderFace { out, center: c.position })
        })
    }
}

/// The colliders [`PhysicsWorld::near`] hands a query, in id order.
struct Near<'a> {
    slots: &'a [Slot],
    which: Which<'a>,
}

enum Which<'a> {
    All(std::slice::Iter<'a, (ColliderId, u32)>),
    Found(std::vec::IntoIter<(ColliderId, u32)>),
}

impl<'a> Iterator for Near<'a> {
    type Item = &'a ColliderInfo;

    fn next(&mut self) -> Option<&'a ColliderInfo> {
        let (_, slot) = match &mut self.which {
            Which::All(all) => *all.next()?,
            Which::Found(found) => found.next()?,
        };
        Some(&self.slots[slot as usize].info)
    }
}

fn aabb_overlap(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.cmple(b_max).all() && b_min.cmple(a_max).all()
}

fn aabb_contains(min: Vec3, max: Vec3, p: Vec3) -> bool {
    min.cmple(p).all() && p.cmple(max).all()
}

/// Slab test: does the segment `origin → origin + dir * max` touch the box?
pub(super) fn ray_hits_aabb(origin: Vec3, dir: Vec3, max: f32, min: Vec3, max_corner: Vec3) -> bool {
    let (mut t0, mut t1) = (0.0_f32, max);
    for axis in 0..3 {
        let (o, d) = (origin[axis], dir[axis]);
        if d.abs() < 1e-12 {
            if o < min[axis] || o > max_corner[axis] {
                return false;
            }
            continue;
        }
        let (a, b) = ((min[axis] - o) / d, (max_corner[axis] - o) / d);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
        if t0 > t1 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_slab_test() {
        let (min, max) = (Vec3::splat(-1.0), Vec3::splat(1.0));
        assert!(ray_hits_aabb(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 10.0, min, max));
        assert!(!ray_hits_aabb(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 3.0, min, max));
        assert!(!ray_hits_aabb(Vec3::new(-5.0, 2.0, 0.0), Vec3::X, 10.0, min, max));
        assert!(ray_hits_aabb(Vec3::ZERO, Vec3::Y, 0.5, min, max), "inside counts");
    }

    #[test]
    fn a_subset_answers_with_the_colliders_it_keeps() {
        let mut physics = PhysicsWorld::default();
        let collider = super::super::Collider::new(Shape::cuboid(Vec3::new(1.0, 0.5, 1.0)));
        for (n, y) in [(1, 0.0), (2, 2.0), (3, 4.0)] {
            let tf = Transform::from_xyz(0.0, y, 0.0);
            let body = if n == 2 { Body::Kinematic } else { Body::Static };
            physics.upsert((Entity::from_raw_u32(n).unwrap(), &ColliderId(n), &collider, &tf, Some(&body), None));
        }
        let down = |world: &PhysicsWorld| {
            let hit = world.raycast(Vec3::new(0.0, 10.0, 0.0), Dir3::NEG_Y, 20.0, &QueryFilter::default());
            hit.map(|h| h.point.y)
        };
        assert_eq!(down(&physics), Some(4.5));
        let low = physics.subset(|c| c.aabb_max.y < 3.0, |_| true);
        assert_eq!((low.len(), down(&low)), (2, Some(2.5)), "the top box is left out");
        let still = physics.subset(|c| c.body == Body::Static, |_| true);
        assert_eq!(still.colliders().map(|c| c.id.0).collect::<Vec<_>>(), [1, 3], "in id order");
        assert!(still.info(Entity::from_raw_u32(3).unwrap()).is_some() && still.info(Entity::from_raw_u32(2).unwrap()).is_none());
        let none = physics.subset(|_| false, |_| false);
        assert!(none.is_empty() && down(&none).is_none());
    }

    // --- the broad phase against a test of every collider -------------------------

    use super::super::broad::LEVELS;
    use super::super::{Collider, LayerMask, Volume};

    /// SplitMix64: the same scenes and queries every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        /// In `0..1`.
        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.unit()
        }

        /// From `lo` to `hi`, as many below 1 m as above it: sizes and lengths.
        fn scale(&mut self, lo: f32, hi: f32) -> f32 {
            lo * ops::powf(hi / lo, self.unit())
        }

        /// A shape between `lo` and `hi` m across.
        fn sized(&mut self, lo: f32, hi: f32) -> Shape {
            let size = self.scale(lo, hi);
            self.shape(size)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, p: f32) -> bool {
            self.unit() < p
        }

        fn vec(&mut self, half: Vec3) -> Vec3 {
            Vec3::new(self.range(-half.x, half.x), self.range(-half.y, half.y), self.range(-half.z, half.z))
        }

        fn dir(&mut self) -> Dir3 {
            loop {
                if let Ok(dir) = Dir3::new(self.vec(Vec3::ONE)) {
                    return dir;
                }
            }
        }

        fn rotation(&mut self) -> Quat {
            if self.chance(0.5) {
                Quat::IDENTITY
            } else {
                Quat::from_xyzw(self.range(-1.0, 1.0), self.range(-1.0, 1.0), self.range(-1.0, 1.0), self.range(0.1, 1.0)).normalize()
            }
        }

        /// A shape about `size` m across.
        fn shape(&mut self, size: f32) -> Shape {
            let h = size * 0.5;
            match self.below(5) {
                0 => Shape::cuboid(Vec3::new(h * self.range(0.05, 1.0), h * self.range(0.05, 1.0), h * self.range(0.05, 1.0))),
                1 => Shape::sphere(h),
                2 => Shape::Capsule { half_height: h * self.range(0.0, 0.8), radius: h * self.range(0.1, 0.5) },
                3 => Shape::Cylinder { half_height: h * self.range(0.1, 1.0), radius: h * self.range(0.1, 1.0) },
                _ => Shape::Cone { half_height: h * self.range(0.1, 1.0), radius: h * self.range(0.1, 1.0) },
            }
        }
    }

    /// One collider as the sync hands it to the world.
    #[derive(Clone)]
    struct Spec {
        entity: Entity,
        id: ColliderId,
        collider: Collider,
        tf: Transform,
        body: Body,
        volume: Option<Volume>,
    }

    impl Spec {
        fn put(&self, world: &mut PhysicsWorld) {
            world.upsert((self.entity, &self.id, &self.collider, &self.tf, Some(&self.body), self.volume.as_ref()));
        }
    }

    const LAYERS: [LayerMask; 5] = [LayerMask::WORLD, LayerMask::CHARACTER, LayerMask::HURTBOX, LayerMask::VOLUME, LayerMask::game(0)];

    /// A collider of any size from props to slabs wider than the grid's widest level, near one
    /// of a few busy places or anywhere in a few kilometers, turned or not, offset or not.
    fn random_spec(rng: &mut Rng, n: u32, centers: &[Vec3]) -> Spec {
        let size = match rng.below(100) {
            0..50 => rng.scale(0.05, 3.0),
            50..75 => rng.scale(3.0, 20.0),
            75..90 => rng.scale(20.0, 140.0),
            90..97 => rng.scale(140.0, 1100.0),
            _ => rng.scale(1100.0, 4000.0),
        };
        let at = if rng.chance(0.6) {
            centers[rng.below(centers.len())] + rng.vec(Vec3::new(40.0, 10.0, 40.0))
        } else {
            rng.vec(Vec3::new(1500.0, 30.0, 1500.0))
        };
        let mut memberships = LayerMask::NONE;
        while memberships == LayerMask::NONE {
            for layer in LAYERS {
                if rng.chance(0.3) {
                    memberships |= layer;
                }
            }
        }
        let volume = rng.chance(0.15).then(|| Volume {
            kind: match rng.below(3) {
                0 => VolumeKind::Water,
                1 => VolumeKind::Updraft { accel: 5.0, max_speed: 8.0 },
                _ => VolumeKind::Trigger { name: format!("t{n}") },
            },
        });
        let offset = if rng.chance(0.2) { rng.vec(Vec3::splat(2.0)) } else { Vec3::ZERO };
        Spec {
            entity: Entity::from_raw_u32(n).unwrap(),
            id: ColliderId(n),
            collider: Collider::new(rng.shape(size)).with_offset(offset).with_layers(CollisionLayers::new(memberships, LayerMask::ALL)),
            tf: Transform::from_translation(at).with_rotation(rng.rotation()),
            body: if rng.chance(0.2) { Body::Kinematic } else { Body::Static },
            volume,
        }
    }

    fn random_filter(rng: &mut Rng, specs: &[Spec]) -> QueryFilter {
        let mut filter = QueryFilter::default();
        if rng.chance(0.4) {
            filter.mask = LAYERS[rng.below(LAYERS.len())] | LAYERS[rng.below(LAYERS.len())];
        }
        if rng.chance(0.3) && !specs.is_empty() {
            filter.exclude.push(specs[rng.below(specs.len())].entity);
        }
        filter.volumes = rng.chance(0.3);
        filter
    }

    /// Where queries start: mostly at or near colliders, some anywhere.
    fn random_point(rng: &mut Rng, world: &PhysicsWorld) -> Vec3 {
        if rng.chance(0.75) && !world.is_empty() {
            let c = world.colliders().nth(rng.below(world.len())).unwrap();
            let near = rng.scale(0.1, 30.0);
            c.position + rng.vec(Vec3::splat(near))
        } else {
            rng.vec(Vec3::new(1600.0, 40.0, 1600.0))
        }
    }

    /// Runs `query` through the broad phase and through a test of every collider: the same
    /// answer, in the same order.
    fn same<T: PartialEq + std::fmt::Debug>(world: &mut PhysicsWorld, what: &str, query: impl Fn(&PhysicsWorld) -> T) -> T {
        world.scan_all = false;
        let fast = query(world);
        world.scan_all = true;
        let scan = query(world);
        world.scan_all = false;
        assert_eq!(fast, scan, "{what}: the broad phase answers otherwise than a scan of every collider");
        fast
    }

    /// Tallies of a run of random queries: how many found something, how many the grid served.
    #[derive(Default, Debug)]
    struct Tally {
        queries: usize,
        answered: usize,
        by_grid: usize,
    }

    /// Every query, at random places, through the broad phase and through a scan.
    fn random_queries(rng: &mut Rng, world: &mut PhysicsWorld, specs: &[Spec], count: usize, tally: &mut Tally) {
        for q in 0..count {
            let filter = random_filter(rng, specs);
            let origin = random_point(rng, world);
            let dir = if rng.chance(0.3) { Dir3::NEG_Y } else { rng.dir() };
            let max = if rng.chance(0.1) { rng.scale(300.0, 5000.0) } else { rng.scale(0.05, 120.0) };
            let shape = rng.sized(0.1, 4.0);
            let rot = rng.rotation();
            let what = |kind: &str| format!("query {q} ({kind} at {origin}, dir {dir:?}, max {max}, {filter:?})");
            let found = match rng.below(8) {
                0 => same(world, &what("raycast"), |w| w.raycast(origin, dir, max, &filter)).is_some(),
                1 => same(world, &what("shapecast"), |w| w.shapecast(&shape, origin, rot, dir, max, &filter)).is_some(),
                2 => {
                    let (geom, pose) = (Geom::new(&shape), Pose::from_parts(origin, rot));
                    same(world, &what("sweep, let out"), |w| w.sweep(&geom, &pose, *dir, max, &filter, StartOverlap::LetOut)).is_some()
                }
                3 => !same(world, &what("overlaps"), |w| w.overlaps(&shape, origin, rot, &filter)).is_empty(),
                4 => !same(world, &what("penetrations"), |w| w.penetrations(&shape, origin, rot, &filter)).is_empty(),
                5 => {
                    let (geom, pose, margin) = (Geom::new(&shape), Pose::from_parts(origin, rot), rng.range(-0.2, 1.0));
                    !same(world, &what("closer_than"), |w| w.closer_than(&geom, &pose, &filter, margin)).is_empty()
                }
                6 => {
                    let filter = rng.chance(0.5).then_some(&filter);
                    !same(world, &what("volumes_at"), |w| w.volumes_at(origin, filter)).is_empty()
                }
                _ => {
                    let (geom, pose) = (Geom::new(&shape), Pose::from_parts(origin, rot));
                    let touching = |w: &PhysicsWorld| w.volumes_touching(&geom, &pose).map(|c| c.entity).collect::<Vec<_>>();
                    !same(world, &what("volumes_touching"), touching).is_empty()
                }
            };
            tally.queries += 1;
            tally.answered += usize::from(found);
            let (lo, hi) = Geom::new(&shape).aabb(&Pose::from_parts(origin, rot));
            tally.by_grid += usize::from(matches!(world.near_box(lo, hi).which, Which::Found(_)));
        }
    }

    /// The grid holds every live collider in exactly the cells its box covers, and nothing else.
    fn check_grid(world: &PhysicsWorld) {
        let mut expected: Vec<(u32, Option<(u8, IVec2)>)> = Vec::new();
        let mut per_level = [0u32; LEVELS.len()];
        for (id, slot) in &world.order {
            let s = &world.slots[*slot as usize];
            assert_eq!(s.info.id, *id, "slot {slot} holds the collider of its id");
            assert_eq!(s.cells, Cells::of(s.info.aabb_min, s.info.aabb_max), "{id} is in the cells of its box");
            match s.cells {
                Cells::Wide => expected.push((*slot, None)),
                Cells::Grid { level, min, max } => {
                    per_level[level as usize] += 1;
                    for z in min.y..=max.y {
                        for x in min.x..=max.x {
                            expected.push((*slot, Some((level, IVec2::new(x, z)))));
                        }
                    }
                }
            }
        }
        expected.sort_unstable_by_key(|(s, key)| (*s, key.map(|(l, c)| (l, c.x, c.y))));
        assert_eq!(world.grid.entries(), expected, "the grid lists the live colliders in their cells, once each");
        assert_eq!(world.grid.per_level(), per_level);
        assert!(world.order.windows(2).all(|w| w[0].0 < w[1].0), "ids in order, once each");
        assert!(world.free.iter().all(|f| world.order.iter().all(|(_, s)| s != f)), "a free slot is no collider's");
        for (entity, id) in &world.ids {
            assert!(world.info_by_id(*id).is_some(), "{entity} names {id}, which is in the world");
        }
    }

    #[test]
    fn the_broad_phase_answers_as_a_test_of_every_collider_would_in_random_worlds() {
        let mut rng = Rng(0x5eed_0001);
        let mut tally = Tally::default();
        for scene in 0..6 {
            let centers: Vec<Vec3> = (0..4).map(|_| rng.vec(Vec3::new(800.0, 5.0, 800.0))).collect();
            let count = [40, 200, 800, 1500, 300, 2500][scene];
            let specs: Vec<Spec> = (1..=count).map(|n| random_spec(&mut rng, n, &centers)).collect();
            let mut world = PhysicsWorld::default();
            specs.iter().for_each(|s| s.put(&mut world));
            check_grid(&world);
            random_queries(&mut rng, &mut world, &specs, 1500, &mut tally);
        }
        // Not a test that passes because nothing is ever found, or nothing goes through the grid.
        assert!(tally.answered * 4 > tally.queries, "{tally:?}");
        assert!(tally.by_grid * 2 > tally.queries, "{tally:?}");
    }

    #[test]
    fn moved_removed_and_renumbered_colliders_leave_no_trace_in_the_grid() {
        let mut rng = Rng(0x5eed_0002);
        let centers = [Vec3::ZERO, Vec3::new(300.0, 0.0, -200.0)];
        let mut specs: Vec<Spec> = (1..=600).map(|n| random_spec(&mut rng, n, &centers)).collect();
        let mut world = PhysicsWorld::default();
        specs.iter().for_each(|s| s.put(&mut world));
        let mut next = 601;
        let mut tally = Tally::default();
        for round in 0..30 {
            for _ in 0..40 {
                let i = rng.below(specs.len());
                match rng.below(6) {
                    // A step, as bodies take every tick: mostly in the same cells.
                    0 | 1 => {
                        specs[i].tf.translation += rng.vec(Vec3::new(0.3, 0.1, 0.3));
                        specs[i].put(&mut world);
                    }
                    // Far away, or grown into another level.
                    2 => {
                        specs[i].tf.translation = rng.vec(Vec3::new(1500.0, 20.0, 1500.0));
                        specs[i].collider.shape = rng.sized(0.1, 3000.0);
                        specs[i].tf.rotation = rng.rotation();
                        specs[i].put(&mut world);
                    }
                    // Gone.
                    3 => {
                        let gone = specs.swap_remove(i);
                        world.remove(gone.entity);
                    }
                    // The same entity under a new id (a collider put on again).
                    4 => {
                        specs[i].id = ColliderId(next);
                        next += 1;
                        specs[i].put(&mut world);
                    }
                    // A new one.
                    _ => {
                        let spec = random_spec(&mut rng, next, &centers);
                        next += 1;
                        spec.put(&mut world);
                        specs.push(spec);
                    }
                }
            }
            check_grid(&world);
            assert_eq!(world.len(), specs.len(), "round {round}");
            random_queries(&mut rng, &mut world, &specs, 150, &mut tally);
        }
        // Everything out: no cell keeps a collider.
        for spec in specs.drain(..) {
            world.remove(spec.entity);
        }
        check_grid(&world);
        assert!(world.is_empty() && world.grid.entries().is_empty() && world.grid.per_level() == [0; LEVELS.len()]);
    }

    /// A voxel region's sea: four water boxes kilometres long around its grid, wider than the
    /// broad phase's widest level, so they are on the list every query reads. A ray and a sweep
    /// near them read that list and the cells on their own path, never the colliders elsewhere.
    #[test]
    fn a_ray_and_a_sweep_by_the_sea_read_the_wide_list_and_the_cells_on_their_path() {
        let mut world = PhysicsWorld::default();
        let mut n = 0;
        let mut add = |world: &mut PhysicsWorld, shape: Shape, at: Vec3, volume: Option<Volume>| {
            n += 1;
            let collider = if volume.is_some() { Collider::new(shape).with_layers(CollisionLayers::VOLUME) } else { Collider::new(shape) };
            let spec = Spec { entity: Entity::from_raw_u32(n).unwrap(), id: ColliderId(n), collider, tf: Transform::from_translation(at), body: Body::Static, volume };
            spec.put(world);
            ColliderId(n)
        };
        let water = || Some(Volume { kind: VolumeKind::Water });
        // A 256 m grid (x and z -128..128); the sea 3 km out, from y -8 to 0.
        let (reach, half_grid) = (3000.0, 128.0);
        let mut sea = Vec::new();
        for (centre, half) in [
            (Vec3::new(0.0, -4.0, -half_grid - reach * 0.5), Vec3::new(half_grid + reach, 4.0, reach * 0.5)),
            (Vec3::new(0.0, -4.0, half_grid + reach * 0.5), Vec3::new(half_grid + reach, 4.0, reach * 0.5)),
            (Vec3::new(-half_grid - reach * 0.5, -4.0, 0.0), Vec3::new(reach * 0.5, 4.0, half_grid)),
            (Vec3::new(half_grid + reach * 0.5, -4.0, 0.0), Vec3::new(reach * 0.5, 4.0, half_grid)),
        ] {
            sea.push(add(&mut world, Shape::cuboid(half), centre, water()));
        }
        // 441 one-metre boxes, 10 m apart, over the grid.
        for i in -10..=10 {
            for j in -10..=10 {
                add(&mut world, Shape::cuboid(Vec3::splat(0.5)), Vec3::new(i as f32 * 10.0, 0.5, j as f32 * 10.0), None);
            }
        }
        check_grid(&world);
        assert_eq!(world.wide_colliders(), 4, "the four sea boxes are wide");
        let budget = cell_budget(world.order.len());
        let read = |gather: &dyn Fn(&mut Vec<u32>) -> bool| -> Vec<ColliderId> {
            let mut out = Vec::new();
            assert!(gather(&mut out), "the grid answers, not a scan");
            let mut ids: Vec<ColliderId> = out.into_iter().map(|s| world.slots[s as usize].info.id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        // Within this of a query's path, a box shares one of its cells (2 m, the first level).
        let near = |id: &ColliderId, lo: Vec3, hi: Vec3| {
            #[expect(clippy::disallowed_methods, reason = "this world has boxes only, no voxel field")]
            let c = world.by_id(*id).unwrap();
            aabb_overlap(lo - Vec3::splat(LEVELS[0] + 0.1), hi + Vec3::splat(LEVELS[0] + 0.1), c.aabb_min, c.aabb_max)
        };
        let filter = QueryFilter::default();

        // A ray across the gap between boxes at x 0 and 10: the sea, nothing else.
        let (origin, dir) = (Vec3::new(3.0, 0.5, 5.0), Dir3::X);
        let ray = read(&|out| world.grid.near_ray(origin, *dir, 4.0, budget, out));
        assert_eq!(ray, sea, "a ray between the boxes reads the wide list alone");
        assert!(world.raycast(origin, dir, 4.0, &filter).is_none());
        // A ray onto the box at (10, 0): the sea and that box.
        let origin = Vec3::new(7.0, 0.5, 0.0);
        let ray = read(&|out| world.grid.near_ray(origin, *dir, 4.0, budget, out));
        assert!(sea.iter().all(|s| ray.contains(s)), "{ray:?}");
        let others: Vec<&ColliderId> = ray.iter().filter(|id| !sea.contains(id)).collect();
        assert!(!others.is_empty() && others.len() <= 2, "the box on the path, at most one neighbour: {others:?}");
        assert!(others.iter().all(|id| near(id, origin, origin + *dir * 4.0)), "{others:?}");
        assert_eq!(world.raycast(origin, dir, 4.0, &filter).map(|h| h.point.x), Some(9.5), "it hits the box");

        // A capsule swept 3 m along z past the box at (20, 20): the sea and the boxes by its path.
        let (shape, from) = (Shape::capsule(0.3, 0.9), Vec3::new(20.0, 1.0, 16.5));
        let geom = Geom::new(&shape);
        let pose = Pose::from_parts(from, Quat::IDENTITY);
        let (lo, hi) = geom.aabb(&pose);
        let (lo, hi) = (lo, hi + Vec3::Z * 3.0);
        let swept = read(&|out| world.grid.near_box(lo, hi, budget, out));
        assert!(sea.iter().all(|s| swept.contains(s)), "{swept:?}");
        let others: Vec<&ColliderId> = swept.iter().filter(|id| !sea.contains(id)).collect();
        assert!(!others.is_empty() && others.len() <= 2, "{others:?}");
        assert!(others.iter().all(|id| near(id, lo, hi)), "{others:?}");
        let hit = world.shapecast(&shape, from, Quat::IDENTITY, Dir3::Z, 3.0, &filter).expect("the sweep meets the box");
        assert!((hit.distance - 2.7).abs() < 0.01, "{}", hit.distance);
        // Of all that, a ray and a sweep test no volume: water is never what they hit.
        let volumes_tested = world.near_box(lo, hi).filter(|c| c.passes(&filter)).filter(|c| c.volume.is_some()).count();
        assert_eq!(volumes_tested, 0, "the sea boxes are skipped by the default filter");
        assert!(world.volumes_at(Vec3::new(500.0, -1.0, 0.0), None).iter().any(|(_, k)| *k == VolumeKind::Water), "but a point in the sea is in water");
    }

    #[test]
    fn huge_long_and_far_colliders_and_cell_edges() {
        let mut world = PhysicsWorld::default();
        let add = |n: u32, shape: Shape, at: Vec3, rotation: Quat, world: &mut PhysicsWorld| {
            let spec = Spec {
                entity: Entity::from_raw_u32(n).unwrap(),
                id: ColliderId(n),
                collider: Collider::new(shape),
                tf: Transform::from_translation(at).with_rotation(rotation),
                body: Body::Static,
                volume: None,
            };
            spec.put(world);
        };
        // Wider than the widest level: on the list every query reads.
        add(1, Shape::cuboid(Vec3::new(3000.0, 0.5, 3000.0)), Vec3::new(0.0, -0.5, 0.0), Quat::IDENTITY, &mut world);
        // A wall 1 km long and 1 m thick, and the same wall turned 45°.
        add(2, Shape::cuboid(Vec3::new(500.0, 2.0, 0.5)), Vec3::new(0.0, 2.0, 40.0), Quat::IDENTITY, &mut world);
        add(3, Shape::cuboid(Vec3::new(500.0, 2.0, 0.5)), Vec3::new(0.0, 2.0, -40.0), Quat::from_rotation_y(0.785), &mut world);
        // Boxes exactly on cell edges of every level, and as wide as a cell.
        let mut n = 4;
        for edge in LEVELS {
            for (x, z) in [(0.0, 0.0), (edge, 0.0), (-edge, edge), (edge * 0.5, -edge)] {
                add(n, Shape::cuboid(Vec3::new(edge * 0.5, 1.0, edge * 0.5)), Vec3::new(x, 1.0, z), Quat::IDENTITY, &mut world);
                n += 1;
            }
        }
        // Far from the origin, where a meter is a few ulps.
        for far in [1.0e5, 3.0e6, -2.0e7] {
            add(n, Shape::cuboid(Vec3::splat(2.0)), Vec3::new(far, 0.0, far * 0.5), Quat::IDENTITY, &mut world);
            add(n + 1, Shape::sphere(0.5), Vec3::new(far + 3.0, 0.0, far * 0.5), Quat::IDENTITY, &mut world);
            n += 2;
        }
        // Enough small ones that the grid, not a scan, answers.
        let mut rng = Rng(0x5eed_0003);
        for _ in 0..60 {
            let (shape, at, rotation) = (rng.sized(0.2, 3.0), rng.vec(Vec3::new(60.0, 3.0, 60.0)), rng.rotation());
            add(n, shape, at, rotation, &mut world);
            n += 1;
        }
        check_grid(&world);
        assert!(matches!(world.slots[world.order[0].1 as usize].cells, Cells::Wide));
        let filter = QueryFilter::default();
        // Rays along and across the long walls, down onto the slab, along cell edges and far out.
        for (origin, dir, max) in [
            (Vec3::new(-600.0, 2.0, 40.0), Vec3::X, 1200.0),
            (Vec3::new(450.0, 2.0, 100.0), Vec3::NEG_Z, 200.0),
            (Vec3::new(17.0, 50.0, -9.0), Vec3::NEG_Y, 100.0),
            (Vec3::new(0.0, 1.0, -300.0), Vec3::Z, 600.0),
            (Vec3::new(16.0, 1.0, -300.0), Vec3::Z, 600.0),
            (Vec3::new(1.0e5 - 10.0, 0.5, 5.0e4), Vec3::X, 30.0),
            (Vec3::new(3.0e6 + 10.0, 0.0, 1.5e6), Vec3::NEG_X, 30.0),
            (Vec3::new(-2.0e7, 30.0, -1.0e7), Vec3::NEG_Y, 40.0),
        ] {
            let dir = Dir3::new(dir).unwrap();
            let hit = same(&mut world, &format!("ray from {origin} along {dir:?}"), |w| w.raycast(origin, dir, max, &filter));
            assert!(hit.is_some(), "the ray from {origin} along {dir:?} hits something");
        }
        let probe = Shape::capsule(0.4, 1.8);
        for at in [Vec3::new(0.0, 1.0, 0.0), Vec3::new(128.0, 1.0, 0.0), Vec3::new(-499.0, 2.0, 40.0), Vec3::new(1.0e5, 0.0, 5.0e4)] {
            let found = same(&mut world, &format!("overlaps at {at}"), |w| w.overlaps(&probe, at, Quat::IDENTITY, &filter));
            assert!(!found.is_empty(), "something is at {at}");
            same(&mut world, &format!("cast down from {at}"), |w| w.shapecast(&probe, at + Vec3::Y * 5.0, Quat::IDENTITY, Dir3::NEG_Y, 10.0, &filter));
        }
        // A query box bigger than the budget of cells: every collider is tested, the same answer.
        let big = Shape::cuboid(Vec3::new(900.0, 5.0, 900.0));
        assert!(matches!(world.near_box(Vec3::splat(-900.0), Vec3::splat(900.0)).which, Which::All(_)));
        let all = same(&mut world, "a box over everything", |w| w.overlaps(&big, Vec3::ZERO, Quat::IDENTITY, &filter));
        assert!(all.len() > 60);
        // Not finite: every collider is tested, as before the broad phase.
        same(&mut world, "a ray of no end", |w| w.raycast(Vec3::new(0.0, 5.0, 0.0), Dir3::NEG_Y, f32::INFINITY, &filter));
        // Finite, but its cells reach past `i32` on both axes (2^32 x 2^32 of them, more than a u64
        // counts): every collider is tested.
        same(&mut world, "a ray as long as f32 goes", |w| w.raycast(Vec3::new(0.0, 5.0, 0.0), Dir3::NEG_Y, f32::MAX, &filter));
        assert!(matches!(world.near_box(Vec3::splat(-1.0e10), Vec3::splat(1.0e10)).which, Which::All(_)));
        // (Its hit point is NaN, which equals nothing: compared as text.)
        same(&mut world, "a ray from nowhere", |w| format!("{:?}", w.raycast(Vec3::new(f32::NAN, 5.0, 0.0), Dir3::NEG_Y, 10.0, &filter)));
        same(&mut world, "a margin turned inside out", |w| {
            w.closer_than(&Geom::new(&Shape::cuboid(Vec3::splat(0.3))), &Pose::from_translation(Vec3::new(0.0, -0.4, 0.0)), &filter, -0.4)
        });
        // Removing the slab and a wall: queries no longer find them, the grid forgets them.
        for gone in [1, 2] {
            world.remove(Entity::from_raw_u32(gone).unwrap());
        }
        check_grid(&world);
        let hit = same(&mut world, "down where the slab was", |w| w.raycast(Vec3::new(37.0, 50.0, 23.0), Dir3::NEG_Y, 100.0, &filter));
        assert!(hit.is_none_or(|h| h.entity != Entity::from_raw_u32(1).unwrap()));
    }

    #[test]
    fn a_subset_of_a_big_world_answers_as_a_test_of_its_colliders_would() {
        let mut rng = Rng(0x5eed_0004);
        let centers = [Vec3::ZERO];
        let specs: Vec<Spec> = (1..=500).map(|n| random_spec(&mut rng, n, &centers)).collect();
        let mut world = PhysicsWorld::default();
        specs.iter().for_each(|s| s.put(&mut world));
        let mut part = world.subset(|c| c.body == Body::Static && c.aabb_min.x < 100.0, |_| true);
        check_grid(&part);
        let kept: Vec<Spec> = specs.iter().filter(|s| part.info(s.entity).is_some()).cloned().collect();
        assert!(kept.len() > 100 && kept.len() < 500, "{}", kept.len());
        let mut tally = Tally::default();
        random_queries(&mut rng, &mut part, &kept, 600, &mut tally);
        assert!(tally.by_grid * 2 > tally.queries, "{tally:?}");
    }

    /// Times the queries a tick asks (bodies walking, probing the ground, looking at each other,
    /// swinging) with the broad phase and with a test of every collider, on a region of 8 × 8
    /// tiles of 96 m and on one of 16 m tiles. Measure in a release build:
    /// `cargo test -p shards_motion --release --lib broad_phase_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement: run it in a release build and read what it prints"]
    fn broad_phase_timing() {
        for (tile, props) in [(96.0_f32, 40), (16.0, 2)] {
            let mut rng = Rng(0x5eed_0005);
            let mut specs = Vec::new();
            let mut n = 1;
            let push = |spec: Spec, specs: &mut Vec<Spec>, n: &mut u32| {
                specs.push(spec);
                *n += 1;
            };
            let side = tile * 8.0;
            let solid = |n: u32, shape: Shape, at: Vec3, rotation: Quat| Spec {
                entity: Entity::from_raw_u32(n).unwrap(),
                id: ColliderId(n),
                collider: Collider::new(shape),
                tf: Transform::from_translation(at).with_rotation(rotation),
                body: Body::Static,
                volume: None,
            };
            for ty in 0..8 {
                for tx in 0..8 {
                    let center = Vec3::new((tx as f32 + 0.5) * tile, 0.0, (ty as f32 + 0.5) * tile);
                    push(solid(n, Shape::cuboid(Vec3::new(tile * 0.5, 0.5, tile * 0.5)), center - Vec3::Y * 0.5, Quat::IDENTITY), &mut specs, &mut n);
                    for p in 0..props {
                        let at = center + rng.vec(Vec3::new(tile * 0.45, 0.0, tile * 0.45));
                        let (shape, rotation) = match p % 8 {
                            0..4 => (rng.sized(0.8, 3.0), rng.rotation()),
                            4 | 5 => (Shape::cuboid(Vec3::new(1.0, 2.0, 1.0)), Quat::IDENTITY),
                            6 => (Shape::cuboid(Vec3::new(4.0, 1.5, 0.5)), Quat::from_rotation_y(rng.range(0.0, 3.0))),
                            _ => (Shape::cuboid(Vec3::new(3.0, 1.0, 3.0)), Quat::IDENTITY),
                        };
                        let mut spec = solid(n, shape, at + Vec3::Y * 1.0, rotation);
                        if p % 8 == 7 {
                            spec.collider.layers = CollisionLayers::VOLUME;
                            spec.volume = Some(Volume { kind: VolumeKind::Water });
                        }
                        push(spec, &mut specs, &mut n);
                    }
                }
            }
            for (at, half) in [
                (Vec3::new(side * 0.5, 0.0, -0.5), Vec3::new(side * 0.5, 50.0, 0.5)),
                (Vec3::new(side * 0.5, 0.0, side + 0.5), Vec3::new(side * 0.5, 50.0, 0.5)),
                (Vec3::new(-0.5, 0.0, side * 0.5), Vec3::new(0.5, 50.0, side * 0.5)),
                (Vec3::new(side + 0.5, 0.0, side * 0.5), Vec3::new(0.5, 50.0, side * 0.5)),
            ] {
                push(solid(n, Shape::cuboid(half), at, Quat::IDENTITY), &mut specs, &mut n);
            }
            // Fifteen monsters and a player, in two groups as in a fight.
            let mut bodies = Vec::new();
            for b in 0..16 {
                let group = if b < 8 { Vec3::new(side * 0.3, 0.0, side * 0.3) } else { Vec3::new(side * 0.7, 0.0, side * 0.6) };
                let at = group + rng.vec(Vec3::new(6.0, 0.0, 6.0)) + Vec3::Y * 0.9;
                let mut spec = solid(n, Shape::capsule(0.4, 1.8), at, Quat::IDENTITY);
                spec.body = Body::Kinematic;
                spec.collider.layers = CollisionLayers::CHARACTER;
                bodies.push(at);
                push(spec, &mut specs, &mut n);
            }
            let mut world = PhysicsWorld::default();
            specs.iter().for_each(|s| s.put(&mut world));
            let walls = QueryFilter::new(LayerMask::WORLD);
            let capsule = Geom::new(&Shape::capsule(0.4, 1.8));
            let swing = Shape::sphere(1.2);
            type Query<'a> = Box<dyn Fn(&PhysicsWorld, usize) -> usize + 'a>;
            let queries: Vec<(&str, Query)> = vec![
                ("sweep 0.1 m (walk)", Box::new(|w: &PhysicsWorld, b: usize| {
                    let dir = Vec3::new(1.0, 0.0, (b as f32 * 0.7).fract() - 0.5).normalize();
                    usize::from(w.sweep(&capsule, &Pose::from_translation(bodies[b]), dir, 0.1, &walls, StartOverlap::LetOut).is_some())
                })),
                ("sweep 0.5 m down (ground)", Box::new(|w: &PhysicsWorld, b: usize| {
                    usize::from(w.sweep(&capsule, &Pose::from_translation(bodies[b]), Vec3::NEG_Y, 0.5, &walls, StartOverlap::LetOut).is_some())
                })),
                ("closer_than (depenetrate)", Box::new(|w: &PhysicsWorld, b: usize| w.closer_than(&capsule, &Pose::from_translation(bodies[b]), &walls, 0.02).len())),
                ("volumes_touching", Box::new(|w: &PhysicsWorld, b: usize| w.volumes_touching(&capsule, &Pose::from_translation(bodies[b])).count())),
                ("ray 1.1 m down", Box::new(|w: &PhysicsWorld, b: usize| usize::from(w.raycast(bodies[b], Dir3::NEG_Y, 1.1, &walls).is_some()))),
                ("ray 60 m down (terrain probe)", Box::new(|w: &PhysicsWorld, b: usize| {
                    usize::from(w.raycast(bodies[b] + Vec3::new(0.7, 1.0, 0.0), Dir3::NEG_Y, 61.0, &walls).is_some())
                })),
                ("ray to another body (sight)", Box::new(|w: &PhysicsWorld, b: usize| {
                    let to = bodies[(b + 5) % bodies.len()] - bodies[b];
                    usize::from(w.raycast(bodies[b], Dir3::new(to).unwrap(), to.length(), &walls).is_some())
                })),
                ("overlaps (a swing)", Box::new(|w: &PhysicsWorld, b: usize| w.overlaps(&swing, bodies[b] + Vec3::X, Quat::IDENTITY, &QueryFilter::default()).len())),
            ];
            println!("\n{} colliders: 8 x 8 tiles of {tile} m, {props} props a tile, 16 bodies; µs a query (16 bodies x 400 rounds)", world.len());
            println!("{:<30} {:>10} {:>10} {:>8}", "query", "scan", "grid", "faster");
            let (mut scan_total, mut grid_total) = (0.0, 0.0);
            for (name, query) in &queries {
                let mut time = |scan_all: bool| {
                    world.scan_all = scan_all;
                    let mut sum = 0;
                    let started = std::time::Instant::now();
                    for _ in 0..400 {
                        for b in 0..bodies.len() {
                            sum += query(&world, b);
                        }
                    }
                    (started.elapsed().as_secs_f64() * 1e6 / (400.0 * bodies.len() as f64), sum)
                };
                let (scan, found_scan) = time(true);
                let (grid, found_grid) = time(false);
                assert_eq!(found_scan, found_grid, "{name}");
                scan_total += scan;
                grid_total += grid;
                println!("{name:<30} {scan:>10.3} {grid:>10.3} {:>7.1}x", scan / grid);
            }
            println!("{:<30} {scan_total:>10.3} {grid_total:>10.3} {:>7.1}x", format!("all {}", queries.len()), scan_total / grid_total);
            // A tick's bookkeeping: every body moved and put back into the grid.
            let started = std::time::Instant::now();
            for round in 0..400 {
                for spec in specs.iter_mut().filter(|s| s.body == Body::Kinematic) {
                    spec.tf.translation.x += if round % 2 == 0 { 0.05 } else { -0.05 };
                    spec.put(&mut world);
                }
            }
            println!("moving 16 bodies: {:.3} µs a tick", started.elapsed().as_secs_f64() * 1e6 / 400.0);
        }
    }

    // --- voxel fields against a world of one collider per cell ----------------------

    /// Columns of random heights over a floor, cells flipped here and there (floating slabs,
    /// holes, overhangs): seams of every kind.
    fn random_field(rng: &mut Rng, size: UVec3, origin: Vec3, cell: f32) -> VoxelField {
        let tops: Vec<i32> = (0..size.x * size.z).map(|_| 1 + rng.below(size.y as usize) as i32).collect();
        let flips: Vec<bool> = (0..size.x * size.y * size.z).map(|_| rng.chance(0.12)).collect();
        VoxelField::new(origin, cell, size, |c| {
            let column = (c.x as u32 * size.z + c.z as u32) as usize;
            let at = ((c.x as u32 * size.z + c.z as u32) * size.y + c.y as u32) as usize;
            (c.y < tops[column]) != flips[at]
        })
    }

    /// The field as its own world (entity 1, id 1).
    fn field_world(field: VoxelField) -> PhysicsWorld {
        let mut voxels = PhysicsWorld::default();
        voxels.upsert_field(Entity::from_raw_u32(1).unwrap(), ColliderId(1), &Arc::new(field));
        voxels
    }

    /// One cuboid collider per solid cell of `field`, ids and entities `1..` in `(x, y, z)` order.
    fn cells_world(field: &VoxelField) -> PhysicsWorld {
        let mut cells = PhysicsWorld::default();
        let mut n = 0;
        let collider = Collider::new(Shape::cuboid(Vec3::splat(field.cell() * 0.5)));
        for x in 0..field.size().x as i32 {
            for y in 0..field.size().y as i32 {
                for z in 0..field.size().z as i32 {
                    let c = IVec3::new(x, y, z);
                    if field.solid(c) {
                        n += 1;
                        let tf = Transform::from_translation(field.center(c));
                        cells.upsert((Entity::from_raw_u32(n).unwrap(), &ColliderId(n), &collider, &tf, None, None));
                    }
                }
            }
        }
        cells
    }

    fn character(rng: &mut Rng) -> Shape {
        match rng.below(4) {
            0 => Shape::sphere(rng.range(0.1, 0.9)),
            1 => Shape::cuboid(Vec3::new(rng.range(0.1, 0.8), rng.range(0.1, 0.8), rng.range(0.1, 0.8))),
            _ => Shape::capsule(rng.range(0.2, 0.5), rng.range(1.0, 2.0)),
        }
    }

    #[test]
    fn a_field_answers_as_a_collider_per_cell_does() {
        // FIELD_SEED and FIELD_ROUNDS run it longer on other seeds (a stress run, by hand).
        let mut rng = Rng(std::env::var("FIELD_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0xf1e1d));
        let (mut rays, mut casts, mut merged_off) = (0, 0, 0);
        // Casts parry missed: (merged boxes, one box per cell).
        let mut missed = (0, 0);
        let mut grazes = 0;
        // Box-shaped casts: the merged field against the cells, compared to a millimetre.
        let (mut boxed, mut boxed_off) = (0, 0);
        // Rays along a seam the field meets and the cuboids let through.
        let mut seams = 0;
        for round in 0..std::env::var("FIELD_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(24) {
            let cell = [1.0, 0.5, 2.0, 0.3][round % 4];
            let size = UVec3::new(2 + rng.below(6) as u32, 1 + rng.below(5) as u32, 2 + rng.below(6) as u32);
            let origin = Vec3::new(rng.range(-3.0, 3.0).round(), rng.range(-2.0, 2.0).round(), rng.range(-3.0, 3.0).round());
            let field = random_field(&mut rng, size, origin, cell);
            let (lo, hi) = field.bounds();
            // The field as it is (its casts merged), cast cell by cell, and a cuboid per cell.
            let (voxels, unmerged, cells) = (field_world(field.clone()), field_world(field.clone().with_cast_merge(false)), cells_world(&field));
            let any = QueryFilter::default();
            let same_ray = |a: Option<RayHit>, b: Option<RayHit>| a.map(|h| (h.distance, h.point, h.normal)) == b.map(|h| (h.distance, h.point, h.normal));
            for _ in 0..250 {
                // Places on the cell lattice (seams, edges, corners) half the time.
                let mut pick = |a: usize| {
                    let p = rng.range(lo[a] - 1.5, hi[a] + 1.5);
                    if rng.chance(0.5) { origin[a] + ((p - origin[a]) / cell).round() * cell } else { p }
                };
                let from = Vec3::new(pick(0), pick(1), pick(2));
                let dir = match rng.below(3) {
                    0 => [Dir3::X, Dir3::NEG_X, Dir3::Y, Dir3::NEG_Y, Dir3::Z, Dir3::NEG_Z][rng.below(6)],
                    1 => Dir3::new(Vec3::new(rng.below(3) as f32 - 1.0, rng.below(3) as f32 - 1.0, rng.below(3) as f32 - 1.0)).unwrap_or(Dir3::NEG_Y),
                    _ => Dir3::new(Vec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0))).unwrap_or(Dir3::X),
                };
                let max = rng.range(0.0, 8.0);
                let (a, b) = (voxels.raycast(from, dir, max, &any), cells.raycast(from, dir, max, &any));
                // A ray in the plane of a seam: cuboids side by side may let it through where the
                // field, solid on both sides, meets it (`VoxelField::along_seam`).
                let in_seam = (0..3).any(|ax| dir[ax] == 0.0 && origin[ax] + ((from[ax] - origin[ax]) / cell).round() * cell == from[ax]);
                if in_seam && a.is_some_and(|a| b.is_none_or(|b| a.distance < b.distance)) {
                    seams += 1;
                } else {
                    assert!(same_ray(a, b), "ray from {from} along {dir:?} within {max}: {a:?} against {b:?}");
                }
                rays += usize::from(b.is_some());

                let shape = character(&mut rng);
                let rot = if rng.chance(0.7) { Quat::IDENTITY } else { Quat::from_rotation_y(rng.range(0.0, 3.0)) };
                let (geom, pose) = (Geom::new(&shape), Pose::from_parts(from, rot));
                for start in [StartOverlap::Hit, StartOverlap::LetOut] {
                    let want = cells.sweep(&geom, &pose, *dir, max, &any, start);
                    // One box per cell: the same tests in the same order, to the bit.
                    let got = unmerged.sweep(&geom, &pose, *dir, max, &any, start);
                    let key = |h: Option<ShapeHit>| h.map(|h| (h.distance, h.point, h.normal));
                    assert_eq!(key(got), key(want), "{shape:?} from {from} along {dir:?} within {max} ({start:?}), merge off");
                    let got = voxels.sweep(&geom, &pose, *dir, max, &any, start);
                    // Column runs: the same first touch, up to rounding. For the rounded shapes the
                    // engine casts (bodies, probes, the camera's ball). Two sharp shapes (a box cast
                    // at a box) get GJK's answer unrefined, good to a millimetre, which a bigger box
                    // moves: no character casts a box, so those are counted, not held.
                    if matches!(shape, Shape::Cuboid { .. }) {
                        boxed += 1;
                        boxed_off += usize::from(got.is_some() != want.is_some() || got.zip(want).is_some_and(|(g, w)| (g.distance - w.distance).abs() > 2e-3));
                        continue;
                    }
                    let boxes = || {
                        let slot = &voxels.fields[0];
                        let (lo, hi) = geom.aabb(&pose);
                        let (s0, s1) = (lo.min(lo + *dir * max), hi.max(hi + *dir * max));
                        let Some((a, b)) = slot.field.cells_in(s0, s1) else { return String::new() };
                        let inside = slot.field.cells_in(lo, hi);
                        let solid: Vec<IVec3> = slot.solid_in(s0, s1).collect();
                        let merged = slot.merged(a, b, |c| inside.is_some_and(|(p, q)| c.cmpge(p).all() && c.cmple(q).all()));
                        format!("\n{:?}\ncells {a}..={b}, start {inside:?}\nsolid {solid:?}\nboxes {merged:?}", slot.field)
                    };
                    // Off the surface by less than 0.1 mm. A grazing hit (the cast nearly along the
                    // surface) keeps GJK's distance, good to millimetres along the cast but not
                    // across it: what a slide along the surface sees.
                    let near = |g: ShapeHit, w: ShapeHit| {
                        let off = (g.distance - w.distance).abs();
                        off < 1e-4 || off * dir.dot(w.normal).abs() < 1e-4
                    };
                    // A touch along the surface (closing slower than a tenth of the motion): whether and
                    // where parry finds it is its tolerance's call, in either world.
                    let grazing = |h: Option<ShapeHit>| h.is_none_or(|h| dir.dot(h.normal).abs() < 0.1);
                    // parry's cast misses some touches at a box's edge or corner (both worlds have that):
                    // a world whose answer is farther than the other's while the shape, placed at the
                    // nearer one, already sinks into it (cell by cell, exactly), missed one.
                    let sinks = |d: f32| {
                        let at = Pose::from_parts(from + *dir * (d + 1e-3), rot);
                        cells.closer_than(&geom, &at, &any, 0.0).iter().any(|p| p.depth > 1e-5)
                    };
                    match (got, want) {
                        (None, None) => {}
                        (Some(g), Some(w)) if near(g, w) => {
                            merged_off += usize::from(g.distance != w.distance);
                            casts += 1;
                        }
                        (g, w) if grazing(g) && grazing(w) => grazes += 1,
                        (Some(g), w) if w.is_none_or(|w| g.distance < w.distance) && sinks(g.distance) => missed.1 += 1,
                        (g, Some(w)) if g.is_none_or(|g| w.distance < g.distance) && sinks(w.distance) => missed.0 += 1,
                        _ => panic!("{shape:?} from {from} along {dir:?} within {max} ({start:?}): {got:?} against {want:?}{}", boxes()),
                    }
                }
                let (overlaps, touched) = (voxels.overlaps(&shape, from, rot, &any), cells.overlaps(&shape, from, rot, &any));
                assert_eq!(overlaps.is_empty(), touched.is_empty(), "{shape:?} at {from}");
                let key = |p: Vec<Penetration>| p.into_iter().map(|p| (p.normal, p.depth)).collect::<Vec<_>>();
                let margin = rng.range(0.0, 0.1);
                let (deep, deep_cells) = (voxels.closer_than(&geom, &pose, &any, margin), cells.closer_than(&geom, &pose, &any, margin));
                assert_eq!(key(deep), key(deep_cells), "{shape:?} at {from}, margin {margin}");
            }
        }
        assert!(rays > 1000 && casts > 1000, "too few hits to mean anything: {rays} rays, {casts} casts");
        // The merged boxes never miss a touch the cells find: a box parry misses but may meet is
        // cast cell by cell (`a_merged_box_parry_misses_is_cast_cell_by_cell`). A miss here is a
        // body sinking into a cell.
        assert_eq!(missed.0, 0, "the merged boxes missed {} touches the cells found ({casts} casts)", missed.0);
        assert!(missed.1 + grazes <= casts / 500, "the cells missed {} touches and {grazes} grazes differ, of {casts} casts", missed.1);
        println!(
            "{rays} rays and {casts} casts hit ({seams} more rays along seams); the merge moved {merged_off} first touches by rounding; parry missed {missed:?} (merged, per cell); {grazes} grazes differ; {boxed_off} of {boxed} box casts differ by more than 2 mm"
        );
    }

    #[test]
    fn a_merged_box_parry_misses_is_cast_cell_by_cell() {
        // Met in the seam test's walk over hills: the controller's ground probe 13.5 mm above a
        // run of two cells, its core 4 mm in from the run's edge. parry finds the touch on
        // either cell, and misses it on the box of the two.
        let field = VoxelField::new(Vec3::new(-12.0, -1.0, -12.0), 0.5, UVec3::new(24, 8, 48), |c| c.x == 11 && c.y == 3 && (24..26).contains(&c.z));
        let probe = Geom::Capsule(Capsule::new(Vec3::new(-0.0, -0.49999997, -0.0), Vec3::new(0.0, 0.49999997, 0.0), 0.41));
        let pose = Pose::from_translation(Vec3::new(-5.9958024, 1.9235163, 0.44552058));
        let (world, unmerged) = (field_world(field.clone()), field_world(field.with_cast_merge(false)));
        let slot = &world.fields[0];
        let run = slot.cells(IVec3::new(11, 3, 24), IVec3::new(12, 4, 26));
        assert_eq!((run.position, run.shape), (Vec3::new(-6.25, 0.75, 0.5), Shape::cuboid(Vec3::new(0.25, 0.25, 0.5))));
        // Whether parry still misses the box is parry's to say (parry3d 0.31.1 does): printed, not
        // held. What is held is the field's answer.
        let missed = sweep_one(&probe, &pose, Vec3::NEG_Y, 0.36, 0.36, StartOverlap::LetOut, &run);
        println!("parry's cast at the merged box alone: {missed:?} (None: the case the rescue is for)");
        let any = QueryFilter::default();
        let merged = world.sweep(&probe, &pose, Vec3::NEG_Y, 0.36, &any, StartOverlap::LetOut).expect("the probe meets the run");
        let cells = unmerged.sweep(&probe, &pose, Vec3::NEG_Y, 0.36, &any, StartOverlap::LetOut).unwrap();
        assert_eq!(merged, cells, "cast cell by cell, as with the merge off");
        assert!((merged.distance - 0.0135).abs() < 1e-4 && merged.point.y == 1.0, "{merged:?}");
    }

    #[test]
    fn colliders_and_fields_take_turns_by_id() {
        // A floor collider (id 2) and a field floor (id 1) at the same height: the collider is
        // asked first, and a field wins only when strictly nearer.
        let mut world = PhysicsWorld::default();
        let (slab, ground) = (Entity::from_raw_u32(10).unwrap(), Entity::from_raw_u32(11).unwrap());
        let collider = Collider::new(Shape::cuboid(Vec3::new(5.0, 0.5, 5.0)));
        world.upsert((slab, &ColliderId(2), &collider, &Transform::from_xyz(0.0, -0.5, 0.0), None, None));
        let field = VoxelField::new(Vec3::new(-5.0, -1.0, -5.0), 1.0, UVec3::new(10, 1, 10), |_| true);
        world.upsert_field(ground, ColliderId(1), &Arc::new(field));
        let any = QueryFilter::default();
        let down = world.raycast(Vec3::new(0.3, 3.0, 0.3), Dir3::NEG_Y, 10.0, &any).unwrap();
        assert_eq!((down.entity, down.distance), (slab, 3.0), "a tie goes to what is asked first");
        assert_eq!(world.overlaps(&Shape::sphere(0.2), Vec3::new(0.3, 0.0, 0.3), Quat::IDENTITY, &any), [ground, slab], "in id order");
        let deep: Vec<Entity> = world.penetrations(&Shape::sphere(0.2), Vec3::new(0.3, 0.0, 0.3), Quat::IDENTITY, &any).iter().map(|p| p.entity).collect();
        assert_eq!(deep, [ground, slab]);
        // Excluded or off the mask: the field is not asked.
        let hit = world.raycast(Vec3::new(0.3, 3.0, 0.3), Dir3::NEG_Y, 10.0, &any.clone().excluding(slab)).unwrap();
        assert_eq!(hit.entity, ground);
        assert!(world.raycast(Vec3::new(0.3, 3.0, 0.3), Dir3::NEG_Y, 10.0, &QueryFilter::new(LayerMask::CHARACTER)).is_none());
        world.remove_field(ground);
        assert_eq!(world.fields().count(), 0);
    }

    #[test]
    fn hooks_read_a_field_by_its_runs_and_a_collider_by_its_box() {
        // A floor 0..1, a tunnel 1..3 (2 m high) under a plateau 3..5, at x = 2..4; a wall at x = 6.
        let field = VoxelField::new(Vec3::ZERO, 1.0, UVec3::new(8, 6, 4), |c| c.y < 1 || ((2..4).contains(&c.x) && (3..5).contains(&c.y)) || (c.x == 6 && c.y < 4));
        let mut world = PhysicsWorld::default();
        let (ground, post) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        world.upsert_field(ground, ColliderId(1), &Arc::new(field));
        let collider = Collider::new(Shape::cuboid(Vec3::new(0.25, 1.5, 0.25)));
        world.upsert((post, &ColliderId(2), &collider, &Transform::from_xyz(-3.0, 1.5, 0.0), None, None));
        // In the tunnel: its floor's run, not the column; above: the plateau's.
        assert_eq!(world.extent_at(ground, Vec3::new(2.5, 1.0, 1.5)), Some((0.0, 1.0)), "on the floor (just below the point)");
        assert_eq!(world.extent_at(ground, Vec3::new(2.5, 0.5, 1.5)), Some((0.0, 1.0)));
        assert_eq!(world.extent_at(ground, Vec3::new(2.5, 3.0, 1.5)), Some((3.0, 5.0)), "on the plateau's underside (just above)");
        assert_eq!(world.extent_at(ground, Vec3::new(2.5, 2.0, 1.5)), None, "air in the tunnel");
        assert_eq!(world.extent_at(post, Vec3::new(100.0, 0.0, 0.0)), Some((0.0, 3.0)), "a collider: its box, wherever");
        assert_eq!(world.extent_at(Entity::from_raw_u32(9).unwrap(), Vec3::ZERO), None);
        // A body standing in the tunnel meets the floor and nothing above its head.
        let body = Shape::capsule(0.4, 1.8);
        assert_eq!(world.top_under(ground, &body, Vec3::new(3.0, 1.85, 1.5), Quat::IDENTITY), Some(1.0), "the plateau above is not in the way");
        assert_eq!(world.top_under(ground, &body, Vec3::new(3.0, 1.95, 1.5), Quat::IDENTITY), None, "touching nothing");
        assert_eq!(world.top_under(ground, &body, Vec3::new(3.0, 2.5, 1.5), Quat::IDENTITY), Some(5.0), "head in the plateau");
        assert_eq!(world.top_under(ground, &body, Vec3::new(5.65, 1.85, 1.5), Quat::IDENTITY), Some(4.0), "against the wall");
        assert_eq!(world.top_under(ground, &body, Vec3::new(3.0, 30.0, 1.5), Quat::IDENTITY), None);
        assert_eq!(world.top_under(post, &body, Vec3::new(30.0, 0.0, 0.0), Quat::IDENTITY), Some(3.0), "a collider: its box top");
        // Every solid of a mask: the post (0..3) and the field's cells (0..5).
        assert_eq!(world.solid_span(LayerMask::WORLD), Some((0.0, 5.0)));
        assert_eq!(world.solid_span(LayerMask::CHARACTER), None);
        assert!(world.has_solid(LayerMask::WORLD) && !world.has_solid(LayerMask::CHARACTER));
        world.remove_field(ground);
        assert_eq!(world.solid_span(LayerMask::WORLD), Some((0.0, 3.0)));
    }

    #[test]
    fn a_subset_keeps_the_fields_it_is_told_to() {
        let mut world = PhysicsWorld::default();
        let field = Arc::new(VoxelField::new(Vec3::ZERO, 1.0, UVec3::splat(2), |c| c.y == 0));
        world.upsert_field(Entity::from_raw_u32(1).unwrap(), ColliderId(1), &field);
        let any = QueryFilter::default();
        let down = |w: &PhysicsWorld| w.raycast(Vec3::new(0.5, 5.0, 0.5), Dir3::NEG_Y, 10.0, &any).map(|h| h.point.y);
        assert_eq!(down(&world.subset(|_| true, |_| true)), Some(1.0));
        assert_eq!(down(&world.subset(|_| true, |_| false)), None);
        assert_eq!(world.subset(|_| true, |f| f.solid_cells() > 0).fields().count(), 1);
        // A world of one field has no collider and has a solid; a field of no solid cell is none.
        assert!(world.is_empty() && world.has_solid(LayerMask::WORLD));
        assert!(!field_world(VoxelField::new(Vec3::ZERO, 1.0, UVec3::splat(2), |_| false)).has_solid(LayerMask::WORLD));
    }

    /// The queries of [`broad_phase_timing`] on rolling hills 64 m across of 0.5 m cells: as a
    /// field with the merge on and off, and as one box per column (the merged boxes) in the
    /// collider grid. Each world is timed seven times, in turn (field, merge off, boxes, field, ...),
    /// 100 rounds of the 16 bodies a time; the least and the median time are printed, so a busy
    /// machine shows in the spread. `cargo test -p shards_motion --release --lib field_timing --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement: run it in a release build and read what it prints"]
    fn field_timing() {
        let (cell, size) = (0.5, UVec3::new(128, 32, 128));
        let height = |x: i32, z: i32| 12 + (ops::sin(x as f32 * 0.11) * ops::cos(z as f32 * 0.07) * 6.0 + ops::sin((x + z) as f32 * 0.31) * 1.5) as i32;
        let field = VoxelField::new(Vec3::ZERO, cell, size, |c| c.y < height(c.x, c.z));
        let (voxels, unmerged) = (field_world(field.clone()), field_world(field.with_cast_merge(false)));
        let mut boxes = PhysicsWorld::default();
        let mut n = 1;
        for x in 0..size.x as i32 {
            for z in 0..size.z as i32 {
                let top = height(x, z) as f32 * cell;
                let collider = Collider::new(Shape::cuboid(Vec3::new(cell * 0.5, top * 0.5, cell * 0.5)));
                let tf = Transform::from_xyz((x as f32 + 0.5) * cell, top * 0.5, (z as f32 + 0.5) * cell);
                boxes.upsert((Entity::from_raw_u32(n).unwrap(), &ColliderId(n), &collider, &tf, None, None));
                n += 1;
            }
        }
        let mut rng = Rng(0x5eed_0006);
        let bodies: Vec<Vec3> = (0..16)
            .map(|_| {
                let (x, z) = (rng.below(120) as i32 + 4, rng.below(120) as i32 + 4);
                Vec3::new((x as f32 + 0.5) * cell, height(x, z) as f32 * cell + 0.905, (z as f32 + 0.5) * cell)
            })
            .collect();
        let walls = QueryFilter::new(LayerMask::WORLD);
        let capsule = Geom::new(&Shape::capsule(0.4, 1.8));
        let swing = Shape::sphere(1.2);
        type Query<'a> = Box<dyn Fn(&PhysicsWorld, usize) -> usize + 'a>;
        let queries: Vec<(&str, Query)> = vec![
            ("sweep 0.1 m (walk)", Box::new(|w: &PhysicsWorld, b: usize| {
                let dir = Vec3::new(1.0, 0.0, (b as f32 * 0.7).fract() - 0.5).normalize();
                usize::from(w.sweep(&capsule, &Pose::from_translation(bodies[b]), dir, 0.1, &walls, StartOverlap::LetOut).is_some())
            })),
            ("sweep 0.5 m down (ground)", Box::new(|w: &PhysicsWorld, b: usize| {
                usize::from(w.sweep(&capsule, &Pose::from_translation(bodies[b]), Vec3::NEG_Y, 0.5, &walls, StartOverlap::LetOut).is_some())
            })),
            ("closer_than (depenetrate)", Box::new(|w: &PhysicsWorld, b: usize| w.closer_than(&capsule, &Pose::from_translation(bodies[b]), &walls, 0.02).len())),
            ("ray 1.1 m down", Box::new(|w: &PhysicsWorld, b: usize| usize::from(w.raycast(bodies[b], Dir3::NEG_Y, 1.1, &walls).is_some()))),
            ("ray 60 m down (terrain probe)", Box::new(|w: &PhysicsWorld, b: usize| {
                usize::from(w.raycast(bodies[b] + Vec3::new(0.7, 1.0, 0.0), Dir3::NEG_Y, 61.0, &walls).is_some())
            })),
            ("ray to another body (sight)", Box::new(|w: &PhysicsWorld, b: usize| {
                let to = bodies[(b + 5) % bodies.len()] - bodies[b];
                usize::from(w.raycast(bodies[b], Dir3::new(to).unwrap(), to.length(), &walls).is_some())
            })),
            ("overlaps (a swing)", Box::new(|w: &PhysicsWorld, b: usize| w.overlaps(&swing, bodies[b] + Vec3::X, Quat::IDENTITY, &QueryFilter::default()).len())),
        ];
        println!("
rolling hills of 128 x 128 columns of 0.5 m cells, 16 bodies; µs a query, least / median of 7 samples of 100 rounds");
        println!("{:<30} {:>16} {:>16} {:>16}", "query", "field", "merge off", "boxes");
        for (name, query) in &queries {
            let time = |world: &PhysicsWorld| {
                let mut sum = 0;
                let started = std::time::Instant::now();
                for _ in 0..100 {
                    for b in 0..bodies.len() {
                        sum += query(world, b);
                    }
                }
                std::hint::black_box(sum);
                started.elapsed().as_secs_f64() * 1e6 / (100.0 * bodies.len() as f64)
            };
            let mut samples: [Vec<f64>; 3] = Default::default();
            for _ in 0..7 {
                for (s, world) in samples.iter_mut().zip([&voxels, &unmerged, &boxes]) {
                    s.push(time(world));
                }
            }
            let [merged, cells, boxed] = samples.map(|mut s| {
                s.sort_by(f64::total_cmp);
                format!("{:.3} / {:.3}", s[0], s[3])
            });
            println!("{name:<30} {merged:>16} {cells:>16} {boxed:>16}");
        }
    }
}
