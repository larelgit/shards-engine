//! Baked light for a level's picture: lightmaps on its meshes, and irradiance volumes and reflection probes in
//! its rooms, from images a bake (Blender's, by the game's own dressing script) wrote. Picture only: nothing here
//! is simulation state, a snapshot or a replay.
//!
//! **Where it is said.** The level's nodes say *which* image each mesh or probe takes, by a file name, as
//! custom properties a bake script writes onto the objects (`lightmap`, `lightmap_rect`, `irradiance_volume`,
//! `reflection_probe`, `probe_falloff`, see [`BakedNode`]); the content kind `baked_light` (one definition per
//! level, under the level's id, [`BakedLightDef`]) says *where* those files are, as named **sets** (a folder
//! each: the lamps on, the lamps off after a breach), which set a level starts with and how bright the images
//! are. The glTF file is the same for every set, so a set is switched without loading the level again.
//!
//! **Switching.** [`BakedLight`] holds the set each level draws when it is not the definition's `set`: a game
//! switches with [`select_baked_set`] (or `ResMut<BakedLight>`) from its own state, which it keeps and saves; the
//! resource is the picture's and not in a snapshot, so a game sets it again from its state after a load. Agents
//! use `baked_light` (what is baked and drawn) and `baked_light_set`.
//!
//! **Drawing** (the `render` feature): a mesh primitive under a `lightmap` node gets Bevy's `Lightmap` (the image,
//! the node's `lightmap_rect`, the mesh's second UV map `TEXCOORD_1`), and its `StandardMaterial` the set's
//! `lightmap_exposure`; a probe node gets a `LightProbe` with an `IrradianceVolume` and/or an
//! `EnvironmentMapLight`. A lightmapped mesh still takes every dynamic light (a flashlight, a muzzle flash, a
//! flicker); a content light whose `baked: true` says its light is in the lightmaps lights no lightmapped mesh's
//! diffuse (`shards_light`), so it is not counted twice, and still lights moving bodies.

use std::collections::BTreeMap;

use ai_core::prelude::*;
use ai_core::tools::{register_tool, ToolKind};
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shards_content::db::closest;
use shards_content::{ContentDb, ContentDef, Refs};

use crate::asset::{check_relative, LevelAsset};
use crate::def::LevelDef;
use crate::library::level_asset;
use crate::props::baked_of;
use crate::spawn::LevelInstances;

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// The baked light of a level (`content/**/*.baked_light.ron`, kind `baked_light`), under the level's id.
///
/// ```ron
/// {
///     "tunnels1": (
///         sets: { "lamps_on": "baked/tunnels1/on", "breach": "baked/tunnels1/breach" },
///         set: "lamps_on",
///         lightmap_exposure: 2000.0,
///         irradiance_intensity: 2000.0,
///     ),
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BakedLightDef {
    /// The sets of baked images, by name: a folder each (relative to the asset folder) that holds every file the
    /// level's nodes name (`lightmap`, `irradiance_volume`, and `<reflection_probe>_diffuse.ktx2` with
    /// `<reflection_probe>_specular.ktx2`).
    pub sets: BTreeMap<String, String>,
    /// The set drawn while the game has chosen none ([`BakedLight`]).
    pub set: String,
    /// What a lightmap texel of 1 is worth (Bevy's `StandardMaterial::lightmap_exposure`, in cd/m²), on every
    /// material of the level's lightmapped meshes. A bake writes radiance in its own units; this brings them to the
    /// units of the game's lights and camera exposure.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub lightmap_exposure: f32,
    /// The same for the irradiance volumes (Bevy's `IrradianceVolume::intensity`).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub irradiance_intensity: f32,
    /// The same for the reflection probes (Bevy's `EnvironmentMapLight::intensity`).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub reflection_intensity: f32,
    /// Bicubic sampling of the lightmaps: smoother, dearer, and it may leak light at the edges of a chart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bicubic: bool,
}

impl BakedLightDef {
    /// The folder of the set `name`.
    pub fn folder(&self, name: &str) -> Option<&str> {
        self.sets.get(name).map(String::as_str)
    }

