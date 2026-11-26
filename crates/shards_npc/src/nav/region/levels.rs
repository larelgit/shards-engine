//! The navmesh of hand-made levels: baked in memory, chunk by chunk, when a level that asks for
//! one is placed.
//!
//! A region's navmesh is made of its tiles' bakes. A game whose world is level files placed by
//! id ([`spawn_level_by_id`](shards_level::spawn_level_by_id): a chapter's rooms, a hub) has no
//! region, and its NPCs planned on the local grid: in the plane, so never up a stair to a walkway
//! over their heads. A level whose definition says `nav: true` ([`LevelDef::nav`]) gets a navmesh
//! instead, built here while no region is loaded:
//!
//! - **What is baked**: the static solids of the levels that ask (every entity they spawned that
//!   stands still on the `WORLD` layer: floors, walls, stairs, props), as they stand in the
//!   world. Left out, and so solids the bake never saw, which carve it as they carve a region's:
//!   movers (a door closes its doorway while it rests shut and gives it back when it rests
//!   open), what has [`Health`] (a crate that can be broken: the ground under it is walked the
//!   tick it goes, and nothing is baked again), and everything that is not the levels' own
//!   (other levels, what the game spawned).
//! - **Chunks**: the ground is cut into squares of [`LEVEL_CHUNK`] columns on a grid aligned to
//!   the world, and each square that has solids is one bake, sampled at its place through the
//!   same flood, move rule and assembly as a tile's ([`bake_chunk`]). A chunk is a pure function
//!   of the solids within the body's reach of it, the body class and its place: its bake is kept
//!   under that ([`ChunkKey`], in [`NavBakes`] and in a process-wide store), so a chapter played
//!   again, a checkpoint loaded or a snapshot put back bakes nothing twice, and a level placed
//!   beside the first bakes only its own squares.
//! - **Several levels** that ask are one navmesh: their chunks are stitched across every edge,
//!   as a voxel region's are, so a walk from one level into the next is a walk.
//! - **Kept up to date**: the navmesh is the one of the levels placed now and of content as it
//!   is now ([`current`]); a level placed, taken away or loaded again, or a content reload,
//!   makes it again. So does a baked solid that goes or moves (the game took a wall away): only
//!   the chunks it stood in are baked again.
//! - **The body classes are built at once**, on the compute task pool: each is a pure function
//!   of its chunks and the solids standing, and they are put back in their order.
//! - **Only machines that simulate** bake, as for a voxel region.
//!
//! Nothing is written to `assets/nav/`, and content checks ask for no bake file.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bevy::ecs::change_detection::Tick;
use bevy::tasks::{ComputeTaskPool, TaskPool};
use shards_action::Health;
use shards_level::{FromLevel, LevelDef, PlacedLevel, PlacedLevels};
use shards_motion::movers::{Mover, MoverPart};
use shards_world::manifest::fnv;

use super::super::bake::{bake_chunk, Frame};
use super::volume::{hash_of, settings_hash, shared_on, Shared};
use super::*;

/// Grid columns across a chunk of the levels' navmesh (16 m of 0.5 m cells).
pub const LEVEL_CHUNK: u32 = 32;

/// The most chunks the levels' grid may have: more is a level with a solid far away from the
/// rest (a floor a kilometer wide), which is not baked.
const MAX_CHUNKS: u32 = 16_384;

/// [`ChunkKey::region`] of a levels' chunk: not a region's id (those are content ids).
const LEVELS: &str = "(levels)";

/// The levels' chunks the process keeps: a game's levels are some hundreds of chunks a body
/// class, and the tests of a game place them again and again.
static SHARED: Shared = Shared::new(4096);

pub(super) fn clear_shared() {
    SHARED.clear();
}

