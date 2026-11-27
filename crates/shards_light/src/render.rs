//! The pictures of the content lights (the `render` feature): each `ContentLight` gets a Bevy `PointLight` or
//! `SpotLight` on a child entity (`LightPicture`), without an `AiId`, driven every frame from
//! [`LightState`](crate::LightState): its colour, power (the flicker applied), range and whether the shadow budget
//! gives it a shadow. A game's own Bevy lights are not touched.
//!
//! The light is a child so that a `ContentLight` on a body (a lamp carried by a character) follows its drawn pose, and
//! so that the entity that carries `ContentLight` gets no component the headless game lacks.

use std::collections::BTreeMap;

use ai_core::display::drawn;
use bevy::light::{PointLight, SpotLight};
use bevy::prelude::*;

use crate::def::LightKind;
use crate::{LightInfo, LightState};

/// The Bevy light of a content light, a child of the entity that carries the `ContentLight`.
#[derive(Component, Clone, Copy, Debug)]
pub struct LightPicture {
    /// The entity with the `ContentLight`.
    pub owner: Entity,
}

pub(crate) fn plugin(app: &mut App) {
    app.add_systems(Update, drive_pictures.after(crate::update_state).run_if(drawn));
    // Without a GPU (a test, `--headless` with the feature on) there is no render world and nothing to clear.
    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        render_app.add_systems(
            bevy::render::Render,
            drop_stale_shadow_views.in_set(bevy::render::RenderSystems::CreateViews).before(bevy::pbr::prepare_lights),
        );
    }
}

/// The render world, before Bevy prepares the lights: a point or spot light that is out of every view loses its
/// shadow views.
///
/// Bevy 0.20 takes a light that left the views out of the lights it draws (`extract_lights` removes its
/// `ExtractedPointLight`), but leaves its shadow views alive: `prepare_lights` despawns the views of a light whose
/// shadow went only among the lights it still draws. Each of those views is a root view: the whole `Core3d` schedule
/// runs for it every frame (about as long as for a view that draws a shadow), until the light is seen again. With a
/// shadow budget that follows the player, every shadowed lamp left behind kept its views (a point light six of them):
/// a walk through a level, or a teleport, drew many more views than the budget. Taking the component off despawns
/// the views (Bevy's own observer on its removal), and a light seen again gets a new one with its
/// `ExtractedPointLight`, which requires it. Every light's, a game's own Bevy lights included: the views are Bevy's.
#[allow(clippy::type_complexity, reason = "a Bevy query filter")]
fn drop_stale_shadow_views(
    mut commands: Commands,
    stale: Query<Entity, (With<bevy::pbr::PointAndSpotLightViewEntities>, Without<bevy::pbr::ExtractedPointLight>)>,
) {
    for light in &stale {
        commands.entity(light).remove::<bevy::pbr::PointAndSpotLightViewEntities>();
    }
}

/// The half angle of a spot's cone in radians, from the whole opening in degrees.
fn half(deg: f32) -> f32 {
    (deg * 0.5).to_radians()
}

fn colour(rgb: [f32; 3]) -> Color {
    Color::srgb(rgb[0], rgb[1], rgb[2])
}

fn point_of(light: &LightInfo) -> Option<PointLight> {
    let def = light.def.as_ref()?;
    Some(PointLight {
        color: colour(light.srgb),
        intensity: light.lumens,
        range: def.range,
        radius: def.radius,
        shadow_maps_enabled: light.casts,
        affects_lightmapped_mesh_diffuse: !def.baked,
        ..default()
    })
}

fn spot_of(light: &LightInfo) -> Option<SpotLight> {
    let def = light.def.as_ref()?;
    let LightKind::Spot { inner_deg, outer_deg } = def.kind else { return None };
    Some(SpotLight {
        color: colour(light.srgb),
        intensity: light.lumens,
        range: def.range,
        radius: def.radius,
        shadow_maps_enabled: light.casts,
        inner_angle: half(inner_deg),
        outer_angle: half(outer_deg),
        affects_lightmapped_mesh_diffuse: !def.baked,
        ..default()
    })
}

/// Writes the named fields of `want` into `light`, each only where it differs, so that a steady light is never touched
/// (a write is what sets a component's change flag).
macro_rules! sync_fields {
    ($light:expr, $want:expr, [$($field:ident),+ $(,)?]) => {{
        let now = **$light;
        $(
            if now.$field != $want.$field {
                $light.$field = $want.$field;
            }
        )+
    }};
}

fn set_point(light: &mut Mut<PointLight>, want: &PointLight) {
    sync_fields!(light, want, [color, intensity, range, radius, shadow_maps_enabled, affects_lightmapped_mesh_diffuse]);
}

