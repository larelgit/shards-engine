//! Physics layer: colliders, collision layers, sensor volumes and spatial queries.
//!
//! Kinematic only: nothing here simulates forces. Gameplay code (the character
//! controller, hit detection, AI queries) asks [`PhysicsWorld`] questions — what does
//! this ray hit, where does this capsule stop, what overlaps this box — and moves
//! things itself.
//!
//! Determinism: every collider gets a [`ColliderId`] in insertion order, never reused.
//! All iteration and every result list follow it, never `Entity` (a window adds render
//! entities that shift `Entity` indices), and a tie between equal distances goes to the
//! lower id.
//!
//! Colliders are read from `Transform`, not `GlobalTransform`: transforms are not
//! propagated inside fixed ticks, so a child's `GlobalTransform` lags behind. Put
//! colliders on top-level entities. `Transform::scale` is ignored: size goes in the
//! [`Shape`].
//!
//! The broad phase is a grid over the ground plane in levels of growing cells (`broad.rs`):
//! a query tests the colliders in the cells it covers, in id order, and gets the answer a
//! test of every collider would give.
//!
//! Besides colliders, a world may hold voxel fields ([`VoxelCollider`], `field.rs`): static
//! grids of solid cells that every query asks after the colliders, one entity for a whole
//! terrain. A field has no bounding box: code that reads the top or the bottom of what a query
//! hit asks [`PhysicsWorld::extent_at`], [`PhysicsWorld::top_under`] or
//! [`PhysicsWorld::solid_span`], never `collider(entity)` and its box (`docs/manual.md`,
//! "Movement", has the rule for new readers). A world without a field runs the colliders'
//! code alone.

mod broad;
pub mod current;
pub mod field;
pub(crate) mod world;

use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign, Not};

use ai_core::{AiAppExt, AiId, EntityKey, SimSet};
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_core::{predicts, NetSet};

pub use current::{current_at, edge_distance, Current};
pub use field::{FieldRay, VoxelField};
pub use world::{ColliderInfo, Penetration, PhysicsWorld, RayHit, ShapeHit};

/// A convex collision shape in the entity's local frame (Y up).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    /// A box with these half extents.
    Cuboid { half: Vec3 },
    Sphere { radius: f32 },
    /// A segment along local Y of length `2 * half_height`, inflated by `radius`:
    /// `2 * (half_height + radius)` tall in all.
    Capsule { half_height: f32, radius: f32 },
    /// Along local Y, `2 * half_height` tall.
    Cylinder { half_height: f32, radius: f32 },
    /// Along local Y: base at `-half_height`, apex at `+half_height`.
    Cone { half_height: f32, radius: f32 },
}

impl Shape {
    pub fn cuboid(half: Vec3) -> Self {
        Shape::Cuboid { half }
    }

    pub fn sphere(radius: f32) -> Self {
        Shape::Sphere { radius }
    }

    /// A capsule `height` tall in all (caps included); the straight part shrinks to
    /// nothing when `height < 2 * radius`.
    pub fn capsule(radius: f32, height: f32) -> Self {
        Shape::Capsule { half_height: (height * 0.5 - radius).max(0.0), radius }
    }

    /// Half extents of the local-frame bounding box.
    pub fn half_extents(&self) -> Vec3 {
        match *self {
            Shape::Cuboid { half } => half,
            Shape::Sphere { radius } => Vec3::splat(radius),
            Shape::Capsule { half_height, radius } => Vec3::new(radius, half_height + radius, radius),
            Shape::Cylinder { half_height, radius } | Shape::Cone { half_height, radius } => {
                Vec3::new(radius, half_height, radius)
            }
        }
    }
}

/// `box 2×4×2 m`, `capsule r0.4 h1.8 m`: total sizes, for observations and errors.
impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = Meters;
        match *self {
            Shape::Cuboid { half } => {
                write!(f, "box {}×{}×{} m", m.fmt(half.x * 2.0), m.fmt(half.y * 2.0), m.fmt(half.z * 2.0))
            }
            Shape::Sphere { radius } => write!(f, "sphere r{} m", m.fmt(radius)),
            Shape::Capsule { half_height, radius } => {
                write!(f, "capsule r{} h{} m", m.fmt(radius), m.fmt(2.0 * (half_height + radius)))
            }
            Shape::Cylinder { half_height, radius } => {
                write!(f, "cylinder r{} h{} m", m.fmt(radius), m.fmt(2.0 * half_height))
            }
            Shape::Cone { half_height, radius } => write!(f, "cone r{} h{} m", m.fmt(radius), m.fmt(2.0 * half_height)),
        }
    }
}

