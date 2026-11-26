//! The navmesh of a voxel region ([`Layout::Volume`]): baked in memory, chunk by chunk, when the
//! region loads.
//!
//! A region of tiles reads or makes one bake per tile id and places it. A volume has no tiles:
//! its chunks (the manifest's cells, `tile_size` m across) are the bakes, each a square of the
//! grid sampled at its place in the world ([`Frame`](super::super::bake::Frame): the region's
//! corner and the chunk's first column), through the same flood, move rule and assembly as a
//! tile's. They are stitched across every chunk edge (all open: ground has no sockets) as the
//! edges of tiles are, carved by the solids placed in the region after it was made, and
//! queried as any navmesh is: the chunks are [`Instance`]s, turned by nothing.
//!
//! - **What a chunk is a function of**: the voxel field and the water volumes of the region
//!   (the sea is no chunk's: the bake covers the grid only), the body class's numbers and the
//!   chunk's place. Not the other colliders placed in the region: they carve the navmesh as they
//!   carve a tile's, and a chunk bake keeps no [`BakedCollider`](super::super::tile::BakedCollider).
//!   [`ChunkKey`] says so, and a chunk bake is kept under it in [`NavBakes`] (and in a process-wide
//!   cache, so the twins of heavy mode and snapshot loads bake nothing twice).
//! - **Parallel, ordered**: the chunks of a body class that are not kept are baked on the compute
//!   task pool, each a pure function of the field; the results are put in chunk order before
//!   anything is assembled, so the navmesh does not depend on which thread finished first.
//! - **Only machines that simulate** bake ([`rebuild`](super::rebuild)).
//! - Nothing is written to `assets/nav/`, and content checks ask for no bake file: no tile or
//!   room names a chunk.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::tasks::{ComputeTaskPool, TaskPool};
use shards_world::manifest::{compact, fnv};

use super::super::bake::{bake_chunk, Frame};
use super::super::tile::hex;
use super::*;

/// How many chunk bakes the process keeps for the next region built from the same field (64: two
/// islands of 16 chunks and two body classes).
const SHARED_SIZE: usize = 64;

/// How a volume's grid is cut into bakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// The manifest's chunks: what a region loads.
    Chunks,
    /// The whole grid as one bake (a square grid): what the chunks are measured against, in tests
    /// and in the bake-time measurement. Never what a game loads.
    Whole,
}

/// What a chunk's bake is a function of, and so what keeps it: the region, its field and water,
/// the chunk's size and place, the body class and the settings it was baked with.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChunkKey {
    /// The region definition's id.
    pub region: String,
    /// FNV of the voxel field's bits (with its size, cell and origin) and of the water volumes:
    /// the volume's hash as far as the navmesh sees it.
    pub volume: u64,
    /// Grid columns across a chunk.
    pub columns: u32,
    /// The chunk of the region's grid (x, z).
    pub chunk: [u32; 2],
    /// The body class.
    pub body: String,
    /// FNV of the nav settings the body was baked with (cell, safe drop, the body's numbers).
    pub settings: u64,
}

/// How a voxel region's navmesh was baked: not state, the loaded machine's own timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoxelBake {
    /// Chunks of the grid.
    pub chunks: usize,
    /// Chunks kept from an earlier bake of the same field (not baked now).
    pub reused: usize,
    /// The chunks baked now, wall clock: the part of the build the pool shares.
    pub chunks_in: Duration,
    /// The whole build: the chunks, stitching them, the tile graph.
    pub built_in: Duration,
    /// Workers of the compute pool.
    pub threads: usize,
}

impl fmt::Display for VoxelBake {
    /// `in memory, 16 chunks, baked in 120 ms`; with chunks kept: `..., 16 kept from an earlier bake`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe(true))
    }
}

