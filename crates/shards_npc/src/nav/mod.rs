//! Navigation: the navmesh of the region (E6.1) and the nav agents that walk it.
//!
//! ## The navmesh
//!
//! Every tile (and dungeon room) is **baked** ahead of time into a grid of walkable cells
//! ([`NavTile`]): the tile's gameplay geometry is spawned alone in a scratch world by the
//! world module's own tile spawner and sampled with the motion module's terrain probe and
//! move rule ([`step_onto`](shards_motion::perception::step_onto)), so the navmesh, the
//! local planner (`path_to`) and the character controller agree on what can be walked,
//! climbed ([`CharacterParams::can_climb`]), dropped and swum. Bakes live in
//! `<assets>/nav/<tile>.navtile.ron` (rooms `.navroom.ron`), hashed against everything they
//! are made from: `--bake-nav [tiles|all]` (the game binary, [`bake_files`]) or the
//! `nav_bake` tool writes them; with a `nav` content definition ([`NavSettingsDef`]: cell
//! size, the bodies, the folder) a missing or stale one is a content error.
//!
//! **Body classes**: the tiles are baked for the `agent` body and once more for every class
//! of the nav settings' `bodies` (`large`, from the locomotion of a big enemy), each class in
//! a folder of its own (`<assets>/nav/large/<tile>.navtile.ron`), and a region builds a
//! navmesh per class. A route asks the tightest class that serves the whole body (as wide, as
//! tall, stepping, climbing and swimming as it: [`NavRegion::serving`]); a body no class
//! serves plans on the local grid with its own numbers (`npc_state` says why).
//!
//! When a region loads, its tiles are placed and turned, **carved** where solids the bakes
//! never saw stand (decor, props, colliders spawned later), **stitched** across every tile
//! edge the sockets leave open, and joined by the levels' `navlink` markers
//! ([`NavRegion`]). A tile without a fresh bake file is baked right then, with a journal
//! warning.
//!
//! **Hand-made levels** have a navmesh too, without a region: a level whose definition says
//! `nav: true` ([`LevelDef::nav`](shards_level::LevelDef::nav)) is baked in memory when it is
//! placed by id, from its own static solids, in chunks of [`LEVEL_CHUNK`] columns stitched as
//! a voxel region's are (`region::levels`). The levels that ask are one navmesh, kept under the
//! same [`NavRegion`]; doors and what the game spawns carve it. A level that does not ask keeps
//! the local grid, which plans in the plane: an NPC there never finds the stairs to a walkway
//! over its head.
//!
//! A **route** ([`NavRegion::path`]) is A* over the tile graph for the corridor, grid A*
//! inside the corridor's tiles, then string pulling checked against the grid and the
//! physics world. Routes need no window and replay identically.
//!
//! ```ron
//! // content/nav/default.nav.ron
//! { "default": (cell: 0.5, agent: Some("grunt"), safe_drop: 2.0, bodies: {"large": (locomotion: Some("ogre"))}) }
//! ```
//!
//! ## Nav agents
//!
//! Where an NPC walks, by which way, without bumping into the others.
//!
//! - **Routes** come from [`NavPaths`], a seam: `fn(&World, from, to, body) -> waypoints`,
//!   the body being the NPC's own [`CharacterParams`]. The default asks the navmesh of the
//!   tightest body class that serves the body inside a loaded region and the motion module's
//!   local grid planner ([`plan_path`]) with that body everywhere else (and for bodies no class
//!   serves); a game may plug in its own. A goal in plain sight ([`plain_sight`]: on the
//!   navmesh of the same class a straight line of walking links, so never over a drop or a
//!   ledge; elsewhere a cast of the body, step height to head, through the `WORLD` layer over
//!   ground without holes or deep drops) needs no route,
//!   until the body strays farther than its radius from the line that was checked (pushed,
//!   knocked back, sidestepping another): then it is looked at again from where it stands.
//!   Another is planned again when the goal moves or every [`NpcTuning::repath`] seconds,
//!   which keeps route queries few however many NPCs chase. Where there is no route the NPC
//!   holds its ground until the next plan (it never walks blindly at the goal).
//! - **Following**: toward the next waypoint at the state's speed; arrived within `within`
//!   meters of the goal.
//! - **Leash**: an NPC whose state keeps it leashed never steers out of its [`Home`] circle.
//!   Following and the leash give the velocity the route asks for ([`NavAgent::preferred`]).
//! - **Avoidance** between bodies ([`avoid`], in `NpcSystems::Drive`): sampled reciprocal
//!   velocity obstacles. An NPC on its way tries a fan of velocities around the preferred one
//!   and takes the cheapest: straying from the route costs, and so does the first collision
//!   with another NPC or a player within the look-ahead; another NPC that avoids too takes
//!   half of each sidestep; to its right when it would meet one head on, so two walkers pass
//!   each other, and on the side it took until it is past; nearly at its goal behind a body
//!   that does not make way, it waits there. The numbers are content ([`AvoidanceDef`](crate::AvoidanceDef): the
//!   tuning's, or the behaviour's own); `weight: 0` turns it off. The leash holds after it
//!   too, and the body it steers around is [`NavAgent::avoiding`].
//!
//! The result is `Intent.move_dir` (and `aim`, presses, held inputs): NPCs drive their body
//! exactly as players do. `move_to path=true` follows navmesh routes too and `path_to`
//! reports them (the motion module's [`RoutePlanner`] seam), `clearance` ends a direction
//! where the navmesh does, and `nav_path` shows routes with their tiles.