/// Lengths without trailing zeros: `2`, `1.8`, `0.25`.
struct Meters;

impl Meters {
    fn fmt(&self, v: f32) -> String {
        let s = format!("{v:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".to_owned() } else { s.to_owned() }
    }
}

/// A set of collision layers (bit flags). Bits 0..16 belong to the engine, 16..32 to
/// the game ([`LayerMask::game`]).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[serde(transparent)]
pub struct LayerMask(pub u32);

impl LayerMask {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self(u32::MAX);
    /// Static level geometry: floors, walls, props.
    pub const WORLD: Self = Self(1 << 0);
    /// Character bodies.
    pub const CHARACTER: Self = Self(1 << 1);
    /// Where a character can be hit.
    pub const HURTBOX: Self = Self(1 << 2);
    /// What hits: weapon swings, explosions.
    pub const HITBOX: Self = Self(1 << 3);
    /// Sensors: water, updrafts, triggers, ladders.
    pub const VOLUME: Self = Self(1 << 4);
    pub const PROJECTILE: Self = Self(1 << 5);
    /// What a character can use by looking at it (F5): the use areas of interactable things. A query
    /// layer only: a collider on it collides with nothing ([`CollisionLayers::USE`]), and the engine's
    /// own queries name the layers they want, so only the ray of [`use_target`](crate::use_target) asks for
    /// it. A query that takes every layer (`QueryFilter::default()`, `LayerMask::ALL`) meets it like any
    /// other: a ray against solid geometry names `WORLD`.
    pub const USE: Self = Self(1 << 6);

