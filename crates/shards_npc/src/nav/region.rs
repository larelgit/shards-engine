//! [`NavRegion`]: the navmesh of the region in the world, stitched from its tiles' bakes.
//!
//! Built when a region loads (the world module's [`RegionLoadedHooks`]), inside the call
//! that loads it, so no tick ever sees a region without it:
//!
//! 1. **Bakes**: each tile (and room) of the manifest, once per id: its bake file when the
//!    file is fresh (its hash matches the definition), else baked now, with a journal
//!    warning naming the files to bake (`--bake-nav`).
//! 2. **Placing**: every placed tile is an instance of its bake, turned with the tile. A
//!    bake is in its tile's own frame, so turning never touches its links; positions and
//!    directions turn on the way out.
//! 3. **Carving**: solids the bakes never saw (decor, props, a neighbour's wall reaching
//!    over the edge, colliders spawned later) switch off the nodes the body no longer fits
//!    on, checked with the motion module's terrain probe in the assembled world.
//! 4. **Stitching**: across every tile edge the region's sockets leave open (the world
//!    module's [`RegionGraph`](shards_world::RegionGraph)), each boundary node gets the move
//!    the motion planner would make into the facing column; edges whose sockets are not
//!    passable stay closed, whatever the geometry.
//! 5. **Navlinks**: level markers `navlink` with a `to` are one-way links from the node at
//!    the marker to the node at `to`, for the moves the body makes: a drop of any depth, a
//!    climb within its mantle height, a step to the next column (`nav_info` says why
//!    another is not used: the body has no jump).
//! 6. **The tile graph**: portals (one per side and area of each tile), the costs between
//!    them inside each tile, the links between tiles; and which nodes reach which at all.
//!
//! Every body class of the nav settings has its own navmesh, built the same way from its own
//! bakes: the region's [`NavRegion`] is the `agent`'s, the others are in its
//! [`bodies`](NavRegion::bodies). A route asks the tightest class that serves its whole body
//! ([`NavRegion::serving`]).
//!
//! Afterwards, every tick (and in tools), solids that spawned, went or moved since are
//! carved out or back in, and the links across tile edges near them made again: a node
//! is off when some solid its bake never saw stands in the body's way there **now**, so the
//! navmesh never depends on the order solids came and went in.
//!
//! Everything is a pure function of the manifest, content and the static solids standing
//! in the physics world, built in a fixed order: the same region gives the same navmesh, so
//! NPC routes replay, and a game loaded from a snapshot (which builds it again) goes on
//! with the navmesh the saved game had.

use std::collections::BTreeMap;
use std::sync::Arc;

use ai_core::journal::{self, JournalKind};
use bevy::prelude::*;
use shards_content::{asset_root, ContentDb};
use shards_level::{LevelMarker, MarkerKind};
use shards_motion::perception::{hop_allowed, step_onto, Block, Probe, Stance, StepKind, Terrain};
use shards_motion::physics::sync_physics;
use shards_motion::{Body, CharacterParams, ColliderId, ColliderInfo, LayerMask, PhysicsWorld, Shape, VolumeKind};
use shards_world::grid::{rotation, turn, turns_of, Dir};
use shards_world::manifest::Layout;
use shards_world::{ActiveRegion, RegionGraph, TileId};

mod levels;
mod volume;
pub(crate) use levels::current as levels_current;
pub use levels::{LevelBake, LEVEL_CHUNK};
pub use volume::{bake_volume, chunk_files_sequential, clear_shared_bakes, prebake_volume, ChunkKey, Cut, Prebaked, VoxelBake};

use super::bake::{bake, dijkstra, solid};
use super::settings::{whole_cells, NavSettings, DEFAULT_BODY};
use super::tile::{link_cost, NavTile, TileKind, MOVES};

/// Global node id: an instance's first id plus the node's index in its bake.
pub type NodeId = u32;

/// A placed tile: its bake, where and how it is turned.
#[derive(Clone, Debug)]
pub struct Instance {
    /// The manifest cell of its north-west corner.
    pub cell: [i32; 2],
    pub tile: String,
    pub kind: TileKind,
    /// Quarter turns clockwise.
    pub turns: u8,
    pub nav: Arc<NavTile>,
    /// Global grid column of its north-west corner.
    pub origin: [i32; 2],
    /// Grid columns it covers once turned.
    pub grid: [u32; 2],
    /// Its center in the world.
    pub center: Vec3,
    /// Nodes switched off by carving (empty: none).
    pub disabled: Vec<bool>,
}

impl Instance {
    /// Local column `(i, j)` → global column.
    pub fn to_global(&self, i: i32, j: i32) -> [i32; 2] {
        // One clockwise turn takes (x, y) of a w×h grid to (h - 1 - y, x) of an h×w one.
        let [mut w, mut h] = [self.nav.size[0] as i32, self.nav.size[1] as i32];
        let (mut x, mut y) = (i, j);
        for _ in 0..self.turns {
            (x, y) = (h - 1 - y, x);
            std::mem::swap(&mut w, &mut h);
        }
        [self.origin[0] + x, self.origin[1] + y]
    }

    /// Global column → local column (it may be outside the bake's grid).
    pub fn to_local(&self, [gx, gz]: [i32; 2]) -> (i32, i32) {
        let (mut x, mut y) = (gx - self.origin[0], gz - self.origin[1]);
        // Undo one turn at a time, from the turned grid (`grid`, w×h) back: (x, y) of the
        // turned one came from (y, w - 1 - x) of the h×w one before.
        let [mut w, mut h] = [self.grid[0] as i32, self.grid[1] as i32];
        for _ in 0..self.turns {
            (x, y) = (y, w - 1 - x);
            std::mem::swap(&mut w, &mut h);
        }
        (x, y)
    }

    /// A local direction (`MOVES`) in the world.
    pub fn to_world_dir(&self, (di, dj): (i32, i32)) -> (i32, i32) {
        let (mut x, mut y) = (di, dj);
        for _ in 0..self.turns {
            (x, y) = (-y, x);
        }
        (x, y)
    }

    /// A world direction in the bake's frame.
    pub fn to_local_dir(&self, (dx, dz): (i32, i32)) -> (i32, i32) {
        let (mut x, mut y) = (dx, dz);
        for _ in 0..self.turns {
            (x, y) = (y, -x);
        }
        (x, y)
    }

    /// The world side of a side of the bake.
    pub fn world_side(&self, side: Dir) -> Dir {
        side.turned(self.turns)
    }

    /// The bake's side that faces `side` in the world.
    pub fn local_side(&self, side: Dir) -> Dir {
        side.turned(4 - self.turns % 4)
    }

    pub fn is_disabled(&self, n: u32) -> bool {
        self.disabled.get(n as usize).copied().unwrap_or(false)
    }
}

/// A link that is not inside one bake: across a tile edge, or a navlink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extra {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: StepKind,
    pub cost: u32,
    pub rise: f32,
    /// A level's navlink (not a stitch).
    pub navlink: bool,
}

/// A navlink marker as the navmesh took it.
#[derive(Clone, Debug, PartialEq)]
pub struct NavLinkUse {
    /// The marker's name.
    pub name: String,
    pub from: Vec3,
    pub to: Vec3,
    /// The nodes it joins, when both ends are on walkable ground and the body can make the
    /// move.
    pub nodes: Option<(NodeId, NodeId)>,
    /// Why it is not used, when it is not.
    pub unused: Option<String>,
}

/// A node of the tile graph: a portal of an instance, or a navlink's end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GraphNode {
    pub inst: u32,
    /// The node it measures from (`None`: every member is switched off).
    pub node: Option<NodeId>,
    /// Its portal in the bake, for a portal.
    pub portal: Option<u32>,
}

/// The tile graph: portals and navlink ends, joined inside tiles and across them.
#[derive(Clone, Debug, Default)]
pub(crate) struct TileGraph {
    pub nodes: Vec<GraphNode>,
    /// The graph nodes of each instance: its portals, then its navlink ends.
    pub of_instance: Vec<Vec<u32>>,
    /// `edges[first[n]..first[n + 1]]` leave node `n`: `(to, cost)`.
    pub first: Vec<u32>,
    pub edges: Vec<(u32, u32)>,
}

/// What a build did, for the journal and the tools.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavReport {
    /// Tiles whose bake file was read.
    pub from_files: Vec<String>,
    /// Tiles baked while loading: no bake file.
    pub missing: Vec<String>,
    /// Tiles baked while loading: a stale or unreadable bake file.
    pub stale: Vec<String>,
    /// Tiles that could not be baked, and why: they have no walkable ground.
    pub failed: Vec<(String, String)>,
    /// Links across tile edges.
    pub stitched: usize,
    /// Tile edges the sockets keep closed although both sides have ground.
    pub closed_edges: usize,
    /// Nodes switched off by carving.
    pub carved: usize,
}

/// What carving and the links across tile edges were last made against: every static solid
/// and water volume of the world, by id, and how many solids may switch off each node.
///
/// A node is switched off when the zone of some solid its tile's bake never saw holds it
/// (near the solid, at a height the body would meet it) and the baked body no longer stands
/// there in the world as it is: a pure function of the solids standing now, whatever order
/// they came and went in. A change (a solid spawned, gone or moved) is found by comparing
/// with what was seen, and the nodes and edge links near it are decided again.
#[derive(Clone, Debug, Default)]
struct Carve {
    /// By id.
    seen: Vec<Seen>,
    /// Per node: the solids whose zone holds it.
    held: Vec<u32>,
}

/// A static solid or water volume as carving last saw it.
#[derive(Clone, Debug)]
struct Seen {
    id: ColliderId,
    shape: Shape,
    position: Vec3,
    rotation: Quat,
    water: bool,
    aabb_min: Vec3,
    aabb_max: Vec3,
    /// The nodes it may switch off (none for water, or for a tile's own solids in that tile).
    zone: Vec<NodeId>,
}

impl Carve {
    /// Whether the static solids and water in `physics` are not the ones last seen.
    fn differs(&self, physics: &PhysicsWorld) -> bool {
        let mut now = physics.colliders().filter(|c| tracked(c));
        self.seen.iter().any(|s| now.next().is_none_or(|c| !s.is(c))) || now.next().is_some()
    }
}

