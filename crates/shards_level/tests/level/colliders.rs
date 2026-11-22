//! Colliders and volumes from custom properties: every shape, sizes from the properties or
//! the mesh bounds, node scale, layers, and the physics layer seeing the result.

use bevy::prelude::*;
use serde_json::{json, Value};
use shards_level::FromLevel;
use shards_motion::{
    character, Body, Collider, ColliderId, CollisionLayers, LayerMask, PhysicsWorld, QueryFilter, Shape, Volume, VolumeKind,
};

use crate::asset::close;
use crate::common::*;
use crate::gltf::{Gltf, NodeSpec};

fn collider_of(app: &mut App, node: NodeSpec) -> Collider {
    let mut g = Gltf::new();
    g.root(node);
    let e = spawn(app, &g).unwrap()[0];
    *app.world().get::<Collider>(e).expect("a collider")
}

fn thing(props: Value) -> NodeSpec {
    NodeSpec::new("Thing").props(props)
}

#[test]
fn every_shape_takes_its_size_from_the_properties() {
    let mut app = app();
    let c = collider_of(&mut app, thing(json!({"collider": "box", "size": [2, 1, 4]})));
    assert_eq!(c.shape, Shape::Cuboid { half: Vec3::new(1.0, 0.5, 2.0) });
    assert_eq!((c.offset, c.layers), (Vec3::ZERO, CollisionLayers::WORLD));

    let c = collider_of(&mut app, thing(json!({"collider": "box", "size": 3})));
    assert_eq!(c.shape, Shape::Cuboid { half: Vec3::splat(1.5) });

    let c = collider_of(&mut app, thing(json!({"collider": "sphere", "radius": 0.5})));
    assert_eq!(c.shape, Shape::Sphere { radius: 0.5 });

    // A capsule is `height` tall in all, caps included.
    let c = collider_of(&mut app, thing(json!({"collider": "capsule", "radius": 0.4, "height": 1.8})));
    assert_eq!(c.shape, Shape::capsule(0.4, 1.8));
    assert!((c.shape.half_extents().y - 0.9).abs() < 1e-6);

    let c = collider_of(&mut app, thing(json!({"collider": "cylinder", "radius": 1, "height": 3})));
    assert_eq!(c.shape, Shape::Cylinder { half_height: 1.5, radius: 1.0 });

    let c = collider_of(&mut app, thing(json!({"collider": "cone", "radius": 1, "height": 2})));
    assert_eq!(c.shape, Shape::Cone { half_height: 1.0, radius: 1.0 });

    // Numbers typed as text count (Blender's string fields); the offset moves the center.
    let c = collider_of(&mut app, thing(json!({"collider": "sphere", "radius": "2.5", "offset": [0, 1, 0]})));
    assert_eq!((c.shape, c.offset), (Shape::Sphere { radius: 2.5 }, Vec3::Y));
}

#[test]
fn node_scale_goes_into_the_size() {
    let mut app = app();
    let c = collider_of(&mut app, thing(json!({"collider": "box", "size": [2, 1, 4]})).scale([1.0, 3.0, 0.5]));
    assert_eq!(c.shape, Shape::Cuboid { half: Vec3::new(1.0, 1.5, 1.0) });
    let c = collider_of(&mut app, thing(json!({"collider": "sphere", "radius": 0.5})).scale([2.0, 4.0, 3.0]));
    assert_eq!(c.shape, Shape::Sphere { radius: 2.0 }, "the biggest axis");
    let c = collider_of(&mut app, thing(json!({"collider": "cylinder", "radius": 1, "height": 2})).scale([2.0, 3.0, 1.0]));
    assert_eq!(c.shape, Shape::Cylinder { half_height: 3.0, radius: 2.0 }, "radius on the ground plane, height up");
    // The entity itself is unscaled: size is in the shape.
    let mut g = Gltf::new();
    g.root(thing(json!({"collider": "box", "size": 1})).scale([2.0, 3.0, 4.0]));
    let e = spawn(&mut app, &g).unwrap()[0];
    assert_eq!(app.world().get::<Transform>(e).unwrap().scale, Vec3::ONE);
}