    /// Game layer `n` (0..16), bit `16 + n`.
    pub const fn game(n: u32) -> Self {
        assert!(n < 16, "games have layers 0..16");
        Self(1 << (16 + n))
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl BitOr for LayerMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for LayerMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for LayerMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Not for LayerMask {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// What a collider is (`memberships`) and what it collides with (`filters`).
/// Queries match a [`QueryFilter::mask`] against `memberships`.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CollisionLayers {
    pub memberships: LayerMask,
    pub filters: LayerMask,
}

impl CollisionLayers {
    /// Level geometry: blocks everything.
    pub const WORLD: Self = Self::new(LayerMask::WORLD, LayerMask::ALL);
    /// A character body: walks on and against the world.
    pub const CHARACTER: Self = Self::new(LayerMask::CHARACTER, LayerMask::WORLD);
    /// A sensor volume that notices characters.
    pub const VOLUME: Self = Self::new(LayerMask::VOLUME, LayerMask::CHARACTER);
    /// The use area of something a character looks at to use it (F5): on the `USE` layer, and it has no
    /// outgoing filter, so it collides with nothing. Queries whose mask includes `USE` (`QueryFilter::default()` takes
    /// every layer) can meet it; solid-geometry queries name `WORLD`.
    pub const USE: Self = Self::new(LayerMask::USE, LayerMask::NONE);

    pub const fn new(memberships: LayerMask, filters: LayerMask) -> Self {
        Self { memberships, filters }
    }

    /// Both sides accept each other.
    pub const fn interacts_with(&self, other: &Self) -> bool {
        self.filters.intersects(other.memberships) && other.filters.intersects(self.memberships)
    }
}

impl Default for CollisionLayers {
    fn default() -> Self {
        Self::WORLD
    }
}

/// A collision shape on this entity, `offset` from its origin in its local frame,
/// turned with its rotation. Gets a [`ColliderId`] when added.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[reflect(Component)]
#[require(Transform, Body, ColliderId)]
#[component(on_add = assign_collider_id, on_remove = forget_collider)]
pub struct Collider {
    pub shape: Shape,
    pub offset: Vec3,
    pub layers: CollisionLayers,
}

impl Collider {
    /// Level geometry centered on the entity.
    pub fn new(shape: Shape) -> Self {
        Self { shape, offset: Vec3::ZERO, layers: CollisionLayers::WORLD }
    }

    /// A box standing on the entity's origin (crates, walls, steps).
    pub fn standing_box(half: Vec3) -> Self {
        Self::new(Shape::cuboid(half)).with_offset(Vec3::Y * half.y)
    }

    pub fn with_offset(mut self, offset: Vec3) -> Self {
        self.offset = offset;
        self
    }

    pub fn with_layers(mut self, layers: CollisionLayers) -> Self {
        self.layers = layers;
        self
    }
}

/// Static solid cells that every physics query asks besides the colliders: a voxel terrain as
/// one entity. Gets a [`ColliderId`] when added, as a [`Collider`] does, and takes its place
/// among the fields in id order; removing the component or despawning the entity takes the
/// field out, and a new value replaces it. The field is in world space (its own `origin`): the
/// entity's `Transform` is not read. An entity carries a `Collider` or a `VoxelCollider`, not
/// both.
///
/// Not reflected and not saved: the entity has no `AiId` (as level geometry), the region that
/// spawned it spawns it again, and the snapshot check `static_colliders` compares its field.
#[derive(Component, Clone, Debug)]
#[require(ColliderId)]
#[component(on_add = assign_collider_id, on_remove = forget_field)]
pub struct VoxelCollider {
    pub field: std::sync::Arc<VoxelField>,
}

impl VoxelCollider {
    pub fn new(field: VoxelField) -> Self {
        Self { field: std::sync::Arc::new(field) }
    }
}

/// How a collider moves. Static colliders are cached and refreshed only when they
/// change; kinematic ones (moved by code every tick, like characters) every sync.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[reflect(Component, Default)]
pub enum Body {
    #[default]
    Static,
    Kinematic,
}

/// A sensor: queries skip it unless [`QueryFilter::volumes`] is set, and it never
/// blocks movement. Its shape is the entity's [`Collider`] (without one, it is ignored);
/// give that collider [`CollisionLayers::VOLUME`].
#[derive(Component, Reflect, Serialize, Deserialize, Clone, PartialEq, Debug)]
#[reflect(Component)]
#[component(on_remove = forget_volume)]
pub struct Volume {
    pub kind: VolumeKind,
}

#[derive(Reflect, Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum VolumeKind {
    /// Swimmable water. The surface is the top of the volume's bounding box.
    Water,
    /// Rising air: a glider inside gains upward speed at `accel` (m/s²) up to
    /// `max_speed` (m/s).
    Updraft { accel: f32, max_speed: f32 },
    /// A named zone for game logic: goal pads, region borders, cutscene starts.
    Trigger { name: String },
    /// A ladder: a character whose `locomotion` gives a `ladder_speed` climbs it (the controller's
    /// [`MotionMode::Ladder`](crate::MotionMode::Ladder)). The volume is the space the ladder takes,
    /// about as thin as the ladder and standing in front of what it hangs on, from its foot to about
    /// half a metre over the floor it leads to. `face` points out of the ladder, toward the side it is
    /// climbed from (away from the wall), in the volume entity's own frame; only its horizontal part
    /// counts once the entity's rotation has turned it.
    Ladder { face: Vec3 },
}

impl fmt::Display for VolumeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VolumeKind::Water => f.write_str("water"),
            VolumeKind::Updraft { .. } => f.write_str("updraft"),
            VolumeKind::Trigger { name } => write!(f, "trigger \"{name}\""),
            VolumeKind::Ladder { .. } => f.write_str("ladder"),
        }
    }
}

/// A character's capsule started touching `volume`.
#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeEntered {
    pub entity: Entity,
    pub volume: Entity,
}

/// A character's capsule stopped touching `volume` (or the volume is gone).
#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeExited {
    pub entity: Entity,
    pub volume: Entity,
}

/// Deterministic collider id: insertion order, never reused. Orders every query
/// result and every internal iteration. Assigned by the engine when [`Collider`] is
/// added (`0` until then, or without [`PhysicsPlugin`]). Only gameplay may spawn
/// colliders: one that only a window spawns would shift every later id.
#[derive(
    Component, Reflect, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default,
)]
#[reflect(Component, Default)]
pub struct ColliderId(pub u32);

impl fmt::Display for ColliderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// The counter behind [`ColliderId`]. Part of snapshots: ids handed out after a load
/// continue where the original session's did.
#[derive(Resource, Reflect, Debug)]
#[reflect(Resource, Default)]
pub struct ColliderIds {
    next: u32,
}

impl Default for ColliderIds {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl ColliderIds {
    /// The id the next collider gets.
    pub fn peek(&self) -> ColliderId {
        ColliderId(self.next)
    }

