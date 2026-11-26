//! The first-person view (F1b) on the sandbox: a shot of it from the GPU (the view model is drawn and
//! the own body is not), and how late the engine shows a click (the latency, F1 item 8).
//!
//! The headless halves are in `crates/shards_motion/tests/motion/camera/first_person.rs` (the camera, the
//! look, the layers), `crates/shards/tests/action_render/view_model.rs` (the second camera and the rig,
//! the bridge) and `scenarios/first_person.scenario` (the tool, the content, the ears).

use std::time::Duration;

use shards::action::render::AnimationBound;
use shards::action::ActionState;
use shards::ai_core::display::Offscreen;
use shards::ai_core::sim::SimTick;
use shards::bevy::camera::visibility::RenderLayers;
use shards::bevy::camera::{Camera, Camera3d};
use shards::bevy::input::mouse::MouseButtonInput;
use shards::bevy::input::{ButtonInput, ButtonState};
use shards::bevy::time::TimeUpdateStrategy;
use shards::core::Gaze;
use shards::motion::camera::{CameraTuning, FirstPersonCamera, OWN_BODY_LAYER};
use shards::prelude::*;
use shards::serde_json::json;

use crate::common::{ok, step};
use crate::offscreen::{bytes, capture_stable, offscreen_app, settle};

/// The player looks through its own eye, down at 35 degrees (where its body would be in view), in
/// the first person.
fn look_down_through_the_eye(app: &mut App) -> Entity {
    ok(app, "set", json!({"id": "e5", "component": "CharacterController", "path": "params", "value": "viewer"}));
    let player = app.world().resource::<shards::ai_core::AiIndex>().get(shards::ai_core::AiId::parse("e5").unwrap()).unwrap();
    app.world_mut().get_mut::<Intent>(player).unwrap().look = Some(Gaze::from_degrees(0.0, -35.0));
    ok(app, "camera_mode", json!({"mode": "first"}));
    player
}

/// How many pixels of the `(x, y, width, height)` region of two pictures of one size differ by more than 40 in some colour.
fn changed(a: &Image, b: &Image, (x, y, w, h): (u32, u32, u32, u32)) -> usize {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let width = a.width() as usize;
    let (a, b) = (bytes(a), bytes(b));
    let mut count = 0;
    for row in y..y + h {
        for col in x..x + w {
            let at = (row as usize * width + col as usize) * 4;
            if (0..3).any(|c| a[at + c].abs_diff(b[at + c]) > 40) {
                count += 1;
            }
        }
    }
    count
}

/// A `shot` and its file, once written (it waits for the pipelines too): the pictures of the PR.
fn shot_to(app: &mut App, name: &str) {
    let path = format!("shots/{name}-{}.png", std::process::id());
    let ticket = ok(app, "shot", json!({"path": path, "marks": false})).result;
    let png = ticket["path"].as_str().unwrap().to_owned();
    let started = std::time::Instant::now();
    while !std::path::Path::new(&png).exists() {
        app.update();
        assert!(started.elapsed() < Duration::from_secs(120), "no screenshot within two minutes");
    }
    if let Ok(keep) = std::env::var("SHARDS_KEEP_SHOTS") {
        let _ = std::fs::create_dir_all(&keep);
        let _ = std::fs::copy(&png, std::path::Path::new(&keep).join(format!("{name}.png")));
    }
    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_file(ticket["legend_path"].as_str().unwrap());
}