pub mod avoid;
pub mod bake;
#[cfg(feature = "render")]
pub mod overlay;
pub mod query;
pub mod region;
pub mod settings;
pub mod tile;
pub(crate) mod tools;

pub use query::{NavError, NavPath, NavQuery};
pub use region::{
    bake_volume, chunk_files_sequential, clear_shared_bakes, prebake_volume, rebuild, refresh, ChunkKey, Cut, LevelBake, NavBakes, NavRegion, Prebaked,
    VoxelBake, LEVEL_CHUNK,
};
pub use settings::{NavBodyDef, NavSettings, NavSettingsDef, DEFAULT_BODY};
pub use tile::NavTile;
pub use tools::{bake_files, bake_region, BakeReport, NavOverlay};

use ai_core::{AiId, AiIndex};
use bevy::prelude::*;
use shards_action::{AimTarget, Dead, Standing};
use shards_content::ContentDb;
use shards_core::{flat, Intent};
use shards_motion::perception::{
    plan_path, Block, PathPlan, PathRequest, Probe, ReachRequest, RoutePlanner, RouteRequest, Terrain,
};
use shards_world::ActiveRegion;
use shards_motion::{
    CharacterController, CharacterParams, LayerMask, MotionAi, MotionState, MoveModifiers, PhysicsWorld, QueryFilter,
    Shape,
};

use crate::brain::{npcs_in_order, Home, Npc, NpcBrain, NpcDrive, LEASH_ROOM};
use crate::def::{BehaviorDef, NpcTuning};
use crate::sequence::SequenceHold;
use avoid::{steer, Ask, Body};

/// Pluggable routes: `find(world, from, to, body) -> waypoints` (the last one at `to`), `None`
/// when there is no way; `body` is the walker's own numbers (radius, height, step, climb,
/// slope, swim). The default ([`navmesh_path`]) asks the navmesh in a loaded region and the
/// motion module's local grid planner elsewhere.
#[derive(Resource, Clone, Copy)]
pub struct NavPaths {
    /// Who answers, for tools: `local grid`, `navmesh`.
    pub name: &'static str,
    pub find: fn(&World, Vec3, Vec3, &CharacterParams) -> Option<Vec<Vec3>>,
}

impl Default for NavPaths {
    fn default() -> Self {
        Self { name: NAVMESH, find: navmesh_path }
    }
}

/// The name of the engine's own provider ([`NavPaths::default`]).
pub const NAVMESH: &str = "navmesh";

/// What the navmesh says about a walk of a body of `params`: `None` when it does not cover
/// it (no region, a goal or start outside it, a body no class serves: wider or taller than
/// every class, or stepping, climbing or swimming less well, a walker off the ground), else
/// the route on the tightest class that serves it, or why there is none.
pub fn navmesh_route(world: &World, from: Vec3, to: Vec3, params: &CharacterParams, goal_level: bool) -> Option<Result<NavPath, String>> {
    route_on(world, serving(world, params)?, from, to, params, goal_level)
}

/// [`navmesh_route`] on the navmesh of one body class.
pub fn route_on(world: &World, nav: &NavRegion, from: Vec3, to: Vec3, params: &CharacterParams, goal_level: bool) -> Option<Result<NavPath, String>> {
    let physics = world.get_resource::<PhysicsWorld>()?;
    let query = NavQuery { from, to, goal_level, radius: params.radius, height: params.height, step_height: params.step_height };
    match nav.path(physics, &query) {
        Ok(path) => Some(Ok(path)),
        Err(NavError::NotCovered(_)) => None,
        Err(NavError::NoRoute(why)) => Some(Err(why)),
    }
}

/// The engine's routes for a body of `params`: on the navmesh of the tightest class that
/// serves it ([`navmesh_route`]) where that covers the walk, else on the local grid with the
/// same body. A body taller than the class its radius fits is never walked as that class.
///
/// Where the navmesh of hand-made levels has no route, the local grid is asked too: that navmesh
/// is a grid of cells fixed in the world, and a doorway narrower than the baked body and a cell
/// together may fall between two columns and be a gap in it, which the local grid, centred on
/// the walker and planned with its own body, walks through. (A region's navmesh has the last
/// word: its tiles are made for its cells.)
pub fn navmesh_path(world: &World, from: Vec3, to: Vec3, params: &CharacterParams) -> Option<Vec<Vec3>> {
    match navmesh_route(world, from, to, params, false) {
        Some(Ok(path)) => Some(path.points),
        Some(Err(_)) if region_nav(world).is_some_and(|nav| nav.levels.is_some()) => local_grid_path(world, from, to, params),
        Some(Err(_)) => None,
        None => local_grid_path(world, from, to, params),
    }
}

