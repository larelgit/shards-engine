//! Reading a level file: the glTF node tree with composed transforms, names, custom
//! properties (`extras`) and mesh bounds, without a GPU and without reading any buffer.
//!
//! The `gltf` crate parses the JSON (a `.gltf` file, or the JSON chunk of a `.glb`); the
//! rest is arithmetic on the node tree. Mesh bounds come from the `min` and `max` the
//! glTF format requires on every `POSITION` accessor, so external `.bin` files and images
//! are never touched. For the same reason the extensions a file marks required (lights,
//! materials, textures) do not stop it, except compressed meshes, which the error explains.
//!
//! Loading goes through the asset server like any asset: [`LevelLoader`] claims **no file
//! extension**. Bevy picks a loader by the asset type when a path has no `#label`, so
//! `asset_server.load::<LevelAsset>("levels/arena.gltf")` reads the file with this loader
//! while `GltfAssetLabel::Scene(0).from_asset("levels/arena.gltf")` (a label) still goes
//! to Bevy's own glTF loader by extension: one file, two readings, no fight over `.gltf`
//! and `.glb`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};

use bevy::asset::io::Reader;
use bevy::asset::{Asset, AssetLoader, LoadContext};
use bevy::prelude::*;
use serde_json::{Map, Value};

use crate::marker::MarkerKind;
use crate::props::marker_of;

/// An axis-aligned box in a node's own space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn half(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    fn union(self, other: Bounds) -> Bounds {
        Bounds { min: self.min.min(other.min), max: self.max.max(other.max) }
    }
}

/// One node of the file (a Blender object).
#[derive(Clone, Debug)]
pub struct LevelNode {
    /// The node's index in the file's `nodes` array: the order entities are spawned in.
    pub index: usize,
    /// The node's name (the Blender object's); `node<index>` when it has none.
    pub name: String,
    /// Names from the scene root down, joined by `/`: `Props/Crate.003`. Errors name nodes
    /// by it.
    pub path: String,
    /// The parent node's index in the file.
    pub parent: Option<usize>,
    /// The node's transform composed with its ancestors', relative to the file's origin.
    pub world: Mat4,
    /// The object's custom properties (Blender exports them as `extras`).
    pub extras: Map<String, Value>,
    /// Why the `extras` could not be read as an object of properties.
    pub extras_problem: Option<String>,
    /// The mesh on this node: its name. Meshes are visuals; only their bounds matter here.
    pub mesh: Option<String>,
    /// The custom properties of the node's mesh (Blender's *Object Data Properties*). The
    /// engine reads the object's, not these: checks only name the ones meant for it.
    pub mesh_extras: Map<String, Value>,
    /// The bounds of the mesh's `POSITION` data in the node's own space (all primitives).
    /// `None` without a mesh, or for a mesh with no vertex positions.
    pub bounds: Option<Bounds>,
    /// Whether every primitive of the mesh has a second UV map (`TEXCOORD_1`), which a lightmap is drawn by.
    pub uv1: bool,
}

/// A marker as the file declares it, for listings and for `required_markers`.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerInfo {
    pub kind: MarkerKind,
    pub name: String,
    /// The node's path.
    pub node: String,
    /// Where it is, relative to the file's origin.
    pub position: Vec3,
}

/// A level file, read: its nodes in file order. Load it with the asset server
/// (`asset_server.load::<LevelAsset>("levels/arena.gltf")`), or read it directly with
/// [`LevelAsset::from_slice`] and [`LevelAsset::read`].
#[derive(Asset, TypePath, Clone, Debug)]
pub struct LevelAsset {
    /// The asset path it was loaded from (`levels/arena.gltf`); empty when read from bytes.
    /// Its visuals load from this path.
    pub path: String,
    /// FNV-1a of the file's bytes: replay checks compare it, so a recording made against
    /// another version of the file diverges at its first check.
    pub digest: u64,
    /// The index of the scene the nodes come from: the file's default scene, else its first;
    /// `None` for a file without scenes. Blender writes every scene of the `.blend` (a kit
    /// of instanced collections may live in one) and makes the active one the default; the
    /// level's visuals show this scene.
    pub scene: Option<usize>,
    /// The scene's custom properties.
    pub extras: Map<String, Value>,
    /// The nodes of the scene (all root nodes when the file has no scene), in the order of
    /// the file's `nodes` array.
    pub nodes: Vec<LevelNode>,
}