#[test]
#[ignore = "needs a GPU, or a software one (WGPU_FORCE_FALLBACK_ADAPTER=1): cargo test -p sandbox --test sandbox -- --ignored first_person"]
fn an_offscreen_first_person_shot_shows_the_view_model_and_not_the_own_body() {
    let mut app = offscreen_app();
    let picture = app.world().resource::<Offscreen>().image.clone();
    step(&mut app, 30);
    let player = look_down_through_the_eye(&mut app);
    // Frames: the camera changes over, the pipelines compile, the view model's file loads.
    settle(&mut app, 200);
    shot_to(&mut app, "first-person");

    // The picture with the view model, the own body not drawn.
    let with = capture_stable(&mut app, &picture);
    // The cameras and the layers are what the shot says.
    let world = app.world_mut();
    let (camera, layers) = world.query_filtered::<(Entity, Option<&RenderLayers>), (With<FirstPersonCamera>, With<Camera3d>)>().single(world).unwrap();
    assert_eq!(layers, None, "the first-person camera draws the default layer alone, so never the own body");
    let count = world.query_filtered::<&Camera, With<shards::action::render::ViewModelCamera>>().iter(world).count();
    assert_eq!(count, 1, "and a second camera draws the view model");
    let own_meshes = world.query_filtered::<&RenderLayers, With<Mesh3d>>().iter(world).filter(|l| **l == RenderLayers::layer(OWN_BODY_LAYER)).count();
    assert!(own_meshes > 0, "the body's model is on the own-body layer ({own_meshes} meshes)");

    // Without a view model: the world is the same, the lower half is not.
    app.world_mut().resource_mut::<CameraTuning>().first_person.view_model = None;
    settle(&mut app, 30);
    let without = capture_stable(&mut app, &picture);
    let world = changed(&with, &without, (300, 200, 980, 200));
    assert!(world < 50, "the world is the world: {world} pixels differ above the arms");
    let arms = changed(&with, &without, (230, 440, 870, 280));
    assert!(arms > 5000, "the lower part of the picture holds the arms: {arms} pixels differ with the view model and without");
    eprintln!("first-person shot: {world} pixels differ in the world above the arms, {arms} with the view model against without");
    shot_to(&mut app, "first-person-no-view-model");

    // The control: the same camera given the own-body layer shows the body below the eye (its shadow was always there).
    app.world_mut().resource_mut::<CameraTuning>().first_person.view_model = Some("arms".into());
    app.world_mut().entity_mut(camera).insert(RenderLayers::from_layers(&[0, OWN_BODY_LAYER]));
    settle(&mut app, 30);
    let with_body = capture_stable(&mut app, &picture);
    let body = changed(&with, &with_body, (300, 300, 700, 420));
    assert!(body > 3000, "with the layer the body is drawn below the eye: {body} pixels differ");
    eprintln!("first-person shot: {body} pixels differ when the own body is drawn too");
    shot_to(&mut app, "first-person-own-body-shown");
    assert!(app.world().get::<Intent>(player).is_some_and(|i| i.look.is_some()), "the look the camera turns is the player's");
}

/// What a click costs the engine, in simulated time (the clock Bevy steps by hand, so the numbers do
/// not depend on this machine).
#[derive(Debug, Clone, Copy)]
struct Click {
    /// The frames from the one the click was written before (that one counts 1) to the one whose tick
    /// started the action.
    frames_to_tick: u32,
    /// And to the first frame that shows the shot's clip on the view model.
    frames_to_picture: u32,
    /// From the start of the frame the click was written before to the end of that frame (milliseconds of the
    /// stepped clock): the picture is presented at the end of its frame.
    to_picture_ms: f64,
}

/// A fixed-update probe: the tick that started the player's action.
#[derive(Resource, Default)]
struct Probe {
    started: Option<u64>,
}

fn watch(ticks: Res<SimTick>, state: Query<&ActionState, With<LocalPlayer>>, mut probe: ResMut<Probe>) {
    if probe.started.is_none() && state.iter().any(|s| s.current.is_some()) {
        probe.started = Some(ticks.0);
    }
}

/// The player, in the first person with the test sidearm in hand, a view model, and Bevy's clock stepped
/// by `frame` seconds a frame.
fn clicking(tick_hz: f64, frame: f64) -> (App, Entity) {
    let mut app = sandbox::test_app_with(|config| config.tick_hz = tick_hz);
    look_down_through_the_eye(&mut app);
    ok(&mut app, "give_weapon", json!({"id": "e5", "moveset": "test_sidearm"}));
    app.world_mut().spawn((Camera3d::default(), Camera::default(), FirstPersonCamera::default(), Transform::default()));
    assert!(app.world().contains_resource::<ButtonInput<MouseButton>>(), "the engine reads the mouse through Bevy's input");
    app.init_resource::<Probe>().add_systems(FixedLast, watch);
    // The files load on the IO threads, on a clock that stands still (paused): frames until the rig is bound.
    let started = std::time::Instant::now();
    let rig = loop {
        app.update();
        std::thread::sleep(Duration::from_millis(2));
        let world = app.world_mut();
        let mut rigs = world.query_filtered::<(Entity, &AnimationBound), With<shards::action::render::ViewModelRig>>();
        if let Some((rig, _)) = rigs.iter(world).next() {
            break rig;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "the view model never bound");
    };
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(frame)));
    shards::ai_core::sim::resume(app.world_mut());
    // A few ticks first: the arsenal gives the body its charges on its first.
    for _ in 0..10 {
        app.update();
    }
    (app, rig)
}

