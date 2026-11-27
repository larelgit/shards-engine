//! # shards_light — lights from content (F13)
//!
//! A light is content: a `light` definition (`*.light.ron`, [`LightDef`]) says its kind (point or spot), colour or
//! temperature, power in lumens, range, whether it casts shadows and how it flickers, and an entity with a
//! [`ContentLight`] shines by it from where it stands. A level node places one with `light=<id>` (an
//! empty, or a Blender lamp whose own light it replaces, see `shards_level`), a prefab with
//! `"ContentLight": (id: "sodium")`, code with `commands.spawn((ContentLight::new("sodium"), Transform::from_xyz(..)))`.
//!
//! - **Pictures only.** Nothing in the simulation reads a light, so a replay, a snapshot and a co-op session never
//!   see one: with the `render` feature each content light gets a Bevy `PointLight` or `SpotLight` on a child
//!   entity without an `AiId`, driven every frame; headless the module keeps only its state (what the `lights`
//!   query shows). A game's own Bevy lights (a sun, a glTF lamp without `light=`) are the game's and untouched.
//! - **Flicker** ([`Flicker`]) is a pure function of the simulation's clock (`Time<Fixed>`: a pause stops it and
//!   `step` moves it, the same headless and drawn) and of a seed that is the light's own (its content id, node name and place
//!   when it first appeared, kept while it is placed: a light that moves keeps its wander), never `SimRng`: a pattern of letters `a` to `z` as brightness steps (the classic light styles), or smooth
//!   noise of a depth.
//! - **Shadow budget.** `graphics.shadow_lights` (default 4, none under `graphics.shadows = "off"`): of the lights
//!   whose content says `shadows: true`, the nearest that many to the viewer (the active camera, else the local player)
//!   cast shadows, ties by a stable key ([`budget::casters`]). `graphics.shadow_views` above 0 counts the budget in
//!   shadow views instead (a point light 6, a spot 1), nearest first, and replaces `shadow_lights`.
//! - **For the AI:** the `lights` query (counts by kind, who casts a shadow now, who flickers and how bright, the
//!   picture's grading and its blend) and `light` in `ai/status`.
//!
//! The colour grade of the picture and the volumes that blend it live in `shards_motion`'s camera
//! (`GradingBlend`).

pub mod budget;
pub mod def;
pub mod flicker;
#[cfg(feature = "render")]
mod render;
mod tools;

#[cfg(feature = "render")]
pub use render::LightPicture;

use ai_core::{AiAppExt, AiCamera};
use std::collections::BTreeMap;

use bevy::prelude::*;
use serde_json::{json, Value};
use shards_content::{ContentAppExt, ContentDb};
use shards_core::{EngineAppExt, LocalPlayer};
use shards_motion::camera::{picture_clock, FirstPersonCamera, ThirdPersonCamera};
use shards_settings::{AccessibilitySettings, FlashLevel, GraphicsSettings};

pub use budget::ShadowBudget;
pub use def::{kelvin_srgb, Flicker, LightDef, LightKind, MAX_RANGE};
pub use tools::LightsArgs;

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many lights cast shadows when the game has no `graphics` settings.
pub const DEFAULT_SHADOW_LIGHTS: usize = 4;

/// The entity shines by the `light` definition `id` from where it stands (its `Transform`; a spot points along its -Z).
/// A level node's `light=<id>` puts one. Not simulation state: nothing reads it in a tick.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
#[cfg_attr(feature = "render", require(Transform, Visibility))]
#[cfg_attr(not(feature = "render"), require(Transform))]
pub struct ContentLight {
    /// The `light` definition.
    pub id: String,
}

impl ContentLight {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// The noise seed each content light took when it first appeared (its definition id, node name and place then), kept for
/// as long as the entity is placed and its definition id stays: a light that moves (a lamp a body carries, a mover's
/// child) keeps wandering the same way, at the rate its definition and the player's `accessibility.flashes` allow.
/// A resource, not a `Local`: the `lights` tool runs its own cached `update_state`, and both must agree.
#[derive(Resource, Default, Debug)]
pub struct LightSeeds(BTreeMap<Entity, (String, u64)>);

impl LightSeeds {
    /// The seed of `entity`, made from `id` and `key` the first time it is asked for (or after its id changed).
    fn seed(&mut self, entity: Entity, id: &str, key: &str) -> u64 {
        if let Some((have, seed)) = self.0.get(&entity)
            && have == id
        {
            return *seed;
        }
        let seed = flicker::seed_of(id, key);
        self.0.insert(entity, (id.to_owned(), seed));
        seed
    }