    /// Makes `next` the id the next collider gets: a module that builds again what it built
    /// before (a region a snapshot puts back) gives the colliders the ids they had. Ids must
    /// stay unique: only for a world where no collider has `next` or a later one, or whose
    /// counter a snapshot sets again right after.
    pub fn restart_at(&mut self, next: ColliderId) {
        self.next = next.0.max(1);
    }
}

fn assign_collider_id(mut world: DeferredWorld, ctx: HookContext) {
    let Some(mut ids) = world.get_resource_mut::<ColliderIds>() else {
        return;
    };
    let id = ColliderId(ids.next);
    ids.next += 1;
    if let Some(mut slot) = world.get_mut::<ColliderId>(ctx.entity) {
        *slot = id;
    }
}

/// Removal goes through a hook, not `RemovedComponents`: those events expire after two
/// frames, and a paused game can sit through many frames between ticks.
fn forget_collider(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut physics) = world.get_resource_mut::<PhysicsWorld>() {
        physics.remove(ctx.entity);
    }
}

fn forget_field(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut physics) = world.get_resource_mut::<PhysicsWorld>() {
        physics.remove_field(ctx.entity);
    }
}

fn forget_volume(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut physics) = world.get_resource_mut::<PhysicsWorld>() {
        physics.clear_volume(ctx.entity);
    }
}

/// Which colliders a query sees.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryFilter {
    /// Colliders whose memberships intersect this mask.
    pub mask: LayerMask,
    /// Never these entities (the caster itself, its weapon...).
    pub exclude: Vec<Entity>,
    /// Include sensor [`Volume`]s (left out by default).
    pub volumes: bool,
}

impl Default for QueryFilter {
    fn default() -> Self {
        Self { mask: LayerMask::ALL, exclude: Vec::new(), volumes: false }
    }
}

impl QueryFilter {
    pub fn new(mask: LayerMask) -> Self {
        Self { mask, ..default() }
    }

    pub fn excluding(mut self, entity: Entity) -> Self {
        self.exclude.push(entity);
        self
    }

    pub fn with_volumes(mut self) -> Self {
        self.volumes = true;
        self
    }
}

/// [`PhysicsWorld`] as a system param, for gameplay systems.
#[derive(SystemParam)]
pub struct SpatialQuery<'w> {
    physics: Res<'w, PhysicsWorld>,
}

impl std::ops::Deref for SpatialQuery<'_> {
    type Target = PhysicsWorld;
    fn deref(&self) -> &PhysicsWorld {
        &self.physics
    }
}

/// When [`PhysicsWorld`] catches up with the ECS inside a tick.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysicsSystems {
    /// Start of every tick, before `SimSet::Input`: whatever moved between ticks.
    Sync,
    /// Right after `SimSet::Move`, before `SimSet::Resolve`: hit detection sees
    /// where things moved to.
    SyncAfterMove,
}

/// Colliders, volumes and [`PhysicsWorld`] queries.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhysicsWorld>()
            .init_resource::<ColliderIds>()
            .register_type::<Shape>()
            .register_type::<LayerMask>()
            .register_type::<CollisionLayers>()
            .register_type::<Collider>()
            .register_type::<Body>()
            .register_type::<Volume>()
            .register_type::<Current>()
            .register_type::<VolumeKind>()
            .register_type::<ColliderId>()
            // Snapshots: the counter is saved; the colliders of entities without an `AiId`
            // (level geometry) are not, but must be the same where a snapshot is loaded
            // (they are checked) and can be named in references (`c7`); `PhysicsWorld` is a
            // cache and is made again.
            .add_snapshot_resource::<ColliderIds>()
            // Each machine counts its own colliders; replicated ones carry their id verbatim.
            .net_ignore_resource::<ColliderIds>()
            .add_snapshot_check(
                "static_colliders",
                "the colliders of entities without an AiId (level geometry) differ: a level the game spawned itself \
                 (not by id) must stand here too, or give what moves an AiId so the snapshot holds it",
                static_colliders_hash,
            )
            .add_snapshot_rebuild("physics", rebuild_physics)
            .add_snapshot_entity_key(EntityKey { prefix: "c", encode: collider_number, index: collider_index })
            .configure_sets(
                FixedUpdate,
                (
                    PhysicsSystems::Sync.before(SimSet::Input),
                    PhysicsSystems::SyncAfterMove.after(SimSet::Move).before(SimSet::Resolve),
                    // A client's prediction sees the replicated world where the host had it.
                    // Before the simulation (as the core plugin orders it too): motion also
                    // runs without it.
                    NetSet::PredictInput.after(PhysicsSystems::Sync),
                    NetSet::Predict.after(PhysicsSystems::Sync).before(PhysicsSystems::SyncAfterMove),
                    (NetSet::PredictInput, NetSet::Predict, NetSet::PredictEnd).chain().before(SimSet::Input).run_if(predicts),
                ),
            )
            .add_systems(
                FixedUpdate,
                (sync_physics.in_set(PhysicsSystems::Sync), sync_physics.in_set(PhysicsSystems::SyncAfterMove)),
            )
            // Between ticks too, so queries from tools see what tools just moved (and the
            // motion tools that ask `PhysicsWorld` sync it themselves, `fresh`, for runners
            // that call tools with no frame between). The result depends only on the ECS,
            // so this never changes a tick's outcome.
            .add_systems(Last, sync_physics);
    }
}