impl VoxelBake {
    /// The bake as the tools say it. `timed: false` leaves out what differs from run to run (the
    /// wall time, and how many chunks the process had kept), so that a test that compares whole
    /// answers (`report_perf: false`) gets the same bytes.
    pub fn describe(&self, timed: bool) -> String {
        let noun = if self.chunks == 1 { "chunk" } else { "chunks" };
        let mut out = format!("in memory, {} {noun}", self.chunks);
        if timed {
            out += &format!(", baked in {} ms", self.built_in.as_millis());
            if self.reused > 0 {
                out += &format!(", {} kept from an earlier bake", self.reused);
            }
        }
        out
    }
}

/// Chunk bakes of every game in the process, newest last, up to a number: the twins of heavy mode
/// and the worlds a snapshot is loaded into have the same ground, and bake nothing again.
pub(super) struct Shared {
    kept: Mutex<Vec<(ChunkKey, Arc<NavTile>)>>,
    size: usize,
}

impl Shared {
    pub(super) const fn new(size: usize) -> Self {
        Self { kept: Mutex::new(Vec::new()), size }
    }

    fn all(&self) -> std::sync::MutexGuard<'_, Vec<(ChunkKey, Arc<NavTile>)>> {
        self.kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn get(&self, key: &ChunkKey) -> Option<Arc<NavTile>> {
        let mut all = self.all();
        let at = all.iter().position(|(k, _)| k == key)?;
        let entry = all.remove(at);
        let tile = entry.1.clone();
        all.push(entry);
        Some(tile)
    }

    pub(super) fn put(&self, key: ChunkKey, tile: Arc<NavTile>) {
        let mut all = self.all();
        all.retain(|(k, _)| *k != key);
        all.push((key, tile));
        while all.len() > self.size {
            all.remove(0);
        }
    }

    pub(super) fn clear(&self) {
        self.all().clear();
    }
}

/// The chunks of voxel regions the process keeps.
static SHARED: Shared = Shared::new(SHARED_SIZE);

/// Whether chunk bakes are kept across games of the process. `SHARDS_VOXEL_CACHE=off` (the voxel
/// module's switch, for tests and dev) turns it off.
pub(super) fn shared_on() -> bool {
    !std::env::var("SHARDS_VOXEL_CACHE").is_ok_and(|v| matches!(v.trim(), "off" | "0" | "false" | "no"))
}

fn shared_get(key: &ChunkKey) -> Option<Arc<NavTile>> {
    SHARED.get(key)
}

fn shared_put(key: ChunkKey, tile: Arc<NavTile>) {
    SHARED.put(key, tile);
}

/// Forgets every chunk bake the process keeps, a voxel region's and the levels' (tests: a cold
/// bake after a warm one).
pub fn clear_shared_bakes() {
    SHARED.clear();
    super::levels::clear_shared();
}

/// What a chunk bake sees of the physics world, and nothing else: the voxel fields, shared, and
/// the static water volumes. The solids placed in the region are not in it.
fn field_world(physics: &PhysicsWorld) -> PhysicsWorld {
    physics.subset(|c| is_water(c) && c.body == Body::Static, |f| f.layers().memberships.intersects(LayerMask::WORLD))
}

/// FNV over what `baked` holds: every field's own hash (bits, size, cell, origin) and every water
/// volume's shape and place.
fn volume_hash(baked: &PhysicsWorld) -> u64 {
    let mut text = String::new();
    for (_, _, field) in baked.fields() {
        text.push_str(&format!("field {:016x} {:?} {:?} {:?}\n", field.hash(), field.origin(), field.cell(), field.size()));
    }
    for c in baked.colliders() {
        text.push_str(&format!("water {:?} {:?} {:?}\n", c.shape, c.position, c.rotation));
    }
    fnv(text.as_bytes())
}

/// FNV over the nav settings of a body class, as a bake of a tile hashes them.
pub(super) fn settings_hash(settings: &NavSettings) -> u64 {
    let mut text = format!("cell {:?} drop {:?} body {} agent ", settings.cell, settings.safe_drop, settings.body);
    if let Ok(agent) = serde_json::to_value(&settings.agent) {
        compact(&agent, &mut text);
    }
    fnv(text.as_bytes())
}

