//! The navmesh (E6.1): bakes, stitching, navlinks, routes, NPCs and agents on it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use bevy::prelude::*;
use serde_json::json;
use shards_content::ContentDb;
use shards_level::{LevelMarker, MarkerKind};
use shards_motion::perception::StepKind;
use shards_motion::PhysicsWorld;
use shards_npc::nav::bake::bake;
use shards_npc::nav::settings::NavSettings;
use shards_npc::nav::tile::TileKind;
use shards_npc::prelude::*;
use shards_npc::{NavPath, NavQuery, NavTile};
use shards_world::{Dormant, WorldSettings};

use crate::common::*;

/// A route on the navmesh in the world, for the default body.
fn route(app: &App, from: Vec3, to: Vec3) -> Result<NavPath, String> {
    let nav = app.world().get_resource::<NavRegion>().expect("a navmesh");
    let physics = app.world().resource::<PhysicsWorld>();
    let query = NavQuery { from, to, goal_level: false, radius: 0.4, height: 1.8, step_height: 0.35 };
    nav.path(physics, &query).map_err(|e| e.to_string())
}

/// The straight lines a route walks, from its start.
fn segments(path: &NavPath) -> Vec<(Vec3, Vec3)> {
    let mut at = path.start;
    path.points
        .iter()
        .map(|p| {
            let s = (at, *p);
            at = *p;
            s
        })
        .collect()
}

/// Whether the flat segment `a`-`b` comes within `margin` of the box `min`-`max` (x, z).
fn touches(a: Vec3, b: Vec3, min: Vec2, max: Vec2, margin: f32) -> bool {
    (0..=200).any(|k| {
        let p = a.lerp(b, k as f32 / 200.0);
        p.x > min.x - margin && p.x < max.x + margin && p.z > min.y - margin && p.z < max.y + margin
    })
}

fn bake_of(app: &App, tile: &str) -> NavTile {
    let settings = NavSettings::of(app.world().resource::<ContentDb>());
    bake(app.world(), None, TileKind::Tile, tile, 8.0, &settings).unwrap()
}

#[test]
fn a_tile_bakes_to_the_same_bytes_every_time() {
    let app = app();
    let a = bake_of(&app, "half_wall");
    let b = bake_of(&app, "half_wall");
    assert_eq!(a.to_file(), b.to_file(), "the same tile, the same bytes");
    let other = crate::common::app();
    assert_eq!(bake_of(&other, "half_wall").to_file(), a.to_file(), "in another game too");
    assert_eq!(NavTile::from_file(&a.to_file()).unwrap(), a, "the file reads back as baked");
    // 8 m at 0.5 m: 16 × 16 columns. Two areas: the ground (the wall leaves a gap), and the
    // top of the wall, where a body stands but never gets to.
    assert_eq!(a.size, [16, 16]);
    assert_eq!(a.areas, 2, "{}", a.summary());
    let floors = |i: i32, j: i32| a.column(i, j).map(|n| a.nodes[n as usize].y()).collect::<Vec<f32>>();
    // The wall runs along z = 4 in the bake's frame from x = 0 to 6; the gap is east of it.
    assert_eq!(floors(4, 8), [3.0], "on the wall, not beside it");
    assert_eq!(floors(4, 7), [3.0], "on its edge, not beside it: too close for the body");
    assert_eq!(floors(4, 6), [0.0], "half a meter off the wall");
    assert_eq!(floors(14, 8), [0.0], "in the gap");
    assert_eq!(a.portals.iter().filter(|p| p.area == a.nodes[a.column(0, 0).start as usize].area).count(), 4, "the ground meets every side");
    assert!(a.to_file().starts_with("// The baked navigation of tile `half_wall`"));
    let pillar = bake_of(&app, "pillar");
    assert_ne!(pillar.hash, a.hash);
    // The ledge: walked up by a mantle, down by a drop, never by walking.
    let ledge = bake_of(&app, "ledge");
    let kinds: Vec<StepKind> = ledge.links.iter().map(|l| l.kind).collect();
    assert!(kinds.contains(&StepKind::Mantle) && kinds.contains(&StepKind::Drop), "{kinds:?}");
    assert_eq!(ledge.areas, 1);
}

#[test]
fn routes_go_around_a_pillar_inside_a_tile_and_across_three_tiles() {
    let mut app = app();
    load_region(&mut app, &["floor pillar floor"]);
    let pillar = (Vec2::new(11.0, 3.0), Vec2::new(13.0, 5.0));
    for (from, to, tiles) in [(Vec3::new(4.0, 0.0, 4.0), Vec3::new(20.0, 0.0, 4.0), 3), (Vec3::new(9.5, 0.0, 4.0), Vec3::new(14.5, 0.0, 4.0), 1)] {
        let path = route(&app, from, to).unwrap();
        assert_eq!(path.tiles.len(), tiles, "{:?}", path.tiles);
        assert!(path.length > (to.x - from.x) + 0.3, "around, not through: {:.2}", path.length);
        assert!(path.length < (to.x - from.x) + 3.0, "not far around: {:.2}", path.length);
        for (a, b) in segments(&path) {
            assert!(!touches(a, b, pillar.0, pillar.1, 0.3), "{a} → {b} cuts the pillar: {:?}", path.points);
        }
        let last = *path.points.last().unwrap();
        assert!(Vec2::new(last.x - to.x, last.z - to.z).length() < 0.01, "ends at the goal: {last}");
    }
}