/// Snapshot check: every collider without an `AiId`, by id: its shape, pose, layers and
/// volume, and every voxel field (its size, cell, origin and bits through [`VoxelField::hash`],
/// and its layers). Those are not saved (a snapshot is loaded into a world that spawned them
/// the same way), so they must match. An entity without a field hashes as it always did.
fn static_colliders_hash(world: &World) -> u64 {
    let mut found: Vec<(ColliderId, String)> = world
        .iter_entities()
        .filter(|e| !e.contains::<AiId>())
        .filter_map(|e| {
            let id = *e.get::<ColliderId>()?;
            let mut facts = format!(
                "{:?} {:?} {:?} {:?}",
                e.get::<Collider>(),
                e.get::<Transform>().map(|t| (t.translation, t.rotation, t.scale)),
                e.get::<Body>(),
                e.get::<Volume>()
            );
            if let Some(v) = e.get::<VoxelCollider>() {
                let f = &v.field;
                facts += &format!(" field {:016x} {:?}", f.hash(), f.layers());
            }
            Some((id, facts))
        })
        .collect();
    found.sort_by_key(|(id, _)| *id);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (id, facts) in found {
        for b in id.0.to_le_bytes().iter().chain(facts.as_bytes()).chain(&[0xff]) {
            h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn collider_number(world: &World, entity: Entity) -> Option<u32> {
    world.get::<ColliderId>(entity).map(|id| id.0)
}

fn collider_index(world: &mut World) -> Vec<(u32, Entity)> {
    world.query_filtered::<(Entity, &ColliderId), Without<AiId>>().iter(world).map(|(e, id)| (id.0, e)).collect()
}

/// After a snapshot replaced the characters: the cache catches up with the ECS now rather
/// than at the next tick, so queries from tools see the restored world.
fn rebuild_physics(world: &mut World) {
    if let Err(e) = world.run_system_cached(sync_physics) {
        warn!("physics: cannot catch up after loading a snapshot: {e}");
    }
}

/// What the sync reads from a collider entity.
pub(crate) type ColliderData<'a> =
    (Entity, &'a ColliderId, &'a Collider, &'a Transform, Option<&'a Body>, Option<&'a Volume>);

/// Anything the sync reads changed.
type ColliderChanged = Or<(Changed<Collider>, Changed<Transform>, Changed<Body>, Changed<Volume>, Changed<ColliderId>)>;

/// What the sync reads from a voxel field entity, when it changed (new, replaced, renumbered).
pub(crate) type FieldsChanged<'w, 's> =
    Query<'w, 's, (Entity, &'static ColliderId, &'static VoxelCollider, Has<Collider>), Or<(Changed<VoxelCollider>, Changed<ColliderId>)>>;

/// Brings [`PhysicsWorld`] up to date: changed colliders, then every kinematic one, then new or
/// replaced voxel fields. Removals already happened in the `Collider` and `VoxelCollider` hooks.
pub fn sync_physics(
    mut physics: ResMut<PhysicsWorld>,
    changed: Query<ColliderData, ColliderChanged>,
    colliders: Query<ColliderData>,
    fields: FieldsChanged,
) {
    for data in &changed {
        physics.upsert(data);
    }
    physics.refresh_kinematic(|entity| colliders.get(entity).ok());
    for (entity, id, voxels, collider) in &fields {
        if collider {
            warn!("{entity}: carries both a Collider and a VoxelCollider; give the field an entity of its own");
        }
        physics.upsert_field(entity, *id, &voxels.field);
    }
}

/// The first half of a tool that asks [`PhysicsWorld`] something (`fresh::<Args>.pipe(tool)`):
/// brings it up to date, then hands the call on. A tool runs between frames, and runners
/// that call tools back to back (scenarios, tests) give the `Last` sync no frame to run in,
/// so a collider spawned or moved by the call before would not be there yet. The sync is
/// the same pure function of the ECS the ticks run, so this never changes a tick's outcome.
pub(crate) fn fresh<A: Send + Sync + 'static>(
    In(call): In<ai_core::prelude::Call<A>>,
    physics: ResMut<PhysicsWorld>,
    changed: Query<ColliderData, ColliderChanged>,
    colliders: Query<ColliderData>,
    fields: FieldsChanged,
) -> ai_core::prelude::Call<A> {
    sync_physics(physics, changed, colliders, fields);
    call
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_static_colliders_hash_follows_a_field_and_leaves_colliders_as_they_were() {
        let block = |solid: fn(IVec3) -> bool, origin: Vec3| VoxelCollider::new(VoxelField::new(origin, 0.5, UVec3::new(4, 2, 4), solid));
        let hash = |field: Option<VoxelCollider>| {
            let mut world = World::new();
            world.spawn((ColliderId(1), Collider::new(Shape::cuboid(Vec3::ONE)), Transform::from_xyz(1.0, 2.0, 3.0)));
            if let Some(field) = field {
                world.spawn((ColliderId(2), field));
            }
            static_colliders_hash(&world)
        };
        let all: fn(IVec3) -> bool = |_| true;
        let base = hash(Some(block(all, Vec3::ZERO)));
        assert_eq!(base, hash(Some(block(all, Vec3::ZERO))));
        assert_ne!(base, hash(Some(block(|c| c != IVec3::ZERO, Vec3::ZERO))), "one cell");
        assert_ne!(base, hash(Some(block(all, Vec3::X))), "the place");
        let ghost = VoxelCollider::new(VoxelField::new(Vec3::ZERO, 0.5, UVec3::new(4, 2, 4), all).with_layers(CollisionLayers::VOLUME));
        assert_ne!(base, hash(Some(ghost)), "the layers");
        assert_ne!(base, hash(None));

        // A collider is hashed as before fields: the same facts, the same order, no more.
        let mut world = World::new();
        let (collider, tf) = (Collider::new(Shape::cuboid(Vec3::ONE)), Transform::from_xyz(1.0, 2.0, 3.0));
        let e = world.spawn((ColliderId(1), collider, tf)).id();
        let facts = format!("{:?} {:?} {:?} {:?}", Some(&collider), Some((tf.translation, tf.rotation, tf.scale)), world.get::<Body>(e), world.get::<Volume>(e));
        let mut want: u64 = 0xcbf2_9ce4_8422_2325;
        for b in 1u32.to_le_bytes().iter().chain(facts.as_bytes()).chain(&[0xff]) {
            want = (want ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        assert_eq!(static_colliders_hash(&world), want);
    }

    #[test]
    fn shapes_describe_their_full_size() {
        assert_eq!(Shape::cuboid(Vec3::new(1.0, 2.0, 1.0)).to_string(), "box 2×4×2 m");
        assert_eq!(Shape::capsule(0.4, 1.8).to_string(), "capsule r0.4 h1.8 m");
        assert_eq!(Shape::sphere(0.25).to_string(), "sphere r0.25 m");
        assert_eq!(Shape::Cylinder { half_height: 1.0, radius: 0.5 }.to_string(), "cylinder r0.5 h2 m");
    }

    #[test]
    fn layer_masks_combine() {
        let both = LayerMask::WORLD | LayerMask::CHARACTER;
        assert!(both.contains(LayerMask::WORLD) && both.intersects(LayerMask::CHARACTER));
        assert!(!both.intersects(LayerMask::VOLUME));
        assert_eq!(both.without(LayerMask::WORLD), LayerMask::CHARACTER);
        assert_eq!(LayerMask::game(0).0, 1 << 16);
        assert!(CollisionLayers::CHARACTER.interacts_with(&CollisionLayers::WORLD));
        assert!(!CollisionLayers::CHARACTER.interacts_with(&CollisionLayers::CHARACTER));
    }
}
