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