/// Why a voxel region has no navmesh on a machine that does not simulate (a co-op client): it
/// does not bake, NPCs are the host's and walk where the host's routes take them. `nav_info`,
/// `nav_path`, `npc_state` and the journal say it.
pub const VOXEL_NOT_SIMULATED: &str = "not baked: this machine does not simulate";

/// Whether this machine simulates the world ([`shards_core::NetRole::simulates`]): a single-player
/// game, a host and the tools (`--verify`, heavy mode, `shards soak`) do; a co-op client does not.
pub fn simulates(world: &World) -> bool {
    world.get_resource::<shards_core::NetRole>().is_none_or(|role| role.simulates())
}

/// Whether the region in the world is a voxel volume ([`Layout::Volume`](shards_world::manifest::Layout::Volume)),
/// whose navmesh is baked in memory, chunk by chunk, when it loads (on machines that simulate).
pub fn region_is_volume(world: &World) -> bool {
    world.get_resource::<ActiveRegion>().is_some_and(|r| r.manifest.layout == shards_world::manifest::Layout::Volume)
}

/// The navmesh of the region in the world, when it is built for that region; without a region,
/// the navmesh of the hand-made levels placed now that ask for one ([`NavRegion::levels`]).
pub fn region_nav(world: &World) -> Option<&NavRegion> {
    let nav = world.get_resource::<NavRegion>()?;
    match world.get_resource::<ActiveRegion>() {
        Some(region) => (region.hash == nav.hash && nav.levels.is_none()).then_some(nav),
        None => region::levels_current(world, nav).then_some(nav),
    }
}

/// The navmesh of the tightest body class that serves a body of `params`.
pub(crate) fn serving<'w>(world: &'w World, params: &CharacterParams) -> Option<&'w NavRegion> {
    region_nav(world)?.serving(params)
}

/// What an NPC of body `params` plans its routes on, for tools: `navmesh, body large`, or the
/// local grid and why (`local grid: no region loaded`, `local grid: no body class fits a
/// 1.60 m radius (the widest: large, 1.20 m)`, `local grid: no body class serves the body:
/// agent has height 1.80 m, the body 2.40 m`), or a game's own provider by its name. Where it
/// stands is not known here: [`routes_at`] knows it.
pub fn routes_for(world: &World, params: &CharacterParams) -> String {
    route_source(world, params, None).to_string()
}

/// [`routes_for`] an NPC standing at `at`: outside the region it plans on the local grid
/// (`local grid: the NPC stands outside the region`), whatever its class. What `npc_state`
/// says (`nav.routes`).
pub fn routes_at(world: &World, params: &CharacterParams, at: Vec3) -> String {
    route_source(world, params, Some(at)).to_string()
}

/// Where an NPC's routes come from, for tools: `npc_state` and `npc_overlay` both ask this.
pub(crate) enum RouteSource<'w> {
    /// A game's own provider ([`NavPaths`] by another name), by its name.
    Provider(&'static str),
    /// The navmesh of a body class.
    Navmesh(&'w NavRegion),
    /// The engine's local grid, and why. `region`: the region in the world has a navmesh,
    /// only not for this NPC (no class serves its body, it stands outside).
    LocalGrid { why: String, region: bool },
}

impl std::fmt::Display for RouteSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(name) => f.write_str(name),
            Self::Navmesh(class) => write!(f, "navmesh, body {}", class.body),
            Self::LocalGrid { why, .. } => write!(f, "local grid: {why}"),
        }
    }
}

/// What an NPC of body `params` (standing at `at`, when known) plans its routes on: the route
/// provider ([`NavPaths`]), then the region's navmesh as [`navmesh_path`] asks it.
pub(crate) fn route_source<'w>(world: &'w World, params: &CharacterParams, at: Option<Vec3>) -> RouteSource<'w> {
    let paths = world.get_resource::<NavPaths>().copied().unwrap_or_default();
    if paths.name != NAVMESH {
        return RouteSource::Provider(paths.name);
    }
    let grid = |why: &str, region: bool| RouteSource::LocalGrid { why: why.to_owned(), region };
    // Without a region, the levels that ask for a navmesh have one.
    if world.get_resource::<ActiveRegion>().is_none() && region_nav(world).is_none() {
        return grid("no region loaded", false);
    }
    let Some(nav) = region_nav(world) else {
        if region_is_volume(world) && !simulates(world) {
            return grid(&format!("no navmesh for this region ({VOXEL_NOT_SIMULATED})"), false);
        }
        return grid("the region has no navmesh", false);
    };
    // A start the region does not cover is the local grid's (`NavError::NotCovered`).
    if at.is_some_and(|at| nav.instance_at(nav.column_of(at)).is_none()) {
        let outside = if nav.levels.is_some() { "the NPC stands outside the levels that have a navmesh" } else { "the NPC stands outside the region" };
        return grid(outside, true);
    }
    match nav.serving(params) {
        Some(class) => RouteSource::Navmesh(class),
        None => grid(&nav.unserved(params), true),
    }
}