/// How the levels' navmesh was made, and what it is the navmesh of.
#[derive(Clone, Debug)]
pub struct LevelBake {
    /// The content ids of the levels it is baked from, in the order they were placed.
    pub levels: Vec<String>,
    /// How the chunks were baked: the loaded machine's own timing, not state.
    pub bake: VoxelBake,
    /// Every level placed when it was built (the ones that do not ask too).
    placed: Vec<PlacedLevel>,
    /// Content when it was built.
    content: Tick,
    /// The colliders it is baked from, by id.
    baked: Vec<ColliderId>,
    /// A baked solid went or moved since: the chunks are baked again at the next refresh.
    stale: bool,
}

impl LevelBake {
    /// Whether collider `id` is one the chunks were baked from.
    pub(super) fn has(&self, id: ColliderId) -> bool {
        self.baked.binary_search(&id).is_ok()
    }

    pub(super) fn mark_stale(&mut self) {
        self.stale = true;
    }
}

/// There is no navmesh to build for the levels placed, with content as it was: no level asks, the
/// machine does not simulate, or it could not be built. Not tried again every tick.
#[derive(Resource, Clone, Debug)]
pub(super) struct NoLevelNav {
    placed: Vec<PlacedLevel>,
    content: Tick,
    simulating: bool,
}

/// The levels placed by id and standing now.
fn placed(world: &World) -> &[PlacedLevel] {
    world.get_resource::<PlacedLevels>().map_or(&[], |p| p.levels.as_slice())
}

/// The placed levels whose definition asks for a navmesh, in the order they were placed.
fn asking(world: &World) -> Vec<PlacedLevel> {
    let Some(db) = world.get_resource::<ContentDb>() else { return Vec::new() };
    placed(world).iter().filter(|l| db.get::<LevelDef>(&l.id).is_some_and(|d| d.nav)).cloned().collect()
}

/// Whether `nav` is the navmesh of the levels standing now, with content as it is now, and no
/// baked solid has gone or moved since.
pub(crate) fn current(world: &World, nav: &NavRegion) -> bool {
    let Some(bake) = &nav.levels else { return false };
    bake.placed == placed(world) && bake.content == content_changed(world) && !nav.classes().any(|c| c.levels.as_ref().is_none_or(|b| b.stale))
}

/// Whether it is known that the levels standing now have no navmesh to build.
pub(super) fn settled(world: &World) -> bool {
    world.get_resource::<NoLevelNav>().is_some_and(|none| {
        none.placed == placed(world) && none.content == content_changed(world) && none.simulating == super::super::simulates(world)
    })
}

/// A baked solid: what a chunk's key and its bake are made from.
struct Solid {
    id: ColliderId,
    /// `shape position rotation`, as its hash reads it.
    text: String,
    min: Vec3,
    max: Vec3,
}

/// What a bake of the levels in the world is made from.
struct Source {
    /// The levels that ask, in the order placed.
    levels: Vec<String>,
    /// Their static solids, movers and what can be destroyed left out: all a chunk bake sees.
    baked: PhysicsWorld,
    /// The same, in id order.
    solids: Vec<Solid>,
}

impl Source {
    fn of(world: &mut World, asking: &[PlacedLevel]) -> Result<Source, String> {
        let _ = world.run_system_cached(sync_physics);
        // Membership only: the order of the entities decides nothing.
        let mut q = world.query_filtered::<(Entity, &FromLevel), (Without<Mover>, Without<MoverPart>, Without<Health>)>();
        let mut own: Vec<Entity> = q.iter(world).filter(|(_, from)| asking.iter().any(|l| l.instance == from.instance)).map(|(e, _)| e).collect();
        own.sort_unstable();
        let baked = world.resource::<PhysicsWorld>().subset(|c| solid(c) && own.binary_search(&c.entity).is_ok(), |_| false);
        let solids: Vec<Solid> = baked
            .colliders()
            .map(|c| {
                #[expect(clippy::disallowed_methods, reason = "the levels' bake is made of colliders only: a voxel field is a region's")]
                let (min, max) = c.bounds();
                Solid { id: c.id, text: format!("{:?} {:?} {:?}\n", c.shape, c.position, c.rotation), min, max }
            })
            .collect();
        let levels: Vec<String> = asking.iter().map(|l| l.id.clone()).collect();
        if solids.is_empty() {
            return Err(format!("{} has no static solid on the WORLD layer", levels.join(", ")));
        }
        Ok(Source { levels, baked, solids })
    }
}

