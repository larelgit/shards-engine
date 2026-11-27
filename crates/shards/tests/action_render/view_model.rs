//! The view model (F1b) on the headless engine: the first-person camera gets a second camera and a rig
//! for the view model of the weapon in hand, the animation bridge moves the rig from the body's actions
//! without a second fighter, a weapon switch swaps it, and nothing of it is part of the simulation.

use std::path::Path;

use shards::action::render::{AnimationBound, ViewModelCamera, ViewModelRig};
use shards::action::{ActionState, Animated, ShowsBody};
use shards::bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use shards::bevy::camera::{Camera, Camera3d, ClearColorConfig, Projection};
use shards::bevy::light::NotShadowCaster;
use shards::motion::camera::{CameraTuning, FirstPersonCamera, FirstPersonTuning, VIEW_MODEL_LAYER};
use shards::motion::{character, Collider, Shape};
use shards::prelude::*;
use shards::serde_json::json;
use shards::testing::ok;

use crate::fixtures::assets;
use crate::headless::update_until;

/// The combat content of the fixtures with a view model per moveset: the hero's is `arms`, the
/// brute's `other` (a narrower view), and `bare` names none.
const MOVESETS: &str = r#"{
    "hero": (inputs: {"light": "light_1"}, view_model: Some("arms")),
    "brute": (inputs: {"light": "slam"}, view_model: Some("other")),
    "bare": (inputs: {"light": "light_1"}),
}"#;

const VIEW_MODELS: &str = r#"{
    "arms": (model: "models/model.gltf", animset: "arms"),
    "other": (model: "models/model.gltf", animset: "arms", fov_deg: 45.0, offset: (0.1, -0.1, -0.2), scale: 0.5),
}"#;

/// The fixtures' humanoid set under another name, so the rig has an animset of its own.
const ARMS: &str = r#"{ "arms": (
    source: "models/humanoid.gltf",
    clips: {
        "idle": (name: "Idle"),
        "slash": (name: "Slash"),
    },
    locomotion: {"idle": "idle"},
    actions: {"light_1": "slash"},
) }"#;

struct World3 {
    app: App,
    hero: Entity,
    camera: Entity,
}

/// A fighter that is the local player, no model of its own, and a first-person camera.
fn scene(name: &str) -> World3 {
    let dir = assets(name);
    for (path, text) in [
        ("content/combat/movesets.moveset.ron", MOVESETS),
        ("content/anim/arms.animset.ron", ARMS),
        ("content/anim/arms.view_model.ron", VIEW_MODELS),
    ] {
        std::fs::write(dir.join(Path::new(path)), text).unwrap();
    }
    let mut app = shards::testing::test_app(|config| config.asset_dir = dir.to_string_lossy().into_owned(), |_| {});
    let errors: Vec<String> = app.world().resource::<ContentDb>().errors().map(ToString::to_string).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    app.world_mut().spawn((Collider::new(Shape::cuboid(Vec3::new(30.0, 0.5, 30.0))), Transform::from_xyz(0.0, -0.5, 0.0)));
    let (controller, intent, transform) = character("", Vec3::ZERO);
    let hero = app
        .world_mut()
        .spawn((Name::new("hero"), AiKind::new("Hero"), LocalPlayer, Fighter::new("hero"), Health::new(100.0), Stamina::new(100.0, 30.0, 0.8), controller, intent, transform))
        .id();
    let camera = app
        .world_mut()
        .spawn((Camera3d::default(), Camera::default(), FirstPersonCamera::default(), Projection::Perspective(PerspectiveProjection::default()), Transform::default()))
        .id();
    World3 { app, hero, camera }
}

fn rigs(app: &mut App) -> Vec<(Entity, ViewModelRig)> {
    let world = app.world_mut();
    world.query::<(Entity, &ViewModelRig)>().iter(world).map(|(e, r)| (e, r.clone())).collect()
}

fn view_cameras(app: &mut App) -> Vec<Entity> {
    let world = app.world_mut();
    world.query_filtered::<Entity, With<ViewModelCamera>>().iter(world).collect()
}