/// The motion module's [`RoutePlanner`]: `move_to path=true` and `path_to` on the navmesh.
pub(crate) fn route_plan(world: &World, request: &RouteRequest) -> Option<Result<PathPlan, String>> {
    let r = request.request;
    navmesh_route(world, r.start, r.goal, request.params, r.goal_level).map(|route| route.map(|path| path.to_plan()))
}

/// The motion module's [`RoutePlanner`] reach: how far `clearance` goes on the navmesh.
pub(crate) fn route_reach(world: &World, request: &ReachRequest) -> Option<f32> {
    serving(world, request.params)?.reach(request.from, request.step, request.range)
}

/// The navmesh as the motion module's route planner.
pub(crate) const ROUTE_PLANNER: RoutePlanner = RoutePlanner { name: NAVMESH, plan: route_plan, reach: route_reach };

/// The route [`NavPaths`] gives right now for a body of `params` (tools, tests).
pub fn find_path(world: &World, from: Vec3, to: Vec3, params: &CharacterParams) -> Option<Vec<Vec3>> {
    let paths = world.get_resource::<NavPaths>().copied().unwrap_or_default();
    (paths.find)(world, from, to, params)
}

/// The default route off the navmesh: `shards_motion`'s grid A* (0.5 m cells) for a body of
/// `params` (its radius and height, step height, mantle height, slope, swim depth), searching
/// up to twice the straight distance (plus 20 m).
pub fn local_grid_path(world: &World, from: Vec3, to: Vec3, params: &CharacterParams) -> Option<Vec<Vec3>> {
    local_grid_route(world, from, to, params, None)
}

/// [`local_grid_path`] for a body of its own numbers (its step height, mantle height, slope,
/// swim depth), as `move_to path=true` plans for the walker; `exclude` is the walker's own
/// body, which does not stand in its way.
pub fn local_grid_route(world: &World, from: Vec3, to: Vec3, params: &CharacterParams, exclude: Option<Entity>) -> Option<Vec<Vec3>> {
    let physics = world.get_resource::<PhysicsWorld>()?;
    let straight = flat(to - from).length();
    let request = PathRequest {
        start: from,
        swimming_at: None,
        goal: to,
        goal_level: false,
        within: 0.3,
        max_length: (straight * 2.0 + 20.0).min(300.0),
        safe_drop: world.get_resource::<MotionAi>().map_or(2.0, |m| m.safe_drop),
        exclude,
    };
    let plan = plan_path(physics, params, &request).ok()?;
    let mut points: Vec<Vec3> = plan.waypoints().into_iter().map(|(p, _)| p).collect();
    match points.last_mut() {
        Some(last) => *last = Vec3::new(to.x, last.y, to.z),
        None => points.push(to),
    }
    Some(points)
}

/// An NPC's way to its goal and what it asks of its legs this tick.
#[derive(Component, Reflect, Clone, Debug, PartialEq, Default)]
#[reflect(Component, Default)]
pub struct NavAgent {
    pub goal: Option<Vec3>,
    /// Arrived within this many meters of the goal.
    pub within: f32,
    /// Movement input: 0.5 walks, 1 runs.
    pub speed: f32,
    /// Waypoints, the last at the goal; none while there is no way there (it holds).
    pub path: Vec<Vec3>,
    /// The waypoint it walks to.
    pub index: u32,
    /// The goal the path was planned for.
    pub planned_to: Option<Vec3>,
    /// Where it stood when the path was planned. A direct route is planned again once the
    /// body is farther than its radius from the line between there and the goal: pushed,
    /// knocked back or sidestepping, it would walk ground [`plain_sight`] never looked at.
    pub planned_from: Option<Vec3>,
    /// The goal was in plain sight: no route needed.
    pub direct: bool,
    /// Seconds before a blocked route is planned again.
    pub repath: f32,
    /// At the goal (or without one).
    pub arrived: bool,
    /// The velocity its route asks for this tick (m/s, on the ground plane), the leash kept:
    /// what it would walk alone. Zero when it stands.
    pub preferred: Vec3,
    /// The movement input it asks for this tick: after avoidance and the leash.
    pub desired: Vec3,
    /// The body avoidance steers it around this tick (the first it would have run into).
    pub avoiding: Option<AiId>,
    /// The side it goes round on while it avoids: 1 right, -1 left, 0 none. It keeps to it
    /// from tick to tick.
    pub side: i8,
}

/// The body numbers of an NPC: its controller's (or its own), else the defaults.
fn params_of<'a>(
    db: &'a ContentDb,
    controller: Option<&'a CharacterController>,
    own: Option<&'a CharacterParams>,
) -> CharacterParams {
    match controller {
        Some(c) => c.resolve(own, Some(db)).clone(),
        None => own.cloned().unwrap_or_default(),
    }
}