/// Where a volume's bakes are: the grid cut into chunks (or not cut), in the world.
struct Grid {
    /// Chunks across (x, z).
    size: [u32; 2],
    /// A chunk's edge, m.
    tile_size: f32,
    /// Grid columns across a chunk.
    per: u32,
    /// The grid's north-west corner (x, z).
    origin: Vec2,
    /// Row-major: z outer, x inner.
    chunks: Vec<[u32; 2]>,
}

impl Grid {
    fn of(manifest: &shards_world::manifest::RegionManifest, settings: &NavSettings, cut: Cut) -> Result<Grid, String> {
        if manifest.layout != Layout::Volume {
            return Err(format!("{} is not a voxel volume", manifest.title()));
        }
        let (size, tile_size) = match cut {
            Cut::Chunks => (manifest.size, manifest.tile_size),
            Cut::Whole if manifest.size[0] == manifest.size[1] => ([1, 1], manifest.tile_size * manifest.size[0] as f32),
            Cut::Whole => return Err(format!("{} is not square: it cannot be baked in one piece", manifest.title())),
        };
        if !whole_cells(tile_size, settings.cell) {
            return Err(format!("its chunk size {tile_size} m is not a whole number of {} m cells (nav settings `cell`)", settings.cell));
        }
        let per = (tile_size / settings.cell).round() as u32;
        let chunks = (0..size[1]).flat_map(|z| (0..size[0]).map(move |x| [x, z])).collect();
        Ok(Grid { size, tile_size, per, origin: Vec2::from_array(manifest.origin), chunks })
    }

    fn id(chunk: [u32; 2]) -> String {
        format!("chunk {} {}", chunk[0], chunk[1])
    }

    /// Where chunk `chunk`'s columns are in the world.
    fn frame(&self, chunk: [u32; 2]) -> Frame {
        Frame { origin: self.origin, base: [(chunk[0] * self.per) as i32, (chunk[1] * self.per) as i32] }
    }
}

/// What a bake of the voxel region in the world is made from: its manifest, what the chunks see
/// of the physics world, and that world's hash.
struct Source {
    region: ActiveRegion,
    baked: PhysicsWorld,
    volume: u64,
}

impl Source {
    fn of(world: &mut World) -> Result<Source, String> {
        let region = world.get_resource::<ActiveRegion>().ok_or("no region")?.clone();
        let _ = world.run_system_cached(sync_physics);
        let baked = field_world(world.resource::<PhysicsWorld>());
        if baked.fields().next().is_none() {
            return Err("the region has no voxel field in physics".to_owned());
        }
        let volume = volume_hash(&baked);
        Ok(Source { region, baked, volume })
    }

    /// The keys of a body class's chunks, in chunk order.
    fn keys(&self, grid: &Grid, settings: &NavSettings) -> Vec<ChunkKey> {
        let settings_key = settings_hash(settings);
        grid.chunks
            .iter()
            .map(|&chunk| ChunkKey {
                region: self.region.manifest.region.clone(),
                volume: self.volume,
                columns: grid.per,
                chunk,
                body: settings.body.clone(),
                settings: settings_key,
            })
            .collect()
    }
}

/// Whether the process's chunk bakes are read and written for the bakes of this world.
pub(super) fn sharing(world: &World) -> bool {
    shared_on() && !world.get_resource::<NavBakes>().is_some_and(|b| b.isolated)
}

/// The kept bake under `key`: this world's, then the process's.
fn kept(world: &World, shared: bool, key: &ChunkKey) -> Option<Arc<NavTile>> {
    let own = world.get_resource::<NavBakes>().and_then(|b| b.chunks.get(key)).cloned();
    own.or_else(|| if shared { shared_get(key) } else { None })
}

pub(super) fn hash_of(key: &ChunkKey) -> String {
    hex(fnv(format!("{key:?}").as_bytes()))
}

/// What [`prebake_volume`] did for one body class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prebaked {
    /// Chunks of the class that were not kept and were baked now.
    pub baked: usize,
}