    /// What is wrong with it without the level, one line each.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.sets.is_empty() {
            out.push("`sets` is empty: name at least one folder of baked images, like { \"lamps_on\": \"baked/level/on\" }".to_owned());
        } else if !self.sets.contains_key(&self.set) {
            let hint = closest(&self.set, self.sets.keys().map(String::as_str)).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
            out.push(format!("`set`: no set `{}` in `sets`{hint}; the sets: {}", self.set, self.set_names()));
        }
        for (name, folder) in &self.sets {
            if let Err(why) = check_relative(folder) {
                out.push(format!("set `{name}`: {why}"));
            }
        }
        for (name, v) in [("lightmap_exposure", self.lightmap_exposure), ("irradiance_intensity", self.irradiance_intensity), ("reflection_intensity", self.reflection_intensity)] {
            if !(v >= 0.0 && v.is_finite()) {
                out.push(format!("`{name}`: 0 or more, not {v}"));
            }
        }
        out
    }

    fn set_names(&self) -> String {
        self.sets.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
    }
}

impl ContentDef for BakedLightDef {
    const KIND: &'static str = "baked_light";
    // Only the picture reads it: editing it never makes a replay diverge or a co-op handshake fail.
    const SIMULATION: bool = false;

    fn validate(&self, id: &str, refs: &mut Refs) {
        for problem in self.problems() {
            refs.error(problem);
        }
        if !refs.require::<LevelDef>(id, "the definition's id (a `baked_light` is named after its level)") {
            return;
        }
        // The level's file, its baked nodes and every set's files: only with an asset folder (a bare database checks
        // the shape alone).
        let Some(root) = refs.asset_root() else { return };
        let Some(level) = refs.db().peek::<LevelDef>(id) else { return };
        let Ok(asset) = LevelAsset::read(root, &level.file) else { return }; // the level's own check names it
        let nodes = baked_nodes(&asset);
        if nodes.is_empty() {
            refs.error(format!(
                "`{}` has no node with `lightmap`, `irradiance_volume` or `reflection_probe`: the bake script writes those custom \
                 properties on the objects it baked",
                level.file
            ));
        }
        for (path, node) in &nodes {
            if let Some(file) = &node.lightmap {
                let mesh = asset.nodes.iter().find(|n| &n.path == path);
                match mesh {
                    Some(n) if n.mesh.is_none() => refs.error(format!("node `{path}`: `lightmap` `{file}` on a node without a mesh")),
                    Some(n) if !n.uv1 => refs.error(format!(
                        "node `{path}`: mesh `{}` has no second UV map (glTF TEXCOORD_1), which a lightmap is drawn by: bake into a \
                         second UV map in Blender and export it (Bevy draws a lightmap without it as nothing)",
                        n.mesh.as_deref().unwrap_or_default()
                    )),
                    _ => {}
                }
            }
            for (set, folder) in &self.sets {
                for file in node.files() {
                    let at = format!("{folder}/{file}");
                    if !root.join(&at).is_file() {
                        refs.error(format!("node `{path}`: set `{set}` has no `{at}`"));
                    }
                }
            }
        }
    }
}

/// What one level node says about baked light: its custom properties, which a bake script writes.
///
/// | property | value | what it does |
/// |---|---|---|
/// | `lightmap` | a file name (`walls_0.ktx2`) | the node's mesh draws this lightmap of the set by its second UV map: a `.ktx2` of linear half floats (`Rgba16Float`) or `Rgb9e5` is the format for a bake; a `.png` is read as sRGB (8 bits, low range); a `.hdr` is 32-bit float, which only a GPU that filters such textures draws |
/// | `lightmap_rect` | `[x0, y0, x1, y1]` | the part of the image its UVs cover (0 to 1, the top left first; the whole image when left out): many meshes share one atlas |
/// | `irradiance_volume` | a file name (`hall.ktx2`) | the node is a light probe box with this irradiance volume (a 3D KTX2 texture in Bevy's layout) of the set: moving bodies inside take its light |
/// | `reflection_probe` | a name (`hall`) | the node is a light probe box with the reflection cube maps `hall_diffuse.ktx2` and `hall_specular.ktx2` of the set |
/// | `probe_falloff` | `[x, y, z]` or one number, 0 to 1 | how far in from the box's sides the probe fades (Bevy's `LightProbe::falloff`; 0, a hard edge, when left out) |
///
/// A probe's box is the unit cube (-0.5 to 0.5) scaled, turned and placed by the node's transform (Bevy's light
/// probe): in Blender an empty of display type *Cube* and size 0.5, scaled to the room.
#[derive(Clone, Debug, PartialEq)]
pub struct BakedNode {
    pub lightmap: Option<String>,
    pub lightmap_rect: Rect,
    pub irradiance_volume: Option<String>,
    pub reflection_probe: Option<String>,
    pub probe_falloff: Vec3,
}