impl Seen {
    /// Whether `c` is this one, where it was.
    fn is(&self, c: &ColliderInfo) -> bool {
        self.id == c.id && self.shape == c.shape && self.position == c.position && self.rotation == c.rotation && self.water == is_water(c)
    }
}

fn is_water(c: &ColliderInfo) -> bool {
    matches!(c.volume, Some(VolumeKind::Water))
}

/// What the navmesh is made against: static solids (the terrain probe's floors and walls) and
/// water (where it swims).
fn tracked(c: &ColliderInfo) -> bool {
    solid(c) || is_water(c)
}

/// Colliders on the `WORLD` layer that move (kinematic platforms): the navmesh is made of what
/// stands still, so its probes leave them out.
fn movers(physics: &PhysicsWorld) -> Vec<Entity> {
    physics
        .colliders()
        .filter(|c| c.volume.is_none() && c.body != Body::Static && c.layers.memberships.intersects(LayerMask::WORLD))
        .map(|c| c.entity)
        .collect()
}

/// The terrain probe carving and stitching use: the baked body, what stands still.
fn terrain<'a>(physics: &'a PhysicsWorld, settings: &'a NavSettings) -> Terrain<'a> {
    Terrain::new(physics, &settings.agent, &movers(physics))
}

/// Which tile edges are open to walk across.
#[derive(Clone, Copy)]
enum Edges<'a> {
    /// The region's sockets and roads say (tiles and rooms).
    Graph(&'a RegionGraph),
    /// Every edge between two chunks of a voxel region: the ground does not have sockets, and a
    /// chunk's seam is only where one bake ends and the next begins.
    Open,
}

impl Edges<'_> {
    /// Whether the stitch makes the diagonal moves across an edge too: where the edge is only a
    /// seam between two bakes of one ground, so that the chunks together are the grid baked in
    /// one piece. (Tiles keep to their stitches: their bakes are the tiles' own, and changing
    /// them would change every region of tiles.)
    fn diagonals(self) -> bool {
        matches!(self, Edges::Open)
    }

    fn open(self, cell: [i32; 2], dir: Dir) -> bool {
        match self {
            Edges::Graph(graph) => graph.open(cell, dir),
            Edges::Open => true,
        }
    }
}

/// How a move across a tile edge goes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    /// Into the facing column.
    Step,
    /// A two-cell hop over this column, where the body does not fit.
    Hop([i32; 2]),
    /// Into the column diagonally across the edge, never cutting a corner (a voxel region's
    /// chunks, where a seam is only where one bake ends and the next begins).
    Diagonal,
}

/// A move across a tile edge: `(from, via, to)` columns.
type EdgeMove = ([i32; 2], Via, [i32; 2]);

/// One row across a tile edge: column `a` (west or north of the edge) faces column `b`.
#[derive(Clone, Copy, Debug)]
struct EdgeRow {
    a: [i32; 2],
    b: [i32; 2],
    /// From `a` to `b`.
    step: [i32; 2],
    open: bool,
    edge: usize,
}

impl EdgeRow {
    /// The four columns its moves touch: one more on either side.
    fn columns(&self) -> [[i32; 2]; 4] {
        let [dx, dz] = self.step;
        [[self.a[0] - dx, self.a[1] - dz], self.a, self.b, [self.b[0] + dx, self.b[1] + dz]]
    }

    /// The columns its moves touch: with `diagonals`, also the ones beside the facing pair.
    fn touched(&self, diagonals: bool) -> Vec<[i32; 2]> {
        let mut out = self.columns().to_vec();
        if diagonals {
            let [px, pz] = self.along();
            for c in [self.a, self.b] {
                out.extend([[c[0] + px, c[1] + pz], [c[0] - px, c[1] - pz]]);
            }
        }
        out
    }

    /// The step along the edge (a unit vector, perpendicular to `step`).
    fn along(&self) -> [i32; 2] {
        [self.step[1].abs(), self.step[0].abs()]
    }

    /// The moves across it, `(from, via, to)`: into the facing column both ways, the two-cell
    /// hops over either edge column both ways, and with `diagonals` the four diagonal moves
    /// between the facing columns and the ones beside them.
    fn moves(&self, diagonals: bool) -> Vec<EdgeMove> {
        let [before, a, b, after] = self.columns();
        let mut out = vec![
            (a, Via::Step, b),
            (b, Via::Step, a),
            (before, Via::Hop(a), b),
            (a, Via::Hop(b), after),
            (after, Via::Hop(b), a),
            (b, Via::Hop(a), before),
        ];
        if diagonals {
            let [px, pz] = self.along();
            let beside = |c: [i32; 2], sign: i32| [c[0] + sign * px, c[1] + sign * pz];
            for sign in [1, -1] {
                out.push((a, Via::Diagonal, beside(b, sign)));
                out.push((b, Via::Diagonal, beside(a, sign)));
            }
        }
        out
    }
}

/// A region whose navmesh could not be built, and the content it was tried with: not tried
/// again every tick, only when the region or content changes.
#[derive(Resource, Clone, Copy, Debug)]
struct NavFailed {
    region: u64,
    content: bevy::ecs::change_detection::Tick,
    /// Whether this machine simulated then: a client that becomes a host tries again.
    simulating: bool,
}

/// The navmesh of the region in the world (see the module docs): the one of the body class
/// `agent`, with the other classes' in [`bodies`](Self::bodies).
#[derive(Resource, Clone, Debug)]
pub struct NavRegion {
    /// [`ActiveRegion::hash`] of the region it was built for.
    pub hash: u64,
    /// The body class it is baked for: `agent`, or a name of the nav settings' `bodies`.
    pub body: String,
    /// The navmeshes of the other body classes, by name (only on the `agent`'s, the
    /// resource; a class that could not be built is missing).
    pub bodies: Vec<NavRegion>,
    /// `meadow seed 42 v1`.
    pub title: String,
    pub cell: f32,
    pub tile_size: f32,
    /// The world position (x, z) of the region's north-west corner: global column `(0, 0)`
    /// starts there ([`RegionManifest::origin`](shards_world::RegionManifest::origin)).
    pub origin: Vec2,
    /// Grid columns per region cell.
    pub per: u32,
    /// Region cells (x, y).
    pub size: [u32; 2],
    /// The body the tiles were baked for.
    pub agent_radius: f32,
    pub agent_height: f32,
    /// All of the body the tiles were baked for: how high it steps and climbs, how steep it
    /// walks, where it swims.
    pub agent: CharacterParams,
    pub safe_drop: f32,
    pub instances: Vec<Instance>,
    /// Instance covering each region cell, row-major (`u32::MAX`: none).
    cell_instance: Vec<u32>,
    /// First global id of each instance, and the total at the end.
    base: Vec<u32>,
    /// Sorted by `from`, then `to`.
    pub(crate) extras: Vec<Extra>,
    pub navlinks: Vec<NavLinkUse>,
    pub(crate) graph: TileGraph,
    /// Which nodes reach which (links taken both ways, switched-off nodes left out): a label
    /// per node.
    components: Vec<u32>,
    pub report: NavReport,
    /// How a voxel region's chunks were baked (`None`: tiles).
    pub voxel: Option<VoxelBake>,
    /// The levels it is the navmesh of and how their chunks were baked, when it is the navmesh
    /// of hand-made levels and not of a region (`region::levels`).
    pub levels: Option<LevelBake>,
    carve: Carve,
}

/// Bakes read or made this run: a region loaded again bakes nothing twice. A tile or room by
/// its hash; a voxel region's chunk by [`ChunkKey`] (region, field and water, chunk size,
/// chunk, body class and the settings it was baked with), in memory only: nothing is written.
#[derive(Resource, Default, Clone)]
pub struct NavBakes {
    pub tiles: BTreeMap<String, Arc<NavTile>>,
    pub chunks: BTreeMap<ChunkKey, Arc<NavTile>>,
    /// Keep chunk bakes to this world: neither read the process's bakes of other games nor leave
    /// any (tests that bake cold, or that must not see the bakes of tests running beside them).
    pub isolated: bool,
}

/// How a tile's bake came to be.
enum Source {
    File,
    Missing,
    Stale,
    Failed(String),
}

impl NavRegion {
    /// Global grid column of a world position.
    pub fn column_of(&self, pos: Vec3) -> [i32; 2] {
        [((pos.x - self.origin.x) / self.cell).floor() as i32, ((pos.z - self.origin.y) / self.cell).floor() as i32]
    }

    /// The world position of a global column's center (y 0).
    pub fn column_center(&self, [gx, gz]: [i32; 2]) -> Vec3 {
        Vec3::new(self.origin.x + (gx as f32 + 0.5) * self.cell, 0.0, self.origin.y + (gz as f32 + 0.5) * self.cell)
    }

    /// The region's north-west corner in the world (y 0).
    pub fn corner(&self) -> Vec3 {
        Vec3::new(self.origin.x, 0.0, self.origin.y)
    }

    /// The instance covering global column `col`.
    pub fn instance_at(&self, [gx, gz]: [i32; 2]) -> Option<u32> {
        if gx < 0 || gz < 0 {
            return None;
        }
        let (cx, cy) = (gx as u32 / self.per, gz as u32 / self.per);
        if cx >= self.size[0] || cy >= self.size[1] {
            return None;
        }
        let i = self.cell_instance[(cy * self.size[0] + cx) as usize];
        (i != u32::MAX).then_some(i)
    }

    /// The nodes of global column `col`, lowest first.
    pub fn nodes_at(&self, col: [i32; 2]) -> std::ops::Range<NodeId> {
        let Some(inst) = self.instance_at(col) else { return 0..0 };
        let instance = &self.instances[inst as usize];
        let (i, j) = instance.to_local(col);
        let range = instance.nav.column(i, j);
        let base = self.base[inst as usize];
        base + range.start..base + range.end
    }

    /// Instance and local index of a global node.
    pub fn split(&self, node: NodeId) -> (u32, u32) {
        let inst = self.base.partition_point(|b| *b <= node) - 1;
        (inst as u32, node - self.base[inst])
    }