/// How far from a chunk a solid can be and still be met by the body baked in it: its radius, and
/// room to spare.
fn reach_of(settings: &NavSettings) -> f32 {
    settings.agent.radius + settings.cell
}

/// One body class's chunks: where the grid is, and every chunk that has solids.
struct Plan {
    /// Chunks across (x, z).
    size: [u32; 2],
    /// A chunk's edge, m.
    tile_size: f32,
    /// The grid's north-west corner (x, z): a whole number of chunks from the world's origin.
    origin: Vec2,
    /// Row-major (z outer, x inner): the chunk, the key of its bake, and the solids within the
    /// body's reach of it (indices into [`Source::solids`]).
    chunks: Vec<([u32; 2], ChunkKey, Vec<u32>)>,
}

impl Plan {
    fn of(source: &Source, settings: &NavSettings) -> Result<Plan, String> {
        let tile_size = LEVEL_CHUNK as f32 * settings.cell;
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for s in &source.solids {
            lo = lo.min(Vec2::new(s.min.x, s.min.z));
            hi = hi.max(Vec2::new(s.max.x, s.max.z));
        }
        if !(lo.is_finite() && hi.is_finite()) {
            return Err("a solid of the levels has no finite place".to_owned());
        }
        let (first, last) = ((lo / tile_size).floor(), (hi / tile_size).floor());
        let across = (last - first + Vec2::ONE).max(Vec2::ONE);
        if across.x * across.y > MAX_CHUNKS as f32 {
            return Err(format!(
                "the levels' solids span {:.0} m by {:.0} m, more than {MAX_CHUNKS} chunks of {tile_size} m: a solid stands far away from the rest",
                hi.x - lo.x,
                hi.y - lo.y
            ));
        }
        let size = [across.x as u32, across.y as u32];
        let origin = first * tile_size;
        // The solids within reach of each chunk, in id order.
        let reach = reach_of(settings);
        let mut near: Vec<Vec<u32>> = vec![Vec::new(); (size[0] * size[1]) as usize];
        let cells = |low: f32, high: f32, from: f32, n: u32| {
            let a = ((low - reach - from) / tile_size).floor().max(0.0) as u32;
            let b = ((high + reach - from) / tile_size).floor().min(n as f32 - 1.0).max(0.0) as u32;
            a..=b
        };
        for (k, s) in source.solids.iter().enumerate() {
            for z in cells(s.min.z, s.max.z, origin.y, size[1]) {
                for x in cells(s.min.x, s.max.x, origin.x, size[0]) {
                    near[(z * size[0] + x) as usize].push(k as u32);
                }
            }
        }
        let settings_key = settings_hash(settings);
        let mut chunks = Vec::new();
        for z in 0..size[1] {
            for x in 0..size[0] {
                let members = std::mem::take(&mut near[(z * size[0] + x) as usize]);
                if members.is_empty() {
                    continue;
                }
                // A chunk's bake is a function of its solids and of where its columns are.
                let mut text = format!("origin {:?} reach {:?}\n", origin, reach);
                for &k in &members {
                    text.push_str(&source.solids[k as usize].text);
                }
                let key = ChunkKey {
                    region: LEVELS.to_owned(),
                    volume: fnv(text.as_bytes()),
                    columns: LEVEL_CHUNK,
                    chunk: [x, z],
                    body: settings.body.clone(),
                    settings: settings_key,
                };
                chunks.push(([x, z], key, members));
            }
        }
        Ok(Plan { size, tile_size, origin, chunks })
    }