fn clip(app: &App, rig: Entity) -> Option<String> {
    app.world().get::<AnimationBound>(rig).and_then(|b| b.clip()).map(str::to_owned)
}

/// The left mouse button goes down or up, as the window's event says it (the engine's input systems turn it
/// into `ButtonInput`, which the input map reads): an event for Bevy, never an OS input.
fn mouse(app: &mut App, state: ButtonState) {
    app.world_mut().write_message(MouseButtonInput { button: MouseButton::Left, state, window: Entity::PLACEHOLDER });
}

/// One click: the button goes down just before a frame, and the frames go on until the view model shows the shot.
fn click(app: &mut App, rig: Entity, frame: f64) -> Click {
    ok(app, "give_ammo", json!({"id": "e5", "charge": "sidearm", "n": 6}));
    app.world_mut().resource_mut::<Probe>().started = None;
    mouse(app, ButtonState::Pressed);
    let (mut to_tick, mut to_picture) = (None, None);
    for frames in 1..=30u32 {
        app.update();
        mouse(app, ButtonState::Released);
        if to_tick.is_none() && app.world().resource::<Probe>().started.is_some() {
            to_tick = Some(frames);
        }
        if to_picture.is_none() && clip(app, rig).as_deref() == Some("fire") {
            to_picture = Some(frames);
            break;
        }
    }
    let (to_tick, to_picture) = (to_tick.expect("the action started"), to_picture.expect("the view model showed the shot"));
    Click { frames_to_tick: to_tick, frames_to_picture: to_picture, to_picture_ms: f64::from(to_picture) * frame * 1000.0 }
}

/// Lets the shot end and the arms go back to idle, `extra` frames more than needed (so the next click
/// meets the tick grid at another phase).
fn rest(app: &mut App, rig: Entity, extra: u32) {
    for _ in 0..4000 {
        app.update();
        if app.world().get::<ActionState>(player_of(app)).is_some_and(|s| s.current.is_none()) && clip(app, rig).as_deref() == Some("idle") {
            break;
        }
    }
    for _ in 0..extra {
        app.update();
    }
}

fn player_of(app: &App) -> Entity {
    app.world().resource::<shards::ai_core::AiIndex>().get(shards::ai_core::AiId::parse("e5").unwrap()).unwrap()
}

#[test]
fn a_click_starts_the_action_on_the_next_tick_and_is_drawn_in_the_frame_of_that_tick() {
    let mut table = String::from("tick Hz | frame Hz | clicks | frames to the tick that starts the action (min/avg/max) | to the picture | ms to the picture\n");
    for (tick_hz, frame_hz) in [(60.0, 30.0), (60.0, 60.0), (60.0, 144.0), (60.0, 240.0), (120.0, 144.0)] {
        let frame = 1.0 / frame_hz;
        let (mut app, rig) = clicking(tick_hz, frame);
        rest(&mut app, rig, 0);
        let clicks: Vec<Click> = (0..16)
            .map(|i| {
                let c = click(&mut app, rig, frame);
                rest(&mut app, rig, i % 7);
                c
            })
            .collect();
        let tick = 1.0 / tick_hz * 1000.0;
        let stat = |f: &dyn Fn(&Click) -> f64| {
            let all: Vec<f64> = clicks.iter().map(f).collect();
            (all.iter().copied().fold(f64::MAX, f64::min), all.iter().sum::<f64>() / all.len() as f64, all.iter().copied().fold(f64::MIN, f64::max))
        };
        let frames = stat(&|c| f64::from(c.frames_to_tick));
        let frames_pic = stat(&|c| f64::from(c.frames_to_picture));
        let to_picture = stat(&|c| c.to_picture_ms);
        table += &format!(
            "{tick_hz:>6} | {frame_hz:>8} | {} | {:.0}/{:.1}/{:.0} | {:.0}/{:.1}/{:.0} | {:.1}/{:.1}/{:.1}\n",
            clicks.len(), frames.0, frames.1, frames.2, frames_pic.0, frames_pic.1, frames_pic.2, to_picture.0, to_picture.1, to_picture.2
        );
        for c in &clicks {
            // The click is read in the frame it is written before, and the first tick that runs after it starts the
            // action: at most one tick and one frame later. The picture is the frame of that tick (the bridge runs after
            // the ticks of its frame), so a frame that runs the tick shows the shot at once.
            assert!(c.to_picture_ms <= tick + frame * 1000.0 + 1e-6, "{tick_hz} Hz ticks, {frame_hz} Hz frames: {c:?}");
            assert_eq!(c.frames_to_picture, c.frames_to_tick, "the frame that ran the tick shows it: {c:?}");
        }
    }
    eprintln!("{table}");
}

