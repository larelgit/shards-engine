//! The collider overlay: every collider and sensor volume of the physics world drawn as a
//! wireframe, for seeing what the controller and the queries see. Toggle it with the
//! `physics_overlay` tool; it draws in the window only.

use ai_core::dev::toggle_line;
use ai_core::highlight::AiGizmos;
use ai_core::prelude::*;
use ai_core::tools::{register_tool, ToolKind};
use ai_core::DevCommand;
use bevy::gizmos::config::GizmoConfigStore;
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::physics::{ColliderInfo, LayerMask, PhysicsWorld, Shape, VolumeKind};

/// Whether colliders are drawn.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct PhysicsOverlay {
    pub enabled: bool,
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<PhysicsOverlay>();
    register_tool(app.world_mut(), "physics_overlay", Scope::Dev, ToolKind::Query, overlay_tool);
    app.add_dev_entry(
        "world",
        "physics_overlay",
        "Colliders",
        DevCommand::Live(|w, _| Ok(toggle_line("physics_overlay", w.resource::<PhysicsOverlay>().enabled))),
    );
    // Headless apps have no gizmo plugin: the tool still answers, nothing is drawn.
    if app.is_plugin_added::<bevy::gizmos::GizmoPlugin>() {
        app.add_systems(PostUpdate, draw_colliders.run_if(resource_exists::<GizmoConfigStore>));
    }
}

/// Draw every collider and sensor volume in the game window: world geometry grey, character
/// bodies green, hit and hurt boxes red, water blue, updrafts cyan, triggers yellow, ladders orange.
#[derive(Deserialize, JsonSchema)]
pub struct OverlayArgs {
    /// Default true.
    pub on: Option<bool>,
}

fn overlay_tool(In(call): In<Call<OverlayArgs>>, mut overlay: ResMut<PhysicsOverlay>) -> ToolResult {
    overlay.enabled = call.args.on.unwrap_or(true);
    Ok(format!("physics overlay {}", if overlay.enabled { "on" } else { "off" }))
}

fn color_of(c: &ColliderInfo) -> Color {
    match &c.volume {
        Some(VolumeKind::Water) => Color::srgb(0.2, 0.5, 1.0),
        Some(VolumeKind::Updraft { .. }) => Color::srgb(0.2, 0.9, 0.9),
        Some(VolumeKind::Trigger { .. }) => Color::srgb(1.0, 0.9, 0.2),
        Some(VolumeKind::Ladder { .. }) => Color::srgb(1.0, 0.55, 0.15),
        None if c.layers.memberships.intersects(LayerMask::HITBOX | LayerMask::HURTBOX) => Color::srgb(1.0, 0.3, 0.25),
        None if c.layers.memberships.intersects(LayerMask::CHARACTER) => Color::srgb(0.3, 1.0, 0.4),
        None => Color::srgb(0.6, 0.65, 0.75),
    }
}

fn draw_colliders(mut gizmos: Gizmos<AiGizmos>, overlay: Res<PhysicsOverlay>, physics: Res<PhysicsWorld>) {
    if !overlay.enabled {
        return;
    }
    for c in physics.colliders() {
        let color = color_of(c);
        let at = Isometry3d::new(c.position, c.rotation);
        match c.shape {
            Shape::Cuboid { half } => {
                gizmos.cube(Transform { translation: c.position, rotation: c.rotation, scale: half * 2.0 }, color);
            }
            Shape::Sphere { radius } => {
                gizmos.sphere(at, radius, color);
            }
            Shape::Capsule { half_height, radius } => {
                gizmos.primitive_3d(&Capsule3d::new(radius, half_height * 2.0), at, color);
            }
            Shape::Cylinder { half_height, radius } => {
                gizmos.primitive_3d(&Cylinder::new(radius, half_height * 2.0), at, color);
            }
            Shape::Cone { half_height, radius } => {
                gizmos.primitive_3d(&Cone::new(radius, half_height * 2.0), at, color);
            }
        }
    }
}