    fn id(chunk: [u32; 2]) -> String {
        format!("chunk {} {}", chunk[0], chunk[1])
    }

    /// Where chunk `chunk`'s columns are in the world.
    fn frame(&self, chunk: [u32; 2]) -> Frame {
        Frame { origin: self.origin, base: [(chunk[0] * LEVEL_CHUNK) as i32, (chunk[1] * LEVEL_CHUNK) as i32] }
    }
}

/// The kept bake under `key`: this world's, then the process's.
fn kept(world: &World, shared: bool, key: &ChunkKey) -> Option<Arc<NavTile>> {
    let own = world.get_resource::<NavBakes>().and_then(|b| b.chunks.get(key)).cloned();
    own.or_else(|| if shared { SHARED.get(key) } else { None })
}

/// Whether the process's level chunks are read and written for the bakes of this world.
fn sharing(world: &World) -> bool {
    shared_on() && !world.get_resource::<NavBakes>().is_some_and(|b| b.isolated)
}

/// Builds the navmesh of the levels in the world that ask for one, of every body class, and
/// journals what it did; without such a level (or on a machine that does not simulate) there is
/// none, and NPCs plan on the local grid. `rebuild` calls it where no region is loaded, with the
/// old navmesh taken away already.
pub(super) fn rebuild(world: &mut World) {
    // It builds a part of the simulation's world, whoever asked first (a tick, a tool that only
    // looks, a snapshot being loaded): what it reads of content is the simulation's, so a replay
    // names the same definitions however its navmesh came to be built.
    let _sim = ai_core::scope::open(world);
    world.remove_resource::<NoLevelNav>();
    let none = |world: &mut World| {
        let none = NoLevelNav { placed: placed(world).to_vec(), content: content_changed(world), simulating: super::super::simulates(world) };
        world.insert_resource(none);
    };
    let asking = asking(world);
    if asking.is_empty() {
        return none(world);
    }
    let names = asking.iter().map(|l| l.id.as_str()).collect::<Vec<_>>().join(", ");
    if !super::super::simulates(world) {
        journal::emit(world, JournalKind::Event, format!("navmesh: none for the levels {names} ({})", super::super::VOXEL_NOT_SIMULATED));
        return none(world);
    }
    let classes = NavSettings::all(world.resource::<ContentDb>());
    let built = Source::of(world, &asking).and_then(|source| {
        let (plans, chunks_in) = prebake(world, &source, &classes);
        let parts: Vec<Result<Parts, String>> =
            plans.into_iter().zip(&classes).map(|((plan, baked), settings)| plan.map(|plan| gather(world, settings, &source, plan, baked))).collect();
        let common = Common { markers: navlink_markers(world), placed: placed(world).to_vec(), content: content_changed(world), chunks_in };
        // Every class at once: each is a pure function of its chunks and the solids standing.
        let physics = world.resource::<PhysicsWorld>();
        let pool = ComputeTaskPool::get_or_init(TaskPool::default);
        let (source, common) = (&source, &common);
        let mut made: Vec<(usize, Result<NavRegion, String>)> = pool.scope(|scope| {
            for (k, (parts, settings)) in parts.into_iter().zip(&classes).enumerate() {
                scope.spawn(async move { (k, parts.map(|parts| assemble(physics, settings, source, common, parts))) });
            }
        });
        made.sort_by_key(|(k, _)| *k);
        let mut made = made.into_iter().map(|(_, nav)| nav);
        install(world, &classes, "these levels", |_, _| made.next().expect("a navmesh per class"))
    });
    if let Err(why) = built {
        journal::emit(world, JournalKind::Warn, format!("navmesh: none for the levels {names}: {why}; NPCs plan on the local grid"));
        none(world);
    }
}

/// A body class's plan (or why it has none), and how many of its chunks were baked now.
type Planned = (Result<Plan, String>, usize);

