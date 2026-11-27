//! The view model on screen (F1b): a second camera over the first-person camera, and the rig it
//! draws. See [`crate::view_model`] for what a view model is.
//!
//! - **The camera** ([`ViewModelCamera`]) is a child of the first-person camera, so it moves with it
//!   (the shake, the bob and the lean included), one `order` above it, with the `view_model` definition's
//!   own field of view and near plane. It draws [`VIEW_MODEL_LAYER`] alone and loads what the world camera
//!   drew, clearing only the depth (`Camera3d`'s default), so the model is drawn over the world and never
//!   inside a wall. It mirrors the world camera's anti-aliasing, tone mapping and grade, so that the two
//!   share one main texture. The UI goes to the camera that draws last, so the HUD is over the arms.
//! - **Over an HDR world camera** (a bloom) it still draws without HDR, as it does over one without: its
//!   materials tone map in their shaders and take the world camera's grade (its exposure; [`ColorGrading`]
//!   is drawn in full only in HDR), so the arms are drawn as they were before the world went HDR. The two
//!   formats cannot share a texture: the view model draws into its own, cleared to nothing, and that is laid
//!   over the world's finished picture (premultiplied alpha, [`CameraOutputMode`]), with Bevy's 4x MSAA of
//!   its own whatever the world camera has (SSAO turns the world's off). An HDR view model drawn into the
//!   world's texture would come after the world's tone mapping and grade, and be drawn with neither.
//! - **The rig** ([`ViewModelRig`]) is a child of the camera: an [`Animated`] root that [`ShowsBody`] of
//!   the looked-through body, so the bridge animates it from that body's actions, with a [`GltfModel`] below.
//!   Which definition it is follows the body's moveset each frame, so a weapon switch swaps it; an edit of
//!   the definition builds it again.
//! - **The meshes** under a rig are put on [`VIEW_MODEL_LAYER`] as they appear (the model loading, a
//!   prop the bridge mounts), cast no shadow and are never culled by their (possibly stale) bounds.

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers, VisibilitySystems};
use bevy::camera::{Camera, Camera3d, CameraOutputMode, ClearColorConfig, Hdr, PerspectiveProjection, Projection};
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::render::view::{ColorGrading, Msaa, Tonemapping};
use shards_content::ContentDb;
use shards_core::LocalPlayer;
use shards_look::Look;
use shards_motion::camera::{CameraTuning, FirstPersonCamera, Shots, VIEW_MODEL_LAYER};

use super::model::GltfModel;
use crate::anim::Animated;
use crate::def::MovesetDef;
use crate::fighter::Fighter;
use crate::view_model::{ShowsBody, ViewModelDef};

/// The camera that draws the view model, a child of a first-person camera.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ViewModelCamera;

/// The root of a drawn view model: which definition it was built from and for which body.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ViewModelRig {
    /// The id of the `view_model` definition.
    pub id: String,
    /// The definition as it was when the rig was built (an edit builds it again).
    pub def: ViewModelDef,
    /// The body whose actions it shows.
    pub body: Entity,
}

/// A mesh under a rig that has been put on the view model layer.
#[derive(Component, Clone, Copy, Debug, Default)]
struct ViewModelMesh;

pub(super) fn plugin(app: &mut App) {
    app.add_systems(Update, sync_view_model.run_if(views_exist))
        .add_systems(PostUpdate, dress_view_model.before(VisibilitySystems::CheckVisibility));
}

/// The first-person cameras, with what their picture is made with (the second camera copies it).
type WorldCameras<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static FirstPersonCamera, &'static Camera, Has<Hdr>, Option<&'static Msaa>, Option<&'static Tonemapping>, Option<&'static ColorGrading>),
    Without<ViewModelCamera>,
>;

/// The view model cameras, with the same.
type ViewCameras<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static ChildOf, &'static mut Camera, &'static mut Projection, Has<Hdr>, Option<&'static Msaa>, Option<&'static Tonemapping>, Option<&'static ColorGrading>),
    With<ViewModelCamera>,
>;

/// Run condition: a first-person camera, or a view model camera that may have to go.
fn views_exist(first: Query<(), With<FirstPersonCamera>>, views: Query<(), With<ViewModelCamera>>) -> bool {
    !first.is_empty() || !views.is_empty()
}