impl Default for BakedNode {
    fn default() -> Self {
        Self { lightmap: None, lightmap_rect: Rect::new(0.0, 0.0, 1.0, 1.0), irradiance_volume: None, reflection_probe: None, probe_falloff: Vec3::ZERO }
    }
}

impl BakedNode {
    /// Whether the node is a light probe box.
    pub fn is_probe(&self) -> bool {
        self.irradiance_volume.is_some() || self.reflection_probe.is_some()
    }

    /// The files it names, relative to a set's folder.
    pub fn files(&self) -> Vec<String> {
        let mut out: Vec<String> = self.lightmap.iter().chain(&self.irradiance_volume).cloned().collect();
        if let Some(name) = &self.reflection_probe {
            out.extend(reflection_files(name));
        }
        out
    }
}

/// The two cube maps of the reflection probe `name`: diffuse, then specular.
pub fn reflection_files(name: &str) -> [String; 2] {
    [format!("{name}_diffuse.ktx2"), format!("{name}_specular.ktx2")]
}

/// The level's nodes that carry baked light, by path, in file order (those whose properties have a problem are
/// left out: the level's own check names it).
pub fn baked_nodes(level: &LevelAsset) -> Vec<(String, BakedNode)> {
    level.nodes.iter().filter_map(|n| baked_of(&n.extras).ok().flatten().map(|b| (n.path.clone(), b))).collect()
}

/// The baked set each level draws, when the game chose one: picture only, in no snapshot or replay. A game derives it
/// from its own state (and sets it again after a load); a level the map does not name draws its definition's `set`.
#[derive(Resource, Default, Clone, Debug, PartialEq)]
pub struct BakedLight {
    chosen: BTreeMap<String, String>,
}

impl BakedLight {
    /// Level `level` (its content id) draws the set `set` from now on. Writes only when it changes.
    pub fn select(&mut self, level: &str, set: &str) {
        if self.chosen(level) != Some(set) {
            self.chosen.insert(level.to_owned(), set.to_owned());
        }
    }

    /// Level `level` draws its definition's `set` again.
    pub fn reset(&mut self, level: &str) {
        self.chosen.remove(level);
    }

    /// The set the game chose for `level`, if it chose one.
    pub fn chosen(&self, level: &str) -> Option<&str> {
        self.chosen.get(level).map(String::as_str)
    }

    /// The set `level` draws under `def`: the chosen one when the definition has it, else the definition's.
    pub fn set_of<'a>(&'a self, level: &str, def: &'a BakedLightDef) -> &'a str {
        self.chosen(level).filter(|s| def.sets.contains_key(*s)).unwrap_or(&def.set)
    }
}