#[test]
fn stitching_turns_tiles_with_them_and_follows_open_sockets() {
    // The wall of the middle tile turned to run north-south: the gap is at its south end
    // turned 90°, at its north end turned 270°.
    for (rot, gap_south) in [(90, true), (270, false)] {
        let mut app = app();
        load_region(&mut app, &[&format!("floor half_wall:{rot} floor")]);
        let path = route(&app, Vec3::new(4.0, 0.0, 4.0), Vec3::new(20.0, 0.0, 4.0)).unwrap();
        let wall = if gap_south { (Vec2::new(11.75, 0.0), Vec2::new(12.25, 6.0)) } else { (Vec2::new(11.75, 2.0), Vec2::new(12.25, 8.0)) };
        for (a, b) in segments(&path) {
            assert!(!touches(a, b, wall.0, wall.1, 0.3), "rot {rot}: {a} → {b} cuts the wall: {:?}", path.points);
        }
        let through = path.points.iter().any(|p| if gap_south { p.z > 6.0 } else { p.z < 2.0 });
        assert!(through, "rot {rot}: through the gap: {:?}", path.points);
        assert_eq!(path.tiles, [[0, 0], [1, 0], [2, 0]]);
    }
    // The west edge of the middle tile is a wall socket: no way across, though its ground
    // meets the first tile's.
    let mut app = app();
    load_region(&mut app, &["floor sealed_w floor"]);
    let err = route(&app, Vec3::new(4.0, 0.0, 4.0), Vec3::new(12.0, 0.0, 4.0)).unwrap_err();
    assert!(err.contains("cannot reach at all") && err.contains("sockets"), "{err}");
    assert_eq!(app.world().resource::<NavRegion>().report.closed_edges, 1);
    // The local grid sees only the geometry: it walks straight across.
    let body = shards_motion::CharacterParams::default();
    let across = shards_npc::nav::local_grid_path(app.world(), Vec3::new(4.0, 0.0, 4.0), Vec3::new(12.0, 0.0, 4.0), &body);
    assert!(across.is_some(), "the ground meets");
    // An agent's `path_to` and `clearance` ask the navmesh: no way, and east ends at the edge.
    player(app.world_mut(), 0, Vec3::new(4.0, 0.0, 4.0));
    let err = fails(&mut app, "path_to", json!({"target": {"x": 12.0, "z": 4.0}}));
    assert!(err.starts_with("navmesh: ") && err.contains("cannot reach at all"), "{err}");
    let line = text(&ok(&mut app, "clearance", json!({})));
    assert!(line.contains("E closed 4.0m (the navmesh goes no further)"), "{line}");
    // Past the region's own edges the navmesh knows nothing: the physics world answers there.
    for dir in ["N", "S", "W", "NW", "SW"] {
        assert!(!line.contains(&format!("{dir} closed")), "{dir}: {line}");
    }
    let from = Vec3::new(4.0, 0.0, 4.0);
    let nav = app.world().resource::<NavRegion>();
    let east = nav.reach(from, (1, 0), 15.0).unwrap();
    assert!((east - 4.0).abs() < 0.05, "to the far side of the last column: {east}");
    assert_eq!(nav.reach(from, (0, 1), 15.0), None, "south leaves the region");
    // Only compass steps: a wider one would overflow the column it walks to.
    for step in [(i32::MAX, 0), (2, 0), (0, i32::MIN), (0, 0)] {
        assert_eq!(nav.reach(from, step, 15.0), None, "{step:?}");
    }
    // Where the sockets meet, the way goes on to the range, or out of the region.
    let mut open = crate::common::app();
    load_region(&mut open, &["floor floor floor"]);
    let nav = open.world().resource::<NavRegion>();
    assert_eq!(nav.reach(from, (1, 0), 15.0), Some(15.0));
    assert_eq!(nav.reach(from, (1, 0), 30.0), None);
}

#[test]
fn navlinks_join_what_the_ground_does_not() {
    let top = Vec3::new(14.0, 4.0, 4.0);
    let below = Vec3::new(4.0, 0.0, 4.0);
    let mut app = app();
    load_region(&mut app, &["floor cliff floor"]);
    let err = route(&app, top, below).unwrap_err();
    assert!(err.contains("cannot reach at all"), "the plateau is cut off: {err}");

    // A navlink from the plateau's edge down to the ground at its foot.
    let mut app = crate::common::app();
    app.world_mut().spawn((
        LevelMarker { kind: MarkerKind::NavLink, name: "rope".into(), to: Some(Vec3::new(11.0, 0.0, 4.0)) },
        Transform::from_xyz(12.6, 4.0, 4.0),
    ));
    load_region(&mut app, &["floor cliff floor"]);
    let path = route(&app, top, below).unwrap();
    let drop = path.legs.iter().find(|l| l.kind == StepKind::Drop).expect("a drop");
    assert!((drop.rise - 4.0).abs() < 0.05, "{drop:?}");
    assert!(path.to_plan().summary().contains("(drop 4.0m)"), "{}", path.to_plan().summary());
    // One way: the ground does not reach the plateau by it.
    let err = route(&app, below, top).unwrap_err();
    assert!(err.contains("only reached the other way"), "{err}");
    // The query tool, from a point on the plateau (it stands on top of what is there).
    let out = ok(&mut app, "nav_path", json!({"from": {"x": 14.0, "z": 4.0}, "to": {"x": 4.0, "z": 4.0}}));
    assert!(out.result["path"].as_str().unwrap().contains("(drop 4.0m)"), "{}", out.result);
    let info = ok(&mut app, "nav_info", json!({}));
    assert_eq!(info.result["navlinks"][0]["name"], "rope");
    assert_eq!(info.result["navlinks"][0]["used"], true);
}

#[test]
fn an_unreachable_goal_is_a_clear_error() {
    let mut app = app();
    load_region(&mut app, &["floor pillar spire"]);
    player(app.world_mut(), 0, Vec3::new(4.0, 0.0, 4.0));
    // The middle of the spire: steep all round, nothing to stand on within reach.
    let err = fails(&mut app, "nav_path", json!({"to": {"x": 20.0, "z": 4.0}}));
    assert!(err.starts_with("no route from (4.0, 4.0) to (20.0, 4.0) for a body of 0.40m (navmesh): the goal (20.0, 4.0) is not on walkable ground"), "{err}");
    // The top of the pillar: ground, but none that the start reaches.
    let err = fails(&mut app, "nav_path", json!({"to": {"x": 12.0, "z": 4.0}}));
    assert!(err.contains("is on ground the start cannot reach at all (tile (1, 0) from tile (0, 0))"), "{err}");
    let out = ok(&mut app, "nav_path", json!({"to": {"x": 22.5, "z": 4.0}}));
    let r = &out.result;
    assert_eq!(r["provider"], "navmesh");
    assert!(r["path"].as_str().unwrap().starts_with("path "), "{r}");
    let distance = r["distance"].as_f64().unwrap();
    assert!((18.6..24.0).contains(&distance), "around the pillar and the spire: {r}");
    assert_eq!(r["tiles"], json!(["(0, 0)", "(1, 0)", "(2, 0)"]));
    assert_eq!(r["waypoints"].as_array().unwrap().last().unwrap()["x"], 22.5);
    // Outside the region the local grid answers.
    let out = ok(&mut app, "nav_path", json!({"from": {"x": -20.0, "z": -20.0}, "to": {"x": -10.0, "z": -20.0}}));
    assert_eq!(out.result["provider"], "local grid");
}