#[test]
fn auto_fits_a_box_to_the_mesh_bounds_in_node_space() {
    let mut app = app();
    let mut g = Gltf::new();
    // A beam whose origin is at one end: bounds x -1..3, y 0..1, z -2..2.
    let beam = g.box_mesh("Beam", [-1.0, 0.0, -2.0], [3.0, 1.0, 2.0]);
    g.root(NodeSpec::new("Beam").at([10.0, 0.0, 0.0]).yaw(90.0).scale([2.0, 3.0, 1.0]).mesh(beam).props(json!({"collider": "auto"})));
    let e = spawn(&mut app, &g).unwrap()[0];
    let c = *app.world().get::<Collider>(e).unwrap();
    // Half extents (2, 0.5, 2) times the scale; center (1, 0.5, 0) times the scale. (The scale
    // comes out of a turned matrix: equal up to rounding.)
    let Shape::Cuboid { half } = c.shape else { panic!("{:?}", c.shape) };
    assert!(close(half, Vec3::new(4.0, 1.5, 2.0)), "{half}");
    assert!(close(c.offset, Vec3::new(2.0, 1.5, 0.0)), "{}", c.offset);

    // Where the box really is: turned a quarter, its long side (local X) runs along world Z,
    // from z = -6 to 2; its top is at y = 3.
    app.update();
    let physics = app.world().resource::<PhysicsWorld>();
    let down = |x: f32, z: f32| physics.raycast(Vec3::new(x, 10.0, z), Dir3::NEG_Y, 20.0, &QueryFilter::default());
    assert!((down(10.0, -2.0).unwrap().point.y - 3.0).abs() < 1e-3);
    assert!(down(10.0, -5.5).is_some() && down(10.0, 1.5).is_some());
    assert!(down(10.0, -6.5).is_none() && down(10.0, 2.5).is_none(), "beyond its ends");
    // Its width (local Z, 2 m each way) runs along world X: 8 to 12.
    assert!(down(8.5, -2.0).is_some() && down(11.5, -2.0).is_some());
    assert!(down(7.5, -2.0).is_none() && down(12.5, -2.0).is_none(), "beyond its width");
}

#[test]
fn a_mirrored_node_mirrors_the_offset() {
    let mut app = app();
    let mut g = Gltf::new();
    let beam = g.box_mesh("Beam", [0.0, 0.0, 0.0], [2.0, 1.0, 1.0]);
    g.root(NodeSpec::new("Beam").scale([-1.0, 1.0, 1.0]).mesh(beam).props(json!({"collider": "auto"})));
    let e = spawn(&mut app, &g).unwrap()[0];
    let c = *app.world().get::<Collider>(e).unwrap();
    assert_eq!(c.shape, Shape::Cuboid { half: Vec3::new(1.0, 0.5, 0.5) });
    // The beam stretched from the origin towards +X in Blender; mirrored, towards -X.
    assert!(close(c.offset, Vec3::new(-1.0, 0.5, 0.5)), "{}", c.offset);
    let w = app.world().get::<Transform>(e).unwrap();
    assert!(close(w.rotation * Vec3::X, Vec3::X) && w.scale == Vec3::ONE);
}

#[test]
fn sizes_the_file_leaves_out_come_from_the_mesh() {
    let mut app = app();
    let mut g = Gltf::new();
    let tall = g.box_mesh("Tall", [-1.0, 0.0, -1.0], [1.0, 4.0, 1.0]);
    for (name, props) in [
        ("Sphere", json!({"collider": "sphere"})),
        ("Capsule", json!({"collider": "capsule"})),
        ("Cylinder", json!({"collider": "cylinder", "radius": 0.5})),
        ("Box", json!({"collider": "box", "size": [1, 1, 1]})),
    ] {
        g.root(NodeSpec::new(name).mesh(tall).props(props));
    }
    let entities = spawn(&mut app, &g).unwrap();
    let shapes: Vec<Shape> = entities.iter().map(|e| app.world().get::<Collider>(*e).unwrap().shape).collect();
    assert_eq!(shapes[0], Shape::Sphere { radius: 2.0 }, "the biggest half extent");
    assert_eq!(shapes[1], Shape::capsule(1.0, 4.0));
    assert_eq!(shapes[2], Shape::Cylinder { half_height: 2.0, radius: 0.5 }, "radius given, height from the mesh");
    // An explicit size is centered on the mesh bounds, which is where the mesh is.
    let offsets: Vec<Vec3> = entities.iter().map(|e| app.world().get::<Collider>(*e).unwrap().offset).collect();
    assert!(offsets.iter().all(|o| close(*o, Vec3::new(0.0, 2.0, 0.0))), "{offsets:?}");
}