/// The plan of every body class, with the chunks that are not kept baked now, on the compute task
/// pool at once (a task per chunk and class), each from the solids within reach of it alone; the
/// results are put back in order before anything is kept, so which task finished first changes
/// nothing. A chunk that cannot be baked is left out: [`gather`] says why. With each plan, how
/// many of its chunks were baked now; and how long the pool took.
fn prebake(world: &mut World, source: &Source, classes: &[NavSettings]) -> (Vec<Planned>, Duration) {
    let plans: Vec<Result<Plan, String>> = classes.iter().map(|settings| Plan::of(source, settings)).collect();
    let shared = sharing(world);
    // (class, its chunk, what the chunk sees) of every chunk that is not kept, in order.
    let mut jobs: Vec<(usize, usize, PhysicsWorld)> = Vec::new();
    let mut wanted: BTreeSet<&ChunkKey> = BTreeSet::new();
    for (k, plan) in plans.iter().enumerate() {
        let Ok(plan) = plan else { continue };
        for (n, (_, key, members)) in plan.chunks.iter().enumerate() {
            wanted.insert(key);
            if kept(world, shared, key).is_none() {
                let ids: Vec<ColliderId> = members.iter().map(|&m| source.solids[m as usize].id).collect();
                jobs.push((k, n, source.baked.subset(|c| ids.binary_search(&c.id).is_ok(), |_| false)));
            }
        }
    }
    // This world keeps the chunks of the levels it has now, not those of every level it ever had.
    if let Some(mut bakes) = world.get_resource_mut::<NavBakes>() {
        bakes.chunks.retain(|key, _| key.region != LEVELS || wanted.contains(key));
    }
    let started = Instant::now();
    let pool = ComputeTaskPool::get_or_init(TaskPool::default);
    let plans_ref = &plans;
    let mut made: Vec<(usize, Result<NavTile, String>)> = pool.scope(|scope| {
        for (j, (k, n, solids)) in jobs.iter().enumerate() {
            let (settings, plan) = (&classes[*k], plans_ref[*k].as_ref().expect("jobs are of planned classes"));
            let (chunk, key, _) = &plan.chunks[*n];
            let (id, hash, frame, tile_size) = (Plan::id(*chunk), hash_of(key), plan.frame(*chunk), plan.tile_size);
            scope.spawn(async move { (j, bake_chunk(solids, settings, &id, hash, tile_size, frame, [LEVEL_CHUNK, LEVEL_CHUNK])) });
        }
    });
    let took = started.elapsed();
    made.sort_by_key(|(j, _)| *j);
    let mut baked = vec![0usize; classes.len()];
    for (j, result) in made {
        if let Ok(tile) = result {
            let (k, n, _) = &jobs[j];
            baked[*k] += 1;
            let key = plans[*k].as_ref().expect("jobs are of planned classes").chunks[*n].1.clone();
            let tile = Arc::new(tile);
            world.get_resource_or_init::<NavBakes>().chunks.insert(key.clone(), tile.clone());
            if shared {
                SHARED.put(key, tile);
            }
        }
    }
    (plans.into_iter().zip(baked).collect(), took)
}

/// A body class's chunks in hand: what its navmesh is put together from.
struct Parts {
    plan: Plan,
    /// The bake of each chunk of the plan, in its order.
    tiles: Vec<Arc<NavTile>>,
    report: NavReport,
    /// How many of them were baked a moment ago.
    baked: usize,
}

/// What every class's navmesh is made with, read from the world once.
struct Common {
    markers: Vec<(String, Vec3, Vec3)>,
    placed: Vec<PlacedLevel>,
    content: Tick,
    /// How long the pool took over the chunks.
    chunks_in: Duration,
}