/// Bakes the chunks of every body class of `classes` that are not kept, on the compute task pool
/// at once (a task per chunk and class: 16 chunks of two classes are 32 tasks for the workers),
/// and keeps them. A chunk is a pure function of the field, the water and the body, and the
/// results are put back in chunk order, so which task finished first changes nothing. A chunk
/// that cannot be baked is left out: [`bake_volume`] says why, per class. How many chunks of each
/// class were baked, and how long the pool took.
pub fn prebake_volume(world: &mut World, classes: &[NavSettings], cut: Cut) -> Result<(Vec<Prebaked>, Duration), String> {
    let source = Source::of(world)?;
    let shared = sharing(world);
    let pool = ComputeTaskPool::get_or_init(TaskPool::default);
    let grids: Vec<Grid> = classes.iter().map(|s| Grid::of(&source.region.manifest, s, cut)).collect::<Result<_, _>>()?;
    // (class, chunk, key) of every chunk that is not kept, in order.
    let mut jobs: Vec<(usize, [u32; 2], ChunkKey)> = Vec::new();
    let mut current: Vec<(String, u64)> = Vec::new();
    for (k, settings) in classes.iter().enumerate() {
        for (&chunk, key) in grids[k].chunks.iter().zip(source.keys(&grids[k], settings)) {
            if !current.iter().any(|(r, v)| *r == key.region && *v == key.volume) {
                current.push((key.region.clone(), key.volume));
            }
            if kept(world, shared, &key).is_none() {
                jobs.push((k, chunk, key));
            }
        }
    }
    // Bakes of a field that is no longer the region's (another seed, changed content) are let go:
    // this world keeps the chunks of the volume it has now, not those of every one it ever had.
    if let Some(mut bakes) = world.get_resource_mut::<NavBakes>() {
        bakes.chunks.retain(|key, _| current.iter().any(|(r, v)| *r == key.region && *v == key.volume));
    }
    let started = Instant::now();
    let baked = &source.baked;
    let (classes_ref, grids_ref) = (classes, &grids);
    let mut made: Vec<(usize, Result<NavTile, String>)> = pool.scope(|scope| {
        for (n, (k, chunk, key)) in jobs.iter().enumerate() {
            let (settings, grid, chunk) = (&classes_ref[*k], &grids_ref[*k], *chunk);
            let hash = hash_of(key);
            scope.spawn(async move { (n, bake_chunk(baked, settings, &Grid::id(chunk), hash, grid.tile_size, grid.frame(chunk), [grid.per, grid.per])) });
        }
    });
    made.sort_by_key(|(n, _)| *n);
    let mut counts = vec![Prebaked::default(); classes.len()];
    for (n, result) in made {
        if let Ok(tile) = result {
            let (k, _, key) = &jobs[n];
            let tile = Arc::new(tile);
            world.get_resource_or_init::<NavBakes>().chunks.insert(key.clone(), tile.clone());
            if shared {
                shared_put(key.clone(), tile);
            }
            counts[*k].baked += 1;
        }
    }
    Ok((counts, started.elapsed()))
}

/// The navmesh of the voxel region in the world for the body class of `settings`, baked now
/// (chunks kept from an earlier bake of the same field are not baked again). Inserts nothing in
/// the world but the chunk bakes in [`NavBakes`]: `rebuild` makes it the region's navmesh.
/// `Cut::Whole` bakes the grid as one piece instead, to compare the chunks against.
pub fn bake_volume(world: &mut World, settings: &NavSettings, cut: Cut) -> Result<NavRegion, String> {
    let (prebaked, chunks_in) = prebake_volume(world, std::slice::from_ref(settings), cut)?;
    build(world, settings, cut, prebaked[0], chunks_in)
}