#[test]
fn a_collider_that_has_no_size_anywhere_is_an_error() {
    let mut app = app();
    let mut g = Gltf::new();
    g.root(NodeSpec::new("Bare").props(json!({"collider": "box"})));
    let err = spawn(&mut app, &g).unwrap_err();
    assert!(err.contains("node `Bare`: collider `box` takes its size from the node's mesh, and the node has none; give the size"), "{err}");

    // With all of it given, no mesh is needed.
    let c = collider_of(&mut app, thing(json!({"collider": "capsule", "radius": 0.3, "height": 1.0})));
    assert_eq!(c.shape, Shape::capsule(0.3, 1.0));
}

#[test]
fn shape_properties_that_do_not_fit_the_shape_are_errors() {
    let mut app = app();
    for (props, needle) in [
        (json!({"collider": "box", "radius": 1}), "`radius` does not apply to a `box` collider"),
        (json!({"collider": "sphere", "size": 1}), "`size` does not apply to a `sphere` collider"),
        (json!({"collider": "capsule", "size": 1}), "`size` does not apply to a `capsule` collider"),
        (json!({"collider": "box", "size": [1, 0, 1]}), "`size` must be above zero on every axis"),
        (json!({"collider": "sphere", "radius": -1}), "`radius` must be above zero, found -1"),
        (json!({"collider": "box", "size": [1, 1]}), "`size` must be [x, y, z] or one number (meters), found a list"),
        (json!({"collider": "box", "size": 1, "offset": 1}), "`offset` must be [x, y, z] (meters)"),
        (json!({"collider": "bxo"}), "unknown collider `bxo` (did you mean `box`?)"),
    ] {
        let err = spawn(&mut app, &one(props.clone())).unwrap_err();
        assert!(err.contains(needle), "{props}: no `{needle}` in {err}");
    }
}

#[test]
fn layers_default_by_role_and_follow_the_properties() {
    let mut app = app();
    let layers = |app: &mut App, props: Value| collider_of(app, thing(props)).layers;
    assert_eq!(layers(&mut app, json!({"collider": "box", "size": 1})), CollisionLayers::WORLD);
    assert_eq!(layers(&mut app, json!({"volume": "water", "collider": "box", "size": 1})), CollisionLayers::VOLUME);
    // Memberships named, filters left alone: solids hit everything, sensors characters.
    assert_eq!(
        layers(&mut app, json!({"collider": "box", "size": 1, "layers": ["world", "game0"]})),
        CollisionLayers::new(LayerMask::WORLD | LayerMask::game(0), LayerMask::ALL)
    );
    assert_eq!(
        layers(&mut app, json!({"volume": "water", "collider": "box", "size": 1, "layers": "game3"})),
        CollisionLayers::new(LayerMask::game(3), LayerMask::CHARACTER)
    );
    assert_eq!(
        layers(&mut app, json!({"collider": "box", "size": 1, "layers": "hitbox", "collides_with": ["hurtbox", "Character"]})),
        CollisionLayers::new(LayerMask::HITBOX, LayerMask::HURTBOX | LayerMask::CHARACTER)
    );
    let err = spawn(&mut app, &one(json!({"collider": "auto", "layers": "wrold"}))).unwrap_err();
    assert!(err.contains("`layers`: unknown layer `wrold` (did you mean `world`?)") && err.contains("game0..game15"), "{err}");
}

