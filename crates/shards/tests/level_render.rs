//! The visual side of a level on the real engine: one glTF file read twice, by the level
//! loader for gameplay and by Bevy's own glTF loader for looks, with no render-only entity
//! among the gameplay ones. The headless engine keeps its render plugins without a GPU, so
//! scenes load here as they do in a window.
//!
//! The levels are fixtures of `shards_level`'s own tests: `arena.gltf`, written by the helper
//! that stands in for Blender's exporter, and `outpost.glb` and `outpost_embedded.gltf`, real
//! exports of Blender 5.2.2 (glTF Binary and glTF Embedded).

#![cfg(feature = "render")]

use std::time::{Duration, Instant};

use shards::ai_core::{AiId, AiIndex};
use shards::bevy::world_serialization::WorldAssetRoot;
use shards::level::{FromLevel, LevelVisual};
use shards::prelude::*;
use shards::serde_json::json;
use shards::testing::ok;

fn arena_dir() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../shards_level/tests/level/assets").to_owned()
}

fn update_until(app: &mut App, what: &str, done: impl Fn(&mut App) -> bool) {
    let started = Instant::now();
    while !done(app) {
        assert!(started.elapsed() < Duration::from_secs(30), "gave up waiting for: {what}");
        app.update();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn is_below(world: &World, mut entity: Entity, ancestor: Entity) -> bool {
    while let Some(parent) = world.get::<ChildOf>(entity) {
        entity = parent.parent();
        if entity == ancestor {
            return true;
        }
    }
    false
}

#[test]
fn a_level_shows_as_a_scene_beside_its_gameplay() {
    let mut app = shards::testing::test_app(|config| config.asset_dir = arena_dir(), |_| {});
    let errors: Vec<String> = app.world().resource::<ContentDb>().errors().map(ToString::to_string).collect();
    assert!(errors.is_empty(), "{errors:#?}");

    ok(&mut app, "spawn_level", json!({"id": "arena", "x": 10.0}));
    let ids_before = app.world().resource::<AiIndex>().ids();
    let gameplay: Vec<Entity> = {
        let world = app.world_mut();
        world.query::<(Entity, &FromLevel)>().iter(world).map(|(e, _)| e).collect()
    };
    assert_eq!(gameplay.len(), 18);

    // One root for the scene, placed like the level, holding Bevy's scene of the same file.
    let root = {
        let world = app.world_mut();
        let roots: Vec<Entity> = world.query_filtered::<Entity, With<LevelVisual>>().iter(world).collect();
        assert_eq!(roots.len(), 1);
        roots[0]
    };
    assert!(app.world().get::<WorldAssetRoot>(root).is_some());
    assert_eq!(app.world().get::<Transform>(root).unwrap().translation, Vec3::new(10.0, 0.0, 0.0));

    // The scene loads: the file's ten meshes appear below the root, under the names of the
    // Blender objects.
    update_until(&mut app, "the scene is instantiated", |app| {
        let world = app.world_mut();
        let meshes: Vec<Entity> = world.query_filtered::<Entity, With<Mesh3d>>().iter(world).collect();
        meshes.iter().filter(|m| is_below(world, **m, root)).count() == 10
    });
    let world = app.world_mut();
    let names: Vec<String> = world
        .query::<(Entity, &Name)>()
        .iter(world)
        .filter(|(e, _)| is_below(world, *e, root))
        .map(|(_, name)| name.as_str().to_owned())
        .collect();
    for wanted in ["Floor", "Pillar.001", "Pond", "Lamp", "PlayerStart"] {
        assert!(names.iter().any(|n| n == wanted), "no `{wanted}` in the scene: {names:?}");
    }

    // Looks add no gameplay: the same 18 entities, none inside the scene, no new AiId.
    let gameplay_now: Vec<Entity> = world.query::<(Entity, &FromLevel)>().iter(world).map(|(e, _)| e).collect();
    assert_eq!(gameplay_now, gameplay);
    assert!(gameplay.iter().all(|e| !is_below(world, *e, root)));
    assert!(gameplay.iter().all(|e| world.get::<Mesh3d>(*e).is_none()));
    assert_eq!(world.resource::<AiIndex>().ids(), ids_before);
    let _ = AiId(0);
}

/// The components the Blender level names that are not the engine's (`shards_level`'s tests
/// register the same).
#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
struct Loot {
    table: String,
    rolls: u32,
    tags: Vec<String>,
}

#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
struct Locked;

#[test]
fn a_blender_export_shows_the_scene_its_level_comes_from() {
    // `outpost.glb`, exported by Blender 5.2.2: scene 0 is `Kit` (the instanced collection's
    // own scene), the level is the default scene, 1. The glTF Embedded export of the same scene
    // keeps its buffer inside the file, as a `data:` URI, and shows alike.
    for level in ["outpost", "outpost_embedded"] {
        shows_its_scene(level);
    }
}

fn shows_its_scene(level: &str) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../shards_level/tests/level/blender").to_owned();
    let mut app = shards::testing::test_app(
        |config| config.asset_dir = dir,
        |app| {
            app.register_type::<Loot>().register_type::<Locked>();
        },
    );
    let errors: Vec<String> = app.world().resource::<ContentDb>().errors().map(ToString::to_string).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    ok(&mut app, "spawn_level", json!({"id": level}));
    let root = {
        let world = app.world_mut();
        world.query_filtered::<Entity, With<LevelVisual>>().single(world).unwrap()
    };
    // The level's fourteen meshes, the barrel of each collection instance among them.
    update_until(&mut app, &format!("the scene of {level} is instantiated"), |app| {
        let world = app.world_mut();
        let meshes: Vec<Entity> = world.query_filtered::<Entity, With<Mesh3d>>().iter(world).collect();
        meshes.iter().filter(|m| is_below(world, **m, root)).count() == 14
    });
    let world = app.world_mut();
    let names: Vec<String> = world
        .query::<(Entity, &Name)>()
        .iter(world)
        .filter(|(e, _)| is_below(world, *e, root))
        .map(|(_, name)| name.as_str().to_owned())
        .collect();
    // Bevy names the scene's root after it.
    for wanted in ["Outpost", "Ground", "Wall North", "Säule", "Костёр", "Barrels.A", "Barrels.B"] {
        assert!(names.iter().any(|n| n == wanted), "{level}: no `{wanted}` in the scene: {names:?}");
    }
    assert!(!names.iter().any(|n| n == "Kit"), "{names:?}");
}


// --- baked light ------------------------------------------------------------------------------------------------

/// A folder of its own for one test's assets, removed afterwards.
struct BakedAssets(std::path::PathBuf);

impl BakedAssets {
    fn put(&self, file: &str, bytes: &[u8]) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn dir(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for BakedAssets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A PNG of `width` x `height` (8-bit RGB, deflate's stored blocks, no compression), each pixel one of the colours
/// with channels 0 or 255.
fn png(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
    fn crc(bytes: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in bytes {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            raw.extend(pixel(x, y).map(|on| if on > 0 { 255 } else { 0 }));
        }
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    let len = raw.len() as u16;
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend(len.to_le_bytes());
    zlib.extend((!len).to_le_bytes());
    zlib.extend(&raw);
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::new();
    header.extend((width as u32).to_be_bytes());
    header.extend((height as u32).to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A level of two quads facing +Z, `Left` (x -2.1 to -0.1) and `Right` (0.1 to 2.1), 2 m high, each with a second
/// UV map over the whole quad (v 0 at the top) and a `lightmap` of `atlas.png` over the top (`Left`) or bottom
/// (`Right`) half of the image; and `Room`, an empty with an irradiance volume and a reflection probe.
fn baked_level() -> String {
    let mut bytes: Vec<u8> = Vec::new();
    let mut f32s = |values: &[f32]| values.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
    // Positions of both quads, then normals, then the UVs (one set, used for both maps).
    let quad = |x0: f32| [x0, -1.0, 0.0, x0 + 2.0, -1.0, 0.0, x0 + 2.0, 1.0, 0.0, x0, 1.0, 0.0];
    f32s(&quad(-2.1));
    f32s(&quad(0.1));
    f32s(&[0.0, 0.0, 1.0].repeat(4));
    f32s(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    for i in [0u16, 1, 2, 0, 2, 3] {
        bytes.extend(i.to_le_bytes());
    }
    let length = bytes.len();
    let view = |offset: usize, len: usize, target: u32| json!({"buffer": 0, "byteOffset": offset, "byteLength": len, "target": target});
    let mesh = |position: usize| {
        json!({"primitives": [{"attributes": {"POSITION": position, "NORMAL": 2, "TEXCOORD_0": 3, "TEXCOORD_1": 3}, "indices": 4, "material": 0}]})
    };
    let gltf = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 2]}],
        "nodes": [
            {"name": "Left", "mesh": 0, "extras": {"lightmap": "atlas.png", "lightmap_rect": [0.0, 0.0, 1.0, 0.5]}},
            {"name": "Right", "mesh": 1, "extras": {"lightmap": "atlas.png", "lightmap_rect": [0.0, 0.5, 1.0, 1.0]}},
            {"name": "Room", "scale": [6.0, 4.0, 6.0], "extras": {"irradiance_volume": "room.ktx2", "reflection_probe": "room", "probe_falloff": 0.2}},
        ],
        "meshes": [mesh(0), mesh(1)],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [1.0, 1.0, 1.0, 1.0], "metallicFactor": 0.0, "roughnessFactor": 1.0}}],
        "buffers": [{"byteLength": length, "uri": format!("data:application/octet-stream;base64,{}", base64(&bytes))}],
        "bufferViews": [view(0, 48, 34962), view(48, 48, 34962), view(96, 48, 34962), view(144, 32, 34962), view(176, 12, 34963)],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-2.1, -1.0, 0.0], "max": [-0.1, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0.1, -1.0, 0.0], "max": [2.1, 1.0, 0.0]},
            {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 3, "componentType": 5126, "count": 4, "type": "VEC2"},
            {"bufferView": 4, "componentType": 5123, "count": 6, "type": "SCALAR"},
        ],
    });
    gltf.to_string()
}