/// Switches level `level` to its baked set `set` (`None`: back to its definition's), checking both against the
/// content. Idempotent: calling it every frame with the same set changes nothing.
pub fn select_baked_set(world: &mut World, level: &str, set: Option<&str>) -> Result<(), String> {
    let db = world.get_resource::<ContentDb>().ok_or("no content database")?;
    let Some(def) = db.peek::<BakedLightDef>(level) else {
        let ids: Vec<&str> = db.peek_ids::<BakedLightDef>().collect();
        let hint = closest(level, ids.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
        let known = if ids.is_empty() { "no level has one".to_owned() } else { format!("the levels with one: {}", ids.join(", ")) };
        return Err(format!("no `baked_light` definition for the level `{level}`{hint}; {known}"));
    };
    if let Some(set) = set
        && !def.sets.contains_key(set)
    {
        let hint = closest(set, def.sets.keys().map(String::as_str)).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
        return Err(format!("the level `{level}` has no baked set `{set}`{hint}; its sets: {}", def.set_names()));
    }
    let mut light = world.get_resource_or_insert_with(BakedLight::default);
    match set {
        Some(set) => light.select(level, set),
        None if light.chosen(level).is_some() => light.reset(level),
        None => {}
    }
    Ok(())
}

pub(crate) fn register(world: &mut World) {
    register_tool(world, "baked_light", Scope::Player, ToolKind::Query, baked_light);
    // Picture only, like `look_outlines`: not journaled or recorded (the game's own state chooses the set).
    register_tool(world, "baked_light_set", Scope::Dev, ToolKind::Query, baked_light_set);
}

/// The levels' baked light: for each level with a `baked_light` definition, its sets and the one drawn now (and
/// who chose it), how many of the level's nodes carry a lightmap or a probe, and, in a drawn game, how many meshes
/// draw a lightmap now and which images are still loading or failed.
#[derive(Deserialize, JsonSchema, Default)]
pub struct BakedLightArgs {
    /// Only this level (its content id).
    pub level: Option<String>,
}

/// Switches a level's baked light to another set of its `baked_light` definition (the lamps off after a breach);
/// without `set`, back to the definition's own. Picture only: a game switches from its own state.
#[derive(Deserialize, JsonSchema, Default)]
pub struct BakedLightSetArgs {
    /// The level (its content id).
    pub level: String,
    /// The set (a key of the definition's `sets`); left out, the definition's `set`.
    pub set: Option<String>,
}

fn baked_light(In(call): In<Call<BakedLightArgs>>, world: &World) -> ToolResult {
    let db = world.get_resource::<ContentDb>().ok_or("no content database")?;
    let light = world.get_resource::<BakedLight>().cloned().unwrap_or_default();
    let ids: Vec<&str> = db.peek_ids::<BakedLightDef>().filter(|id| call.args.level.as_deref().is_none_or(|l| l == *id)).collect();
    if let Some(level) = &call.args.level
        && ids.is_empty()
    {
        let all: Vec<&str> = db.peek_ids::<BakedLightDef>().collect();
        let hint = closest(level, all.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
        return Err(format!("no `baked_light` definition for the level `{level}`{hint}"));
    }
    let mut out = vec![format!("baked light: {} levels", ids.len())];
    let drawn = drawn_counts(world);
    for id in ids {
        let Some(def) = db.peek::<BakedLightDef>(id) else { continue };
        let set = light.set_of(id, def);
        let by = if light.chosen(id) == Some(set) { "chosen by the game" } else { "the definition's" };
        let placed = world.get_resource::<LevelInstances>().map_or(0, |i| i.iter().filter(|i| i.label == id).count());
        out.push(format!("  {id}: set `{set}` ({by}) of {} · placed {placed} times", def.set_names()));
        let file = db.peek::<LevelDef>(id).map(|l| l.file.clone());
        match file.as_deref().map(|f| level_asset(world, f)) {
            Some(Ok(level)) => {
                let nodes = baked_nodes(level);
                let lightmaps = nodes.iter().filter(|(_, n)| n.lightmap.is_some()).count();
                let volumes = nodes.iter().filter(|(_, n)| n.irradiance_volume.is_some()).count();
                let reflections = nodes.iter().filter(|(_, n)| n.reflection_probe.is_some()).count();
                out.push(format!("    nodes: {lightmaps} with a lightmap, {volumes} irradiance volumes, {reflections} reflection probes"));
            }
            Some(Err(why)) => out.push(format!("    the level's file: {why}")),
            None => out.push(format!("    no `level` definition `{id}`")),
        }
        match drawn.as_ref().and_then(|d| d.get(id)) {
            Some(d) => out.push(format!(
                "    drawn: {} meshes lightmapped, {} probes · images: {} loading, {} failed{}",
                d.meshes,
                d.probes,
                d.loading,
                d.failed.len(),
                if d.failed.is_empty() { String::new() } else { format!(" ({})", d.failed.join(", ")) }
            )),
            None if drawn.is_some() => out.push("    drawn: nothing yet (the level is not placed, or its picture is loading)".to_owned()),
            None => out.push("    drawn: nothing (no picture in this game)".to_owned()),
        }
    }
    Ok(out.join("\n"))
}

fn baked_light_set(In(call): In<Call<BakedLightSetArgs>>, world: &mut World) -> ToolResult {
    let BakedLightSetArgs { level, set } = call.args;
    select_baked_set(world, &level, set.as_deref())?;
    let db = world.resource::<ContentDb>();
    let def = db.peek::<BakedLightDef>(&level).ok_or("the definition is gone")?;
    let now = world.get_resource::<BakedLight>().map_or(def.set.as_str(), |l| l.set_of(&level, def)).to_owned();
    Ok(format!("level `{level}` draws its baked set `{now}`"))
}

/// What the picture draws now, per level id: `None` in a game without a picture.
#[derive(Default)]
#[cfg_attr(not(feature = "render"), allow(dead_code, reason = "only a picture fills it"))]
struct Drawn {
    meshes: usize,
    probes: usize,
    loading: usize,
    failed: Vec<String>,
}

#[cfg(feature = "render")]
fn drawn_counts(world: &World) -> Option<BTreeMap<String, Drawn>> {
    render::drawn_counts(world)
}

#[cfg(not(feature = "render"))]
fn drawn_counts(_: &World) -> Option<BTreeMap<String, Drawn>> {
    None
}

#[cfg(feature = "render")]
pub(crate) use render::plugin as render_plugin;

#[cfg(feature = "render")]
mod render {
    use std::collections::BTreeMap;

    use bevy::asset::LoadState;
    use bevy::gltf::GltfExtras;
    use bevy::image::{ImageLoaderSettings, ImageSampler};
    use bevy::light::{EnvironmentMapLight, IrradianceVolume, LightProbe};
    use bevy::pbr::Lightmap;
    use bevy::prelude::*;
    use shards_content::ContentDb;

    use super::{reflection_files, BakedLight, BakedLightDef, BakedNode, Drawn};
    use crate::props::baked_of;
    use crate::render::LevelVisual;
    use crate::spawn::LevelInstances;

    /// A node of a level's picture that carries baked light: what its properties say, and what is drawn on it now.
    #[derive(Component, Clone, Debug)]
    pub struct BakedPicture {
        /// The level instance.
        pub instance: u32,
        pub node: BakedNode,
        /// What was put on it: the folder and the numbers, so a switch or an edit puts it again.
        applied: Option<Applied>,
        /// The images it draws now, for the query.
        images: Vec<(String, Handle<Image>)>,
        /// The meshes it lightmapped.
        meshes: usize,
        /// A switch's new lightmap, loading while the old one is still drawn.
        staged: Option<(String, Handle<Image>)>,
        /// Whether the old lightmap was taken off for the switch (Bevy takes a new one only on a mesh that has none).
        lifted: bool,
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Applied {
        folder: String,
        lightmap_exposure: f32,
        irradiance_intensity: f32,
        reflection_intensity: f32,
        bicubic: bool,
    }

    pub(crate) fn plugin(app: &mut App) {
        // Known before any level is placed, so the query tells "nothing placed" from "no picture".
        app.world_mut().register_component::<BakedPicture>();
        // After the glTF scene is spawned (`replace_lamps` reads its extras in `PostUpdate` too), before extraction.
        app.add_systems(PostUpdate, (find_baked_nodes, draw_baked, keep_exposure).chain());
    }

    /// `PostUpdate`: a node under a level's picture whose extras carry baked light is marked with what they say.
    fn find_baked_nodes(
        mut commands: Commands,
        new: Query<(Entity, &GltfExtras), Added<GltfExtras>>,
        visuals: Query<&LevelVisual>,
        parents: Query<&ChildOf>,
    ) {
        for (entity, extras) in &new {
            let Ok(serde_json::Value::Object(props)) = serde_json::from_str::<serde_json::Value>(&extras.value) else { continue };
            let Ok(Some(node)) = baked_of(&props) else { continue };
            let Some(instance) = parents.iter_ancestors(entity).find_map(|a| visuals.get(a).ok()).map(|v| v.instance) else { continue };
            commands.entity(entity).insert(BakedPicture { instance, node, applied: None, images: Vec::new(), meshes: 0, staged: None, lifted: false });
        }
    }

    /// Baked light is sampled linearly between texels and clamped at the edges, whatever sampler the game made the
    /// default (a pixel-art game's nearest). A `.hdr` has a loader of its own, which takes the default sampler.
    fn load(server: &AssetServer, path: String) -> Handle<Image> {
        if path.ends_with(".hdr") {
            return server.load(path);
        }
        server.load_builder().with_settings(|s: &mut ImageLoaderSettings| s.sampler = ImageSampler::linear()).load(path)
    }

    /// `PostUpdate`: each baked node draws the set its level draws now (the game's choice, else the definition's),
    /// put again when the set or the definition's numbers change; a node whose level lost its definition loses it.
    #[allow(clippy::too_many_arguments, reason = "a Bevy system: one parameter per thing it reads")]
    fn draw_baked(
        mut commands: Commands,
        server: Option<Res<AssetServer>>,
        db: Option<Res<ContentDb>>,
        light: Option<Res<BakedLight>>,
        instances: Option<Res<LevelInstances>>,
        mut pictures: Query<(Entity, &mut BakedPicture, Option<&Children>)>,
        mut meshes: Query<&mut Mesh3d>,
    ) {
        let (Some(server), Some(db), Some(instances)) = (server, db, instances) else { return };
        let none = BakedLight::default();
        let light = light.as_deref().unwrap_or(&none);
        for (entity, mut picture, children) in &mut pictures {
            let Some(level) = instances.get(picture.instance).map(|i| i.label.clone()) else { continue };
            let def: Option<&BakedLightDef> = db.peek::<BakedLightDef>(&level);
            let want = def.and_then(|def| {
                let folder = def.folder(light.set_of(&level, def))?.to_owned();
                Some(Applied {
                    folder,
                    lightmap_exposure: def.lightmap_exposure,
                    irradiance_intensity: def.irradiance_intensity,
                    reflection_intensity: def.reflection_intensity,
                    bicubic: def.bicubic,
                })
            });
            // (A lightmap taken off for a switch goes back on even when the switch was undone meanwhile.)
            if picture.applied == want && !picture.lifted {
                continue;
            }
            // The meshes of the node: its own, and its primitives (Bevy's glTF loader puts each on a child).
            let targets: Vec<Entity> =
                std::iter::once(entity).chain(children.into_iter().flat_map(|c| c.iter())).filter(|e| meshes.contains(*e)).collect();
            // Bevy 0.20 picks a mesh's pipeline again only when its `Mesh3d` or material changes, not when a `Lightmap`
            // comes or goes, while its bind group follows the lightmap at once: a lightmap put on or taken off without a
            // touch of the mesh draws with a pipeline and a bind group that disagree (a wgpu validation error, and Bevy
            // stops drawing). So every change of a `Lightmap` marks the mesh changed in the same frame.
            let Some(want) = want else {
                // The definition is gone: the node draws no baked light.
                for target in &targets {
                    commands.entity(*target).remove::<(Lightmap, BakedExposure)>();
                }
                if picture.node.lightmap.is_some() {
                    touch(&mut meshes, &targets);
                }
                commands.entity(entity).remove::<(LightProbe, IrradianceVolume, EnvironmentMapLight)>();
                (picture.applied, picture.images, picture.meshes, picture.staged, picture.lifted) = (None, Vec::new(), 0, None, false);
                continue;
            };
            if picture.node.lightmap.is_some() && targets.is_empty() {
                // The scene's primitives are not in yet: next frame.
                continue;
            }
            let mut images = Vec::new();
            if let Some(file) = &picture.node.lightmap {
                let path = format!("{}/{file}", want.folder);
                let image = match &picture.staged {
                    Some((staged, image)) if *staged == path => image.clone(),
                    _ => load(&server, path.clone()),
                };
                // Bevy 0.20 takes no new `Lightmap` on a mesh that has one (`extract_lightmaps` skips a mesh it knows), so a
                // switch waits for the new image to load while the old one is drawn, takes the old one off for a frame,
                // then puts the new one on: one frame without a lightmap, and no frame of a blank image (the new one is on
                // the GPU by then).
                let switch = picture.applied.as_ref().is_some_and(|a| a.folder != want.folder || a.bicubic != want.bicubic);
                if switch && !picture.lifted {
                    let ready = matches!(server.load_state(image.id()), LoadState::Loaded | LoadState::Failed(_));
                    if ready {
                        for target in &targets {
                            commands.entity(*target).remove::<Lightmap>();
                        }
                        touch(&mut meshes, &targets);
                        picture.lifted = true;
                    }
                    picture.staged = Some((path, image));
                    continue;
                }
                let put = picture.applied.as_ref().is_none_or(|a| a.folder != want.folder || a.bicubic != want.bicubic) || picture.lifted;
                for target in &targets {
                    let lightmap = Lightmap { image: image.clone(), uv_rect: picture.node.lightmap_rect, bicubic_sampling: want.bicubic };
                    if put {
                        commands.entity(*target).insert((lightmap, BakedExposure(want.lightmap_exposure)));
                    } else {
                        // Only the exposure changed: the lightmap stays as it is.
                        commands.entity(*target).insert(BakedExposure(want.lightmap_exposure));
                    }
                }
                if put {
                    touch(&mut meshes, &targets);
                }
                images.push((path, image));
            }
            if picture.node.is_probe() {
                let mut probe = commands.entity(entity);
                probe.insert(LightProbe { falloff: picture.node.probe_falloff });
                match &picture.node.irradiance_volume {
                    Some(file) => {
                        let path = format!("{}/{file}", want.folder);
                        let voxels = load(&server, path.clone());
                        probe.insert(IrradianceVolume { voxels: voxels.clone(), intensity: want.irradiance_intensity, affects_lightmapped_meshes: false });
                        images.push((path, voxels));
                    }
                    None => {
                        probe.remove::<IrradianceVolume>();
                    }
                }
                match &picture.node.reflection_probe {
                    Some(name) => {
                        let [diffuse, specular] = reflection_files(name).map(|f| format!("{}/{f}", want.folder));
                        let (d, s) = (load(&server, diffuse.clone()), load(&server, specular.clone()));
                        probe.insert(EnvironmentMapLight {
                            diffuse_map: d.clone(),
                            specular_map: s.clone(),
                            intensity: want.reflection_intensity,
                            affects_lightmapped_mesh_diffuse: false,
                            ..default()
                        });
                        images.extend([(diffuse, d), (specular, s)]);
                    }
                    None => {
                        probe.remove::<EnvironmentMapLight>();
                    }
                }
            }
            picture.meshes = if picture.node.lightmap.is_some() { targets.len() } else { 0 };
            picture.images = images;
            picture.applied = Some(want);
            (picture.staged, picture.lifted) = (None, false);
        }
    }

    /// Marks the meshes changed, so Bevy picks their pipeline again (see `draw_baked`).
    fn touch(meshes: &mut Query<&mut Mesh3d>, targets: &[Entity]) {
        for target in targets {
            if let Ok(mut mesh) = meshes.get_mut(*target) {
                mesh.set_changed();
            }
        }
    }

    /// The lightmap exposure of a lightmapped mesh's level, kept on whatever `StandardMaterial` the mesh draws with (the
    /// glTF's, or one a `Lit` look made later).
    #[derive(Component, Clone, Copy, Debug, PartialEq)]
    pub struct BakedExposure(pub f32);

    /// The lightmapped meshes whose exposure or material changed.
    type ExposureChanged<'w, 's> = Query<
        'w,
        's,
        (&'static BakedExposure, &'static MeshMaterial3d<StandardMaterial>),
        Or<(Changed<BakedExposure>, Changed<MeshMaterial3d<StandardMaterial>>)>,
    >;

    /// `PostUpdate`, after the baked nodes are drawn: the material of a mesh whose exposure or material changed takes the
    /// exposure, written only where it differs (a write prepares the material again).
    fn keep_exposure(meshes: ExposureChanged, materials: Option<ResMut<Assets<StandardMaterial>>>) {
        // A game without the PBR renderer has no materials, and draws no lightmap.
        let Some(mut materials) = materials else { return };
        for (exposure, material) in &meshes {
            if materials.get(&material.0).is_some_and(|m| m.lightmap_exposure != exposure.0)
                && let Some(mut m) = materials.get_mut(&material.0)
            {
                m.lightmap_exposure = exposure.0;
            }
        }
    }

    /// What the baked pictures draw now, per level id.
    pub(super) fn drawn_counts(world: &World) -> Option<BTreeMap<String, Drawn>> {
        let server = world.get_resource::<AssetServer>()?;
        let instances = world.get_resource::<LevelInstances>()?;
        // A game without scenes draws nothing: no picture.
        world.get_resource::<Assets<StandardMaterial>>()?;
        let mut out: BTreeMap<String, Drawn> = BTreeMap::new();
        let mut query = world.try_query::<&BakedPicture>()?;
        for picture in query.iter(world) {
            let Some(level) = instances.get(picture.instance).map(|i| i.label.clone()) else { continue };
            let entry = out.entry(level).or_default();
            entry.meshes += picture.meshes;
            entry.probes += usize::from(picture.applied.is_some() && picture.node.is_probe());
            for (path, image) in &picture.images {
                match server.load_state(image.id()) {
                    LoadState::Loaded => {}
                    LoadState::Failed(_) => {
                        if !entry.failed.contains(path) {
                            entry.failed.push(path.clone());
                        }
                    }
                    _ => entry.loading += 1,
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::*;

    fn props(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn a_nodes_baked_properties_are_read_and_their_mistakes_named() {
        let mesh = baked_of(&props(json!({"lightmap": "walls_0.ktx2", "lightmap_rect": [0.0, 0.5, 0.5, 1.0]}))).unwrap().unwrap();
        assert_eq!((mesh.lightmap.as_deref(), mesh.lightmap_rect, mesh.is_probe()), (Some("walls_0.ktx2"), Rect::new(0.0, 0.5, 0.5, 1.0), false));
        let probe = baked_of(&props(json!({"irradiance_volume": "hall.ktx2", "reflection_probe": "hall", "probe_falloff": 0.2}))).unwrap().unwrap();
        assert_eq!((probe.probe_falloff, probe.is_probe()), (Vec3::splat(0.2), true));
        assert_eq!(probe.files(), ["hall.ktx2", "hall_diffuse.ktx2", "hall_specular.ktx2"]);
        assert_eq!(baked_of(&props(json!({"collider": "auto"}))), Ok(None), "a node without baked light");

        let wrong = baked_of(&props(json!({"lightmap": "../up.ktx2", "lightmap_rect": [0.5, 0.0, 0.2, 1.0], "probe_falloff": [0.1, 2.0, 0.1]})));
        let text = wrong.unwrap_err().join("\n");
        for want in [
            "`lightmap` `../up.ktx2` must be a file name inside a baked set's folder",
            "`lightmap_rect` must be [x0, y0, x1, y1] within 0 to 1 with x0 < x1",
            "`probe_falloff` must be [x, y, z] or one number, each 0 to 1",
            "`probe_falloff` does nothing without `irradiance_volume` or `reflection_probe`",
        ] {
            assert!(text.contains(want), "{want}\n---\n{text}");
        }
        let orphan = baked_of(&props(json!({"lightmap_rect": [0.0, 0.0, 1.0, 1.0]}))).unwrap_err();
        assert!(orphan.join("\n").contains("`lightmap_rect` does nothing without `lightmap`"), "{orphan:?}");
    }

    #[test]
    fn a_baked_light_definition_reads_with_its_defaults_and_names_its_mistakes() {
        let read: BakedLightDef = ron::from_str(r#"(sets: {"on": "baked/on", "off": "baked/off"}, set: "on")"#).unwrap();
        assert_eq!((read.lightmap_exposure, read.irradiance_intensity, read.reflection_intensity, read.bicubic), (1.0, 1.0, 1.0, false));
        assert!(read.problems().is_empty(), "{:?}", read.problems());
        assert_eq!(read.folder("off"), Some("baked/off"));

        let bad = BakedLightDef {
            sets: BTreeMap::from([("off".to_owned(), "baked/off".to_owned()), ("up".to_owned(), "../baked".to_owned())]),
            set: "offf".into(),
            lightmap_exposure: -1.0,
            reflection_intensity: f32::NAN,
            ..read
        };
        let text = bad.problems().join("\n");
        for want in ["`set`: no set `offf` in `sets` (did you mean `off`?)", "set `up`: `../baked` must be a path inside the asset folder", "`lightmap_exposure`: 0 or more, not -1", "`reflection_intensity`: 0 or more, not NaN"] {
            assert!(text.contains(want), "{want}\n---\n{text}");
        }
        let empty = BakedLightDef { sets: BTreeMap::new(), set: "on".into(), ..bad };
        assert!(empty.problems()[0].starts_with("`sets` is empty"), "{:?}", empty.problems());
    }

    #[test]
    fn the_game_chooses_a_set_and_an_unknown_one_falls_back_to_the_definitions() {
        let def: BakedLightDef = ron::from_str(r#"(sets: {"on": "baked/on", "off": "baked/off"}, set: "on")"#).unwrap();
        let mut light = BakedLight::default();
        assert_eq!((light.set_of("hall", &def), light.chosen("hall")), ("on", None));
        light.select("hall", "off");
        assert_eq!((light.set_of("hall", &def), light.chosen("hall")), ("off", Some("off")));
        // A set the definition lost (a content edit): the definition's own is drawn, the choice kept for when it is back.
        light.select("hall", "breach");
        assert_eq!((light.set_of("hall", &def), light.chosen("hall")), ("on", Some("breach")));
        light.reset("hall");
        assert_eq!(light, BakedLight::default());
    }
}