#[test]
fn volumes_are_sensors_shaped_by_the_mesh() {
    let mut app = app();
    let nodes = spawn_arena(&mut app);
    let w = app.world();
    let kind = |path: &str| w.get::<Volume>(nodes[path]).map(|v| v.kind.clone());
    assert_eq!(kind("Pond"), Some(VolumeKind::Water));
    assert_eq!(kind("Vent"), Some(VolumeKind::Updraft { accel: 12.0, max_speed: 8.0 }));
    assert_eq!(kind("GoalPad"), Some(VolumeKind::Trigger { name: "goal".into() }));
    assert_eq!(kind("Floor"), None);
    for path in ["Pond", "Vent", "GoalPad"] {
        let c = w.get::<Collider>(nodes[path]).unwrap();
        assert_eq!(c.layers, CollisionLayers::VOLUME, "{path}");
        assert_eq!(w.get::<Body>(nodes[path]), Some(&Body::Static));
    }
    assert_eq!(w.get::<Collider>(nodes["Pond"]).unwrap().shape, Shape::Cuboid { half: Vec3::new(2.5, 1.0, 2.5) });

    app.update();
    let physics = app.world().resource::<PhysicsWorld>();
    let at = |p: Vec3| physics.volumes_at(p, None).into_iter().map(|(_, k)| k).collect::<Vec<_>>();
    assert_eq!(at(Vec3::new(9.0, -0.5, -3.0)), [VolumeKind::Water]);
    assert_eq!(at(Vec3::new(-9.0, 5.0, -3.0)), [VolumeKind::Updraft { accel: 12.0, max_speed: 8.0 }]);
    assert_eq!(at(Vec3::new(0.0, 1.0, -6.0)), [VolumeKind::Trigger { name: "goal".into() }]);
    assert!(at(Vec3::new(0.0, 1.0, 0.0)).is_empty());
}

#[test]
fn a_ladder_is_a_volume_with_the_face_it_is_climbed_from() {
    let mut app = app();
    let mut g = Gltf::new();
    let cube = g.cube();
    // 0.3 m thick against a wall at x = 0, 0.8 m wide, from the floor to 3.5 m; climbed from +X.
    g.root(NodeSpec::new("Ladder").at([0.15, 1.75, 0.0]).scale([0.3, 3.5, 0.8]).mesh(cube).props(json!({"volume": "ladder", "face": [2, 0, 0]})));
    // The same ladder on a node turned a quarter: the face is the node's own, and turns with it.
    g.root(NodeSpec::new("Turned").at([8.0, 1.75, 0.0]).yaw(90.0).scale([0.3, 3.5, 0.8]).mesh(cube).props(json!({"volume": "ladder", "face": [1, 0, 0]})));
    spawn(&mut app, &g).unwrap();
    let nodes = by_node(&mut app);
    let w = app.world();
    for path in ["Ladder", "Turned"] {
        assert_eq!(w.get::<Volume>(nodes[path]).map(|v| v.kind.clone()), Some(VolumeKind::Ladder { face: Vec3::X }), "{path}: kept at length 1");
        assert_eq!(w.get::<Collider>(nodes[path]).unwrap().layers, CollisionLayers::VOLUME, "{path}");
        // The box is the node's own, whatever way it is turned (a turned node's scale comes back through its matrix).
        let Shape::Cuboid { half } = w.get::<Collider>(nodes[path]).unwrap().shape else { panic!("{path}: not a box") };
        assert!((half - Vec3::new(0.15, 1.75, 0.4)).length() < 1e-5, "{path}: {half}");
    }
    let turned = w.get::<Transform>(nodes["Turned"]).unwrap().rotation * Vec3::X;
    assert!((turned - Vec3::NEG_Z).length() < 1e-5, "a quarter turn about Y faces the ladder to -Z: {turned}");
    assert_eq!(VolumeKind::Ladder { face: Vec3::X }.to_string(), "ladder");

    app.update();
    let physics = app.world().resource::<PhysicsWorld>();
    let at = |p: Vec3| physics.volumes_at(p, None).into_iter().map(|(_, k)| k).collect::<Vec<_>>();
    assert_eq!(at(Vec3::new(0.2, 3.0, 0.0)), [VolumeKind::Ladder { face: Vec3::X }]);
    // It is a sensor: nothing solid stands there.
    assert!(physics.raycast(Vec3::new(3.0, 1.0, 0.0), Dir3::NEG_X, 6.0, &QueryFilter::default()).is_none());
}