/// Whether a body of `params` walks from `from` straight to `to`, so it needs no route: on
/// the navmesh of the class that serves it ([`NavRegion::serving`], the one its routes are
/// planned on) where that covers the walk, a line of walking links (no drop, no ledge, no
/// headroom it lacks, nothing carved) a body-wide sweep agrees with; elsewhere
/// [`straight_clear`] over ground without a hole or a drop deeper than the safe drop all along.
pub fn plain_sight(world: &World, from: Vec3, to: Vec3, params: &CharacterParams) -> bool {
    let Some(physics) = world.get_resource::<PhysicsWorld>() else { return true };
    if let Some(nav) = serving(world, params) {
        let query = NavQuery { from, to, goal_level: false, radius: params.radius, height: params.height, step_height: params.step_height };
        if let Some(straight) = nav.straight_walk(physics, &query) {
            return straight;
        }
    }
    let safe_drop = world.get_resource::<MotionAi>().map_or(2.0, |m| m.safe_drop);
    straight_clear(physics, from, to, params) && ground_along(physics, from, to, params, safe_drop)
}

/// Whether there is ground all along the line from `from` to `to` for a body of `params`:
/// no hole, no floor too steep to stand on, no drop deeper than `safe_drop` (what stands in
/// the way is [`straight_clear`]'s business).
fn ground_along(physics: &PhysicsWorld, from: Vec3, to: Vec3, params: &CharacterParams, safe_drop: f32) -> bool {
    let terrain = Terrain::new(physics, params, &[]);
    let delta = flat(to - from);
    let steps = (delta.length() / 0.5).ceil() as usize;
    let mut y = from.y;
    for k in 1..=steps {
        let at = from + delta * (k as f32 / steps as f32);
        match terrain.probe(at.x, at.z, y) {
            Probe::Floor { y: floor, .. } if floor >= y - safe_drop => y = floor,
            Probe::Water { surface } => y = terrain.swim_height(surface),
            Probe::Blocked(Block::Tight | Block::Wall) => {}
            _ => return false,
        }
    }
    true
}

/// The margin over a walker's head, on top of the skin it rests on, as the motion module's
/// room test reckons it (m).
const HEAD_GRAZE: f32 = 0.02;

/// Whether a body of `params` walks from `from` straight to `to` without touching the
/// `WORLD` layer: a capsule a little narrower than the body, from above its step height up to
/// its head, so a lintel or an overhang it does not pass under is in the way.
pub fn straight_clear(physics: &PhysicsWorld, from: Vec3, to: Vec3, params: &CharacterParams) -> bool {
    let delta = flat(to - from);
    let distance = delta.length();
    let Ok(dir) = Dir3::new(delta) else { return true };
    let ball = (params.radius * 0.95).max(0.05);
    // Centers of the lowest and the highest ball of the capsule above the feet; a body whose
    // head is no higher than the lowest ball's center is that ball alone.
    let low = params.step_height + params.radius + 0.05;
    let high = params.skin_width + HEAD_GRAZE + params.height - ball;
    let filter = QueryFilter::new(LayerMask::WORLD);
    let (body, center) = if high > low {
        (Shape::capsule(ball, high - low + 2.0 * ball), (low + high) * 0.5)
    } else {
        (Shape::sphere(ball), low)
    };
    let center = from + Vec3::Y * center;
    physics.shapecast(&body, center, Quat::IDENTITY, dir, distance, &filter).is_none_or(|hit| hit.distance >= distance - 0.05)
}

/// `NpcSystems::Navigate`, first: routes for the agents whose goal is new, moved or blocked.
pub(crate) fn nav_paths(world: &mut World) {
    // The navmesh belongs to the region in the world and knows the solids spawned since.
    refresh(world);
    let dt = world.resource::<Time>().delta_secs();
    let mut updated: Vec<(Entity, NavAgent)> = Vec::new();
    {
        let world: &World = world;
        let Some(db) = world.get_resource::<ContentDb>() else { return };
        if !world.contains_resource::<PhysicsWorld>() {
            return;
        }
        let tuning = NpcTuning::of(db);
        for (_, entity) in npcs_in_order(world) {
            let e = world.entity(entity);
            let (Some(agent), Some(drive), Some(brain), Some(tf)) =
                (e.get::<NavAgent>(), e.get::<NpcDrive>(), e.get::<NpcBrain>(), e.get::<Transform>())
            else {
                continue;
            };
            let mut next = agent.clone();
            next.goal = drive.goal.filter(|_| brain.active);
            next.within = drive.within;
            next.speed = drive.speed;
            next.repath = (next.repath - dt).max(0.0);
            let Some(goal) = next.goal else {
                if !next.path.is_empty() || next.planned_to.is_some() || next.planned_from.is_some() {
                    next.path.clear();
                    next.planned_to = None;
                    next.planned_from = None;
                    next.index = 0;
                }
                if next != *agent {
                    updated.push((entity, next));
                }
                continue;
            };
            let here = tf.translation;
            let params = params_of(db, e.get::<CharacterController>(), e.get::<CharacterParams>());
            let away = flat(goal - here).length();
            let moved = next.planned_to.is_none_or(|p| flat(p - goal).length() > (0.2 * away).max(0.75));
            // A straight walk was checked along one line only: off it by more than its
            // radius, the body plans again from where it stands (once per radius strayed, so
            // an NPC that keeps to its line asks nothing more).
            let strayed = next.direct && next.planned_from.is_some_and(|from| off_line(here, from, goal) > params.radius);
            if moved || strayed || (!next.direct && next.repath <= 0.0) {
                next.direct = plain_sight(world, here, goal, &params);
                // No route: no path, and it holds where it is until it plans again.
                next.path = if next.direct {
                    vec![goal]
                } else {
                    find_path(world, here, goal, &params).filter(|p| !p.is_empty()).unwrap_or_default()
                };
                next.index = 0;
                next.planned_to = Some(goal);
                next.planned_from = Some(here);
                next.repath = tuning.repath;
            } else if let Some(last) = next.path.last_mut() {
                // A goal that moved a little: the last leg follows it.
                *last = goal;
            }
            if next != *agent {
                updated.push((entity, next));
            }
        }
    }
    for (entity, next) in updated {
        if let Some(mut agent) = world.get_mut::<NavAgent>(entity) {
            *agent = next;
        }
    }
}