/// The mouse moves `delta` pixels, as the window's event says it.
fn mouse_motion(app: &mut App, delta: Vec2) {
    app.world_mut().write_message(shards::bevy::input::mouse::MouseMotion { delta });
}

#[test]
fn a_mouse_move_turns_the_look_and_the_picture_in_its_frame_and_the_body_on_the_next_tick() {
    let mut table = String::from("tick Hz | frame Hz | frames to the look and the camera | frames to the body (min/avg/max)\n");
    for (tick_hz, frame_hz) in [(60.0, 30.0), (60.0, 60.0), (60.0, 144.0), (60.0, 240.0), (120.0, 144.0)] {
        let frame = 1.0 / frame_hz;
        let (mut app, rig) = clicking(tick_hz, frame);
        rest(&mut app, rig, 0);
        let player = player_of(&app);
        let yaw = |app: &App| app.world().get::<Transform>(player).unwrap().rotation.to_euler(EulerRot::YXZ).0;
        let mut to_body = Vec::new();
        for i in 0..16u32 {
            // Each turn is a quarter radian more to the left; the frames before it shift the tick grid.
            for _ in 0..(i % 5) {
                app.update();
            }
            let was = yaw(&app);
            let camera_was = {
                let world = app.world_mut();
                world.query_filtered::<&Transform, With<FirstPersonCamera>>().single(world).unwrap().rotation
            };
            mouse_motion(&mut app, Vec2::new(-100.0, 0.0));
            let mut seen = (None, None, None);
            for frames in 1..=20u32 {
                app.update();
                let look = app.world().get::<Intent>(player).unwrap().look.map(|l| l.yaw);
                let camera = {
                    let world = app.world_mut();
                    world.query_filtered::<&Transform, With<FirstPersonCamera>>().single(world).unwrap().rotation
                };
                if seen.0.is_none() && look.is_some_and(|l| (l - was).abs() > 0.2) {
                    seen.0 = Some(frames);
                }
                if seen.1.is_none() && camera.angle_between(camera_was) > 0.2 {
                    seen.1 = Some(frames);
                }
                if seen.2.is_none() && (yaw(&app) - was).abs() > 0.2 {
                    seen.2 = Some(frames);
                    break;
                }
            }
            let (look, camera, body) = (seen.0.expect("the look turned"), seen.1.expect("the camera turned"), seen.2.expect("the body turned"));
            assert_eq!((look, camera), (1, 1), "the look and the picture turn in the frame the mouse moved in ({tick_hz} Hz ticks, {frame_hz} Hz frames)");
            to_body.push(f64::from(body));
            // The mouse comes back, so the yaw stays near 0 and the next turn is measured from the same place.
            mouse_motion(&mut app, Vec2::new(100.0, 0.0));
            for _ in 0..8 {
                app.update();
            }
        }
        let (lo, hi) = (to_body.iter().copied().fold(f64::MAX, f64::min), to_body.iter().copied().fold(f64::MIN, f64::max));
        let ms = |frames: f64| frames * frame * 1000.0;
        table += &format!("{tick_hz:>6} | {frame_hz:>8} | 1 ({:.1} ms) | {lo:.0}/{:.1}/{hi:.0} ({:.1}/{:.1}/{:.1} ms)\n", ms(1.0), to_body.iter().sum::<f64>() / to_body.len() as f64, ms(lo), ms(to_body.iter().sum::<f64>() / to_body.len() as f64), ms(hi));
        assert!(ms(hi) <= 1000.0 / tick_hz + 2.0 * frame * 1000.0 + 1e-6, "the body turns within a tick and a frame of the look: {hi} frames at {frame_hz} Hz, {tick_hz} Hz ticks");
    }
    eprintln!("{table}");
}

