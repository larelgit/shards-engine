//! A game that draws (offscreen, with no GPU behind it): each content light gets a Bevy light on a child entity, driven
//! every frame, and the game's own lights are left alone.

use shards::bevy::camera::{Camera3d, Hdr};
use shards::bevy::light::{PointLight, SpotLight};
use shards::light::{ContentLight, LightPicture};
use shards::motion::camera::{CameraTuning, Grading, ThirdPersonCamera};
use shards::settings::GraphicsSettings;

use crate::*;

/// The picture of the content light `owner`: its child entity with a Bevy light.
fn picture(app: &mut App, owner: Entity) -> Entity {
    let world = app.world_mut();
    let mut found = world.query::<(Entity, &LightPicture)>();
    let all: Vec<Entity> = found.iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e).collect();
    assert_eq!(all.len(), 1, "one picture for a light, not {}", all.len());
    all[0]
}

fn point(app: &App, picture: Entity) -> PointLight {
    *app.world().get::<PointLight>(picture).expect("a point light")
}

#[test]
fn a_content_light_gets_a_bevy_light_on_a_child_that_follows_its_flicker() {
    let assets = Folder::new("point");
    let mut app = drawn(&assets);
    let blink = light(&mut app, "blink", "Lamp.B", Vec3::new(0.0, 2.0, 3.0));
    let steady = light(&mut app, "steady", "Lamp.A", Vec3::new(1.0, 2.0, 0.0));
    app.update();
    app.update();

    let child = picture(&mut app, blink);
    assert_eq!(app.world().get::<ChildOf>(child).map(ChildOf::parent), Some(blink), "the light is a child of the ContentLight");
    assert!(app.world().get::<shards::ai_core::AiId>(child).is_none(), "and has no AiId: a picture, not state");
    let first = point(&app, child);
    assert_eq!((first.intensity, first.range, first.shadow_maps_enabled), (2000.0, 10.0, false), "tick 0: the letter `m`, the light as written");

    // 45 ticks, three quarters of a second at two steps a second: the letter `a`, dark.
    step(&mut app, 45);
    app.update();
    assert_eq!(point(&app, child).intensity, 0.0);
    step(&mut app, 30);
    app.update();
    assert_eq!(point(&app, child).intensity, 2000.0, "and back");

    // The colour is the definition's: `#ff8000` in sRGB.
    let steady_child = picture(&mut app, steady);
    let orange = point(&app, steady_child);
    assert_eq!(orange.color, Color::srgb(1.0, 128.0 / 255.0, 0.0));
    assert!(orange.shadow_maps_enabled, "it asks for a shadow and the budget has room");
    // A steady light is written once: the next frame leaves it alone.
    let child = picture(&mut app, steady);
    let written = app.world().entity(child).get_ref::<PointLight>().unwrap().last_changed().get();
    app.update();
    app.update();
    assert_eq!(app.world().entity(child).get_ref::<PointLight>().unwrap().last_changed().get(), written, "a steady light is not rewritten");
}

#[test]
fn a_spot_has_its_cone_along_the_light_and_a_light_of_another_kind_is_made_again() {
    let assets = Folder::new("spot");
    let mut app = drawn(&assets);
    let lamp = light(&mut app, "beam", "Spot.A", Vec3::new(0.0, 3.0, 0.0));
    app.update();
    app.update();
    let child = picture(&mut app, lamp);
    let spot = *app.world().get::<SpotLight>(child).expect("a spot light");
    // The definition's angles are the cone's whole opening; Bevy's are half of it.
    assert!((spot.inner_angle - 10f32.to_radians()).abs() < 1e-6 && (spot.outer_angle - 30f32.to_radians()).abs() < 1e-6, "{spot:?}");
    assert_eq!((spot.range, spot.radius, spot.intensity), (8.0, 0.1, 500.0));
    assert!(app.world().get::<PointLight>(child).is_none());
    assert_eq!(app.world().get::<Transform>(child).copied(), Some(Transform::IDENTITY), "it points along the owner's -Z");

    // The same entity now shines by a point light: the spot's picture is replaced.
    app.world_mut().get_mut::<ContentLight>(lamp).unwrap().id = "steady".into();
    app.update();
    app.update();
    let child = picture(&mut app, lamp);
    assert!(app.world().get::<SpotLight>(child).is_none() && app.world().get::<PointLight>(child).is_some());
}