impl LevelAsset {
    /// Reads a `.gltf` (JSON) or `.glb` file's bytes. `path` is only recorded.
    pub fn from_slice(path: &str, bytes: &[u8]) -> Result<LevelAsset, String> {
        let gltf = parse(bytes)?;
        let scene = gltf.default_scene().or_else(|| gltf.scenes().next());
        let scene_index = scene.as_ref().map(gltf::Scene::index);
        let (scene_extras, scene_problem) = match &scene {
            Some(scene) => parse_extras(scene.extras()),
            None => (Map::new(), None),
        };
        if let Some(problem) = scene_problem {
            return Err(format!("scene {problem}"));
        }
        let roots: Vec<gltf::Node> = match scene {
            Some(scene) => scene.nodes().collect(),
            None => {
                let children: Vec<usize> = gltf.nodes().flat_map(|n| n.children().map(|c| c.index()).collect::<Vec<_>>()).collect();
                gltf.nodes().filter(|n| !children.contains(&n.index())).collect()
            }
        };

        let mut found: BTreeMap<usize, LevelNode> = BTreeMap::new();
        // Depth first; the order of visiting does not matter, the result is sorted by index.
        let mut stack: Vec<(gltf::Node, Option<usize>, Mat4, String)> =
            roots.into_iter().rev().map(|node| (node, None, Mat4::IDENTITY, String::new())).collect();
        while let Some((node, parent, parent_world, parent_path)) = stack.pop() {
            if found.contains_key(&node.index()) {
                continue;
            }
            let name = node.name().map_or_else(|| format!("node{}", node.index()), str::to_owned);
            let path = if parent_path.is_empty() { name.clone() } else { format!("{parent_path}/{name}") };
            let world = parent_world * local_matrix(node.transform());
            let (extras, extras_problem) = parse_extras(node.extras());
            let (mesh, bounds, mesh_extras, uv1) = match node.mesh() {
                Some(mesh) => (
                    Some(mesh.name().map_or_else(|| format!("mesh{}", mesh.index()), str::to_owned)),
                    mesh_bounds(&mesh),
                    parse_extras(mesh.extras()).0,
                    mesh.primitives().len() > 0 && mesh.primitives().all(|p| p.get(&gltf::Semantic::TexCoords(1)).is_some()),
                ),
                None => (None, None, Map::new(), false),
            };
            let index = node.index();
            for child in node.children().collect::<Vec<_>>().into_iter().rev() {
                stack.push((child, Some(index), world, path.clone()));
            }
            found.insert(index, LevelNode { index, name, path, parent, world, extras, extras_problem, mesh, mesh_extras, bounds, uv1 });
        }

        Ok(LevelAsset {
            path: path.to_owned(),
            digest: fnv(bytes),
            scene: scene_index,
            extras: scene_extras,
            nodes: found.into_values().collect(),
        })
    }

    /// Reads the file `file` of the asset folder `root` straight from disk, for checks and
    /// tools that run without the asset server.
    pub fn read(root: &Path, file: &str) -> Result<LevelAsset, String> {
        check_relative(file)?;
        let bytes = std::fs::read(root.join(file)).map_err(|e| format!("cannot read `{file}`: {e} (levels live in the asset folder, {})", root.display()))?;
        Self::from_slice(file, &bytes).map_err(|e| format!("`{file}`: {e}"))
    }

    /// The first node called `name`. Names repeat below instanced collections (each instancer
    /// holds its own `Barrel`); [`node_at`](Self::node_at) tells those apart by path.
    pub fn node(&self, name: &str) -> Option<&LevelNode> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// The node at `path` (`Props/Crate.003`).
    pub fn node_at(&self, path: &str) -> Option<&LevelNode> {
        self.nodes.iter().find(|n| n.path == path)
    }

    /// Every marker the file declares, in file order. A node whose marker properties are
    /// wrong is left out here; [`check_level`](crate::check_level) reports it.
    pub fn markers(&self) -> Vec<MarkerInfo> {
        self.nodes
            .iter()
            .filter_map(|node| {
                let (kind, name) = marker_of(node).ok().flatten()?;
                Some(MarkerInfo { kind, name, node: node.path.clone(), position: node.world.w_axis.truncate() })
            })
            .collect()
    }
}

/// Required extensions that store meshes compressed or quantized: Bevy's glTF loader has no
/// decoder for them, so the level could not be drawn. Each entry says how to export without
/// it. Blender 5.2's exporter
/// writes Draco and both meshopt variants (*Data → Meshopt Compression* chooses which), but
/// never `KHR_mesh_quantization`: a file with it went through another tool.
const COMPRESSED_MESHES: [(&str, &str); 4] = [
    ("KHR_draco_mesh_compression", "in Blender's glTF export, untick *Data → Draco Compression* and export again"),
    ("EXT_meshopt_compression", "in Blender's glTF export, untick *Data → Meshopt Compression* and export again"),
    ("KHR_meshopt_compression", "in Blender's glTF export, untick *Data → Meshopt Compression* and export again"),
    (
        "KHR_mesh_quantization",
        "Blender's glTF export never quantizes, so another tool did (gltfpack does unless given `-noq`): export from Blender again, or run that tool without quantization",
    ),
];