    /// Global id of node `n` of instance `inst`.
    pub fn global(&self, inst: u32, n: u32) -> NodeId {
        self.base[inst as usize] + n
    }

    /// Every node of the region.
    pub fn node_count(&self) -> u32 {
        *self.base.last().unwrap_or(&0)
    }

    /// The global column of a node.
    pub fn node_column(&self, node: NodeId) -> [i32; 2] {
        let (inst, n) = self.split(node);
        let instance = &self.instances[inst as usize];
        let local = &instance.nav.nodes[n as usize];
        instance.to_global(local.i as i32, local.j as i32)
    }

    /// Where a node is: feet (a swimmer's float height), at its column's center.
    pub fn node_pos(&self, node: NodeId) -> Vec3 {
        let (inst, n) = self.split(node);
        let local = &self.instances[inst as usize].nav.nodes[n as usize];
        let c = self.column_center(self.node_column(node));
        Vec3::new(c.x, local.y(), c.z)
    }

    pub fn is_swim(&self, node: NodeId) -> bool {
        let (inst, n) = self.split(node);
        self.instances[inst as usize].nav.nodes[n as usize].swim
    }

    pub fn is_disabled(&self, node: NodeId) -> bool {
        let (inst, n) = self.split(node);
        self.instances[inst as usize].is_disabled(n)
    }

    /// The links out of `node` that are not inside its bake.
    pub fn extras_of(&self, node: NodeId) -> &[Extra] {
        let start = self.extras.partition_point(|e| e.from < node);
        let end = self.extras.partition_point(|e| e.from <= node);
        &self.extras[start..end]
    }

    /// Every move out of `node`: `(to, kind, cost, rise)`. Switched-off ends are left out.
    pub fn moves(&self, node: NodeId, out: &mut Vec<(NodeId, StepKind, u32, f32)>) {
        out.clear();
        let (inst, n) = self.split(node);
        let instance = &self.instances[inst as usize];
        if instance.is_disabled(n) {
            return;
        }
        let base = self.base[inst as usize];
        for link in instance.nav.links_of(n) {
            if !instance.is_disabled(link.to) {
                out.push((base + link.to, link.kind, link.cost, link.rise));
            }
        }
        for extra in self.extras_of(node) {
            if !self.is_disabled(extra.to) {
                out.push((extra.to, extra.kind, extra.cost, extra.rise));
            }
        }
    }

    /// The walking move from `node` to the neighbouring global column `(dx, dz)` away
    /// (one step: orthogonal or diagonal), if there is one.
    pub fn walk_toward(&self, node: NodeId, step: (i32, i32)) -> Option<NodeId> {
        self.move_toward(node, step, |kind| kind == StepKind::Walk)
    }

    /// The move from `node` to the neighbouring global column `(dx, dz)` away (one step:
    /// orthogonal or diagonal, no navlink) of a kind `takes` accepts, if there is one.
    pub fn move_toward(&self, node: NodeId, (dx, dz): (i32, i32), takes: impl Fn(StepKind) -> bool) -> Option<NodeId> {
        let (inst, n) = self.split(node);
        let instance = &self.instances[inst as usize];
        let local = instance.to_local_dir((dx, dz));
        let code = MOVES.iter().take(8).position(|m| *m == local)? as u8;
        let base = self.base[inst as usize];
        if let Some(link) = instance.nav.links_of(n).iter().find(|l| l.code == code && takes(l.kind)) {
            return (!instance.is_disabled(link.to)).then_some(base + link.to);
        }
        let target = {
            let c = self.node_column(node);
            [c[0] + dx, c[1] + dz]
        };
        self.extras_of(node)
            .iter()
            .find(|e| takes(e.kind) && !e.navlink && self.node_column(e.to) == target)
            .filter(|e| !self.is_disabled(e.to))
            .map(|e| e.to)
    }

    /// The area label of a node across the region, carving included: two nodes with
    /// different labels never reach each other.
    pub fn component(&self, node: NodeId) -> u32 {
        self.components.get(node as usize).copied().unwrap_or(node)
    }

    /// Whether a body of this radius and height fits wherever the baked one does: not
    /// bigger.
    pub fn fits(&self, radius: f32, height: f32) -> bool {
        radius <= self.agent_radius + 0.01 && height <= self.agent_height + 0.01
    }

    /// Whether a walker of `params` makes every move of this navmesh: it [`fits`](Self::fits),
    /// steps, climbs and walks slopes at least as well as the baked body, and swims where it
    /// does. Others plan on the local grid, which knows their own limits.
    pub fn serves(&self, params: &CharacterParams) -> bool {
        self.lack(params).is_none()
    }

    /// The first number of the baked body that keeps it from serving a walker of `params`
    /// ([`serves`](Self::serves)): its `locomotion` field, the baked body's value and the
    /// walker's. `None`: it serves it.
    pub fn lack(&self, params: &CharacterParams) -> Option<(&'static str, f32, f32)> {
        let a = &self.agent;
        if params.radius > self.agent_radius + 0.01 {
            return Some(("radius", self.agent_radius, params.radius));
        }
        if params.height > self.agent_height + 0.01 {
            return Some(("height", self.agent_height, params.height));
        }
        [
            ("step_height", a.step_height, params.step_height),
            ("mantle_height", a.mantle_height, params.mantle_height),
            ("max_slope_deg", a.max_slope_deg, params.max_slope_deg),
        ]
        .into_iter()
        .find(|(_, baked, own)| own + 0.01 < *baked)
        .or_else(|| {
            [("swim_depth", a.swim_depth, params.swim_depth), ("float_depth", a.float_depth, params.float_depth)]
                .into_iter()
                .find(|(_, baked, own)| (own - baked).abs() >= 0.01)
        })
    }

    /// The navmesh of every body class: this one (`agent`), then the others by name.
    pub fn classes(&self) -> impl Iterator<Item = &NavRegion> {
        std::iter::once(self).chain(&self.bodies)
    }

    /// The navmesh of body class `name`.
    pub fn class(&self, name: &str) -> Option<&NavRegion> {
        self.classes().find(|c| c.body == name)
    }

    /// The class whose body a route of only a radius is walked as (`nav_path` from a point,
    /// which has no body of its own): the tightest class whose body is at least as wide (the
    /// first of equals), else none. A walker with a body asks [`serving`](Self::serving).
    pub fn for_radius(&self, radius: f32) -> Option<&NavRegion> {
        self.classes().filter(|c| radius <= c.agent_radius + 0.01).min_by(|a, b| a.agent_radius.total_cmp(&b.agent_radius))
    }

    /// The navmesh that serves a walker of `params` ([`serves`](Self::serves)): the tightest
    /// such class, the narrowest and then the lowest (the first of equals), else none. What
    /// every route of a body is planned on: NPCs' ([`NavPaths`](super::NavPaths)), their
    /// straight walks, `move_to path=true`, `path_to`, `clearance` and `nav_path`.
    pub fn serving(&self, params: &CharacterParams) -> Option<&NavRegion> {
        self.classes()
            .filter(|c| c.serves(params))
            .min_by(|a, b| a.agent_radius.total_cmp(&b.agent_radius).then(a.agent_height.total_cmp(&b.agent_height)))
    }

    /// Why a body of `radius` plans on the local grid in this region: `no body class fits a
    /// 1.60 m radius (the widest: large, 1.20 m)`.
    pub fn too_wide(&self, radius: f32) -> String {
        let widest = self.classes().max_by(|a, b| a.agent_radius.total_cmp(&b.agent_radius)).unwrap_or(self);
        format!("no body class fits a {radius:.2} m radius (the widest: {}, {:.2} m)", widest.body, widest.agent_radius)
    }

    /// Why no class serves a walker of `params` ([`serving`](Self::serving) is `None`), so it
    /// plans on the local grid: [`too_wide`](Self::too_wide) when every class is narrower,
    /// else what each class lacks: `no body class serves the body: agent has height 1.80 m,
    /// the body 2.40 m; large has step_height 0.40 m, the body 0.35 m`.
    pub fn unserved(&self, params: &CharacterParams) -> String {
        if self.for_radius(params.radius).is_none() {
            return self.too_wide(params.radius);
        }
        let lacks: Vec<String> = self
            .classes()
            .filter_map(|c| {
                let (field, baked, own) = c.lack(params)?;
                let unit = if field == "max_slope_deg" { "°" } else { " m" };
                Some(format!("{} has {field} {baked:.2}{unit}, the body {own:.2}{unit}", c.body))
            })
            .collect();
        format!("no body class serves the body: {}", lacks.join("; "))
    }

    /// Whether it is cut into chunks of one ground, whose edges are only seams (a voxel region,
    /// hand-made levels), and not made of tiles with sockets.
    pub fn chunked(&self) -> bool {
        self.voxel.is_some() || self.levels.is_some()
    }

    /// The settings it was built with (carving and stitching again use its own body).
    pub fn settings(&self) -> NavSettings {
        NavSettings { cell: self.cell, safe_drop: self.safe_drop, agent: self.agent.clone(), dir: String::new(), body: self.body.clone() }
    }

    /// `64 tiles, 16384 nodes, 896 links across edges, 2 navlinks`.
    pub fn summary(&self) -> String {
        let links = self.navlinks.iter().filter(|l| l.nodes.is_some()).count();
        let noun = if self.chunked() { "chunks" } else { "tiles" };
        format!(
            "{} {noun}, {} nodes, {} links across edges, {} navlinks",
            self.instances.len(),
            self.node_count(),
            self.report.stitched,
            links
        )
    }
}

/// `RegionLoadedHooks`: the navmesh of the region that just loaded, journaled.
pub(crate) fn region_loaded(world: &mut World) {
    rebuild(world);
}