    /// How many lights have a seed.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forgets the entities that are no longer placed.
    fn forget_unplaced(&mut self, placed: &Placed) {
        if self.0.keys().any(|e| !placed.contains(*e)) {
            self.0.retain(|e, _| placed.contains(*e));
        }
    }
}

/// What the module knows of one content light now.
#[derive(Clone, Debug, PartialEq)]
pub struct LightInfo {
    pub entity: Entity,
    /// The `light` definition's id.
    pub id: String,
    /// The entity's name (a level node's), if it has one.
    pub name: String,
    /// A key for ties and for the noise seed it took when it first appeared (see [`LightSeeds`]): the id, the name and the place now.
    pub key: String,
    /// Where it stands (world).
    pub position: Vec3,
    /// `None`: the definition does not exist (a content error), and nothing is drawn.
    pub def: Option<LightDef>,
    /// The flicker as drawn: the definition's, as far as the player's `accessibility.flashes` lets it through.
    pub flicker: Flicker,
    /// The brightness factor of the flicker now (1 steady, 0 dark).
    pub level: f32,
    /// The pattern step the clock is at, for a pattern.
    pub step: Option<usize>,
    /// Its power now (lm): the definition's times `level`.
    pub lumens: f32,
    /// Its colour (sRGB).
    pub srgb: [f32; 3],
    /// Whether it wants a shadow, and whether the budget gives it one.
    pub wants_shadow: bool,
    pub casts: bool,
    /// How far it is from the viewer (m); `None` with no viewer.
    pub distance: Option<f32>,
}

impl LightInfo {
    /// The shadow views its shadow takes: 6 for a point light (a cube), 1 for a spot, 0 with no definition.
    pub fn shadow_views(&self) -> usize {
        match self.def.as_ref().map(|d| d.kind) {
            Some(LightKind::Point) => budget::POINT_VIEWS,
            Some(LightKind::Spot { .. }) => budget::SPOT_VIEWS,
            None => 0,
        }
    }
}

/// Where the picture's lights stand now: refreshed every frame, and by the `lights` query. The picture's, never
/// the simulation's (no `AiId`, not in a snapshot or a replay).
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct LightState {
    /// Every content light, in key order.
    pub lights: Vec<LightInfo>,
    /// The simulation's clock this was made at (s).
    pub clock: f64,
    /// The viewer the shadow budget measures from, and what it is (`camera`, `player`).
    pub viewer: Option<(Vec3, &'static str)>,
    /// How many lights, or shadow views, may cast a shadow.
    pub budget: ShadowBudget,
    /// The shadow views the lights that cast one take now (a point light 6, a spot 1), whatever the budget counts.
    pub views_used: usize,
    /// The player's `accessibility.flashes`: what flickers is slower and shallower (`Reduced`) or steady (`Off`).
    pub flashes: FlashLevel,
}

impl Default for LightState {
    fn default() -> Self {
        Self {
            lights: Vec::new(),
            clock: 0.0,
            viewer: None,
            budget: ShadowBudget::Lights(DEFAULT_SHADOW_LIGHTS),
            views_used: 0,
            flashes: FlashLevel::default(),
        }
    }
}

impl LightState {
    /// The lights that cast a shadow now.
    pub fn casting(&self) -> impl Iterator<Item = &LightInfo> {
        self.lights.iter().filter(|l| l.casts)
    }

    /// The lights that flicker.
    pub fn flickering(&self) -> impl Iterator<Item = &LightInfo> {
        self.lights.iter().filter(|l| l.def.as_ref().is_some_and(|d| d.flicker != Flicker::None))
    }
}

/// The light module: the `light` kind, the lights' state, the shadow budget, the tool and (with `render`) the
/// pictures. Needs the engine core, content, settings and motion first; `shards::EnginePlugins` adds it after them.
pub struct LightPlugin;

impl Plugin for LightPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<ai_core::AiCorePlugin>(),
            "LightPlugin needs AiCorePlugin added first (shards::EnginePlugins adds both)"
        );
        app.register_type::<ContentLight>()
            // A picture's component, on an entity that may have an `AiId` (a prefab with an `AiKind`): never part of a replay's hash.
            .replay_ignore::<ContentLight>()
            .add_content::<LightDef>()
            .init_resource::<LightState>()
            .init_resource::<LightSeeds>()
            .add_engine_module("light", VERSION)
            .add_ai_status("light", status)
            .add_dev_entry("world", "lights", "Lights: the content lights, their flicker, shadows and the picture's grade", ai_core::DevCommand::Run("lights".into()))
            .add_systems(Update, update_state);
        tools::register(app.world_mut());
        #[cfg(feature = "render")]
        render::plugin(app);
    }
}

/// A light's stable key: its definition, its node name and the place it stands.
fn key_of(id: &str, name: &str, at: Vec3) -> String {
    format!("{id}|{name}|{:.3},{:.3},{:.3}", at.x, at.y, at.z)
}