/// The file's JSON, validated. Extensions the file marks required are no reason to refuse
/// it, except compressed meshes: the reader takes nothing but the node tree, names, extras
/// and `POSITION` bounds, which lights (`KHR_lights_punctual`, required in Blender's export
/// with *Punctual Lights*), materials and textures do not change. Whether the file can be
/// drawn is for Bevy's glTF loader to say. (The `gltf` crate refuses any required extension
/// whose cargo feature is off, so without this a file would read in a build with `render`
/// and not in one without.)
fn parse(bytes: &[u8]) -> Result<gltf::Document, String> {
    let invalid = |e: gltf::Error| format!("not a valid glTF file: {e}");
    let mut json = gltf::Gltf::from_slice_without_validation(bytes).map_err(invalid)?.document.into_json();
    if let Some((name, fix)) = COMPRESSED_MESHES.iter().find(|(name, _)| json.extensions_required.iter().any(|e| e == name)) {
        return Err(format!(
            "the meshes are compressed with `{name}`, which the engine cannot read or draw; {fix}"
        ));
    }
    json.extensions_required.clear();
    gltf::Document::from_json(json).map_err(invalid)
}

/// `levels/arena.gltf` is a path inside the asset folder: relative, no `..`.
pub(crate) fn check_relative(file: &str) -> Result<(), String> {
    let ok = !file.is_empty() && Path::new(file).components().all(|c| matches!(c, Component::Normal(_)));
    if ok { Ok(()) } else { Err(format!("`{file}` must be a path inside the asset folder, like `levels/arena.gltf` (relative, no `..`)")) }
}

fn local_matrix(transform: gltf::scene::Transform) -> Mat4 {
    match transform {
        gltf::scene::Transform::Matrix { matrix } => Mat4::from_cols_array_2d(&matrix),
        gltf::scene::Transform::Decomposed { translation, rotation, scale } => {
            let rotation = Quat::from_array(rotation);
            let rotation = if rotation.length_squared() > 0.0 && rotation.is_finite() { rotation.normalize() } else { Quat::IDENTITY };
            Mat4::from_scale_rotation_translation(Vec3::from(scale), rotation, Vec3::from(translation))
        }
    }
}

/// A node's custom properties, or why they are not an object.
fn parse_extras(extras: &gltf::json::Extras) -> (Map<String, Value>, Option<String>) {
    let Some(raw) = extras else { return (Map::new(), None) };
    match serde_json::from_str::<Value>(raw.get()) {
        Ok(Value::Object(map)) => (map, None),
        Ok(Value::Null) => (Map::new(), None),
        Ok(other) => (Map::new(), Some(format!("extras must be an object of custom properties, found {}", json_kind(&other)))),
        Err(e) => (Map::new(), Some(format!("extras are not valid JSON: {e}"))),
    }
}

pub(crate) fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The union of the `POSITION` bounds of every primitive of `mesh`. The format requires `min`
/// and `max` on `POSITION` accessors, and the reader's validation has checked them.
fn mesh_bounds(mesh: &gltf::Mesh) -> Option<Bounds> {
    let mut total: Option<Bounds> = None;
    for primitive in mesh.primitives() {
        let Some(accessor) = primitive.get(&gltf::Semantic::Positions) else { continue };
        let bounds = Bounds { min: accessor.min().as_ref().and_then(vec3)?, max: accessor.max().as_ref().and_then(vec3)? };
        total = Some(total.map_or(bounds, |t| t.union(bounds)));
    }
    total
}

fn vec3(value: &Value) -> Option<Vec3> {
    let items = value.as_array()?;
    let [x, y, z] = items.as_slice() else { return None };
    Some(Vec3::new(x.as_f64()? as f32, y.as_f64()? as f32, z.as_f64()? as f32))
}

/// FNV-1a, fed piece by piece.
pub(crate) struct Fnv(pub u64);

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn feed(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// Length-terminated, so "ab"+"c" and "a"+"bc" differ.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.feed(bytes);
        self.feed(&(bytes.len() as u64).to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.feed(&v.to_le_bytes());
    }
}

/// FNV-1a over `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv::new();
    h.feed(bytes);
    h.0
}

/// Why a level file did not load.
#[derive(Debug)]
pub struct LevelLoadError(pub String);

impl fmt::Display for LevelLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LevelLoadError {}

/// Loads a `.gltf` or `.glb` file as a [`LevelAsset`] when the asset type asks for it.
/// It claims no extension (see the module docs), so Bevy's glTF loader is left alone.
#[derive(TypePath, Default)]
pub struct LevelLoader;

impl AssetLoader for LevelLoader {
    type Asset = LevelAsset;
    type Settings = ();
    type Error = LevelLoadError;

    async fn load(&self, reader: &mut dyn Reader, _: &(), context: &mut LoadContext<'_>) -> Result<LevelAsset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.map_err(|e| LevelLoadError(format!("cannot read: {e}")))?;
        let path = context.path().path().to_string_lossy().replace('\\', "/");
        LevelAsset::from_slice(&path, &bytes).map_err(LevelLoadError)
    }

    fn extensions(&self) -> &[&str] {
        &[]
    }
}