/// The camera content's SSAO drawn for real, through the first-person camera and its view model's camera: the picture
/// darkens where things meet, a prepass another module takes away comes back without a lost frame, and taking SSAO
/// out gives the cameras their MSAA back. Every capture must arrive: a camera whose prepasses, MSAA and SSAO disagree
/// is an error Bevy logs and draws nothing for, or a wgpu validation error that stops the drawing.
#[test]
#[ignore = "needs a GPU, or a software one (WGPU_FORCE_FALLBACK_ADAPTER=1): cargo test -p sandbox --test sandbox -- --ignored first_person"]
fn ssao_from_the_camera_content_draws_through_the_first_person_cameras_and_goes_again() {
    use shards::bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
    use shards::bevy::pbr::ScreenSpaceAmbientOcclusion;
    use shards::bevy::render::view::Msaa;
    use shards::motion::camera::SsaoTuning;

    use crate::offscreen::{block, capture, mean_difference};

    // A picture once the pipelines are compiled (a `shot` waits for them): a change of the camera's passes compiles new
    // ones, and a frame drawn meanwhile leaves out what waits for them (a capture alike twice can still be one of those).
    let drawn = |app: &mut App, picture: &Handle<Image>| {
        shot_to(app, "ssao");
        capture_stable(app, picture)
    };

    let mut app = offscreen_app();
    let picture = app.world().resource::<Offscreen>().image.clone();
    step(&mut app, 30);
    look_down_through_the_eye(&mut app);
    settle(&mut app, 200);
    let plain = drawn(&mut app, &picture);
    let world = app.world_mut();
    let camera = world.query_filtered::<Entity, (With<FirstPersonCamera>, With<Camera3d>)>().single(world).unwrap();
    let view_model = world.query_filtered::<Entity, With<shards::action::render::ViewModelCamera>>().single(world).unwrap();
    let msaa = |app: &App, e: Entity| app.world().get::<Msaa>(e).copied();
    let own = msaa(&app, camera);

    app.world_mut().resource_mut::<CameraTuning>().picture.ssao = Some(SsaoTuning::default());
    settle(&mut app, 30);
    let occluded = drawn(&mut app, &picture);
    let world = app.world();
    assert!(world.get::<ScreenSpaceAmbientOcclusion>(camera).is_some() && world.get::<NormalPrepass>(camera).is_some());
    assert_eq!((msaa(&app, camera), msaa(&app, view_model)), (Some(Msaa::Off), Some(Msaa::Off)), "both cameras without MSAA");
    assert!(world.get::<ScreenSpaceAmbientOcclusion>(view_model).is_none(), "the arms have no SSAO of their own");
    let luma = |[r, g, b]: [f64; 3]| 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let (w, h) = (plain.width(), plain.height());
    let (before, after) = (luma(block(&plain, 0, 0, w, h)), luma(block(&occluded, 0, 0, w, h)));
    let differ = mean_difference(&plain, &occluded);
    eprintln!("SSAO in the sandbox's first person: luma {before:.2} -> {after:.2}, mean difference {differ:.3}");
    assert!(differ > 0.05 && after < before, "SSAO darkens the picture: luma {before:.2} -> {after:.2}, difference {differ:.3}");

    // A module takes the depth prepass away (the decals do when they end): it is back the next frame, and the frames go on.
    app.world_mut().entity_mut(camera).remove::<DepthPrepass>();
    for _ in 0..5 {
        capture(&mut app, picture.clone());
    }
    assert!(app.world().get::<DepthPrepass>(camera).is_some(), "the prepass SSAO needs is put back");
    let again = drawn(&mut app, &picture);
    assert!(mean_difference(&occluded, &again) < 1.0, "and the picture is the occluded one: {:.3}", mean_difference(&occluded, &again));

    // Out again: the cameras' own MSAA, no SSAO, the plain picture.
    app.world_mut().resource_mut::<CameraTuning>().picture.ssao = None;
    settle(&mut app, 30);
    let back = drawn(&mut app, &picture);
    assert!(app.world().get::<ScreenSpaceAmbientOcclusion>(camera).is_none());
    assert_eq!((msaa(&app, camera), msaa(&app, view_model)), (own, own), "the MSAA the camera had, on both");
    assert!(mean_difference(&plain, &back) < 1.0, "the plain picture again: {:.3}", mean_difference(&plain, &back));
}