/// Builds the navmesh of the region in the world now, of every body class (or removes a
/// stale one when there is no region, or it cannot be built), and journals what it did. Without a
/// region, the navmesh of the hand-made levels that ask for one (`region::levels`). Also
/// what a snapshot load runs: the navmesh is a pure function of the region, content and the
/// solids standing, so the one made again is the one the saved game had.
pub fn rebuild(world: &mut World) {
    world.remove_resource::<NavRegion>();
    world.remove_resource::<NavFailed>();
    let Some(region) = world.get_resource::<ActiveRegion>().map(|r| r.hash) else {
        levels::rebuild(world);
        return;
    };
    world.remove_resource::<levels::NoLevelNav>();
    // A machine that does not simulate (a co-op client) never plans a route: the host's NPCs
    // walk, and its own navmesh is the host's business. A voxel region's bake is the one second
    // it would save.
    if super::region_is_volume(world) && !super::simulates(world) {
        let content = content_changed(world);
        world.insert_resource(NavFailed { region, content, simulating: false });
        journal::emit(world, JournalKind::Event, format!("navmesh: {}", super::VOXEL_NOT_SIMULATED));
        return;
    }
    let classes = NavSettings::all(world.resource::<ContentDb>());
    // A voxel region's chunks, of every body class at once, on the compute pool.
    let pre = if super::region_is_volume(world) { prebake_volume(world, &classes, Cut::Chunks).ok() } else { None };
    let mut pre_of = pre.iter().flat_map(|(counts, took)| counts.iter().map(|count| (*count, *took)));
    if let Err(why) = install(world, &classes, "this region", |world, settings| build(world, settings, pre_of.next())) {
        let content = content_changed(world);
        world.insert_resource(NavFailed { region, content, simulating: super::simulates(world) });
        journal::emit(world, JournalKind::Warn, format!("navmesh: none for this region: {why}; NPCs plan on the local grid"));
    }
}

/// Builds the navmesh of every body class of `classes` with `build` (called once per class, in
/// order), makes it the world's and journals what was built; `place` is what it is the navmesh
/// of, for the journal (`this region`). Why there is none, when the first class (`agent`) cannot
/// be built: nothing is inserted then.
fn install(
    world: &mut World,
    classes: &[NavSettings],
    place: &str,
    mut build: impl FnMut(&mut World, &NavSettings) -> Result<NavRegion, String>,
) -> Result<(), String> {
    let mut nav = build(world, &classes[0])?;
    let mut lines = vec![(JournalKind::Event, format!("navmesh: {} ({})", nav.title, nav.summary()))];
    report_lines(&nav, "navmesh", &mut lines);
    for settings in classes.iter().skip(1) {
        let label = format!("navmesh (body {})", settings.body);
        match build(world, settings) {
            Ok(class) => {
                lines.push((JournalKind::Event, format!("{label}: {}", class.summary())));
                report_lines(&class, &label, &mut lines);
                nav.bodies.push(class);
            }
            Err(why) => lines.push((JournalKind::Warn, format!("{label}: none for {place}: {why}; bodies of its size plan on the local grid"))),
        }
    }
    world.insert_resource(nav);
    for (kind, line) in lines {
        journal::emit(world, kind, line);
    }
    Ok(())
}

/// The journal's warnings about a navmesh just built (bakes made while loading, tiles that
/// cannot be baked, navlinks not used), each line starting with `label`.
fn report_lines(nav: &NavRegion, label: &str, lines: &mut Vec<(JournalKind, String)>) {
    let report = &nav.report;
    let mut baked: Vec<String> = Vec::new();
    if !report.missing.is_empty() {
        baked.push(format!("no bake file: {}", report.missing.join(", ")));
    }
    if !report.stale.is_empty() {
        baked.push(format!("stale: {}", report.stale.join(", ")));
    }
    if !baked.is_empty() {
        let n = report.missing.len() + report.stale.len();
        let tiles = if n == 1 { "tile" } else { "tiles" };
        lines.push((
            JournalKind::Warn,
            format!("{label}: {n} {tiles} baked while loading ({}); bake them with `--bake-nav all` (or the nav_bake tool)", baked.join("; ")),
        ));
    }
    let who = if nav.body == DEFAULT_BODY { "NPCs".to_owned() } else { format!("bodies of the class {}", nav.body) };
    for (tile, why) in &report.failed {
        lines.push((JournalKind::Warn, format!("{label}: tile `{tile}` cannot be baked ({why}); {who} plan on the local grid there")));
    }
    for link in &nav.navlinks {
        if let Some(why) = &link.unused {
            lines.push((JournalKind::Warn, format!("{label}: navlink `{}` is not used: {why}", link.name)));
        }
    }
}

/// When content last changed (a hot reload).
fn content_changed(world: &World) -> bevy::ecs::change_detection::Tick {
    world.get_resource_ref::<ContentDb>().map_or_else(Default::default, |db| db.last_changed())
}

/// The navmesh is the region's (or, without a region, the levels'): built for it and up to date
/// with the solids standing now (carved). Inside ticks and in tools; a pure function of the
/// world, so playback and a game loaded from a snapshot agree.
pub fn refresh(world: &mut World) {
    match world.get_resource::<ActiveRegion>().map(|r| r.hash) {
        Some(region) => {
            if world.get_resource::<NavRegion>().is_none_or(|nav| nav.hash != region || nav.levels.is_some()) {
                let failed = world.get_resource::<NavFailed>().copied();
                if failed.is_some_and(|f| f.region == region && f.content == content_changed(world) && f.simulating == super::simulates(world)) {
                    return;
                }
                rebuild(world);
                return;
            }
        }
        None => {
            // The levels placed, content or a baked solid changed: made again (kept chunks are
            // not baked twice). No level asks for one: nothing to do, until that changes.
            if !world.get_resource::<NavRegion>().is_some_and(|nav| levels::current(world, nav)) {
                if !(world.get_resource::<NavRegion>().is_none() && levels::settled(world)) {
                    rebuild(world);
                }
                return;
            }
        }
    }
    let changed = match (world.get_resource::<NavRegion>(), world.get_resource::<PhysicsWorld>()) {
        (Some(nav), Some(physics)) => nav.classes().any(|c| c.carve.differs(physics)),
        _ => false,
    };
    if !changed {
        return;
    }
    let _ = world.run_system_cached(sync_physics);
    let tiles = tile_ids(world);
    let mut nav = world.remove_resource::<NavRegion>().expect("checked above");
    let physics = world.resource::<PhysicsWorld>();
    // A volume's chunks all meet, and the levels'; a region of tiles has the sockets' say.
    let edges = match world.get_resource::<ActiveRegion>() {
        Some(r) if r.manifest.layout == Layout::Volume => Some(Edges::Open),
        Some(r) => r.graph.as_ref().map(Edges::Graph),
        None => Some(Edges::Open),
    };
    // Each class with its own body.
    let carve = |class: &mut NavRegion| {
        let settings = class.settings();
        if class.update(physics, &settings, &tiles, edges) {
            class.build_graph();
        }
    };
    carve(&mut nav);
    nav.bodies.iter_mut().for_each(carve);
    world.insert_resource(nav);
    // A solid the levels' chunks were baked from went or moved: they are baked again.
    if world.get_resource::<ActiveRegion>().is_none() && !world.get_resource::<NavRegion>().is_some_and(|nav| levels::current(world, nav)) {
        rebuild(world);
    }
}

/// The `TileId` of every region entity with a collider.
fn tile_ids(world: &mut World) -> BTreeMap<ColliderId, TileId> {
    let mut q = world.query::<(&ColliderId, &TileId)>();
    q.iter(world).map(|(c, t)| (*c, *t)).collect()
}