type Agents<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static AiId,
        &'static mut NavAgent,
        &'static NpcDrive,
        &'static NpcBrain,
        &'static Transform,
        Option<&'static Home>,
        Option<&'static CharacterController>,
        Option<&'static CharacterParams>,
        Option<&'static MoveModifiers>,
    ),
    With<Npc>,
>;

/// `NpcSystems::Navigate`, second: every agent follows its route and stays on its leash:
/// the velocity it would walk alone ([`NavAgent::preferred`]), and that as its movement
/// input until avoidance has its say.
pub(crate) fn nav_steer(db: Res<ContentDb>, mut agents: Agents) {
    let tuning = NpcTuning::of(&db);
    let mut order: Vec<(AiId, Entity)> = agents.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort();
    for (_, entity) in order {
        let Ok((_, _, mut agent, drive, brain, tf, home, controller, own, modifiers)) = agents.get_mut(entity) else {
            continue;
        };
        let params = params_of(&db, controller, own);
        let here = tf.translation;
        let mut next = agent.clone();
        next.desired = Vec3::ZERO;
        next.preferred = Vec3::ZERO;
        match next.goal.filter(|_| brain.active) {
            None => next.arrived = true,
            Some(goal) if flat(goal - here).length() <= next.within.max(0.05) => next.arrived = true,
            // No way there: it holds (and plans again every `repath` seconds).
            Some(_) if next.path.is_empty() => next.arrived = false,
            Some(goal) => {
                next.arrived = false;
                let last = next.path.len().saturating_sub(1);
                let mut index = (next.index as usize).min(last);
                while index < last && flat(next.path[index] - here).length() <= tuning.waypoint_reach {
                    index += 1;
                }
                next.index = index as u32;
                let toward = next.path.get(index).copied().unwrap_or(goal);
                let dir = flat(toward - here).try_normalize().unwrap_or(Vec3::ZERO);
                let speed = next.speed.clamp(0.0, 1.0);
                let (top, _) = ground_speed(&params, modifiers, speed);
                let mut vel = dir * top;
                if drive.leashed
                    && let Some(home) = home
                {
                    vel = keep_on_leash(vel, here, home.pos.unwrap_or(here), home.leash);
                }
                next.preferred = vel;
                next.desired = input_of(vel, top, speed);
            }
        }
        if *agent != next {
            *agent = next;
        }
    }
}

/// The ground speed an NPC walking at input `speed` makes: its params' through its
/// `MoveModifiers` (slowed, capped or rooted), the same the controller moves it by.
fn ground_speed(params: &CharacterParams, modifiers: Option<&MoveModifiers>, speed: f32) -> (f32, bool) {
    match modifiers {
        Some(modifiers) => modifiers.ground_speed(params, speed, false),
        None => params.ground_speed(speed, false),
    }
}

/// The movement input for velocity `vel` of a walker whose input `speed` makes `top` m/s.
fn input_of(vel: Vec3, top: f32, speed: f32) -> Vec3 {
    if top > 0.0 { (vel / top).clamp_length_max(1.0) * speed } else { Vec3::ZERO }
}

type Bodies<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        (&'static AiId, &'static Transform, &'static CharacterController, Option<&'static CharacterParams>),
        (Option<&'static MotionState>, Has<Dead>, Option<&'static Intent>, Option<&'static MoveModifiers>),
        (Option<&'static Npc>, Option<&'static NpcBrain>, Option<&'static NpcDrive>, Option<&'static Home>),
        Option<&'static mut NavAgent>,
    ),
>;

/// An agent avoidance steers this tick.
struct Asking {
    entity: Entity,
    body: Body,
    preferred: Vec3,
    until_stop: f32,
    target: Option<AiId>,
    side: i8,
    def: crate::def::AvoidanceDef,
    /// Its input speed and the top speed it makes.
    speed: f32,
    top: f32,
    /// Home and leash, while its state keeps it leashed.
    leash: Option<(Vec3, f32)>,
}