#[test]
fn the_budget_turns_shadows_on_and_off_on_the_lights_it_gives_them_to() {
    let assets = Folder::new("shadows");
    let mut app = drawn(&assets);
    camera(&mut app, Vec3::new(0.0, 2.0, 0.0));
    let near = light(&mut app, "steady", "Lamp.A", Vec3::new(1.0, 2.0, 0.0));
    let far = light(&mut app, "beam", "Lamp.D", Vec3::new(0.0, 8.0, 0.0));
    app.update();
    app.update();
    let shadow = |app: &mut App, owner: Entity| {
        let child = picture(app, owner);
        app.world().get::<PointLight>(child).map(|l| l.shadow_maps_enabled).or_else(|| app.world().get::<SpotLight>(child).map(|l| l.shadow_maps_enabled)).unwrap()
    };
    assert_eq!((shadow(&mut app, near), shadow(&mut app, far)), (true, true), "four may have one, two ask");
    app.world_mut().resource_mut::<GraphicsSettings>().shadow_lights = 1;
    app.update();
    assert_eq!((shadow(&mut app, near), shadow(&mut app, far)), (true, false), "the nearer keeps it");
    app.world_mut().resource_mut::<GraphicsSettings>().shadow_lights = 0;
    app.update();
    assert_eq!((shadow(&mut app, near), shadow(&mut app, far)), (false, false));
    app.world_mut().resource_mut::<GraphicsSettings>().shadow_lights = 4;
    app.update();
    assert_eq!((shadow(&mut app, near), shadow(&mut app, far)), (true, true), "and they come back");
}

#[test]
fn a_light_that_loses_its_content_light_or_its_definition_loses_its_picture_and_a_games_own_light_is_left_alone() {
    let assets = Folder::new("gone");
    let mut app = drawn(&assets);
    let own = app.world_mut().spawn((PointLight { intensity: 777.0, shadow_maps_enabled: true, ..default() }, Transform::from_xyz(3.0, 3.0, 3.0))).id();
    let lamp = light(&mut app, "steady", "Lamp.A", Vec3::ZERO);
    let ghost = light(&mut app, "ghost", "Lamp.G", Vec3::ONE);
    app.update();
    app.update();
    let count = |app: &mut App| {
        let world = app.world_mut();
        world.query::<&LightPicture>().iter(world).count()
    };
    assert_eq!(count(&mut app), 1, "the ghost has a content light and no definition: nothing is drawn for it");
    let child = picture(&mut app, lamp);
    assert_eq!(app.world().get::<ChildOf>(child).map(ChildOf::parent), Some(lamp));
    assert!(app.world().get::<PointLight>(ghost).is_none());
    app.world_mut().entity_mut(lamp).remove::<ContentLight>();
    app.update();
    app.update();
    assert_eq!(count(&mut app), 0, "the picture goes with the component");
    let own_light = app.world().get::<PointLight>(own).unwrap();
    assert_eq!((own_light.intensity, own_light.shadow_maps_enabled), (777.0, true), "the game's own light is its own");
}

/// The `lights` query says how much of the grade the world camera draws: without HDR Bevy grades in each material's
/// shader and draws the exposure alone, in HDR all of it; a grade of nothing but an exposure needs no word.
#[test]
fn the_query_says_how_much_of_the_grade_the_camera_draws() {
    let assets = Folder::new("drawn_grade");
    let mut app = drawn(&assets);
    let camera = app.world_mut().spawn((Camera3d::default(), ThirdPersonCamera::default())).id();
    app.world_mut().resource_mut::<CameraTuning>().picture.grading = Some(Grading { exposure: 0.5, temperature: 0.03, contrast: 1.1, ..default() });
    app.update();
    let said = ok(&mut app, "lights", json!({}));
    assert!(said.contains("shows: exposure 0.500, temperature 0.030, contrast 1.100"), "{said}");
    assert!(
        said.contains("  drawn: its exposure alone; the camera draws without HDR, where Bevy grades in each material's shader and leaves out temperature, contrast"),
        "{said}"
    );
    app.world_mut().entity_mut(camera).insert(Hdr);
    let said = ok(&mut app, "lights", json!({}));
    assert!(said.contains("  drawn: all of it (the camera draws in HDR)"), "{said}");
    app.world_mut().resource_mut::<CameraTuning>().picture.grading = Some(Grading { exposure: 0.5, ..default() });
    app.world_mut().entity_mut(camera).remove::<Hdr>();
    app.update();
    let said = ok(&mut app, "lights", json!({}));
    assert!(said.contains("shows: exposure 0.500") && !said.contains("drawn:"), "every camera draws an exposure: {said}");
}