/// What the viewer is: the active camera (the AI's, else a third-person or first-person camera), else the local player's eye.
type Cameras<'w, 's> = Query<'w, 's, &'static GlobalTransform, With<AiCamera>>;
type OrbitCameras<'w, 's> = Query<'w, 's, &'static GlobalTransform, Or<(With<ThirdPersonCamera>, With<FirstPersonCamera>)>>;
type Players<'w, 's> = Query<'w, 's, &'static Transform, With<LocalPlayer>>;
type Placed<'w, 's> =
    Query<'w, 's, (Entity, &'static ContentLight, &'static Transform, &'static GlobalTransform, Option<&'static Name>, Has<ChildOf>)>;

/// `Update`: every content light's flicker, shadow and place for this frame. It runs in a headless game too: the
/// state is what the `lights` query says.
#[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
fn update_state(
    fixed: Res<Time<Fixed>>,
    db: Option<Res<ContentDb>>,
    settings: Option<Res<GraphicsSettings>>,
    accessibility: Option<Res<AccessibilitySettings>>,
    placed: Placed,
    cameras: Cameras,
    orbits: OrbitCameras,
    players: Players,
    mut state: ResMut<LightState>,
    mut seeds: ResMut<LightSeeds>,
) {
    let clock = picture_clock(&fixed);
    let budget = settings.as_ref().map_or(ShadowBudget::Lights(DEFAULT_SHADOW_LIGHTS), |s| s.shadow_budget());
    let flashes = accessibility.as_ref().map_or(FlashLevel::Full, |a| a.flash_level());
    let viewer = cameras
        .iter()
        .next()
        .or_else(|| orbits.iter().next())
        .map(|g| (g.translation(), "camera"))
        .or_else(|| players.iter().next().map(|t| (t.translation + Vec3::Y * 1.6, "player")));
    // A game with no lights keeps its state as it is (the viewer and the budget, which the query shows, aside).
    if placed.is_empty() && state.lights.is_empty() {
        if state.viewer != viewer || state.budget != budget || state.flashes != flashes {
            state.viewer = viewer;
            state.budget = budget;
            state.flashes = flashes;
        }
        return;
    }

    let mut lights: Vec<LightInfo> = placed
        .iter()
        .map(|(entity, light, local, global, name, child)| {
            let position = if child { global.translation() } else { local.translation };
            let name = name.map(|n| n.as_str().to_owned()).unwrap_or_default();
            let key = key_of(&light.id, &name, position);
            let def = db.as_ref().and_then(|db| db.peek::<LightDef>(&light.id)).cloned();
            let (shown, level, step, lumens, srgb, wants_shadow) = match &def {
                Some(def) => {
                    let shown = flicker::shaped(&def.flicker, flashes);
                    let level = flicker::level_for(&def.flicker, clock, seeds.seed(entity, &light.id, &key), flashes);
                    (shown.clone(), level, flicker::step_of(&shown, clock), def.intensity * level, def.srgb(), def.shadows)
                }
                None => (Flicker::None, 0.0, None, 0.0, [1.0; 3], false),
            };
            let distance = viewer.map(|(at, _)| at.distance(position));
            LightInfo { entity, id: light.id.clone(), name, key, position, def, flicker: shown, level, step, lumens, srgb, wants_shadow, casts: false, distance }
        })
        .collect();
    seeds.forget_unplaced(&placed);
    // Key order, then place: never the order of a query.
    lights.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.position.to_array().map(f32::to_bits).cmp(&b.position.to_array().map(f32::to_bits))));
    let candidates: Vec<budget::Candidate> = lights
        .iter()
        .map(|l| budget::Candidate { distance: l.distance.unwrap_or(0.0), key: &l.key, wants: l.wants_shadow, views: l.shadow_views() })
        .collect();
    let casts = budget::casters(&candidates, budget);
    let views_used = budget::views_used(&candidates, &casts);
    for (light, casts) in lights.iter_mut().zip(casts) {
        light.casts = casts;
    }
    let next = LightState { lights, clock, viewer, budget, views_used, flashes };
    if *state != next {
        *state = next;
    }
}

/// `ai/status` → `light: {lights, casting, flickering, budget}`, and `budget_views` (the budget counts views, a point
/// light 6, a spot 1) with `views_used` while `graphics.shadow_views` is set.
fn status(world: &World) -> Value {
    let Some(state) = world.get_resource::<LightState>() else { return json!({}) };
    let mut out = json!({
        "lights": state.lights.len(),
        "casting": state.casting().count(),
        "flickering": state.flickering().count(),
        "budget": state.budget.amount(),
    });
    if let ShadowBudget::Views(_) = state.budget {
        out["budget_views"] = json!(true);
        out["views_used"] = json!(state.views_used);
    }
    out
}