/// `NpcSystems::Drive`, first: every NPC on its way keeps clear of the other character
/// bodies ([`avoid`]), in `AiId` order, all against the bodies as they stand at the start of
/// the system; the leash holds after it. Writes [`NavAgent::desired`] (the movement input),
/// [`NavAgent::avoiding`] and [`NavAgent::side`].
pub(crate) fn nav_avoid(db: Res<ContentDb>, mut bodies: Bodies) {
    let tuning = NpcTuning::of(&db);
    let mut all: Vec<Body> = Vec::new();
    let mut asking: Vec<Asking> = Vec::new();
    let mut idle: Vec<Entity> = Vec::new();
    for (entity, (id, tf, controller, own), (motion, dead, intent, modifiers), (npc, brain, drive, home), agent) in bodies.iter() {
        let params = params_of(&db, Some(controller), own);
        let def = npc
            .and_then(|n| db.get::<BehaviorDef>(&n.behavior))
            .and_then(|b| b.avoidance.clone())
            .unwrap_or_else(|| tuning.avoidance.clone());
        let steering = !dead
            && def.on()
            && npc.is_some()
            && brain.is_some_and(|b| b.active)
            && intent.is_none_or(|i| i.orders.is_empty())
            && agent.is_some_and(|a| a.preferred != Vec3::ZERO);
        let here = tf.translation;
        let body = Body {
            id: *id,
            pos: here,
            vel: motion.map_or(Vec3::ZERO, |m| flat(m.velocity)),
            radius: params.radius,
            height: params.height,
            reciprocal: steering,
        };
        if !dead {
            all.push(body);
        }
        let Some(agent) = agent else { continue };
        if !steering {
            if agent.avoiding.is_some() || agent.side != 0 {
                idle.push(entity);
            }
            continue;
        }
        let speed = agent.speed.clamp(0.0, 1.0);
        let (top, _) = ground_speed(&params, modifiers, speed);
        let pace = agent.preferred.length();
        asking.push(Asking {
            entity,
            body,
            preferred: agent.preferred,
            until_stop: if pace > 1e-4 { (remaining(agent, here) - agent.within).max(0.0) / pace } else { 0.0 },
            target: brain.and_then(|b| b.target),
            side: agent.side,
            def,
            speed,
            top,
            leash: drive.filter(|d| d.leashed).and(home).map(|h| (h.pos.unwrap_or(here), h.leash)),
        });
    }
    all.sort_by_key(|b| b.id);
    asking.sort_by_key(|a| a.body.id);
    let chosen: Vec<(Entity, Vec3, Option<AiId>, i8)> = asking
        .iter()
        .map(|a| {
            let ask = Ask { me: a.body, preferred: a.preferred, until_stop: a.until_stop, target: a.target, side: a.side, def: &a.def };
            let steered = steer(&ask, &all);
            if steered.vel == a.preferred {
                return (a.entity, Vec3::NAN, None, 0);
            }
            let vel = match a.leash {
                Some((home, leash)) => keep_on_leash(steered.vel, a.body.pos, home, leash),
                None => steered.vel,
            };
            (a.entity, input_of(vel, a.top, a.speed), steered.avoiding, steered.side)
        })
        .collect();
    for (entity, desired, avoiding, side) in chosen {
        let Ok((.., Some(mut agent))) = bodies.get_mut(entity) else { continue };
        // NaN: the route's own input, set by `nav_steer`, stands.
        if !desired.is_nan() && agent.desired != desired {
            agent.desired = desired;
        }
        if agent.avoiding != avoiding || agent.side != side {
            agent.avoiding = avoiding;
            agent.side = side;
        }
    }
    for entity in idle {
        if let Ok((.., Some(mut agent))) = bodies.get_mut(entity) {
            agent.avoiding = None;
            agent.side = 0;
        }
    }
}

/// How far an agent at `here` still walks along its path to its goal (m, on the ground
/// plane).
fn remaining(agent: &NavAgent, here: Vec3) -> f32 {
    let index = agent.index as usize;
    match agent.path.get(index) {
        Some(next) => {
            let rest: f32 = agent.path[index..].windows(2).map(|w| flat(w[1] - w[0]).length()).sum();
            flat(*next - here).length() + rest
        }
        None => agent.goal.map_or(0.0, |g| flat(g - here).length()),
    }
}