/// [`bake_volume`] for a class whose missing chunks [`prebake_volume`] baked (`prebaked`, in
/// `chunks_in`) a moment ago.
pub(super) fn build(world: &mut World, settings: &NavSettings, cut: Cut, prebaked: Prebaked, chunks_in: Duration) -> Result<NavRegion, String> {
    let started = Instant::now();
    let source = Source::of(world)?;
    let grid = Grid::of(&source.region.manifest, settings, cut)?;
    let manifest = &source.region.manifest;
    let shared = sharing(world);
    let keys = source.keys(&grid, settings);
    let mut report = NavReport::default();
    let mut tiles: Vec<Arc<NavTile>> = Vec::with_capacity(keys.len());
    for (&chunk, key) in grid.chunks.iter().zip(&keys) {
        if let Some(tile) = kept(world, shared, key) {
            tiles.push(tile);
            continue;
        }
        // Not kept after the prebake: it could not be baked. Say why, and go on without it.
        let id = Grid::id(chunk);
        match bake_chunk(&source.baked, settings, &id, hash_of(key), grid.tile_size, grid.frame(chunk), [grid.per, grid.per]) {
            Ok(tile) => tiles.push(Arc::new(tile)),
            Err(why) => {
                report.failed.push((id.clone(), why));
                tiles.push(Arc::new(empty_tile(TileKind::Tile, &id, grid.tile_size, settings)));
            }
        }
    }
    let reused = keys.len().saturating_sub(prebaked.baked + report.failed.len());
    let (per, tile_size, origin, size) = (grid.per, grid.tile_size, grid.origin, grid.size);

    // Instances in chunk order.
    let mut instances = Vec::with_capacity(grid.chunks.len());
    let mut base = vec![0u32];
    let mut cell_instance = Vec::with_capacity(grid.chunks.len());
    for (index, (&[x, z], nav)) in grid.chunks.iter().zip(tiles).enumerate() {
        base.push(base[index] + nav.nodes.len() as u32);
        cell_instance.push(index as u32);
        instances.push(Instance {
            cell: [x as i32, z as i32],
            tile: Grid::id([x, z]),
            kind: TileKind::Tile,
            turns: 0,
            origin: [(x * per) as i32, (z * per) as i32],
            grid: [per, per],
            center: Vec3::new(origin.x + (x as f32 + 0.5) * tile_size, 0.0, origin.y + (z as f32 + 0.5) * tile_size),
            nav,
            disabled: Vec::new(),
        });
    }
    let tile_of = tile_ids(world);
    let markers = navlink_markers(world);
    let physics = world.resource::<PhysicsWorld>();
    let mut nav = NavRegion {
        hash: source.region.hash,
        body: settings.body.clone(),
        bodies: Vec::new(),
        title: manifest.title(),
        cell: settings.cell,
        tile_size,
        origin,
        per,
        size,
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
        costs: Vec::new(),
        components: Vec::new(),
        report,
        voxel: None,
        levels: None,
        carve: Carve::default(),
    };
    // Solids placed in the region (not the field, not the sea) carve it, as they carve tiles.
    nav.update(physics, settings, &tile_of, None);
    nav.stitch(&terrain(physics, settings), settings, Edges::Open);
    nav.add_navlinks(settings, markers);
    nav.build_graph();
    let threads = ComputeTaskPool::get_or_init(TaskPool::default).thread_num();
    nav.voxel = Some(VoxelBake { chunks: grid.chunks.len(), reused, chunks_in, built_in: chunks_in + started.elapsed(), threads });
    Ok(nav)
}

/// The chunks of the voxel region in the world for `settings`, baked one after another on this
/// thread and written as bake files, in chunk order: what the pool's bakes are compared with in
/// tests (a chunk must not depend on which worker made it, or when). Keeps nothing.
pub fn chunk_files_sequential(world: &mut World, settings: &NavSettings) -> Result<Vec<String>, String> {
    let source = Source::of(world)?;
    let grid = Grid::of(&source.region.manifest, settings, Cut::Chunks)?;
    let keys = source.keys(&grid, settings);
    grid.chunks
        .iter()
        .zip(&keys)
        .map(|(&chunk, key)| {
            let id = Grid::id(chunk);
            bake_chunk(&source.baked, settings, &id, hash_of(key), grid.tile_size, grid.frame(chunk), [grid.per, grid.per]).map(|t| t.to_file())
        })
        .collect()
}