/// The chunks of `plan` that [`prebake`] kept (`baked` of them baked a moment ago). One that is not
/// kept could not be baked: it is tried once more to say why, and counts as ground nobody walks.
fn gather(world: &World, settings: &NavSettings, source: &Source, plan: Plan, baked: usize) -> Parts {
    let shared = sharing(world);
    let mut report = NavReport::default();
    let mut tiles = Vec::with_capacity(plan.chunks.len());
    for (chunk, key, members) in &plan.chunks {
        let tile = kept(world, shared, key).unwrap_or_else(|| {
            let id = Plan::id(*chunk);
            let ids: Vec<ColliderId> = members.iter().map(|&m| source.solids[m as usize].id).collect();
            let solids = source.baked.subset(|c| ids.binary_search(&c.id).is_ok(), |_| false);
            match bake_chunk(&solids, settings, &id, hash_of(key), plan.tile_size, plan.frame(*chunk), [LEVEL_CHUNK, LEVEL_CHUNK]) {
                Ok(tile) => Arc::new(tile),
                Err(why) => {
                    report.failed.push((id.clone(), why));
                    Arc::new(empty_tile(TileKind::Tile, &id, plan.tile_size, settings))
                }
            }
        });
        tiles.push(tile);
    }
    Parts { plan, tiles, report, baked }
}

/// The navmesh of the levels for the body class of `settings`, from its chunks: placed, carved by
/// the solids the bake never saw, stitched, joined by the navlinks. A pure function of what it is
/// given, on any thread.
fn assemble(physics: &PhysicsWorld, settings: &NavSettings, source: &Source, common: &Common, parts: Parts) -> NavRegion {
    let started = Instant::now();
    let Parts { plan, tiles, report, baked } = parts;
    let per = LEVEL_CHUNK;
    let (tile_size, origin, size) = (plan.tile_size, plan.origin, plan.size);

    // Instances in chunk order; a square without solids has none.
    let mut instances = Vec::with_capacity(plan.chunks.len());
    let mut base = vec![0u32];
    let mut cell_instance = vec![u32::MAX; (size[0] * size[1]) as usize];
    for (index, ((chunk, _, _), nav)) in plan.chunks.iter().zip(tiles).enumerate() {
        let [x, z] = *chunk;
        base.push(base[index] + nav.nodes.len() as u32);
        cell_instance[(z * size[0] + x) as usize] = index as u32;
        instances.push(Instance {
            cell: [x as i32, z as i32],
            tile: Plan::id(*chunk),
            kind: TileKind::Tile,
            turns: 0,
            origin: [(x * per) as i32, (z * per) as i32],
            grid: [per, per],
            center: Vec3::new(origin.x + (x as f32 + 0.5) * tile_size, 0.0, origin.y + (z as f32 + 0.5) * tile_size),
            nav,
            disabled: Vec::new(),
        });
    }
    let chunks = instances.len();
    let reused = chunks.saturating_sub(baked + report.failed.len());
    let chunks_in = common.chunks_in;
    let mut nav = NavRegion {
        hash: fnv(format!("{:?}", common.placed).as_bytes()),
        body: settings.body.clone(),
        bodies: Vec::new(),
        title: format!("levels {}", source.levels.join(", ")),
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
        levels: Some(LevelBake {
            levels: source.levels.clone(),
            bake: VoxelBake { chunks, reused, chunks_in, built_in: chunks_in, threads: 0 },
            placed: common.placed.clone(),
            content: common.content,
            baked: source.solids.iter().map(|s| s.id).collect(),
            stale: false,
        }),
        carve: Carve::default(),
    };
    // What the bake never saw (doors, what the game spawned, the other levels) carves it.
    nav.update(physics, settings, &BTreeMap::new(), None);
    nav.stitch(&terrain(physics, settings), settings, Edges::Open);
    nav.add_navlinks(settings, common.markers.clone());
    nav.build_graph();
    if let Some(bake) = &mut nav.levels {
        bake.bake.built_in = chunks_in + started.elapsed();
        bake.bake.threads = ComputeTaskPool::get_or_init(TaskPool::default).thread_num();
    }
    nav
}