/// The assets of the baked level: two sets, `on` (an atlas red over green) and `off` (all blue); the probe files are
/// placeholders (a probe is checked for its components, not drawn).
fn baked_assets(tag: &str) -> BakedAssets {
    let assets = BakedAssets(std::env::temp_dir().join(format!("shards_baked_{tag}_{}", std::process::id())));
    assets.put("levels/lit.gltf", baked_level().as_bytes());
    assets.put("content/levels/lit.level.ron", br#"{ "lit": (file: "levels/lit.gltf") }"#);
    assets.put(
        "content/levels/lit.baked_light.ron",
        br#"{ "lit": (sets: { "on": "baked/on", "off": "baked/off" }, set: "on", lightmap_exposure: 2000.0, irradiance_intensity: 300.0) }"#,
    );
    assets.put("content/light/lamps.light.ron", br#"{ "bulb": (intensity: 16000.0, range: 20.0), "baked_bulb": (intensity: 16000.0, range: 20.0, baked: true) }"#);
    // Top two rows red, bottom two green: a mesh that reads the wrong half, or the image upside down, shows the other colour.
    assets.put("baked/on/atlas.png", &png(4, 4, |_, y| if y < 2 { [1, 0, 0] } else { [0, 1, 0] }));
    assets.put("baked/off/atlas.png", &png(4, 4, |_, _| [0, 0, 1]));
    for set in ["on", "off"] {
        for file in ["room.ktx2", "room_diffuse.ktx2", "room_specular.ktx2"] {
            assets.put(&format!("baked/{set}/{file}"), b"not read by the test");
        }
    }
    assets
}

/// The lightmap of each lightmapped mesh by its node's name: the image's path and the rect.
fn lightmaps(app: &mut App) -> Vec<(String, String, Rect)> {
    use shards::bevy::pbr::Lightmap;
    let world = app.world_mut();
    let mut out: Vec<(String, String, Rect)> = world
        .query::<(&Lightmap, &ChildOf)>()
        .iter(world)
        .map(|(lightmap, parent)| {
            let node = world.get::<Name>(parent.parent()).map(|n| n.as_str().to_owned()).unwrap_or_default();
            let path = lightmap.image.path().map(|p| p.path().to_string_lossy().replace('\\', "/")).unwrap_or_default();
            (node, path, lightmap.uv_rect)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn a_levels_baked_light_puts_lightmaps_and_probes_on_its_picture_and_switches_sets() {
    use shards::bevy::light::{EnvironmentMapLight, IrradianceVolume, LightProbe};
    use shards::level::BakedLight;

    let assets = baked_assets("wiring");
    let mut app = shards::testing::test_app(|config| config.asset_dir = assets.dir(), |_| {});
    let errors: Vec<String> = app.world().resource::<ContentDb>().errors().map(ToString::to_string).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    ok(&mut app, "spawn_level", json!({"id": "lit"}));
    // Nothing for gameplay: the baked nodes spawn no entity.
    let gameplay = {
        let world = app.world_mut();
        world.query::<&FromLevel>().iter(world).count()
    };
    assert_eq!(gameplay, 0, "baked light is the picture's alone");
    update_until(&mut app, "both quads are lightmapped", |app| lightmaps(app).len() == 2);
    let top = Rect::new(0.0, 0.0, 1.0, 0.5);
    let bottom = Rect::new(0.0, 0.5, 1.0, 1.0);
    assert_eq!(
        lightmaps(&mut app),
        [("Left".to_owned(), "baked/on/atlas.png".to_owned(), top), ("Right".to_owned(), "baked/on/atlas.png".to_owned(), bottom)]
    );
    // The material takes the set's exposure.
    let exposures: Vec<f32> = {
        let world = app.world_mut();
        let handles: Vec<_> = world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<shards::bevy::pbr::Lightmap>>().iter(world).map(|m| m.0.clone()).collect();
        let materials = world.resource::<Assets<StandardMaterial>>();
        handles.iter().map(|h| materials.get(h).unwrap().lightmap_exposure).collect()
    };
    assert_eq!(exposures, [2000.0, 2000.0]);
    // The probe box: a light probe with both kinds of baked light, from the same set.
    let (falloff, voxels, intensity, specular) = {
        let world = app.world_mut();
        let (probe, volume, reflection) = world.query::<(&LightProbe, &IrradianceVolume, &EnvironmentMapLight)>().single(world).expect("one probe");
        let path = |h: &Handle<Image>| h.path().map(|p| p.path().to_string_lossy().replace('\\', "/")).unwrap_or_default();
        (probe.falloff, path(&volume.voxels), volume.intensity, path(&reflection.specular_map))
    };
    assert_eq!((falloff, voxels.as_str(), intensity, specular.as_str()), (Vec3::splat(0.2), "baked/on/room.ktx2", 300.0, "baked/on/room_specular.ktx2"));

    // The story turns the lamps off: the game (here the tool) picks the other set, and every image follows.
    let said = ok(&mut app, "baked_light_set", json!({"level": "lit", "set": "off"})).result;
    assert_eq!(said, "level `lit` draws its baked set `off`");
    // The new image loads while the old one is drawn; then the old one is off for a frame (Bevy takes a new lightmap only
    // on a mesh with none) and the new one on.
    let started = Instant::now();
    loop {
        app.update();
        let paths: Vec<String> = lightmaps(&mut app).into_iter().map(|(_, path, _)| path).collect();
        if paths == ["baked/off/atlas.png", "baked/off/atlas.png"] {
            break;
        }
        assert!(paths.is_empty() || paths.iter().all(|p| p == "baked/on/atlas.png"), "a switch shows the old set or none, never a mix: {paths:?}");
        assert!(started.elapsed() < Duration::from_secs(30), "the `off` set was not put on: {paths:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    let said = ok(&mut app, "baked_light", json!({})).result.as_str().unwrap_or_default().to_owned();
    assert!(said.contains("lit: set `off` (chosen by the game) of off, on"), "{said}");
    assert!(said.contains("nodes: 2 with a lightmap, 1 irradiance volumes, 1 reflection probes"), "{said}");
    assert!(said.contains("drawn: 2 meshes lightmapped, 1 probes"), "{said}");
    // A wrong set is named, with the ones there are; back to the definition's own without `set`.
    let refused = shards::testing::call(&mut app, "baked_light_set", json!({"level": "lit", "set": "of"}));
    assert!(format!("{refused:?}").contains("no baked set `of` (did you mean `off`?)"), "{refused:?}");
    ok(&mut app, "baked_light_set", json!({"level": "lit"}));
    assert_eq!(app.world().resource::<BakedLight>().chosen("lit"), None);
    let started = Instant::now();
    while lightmaps(&mut app).iter().map(|(_, path, _)| path.as_str()).collect::<Vec<_>>() != ["baked/on/atlas.png", "baked/on/atlas.png"] {
        assert!(started.elapsed() < Duration::from_secs(30), "the definition's set was not put back");
        app.update();
    }
}

#[test]
#[ignore = "needs a GPU, or a software one (WGPU_FORCE_FALLBACK_ADAPTER=1): cargo test -p shards --test level_render -- --ignored offscreen"]
fn offscreen_a_lightmapped_mesh_draws_its_lightmap_and_still_takes_dynamic_lights() {
    use std::sync::{Arc, Mutex};

    use shards::ai_core::display::Offscreen;
    use shards::bevy::light::GlobalAmbientLight;
    use shards::bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
    use shards::bevy::render::view::{DebandDither, Msaa, Tonemapping};
    use shards::light::ContentLight;

    fn capture(app: &mut App) -> Image {
        let image = app.world().resource::<Offscreen>().image.clone();
        let slot: Arc<Mutex<Option<Image>>> = Arc::default();
        let into = slot.clone();
        app.world_mut().spawn(Screenshot::image(image)).observe(move |captured: On<ScreenshotCaptured>| {
            *into.lock().unwrap() = Some(captured.image.clone());
        });
        let started = Instant::now();
        loop {
            app.update();
            if let Some(image) = slot.lock().unwrap().take() {
                return image;
            }
            if started.elapsed() > Duration::from_secs(60) {
                let journal = app.world().resource::<shards::ai_core::AiJournal>();
                let errors: Vec<String> = journal.after(0).0.filter(|e| matches!(e.kind, shards::ai_core::JournalKind::Warn | shards::ai_core::JournalKind::Error)).map(|e| e.to_string()).collect();
                panic!("no screenshot within a minute: {errors:#?}");
            }
        }
    }
    /// The mean colour (0 to 255) of a 20 x 20 block around (x, y).
    fn mean(image: &Image, x: u32, y: u32) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        for dy in 0..20 {
            for dx in 0..20 {
                let [r, g, b, _] = image.get_color_at(x - 10 + dx, y - 10 + dy).unwrap().to_srgba().to_u8_array();
                for (s, c) in sum.iter_mut().zip([r, g, b]) {
                    *s += f32::from(c) / 400.0;
                }
            }
        }
        sum
    }

    const SIZE: (u32, u32) = (320, 160);
    let assets = baked_assets("offscreen");
    let dir = assets.dir();
    let mut app = shards::testing::test_app(
        move |config| {
            (config.headless, config.offscreen, config.window) = (false, true, SIZE);
            config.asset_dir = dir;
        },
        |app| {
            app.insert_resource(ClearColor(Color::BLACK)).insert_resource(GlobalAmbientLight { brightness: 0.0, ..default() });
        },
    );
    ok(&mut app, "spawn_level", json!({"id": "lit"}));
    // Straight at the quads, which fill the picture's middle: no anti-aliasing, tone curve or dither.
    let camera = (Camera3d::default(), Msaa::Off, Tonemapping::None, DebandDither::Disabled);
    app.world_mut().spawn((camera, Transform::from_xyz(0.0, 0.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y)));
    let (left, right) = ((SIZE.0 / 4, SIZE.1 / 2), (SIZE.0 * 3 / 4, SIZE.1 / 2));

    // The pipelines compile and the atlas loads a few frames in: wait for a lit picture.
    let started = Instant::now();
    let picture = loop {
        let picture = capture(&mut app);
        let (l, r) = (mean(&picture, left.0, left.1), mean(&picture, right.0, right.1));
        if l[0] > 40.0 && r[1] > 40.0 {
            break picture;
        }
        assert!(started.elapsed() < Duration::from_secs(120), "the lightmaps were not drawn within two minutes: left {l:?}, right {r:?}");
    };
    let (l, r) = (mean(&picture, left.0, left.1), mean(&picture, right.0, right.1));
    eprintln!("lightmapped: left {l:?}, right {r:?}");
    assert!(l[0] > 4.0 * (l[1] + l[2]).max(1.0), "the left quad covers the red top half of the atlas: {l:?}");
    assert!(r[1] > 4.0 * (r[0] + r[2]).max(1.0), "the right quad covers the green bottom half: {r:?}");

    // The other set: blue over both.
    ok(&mut app, "baked_light_set", json!({"level": "lit", "set": "off"}));
    let started = Instant::now();
    loop {
        let picture = capture(&mut app);
        let (l, r) = (mean(&picture, left.0, left.1), mean(&picture, right.0, right.1));
        if l[2] > 40.0 && r[2] > 40.0 && l[0] < 10.0 && r[1] < 10.0 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "the `off` set was not drawn: left {l:?}, right {r:?}");
    }
    let base = mean(&capture(&mut app), left.0, left.1);

    // A dynamic light in front of the quads adds its light to a lightmapped mesh; a light whose content says it is
    // baked adds no diffuse light to it (it is in the lightmap already).
    let lamp = app.world_mut().spawn((ContentLight::new("baked_bulb"), Transform::from_xyz(-1.1, 0.0, 1.0))).id();
    for _ in 0..5 {
        capture(&mut app);
    }
    let with_baked = mean(&capture(&mut app), left.0, left.1);
    app.world_mut().entity_mut(lamp).insert(ContentLight::new("bulb"));
    for _ in 0..5 {
        capture(&mut app);
    }
    let with_bulb = mean(&capture(&mut app), left.0, left.1);
    eprintln!("blue set {base:?}, a baked lamp {with_baked:?}, a dynamic lamp {with_bulb:?}");
    // The blue lightmap has no red: the red that comes is the lamp's (white light on a white surface).
    assert!(with_bulb[0] > base[0] + 60.0, "a dynamic light still lights the lightmapped quad: {base:?} -> {with_bulb:?}");
    // Only the baked lamp's faint shine (specular, which the bake does not hold) is left of it.
    assert!(with_baked[0] - base[0] < 0.35 * (with_bulb[0] - base[0]), "a baked lamp adds no diffuse to it: {base:?} -> {with_baked:?}");
}

/// What Bevy's SSAO costs the GPU at 1920x1080, at each quality, over a floor with a grid of blocks that fills the
/// picture: the GPU time of each render pass (Bevy's `RenderDiagnosticsPlugin`, timestamp queries) with no SSAO and
/// with it. A measurement, not a check: it prints the numbers for the manual ("What it costs").
#[test]
#[ignore = "a measurement on a GPU: cargo test -p shards --test level_render -- --ignored --nocapture offscreen_measure"]
fn offscreen_measure_what_ssao_costs_on_the_gpu() {
    use shards::bevy::diagnostic::DiagnosticsStore;
    use shards::bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel as Quality};
    use shards::bevy::render::diagnostic::RenderDiagnosticsPlugin;
    use shards::bevy::render::view::Msaa;

    const SIZE: (u32, u32) = (1920, 1080);
    let mut app = shards::testing::test_app(
        |config| (config.headless, config.offscreen, config.window) = (false, true, SIZE),
        |app| {
            if !app.is_plugin_added::<RenderDiagnosticsPlugin>() {
                app.add_plugins(RenderDiagnosticsPlugin);
            }
        },
    );
    let world = app.world_mut();
    let cube = world.resource_mut::<Assets<Mesh>>().add(Cuboid::new(1.0, 2.0, 1.0));
    let floor = world.resource_mut::<Assets<Mesh>>().add(Plane3d::default().mesh().size(60.0, 60.0));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.7, 0.7, 0.7), ..default() });
    world.spawn((Mesh3d(floor), MeshMaterial3d(grey.clone())));
    for x in -6..=6 {
        for z in -12..=0 {
            world.spawn((Mesh3d(cube.clone()), MeshMaterial3d(grey.clone()), Transform::from_xyz(x as f32 * 2.0, 1.0, z as f32 * 2.0)));
        }
    }
    world.spawn((PointLight { intensity: 2_000_000.0, range: 40.0, ..default() }, Transform::from_xyz(0.0, 6.0, 2.0)));
    let camera = world.spawn((Camera3d::default(), Msaa::Off, Transform::from_xyz(0.0, 3.0, 8.0).looking_at(Vec3::new(0.0, 0.0, -6.0), Vec3::Y))).id();

    // The GPU time of each pass over `frames` frames, after a warm-up (pipelines compile, timings arrive a frame late).
    let measure = |app: &mut App, frames: usize| -> Vec<(String, f64)> {
        for _ in 0..60 {
            app.update();
        }
        let mut store = app.world_mut().resource_mut::<DiagnosticsStore>();
        let paths: Vec<_> = store.iter().map(|d| d.path().clone()).filter(|p| p.as_str().starts_with("render/")).collect();
        for path in &paths {
            if let Some(d) = store.get_mut(path) {
                d.clear_history();
            }
        }
        for _ in 0..frames {
            app.update();
        }
        let store = app.world().resource::<DiagnosticsStore>();
        let mut out: Vec<(String, f64)> = store
            .iter()
            .filter(|d| d.path().as_str().starts_with("render/") && d.path().as_str().ends_with("elapsed_gpu"))
            .filter_map(|d| d.average().map(|a| (d.path().as_str().to_owned(), a)))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    };
    let total = |passes: &[(String, f64)]| -> f64 {
        // Only the top-level spans: a nested one is inside its parent's time.
        passes.iter().filter(|(p, _)| p.matches('/').count() == 2).map(|(_, t)| t).sum()
    };
    let without = measure(&mut app, 240);
    eprintln!("no SSAO: {:.3} ms on the GPU", total(&without));
    for (path, ms) in &without {
        eprintln!("    {path}: {ms:.3} ms");
    }
    assert!(!without.is_empty(), "no GPU timings: the adapter has no timestamp queries");
    for quality in [Quality::Low, Quality::Medium, Quality::High, Quality::Ultra] {
        app.world_mut().entity_mut(camera).insert(ScreenSpaceAmbientOcclusion { quality_level: quality, ..default() });
        let with = measure(&mut app, 240);
        let ssao: f64 = with.iter().filter(|(p, _)| p.contains("ssao")).map(|(_, t)| t).sum();
        let prepass: f64 = with.iter().filter(|(p, _)| p.contains("prepass")).map(|(_, t)| t).sum();
        eprintln!("SSAO {quality:?}: {:.3} ms on the GPU (+{:.3}); the ssao pass {ssao:.3} ms, the prepasses {prepass:.3} ms", total(&with), total(&with) - total(&without));
        for (path, ms) in &with {
            eprintln!("    {path}: {ms:.3} ms");
        }
    }
}