#[test]
fn the_first_person_camera_gets_a_second_camera_and_a_rig_for_the_weapon_in_hand() {
    let World3 { mut app, hero, camera } = scene("action_render_view_model");
    app.update();
    app.update();

    let cameras = view_cameras(&mut app);
    assert_eq!(cameras.len(), 1, "one view model camera");
    let vm = cameras[0];
    let world = app.world();
    assert_eq!(world.get::<ChildOf>(vm).map(|c| c.parent()), Some(camera), "it rides on the first-person camera");
    assert_eq!(world.get::<RenderLayers>(vm), Some(&RenderLayers::layer(VIEW_MODEL_LAYER)), "it draws the view model layer alone");
    let (world_camera, view_camera) = (world.get::<Camera>(camera).unwrap(), world.get::<Camera>(vm).unwrap());
    assert_eq!(view_camera.order, world_camera.order + 1, "after the world");
    assert!(matches!(view_camera.clear_color, ClearColorConfig::None), "it loads what the world drew and clears only the depth");
    let Some(Projection::Perspective(p)) = world.get::<Projection>(vm) else { panic!("perspective") };
    assert!((p.fov - 60f32.to_radians()).abs() < 1e-5, "the definition's own field of view, 60 by default: {}", p.fov.to_degrees());
    assert!(world.get::<AiId>(vm).is_none(), "the camera is a picture");

    let rigs_now = rigs(&mut app);
    assert_eq!(rigs_now.len(), 1);
    let (rig, built) = &rigs_now[0];
    assert_eq!((built.id.as_str(), built.body), ("arms", hero), "the hero's moveset names `arms`");
    let world = app.world();
    assert_eq!(world.get::<ChildOf>(*rig).map(|c| c.parent()), Some(vm));
    assert_eq!(world.get::<ShowsBody>(*rig), Some(&ShowsBody(hero)));
    assert_eq!(world.get::<Animated>(*rig).map(|a| a.animset.as_str()), Some("arms"));
    assert!(world.get::<AiId>(*rig).is_none() && world.get::<ActionState>(*rig).is_none(), "no id, and not a fighter: the machine never sees it");

    // The model loads under it; every mesh of it is on the view model layer, casts no shadow, is never culled.
    update_until(&mut app, "the model", |app| {
        let world = app.world();
        let mut meshes = app.world().try_query_filtered::<Entity, With<Mesh3d>>().unwrap();
        meshes.iter(world).any(|m| is_below(world, m, *rig))
    });
    app.update();
    app.update();
    let world = app.world_mut();
    let meshes: Vec<Entity> = world.query_filtered::<Entity, With<Mesh3d>>().iter(world).filter(|m| is_below(world, *m, *rig)).collect();
    assert!(!meshes.is_empty());
    for mesh in meshes {
        let e = world.entity(mesh);
        assert_eq!(e.get::<RenderLayers>(), Some(&RenderLayers::layer(VIEW_MODEL_LAYER)), "a mesh of the view model");
        assert!(e.contains::<NotShadowCaster>() && e.contains::<NoFrustumCulling>());
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
fn a_weapon_switch_swaps_the_view_model_and_a_body_with_none_shows_none() {
    let World3 { mut app, hero, camera } = scene("action_render_view_model_switch");
    app.update();
    app.update();
    let first = rigs(&mut app);
    assert_eq!(first.len(), 1);
    let (old_rig, _) = first[0].clone();
    let vm = view_cameras(&mut app)[0];

    // Another weapon: the brute's moveset names `other`, a narrower view that sits elsewhere.
    app.world_mut().get_mut::<Fighter>(hero).unwrap().moveset = "brute".into();
    app.update();
    app.update();
    let now = rigs(&mut app);
    assert_eq!(now.len(), 1, "the old rig is gone");
    let (rig, built) = &now[0];
    assert_ne!(*rig, old_rig);
    assert_eq!(built.id, "other");
    assert_eq!(view_cameras(&mut app), vec![vm], "the camera stays: only its numbers change");
    let world_ref = app.world();
    let Some(Projection::Perspective(p)) = world_ref.get::<Projection>(vm) else { panic!("perspective") };
    assert!((p.fov - 45f32.to_radians()).abs() < 1e-5, "{}", p.fov.to_degrees());
    let at = world_ref.get::<Transform>(*rig).unwrap();
    assert!((at.translation - Vec3::new(0.1, -0.1, -0.2)).length() < 1e-6 && (at.scale - Vec3::splat(0.5)).length() < 1e-6);

    // A moveset that names none, and the camera content names none: nothing is drawn.
    app.world_mut().get_mut::<Fighter>(hero).unwrap().moveset = "bare".into();
    app.update();
    app.update();
    assert!(rigs(&mut app).is_empty() && view_cameras(&mut app).is_empty(), "no view model, no second camera");

    // The camera content's own view model is the one for a body whose moveset names none.
    app.insert_resource(CameraTuning { first_person: FirstPersonTuning { view_model: Some("other".into()), ..default() }, ..default() });
    app.update();
    app.update();
    assert_eq!(rigs(&mut app).iter().map(|(_, r)| r.id.clone()).collect::<Vec<_>>(), ["other"]);

    // The camera stops looking through a body: the second camera goes with it.
    app.world_mut().entity_mut(camera).remove::<FirstPersonCamera>();
    app.update();
    app.update();
    assert!(rigs(&mut app).is_empty() && view_cameras(&mut app).is_empty());
}

#[test]
fn the_rig_plays_the_clip_of_the_bodys_action_though_it_is_no_fighter() {
    let World3 { mut app, hero, camera: _ } = scene("action_render_view_model_clips");
    update_until(&mut app, "the rig is bound", |app| {
        let world = app.world();
        world.try_query::<&AnimationBound>().is_some_and(|mut q| q.iter(world).next().is_some())
    });
    let rig = rigs(&mut app)[0].0;
    update_until(&mut app, "idle", |app| app.world().get::<AnimationBound>(rig).and_then(AnimationBound::clip) == Some("idle"));
    assert!(app.world().get::<AnimationBound>(hero).is_none(), "the hero has no model and no animation of its own");

    // The hero swings: the rig, which has no state of its own, plays the swing's clip from the hero's action.
    app.world_mut().get_mut::<Intent>(hero).unwrap().press("light");
    ok(&mut app, "step", json!({"ticks": 6}));
    app.update();
    app.update();
    assert!(app.world().get::<ActionState>(hero).unwrap().current.is_some());
    assert_eq!(app.world().get::<AnimationBound>(rig).unwrap().clip(), Some("slash"));
    assert!(app.world().get::<ActionState>(rig).is_none(), "no second fighter");
    // And back to idle when the swing ends.
    ok(&mut app, "step", json!({"ticks": 60}));
    app.update();
    assert_eq!(app.world().get::<AnimationBound>(rig).unwrap().clip(), Some("idle"));

    // The body is gone: there is nobody to show, and the view model goes with it.
    app.world_mut().despawn(hero);
    app.update();
    app.update();
    assert!(rigs(&mut app).is_empty() && view_cameras(&mut app).is_empty());
}

#[test]
fn a_world_camera_that_grades_in_its_shaders_hands_its_grade_to_the_view_model_camera() {
    use shards::bevy::camera::Hdr;
    use shards::bevy::render::view::ColorGrading;
    use shards::motion::camera::Grading;

    let World3 { mut app, camera, .. } = scene("action_render_view_model_grade");
    app.update();
    app.update();
    let vm = view_cameras(&mut app)[0];
    // A camera always has a grade (Bevy's required component, neutral by default); absent counts as neutral.
    let grade_of = |app: &App, e: Entity| app.world().get::<ColorGrading>(e).cloned().unwrap_or_default();
    assert_eq!(grade_of(&app, vm), ColorGrading::default(), "no grade, nothing to hand over");

    // A camera that does not draw in HDR tone maps and grades in each material's shader: the second camera must grade alike.
    let grade = Grading { temperature: 0.2, saturation: 0.5, ..default() }.bevy();
    app.world_mut().entity_mut(camera).insert(grade.clone());
    app.update();
    assert_eq!(grade_of(&app, vm), grade, "the view model is graded like the world");
    app.world_mut().entity_mut(camera).remove::<ColorGrading>();
    app.update();
    assert_eq!(grade_of(&app, vm), ColorGrading::default(), "and ungraded when the world is");

    // Over an HDR camera the second one still draws without HDR, graded and tone mapped in its shaders as before, into a
    // texture of its own (cleared to nothing) that is laid over the world's finished picture.
    use shards::bevy::camera::{CameraOutputMode, ClearColorConfig};
    use shards::bevy::render::view::Tonemapping;
    app.world_mut().entity_mut(camera).insert((Hdr, grade.clone(), Tonemapping::TonyMcMapface));
    app.update();
    app.update();
    assert_eq!(grade_of(&app, vm), grade, "under HDR the view model is graded like the world still");
    let world = app.world();
    assert!(world.get::<Hdr>(vm).is_none(), "and draws without HDR");
    assert_eq!(world.get::<Tonemapping>(vm), Some(&Tonemapping::TonyMcMapface), "tone mapped as the world is, in its shaders");
    let cam = world.get::<Camera>(vm).unwrap();
    assert!(matches!(cam.clear_color, ClearColorConfig::Custom(c) if c == Color::NONE), "its own texture starts empty: {:?}", cam.clear_color);
    assert!(
        matches!(cam.output_mode, CameraOutputMode::Write { blend_state: Some(_), clear_color: ClearColorConfig::None }),
        "and is laid over the world's picture: {:?}",
        cam.output_mode
    );

    // Without HDR again: the shared texture, loaded as the world drew it.
    app.world_mut().entity_mut(camera).remove::<Hdr>();
    app.update();
    app.update();
    let cam = app.world().get::<Camera>(vm).unwrap();
    assert!(matches!(cam.clear_color, ClearColorConfig::None) && matches!(cam.output_mode, CameraOutputMode::Write { blend_state: None, .. }));
}

#[test]
fn ssao_on_the_world_camera_turns_its_msaa_off_and_the_view_model_camera_follows_without_ssao_of_its_own() {
    use shards::bevy::pbr::ScreenSpaceAmbientOcclusion;
    use shards::bevy::render::view::Msaa;
    use shards::motion::camera::{PictureTuning, SsaoTuning};

    let World3 { mut app, camera, .. } = scene("action_render_view_model_ssao");
    // A window's camera has MSAA (the render plugin's required component); the bare test app's does not, so give it one.
    app.world_mut().entity_mut(camera).insert(Msaa::Sample4);
    app.update();
    app.update();
    let vm = view_cameras(&mut app)[0];
    let msaa = |app: &App, e: Entity| app.world().get::<Msaa>(e).copied();
    assert_eq!(msaa(&app, vm), Some(Msaa::Sample4), "the two cameras share one main texture, so one sample count");

    app.world_mut().resource_mut::<CameraTuning>().picture = PictureTuning { ssao: Some(SsaoTuning::default()), ..default() };
    app.update();
    app.update();
    assert!(app.world().get::<ScreenSpaceAmbientOcclusion>(camera).is_some());
    assert_eq!(msaa(&app, camera), Some(Msaa::Off), "Bevy draws no SSAO under MSAA");
    assert_eq!(msaa(&app, vm), Some(Msaa::Off), "the view model's camera follows the world camera");
    assert!(app.world().get::<ScreenSpaceAmbientOcclusion>(vm).is_none(), "the arms are drawn over the occluded world, with none of their own");

    app.world_mut().resource_mut::<CameraTuning>().picture = PictureTuning::default();
    app.update();
    app.update();
    assert!(app.world().get::<ScreenSpaceAmbientOcclusion>(camera).is_none());
    assert_eq!((msaa(&app, camera), msaa(&app, vm)), (Some(Msaa::Sample4), Some(Msaa::Sample4)), "both get their MSAA back");
}
