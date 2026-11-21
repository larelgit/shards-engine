//! The navmesh of hand-made levels (`nav: true` in a `level` definition): baked in memory from the
//! level's own static solids when it is placed by id, with no region loaded.
//!
//! The level is a tower hall written here: a floor 20 by 12 m that crosses the chunk seams (the
//! world's x 0 and z 0), a walkway 3 m up along its north side, stairs up to it, a wall across the
//! floor with a door (a mover) in it, a crate, and a barrel that can be broken (it has `Health`).

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use shards_level::{despawn_level, spawn_level_by_id, FromLevel, PlacedLevels};
use shards_motion::perception::StepKind;
use shards_motion::{CharacterParams, Mover, PhysicsWorld};
use shards_npc::nav::{rebuild, refresh, NavBakes};
use shards_npc::{plain_sight, region_nav, routes_at, NavPath, NavQuery, NavRegion};

use crate::common::*;

/// A node of a level file: a box collider `size` across, its centre at `at`.
fn solid(name: &str, at: [f32; 3], size: [f32; 3]) -> String {
    format!(r#"{{"name": "{name}", "translation": {at:?}, "extras": {{"collider": "box", "size": {size:?}}}}}"#)
}

fn gltf(nodes: &[String]) -> String {
    let roots: Vec<String> = (0..nodes.len()).map(|n| n.to_string()).collect();
    format!(
        r#"{{"asset": {{"version": "2.0"}}, "scene": 0, "scenes": [{{"nodes": [{}]}}], "nodes": [{}]}}"#,
        roots.join(", "),
        nodes.join(", ")
    )
}

/// The tower hall (see the module docs).
fn tower() -> String {
    let mut nodes = vec![
        solid("Floor", [0.0, -0.5, 0.0], [20.0, 1.0, 12.0]),
        // The walkway: z -6..-4, its top 3 m up.
        solid("Walkway", [0.0, 2.9, -5.0], [20.0, 0.2, 2.0]),
        // A wall across the floor at x 6, under the walkway, with a gap at z 1..3.
        solid("Wall.North", [6.0, 1.25, -2.5], [0.3, 2.5, 7.0]),
        solid("Wall.South", [6.0, 1.25, 4.5], [0.3, 2.5, 3.0]),
        // A crate on the floor, in one chunk (x below 0, z above 0).
        solid("Crate", [-6.0, 0.5, 3.0], [1.0, 1.0, 1.0]),
    ];
    // Stairs 2 m wide from the floor at z 2 up north to the walkway's edge at z -4: twelve
    // steps of 0.25 m, each a block down to the floor.
    for k in 0..12 {
        let top = 0.25 * (k as f32 + 1.0);
        nodes.push(solid(&format!("Step.{k:02}"), [0.0, top * 0.5, 1.75 - 0.5 * k as f32], [2.0, top, 0.5]));
    }
    // The door in the wall's gap: shut as the level is placed, it lifts 3 m to open.
    nodes.push(r#"{"name": "Door", "translation": [6.0, 1.25, 2.0], "extras": {"mover": "door", "collider": "box", "size": [0.3, 2.5, 2.0]}}"#.to_owned());
    // A barrel that can be broken, on the floor in the south-east chunk.
    nodes.push(r#"{"name": "Barrel", "translation": [3.0, 0.5, 4.0], "extras": {"collider": "box", "size": [1.0, 1.0, 1.0], "Health": {"current": 10.0, "max": 10.0}}}"#.to_owned());
    gltf(&nodes)
}

/// An asset folder with the test content, the tower as the levels `tower` (which asks for a
/// navmesh) and `plain` (which does not), and a floor east of it as the level `annex`.
fn assets(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("level_nav_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("content")).unwrap();
    std::fs::create_dir_all(dir.join("levels")).unwrap();
    for entry in std::fs::read_dir(Path::new(ASSETS).join("content")).unwrap().flatten() {
        if entry.path().is_file() {
            std::fs::copy(entry.path(), dir.join("content").join(entry.file_name())).unwrap();
        }
    }
    let levels = r#"{
        "tower": (file: "levels/tower.gltf", nav: true),
        "plain": (file: "levels/tower.gltf"),
        "annex": (file: "levels/annex.gltf", nav: true),
    }"#;
    std::fs::write(dir.join("content/tower.level.ron"), levels).unwrap();
    std::fs::write(dir.join("levels/tower.gltf"), tower()).unwrap();
    std::fs::write(dir.join("levels/annex.gltf"), gltf(&[solid("Floor", [15.0, -0.5, 0.0], [10.0, 1.0, 12.0])])).unwrap();
    dir
}

/// A game with the level `id` placed; its chunk bakes are its own (the tests run side by side).
fn hall(name: &str, id: &str) -> (App, PathBuf) {
    let dir = assets(name);
    let mut app = app_in(dir.to_str().unwrap(), 7, |app| {
        app.add_plugins(shards_level::LevelPlugin);
    });
    app.world_mut().get_resource_or_init::<NavBakes>().isolated = true;
    spawn_level_by_id(app.world_mut(), id, None).unwrap_or_else(|e| panic!("{id}: {e}"));
    refresh(app.world_mut());
    (app, dir)
}

fn route(app: &App, from: Vec3, to: Vec3) -> Result<NavPath, String> {
    let nav = region_nav(app.world()).expect("a navmesh");
    let physics = app.world().resource::<PhysicsWorld>();
    let query = NavQuery { from, to, goal_level: false, radius: 0.4, height: 1.8, step_height: 0.35 };
    nav.path(physics, &query).map_err(|e| e.to_string())
}

fn entity_named(app: &mut App, node: &str) -> Entity {
    let world = app.world_mut();
    let mut q = world.query::<(Entity, &FromLevel)>();
    q.iter(world).find(|(_, from)| from.node == node).map(|(e, _)| e).unwrap_or_else(|| panic!("no node `{node}`"))
}

/// On the floor west of the stairs, and on the walkway right over a point of the floor.
const FLOOR: Vec3 = Vec3::new(-5.0, 0.0, -1.0);
const UNDER: Vec3 = Vec3::new(-5.0, 0.0, -5.0);
const WALKWAY: Vec3 = Vec3::new(-5.0, 3.0, -5.0);

#[test]
fn a_level_that_asks_has_a_navmesh_that_takes_the_stairs() {
    let (app, dir) = hall("stairs", "tower");
    let nav = region_nav(app.world()).expect("the level's navmesh");
    let bake = nav.levels.as_ref().expect("a navmesh of levels");
    assert_eq!(bake.levels, ["tower"]);
    // The floor crosses the world's x 0 and z 0: four chunks, stitched.
    assert_eq!(bake.bake.chunks, 4, "{}", nav.summary());
    assert!(nav.report.stitched > 0, "{}", nav.summary());
    assert!(lines(&app, "navmesh: levels tower").len() == 1, "{:#?}", all_journal(&app));

    // From the floor to the walkway over it: round by the stairs (onto which the default body,
    // which mantles 1.6 m, may climb from the side).
    let up = route(&app, UNDER, WALKWAY).unwrap_or_else(|e| panic!("up: {e}"));
    assert!(up.length > 12.0, "round by the stairs, not 3 m straight up: {}", up.to_plan().summary());
    assert!(up.legs.iter().all(|l| matches!(l.kind, StepKind::Walk | StepKind::Mantle)), "{}", up.to_plan().summary());
    assert!(up.points.iter().any(|p| p.y > 0.5 && p.y < 2.9), "it climbs: {:?}", up.points);
    let down = route(&app, WALKWAY, FLOOR).unwrap_or_else(|e| panic!("down: {e}"));
    assert!(down.length > 8.0, "{}", down.to_plan().summary());

    // A goal overhead is not in plain sight, though nothing stands between in the plane.
    let body = CharacterParams::default();
    assert!(!plain_sight(app.world(), UNDER, WALKWAY, &body), "the walkway is not a walk straight ahead");
    assert!(plain_sight(app.world(), UNDER, FLOOR, &body), "the floor is");
    assert_eq!(routes_at(app.world(), &body, FLOOR), "navmesh, body agent");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_level_that_does_not_ask_keeps_the_local_grid() {
    let (mut app, dir) = hall("plain", "plain");
    assert!(app.world().get_resource::<NavRegion>().is_none(), "no navmesh for a level without `nav`");
    assert_eq!(routes_at(app.world(), &CharacterParams::default(), FLOOR), "local grid: no region loaded");
    step(&mut app, 3);
    assert!(app.world().get_resource::<NavRegion>().is_none());
    assert!(lines(&app, "navmesh").is_empty(), "{:#?}", lines(&app, "navmesh"));
    // Placed beside it, the one that asks is baked; taken away again, its navmesh goes.
    let tower = spawn_level_by_id(app.world_mut(), "annex", None).unwrap().instance;
    refresh(app.world_mut());
    assert_eq!(region_nav(app.world()).and_then(|n| n.levels.as_ref()).map(|b| b.levels.clone()), Some(vec!["annex".to_owned()]));
    despawn_level(app.world_mut(), tower);
    assert!(region_nav(app.world()).is_none(), "not the navmesh of the levels standing now");
    refresh(app.world_mut());
    assert!(app.world().get_resource::<NavRegion>().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_door_of_the_level_carves_its_doorway_while_it_is_shut() {
    let (mut app, dir) = hall("door", "tower");
    let (west, east) = (Vec3::new(4.0, 0.0, 2.0), Vec3::new(8.0, 0.0, 2.0));
    // Shut: the wall has no other gap, and the walkway is a 3 m drop from the floor beyond.
    let shut = route(&app, west, east).unwrap_err();
    assert!(shut.contains("cannot reach"), "{shut}");
    let door = entity_named(&mut app, "Door");
    assert!(app.world().get::<Mover>(door).is_some());
    shards_motion::mover_go(app.world_mut(), door, 1).unwrap();
    step(&mut app, 90);
    refresh(app.world_mut());
    let open = route(&app, west, east).unwrap_or_else(|e| panic!("open: {e}"));
    assert!(open.length < 4.01, "straight through the doorway: {}", open.to_plan().summary());
    // The door moved; no chunk was baked again for it.
    let bake = region_nav(app.world()).and_then(|n| n.levels.as_ref()).expect("still the levels' navmesh");
    assert_eq!(bake.bake.reused, 0, "the first bake stands");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_baked_solid_that_goes_bakes_its_chunk_again() {
    let (mut app, dir) = hall("crate", "tower");
    // The middle of a column under the crate.
    let at = Vec3::new(-5.75, 0.0, 3.25);
    let stands = |app: &App| region_nav(app.world()).and_then(|nav| nav.nearest_node(at, 0.1, 0.5)).is_some();
    assert!(region_nav(app.world()).and_then(|nav| nav.nearest_node(at + Vec3::X * 2.0, 0.1, 0.5)).is_some(), "the floor beside it is walked");
    assert!(!stands(&app), "the crate stands there");
    let crate_ = entity_named(&mut app, "Crate");
    app.world_mut().despawn(crate_);
    step(&mut app, 1);
    refresh(app.world_mut());
    assert!(stands(&app), "the floor under the crate is walked");
    let bake = region_nav(app.world()).and_then(|n| n.levels.as_ref()).unwrap().bake;
    assert_eq!((bake.chunks, bake.reused), (4, 3), "only the crate's chunk was baked again");
    let _ = std::fs::remove_dir_all(dir);
}

/// What can be destroyed is not baked: it carves the ground it stands on, and gives it back the tick it goes,
/// with nothing built again.
#[test]
fn a_solid_with_health_carves_and_gives_its_ground_back_when_it_goes() {
    let (mut app, dir) = hall("barrel", "tower");
    let at = Vec3::new(3.25, 0.0, 4.25);
    let stands = |app: &App| region_nav(app.world()).and_then(|nav| nav.nearest_node(at, 0.1, 0.5)).is_some();
    assert!(!stands(&app), "the barrel stands there");
    assert!(region_nav(app.world()).unwrap().report.carved > 0, "it is carved, not baked in");
    let barrel = entity_named(&mut app, "Barrel");
    app.world_mut().despawn(barrel);
    step(&mut app, 1);
    refresh(app.world_mut());
    assert!(stands(&app), "the floor under the barrel is walked");
    let bake = region_nav(app.world()).and_then(|n| n.levels.as_ref()).unwrap().bake;
    assert_eq!(bake.reused, 0, "the first bake stands: nothing was made again");
    assert_eq!(lines(&app, "navmesh: levels tower").len(), 1, "{:#?}", lines(&app, "navmesh"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn levels_that_ask_are_one_navmesh_and_a_level_placed_again_bakes_nothing() {
    let (mut app, dir) = hall("two", "tower");
    let first = app.world().resource::<PlacedLevels>().levels[0].instance;
    let annex = Vec3::new(18.0, 0.0, 4.0);
    assert!(route(&app, FLOOR, annex).is_err(), "the annex is not there yet");
    // The door's wall stands between: open it first.
    let door = entity_named(&mut app, "Door");
    shards_motion::mover_go(app.world_mut(), door, 1).unwrap();
    step(&mut app, 90);
    spawn_level_by_id(app.world_mut(), "annex", None).unwrap();
    refresh(app.world_mut());
    let nav = region_nav(app.world()).expect("one navmesh");
    let bake = nav.levels.as_ref().unwrap();
    assert_eq!(bake.levels, ["tower", "annex"]);
    let walk = route(&app, FLOOR, annex).unwrap_or_else(|e| panic!("into the annex: {e}"));
    assert!(walk.legs.iter().all(|l| l.kind == StepKind::Walk), "{}", walk.to_plan().summary());

    // The same levels placed again (a chapter restarted, a checkpoint loaded): every chunk kept.
    let nodes = nav.node_count();
    let placed: Vec<u32> = app.world().resource::<PlacedLevels>().levels.iter().map(|l| l.instance).collect();
    assert_eq!(placed[0], first);
    for instance in placed {
        despawn_level(app.world_mut(), instance);
    }
    spawn_level_by_id(app.world_mut(), "tower", None).unwrap();
    spawn_level_by_id(app.world_mut(), "annex", None).unwrap();
    refresh(app.world_mut());
    let again = region_nav(app.world()).expect("the navmesh of the levels placed again");
    let bake = again.levels.as_ref().unwrap().bake;
    assert_eq!(bake.reused, bake.chunks, "nothing baked twice");
    // The door is shut again, so its doorway is carved; the bakes are the same.
    assert_eq!(again.node_count(), nodes);
    // What a snapshot load runs builds the same navmesh.
    let digest = again.tile_graph_digest();
    rebuild(app.world_mut());
    assert_eq!(region_nav(app.world()).unwrap().tile_graph_digest(), digest);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_npc_climbs_the_stairs_to_a_player_on_the_walkway() {
    let (mut app, dir) = hall("chase", "tower");
    let hero = player(app.world_mut(), 0, WALKWAY);
    let grunt = grunt(app.world_mut(), "grunt", "grunt", "default", UNDER + Vec3::new(1.0, 0.0, 3.0));
    let mut top = 0.0f32;
    for _ in 0..900 {
        step(&mut app, 1);
        top = top.max(feet(&app, grunt).y);
        if flat_distance(feet(&app, grunt), feet(&app, hero)) < 2.5 && feet(&app, grunt).y > 2.9 {
            break;
        }
    }
    let at = feet(&app, grunt);
    assert!(at.y > 2.9 && flat_distance(at, feet(&app, hero)) < 2.5, "it came up to him: at {at:?}, highest {top:.2} ({})", state(&app, grunt));
    let _ = std::fs::remove_dir_all(dir);
}