#[test]
fn volumes_say_what_is_missing() {
    let mut app = app();
    for (props, needle) in [
        (json!({"volume": "ladder"}), "a ladder needs `face`: `[x, y, z]`, the way out of the ladder toward the side it is climbed from"),
        (json!({"volume": "ladder", "face": [0, 1, 0]}), "`face` must point sideways out of the ladder"),
        (json!({"volume": "ladder", "face": "east"}), "`face` must be [x, y, z], found"),
        (json!({"volume": "water", "face": [1, 0, 0]}), "`face` does nothing on this node: it is the side a ladder volume is climbed from"),
        (json!({"volume": "lader", "face": [1, 0, 0]}), "unknown volume `lader` (did you mean `ladder`?); volumes: water, updraft, ladder, trigger"),
        (json!({"volume": "updraft"}), "an updraft needs `accel` (m/s²) and `max_speed` (m/s), both above zero"),
        (json!({"volume": "updraft", "accel": 5}), "an updraft needs `accel`"),
        (json!({"volume": "updraft", "accel": 5, "max_speed": 0}), "`max_speed` must be above zero"),
        (json!({"volume": "wter"}), "unknown volume `wter` (did you mean `water`?)"),
        (json!({"volume": "water", "accel": 1}), "`accel` does nothing on this node"),
    ] {
        let err = spawn(&mut app, &one(props.clone())).unwrap_err();
        assert!(err.contains(needle), "{props}: no `{needle}` in {err}");
    }
    // A volume without a mesh needs its shape spelled out.
    let mut g = Gltf::new();
    g.root(NodeSpec::new("Zone").props(json!({"volume": "water"})));
    assert!(spawn(&mut app, &g).unwrap_err().contains("takes its size from the node's mesh"));
    let mut g = Gltf::new();
    g.root(NodeSpec::new("Zone").props(json!({"volume": "water", "collider": "box", "size": [4, 2, 4]})));
    assert!(spawn(&mut app, &g).is_ok());
}

#[test]
fn the_physics_layer_sees_the_level_as_top_level_entities() {
    let mut app = app();
    let nodes = spawn_arena(&mut app);
    app.update();
    let physics = app.world().resource::<PhysicsWorld>();
    let ray = |x: f32, z: f32| physics.raycast(Vec3::new(x, 10.0, z), Dir3::NEG_Y, 30.0, &QueryFilter::default()).expect("a hit");

    let pillar = ray(6.0, 6.0);
    assert_eq!(pillar.entity, nodes["Pillars/Pillar.001"]);
    assert!((pillar.point.y - 4.0).abs() < 1e-3, "{}", pillar.point);
    let floor = ray(0.0, 0.0);
    assert_eq!(floor.entity, nodes["Floor"]);
    assert!(floor.point.y.abs() < 1e-3);
    // Sensors do not block a ray unless asked for: the pond's surface is the floor's.
    assert_eq!(ray(9.0, -3.0).entity, nodes["Floor"]);
    let crate_top = ray(-3.0, -3.0);
    assert_eq!((crate_top.entity, (crate_top.point.y * 10.0).round()), (nodes["Crate.003"], 10.0));

    // Nothing hangs from a parent: physics reads `Transform`, which is not propagated in ticks.
    for (path, e) in &nodes {
        assert!(app.world().get::<ChildOf>(*e).is_none(), "{path} has a parent");
    }
}

#[test]
fn collider_ids_follow_the_file() {
    let mut app = app();
    let nodes = spawn_arena(&mut app);
    let mut ids: Vec<(u32, ColliderId)> = nodes
        .values()
        .filter_map(|e| Some((app.world().get::<FromLevel>(*e)?.index, *app.world().get::<ColliderId>(*e)?)))
        .collect();
    ids.sort();
    // Floor, 4 pillars, pond, vent, goal, crate, gate: 10 colliders, numbered in file order.
    assert_eq!(ids.len(), 10);
    assert!(ids.windows(2).all(|w| w[1].1 .0 == w[0].1 .0 + 1), "{ids:?}");
    assert_eq!(app.world().get::<ColliderId>(nodes["Floor"]).copied(), Some(ids[0].1));
}

#[test]
fn a_character_stands_on_the_level_floor() {
    let mut app = app();
    spawn_arena(&mut app);
    let (controller, intent, transform) = character("", Vec3::new(1.0, 1.5, 1.0));
    let hero = app.world_mut().spawn((Name::new("Hero"), controller, intent, transform)).id();
    step(&mut app, 90);
    let feet = app.world().get::<Transform>(hero).unwrap().translation;
    assert!(feet.y.abs() < 0.05, "standing on the floor: {feet}");
}