/// How far `p` is from the segment from `a` to `b`, on the ground plane (m).
fn off_line(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let (ab, ap) = (flat(b - a), flat(p - a));
    let len2 = ab.length_squared();
    let t = if len2 > 1e-8 { (ap.dot(ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (ap - ab * t).length()
}

/// `vel` without the part that takes a body at `here` out of its leash.
fn keep_on_leash(vel: Vec3, here: Vec3, home: Vec3, leash: f32) -> Vec3 {
    let out = flat(here - home);
    let d = out.length();
    if d < leash - LEASH_ROOM * 0.5 || d <= 1e-4 {
        return vel;
    }
    let n = out / d;
    let outward = vel.dot(n);
    if outward > 0.0 { vel - n * outward } else { vel }
}

/// What `drive_intents` reads and writes per NPC (`AimTarget` is optional: a body restored without it gets it).
type DriveData = (Entity, &'static NpcBrain, &'static NavAgent, &'static mut NpcDrive, &'static mut Intent, Option<&'static mut AimTarget>, Has<SequenceHold>);

/// `NpcSystems::Drive`: what NPCs decided goes into their `Intent`, like a keyboard's: the
/// movement, the point to face, the presses, the held inputs, and its brain's target for `AimTarget`. An NPC with orders of its own
/// (an agent took it over with `move_to` or `attack`) follows those instead, unless a sequence (F12) holds it: the scene
/// drives a held body, so an order given to it meanwhile is dropped.
pub(crate) fn drive_intents(
    index: Res<AiIndex>,
    standing: Query<(), Standing>,
    mut npcs: Query<DriveData, With<Npc>>,
    mut commands: Commands,
) {
    for (entity, brain, agent, mut drive, mut intent, aim_target, held_by_scene) in &mut npcs {
        // The scene has this body (`play_sequence` dropped its orders when it took it): an order given since would win over
        // the run's `NpcDrive`, so it goes too.
        if held_by_scene && !intent.orders.is_empty() {
            intent.orders.clear();
        }
        // The brain's target is what its spawned objects aim at (`SpawnAim::Target`, `AtTarget`): the brain never writes
        // `Intent::target` (a lock-on), so `shards_action` reads this mirror after it. A body that has none yet (one restored
        // from an older snapshot) gets it when there is something to mirror. Only a body that stands (B5: neither dead nor
        // down) is mirrored: perception already forgets a fallen one, and the mirror never names it in between.
        let target = if brain.active { brain.target } else { None };
        let target = target.filter(|id| index.get(*id).is_some_and(|e| standing.contains(e)));
        match aim_target {
            Some(mut aim) if aim.0 != target => aim.0 = target,
            Some(_) => {}
            None if target.is_some() => {
                commands.entity(entity).insert(AimTarget(target));
            }
            None => {}
        }
        if !intent.orders.is_empty() {
            // Its own presses are dropped and what it held is let go (the director and the
            // planner leave it out of the fight meanwhile).
            if !drive.press.is_empty() {
                drive.press.clear();
            }
            if !drive.held.is_empty() {
                for input in std::mem::take(&mut drive.held) {
                    intent.set_held(&input, false);
                }
            }
            continue;
        }
        let (move_dir, aim) = if brain.active { (agent.desired, drive.face) } else { (Vec3::ZERO, None) };
        if intent.move_dir != move_dir {
            intent.move_dir = move_dir;
        }
        if intent.aim != aim {
            intent.aim = aim;
        }
        if !drive.press.is_empty() {
            let presses = std::mem::take(&mut drive.press);
            for input in presses {
                intent.press(input);
            }
        }
        let hold = if brain.active { drive.hold.clone() } else { Vec::new() };
        if drive.held != hold {
            for input in drive.held.iter().filter(|h| !hold.contains(h)) {
                intent.set_held(input, false);
            }
            for input in &hold {
                intent.set_held(input, true);
            }
            drive.held = hold;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_of_a_route_is_measured_along_its_waypoints() {
        let agent = NavAgent {
            goal: Some(Vec3::new(4.0, 0.0, 3.0)),
            path: vec![Vec3::new(0.0, 0.0, 5.0), Vec3::new(4.0, 0.0, 5.0), Vec3::new(4.0, 0.0, 3.0)],
            index: 1,
            ..default()
        };
        assert!((remaining(&agent, Vec3::new(1.0, 0.0, 5.0)) - (3.0 + 2.0)).abs() < 1e-5);
        let straight = NavAgent { goal: Some(Vec3::new(3.0, 0.0, 4.0)), ..default() };
        assert!((remaining(&straight, Vec3::ZERO) - 5.0).abs() < 1e-5, "no path: straight to the goal");
    }

    #[test]
    fn straying_is_measured_from_the_checked_segment_on_the_ground() {
        let (a, b) = (Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
        assert!((off_line(Vec3::new(4.0, 3.0, 0.5), a, b) - 0.5).abs() < 1e-5, "beside it, any height");
        assert!((off_line(Vec3::new(-2.0, 0.0, 0.0), a, b) - 2.0).abs() < 1e-5, "behind where it was planned");
        assert!((off_line(Vec3::new(13.0, 0.0, 4.0), a, b) - 5.0).abs() < 1e-5, "past the goal");
        assert!((off_line(Vec3::new(3.0, 0.0, 4.0), a, a) - 5.0).abs() < 1e-5, "planned at the goal");
    }

    #[test]
    fn the_leash_stops_the_outward_part_only() {
        let home = Vec3::ZERO;
        let at_edge = Vec3::new(9.9, 0.0, 0.0);
        let v = keep_on_leash(Vec3::new(1.0, 0.0, 1.0), at_edge, home, 10.0);
        assert!(v.x.abs() < 1e-5 && (v.z - 1.0).abs() < 1e-5, "{v}");
        assert_eq!(keep_on_leash(Vec3::new(-1.0, 0.0, 0.0), at_edge, home, 10.0), Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(keep_on_leash(Vec3::X, Vec3::new(3.0, 0.0, 0.0), home, 10.0), Vec3::X, "well inside");
    }
}