/// The navmesh of the region in the world for the body class of `settings`.
fn build(world: &mut World, settings: &NavSettings, prebaked: Option<(Prebaked, std::time::Duration)>) -> Result<NavRegion, String> {
    if super::region_is_volume(world) {
        // A volume has no tiles to bake: its navigation is baked in memory, chunk by chunk (the
        // pool has done the chunks already, when it could).
        return match prebaked {
            Some((prebaked, took)) => volume::build(world, settings, Cut::Chunks, prebaked, took),
            None => bake_volume(world, settings, Cut::Chunks),
        };
    }
    let region = world.get_resource::<ActiveRegion>().ok_or("no region")?.clone();
    let graph = region.graph.clone().ok_or("content no longer describes every tile of its manifest")?;
    let manifest = &region.manifest;
    if !whole_cells(manifest.tile_size, settings.cell) {
        return Err(format!("its tile size {} m is not a whole number of {} m cells (nav settings `cell`)", manifest.tile_size, settings.cell));
    }
    let per = (manifest.tile_size / settings.cell).round() as u32;
    // Not reached for a volume: it is baked by `bake_volume`, above.
    debug_assert!(manifest.layout != Layout::Volume, "a voxel region has no tile navmesh");
    let kind = if manifest.layout == Layout::Rooms { TileKind::Room } else { TileKind::Tile };
    let mut report = NavReport::default();

    // Bakes, once per id, in id order.
    let mut ids: Vec<&str> = manifest.tiles.iter().map(|t| t.tile.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut bakes: BTreeMap<String, Arc<NavTile>> = BTreeMap::new();
    for id in ids {
        let (nav, source) = tile_nav(world, kind, id, manifest.tile_size, settings);
        match source {
            Source::File => report.from_files.push(id.to_owned()),
            Source::Missing => report.missing.push(id.to_owned()),
            Source::Stale => report.stale.push(id.to_owned()),
            Source::Failed(why) => report.failed.push((id.to_owned(), why)),
        }
        bakes.insert(id.to_owned(), nav);
    }

    // Instances in manifest order.
    let mut instances = Vec::with_capacity(manifest.tiles.len());
    let mut base = vec![0u32];
    let mut cell_instance = vec![u32::MAX; (manifest.size[0] * manifest.size[1]) as usize];
    for (index, placed) in manifest.tiles.iter().enumerate() {
        let nav = bakes[&placed.tile].clone();
        let turns = turns_of(placed.rot).ok_or_else(|| format!("tile `{}`: rot {} is not a quarter turn", placed.tile, placed.rot))?;
        let [w, h] = nav.size;
        let grid = if turns % 2 == 1 { [h, w] } else { [w, h] };
        for (c, slot) in graph.cell_tile.iter().enumerate() {
            if *slot == Some(index) {
                cell_instance[c] = index as u32;
            }
        }
        base.push(base[index] + nav.nodes.len() as u32);
        instances.push(Instance {
            cell: placed.at,
            tile: placed.tile.clone(),
            kind,
            turns,
            origin: [placed.at[0] * per as i32, placed.at[1] * per as i32],
            grid,
            center: manifest.tile_center(placed),
            nav,
            disabled: Vec::new(),
        });
    }
    let _ = world.run_system_cached(sync_physics);
    let tiles = tile_ids(world);
    let markers = navlink_markers(world);
    let physics = world.resource::<PhysicsWorld>();
    let mut nav = NavRegion {
        hash: region.hash,
        body: settings.body.clone(),
        bodies: Vec::new(),
        title: manifest.title(),
        cell: settings.cell,
        tile_size: manifest.tile_size,
        origin: Vec2::from_array(manifest.origin),
        per,
        size: manifest.size,
        agent_radius: settings.agent.radius,
        agent_height: settings.agent.height,
        agent: settings.agent.clone(),
        safe_drop: settings.safe_drop,
        instances,
        cell_instance,
        base,
        extras: Vec::new(),
        navlinks: Vec::new(),
        graph: TileGraph::default(),
        components: Vec::new(),
        report,
        voxel: None,
        levels: None,
        carve: Carve::default(),
    };
    nav.update(physics, settings, &tiles, None);
    nav.stitch(&terrain(physics, settings), settings, Edges::Graph(&graph));
    nav.add_navlinks(settings, markers);
    nav.build_graph();
    Ok(nav)
}

/// A tile's bake: from this run's bakes, its file when fresh, else baked now.
fn tile_nav(world: &mut World, kind: TileKind, id: &str, tile_size: f32, settings: &NavSettings) -> (Arc<NavTile>, Source) {
    let root = asset_root(world);
    let db = world.resource::<ContentDb>();
    let hash = match super::settings::bake_hash(db, Some(&root), kind, id, tile_size, settings) {
        Ok(hash) => hash,
        Err(why) => return (Arc::new(empty_tile(kind, id, tile_size, settings)), Source::Failed(why)),
    };
    let file = std::fs::read_to_string(settings.file(&root, kind, id)).ok();
    let from_file = file.as_deref().map(|text| NavTile::from_file(text).ok().filter(|t| t.hash == hash));
    let source = match &from_file {
        Some(Some(_)) => Source::File,
        Some(None) => Source::Stale,
        None => Source::Missing,
    };
    if let Some(known) = world.get_resource::<NavBakes>().and_then(|b| b.tiles.get(&hash)).cloned() {
        return (known, source);
    }
    let made = match from_file.flatten() {
        Some(tile) => Ok(tile),
        None => bake(world, Some(&root), kind, id, tile_size, settings),
    };
    match made {
        Ok(tile) => {
            let tile = Arc::new(tile);
            world.get_resource_or_init::<NavBakes>().tiles.insert(hash, tile.clone());
            (tile, source)
        }
        Err(why) => (Arc::new(empty_tile(kind, id, tile_size, settings)), Source::Failed(why)),
    }
}

/// A tile without walkable ground: what a tile that cannot be baked counts as.
fn empty_tile(kind: TileKind, id: &str, tile_size: f32, settings: &NavSettings) -> NavTile {
    let per = (tile_size / settings.cell).round().max(1.0) as u32;
    NavTile {
        kind,
        id: id.to_owned(),
        hash: String::new(),
        tile_size,
        cell: settings.cell,
        size: [per, per],
        nodes: Vec::new(),
        first_link: vec![0],
        links: Vec::new(),
        columns: vec![0; (per * per) as usize + 1],
        areas: 0,
        portals: Vec::new(),
        portal_costs: Vec::new(),
        colliders: Vec::new(),
    }
}

/// Navlink markers with an end, in a fixed order: `(name, from, to)`.
fn navlink_markers(world: &mut World) -> Vec<(String, Vec3, Vec3)> {
    let mut q = world.query::<(&LevelMarker, &Transform)>();
    let mut out: Vec<(String, Vec3, Vec3)> = q
        .iter(world)
        .filter(|(m, _)| m.kind == MarkerKind::NavLink)
        .filter_map(|(m, tf)| m.to.map(|to| (m.name.clone(), tf.translation, to)))
        .collect();
    let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
    out.sort_by(|a, b| (bits(a.1), bits(a.2), &a.0).cmp(&(bits(b.1), bits(b.2), &b.0)));
    out
}

/// Whether two shapes are the same within float noise.
fn same_shape(a: &shards_motion::Shape, b: &shards_motion::Shape) -> bool {
    use shards_motion::Shape::*;
    let close = |x: f32, y: f32| (x - y).abs() < 1e-4;
    match (a, b) {
        (Cuboid { half: x }, Cuboid { half: y }) => x.abs_diff_eq(*y, 1e-4),
        (Sphere { radius: x }, Sphere { radius: y }) => close(*x, *y),
        (Capsule { half_height: h1, radius: r1 }, Capsule { half_height: h2, radius: r2 })
        | (Cylinder { half_height: h1, radius: r1 }, Cylinder { half_height: h2, radius: r2 })
        | (Cone { half_height: h1, radius: r1 }, Cone { half_height: h2, radius: r2 }) => close(*h1, *h2) && close(*r1, *r2),
        _ => false,
    }
}

impl NavRegion {
    /// The instances whose footprint, widened by `margin`, overlaps the box `min..max`.
    fn instances_near(&self, min: Vec3, max: Vec3, margin: f32) -> Vec<u32> {
        let s = self.tile_size;
        let cells = |lo: f32, hi: f32, n: u32| {
            let a = ((lo - margin) / s).floor().max(0.0) as i64;
            let b = ((hi + margin) / s).floor().min(n as f32 - 1.0) as i64;
            a..=b
        };
        let (min, max) = (min - self.corner(), max - self.corner());
        let mut out = Vec::new();
        for y in cells(min.z, max.z, self.size[1]) {
            for x in cells(min.x, max.x, self.size[0]) {
                if x < 0 || y < 0 {
                    continue;
                }
                let i = self.cell_instance[(y as u32 * self.size[0] + x as u32) as usize];
                if i != u32::MAX && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Whether collider `c` is one the bake of instance `inst` was made with.
    fn baked_with(&self, inst: u32, c: &shards_motion::ColliderInfo) -> bool {
        let instance = &self.instances[inst as usize];
        // The bake's frame has its north-west corner at the origin; the tile's, its center.
        let half = Vec3::new(instance.nav.size[0] as f32, 0.0, instance.nav.size[1] as f32) * self.cell * 0.5;
        let turned = rotation(instance.turns);
        instance.nav.colliders.iter().any(|b| {
            let pos = instance.center + turn(b.pos - half, instance.turns);
            let rot = turned * b.rot;
            same_shape(&b.shape, &c.shape) && pos.distance(c.position) < 2e-3 && rot.dot(c.rotation).abs() > 0.99999
        })
    }

    /// Whether the baked body still stands at `node` in the world as it is.
    fn still_stands(&self, terrain: &Terrain, node: NodeId) -> bool {
        let pos = self.node_pos(node);
        match terrain.probe(pos.x, pos.z, pos.y) {
            Probe::Floor { y, .. } => !self.is_swim(node) && (y - pos.y).abs() < 0.13,
            Probe::Water { surface } => self.is_swim(node) && (terrain.swim_height(surface) - pos.y).abs() < 0.13,
            _ => false,
        }
    }

    /// The nodes collider `c` may switch off: in the instances near it whose bakes did not
    /// have it, in the columns within reach of its box (clipped to each instance), at
    /// heights where it would be in the baked body's way (above its step, below its head).
    fn zone_of(&self, c: &ColliderInfo, tiles: &BTreeMap<ColliderId, TileId>, settings: &NavSettings, reach: f32) -> Vec<NodeId> {
        #[expect(clippy::disallowed_methods, reason = "carving tracks the colliders only; a voxel field is never carved (it does not change after it is built)")]
        let (min, max) = c.bounds();
        // The levels' chunks are all baked from the same solids: none of them is news to any.
        if self.levels.as_ref().is_some_and(|bake| bake.has(c.id)) {
            return Vec::new();
        }
        let owner = tiles.get(&c.id).and_then(|t| {
            let cell = [t.x, t.y];
            self.instances.iter().position(|i| i.cell == cell).map(|i| i as u32)
        });
        let (height, step) = (settings.agent.height, settings.agent.step_height);
        let lo = self.column_of(min - Vec3::new(reach, 0.0, reach));
        let hi = self.column_of(max + Vec3::new(reach, 0.0, reach));
        let mut zone = Vec::new();
        for inst in self.instances_near(min, max, reach) {
            if Some(inst) == owner && self.baked_with(inst, c) {
                continue;
            }
            let instance = &self.instances[inst as usize];
            let end = [instance.origin[0] + instance.grid[0] as i32 - 1, instance.origin[1] + instance.grid[1] as i32 - 1];
            for gz in lo[1].max(instance.origin[1])..=hi[1].min(end[1]) {
                for gx in lo[0].max(instance.origin[0])..=hi[0].min(end[0]) {
                    if self.instance_at([gx, gz]) != Some(inst) {
                        continue;
                    }
                    let (i, j) = instance.to_local([gx, gz]);
                    for n in instance.nav.column(i, j) {
                        let y = instance.nav.nodes[n as usize].y();
                        // Below the step it is never in the body's way; far above, neither.
                        if max.y > y + step && min.y <= y + height + 0.1 {
                            zone.push(self.global(inst, n));
                        }
                    }
                }
            }
        }
        zone
    }

    /// The global grid columns (row-major flags) within `reach` of any of `boxes`, clipped
    /// to the region.
    fn mark_columns(&self, boxes: &[(Vec3, Vec3)], reach: f32) -> Vec<bool> {
        let (w, h) = ((self.per * self.size[0]) as i32, (self.per * self.size[1]) as i32);
        let mut marked = vec![false; (w * h) as usize];
        for (min, max) in boxes {
            let lo = self.column_of(*min - Vec3::new(reach, 0.0, reach));
            let hi = self.column_of(*max + Vec3::new(reach, 0.0, reach));
            for gz in lo[1].max(0)..=hi[1].min(h - 1) {
                for gx in lo[0].max(0)..=hi[0].min(w - 1) {
                    marked[(gz * w + gx) as usize] = true;
                }
            }
        }
        marked
    }

    /// Brings carving (and, given the region's tile graph, the links across tile edges) up
    /// to date with the static solids and water in `physics`: what spawned, went or moved
    /// since the last look leaves its zone or enters it, and every node and edge link near
    /// such a change is decided again from the world as it is (see [`Carve`]). Whether
    /// anything changed.
    fn update(&mut self, physics: &PhysicsWorld, settings: &NavSettings, tiles: &BTreeMap<ColliderId, TileId>, edges: Option<Edges>) -> bool {
        let count = self.node_count() as usize;
        if self.carve.held.len() != count {
            self.carve.held = vec![0; count];
        }
        // What changed, by id: both lists are in id order.
        let mut before = std::mem::take(&mut self.carve.seen).into_iter().peekable();
        let mut seen: Vec<Seen> = Vec::new();
        let mut gone: Vec<Seen> = Vec::new();
        let mut came: Vec<&ColliderInfo> = Vec::new();
        for c in physics.colliders().filter(|c| tracked(c)) {
            while let Some(old) = before.next_if(|s| s.id < c.id) {
                gone.push(old);
            }
            match before.next_if(|s| s.id == c.id) {
                Some(old) if old.is(c) => seen.push(old),
                Some(old) => {
                    gone.push(old);
                    came.push(c);
                }
                None => came.push(c),
            }
        }
        gone.extend(before);
        if gone.is_empty() && came.is_empty() {
            self.carve.seen = seen;
            return false;
        }
        // A solid the levels' chunks were baked from is no longer where it was baked.
        if let Some(bake) = &mut self.levels
            && gone.iter().any(|old| bake.has(old.id))
        {
            bake.mark_stale();
        }
        let reach = settings.agent.radius + self.cell * 0.5 + 0.05;
        let mut boxes: Vec<(Vec3, Vec3)> = Vec::with_capacity(gone.len() + came.len());
        for old in &gone {
            for &n in &old.zone {
                self.carve.held[n as usize] -= 1;
            }
            boxes.push((old.aabb_min, old.aabb_max));
        }
        for c in came {
            let zone = if solid(c) { self.zone_of(c, tiles, settings, reach) } else { Vec::new() };
            for &n in &zone {
                self.carve.held[n as usize] += 1;
            }
            #[expect(clippy::disallowed_methods, reason = "carving tracks the colliders only; a voxel field is never carved (it does not change after it is built)")]
            let (aabb_min, aabb_max) = c.bounds();
            boxes.push((aabb_min, aabb_max));
            seen.push(Seen {
                id: c.id,
                shape: c.shape,
                position: c.position,
                rotation: c.rotation,
                water: is_water(c),
                aabb_min,
                aabb_max,
                zone,
            });
        }
        seen.sort_by_key(|s| s.id);
        self.carve.seen = seen;

        // Every node near a change: switched off when a zone holds it and the body no longer
        // stands there.
        let marked = self.mark_columns(&boxes, reach);
        let terrain = terrain(physics, settings);
        let width = (self.per * self.size[0]) as i32;
        let mut changed = false;
        let mut touched: Vec<u32> = Vec::new();
        for (k, _) in marked.iter().enumerate().filter(|(_, m)| **m) {
            let col = [k as i32 % width, k as i32 / width];
            for node in self.nodes_at(col) {
                let off = self.carve.held[node as usize] > 0 && !self.still_stands(&terrain, node);
                if off == self.is_disabled(node) {
                    continue;
                }
                let (inst, n) = self.split(node);
                let instance = &mut self.instances[inst as usize];
                if instance.disabled.is_empty() {
                    instance.disabled = vec![false; instance.nav.nodes.len()];
                }
                instance.disabled[n as usize] = off;
                touched.push(inst);
                changed = true;
            }
        }
        touched.sort_unstable();
        touched.dedup();
        for inst in touched {
            // None switched off: the bake's own numbers serve again.
            let instance = &mut self.instances[inst as usize];
            if !instance.disabled.contains(&true) {
                instance.disabled.clear();
            }
        }
        if changed {
            self.report.carved = self.instances.iter().map(|i| i.disabled.iter().filter(|d| **d).count()).sum();
        }
        if let Some(edges) = edges {
            changed |= self.restitch(&terrain, settings, edges, &marked);
        }
        changed
    }

    /// Every row across the region's tile edges: the facing columns (west/north side first),
    /// the step from the first to the second, whether the sockets leave that edge open, and
    /// which edge it is.
    fn edge_rows(&self, edges: Edges) -> Vec<EdgeRow> {
        let per = self.per as i32;
        let mut out = Vec::new();
        let mut edge = 0;
        for y in 0..self.size[1] as i32 {
            for x in 0..self.size[0] as i32 {
                for dir in [Dir::E, Dir::S] {
                    let (dx, dz) = dir.offset();
                    let (nx, ny) = (x + dx, y + dz);
                    if nx >= self.size[0] as i32 || ny >= self.size[1] as i32 {
                        continue;
                    }
                    let a = self.cell_instance[(y as u32 * self.size[0] + x as u32) as usize];
                    let b = self.cell_instance[(ny as u32 * self.size[0] + nx as u32) as usize];
                    if a == u32::MAX || b == u32::MAX || a == b {
                        continue;
                    }
                    let open = edges.open([x, y], dir);
                    for k in 0..per {
                        let ca = if dir == Dir::E { [(x + 1) * per - 1, y * per + k] } else { [x * per + k, (y + 1) * per - 1] };
                        out.push(EdgeRow { a: ca, b: [ca[0] + dx, ca[1] + dz], step: [dx, dz], open, edge });
                    }
                    edge += 1;
                }
            }
        }
        out
    }

    /// Links across every tile edge the sockets leave open, made in the world as it is: each
    /// boundary node gets the move the motion planner would make into the facing column, and
    /// the two-cell hop over it where the body does not fit there (a ledge or a drop whose
    /// face lies on the edge), both ways. A link to or from a switched-off node stays, unused
    /// while it is off.
    fn stitch(&mut self, terrain: &Terrain, settings: &NavSettings, edges: Edges) {
        let mut extras = Vec::new();
        let mut closed: Vec<usize> = Vec::new();
        for row in self.edge_rows(edges) {
            if !row.open {
                // Counted, for the report: both sides have ground, the sockets close it.
                if !self.nodes_at(row.a).is_empty() && !self.nodes_at(row.b).is_empty() && closed.last() != Some(&row.edge) {
                    closed.push(row.edge);
                }
                continue;
            }
            for (from, via, to) in row.moves(edges.diagonals()) {
                self.link_columns(terrain, settings, from, via, to, &mut extras);
            }
        }
        // A corner where four chunks meet is faced by two rows (the east and the south edge of
        // one chunk): each diagonal across it is made twice, and kept once.
        extras.sort_by_key(|e| (e.from, e.to));
        extras.dedup_by_key(|e| (e.from, e.to));
        self.report.closed_edges = closed.len();
        self.extras.extend(extras);
        self.extras.sort_by_key(|e| (e.from, e.to));
        self.report.stitched = self.extras.iter().filter(|e| !e.navlink).count();
    }

    /// The links across open tile edges near `marked` columns, made again in the world as it
    /// is; whether any changed.
    fn restitch(&mut self, terrain: &Terrain, settings: &NavSettings, edges: Edges, marked: &[bool]) -> bool {
        let width = (self.per * self.size[0]) as i32;
        let height = (self.per * self.size[1]) as i32;
        let is_marked = |[x, z]: [i32; 2]| (0..width).contains(&x) && (0..height).contains(&z) && marked[(z * width + x) as usize];
        let rows: Vec<EdgeRow> = self.edge_rows(edges).into_iter().filter(|r| r.open && r.touched(edges.diagonals()).into_iter().any(is_marked)).collect();
        if rows.is_empty() {
            return false;
        }
        let mut made = Vec::new();
        let mut pairs: Vec<([i32; 2], [i32; 2])> = Vec::new();
        for row in &rows {
            for (from, via, to) in row.moves(edges.diagonals()) {
                self.link_columns(terrain, settings, from, via, to, &mut made);
                pairs.push((from, to));
            }
        }
        made.sort_by_key(|e| (e.from, e.to));
        made.dedup_by_key(|e| (e.from, e.to));
        pairs.sort_unstable();
        let in_rows = |nav: &NavRegion, e: &Extra| !e.navlink && pairs.binary_search(&(nav.node_column(e.from), nav.node_column(e.to))).is_ok();
        let old: Vec<Extra> = self.extras.iter().filter(|e| in_rows(self, e)).copied().collect();
        if old == made {
            return false;
        }
        let kept: Vec<Extra> = self.extras.iter().filter(|e| !in_rows(self, e)).copied().collect();
        self.extras = kept;
        self.extras.extend(made);
        self.extras.sort_by_key(|e| (e.from, e.to));
        self.report.stitched = self.extras.iter().filter(|e| !e.navlink).count();
        true
    }

    /// Whether the body at `stance` makes a move into column `col` (one step, never mind where
    /// it lands): what `Via::Diagonal` asks of both columns it would cut across.
    fn can_step(&self, terrain: &Terrain, settings: &NavSettings, stance: Stance, col: [i32; 2]) -> bool {
        let c = self.column_center(col);
        let dist = Vec2::new(c.x - stance.pos.x, c.z - stance.pos.z).length();
        let probe = terrain.probe(c.x, c.z, stance.pos.y);
        step_onto(terrain, stance, (c.x, c.z), probe, dist, settings.safe_drop).is_some()
    }

    /// The moves from the nodes of column `from` onto column `to`: the next column; with a
    /// `Via::Hop`, a two-cell hop over a column where the body does not fit (only a climb, a
    /// drop or a jump into water, as the bake hops); with `Via::Diagonal`, a diagonal step
    /// that cuts no corner. How many.
    fn link_columns(&self, terrain: &Terrain, settings: &NavSettings, from: [i32; 2], via: Via, to: [i32; 2], out: &mut Vec<Extra>) -> usize {
        let targets = self.nodes_at(to);
        if targets.is_empty() {
            return 0;
        }
        let c = self.column_center(to);
        let mut made = 0;
        for a in self.nodes_at(from) {
            let (inst, n) = self.split(a);
            let local = self.instances[inst as usize].nav.nodes[n as usize];
            let pos = self.node_pos(a);
            let stance = Stance { pos, swimming: local.swim, up: local.up() };
            let dist = match via {
                Via::Step => self.cell,
                Via::Hop(o) => {
                    let o = self.column_center(o);
                    if !matches!(terrain.probe(o.x, o.z, pos.y), Probe::Blocked(Block::Tight)) {
                        continue;
                    }
                    2.0 * self.cell
                }
                Via::Diagonal => {
                    // As the bake moves diagonally: both columns it would cut across are steps.
                    if !self.can_step(terrain, settings, stance, [to[0], from[1]]) || !self.can_step(terrain, settings, stance, [from[0], to[1]]) {
                        continue;
                    }
                    Vec2::new(c.x - pos.x, c.z - pos.z).length()
                }
            };
            let probe = terrain.probe(c.x, c.z, pos.y);
            let Some(step) = step_onto(terrain, stance, (c.x, c.z), probe, dist, settings.safe_drop) else { continue };
            if matches!(via, Via::Hop(_)) && !hop_allowed(step.kind, stance.swimming) {
                continue;
            }
            let found = targets
                .clone()
                .filter(|b| self.is_swim(*b) == step.swimming)
                .map(|b| (b, (self.node_pos(b).y - step.pos.y).abs()))
                .filter(|(_, dy)| *dy < 0.13)
                .min_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
            if let Some((b, _)) = found {
                out.push(Extra { from: a, to: b, kind: step.kind, cost: step.cost, rise: step.rise, navlink: false });
                made += 1;
            }
        }
        made
    }

    /// Whether `e` is a stitch that is not a step into the facing column: a two-cell hop across a
    /// tile edge, or a diagonal step across a chunk seam (a corner where chunks meet). It has no
    /// portal pair to join: the graph gets its two ends as nodes (as for a navlink).
    fn is_hop(&self, e: &Extra) -> bool {
        let (a, b) = (self.node_column(e.from), self.node_column(e.to));
        !e.navlink && matches!(((a[0] - b[0]).abs(), (a[1] - b[1]).abs()), (2, 0) | (0, 2) | (1, 1))
    }

    /// The node nearest `pos` within `reach` meters (flat), preferring its own height
    /// (within `band` meters of it); switched-off nodes left out.
    pub fn nearest_node(&self, pos: Vec3, reach: f32, band: f32) -> Option<NodeId> {
        self.nearest(pos, reach, band, false)
    }

    /// [`nearest_node`](Self::nearest_node), switched-off nodes too with `any`.
    fn nearest(&self, pos: Vec3, reach: f32, band: f32, any: bool) -> Option<NodeId> {
        let col = self.column_of(pos);
        let rings = (reach / self.cell).ceil() as i32 + 1;
        let mut best: Option<(f32, NodeId)> = None;
        for dz in -rings..=rings {
            for dx in -rings..=rings {
                for node in self.nodes_at([col[0] + dx, col[1] + dz]) {
                    if !any && self.is_disabled(node) {
                        continue;
                    }
                    let at = self.node_pos(node);
                    let flat = Vec2::new(at.x - pos.x, at.z - pos.z).length();
                    let dy = (at.y - pos.y).abs();
                    if flat > reach || dy > band {
                        continue;
                    }
                    let score = flat + dy * 0.5;
                    if best.is_none_or(|(s, n)| score < s || (score == s && node < n)) {
                        best = Some((score, node));
                    }
                }
            }
        }
        best.map(|(_, n)| n)
    }

    /// Navlink markers as one-way links between the nodes at their two ends (carved or not:
    /// a switched-off end is left out while it is off), when the body can make the move: a
    /// drop of any depth, a climb within its mantle height onto a ledge next to it, a walk
    /// to the next column. The others are reported unused, with why.
    fn add_navlinks(&mut self, settings: &NavSettings, markers: Vec<(String, Vec3, Vec3)>) {
        let reach = (settings.agent.radius * 2.0 + self.cell).max(1.0);
        let step = settings.agent.step_height;
        for (name, from, to) in markers {
            let ends = self.nearest(from, reach, 1.5, true).zip(self.nearest(to, reach, 1.5, true)).filter(|(a, b)| a != b);
            let Some((a, b)) = ends else {
                let why = "an end is not on walkable ground (within reach of a node of the navmesh)".to_owned();
                self.navlinks.push(NavLinkUse { name, from, to, nodes: None, unused: Some(why) });
                continue;
            };
            let (pa, pb) = (self.node_pos(a), self.node_pos(b));
            let dy = pb.y - pa.y;
            let flat = Vec2::new(pb.x - pa.x, pb.z - pa.z).length();
            let (kind, unused) = if dy < -step {
                (StepKind::Drop, None)
            } else if dy > step {
                let why = if !settings.agent.can_climb(dy, false) {
                    Some(format!("a climb of {dy:.1} m is more than the body's mantle height ({:.1} m)", settings.agent.mantle_height))
                } else if flat > reach {
                    Some(format!("a climb onto a ledge {flat:.1} m away: the body only mantles what is in front of it"))
                } else {
                    None
                };
                (StepKind::Mantle, why)
            } else if flat > self.cell * 1.5 + 1e-3 {
                let why = format!("a level jump of {flat:.1} m: the body has no jump; make it a drop or a climb, or leave it out");
                (StepKind::Walk, Some(why))
            } else {
                (StepKind::Walk, None)
            };
            if unused.is_none() {
                let rise = if kind == StepKind::Walk { 0.0 } else { dy.abs() };
                let cost = link_cost(kind, flat.max(self.cell), rise);
                self.extras.push(Extra { from: a, to: b, kind, cost, rise, navlink: true });
            }
            let nodes = unused.is_none().then_some((a, b));
            self.navlinks.push(NavLinkUse { name, from, to, nodes, unused });
        }
        self.extras.sort_by_key(|e| (e.from, e.to));
    }

    /// The tile graph and the areas that reach each other, from the instances, their
    /// switched-off nodes and the links between tiles.
    pub(crate) fn build_graph(&mut self) {
        let mut graph = TileGraph { of_instance: vec![Vec::new(); self.instances.len()], ..default() };
        // Portals of each instance.
        for (inst, instance) in self.instances.iter().enumerate() {
            for (p, portal) in instance.nav.portals.iter().enumerate() {
                let node = if instance.is_disabled(portal.rep) {
                    let [nx, nz] = instance.nav.size;
                    portal
                        .nodes
                        .iter()
                        .filter(|n| !instance.is_disabled(**n))
                        .min_by_key(|n| {
                            let node = &instance.nav.nodes[**n as usize];
                            let (along, len) = if matches!(portal.side, Dir::N | Dir::S) { (node.i as u32, nx) } else { (node.j as u32, nz) };
                            ((2 * along + 1).abs_diff(len), **n)
                        })
                        .copied()
                } else {
                    Some(portal.rep)
                };
                graph.of_instance[inst].push(graph.nodes.len() as u32);
                graph.nodes.push(GraphNode { inst: inst as u32, node: node.map(|n| self.base[inst] + n), portal: Some(p as u32) });
            }
        }
        // Ends of navlinks, of hops across tile edges (they start or land off the edge) and of
        // diagonal steps across a chunk seam (they join corners, not the portals of an edge).
        let mut ends: Vec<NodeId> = self.extras.iter().filter(|e| e.navlink || self.is_hop(e)).flat_map(|e| [e.from, e.to]).collect();
        ends.sort_unstable();
        ends.dedup();
        let mut end_index: BTreeMap<NodeId, u32> = BTreeMap::new();
        for node in ends {
            let (inst, _) = self.split(node);
            end_index.insert(node, graph.nodes.len() as u32);
            graph.of_instance[inst as usize].push(graph.nodes.len() as u32);
            graph.nodes.push(GraphNode { inst, node: Some(node), portal: None });
        }
        let mut edges: Vec<(u32, u32, u32)> = Vec::new();
        // Inside each instance: the bake's costs, or measured again where it is carved or has
        // navlink or hop ends.
        for (inst, instance) in self.instances.iter().enumerate() {
            let keys = &graph.of_instance[inst];
            let plain = instance.disabled.is_empty() && keys.len() == instance.nav.portals.len();
            for &a in keys {
                let Some(from) = graph.nodes[a as usize].node else { continue };
                let dist = if plain { None } else { Some(dijkstra(&instance.nav, from - self.base[inst], Some(&instance.disabled))) };
                for &b in keys {
                    let Some(to) = graph.nodes[b as usize].node.filter(|_| a != b) else { continue };
                    let cost = match (&dist, graph.nodes[a as usize].portal, graph.nodes[b as usize].portal) {
                        (None, Some(p), Some(q)) => instance.nav.portal_cost(p as usize, q as usize),
                        (Some(dist), ..) => Some(dist[(to - self.base[inst]) as usize]).filter(|c| *c != u32::MAX),
                        _ => None,
                    };
                    if let Some(cost) = cost {
                        edges.push((a, b, cost));
                    }
                }
            }
        }
        // Across tiles: a stitch joins the portals of the two sides; a navlink, a hop or a diagonal its
        // two ends.
        for extra in &self.extras {
            let pair = if extra.navlink || self.is_hop(extra) {
                end_index.get(&extra.from).copied().zip(end_index.get(&extra.to).copied())
            } else {
                self.portal_node(&graph, extra.from, extra.to).zip(self.portal_node(&graph, extra.to, extra.from))
            };
            if let Some((a, b)) = pair {
                edges.push((a, b, extra.cost));
            }
        }
        edges.sort_unstable();
        edges.dedup_by_key(|e| (e.0, e.1));
        graph.first = vec![0; graph.nodes.len() + 1];
        for &(a, ..) in &edges {
            graph.first[a as usize + 1] += 1;
        }
        for n in 0..graph.nodes.len() {
            graph.first[n + 1] += graph.first[n];
        }
        graph.edges = edges.into_iter().map(|(_, b, c)| (b, c)).collect();
        self.graph = graph;
        self.find_components();
    }

    /// The graph node of the portal `node` is in, on the side facing `toward`.
    fn portal_node(&self, graph: &TileGraph, node: NodeId, toward: NodeId) -> Option<u32> {
        let (inst, n) = self.split(node);
        let instance = &self.instances[inst as usize];
        let (a, b) = (self.node_column(node), self.node_column(toward));
        let world_side = match (b[0] - a[0], b[1] - a[1]) {
            (0, -1) => Dir::N,
            (1, 0) => Dir::E,
            (0, 1) => Dir::S,
            (-1, 0) => Dir::W,
            _ => return None,
        };
        let area = instance.nav.nodes[n as usize].area;
        let p = instance.nav.portal(instance.local_side(world_side), area)?;
        graph.of_instance[inst as usize].get(p).copied()
    }

    /// Labels nodes that reach each other (links taken both ways, switched-off nodes left
    /// out): an uncarved tile by its bake's areas, a carved one link by link.
    fn find_components(&mut self) {
        let total = self.node_count() as usize;
        let mut parent: Vec<u32> = (0..total as u32).collect();
        fn find(parent: &mut [u32], mut n: u32) -> u32 {
            while parent[n as usize] != n {
                parent[n as usize] = parent[parent[n as usize] as usize];
                n = parent[n as usize];
            }
            n
        }
        fn union(parent: &mut [u32], a: u32, b: u32) {
            let (ra, rb) = (find(parent, a), find(parent, b));
            if ra != rb {
                parent[ra.max(rb) as usize] = ra.min(rb);
            }
        }
        for (inst, instance) in self.instances.iter().enumerate() {
            let base = self.base[inst];
            if instance.disabled.is_empty() {
                let mut first = vec![u32::MAX; instance.nav.areas as usize];
                for (n, node) in instance.nav.nodes.iter().enumerate() {
                    match first.get(node.area as usize).copied() {
                        Some(u32::MAX) => first[node.area as usize] = base + n as u32,
                        Some(f) => union(&mut parent, f, base + n as u32),
                        None => {}
                    }
                }
                continue;
            }
            for n in 0..instance.nav.nodes.len() as u32 {
                if instance.is_disabled(n) {
                    continue;
                }
                for link in instance.nav.links_of(n) {
                    if !instance.is_disabled(link.to) {
                        union(&mut parent, base + n, base + link.to);
                    }
                }
            }
        }
        for extra in &self.extras {
            if !self.is_disabled(extra.from) && !self.is_disabled(extra.to) {
                union(&mut parent, extra.from, extra.to);
            }
        }
        self.components = (0..total as u32).map(|n| find(&mut parent, n)).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::tile::NavNode;

    fn instance(turns: u8, size: [u32; 2]) -> Instance {
        let nav = NavTile {
            kind: TileKind::Tile,
            id: "t".into(),
            hash: String::new(),
            tile_size: 4.0,
            cell: 0.5,
            size,
            nodes: vec![NavNode { i: 0, j: 0, y_mm: 0, up_q: 10000, swim: false, area: 0 }],
            first_link: vec![0, 0],
            links: Vec::new(),
            columns: Vec::new(),
            areas: 1,
            portals: Vec::new(),
            portal_costs: Vec::new(),
            colliders: Vec::new(),
        };
        let grid = if turns % 2 == 1 { [size[1], size[0]] } else { size };
        Instance { cell: [0, 0], tile: "t".into(), kind: TileKind::Tile, turns, nav: Arc::new(nav), origin: [10, 20], grid, center: Vec3::ZERO, disabled: Vec::new() }
    }

    #[test]
    fn columns_and_directions_turn_with_the_tile_and_back() {
        for turns in 0..4 {
            let inst = instance(turns, [8, 6]);
            for (i, j) in [(0, 0), (7, 0), (3, 5), (7, 5)] {
                let g = inst.to_global(i, j);
                assert_eq!(inst.to_local(g), (i, j), "turns {turns}: ({i}, {j}) → {g:?}");
                assert!((0..inst.grid[0] as i32).contains(&(g[0] - 10)) && (0..inst.grid[1] as i32).contains(&(g[1] - 20)));
            }
            for d in MOVES {
                assert_eq!(inst.to_local_dir(inst.to_world_dir(d)), d);
            }
            // A step east in the bake is a step toward its east side's world side.
            let (dx, dz) = inst.to_world_dir((1, 0));
            assert_eq!(Dir::ALL.iter().find(|d| d.offset() == (dx, dz)).copied(), Some(inst.world_side(Dir::E)));
            for side in Dir::ALL {
                assert_eq!(inst.local_side(inst.world_side(side)), side);
            }
        }
        // One turn: the north-west corner goes to the north-east.
        let inst = instance(1, [8, 6]);
        assert_eq!(inst.to_global(0, 0), [10 + 5, 20]);
    }

    /// The navmesh of body class `body`, baked for `agent`, with nothing in it: what choosing
    /// a class looks at.
    fn class(body: &str, agent: CharacterParams) -> NavRegion {
        NavRegion {
            hash: 0,
            body: body.into(),
            bodies: Vec::new(),
            title: String::new(),
            cell: 0.5,
            tile_size: 8.0,
            origin: Vec2::ZERO,
            per: 16,
            size: [0, 0],
            agent_radius: agent.radius,
            agent_height: agent.height,
            agent,
            safe_drop: 2.0,
            instances: Vec::new(),
            cell_instance: Vec::new(),
            base: vec![0],
            extras: Vec::new(),
            navlinks: Vec::new(),
            graph: TileGraph::default(),
            components: Vec::new(),
            report: NavReport::default(),
            voxel: None,
            levels: None,
            carve: Carve::default(),
        }
    }

    /// A region's navmeshes: the first class is `agent`'s, the rest its `bodies`.
    fn classes(all: Vec<NavRegion>) -> NavRegion {
        let mut all = all.into_iter();
        let mut nav = all.next().unwrap();
        nav.bodies = all.collect();
        nav
    }

    #[test]
    fn a_class_serves_a_body_only_when_it_covers_all_of_it() {
        let agent = class("agent", CharacterParams::default());
        assert!(agent.serves(&CharacterParams::default()));
        assert!(agent.serves(&CharacterParams { radius: 0.3, height: 1.2, ..default() }), "smaller");
        assert!(agent.serves(&CharacterParams { step_height: 0.5, mantle_height: 2.0, max_slope_deg: 60.0, ..default() }), "stronger");
        let lack = |params: CharacterParams| agent.lack(&params);
        assert_eq!(lack(CharacterParams { height: 2.4, ..default() }), Some(("height", 1.8, 2.4)), "as wide, taller");
        assert_eq!(lack(CharacterParams { radius: 0.5, height: 2.4, ..default() }), Some(("radius", 0.4, 0.5)), "the radius first");
        assert_eq!(lack(CharacterParams { step_height: 0.2, ..default() }), Some(("step_height", 0.35, 0.2)));
        assert_eq!(lack(CharacterParams { mantle_height: 1.0, ..default() }), Some(("mantle_height", 1.6, 1.0)));
        assert_eq!(lack(CharacterParams { max_slope_deg: 30.0, ..default() }), Some(("max_slope_deg", 45.0, 30.0)));
        assert_eq!(lack(CharacterParams { swim_depth: 2.0, ..default() }), Some(("swim_depth", 1.3, 2.0)), "deeper or not");
        assert_eq!(lack(CharacterParams { height: 1.805, step_height: 0.345, ..default() }), None, "a centimeter of float noise");
    }

    #[test]
    fn the_class_that_serves_a_body_is_the_narrowest_then_the_lowest() {
        let nav = classes(vec![
            class("agent", CharacterParams::default()),
            class("tall", CharacterParams { height: 2.6, ..default() }),
            class("mid", CharacterParams { height: 2.2, ..default() }),
            class("large", CharacterParams { radius: 1.2, height: 2.4, ..default() }),
        ]);
        let serving = |params: CharacterParams| nav.serving(&params).map(|c| c.body.as_str());
        assert_eq!(serving(CharacterParams::default()), Some("agent"));
        // As wide as `agent` and taller: what its radius picks is not what serves it.
        let taller = CharacterParams { height: 2.0, ..default() };
        assert_eq!(nav.for_radius(taller.radius).map(|c| c.body.as_str()), Some("agent"));
        assert_eq!(serving(taller), Some("mid"), "the lowest of the classes as wide and as tall");
        assert_eq!(serving(CharacterParams { height: 2.4, ..default() }), Some("tall"));
        assert_eq!(serving(CharacterParams { radius: 0.9, height: 2.0, ..default() }), Some("large"));
        assert_eq!(serving(CharacterParams { height: 3.0, ..default() }), None, "taller than every class");
        assert_eq!(serving(CharacterParams { step_height: 0.2, ..default() }), None, "steps less than every class");
    }

    #[test]
    fn why_no_class_serves_a_body_names_what_each_class_lacks() {
        let nav = classes(vec![
            class("agent", CharacterParams::default()),
            class("large", CharacterParams { radius: 1.2, height: 2.4, step_height: 0.4, ..default() }),
        ]);
        let unserved = |params: CharacterParams| nav.unserved(&params);
        assert_eq!(
            unserved(CharacterParams { radius: 1.5, ..default() }),
            "no body class fits a 1.50 m radius (the widest: large, 1.20 m)"
        );
        assert_eq!(
            unserved(CharacterParams { height: 2.2, ..default() }),
            "no body class serves the body: agent has height 1.80 m, the body 2.20 m; large has step_height 0.40 m, the body 0.35 m"
        );
        assert_eq!(
            unserved(CharacterParams { radius: 0.9, max_slope_deg: 30.0, ..default() }),
            "no body class serves the body: agent has radius 0.40 m, the body 0.90 m; large has step_height 0.40 m, the body \
             0.35 m"
        );
        assert_eq!(
            unserved(CharacterParams { height: 2.6, step_height: 0.5, max_slope_deg: 30.0, ..default() }),
            "no body class serves the body: agent has height 1.80 m, the body 2.60 m; large has height 2.40 m, the body 2.60 m"
        );
        let nav = class("agent", CharacterParams::default());
        assert_eq!(
            nav.unserved(&CharacterParams { max_slope_deg: 30.0, ..default() }),
            "no body class serves the body: agent has max_slope_deg 45.00°, the body 30.00°"
        );
    }
}