fn set_spot(light: &mut Mut<SpotLight>, want: &SpotLight) {
    sync_fields!(light, want, [color, intensity, range, radius, shadow_maps_enabled, inner_angle, outer_angle, affects_lightmapped_mesh_diffuse]);
}

/// `Update`, after the state: each content light has the Bevy light its definition and the budget say, and a
/// light whose definition is gone, whose kind changed or whose owner lost its `ContentLight` loses its picture.
fn drive_pictures(
    mut commands: Commands,
    state: Res<LightState>,
    mut pictures: Query<(Entity, &LightPicture, Option<&mut PointLight>, Option<&mut SpotLight>)>,
) {
    let wanted: BTreeMap<Entity, &LightInfo> = state.lights.iter().filter(|l| l.def.is_some()).map(|l| (l.entity, l)).collect();
    let mut covered: Vec<Entity> = Vec::new();
    for (picture, owner, point, spot) in &mut pictures {
        let Some(light) = wanted.get(&owner.owner) else {
            commands.entity(picture).despawn();
            continue;
        };
        match (light.def.as_ref().map(|d| d.kind), point, spot) {
            (Some(LightKind::Point), Some(mut point), None) => {
                if let Some(want) = point_of(light) {
                    set_point(&mut point, &want);
                }
            }
            (Some(LightKind::Spot { .. }), None, Some(mut spot)) => {
                if let Some(want) = spot_of(light) {
                    set_spot(&mut spot, &want);
                }
            }
            // Another kind than the picture has: it is made again below.
            _ => {
                commands.entity(picture).despawn();
                continue;
            }
        }
        covered.push(owner.owner);
    }
    for (owner, light) in wanted {
        if covered.contains(&owner) {
            continue;
        }
        let mut child = commands.spawn((Name::new("light picture"), LightPicture { owner }, Transform::IDENTITY, ChildOf(owner)));
        match light.def.as_ref().map(|d| d.kind) {
            Some(LightKind::Point) => {
                child.insert(point_of(light).expect("a point light has a picture"));
            }
            Some(LightKind::Spot { .. }) => {
                child.insert(spot_of(light).expect("a spot light has a picture"));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::pbr::{ExtractedPointLight, PointAndSpotLightViewEntities};

    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    fn extracted() -> ExtractedPointLight {
        ExtractedPointLight {
            color: LinearRgba::WHITE,
            intensity: 1.0,
            range: 10.0,
            radius: 0.0,
            transform: GlobalTransform::IDENTITY,
            shadow_maps_enabled: true,
            contact_shadows_enabled: false,
            shadow_depth_bias: 0.0,
            shadow_normal_bias: 0.0,
            shadow_map_near_z: 0.1,
            spot_light_angles: None,
            volumetric: false,
            soft_shadows_enabled: false,
            affects_lightmapped_mesh_diffuse: true,
        }
    }

    /// A light with shadow views, as `prepare_lights` leaves one: the views are entities named in its component.
    fn light_with_views(world: &mut World, views: usize, seen: bool) -> (Entity, Vec<Entity>) {
        let made: Vec<Entity> = (0..views).map(|_| world.spawn_empty().id()).collect();
        let mut of_light = PointAndSpotLightViewEntities::default();
        of_light.extend(made.iter().copied());
        let light = if seen { world.spawn((extracted(), of_light)).id() } else { world.spawn(of_light).id() };
        (light, made)
    }

    #[test]
    fn a_light_out_of_every_view_loses_its_shadow_views_and_a_light_in_view_keeps_them() {
        let mut world = World::new();
        // Bevy's own observer does the despawning; it is private, so the test has one that does the same.
        world.add_observer(|gone: On<Remove<PointAndSpotLightViewEntities>>, lights: Query<&PointAndSpotLightViewEntities>, mut commands: Commands| {
            if let Ok(views) = lights.get(gone.entity) {
                for view in views.iter().copied() {
                    commands.entity(view).despawn();
                }
            }
        });
        let (left_behind, its_views) = light_with_views(&mut world, 6, false);
        let (seen, seen_views) = light_with_views(&mut world, 1, true);
        world.run_system_once(drop_stale_shadow_views).unwrap();
        world.flush();
        assert!(world.get::<PointAndSpotLightViewEntities>(left_behind).is_none(), "the light left behind has no views");
        assert!(its_views.iter().all(|v| world.get_entity(*v).is_err()), "and its six views are gone");
        assert_eq!(world.get::<PointAndSpotLightViewEntities>(seen).map(|v| v.len()), Some(1), "the light in view keeps its view");
        assert!(world.get_entity(seen_views[0]).is_ok());
        // Seen again, the light gets the component back with its `ExtractedPointLight` (Bevy requires it).
        world.entity_mut(left_behind).insert(extracted());
        assert_eq!(world.get::<PointAndSpotLightViewEntities>(left_behind).map(|v| v.len()), Some(0));
    }
}