/// Whether two clear colours are the same (Bevy's `ClearColorConfig` has no `PartialEq`).
fn same_clear(a: &ClearColorConfig, b: &ClearColorConfig) -> bool {
    match (a, b) {
        (ClearColorConfig::Default, ClearColorConfig::Default) | (ClearColorConfig::None, ClearColorConfig::None) => true,
        (ClearColorConfig::Custom(x), ClearColorConfig::Custom(y)) => x == y,
        _ => false,
    }
}

/// The view model a body shows now: its moveset's, else the camera content's default. `None`: no
/// view model (the body has none to show, or the definition is missing).
pub fn view_model_of<'a>(db: &'a ContentDb, tuning: &'a CameraTuning, moveset: Option<&str>) -> Option<(&'a str, &'a ViewModelDef)> {
    let named = moveset.and_then(|m| db.get::<MovesetDef>(m)).and_then(|m| m.view_model.as_deref());
    let id = named.or(tuning.first_person.view_model.as_deref())?;
    db.get::<ViewModelDef>(id).map(|def| (id, def))
}

/// `Update`: each first-person camera gets the camera and the rig for the view model its body shows
/// now, and loses them when it shows none, or when the camera is gone.
#[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
fn sync_view_model(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    db: Option<Res<ContentDb>>,
    tuning: Res<CameraTuning>,
    shots: Option<Res<Shots>>,
    players: Query<Entity, With<LocalPlayer>>,
    bodies: Query<Option<&Fighter>>,
    worlds: WorldCameras,
    mut cameras: ViewCameras,
    rigs: Query<(Entity, &ViewModelRig, &ChildOf)>,
) {
    // A camera whose world camera stopped looking through a body.
    for (entity, parent, ..) in &cameras {
        if !worlds.contains(parent.parent()) {
            commands.entity(entity).despawn();
        }
    }
    let Some(db) = db else { return };
    let player = players.single().ok();
    // A scene camera shot (K3) has the view: the arms are not drawn over it.
    let in_shot = shots.is_some_and(|s| s.playing().is_some());
    for (world, first, world_camera, hdr, msaa, tonemapping, world_grading) in &worlds {
        let subject = if first.target == Entity::PLACEHOLDER { player } else { Some(first.target) };
        let shown = subject.and_then(|body| {
            let moveset = bodies.get(body).ok()?.map(|f| f.moveset.as_str());
            view_model_of(&db, &tuning, moveset).map(|(id, def)| (body, id, def))
        });
        let existing = cameras.iter_mut().find(|(_, parent, ..)| parent.parent() == world);
        let Some((body, id, def)) = shown else {
            if let Some((camera, ..)) = existing {
                commands.entity(camera).despawn();
            }
            continue;
        };

        // The projection, the order and the picture settings follow the world camera and the definition.
        let (fov, near) = (def.fov_deg.to_radians(), def.near);
        let projection = Projection::Perspective(PerspectiveProjection { fov, near, ..default() });
        let right_projection = |p: &Projection| matches!(p, Projection::Perspective(p) if (p.fov - fov).abs() < 1e-6 && (p.near - near).abs() < 1e-9);
        // The view model never draws in HDR: it tone maps (and grades, F13) in its materials' shaders, with the world
        // camera's tone mapping and grade, so the arms are drawn alike over a world camera with HDR and without.
        let (tone, grading) = (tonemapping.copied(), world_grading.cloned());
        // Sharing a texture, the two must have the same MSAA. Over an HDR camera the view model has a texture of its own
        // and keeps Bevy's 4x MSAA for the arms, though the world camera may have none (SSAO turns it off).
        let msaa = if hdr { Some(&Msaa::Sample4) } else { msaa };
        // Over a world camera without HDR it draws into the same texture, over what the world drew. Over an HDR one (whose
        // texture is of another format) it draws into its own, cleared to nothing, and lays that over the world's picture.
        let (clear, output) = if hdr {
            let blend = CameraOutputMode::Write { blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING), clear_color: ClearColorConfig::None };
            (ClearColorConfig::Custom(Color::NONE), blend)
        } else {
            (ClearColorConfig::None, CameraOutputMode::default())
        };
        let same_output = |a: &CameraOutputMode| match (a, &output) {
            (CameraOutputMode::Write { blend_state: x, clear_color: c }, CameraOutputMode::Write { blend_state: y, clear_color: d }) => x == y && same_clear(c, d),
            (CameraOutputMode::Skip, CameraOutputMode::Skip) => true,
            _ => false,
        };
        let camera = match existing {
            Some((camera, _, mut cam, mut proj, has_hdr, has_msaa, has_tone, has_grading)) => {
                let active = world_camera.is_active && !in_shot;
                if cam.order != world_camera.order + 1 || cam.is_active != active {
                    cam.order = world_camera.order + 1;
                    cam.is_active = active;
                }
                if !same_clear(&cam.clear_color, &clear) || !same_output(&cam.output_mode) {
                    cam.clear_color = clear;
                    cam.output_mode = output;
                }
                if !right_projection(&proj) {
                    *proj = projection;
                }
                let mut e = commands.entity(camera);
                if has_hdr {
                    e.remove::<Hdr>();
                }
                if has_msaa.copied() != msaa.copied()
                    && let Some(msaa) = msaa
                {
                    e.insert(*msaa);
                }
                if has_tone.copied() != tone
                    && let Some(tone) = tone
                {
                    e.insert(tone);
                }
                if has_grading != grading.as_ref() {
                    match &grading {
                        Some(grading) => e.insert(grading.clone()),
                        None => e.remove::<ColorGrading>(),
                    };
                }
                camera
            }
            None => {
                let mut e = commands.spawn((
                    Name::new("view model camera"),
                    ViewModelCamera,
                    Camera3d::default(),
                    Camera { order: world_camera.order + 1, is_active: world_camera.is_active && !in_shot, clear_color: clear, output_mode: output, ..default() },
                    projection,
                    RenderLayers::layer(VIEW_MODEL_LAYER),
                    Transform::IDENTITY,
                    ChildOf(world),
                ));
                if let Some(msaa) = msaa {
                    e.insert(*msaa);
                }
                if let Some(tone) = tone {
                    e.insert(tone);
                }
                if let Some(grading) = grading {
                    e.insert(grading);
                }
                e.id()
            }
        };

        // The rig: built again when the definition, its id or the body changes.
        let rig = rigs.iter().find(|(_, _, parent)| parent.parent() == camera);
        if rig.is_some_and(|(_, r, _)| r.id == id && r.body == body && r.def == *def) {
            continue;
        }
        if let Some((old, ..)) = rig {
            commands.entity(old).despawn();
        }
        let root = commands
            .spawn((
                Name::new(format!("view model {id}")),
                ViewModelRig { id: id.to_owned(), def: def.clone(), body },
                Animated::new(def.animset.clone()),
                ShowsBody(body),
                Transform { translation: def.offset, scale: Vec3::splat(def.scale), ..default() },
                Visibility::default(),
                RenderLayers::layer(VIEW_MODEL_LAYER),
                ChildOf(camera),
            ))
            .id();
        let mut model = commands.spawn((GltfModel::load(&asset_server, def.model.clone()), Transform::default(), RenderLayers::layer(VIEW_MODEL_LAYER), ChildOf(root)));
        if let Some(look) = &def.look {
            model.insert(Look::new(look.clone()));
        }
    }
}

/// `PostUpdate`, before visibility is worked out: every mesh under a rig is on the view model layer, casts
/// no shadow and is drawn whatever its bounds say (a skinned mesh's can be stale); the ones that appear
/// later (the model finishes loading, the bridge mounts a prop) too.
fn dress_view_model(
    mut commands: Commands,
    rigs: Query<Entity, With<ViewModelRig>>,
    children: Query<&Children>,
    meshes: Query<(), (With<Mesh3d>, Without<ViewModelMesh>)>,
) {
    for rig in &rigs {
        let mut stack = vec![rig];
        while let Some(entity) = stack.pop() {
            if meshes.contains(entity) {
                commands.entity(entity).insert((RenderLayers::layer(VIEW_MODEL_LAYER), NotShadowCaster, NoFrustumCulling, ViewModelMesh));
            }
            if let Ok(kids) = children.get(entity) {
                stack.extend(kids.iter());
            }
        }
    }
}