/// A temporary asset folder with the test world and a `nav` definition.
fn assets_with_nav(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("nav_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("content/world")).unwrap();
    for file in ["nav.tile.ron", "nav.region.ron", "nav.socket_rules.ron", "nav.poi.ron"] {
        std::fs::copy(Path::new(ASSETS).join("content/world").join(file), dir.join("content/world").join(file)).unwrap();
    }
    std::fs::write(dir.join("content/world/default.nav.ron"), "{ \"default\": (cell: 0.5) }\n").unwrap();
    dir
}

fn errors(app: &App) -> Vec<String> {
    app.world().resource::<ContentDb>().errors().map(ToString::to_string).collect()
}

fn warnings(app: &App) -> Vec<String> {
    all_journal(app).into_iter().filter(|l| l.contains("[warn]") && l.contains("navmesh")).collect()
}

#[test]
fn bakes_are_checked_against_their_tiles_and_baked_again_when_stale() {
    let dir = assets_with_nav("stale");
    let mut app = app_in(dir.to_str().unwrap(), 7, |_| {});
    // No bake files yet: each tile a region uses is a content error.
    let missing = errors(&app);
    assert_eq!(missing.iter().filter(|e| e.contains("has no bake file")).count(), 7, "{missing:#?}");
    assert!(missing.iter().any(|e| e.contains("tile `pillar` has no bake file (nav/pillar.navtile.ron); bake it with `--bake-nav pillar`")), "{missing:#?}");

    let out = ok(&mut app, "nav_bake", json!({}));
    assert!(text(&out).starts_with("baked 7 tiles into nav/"), "{}", text(&out));
    let first = std::fs::read_to_string(dir.join("nav/pillar.navtile.ron")).unwrap();
    ok(&mut app, "nav_bake", json!({"tile": "pillar"}));
    assert_eq!(std::fs::read_to_string(dir.join("nav/pillar.navtile.ron")).unwrap(), first, "baked again: the same bytes");
    assert!(text(&ok(&mut app, "content_check", json!({}))).starts_with("ok: "), "fresh bakes are clean content");
    let unknown = fails(&mut app, "nav_bake", json!({"tile": "pilar"}));
    assert!(unknown.contains("did you mean `pillar`?"), "{unknown}");

    // Fresh files: a region reads them and bakes nothing.
    load_region(&mut app, &["floor pillar floor"]);
    assert!(warnings(&app).is_empty(), "{:#?}", warnings(&app));
    let info = ok(&mut app, "nav_info", json!({}));
    assert_eq!(info.result["from_files"], json!(["floor", "pillar"]));

    // The pillar changes on disk: its bake is stale.
    let tiles = dir.join("content/world/nav.tile.ron");
    let text_before = std::fs::read_to_string(&tiles).unwrap();
    std::fs::write(&tiles, text_before.replace("Box(min: (-1.0, 0.0, -1.0), max: (1.0, 3.0, 1.0))", "Box(min: (-1.5, 0.0, -1.5), max: (1.5, 3.0, 1.5))")).unwrap();
    let check = text(&ok(&mut app, "content_check", json!({})));
    assert!(check.contains("the bake of tile `pillar` (nav/pillar.navtile.ron) is stale"), "{check}");
    assert!(!check.contains("`floor`"), "only the pillar: {check}");

    // A bake file of something else (a stale hash) is baked again when the region loads, and said.
    std::fs::write(dir.join("nav/floor.navtile.ron"), &first).unwrap();
    load_region(&mut app, &["floor pillar floor"]);
    let warned = warnings(&app);
    assert!(warned.iter().any(|w| w.contains("navmesh: 1 tile baked while loading (stale: floor)") && w.contains("--bake-nav all")), "{warned:#?}");
    route(&app, Vec3::new(4.0, 0.0, 4.0), Vec3::new(20.0, 0.0, 4.0)).expect("it works anyway");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_npc_walks_around_pillars_by_navmesh_and_stays_on_its_leash() {
    let mut app = app();
    load_region(&mut app, &["floor pillar floor"]);
    let world = app.world_mut();
    let home_at = Vec3::new(4.0, 0.0, 4.0);
    // It senses through walls (`boss` perception): what stands between is the pillar.
    let sentry = npc(world, "grunt", Npc::new("sentry", "boss"), home_at, Vec3::X);
    home(world, sentry, home_at, 10.0);
    let hero = player(world, 0, Vec3::new(14.6, 0.0, 4.0));
    let pillar = (Vec2::new(11.0, 3.0), Vec2::new(13.0, 5.0));
    let mut farthest: f32 = 0.0;
    let mut routed = false;
    for _ in 0..480 {
        step(&mut app, 1);
        let at = feet(&app, sentry);
        farthest = farthest.max(flat_distance(at, home_at));
        assert!(!touches(at, at, pillar.0, pillar.1, 0.3), "in the pillar at {at}");
        let agent = app.world().get::<NavAgent>(sentry).unwrap();
        routed |= !agent.direct && agent.path.len() > 1;
    }
    assert_eq!(state(&app, sentry), "chase");
    assert!(routed, "it followed a route, not a straight line");
    assert!(farthest <= 10.0 + 0.05, "never beyond its leash: {farthest:.2} m");
    let at = feet(&app, sentry);
    assert!(at.x > 12.0, "around the pillar to the hero's side: {at}");
    // The route came from the navmesh.
    let _ = hero;
    let out = ok(&mut app, "nav_path", json!({"from": {"x": 4.0, "z": 4.0}, "to": {"x": 13.5, "z": 4.0}}));
    assert_eq!(out.result["provider"], "navmesh");

    // What a scenario sees of the same: `npc_detour` while it goes around, never
    // `npc_beyond_leash`.
    let mut app = crate::common::app();
    load_region(&mut app, &["floor pillar floor"]);
    let world = app.world_mut();
    let sentry = npc(world, "grunt", Npc::new("sentry", "boss"), home_at, Vec3::X);
    home(world, sentry, home_at, 10.0);
    player(world, 0, Vec3::new(14.6, 0.0, 4.0));
    let out = ok(&mut app, "step", json!({"ticks": 480, "until": [{"named": "npc_detour"}]}));
    assert!(text(&out).contains("stopped: condition npc_detour"), "{}", text(&out));
    let out = ok(&mut app, "step", json!({"ticks": 480, "until": [{"named": "npc_beyond_leash"}]}));
    assert!(text(&out).contains("no condition met"), "{}", text(&out));
    // Put out of its zone by hand: the condition says so.
    let sentry_id = id(&app, sentry);
    ok(&mut app, "teleport", json!({"id": sentry_id, "x": 4.0, "z": 16.0}));
    let out = ok(&mut app, "step", json!({"ticks": 1, "until": [{"named": "npc_beyond_leash"}]}));
    assert!(text(&out).contains("stopped: condition npc_beyond_leash"), "{}", text(&out));
}

#[test]
fn dormant_npcs_do_nothing_until_a_player_comes_near() {
    let mut app = app_with(|app| {
        app.insert_resource(WorldSettings { active_radius: Some(10.0), ..default() });
    });
    let world = app.world_mut();
    let walker = npc(world, "grunt", Npc::new("walker", "eyes"), Vec3::ZERO, Vec3::X);
    world.entity_mut(walker).insert(Patrol { points: vec![Vec3::new(5.0, 0.0, 0.0)] });
    let hero = player(world, 0, Vec3::new(30.0, 0.0, 30.0));
    step(&mut app, 60);
    assert!(app.world().get::<Dormant>(walker).is_some(), "far from the player: dormant");
    assert!(!app.world().get::<NpcBrain>(walker).unwrap().active);
    assert!(flat_distance(feet(&app, walker), Vec3::ZERO) < 0.05, "it did not move: {}", feet(&app, walker));
    let me = id(&app, walker);
    let state = ok(&mut app, "npc_state", json!({"id": me}));
    assert_eq!(state.result["active"], false, "{}", state.result);
    // The player comes near: it wakes the next tick and walks its patrol.
    teleport(&mut app, hero, Vec3::new(3.0, 0.0, 3.0));
    step(&mut app, 1);
    assert!(app.world().get::<NpcBrain>(walker).unwrap().active, "awake");
    step(&mut app, 180);
    assert!(feet(&app, walker).x > 2.0, "walking: {}", feet(&app, walker));
}

#[test]
fn move_to_path_follows_the_navmesh_beyond_the_local_planners_reach() {
    let mut app = app();
    let rows: Vec<String> = (0..8).map(|y| (0..8).map(|x| if (x + y) % 3 == 1 { "pillar" } else { "floor" }).collect::<Vec<_>>().join(" ")).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    load_region(&mut app, &rows);
    let hero = player(app.world_mut(), 0, Vec3::new(2.0, 0.0, 2.0));
    let goal = json!({"x": 62.0, "z": 62.0});
    // The local grid stops at 80 m; `path_to` asks the navmesh first, which goes as far as
    // the region does, unless `max` says otherwise.
    let far = ok(&mut app, "path_to", json!({"target": goal}));
    assert_eq!(far.result["provider"], "navmesh", "{}", far.result);
    assert!(far.result["path"].as_str().unwrap().starts_with("path 8"), "{}", far.result);
    let capped = fails(&mut app, "path_to", json!({"target": goal, "max": 80}));
    assert!(capped.contains("the shortest route (navmesh) is 8") && capped.contains("longer than max 80m"), "{capped}");
    let out = ok(&mut app, "move_to", json!({"target": goal, "path": true}));
    assert!(text(&out).starts_with("ok: following path 8"), "{}", text(&out));
    for _ in 0..40 {
        step(&mut app, 60);
        if app.world().get::<shards_core::Intent>(hero).unwrap().orders.is_empty() {
            break;
        }
    }
    let at = feet(&app, hero);
    assert!(flat_distance(at, Vec3::new(62.0, 0.0, 62.0)) < 1.0, "arrived: {at}");
}

#[test]
fn a_hundred_routes_on_an_8x8_region_stay_within_budget() {
    let mut app = app();
    const TILES: [&str; 6] = ["floor", "pillar", "half_wall:0", "half_wall:90", "half_wall:180", "half_wall:270"];
    let rows: Vec<String> = (0..8).map(|y| (0..8).map(|x| TILES[(x * 3 + y * 5) % 6]).collect::<Vec<_>>().join(" ")).collect();
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let built = Instant::now();
    load_region(&mut app, &rows);
    let build = built.elapsed();
    // Points near tile corners: open ground in every tile.
    let mut seed = 12345u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let mut point = || {
        let (x, y, cx, cz) = (next() % 8, next() % 8, next() % 2, next() % 2);
        Vec3::new(x as f32 * 8.0 + if cx == 0 { 1.5 } else { 6.5 }, 0.0, y as f32 * 8.0 + if cz == 0 { 1.5 } else { 6.5 })
    };
    let pairs: Vec<(Vec3, Vec3)> = (0..100).map(|_| (point(), point())).collect();
    let started = Instant::now();
    let mut total = 0.0;
    for (from, to) in &pairs {
        let path = route(&app, *from, *to).unwrap_or_else(|e| panic!("{from} → {to}: {e}"));
        total += path.length;
    }
    let took = started.elapsed();
    println!("8×8 region built in {build:?}; 100 routes ({total:.0} m) in {took:?}");
    assert!(took.as_secs_f32() < 10.0, "100 routes took {took:?}");
    // The same routes again: the same answers.
    let again: f32 = pairs.iter().map(|(a, b)| route(&app, *a, *b).unwrap().length).sum();
    assert_eq!(again, total);
}

#[test]
fn a_recorded_npc_walk_on_the_navmesh_plays_back_identically() {
    use ai_core::replay;
    use ai_core::AiReplay;
    let setup = |app: &mut App| -> Entity {
        load_region(app, &["floor half_wall:90 floor"]);
        let world = app.world_mut();
        let walker = npc(world, "grunt", Npc::new("walker", "eyes"), Vec3::new(4.0, 0.0, 2.0), Vec3::X);
        world.entity_mut(walker).insert(Patrol { points: vec![Vec3::new(20.0, 0.0, 2.0)] });
        walker
    };
    let mut original = app();
    let walker = setup(&mut original);
    let mut deepest: f32 = 0.0;
    for _ in 0..600 {
        step(&mut original, 1);
        deepest = deepest.max(feet(&original, walker).z);
    }
    assert!(deepest > 6.0, "it went through the gap at the wall's south end: {deepest:.2}");
    assert!(flat_distance(feet(&original, walker), Vec3::new(20.0, 0.0, 2.0)) < 1.0, "and got there: {}", feet(&original, walker));
    let recording = replay::save(original.world());
    let recording: replay::Replay = serde_json::from_str(&serde_json::to_string(&recording).unwrap()).unwrap();

    let mut copy = app();
    setup(&mut copy);
    replay::play(copy.world_mut(), recording.clone()).unwrap();
    step(&mut copy, recording.end.tick);
    copy.update();
    let report = copy.world().resource::<AiReplay>().report().cloned().expect("playback finished");
    assert!(report.identical(), "{report}");
}

#[test]
fn solids_the_bakes_never_saw_are_carved_out_and_back_in() {
    let mut app = app();
    load_region(&mut app, &["floor pillar floor"]);
    let carved = |app: &App| app.world().resource::<NavRegion>().report.carved;
    assert_eq!(carved(&app), 0, "the tiles' own solids are their bakes', the arena's floor lies below the ground");
    let (from, to) = (Vec3::new(2.0, 0.0, 4.0), Vec3::new(6.0, 0.0, 4.0));
    assert!(route(&app, from, to).unwrap().length < 4.01, "straight across");
    // The game puts a wall in the way after the region loaded: the next tick carves it out.
    let blocker = wall(app.world_mut(), Vec3::new(3.5, 0.0, 2.0), Vec3::new(4.5, 2.5, 6.0));
    step(&mut app, 1);
    assert!(carved(&app) > 0);
    let around = route(&app, from, to).unwrap();
    assert!(around.length > 5.0, "around the wall: {:?}", around.points);
    for (a, b) in segments(&around) {
        assert!(!touches(a, b, Vec2::new(3.5, 2.0), Vec2::new(4.5, 6.0), 0.3), "{a} → {b} cuts the wall");
    }
    // It goes again: the cells are walkable again.
    app.world_mut().despawn(blocker);
    step(&mut app, 1);
    assert_eq!(carved(&app), 0);
    assert!(route(&app, from, to).unwrap().length < 4.01, "straight across again");
}

/// A diagonal step passes between two side columns: where carving switched one of them off,
/// `reach` stops, though the bake's diagonal link and the column it leads to are still there.
#[test]
fn a_diagonal_reach_stops_where_a_side_column_is_switched_off() {
    let mut app = app();
    load_region(&mut app, &["floor floor floor"]);
    // A thin post: carving switches off the columns round it, not the ones a step further.
    wall(app.world_mut(), Vec3::new(11.9, 0.0, 3.9), Vec3::new(12.1, 2.5, 4.1));
    step(&mut app, 1);
    let off: Vec<[i32; 2]> = {
        let nav = app.world().resource::<NavRegion>();
        disabled(&app).into_iter().map(|n| nav.node_column(n)).collect()
    };
    let nav = app.world().resource::<NavRegion>();
    let open = |c: [i32; 2]| !off.contains(&c) && !nav.nodes_at(c).is_empty();
    // A column whose east side is off, while it, its south-east diagonal and its south side are open.
    let column = off
        .iter()
        .map(|c| [c[0] - 1, c[1]])
        .find(|c| open(*c) && open([c[0] + 1, c[1] + 1]) && open([c[0], c[1] + 1]))
        .expect("a column beside the carved ones");
    let from = nav.node_pos(nav.nodes_at(column).start);
    let half_diagonal = 0.5 * nav.cell * std::f32::consts::SQRT_2;
    let south_east = nav.reach(from, (1, 1), 2.0).unwrap();
    assert!(
        south_east <= half_diagonal + 1e-3,
        "south-east stops at the corner of its own column: {south_east} (column {column:?}, off {off:?})"
    );
    // South, its one way open, goes on.
    assert!(nav.reach(from, (0, 1), 2.0).unwrap() > 1.0);
}

/// The nodes switched off now, in node order.
fn disabled(app: &App) -> Vec<u32> {
    let nav = app.world().resource::<NavRegion>();
    (0..nav.node_count()).filter(|n| nav.is_disabled(*n)).collect()
}

/// The disabled nodes of a region built now around the solids `solids` (fresh: no history).
fn fresh_disabled(rows: &[&str], solids: &[(Vec3, Vec3)]) -> Vec<u32> {
    let mut app = app();
    for (min, max) in solids {
        wall(app.world_mut(), *min, *max);
    }
    load_region(&mut app, rows);
    disabled(&app)
}

/// Two solids carving the same ground, spawned and taken away one at a time in either
/// order: after every change the navmesh is what a region built around the solids standing
/// then would be, and the ground comes back whole at the end.
#[test]
fn carving_is_the_solids_standing_now_not_their_history() {
    let rows = ["floor floor floor"];
    let a = (Vec3::new(11.0, 0.0, 2.0), Vec3::new(12.0, 2.5, 6.0));
    let b = (Vec3::new(11.6, 0.0, 3.0), Vec3::new(12.6, 2.5, 7.0));
    let (from, to) = (Vec3::new(10.0, 0.0, 4.0), Vec3::new(14.0, 0.0, 4.0));
    for gone_first in [0, 1] {
        let mut app = app();
        load_region(&mut app, &rows);
        let wa = wall(app.world_mut(), a.0, a.1);
        step(&mut app, 1);
        let wb = wall(app.world_mut(), b.0, b.1);
        step(&mut app, 1);
        assert_eq!(disabled(&app), fresh_disabled(&rows, &[a, b]), "both standing");
        let (first, rest) = if gone_first == 0 { (wa, b) } else { (wb, a) };
        app.world_mut().despawn(first);
        step(&mut app, 1);
        assert_eq!(disabled(&app), fresh_disabled(&rows, &[rest]), "one gone (gone first: {gone_first})");
        let second = if gone_first == 0 { wb } else { wa };
        app.world_mut().despawn(second);
        step(&mut app, 1);
        assert_eq!(disabled(&app), Vec::<u32>::new(), "both gone (gone first: {gone_first})");
        assert!(route(&app, from, to).unwrap().length < 4.01, "straight across again");
    }
}

/// Routes on a navmesh carved again and again are the routes of one built around the solids
/// standing now: the costs inside the tiles a change left alone are kept from the build before,
/// and those of the tiles it touched are measured again. Wall `a` shuts the north-east tile's
/// way from its west side to its south side and stays clear of both sides' middles (where the
/// tile graph measures from), so only a cost measured again sends the route round by the
/// south-west tile.
#[test]
fn a_navmesh_carved_again_routes_as_one_built_now() {
    let rows = ["floor floor", "floor floor"];
    let a = (Vec3::new(8.0, 0.0, 5.5), Vec3::new(16.0, 2.5, 6.0));
    let b = (Vec3::new(2.0, 0.0, 10.0), Vec3::new(3.0, 2.5, 11.0));
    let c = (Vec3::new(13.0, 0.0, 13.0), Vec3::new(14.0, 2.5, 14.0));
    let pairs = [
        (Vec3::new(6.0, 0.0, 2.0), Vec3::new(14.0, 0.0, 10.0)),
        (Vec3::new(2.0, 0.0, 6.0), Vec3::new(13.0, 0.0, 3.0)),
        (Vec3::new(14.0, 0.0, 14.0), Vec3::new(12.0, 0.0, 2.0)),
        (Vec3::new(3.0, 0.0, 13.0), Vec3::new(10.0, 0.0, 7.0)),
        (Vec3::new(1.0, 0.0, 1.0), Vec3::new(15.0, 0.0, 15.0)),
    ];
    // Lengths to the millimetre and the tiles crossed, or why there is no route.
    type Routes = Vec<Result<(i64, Vec<[i32; 2]>), String>>;
    let routes = |app: &App| -> Routes {
        pairs.iter().map(|(from, to)| route(app, *from, *to).map(|p| ((p.length * 1000.0).round() as i64, p.tiles))).collect()
    };
    // The routes of the region built with `walls` standing from the start.
    let built_with = |walls: &[(Vec3, Vec3)]| {
        let mut fresh = app();
        for (min, max) in walls {
            wall(fresh.world_mut(), *min, *max);
        }
        load_region(&mut fresh, &rows);
        routes(&fresh)
    };
    assert_ne!(built_with(&[a]), built_with(&[]), "wall a changes a route");
    let mut carved = app();
    load_region(&mut carved, &rows);
    assert_eq!(routes(&carved), built_with(&[]), "as built");
    let wa = wall(carved.world_mut(), a.0, a.1);
    step(&mut carved, 1);
    assert_eq!(routes(&carved), built_with(&[a]), "a put up");
    wall(carved.world_mut(), b.0, b.1);
    step(&mut carved, 1);
    assert_eq!(routes(&carved), built_with(&[a, b]), "b put up");
    carved.world_mut().despawn(wa);
    step(&mut carved, 1);
    assert_eq!(routes(&carved), built_with(&[b]), "a taken down");
    wall(carved.world_mut(), c.0, c.1);
    step(&mut carved, 1);
    assert_eq!(routes(&carved), built_with(&[b, c]), "c put up");
}

/// A solid that moves (it keeps its collider id) is carved where it stands now.
#[test]
fn a_solid_that_moves_is_carved_where_it_stands() {
    let rows = ["floor floor floor"];
    let mut app = app();
    load_region(&mut app, &rows);
    let block = wall(app.world_mut(), Vec3::new(11.0, 0.0, 2.0), Vec3::new(12.0, 2.5, 3.0));
    step(&mut app, 1);
    app.world_mut().get_mut::<Transform>(block).unwrap().translation += Vec3::new(0.0, 0.0, 3.0);
    step(&mut app, 1);
    assert_eq!(disabled(&app), fresh_disabled(&rows, &[(Vec3::new(11.0, 0.0, 5.0), Vec3::new(12.0, 2.5, 6.0))]));
}

/// A solid standing on a tile edge when the region loads seals that edge only while it
/// stands: once it goes, the tiles are joined again.
#[test]
fn a_solid_on_a_tile_edge_at_load_seals_it_only_while_it_stands() {
    let mut app = app();
    let blocker = wall(app.world_mut(), Vec3::new(7.75, 0.0, -0.5), Vec3::new(8.25, 3.0, 8.5));
    load_region(&mut app, &["floor floor"]);
    let (from, to) = (Vec3::new(4.0, 0.0, 4.0), Vec3::new(12.0, 0.0, 4.0));
    let err = route(&app, from, to).unwrap_err();
    assert!(err.contains("cannot reach at all"), "{err}");
    app.world_mut().despawn(blocker);
    step(&mut app, 1);
    let path = route(&app, from, to).unwrap_or_else(|e| panic!("joined again: {e}"));
    assert!(path.length < 8.01, "straight across: {:?}", path.points);
}

/// Ground a solid seals off is known to be cut off at once (no search over the region).
#[test]
fn ground_sealed_by_a_solid_is_cut_off_at_once() {
    let mut app = app();
    load_region(&mut app, &["floor floor floor"]);
    wall(app.world_mut(), Vec3::new(11.0, 0.0, -1.0), Vec3::new(13.0, 3.0, 9.0));
    step(&mut app, 1);
    let err = route(&app, Vec3::new(4.0, 0.0, 4.0), Vec3::new(20.0, 0.0, 4.0)).unwrap_err();
    assert!(err.contains("cannot reach at all"), "{err}");
}

/// A huge solid near the region (a ground plane kilometers wide) is carved in no time.
#[test]
fn a_huge_solid_is_carved_in_no_time() {
    let mut app = app();
    load_region(&mut app, &["floor pillar floor"]);
    let started = Instant::now();
    wall(app.world_mut(), Vec3::new(-20000.0, -2.0, -20000.0), Vec3::new(20000.0, -0.5, 20000.0));
    step(&mut app, 1);
    let took = started.elapsed();
    assert!(took.as_secs_f32() < 2.0, "carving took {took:?}");
    assert_eq!(app.world().resource::<NavRegion>().report.carved, 0, "below the ground: nothing carved");
}

/// A region whose navmesh cannot be built says so once, not every tick.
#[test]
fn a_navmesh_that_cannot_be_built_is_said_once() {
    let dir = assets_with_nav("odd_cell");
    std::fs::write(dir.join("content/world/default.nav.ron"), "{ \"default\": (cell: 0.3, check_bakes: false) }\n").unwrap();
    let mut app = app_in(dir.to_str().unwrap(), 7, |_| {});
    floor(app.world_mut());
    load_region(&mut app, &["floor floor"]);
    step(&mut app, 30);
    let said: Vec<String> = warnings(&app).into_iter().filter(|w| w.contains("navmesh: none for this region")).collect();
    assert_eq!(said.len(), 1, "{said:#?}");
    assert!(app.world().get_resource::<NavRegion>().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Navlinks the body cannot make (a climb higher than its mantle, a walk over no ground: it
/// has no jump) are not used, and `nav_info` says why.
#[test]
fn navlinks_the_body_cannot_make_are_not_used() {
    let mut app = app();
    let world = app.world_mut();
    // Up the 4 m cliff face: too high to climb.
    world.spawn((
        LevelMarker { kind: MarkerKind::NavLink, name: "ladder".into(), to: Some(Vec3::new(12.6, 4.0, 2.0)) },
        Transform::from_xyz(11.0, 0.0, 2.0),
    ));
    // Across the spire's tile at one level: a jump, and the body has none.
    world.spawn((
        LevelMarker { kind: MarkerKind::NavLink, name: "leap".into(), to: Some(Vec3::new(20.0, 0.0, 12.0)) },
        Transform::from_xyz(4.0, 0.0, 12.0),
    ));
    // A step down onto the ground: fine.
    world.spawn((
        LevelMarker { kind: MarkerKind::NavLink, name: "rope".into(), to: Some(Vec3::new(11.0, 0.0, 4.0)) },
        Transform::from_xyz(12.6, 4.0, 4.0),
    ));
    load_region(&mut app, &["floor cliff floor", "floor spire floor"]);
    let info = ok(&mut app, "nav_info", json!({}));
    let link = |name: &str| info.result["navlinks"].as_array().unwrap().iter().find(|l| l["name"] == name).cloned().unwrap();
    let ladder = link("ladder");
    assert_eq!(ladder["used"], false, "{ladder}");
    assert!(ladder["why"].as_str().unwrap().contains("more than the body's mantle height"), "{ladder}");
    let leap = link("leap");
    assert_eq!(leap["used"], false, "{leap}");
    assert!(leap["why"].as_str().unwrap().contains("has no jump"), "{leap}");
    assert_eq!(link("rope")["used"], true);
    let err = route(&app, Vec3::new(4.0, 0.0, 2.0), Vec3::new(14.0, 4.0, 2.0)).unwrap_err();
    assert!(err.contains("only reached the other way") || err.contains("cannot reach"), "no way up: {err}");
}

/// An NPC on the cliff's plateau chasing a player on the ground below: in plain sight, but
/// the only way down is a 4 m fall. The navmesh has no walk there and no route: it stays up.
#[test]
fn an_npc_does_not_walk_off_a_cliff_toward_a_target_in_plain_sight() {
    let mut app = app();
    load_region(&mut app, &["floor cliff floor"]);
    let world = app.world_mut();
    let top = Vec3::new(14.0, 4.0, 4.0);
    let sentry = npc(world, "grunt", Npc::new("sentry", "boss"), top, W);
    player(world, 0, Vec3::new(4.0, 0.0, 4.0));
    for t in 0..240 {
        step(&mut app, 1);
        let at = feet(&app, sentry);
        assert!(at.y > 3.9 && at.x > 12.0, "tick {t}: it went over the edge: {at}");
    }
    assert_eq!(state(&app, sentry), "chase");
    let agent = app.world().get::<NavAgent>(sentry).unwrap();
    assert!(!agent.direct && agent.path.is_empty(), "no straight walk, no route: it holds ({agent:?})");
    let me = id(&app, sentry);
    let out = ok(&mut app, "npc_state", json!({"id": me}));
    assert_eq!(out.result["nav"]["no_route"], true, "{}", out.result);
}

/// A walker that climbs less than the body the tiles were baked for is not planned on the
/// navmesh: its route would have a climb it cannot make. The local grid answers for it.
#[test]
fn a_walker_that_climbs_less_than_the_baked_body_plans_on_the_local_grid() {
    let mut app = app();
    load_region(&mut app, &["floor ledge floor"]);
    let hero = player(app.world_mut(), 0, Vec3::new(4.0, 0.0, 4.0));
    let goal = json!({"target": {"x": 14.0, "z": 4.0}, "path": true});
    // The baked body (the defaults) mantles the 1 m ledge.
    let out = ok(&mut app, "move_to", goal.clone());
    assert!(text(&out).contains("(mantle 1.0m)"), "{}", text(&out));
    app.world_mut().get_mut::<shards_core::Intent>(hero).unwrap().orders.clear();
    // This one only mantles half a meter.
    app.world_mut().entity_mut(hero).insert(shards_motion::CharacterParams { mantle_height: 0.5, ..default() });
    let out = call(&mut app, "move_to", goal);
    assert!(!text(&out).contains("mantle"), "no climb it cannot make: {}", text(&out));
}

/// `nav_path` for a walker the navmesh does not serve plans on the local grid with the
/// walker's own body, as `move_to path=true` does: the preview and the walk agree on what
/// it can climb.
#[test]
fn nav_path_for_a_walker_the_navmesh_does_not_serve_plans_with_its_own_body() {
    let mut app = app();
    load_region(&mut app, &["floor ledge floor"]);
    let hero = player(app.world_mut(), 0, Vec3::new(4.0, 0.0, 4.0));
    let me = id(&app, hero);
    let goal = json!({"x": 14.0, "z": 4.0});
    // The baked body (the defaults) mantles the 1 m ledge: both tools find the way up.
    let out = ok(&mut app, "nav_path", json!({"from": me, "to": goal}));
    assert_eq!(out.result["provider"], "navmesh", "{}", out.result);
    let out = ok(&mut app, "move_to", json!({"target": goal, "path": true}));
    assert!(text(&out).contains("(mantle 1.0m)"), "{}", text(&out));
    app.world_mut().get_mut::<shards_core::Intent>(hero).unwrap().orders.clear();
    // This one only mantles half a meter: the 1 m ledge spans the area, so there is no way.
    app.world_mut().entity_mut(hero).insert(shards_motion::CharacterParams { mantle_height: 0.5, ..default() });
    let walk = call(&mut app, "move_to", json!({"target": goal, "path": true}));
    assert!(!text(&walk).contains("mantle"), "no climb it cannot make: {}", text(&walk));
    let preview = fails(&mut app, "nav_path", json!({"from": me, "to": goal}));
    assert!(preview.contains("no route"), "the preview plans for its mantle height too: {preview}");
}

/// A bake is made from the prefabs its tile spawns, the ones they extend and the ones its
/// level file's nodes name: a change anywhere down those chains makes it stale.
#[test]
fn a_bake_goes_stale_when_any_prefab_down_its_chains_changes() {
    use shards_npc::nav::settings::bake_hash;
    let hashes = |stone: &str, tag: &str| -> (String, String) {
        let dir = assets_with_nav(tag);
        let prefabs = format!("{{ \"stone\": (components: {{\"Name\": \"{stone}\"}}), \"big_stone\": (extends: \"stone\", components: {{}}) }}\n");
        std::fs::write(dir.join("content/world/stones.prefab.ron"), prefabs).unwrap();
        let tiles = r#"{
            "rocky": (sockets: (n: "open", e: "open", s: "open", w: "open"), geometry: Inline([Box(min: (-4.0, -1.0, -4.0), max: (4.0, 0.0, 4.0)), Prefab(id: "big_stone", at: (1.0, 0.0, 1.0))])),
            "rocky_level": (sockets: (n: "open", e: "open", s: "open", w: "open"), geometry: Level(path: "levels/rocks.gltf")),
        }"#;
        std::fs::write(dir.join("content/world/rocks.tile.ron"), tiles).unwrap();
        std::fs::create_dir_all(dir.join("levels")).unwrap();
        let gltf = r#"{"asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}], "nodes": [{"name": "Rock", "translation": [1.0, 0.0, 1.0], "extras": {"prefab": "big_stone"}}]}"#;
        std::fs::write(dir.join("levels/rocks.gltf"), gltf).unwrap();
        let app = app_in(dir.to_str().unwrap(), 7, |_| {});
        let db = app.world().resource::<ContentDb>();
        let settings = NavSettings::of(db);
        let hash = |tile: &str| bake_hash(db, Some(&dir), TileKind::Tile, tile, 8.0, &settings).unwrap();
        let out = (hash("rocky"), hash("rocky_level"));
        let _ = std::fs::remove_dir_all(&dir);
        out
    };
    let (inline, level) = hashes("Stone", "chain_a");
    assert_eq!(hashes("Stone", "chain_b"), (inline.clone(), level.clone()), "the same content, the same hashes");
    let (inline2, level2) = hashes("Pebble", "chain_c");
    assert_ne!(inline, inline2, "a tile's prefab extends one that changed");
    assert_ne!(level, level2, "a tile's level file spawns a prefab whose parent changed");
}

/// A ledge whose face lies on a tile edge: the floor tile's last column is too close to the
/// face to stand in, so the way up is the bake's own two-cell hop (a mantle over the foot of
/// the ledge), across the edge; and down is a drop the same way.
#[test]
fn a_ledge_on_a_tile_edge_is_climbed_and_dropped_across_it() {
    let dir = assets_with_nav("edge_ledge");
    let block = r#"{ "block": (sockets: (n: "open", e: "open", s: "open", w: "open"), geometry: Inline([Box(min: (-4.0, -1.0, -4.0), max: (4.0, 1.0, 4.0))])) }"#;
    std::fs::write(dir.join("content/world/edge.tile.ron"), block).unwrap();
    let mut app = app_in(dir.to_str().unwrap(), 7, |_| {});
    load_region(&mut app, &["floor block"]);
    let (low, high) = (Vec3::new(4.0, 0.0, 4.0), Vec3::new(12.0, 1.0, 4.0));
    let up = route(&app, low, high).unwrap_or_else(|e| panic!("up: {e}"));
    assert!(up.legs.iter().any(|l| l.kind == StepKind::Mantle), "{}", up.to_plan().summary());
    let down = route(&app, high, low).unwrap_or_else(|e| panic!("down: {e}"));
    assert!(down.legs.iter().any(|l| l.kind == StepKind::Drop), "{}", down.to_plan().summary());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A door (a mover, F6) that rests closed is carved like any laid solid; while it moves it is
/// not (a kinematic collider), and once it rests open the ground it held is walkable again.
#[test]
fn a_closed_door_is_carved_and_an_open_one_is_not() {
    let rows = ["floor floor floor"];
    let closed = (Vec3::new(11.0, 0.0, 2.0), Vec3::new(12.0, 2.5, 6.0));
    let mut app = app();
    load_region(&mut app, &rows);
    let half = (closed.1 - closed.0) * 0.5;
    let door = app
        .world_mut()
        .spawn((
            Name::new("Door"),
            shards_motion::Mover { def: "door".into() },
            shards_motion::Collider::new(shards_motion::Shape::cuboid(half)),
            Transform::from_translation(closed.0 + half),
        ))
        .id();
    step(&mut app, 2);
    assert_eq!(disabled(&app), fresh_disabled(&rows, &[closed]), "closed, it is carved");
    let (from, to) = (Vec3::new(10.0, 0.0, 4.0), Vec3::new(14.0, 0.0, 4.0));
    assert!(route(&app, from, to).unwrap().length > 4.5, "around the closed door");
    shards_motion::mover_go(app.world_mut(), door, 1).unwrap();
    step(&mut app, 2);
    assert_eq!(disabled(&app), Vec::<u32>::new(), "moving, it is not");
    step(&mut app, 70);
    let lifted = (closed.0 + Vec3::Y * 3.0, closed.1 + Vec3::Y * 3.0);
    assert_eq!(disabled(&app), fresh_disabled(&rows, &[lifted]), "open, it is carved where it rests");
    assert!(route(&app, from, to).unwrap().length < 4.01, "straight through");
}
