//! A node's custom properties read as instructions: what to spawn for it.
//!
//! Blender's *Custom Properties* panel on an object lands in the glTF node's `extras`. Two
//! kinds of key live there:
//!
//! - **Level properties**, lowercase words the engine reads itself ([`PROPERTIES`]):
//!   `marker`, `collider`, `volume`, `prefab`, ... They become markers, colliders and
//!   volumes.
//! - **Components**, everything else: the key is a component's short name (`Health`) or
//!   full type path (`my_game::combat::Health`) and the value is the component as JSON, the
//!   shape `inspect` and the `get` tool show. They are built through reflection, like
//!   prefabs.
//!
//! Nothing here touches the world: [`plan_node`] turns one node into a [`NodePlan`] or a
//! list of problems, which is what both `content check` and spawning need.

use std::any::TypeId;
use std::fmt;

use bevy::prelude::*;
use bevy::reflect::TypeRegistry;
use serde_json::{json, Map, Value};
use shards_content::db::closest;
use shards_content::prefab::{check_prefab, component_from_json, resolve_component};
use shards_content::{ContentDb, ContentDef};
use shards_light::LightDef;
use shards_motion::camera::GradingDef;
use shards_motion::{CollisionLayers, LayerMask, MoverDef, VolumeKind, PROBE_RANGE};

use crate::asset::{check_relative, json_kind, LevelAsset, LevelNode};
use crate::baked::BakedNode;
use crate::marker::{MarkerKind, RequiredMarker};

/// The level properties, in documentation order.
pub const PROPERTIES: [&str; 27] = [
    "marker",
    "name",
    "socket",
    "to",
    "collider",
    "size",
    "radius",
    "height",
    "offset",
    "layers",
    "collides_with",
    "volume",
    "accel",
    "max_speed",
    "face",
    "prefab",
    "mover",
    "light",
    "grading",
    "hold_s",
    "use_range",
    "prompt",
    "lightmap",
    "lightmap_rect",
    "irradiance_volume",
    "reflection_probe",
    "probe_falloff",
];

/// The level properties of baked light (the picture's, [`BakedNode`]): nothing spawns for them.
pub(crate) const BAKED_PROPERTIES: [&str; 5] = ["lightmap", "lightmap_rect", "irradiance_volume", "reflection_probe", "probe_falloff"];

/// Components the file cannot set: the engine owns them.
const ENGINE_OWNED: [(&str, &str); 3] = [
    ("ai_core::id::AiId", "AiId is assigned by the engine"),
    ("bevy_transform::components::transform::Transform", "the pose comes from the node's place in the file"),
    ("bevy_transform::components::global_transform::GlobalTransform", "the pose comes from the node's place in the file"),
];

/// Something wrong in a level file, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelProblem {
    /// The node's path (`Props/Crate.003`); `None` for the file as a whole.
    pub node: Option<String>,
    pub message: String,
}

impl LevelProblem {
    fn at(node: &LevelNode, message: impl Into<String>) -> Self {
        Self { node: Some(node.path.clone()), message: message.into() }
    }

    fn file(message: impl Into<String>) -> Self {
        Self { node: None, message: message.into() }
    }
}

/// `node `Props/Crate.003`: unknown component `Helth` (did you mean `Health`?)`.
impl fmt::Display for LevelProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node {
            Some(node) => write!(f, "node `{node}`: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// The size of a collider, as far as the file says. A missing dimension comes from the
/// node's mesh bounds, so it can only be filled in when the node's scale is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ShapeSpec {
    /// Full extents.
    Box { size: Option<Vec3> },
    Sphere { radius: Option<f32> },
    /// `height` counts the caps, like `Shape::capsule`.
    Capsule { radius: Option<f32>, height: Option<f32> },
    Cylinder { radius: Option<f32>, height: Option<f32> },
    Cone { radius: Option<f32>, height: Option<f32> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColliderSpec {
    pub shape: ShapeSpec,
    /// In the node's space; the middle of the mesh bounds when the node has a mesh.
    pub offset: Option<Vec3>,
    pub layers: CollisionLayers,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MarkerSpec {
    pub kind: MarkerKind,
    pub name: String,
    /// A navlink's end: the name of the node it goes to.
    pub link: Option<String>,
    /// For an `interactable`: how it is used (F5).
    pub using: UseSpec,
}

/// How an `interactable` is used: the node properties `hold_s`, `use_range` and `prompt`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct UseSpec {
    /// Seconds the button is held for a use; 0: a press.
    pub hold_s: f32,
    /// How far from the eye it can be used (m); `None`: the body's own reach.
    pub use_range: Option<f32>,
    /// What a HUD shows; empty: the marker's name.
    pub prompt: String,
}

/// A node that spawns a content prefab; the node's component properties patch it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrefabUse {
    pub id: String,
    pub overrides: Map<String, Value>,
}

/// What to spawn for one node.
pub(crate) struct NodePlan {
    /// Position in [`LevelAsset::nodes`].
    pub node: usize,
    pub marker: Option<MarkerSpec>,
    pub collider: Option<ColliderSpec>,
    pub volume: Option<VolumeKind>,
    pub prefab: Option<PrefabUse>,
    /// `mover=<id>`: the node is a mover (`shards_motion::Mover`) moving by this `mover` definition,
    /// and the nodes below it move with it.
    pub mover: Option<String>,
    /// `light=<id>`: the node shines by this `light` definition (`shards_light::ContentLight`): a lamp, or an empty.
    pub light: Option<String>,
    /// `grading=<id>`: the trigger volume of the node blends the picture toward this `grading` definition
    /// (`shards_motion::camera::GradingVolume`) while the player is inside.
    pub grading: Option<String>,
    pub components: Vec<(TypeId, Box<dyn Reflect>)>,
}

impl NodePlan {
    fn new(node: usize) -> Self {
        Self { node, marker: None, collider: None, volume: None, prefab: None, mover: None, light: None, grading: None, components: Vec::new() }
    }

    /// Nodes that carry nothing for gameplay (groups, plain meshes) spawn no entity.
    pub fn spawns(&self) -> bool {
        self.marker.is_some()
            || self.collider.is_some()
            || self.volume.is_some()
            || self.prefab.is_some()
            || self.mover.is_some()
            || self.light.is_some()
            || self.grading.is_some()
            || !self.components.is_empty()
    }
}

// --- reading values --------------------------------------------------------------------

fn text<'a>(props: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match props.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim())),
        Some(other) => Err(format!("`{key}` must be text, found {}", json_kind(other))),
    }
}

/// The name of a node: text, or the object itself. Blender's *Data-Block* property type
/// points at an object, and the exporter writes it as `{"name": "Ladder Top", "type":
/// "Object"}`.
fn node_name<'a>(props: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    let Some(Value::Object(reference)) = props.get(key) else {
        return text(props, key).map_err(|_| {
            let found = props.get(key).map_or("nothing", json_kind);
            format!("`{key}` must be a node's name or the object itself (a Data-Block property), found {found}")
        });
    };
    match (reference.get("name").and_then(Value::as_str), reference.get("type").and_then(Value::as_str)) {
        (Some(name), None | Some("Object")) => Ok(Some(name.trim())),
        (Some(name), Some(other)) => Err(format!("`{key}` points at the {other} `{name}`; pick the object (Data-Block type Object)")),
        (None, _) => Err(format!("`{key}` must be a node's name or the object itself (a Data-Block property), found an object without `name`")),
    }
}

/// A finite number; text that holds one counts (Blender's string fields).
fn number(props: &Map<String, Value>, key: &str) -> Result<Option<f32>, String> {
    let value = match props.get(key) {
        None => return Ok(None),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(_) => None,
    };
    match value {
        Some(v) if v.is_finite() && (v as f32).is_finite() => Ok(Some(v as f32)),
        // A number too big for an f32 would be infinity once read: it is no number the level can use.
        Some(v) if v.is_finite() => Err(format!("`{key}` is too big for a number here (found {v})")),
        _ => Err(format!("`{key}` must be a number, found {}", props.get(key).map_or("nothing", json_kind))),
    }
}

fn positive(props: &Map<String, Value>, key: &str) -> Result<Option<f32>, String> {
    match number(props, key)? {
        Some(v) if v <= 0.0 => Err(format!("`{key}` must be above zero, found {v}")),
        other => Ok(other),
    }
}

/// `[x, y, z]`, or one number for all three when `scalar` allows.
fn vector(props: &Map<String, Value>, key: &str, scalar: bool) -> Result<Option<Vec3>, String> {
    let Some(value) = props.get(key) else { return Ok(None) };
    let as_f32 = |v: &Value| v.as_f64().filter(|f| f.is_finite()).map(|f| f as f32);
    let read = match value {
        Value::Array(items) if items.len() == 3 => {
            let parts: Vec<Option<f32>> = items.iter().map(as_f32).collect();
            parts.iter().all(Option::is_some).then(|| Vec3::new(parts[0].unwrap_or(0.0), parts[1].unwrap_or(0.0), parts[2].unwrap_or(0.0)))
        }
        other if scalar => as_f32(other).map(Vec3::splat),
        _ => None,
    };
    read.map(Some).ok_or_else(|| {
        let want = if scalar { "[x, y, z] or one number" } else { "[x, y, z]" };
        format!("`{key}` must be {want} (meters), found {}", json_kind(value))
    })
}

/// One name or a list of names.
fn names<'a>(props: &'a Map<String, Value>, key: &str) -> Result<Option<Vec<&'a str>>, String> {
    match props.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(vec![s.trim()])),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::trim).ok_or_else(|| format!("`{key}` must be names, found {} in the list", json_kind(v))))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(other) => Err(format!("`{key}` must be a name or a list of names, found {}", json_kind(other))),
    }
}

/// `["world", "game0"]` as a mask.
fn layer_mask(key: &str, names: &[&str]) -> Result<LayerMask, String> {
    const FIXED: [(&str, LayerMask); 9] = [
        ("world", LayerMask::WORLD),
        ("character", LayerMask::CHARACTER),
        ("hurtbox", LayerMask::HURTBOX),
        ("hitbox", LayerMask::HITBOX),
        ("volume", LayerMask::VOLUME),
        ("projectile", LayerMask::PROJECTILE),
        ("use", LayerMask::USE),
        ("all", LayerMask::ALL),
        ("none", LayerMask::NONE),
    ];
    let mut mask = LayerMask::NONE;
    for name in names {
        let lower = name.to_ascii_lowercase();
        let game = lower.strip_prefix("game").and_then(|n| n.parse::<u32>().ok()).filter(|n| *n < 16);
        mask |= match (FIXED.iter().find(|(n, _)| *n == lower), game) {
            (Some((_, mask)), _) => *mask,
            (None, Some(n)) => LayerMask::game(n),
            (None, None) => {
                let all: Vec<&str> = FIXED.iter().map(|(n, _)| *n).chain(["game0", "game15"]).collect();
                let hint = closest(&lower, all.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
                return Err(format!("`{key}`: unknown layer `{name}`{hint}; layers: world, character, hurtbox, hitbox, volume, projectile, use, game0..game15, all, none"));
            }
        };
    }
    Ok(mask)
}

// --- markers ---------------------------------------------------------------------------

/// The marker a node declares, with its name; `None` for a node that is no marker. An error
/// says what is wrong with its `marker`, `socket` or `name`.
pub(crate) fn marker_of(node: &LevelNode) -> Result<Option<(MarkerKind, String)>, String> {
    let props = &node.extras;
    let (marker, socket, volume, name) = (text(props, "marker")?, text(props, "socket")?, text(props, "volume")?, text(props, "name")?);
    let kind = match (marker, socket) {
        (Some(m), socket) => {
            let kind: MarkerKind = m.parse().map_err(|e| format!("`marker`: {e}"))?;
            if socket.is_some() && kind != MarkerKind::Socket {
                return Err(format!("`socket` makes a socket marker, but `marker` says `{kind}`; use one of them"));
            }
            kind
        }
        (None, Some(_)) => MarkerKind::Socket,
        // A trigger volume is a trigger marker.
        (None, None) if volume == Some("trigger") => MarkerKind::Trigger,
        (None, None) => return Ok(None),
    };
    let name = if kind == MarkerKind::Socket { socket.or(name) } else { name };
    let name = name.filter(|n| !n.is_empty()).map_or_else(|| node.name.clone(), str::to_owned);
    Ok(Some((kind, name)))
}

// --- planning --------------------------------------------------------------------------

/// Reads the properties of one node. `registry` resolves the component names. Returns
/// everything that could be planned and every problem found: a plan with problems is
/// not to be spawned.
pub(crate) fn plan_node(node: &LevelNode, position: usize, registry: &TypeRegistry) -> (NodePlan, Vec<String>) {
    let mut plan = NodePlan::new(position);
    let mut problems = Vec::new();
    if let Some(problem) = &node.extras_problem {
        problems.push(problem.clone());
        return (plan, problems);
    }
    let props = &node.extras;
    // Properties on the mesh data instead of the object are not read: say so, for the ones
    // meant for the engine (other tools may keep their own data on meshes).
    let stray: Vec<&str> = node
        .mesh_extras
        .keys()
        .filter(|k| !props.contains_key(*k) && (PROPERTIES.contains(&k.as_str()) || resolve_component(registry, k).is_ok()))
        .map(String::as_str)
        .collect();
    if !stray.is_empty() {
        problems.push(format!(
            "custom properties on mesh `{}` are not read: `{}`; the engine reads the object's (Object Properties → Custom \
             Properties), move them there",
            node.mesh.as_deref().unwrap_or_default(),
            stray.join("`, `")
        ));
    }
    // Properties that are always read; the others must be used by something on the node.
    let mut used: Vec<&'static str> = vec!["marker", "socket", "volume", "collider", "prefab", "mover", "light", "grading"];

    // Components and unknown keys.
    let mut entries: Vec<(&String, &Value)> = Vec::new();
    for (key, value) in props {
        if key.starts_with('_') || PROPERTIES.contains(&key.as_str()) {
            continue;
        }
        // Lowercase words are properties (types are `CamelCase`): a typo in one must not
        // turn into a component lookup that fails with a confusing hint.
        let property_like = key.starts_with(|c: char| c.is_ascii_lowercase()) && !key.contains("::");
        if property_like && let Err(why) = resolve_component(registry, key) {
            let known = PROPERTIES.join(", ");
            problems.push(match closest(key, PROPERTIES.into_iter()) {
                Some(close) => format!("unknown property `{key}` (did you mean `{close}`?)"),
                // `health` was probably meant to be the component `Health`.
                None if why.contains("did you mean") => format!("{why}; level properties are {known}"),
                None => format!("unknown property `{key}`; level properties are {known}; a component is written like `Health`"),
            });
            continue;
        }
        entries.push((key, value));
    }

    // Marker.
    match marker_of(node) {
        Ok(Some((kind, name))) => {
            used.push("name");
            let link = match (&kind, node_name(props, "to")) {
                (MarkerKind::NavLink, Ok(Some(to))) if !to.is_empty() => {
                    used.push("to");
                    Some(to.to_owned())
                }
                (MarkerKind::NavLink, Ok(_)) => {
                    problems.push("a navlink needs `to`: the name of the node where it ends".into());
                    None
                }
                (MarkerKind::NavLink, Err(e)) => {
                    used.push("to");
                    problems.push(e);
                    None
                }
                (_, Err(e)) => {
                    problems.push(e);
                    None
                }
                _ => None,
            };
            let using = if kind == MarkerKind::Interactable { read_use(props, &mut used, &mut problems) } else { UseSpec::default() };
            plan.marker = Some(MarkerSpec { kind, name, link, using });
        }
        Ok(None) => {}
        Err(e) => problems.push(e),
    }

    // Volume and collider.
    let volume_word = text(props, "volume").unwrap_or_else(|e| {
        problems.push(e);
        None
    });
    let trigger_marker = plan.marker.as_ref().filter(|m| m.kind == MarkerKind::Trigger);
    let volume = match (volume_word, trigger_marker) {
        (None, None) => None,
        (None, Some(marker)) | (Some("trigger"), Some(marker)) => Some(Ok(VolumeKind::Trigger { name: marker.name.clone() })),
        (Some("trigger"), None) => Some(Err("a trigger volume is a `trigger` marker; `marker` says something else".to_owned())),
        (Some(word), Some(_)) => Some(Err(format!("`marker` is `trigger`, so the volume is a trigger, but `volume` says `{word}`"))),
        (Some("water"), None) => Some(Ok(VolumeKind::Water)),
        (Some("updraft"), None) => Some(read_updraft(props, &mut used)),
        (Some("ladder"), None) => Some(read_ladder(props, &mut used)),
        (Some(other), None) => {
            let volumes = ["water", "updraft", "ladder", "trigger"];
            let hint = closest(other, volumes.into_iter()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
            Some(Err(format!("unknown volume `{other}`{hint}; volumes: water, updraft, ladder, trigger")))
        }
    };
    match volume {
        Some(Ok(kind)) => plan.volume = Some(kind),
        Some(Err(e)) => problems.push(e),
        None => {}
    }
    let collider_word = text(props, "collider").unwrap_or_else(|e| {
        problems.push(e);
        None
    });
    // An interactable is looked at to be used (F5): its node's shape is its use area, unless the node has nothing to
    // take a shape from (a bare empty stays a plain marker, as it always was, and cannot be looked at).
    let interactable = plan.marker.as_ref().is_some_and(|m| m.kind == MarkerKind::Interactable);
    let shaped = interactable && (node.bounds.is_some() || props.contains_key("size"));
    if interactable && plan.volume.is_some() {
        // A sensor is skipped by every ray that does not ask for sensors: a look would never meet it.
        problems.push("an `interactable` cannot also be a `volume`: a look never meets a sensor; give the volume a node of its own".into());
    } else if collider_word.is_some() || plan.volume.is_some() || shaped {
        used.extend(["size", "radius", "height", "offset", "layers", "collides_with"]);
        let role = match (plan.volume.is_some(), interactable, collider_word.is_some()) {
            (true, ..) => ColliderRole::Volume,
            (false, true, true) => ColliderRole::SolidUse,
            (false, true, false) => ColliderRole::Use,
            (false, false, _) => ColliderRole::Solid,
        };
        match plan_collider(node, collider_word.unwrap_or("auto"), role) {
            Ok(spec) => plan.collider = Some(spec),
            Err(e) => problems.push(e),
        }
    }

    // Prefab or components.
    match text(props, "prefab") {
        Ok(Some("")) => problems.push("`prefab` is empty; name a prefab id (`content_list kind=prefab`)".into()),
        Ok(Some(id)) => {
            let mut overrides = Map::new();
            for (key, value) in entries {
                match resolve_component(registry, key) {
                    Ok(reg) => match engine_owned(reg.type_info().type_path()) {
                        Some(why) => problems.push(format!("`{key}` cannot be set here: {why}")),
                        None => {
                            overrides.insert(key.clone(), patch_value(value));
                        }
                    },
                    Err(why) => problems.push(why),
                }
            }
            plan.prefab = Some(PrefabUse { id: id.to_owned(), overrides });
        }
        Ok(None) => {
            for (key, value) in entries {
                match resolve_component(registry, key) {
                    Ok(reg) => {
                        if let Some(why) = engine_owned(reg.type_info().type_path()) {
                            problems.push(format!("`{key}` cannot be set here: {why}"));
                            continue;
                        }
                        match read_value(registry, reg, value) {
                            Ok(component) => plan.components.push((reg.type_id(), component)),
                            Err(why) => problems.push(why),
                        }
                    }
                    Err(why) => problems.push(why),
                }
            }
        }
        Err(e) => problems.push(e),
    }

    // Mover: the node and the nodes below it move together.
    match text(props, "mover") {
        Ok(Some("")) => problems.push("`mover` is empty; name a mover definition (`content_list kind=mover`)".into()),
        Ok(Some(id)) => plan.mover = Some(id.to_owned()),
        Ok(None) => {}
        Err(e) => problems.push(e),
    }

    // Light: the node shines by a `light` definition (a lamp's own light is replaced by it in the picture).
    match text(props, "light") {
        Ok(Some("")) => problems.push("`light` is empty; name a light definition (`content_list kind=light`)".into()),
        Ok(Some(id)) => plan.light = Some(id.to_owned()),
        Ok(None) => {}
        Err(e) => problems.push(e),
    }

    // Grading: a trigger volume that blends the picture's colour grade.
    match text(props, "grading") {
        Ok(Some("")) => problems.push("`grading` is empty; name a grading definition (`content_list kind=grading`)".into()),
        Ok(Some(id)) => {
            if !matches!(plan.volume, Some(VolumeKind::Trigger { .. })) {
                problems.push(
                    "`grading` goes on a trigger volume: add `marker` = `trigger` (a collider shapes it, `auto` by default) and the \
                     picture blends toward the grading while the player is inside"
                        .into(),
                );
            }
            plan.grading = Some(id.to_owned());
        }
        Ok(None) => {}
        Err(e) => problems.push(e),
    }

    // Baked light: the picture's (a `baked_light` definition says where its files are); the node spawns nothing for it.
    if let Err(found) = baked_of(props) {
        problems.extend(found);
    }
    used.extend(BAKED_PROPERTIES);

    for key in PROPERTIES {
        if props.contains_key(key) && !used.contains(&key) {
            problems.push(format!("`{key}` does nothing on this node: {}", why_unused(key)));
        }
    }
    // The same fault can be found twice (`volume` is read by the marker and the volume).
    let mut seen = Vec::new();
    problems.retain(|p| !seen.contains(p) && {
        seen.push(p.clone());
        true
    });
    (plan, problems)
}

/// What a node's properties say about baked light: `None` when it names none, the problems when they are wrong.
pub(crate) fn baked_of(props: &Map<String, Value>) -> Result<Option<BakedNode>, Vec<String>> {
    if !BAKED_PROPERTIES.iter().any(|k| props.contains_key(*k)) {
        return Ok(None);
    }
    let mut node = BakedNode::default();
    let mut problems = Vec::new();
    let mut file = |key: &str| -> Option<String> {
        match text(props, key) {
            Ok(Some("")) => {
                problems.push(format!("`{key}` is empty; name a file in the folders of the level's baked sets (a `baked_light` definition)"));
                None
            }
            Ok(Some(name)) => match check_relative(name) {
                Ok(()) => Some(name.to_owned()),
                Err(_) => {
                    problems.push(format!("`{key}` `{name}` must be a file name inside a baked set's folder (relative, no `..`)"));
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                problems.push(e);
                None
            }
        }
    };
    node.lightmap = file("lightmap");
    node.irradiance_volume = file("irradiance_volume");
    node.reflection_probe = file("reflection_probe");
    if let Some(value) = props.get("lightmap_rect") {
        let parts: Option<Vec<f32>> = value.as_array().filter(|a| a.len() == 4).and_then(|a| a.iter().map(|v| v.as_f64().map(|f| f as f32)).collect());
        match parts.as_deref() {
            Some(&[x0, y0, x1, y1]) if [x0, y0, x1, y1].iter().all(|v| (0.0..=1.0).contains(v)) && x0 < x1 && y0 < y1 => {
                node.lightmap_rect = Rect::new(x0, y0, x1, y1);
            }
            _ => problems.push(format!(
                "`lightmap_rect` must be [x0, y0, x1, y1] within 0 to 1 with x0 < x1 and y0 < y1 (the top left first), found {value}"
            )),
        }
        if !props.contains_key("lightmap") {
            problems.push("`lightmap_rect` does nothing without `lightmap`: it is the part of that image the mesh covers".to_owned());
        }
    }
    if let Some(value) = props.get("probe_falloff") {
        let as_f32 = |v: &Value| v.as_f64().map(|f| f as f32);
        let read = match value {
            Value::Array(items) if items.len() == 3 => {
                let v: Option<Vec<f32>> = items.iter().map(as_f32).collect();
                v.map(|v| Vec3::new(v[0], v[1], v[2]))
            }
            other => as_f32(other).map(Vec3::splat),
        };
        match read {
            Some(v) if v.to_array().iter().all(|c| (0.0..=1.0).contains(c)) => node.probe_falloff = v,
            _ => problems.push(format!("`probe_falloff` must be [x, y, z] or one number, each 0 to 1, found {value}")),
        }
        if !props.contains_key("irradiance_volume") && !props.contains_key("reflection_probe") {
            problems.push("`probe_falloff` does nothing without `irradiance_volume` or `reflection_probe`: it is how a probe's box fades".to_owned());
        }
    }
    if problems.is_empty() { Ok(Some(node)) } else { Err(problems) }
}

fn why_unused(key: &str) -> &'static str {
    match key {
        "name" => "it names a marker, and the node has no `marker`",
        "to" => "it is where a navlink ends, and the node is no navlink",
        "size" | "radius" | "height" | "offset" | "layers" | "collides_with" => "it shapes a collider, and the node has no `collider` or `volume`",
        "accel" | "max_speed" => "they belong to an updraft volume",
        "face" => "it is the side a ladder volume is climbed from, and the node is no ladder",
        "hold_s" | "use_range" | "prompt" => "it says how an `interactable` is used, and the node is no interactable marker",
        _ => "check the spelling of the property",
    }
}

fn engine_owned(type_path: &str) -> Option<&'static str> {
    ENGINE_OWNED.iter().find(|(path, _)| *path == type_path).map(|(_, why)| *why)
}

fn read_updraft(props: &Map<String, Value>, used: &mut Vec<&'static str>) -> Result<VolumeKind, String> {
    used.extend(["accel", "max_speed"]);
    match (positive(props, "accel")?, positive(props, "max_speed")?) {
        (Some(accel), Some(max_speed)) => Ok(VolumeKind::Updraft { accel, max_speed }),
        _ => Err("an updraft needs `accel` (m/s²) and `max_speed` (m/s), both above zero".into()),
    }
}

/// A ladder's `face`: the way out of the ladder, toward the side it is climbed from, in the node's space.
/// Kept at length 1; the controller uses its level part once the node's rotation has turned it.
fn read_ladder(props: &Map<String, Value>, used: &mut Vec<&'static str>) -> Result<VolumeKind, String> {
    used.push("face");
    const NEEDS: &str = "a ladder needs `face`: `[x, y, z]`, the way out of the ladder toward the side it is climbed from (away \
                         from the wall it hangs on), in the node's space, +Y up";
    // A direction, not a length: the vector reader's unit does not belong in its message.
    match vector(props, "face", false).map_err(|e| e.replace(" (meters)", ""))? {
        Some(face) if Vec3::new(face.x, 0.0, face.z).length_squared() > 1e-6 => Ok(VolumeKind::Ladder { face: face.normalize() }),
        Some(face) => Err(format!("`face` must point sideways out of the ladder, found {face:?}: a ladder is climbed from one of its sides, not from above or below")),
        None => Err(NEEDS.into()),
    }
}

/// The collider properties of a node: shape word, dimensions, offset and layers.
fn plan_collider(node: &LevelNode, word: &str, role: ColliderRole) -> Result<ColliderSpec, String> {
    let props = &node.extras;
    let size = vector(props, "size", true)?;
    let radius = positive(props, "radius")?;
    let height = positive(props, "height")?;
    let size = size.map(|s| s.abs()).filter(|s| s.min_element() > 0.0);
    if props.contains_key("size") && size.is_none() {
        return Err("`size` must be above zero on every axis".into());
    }
    let reject = |key: &str, present: bool| -> Result<(), String> {
        if present { Err(format!("`{key}` does not apply to a `{word}` collider")) } else { Ok(()) }
    };
    let shape = match word {
        "box" | "auto" => {
            reject("radius", radius.is_some())?;
            reject("height", height.is_some())?;
            ShapeSpec::Box { size }
        }
        "sphere" => {
            reject("size", size.is_some())?;
            reject("height", height.is_some())?;
            ShapeSpec::Sphere { radius }
        }
        "capsule" | "cylinder" | "cone" => {
            reject("size", size.is_some())?;
            match word {
                "capsule" => ShapeSpec::Capsule { radius, height },
                "cylinder" => ShapeSpec::Cylinder { radius, height },
                _ => ShapeSpec::Cone { radius, height },
            }
        }
        other => {
            let words = ["box", "sphere", "capsule", "cylinder", "cone", "auto"];
            let hint = closest(other, words.into_iter()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
            return Err(format!("unknown collider `{other}`{hint}; colliders: box, sphere, capsule, cylinder, cone, auto (a box fitted to the mesh)"));
        }
    };
    // What the file leaves out comes from the mesh.
    let missing = match shape {
        ShapeSpec::Box { size } => size.is_none(),
        ShapeSpec::Sphere { radius } => radius.is_none(),
        ShapeSpec::Capsule { radius, height } | ShapeSpec::Cylinder { radius, height } | ShapeSpec::Cone { radius, height } => {
            radius.is_none() || height.is_none()
        }
    };
    if missing && node.bounds.is_none() {
        let give = "give the size (`size`, `radius`, `height`) or put the property on a mesh object";
        return Err(match &node.mesh {
            Some(mesh) => format!("collider `{word}` takes its size from mesh `{mesh}`, which has no vertex positions; {give}"),
            None => format!("collider `{word}` takes its size from the node's mesh, and the node has none; {give}"),
        });
    }
    let offset = vector(props, "offset", false)?;

    let memberships = names(props, "layers")?;
    let filters = names(props, "collides_with")?;
    let preset = match role {
        ColliderRole::Solid | ColliderRole::SolidUse => CollisionLayers::WORLD,
        ColliderRole::Volume => CollisionLayers::VOLUME,
        ColliderRole::Use => CollisionLayers::USE,
    };
    let layers = CollisionLayers::new(
        match &memberships {
            Some(list) => layer_mask("layers", list)?,
            None => preset.memberships,
        },
        match (&filters, &memberships) {
            (Some(list), _) => layer_mask("collides_with", list)?,
            (None, None) => preset.filters,
            // Own memberships, nothing said about what they hit: solids hit everything,
            // sensors characters, use areas nothing.
            (None, Some(_)) => match role {
                ColliderRole::Solid | ColliderRole::SolidUse => LayerMask::ALL,
                ColliderRole::Volume => LayerMask::CHARACTER,
                ColliderRole::Use => LayerMask::NONE,
            },
        },
    );
    // An interactable's collider is on the `USE` layer whatever else it is on: that is where a look finds it.
    let layers = match role {
        ColliderRole::Use | ColliderRole::SolidUse => CollisionLayers::new(layers.memberships | LayerMask::USE, layers.filters),
        ColliderRole::Solid | ColliderRole::Volume => layers,
    };
    Ok(ColliderSpec { shape, offset, layers })
}

/// What a node's collider is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColliderRole {
    /// Level geometry: blocks.
    Solid,
    /// A sensor volume.
    Volume,
    /// The use area of an `interactable` with no solid of its own: on `USE`, blocks nothing.
    Use,
    /// An `interactable` that is also solid (a door): on `WORLD` and `USE`.
    SolidUse,
}

/// The properties of an `interactable` that say how it is used: `hold_s` (seconds, zero or more),
/// `use_range` (metres, above zero) and `prompt` (text).
fn read_use(props: &Map<String, Value>, used: &mut Vec<&'static str>, problems: &mut Vec<String>) -> UseSpec {
    used.extend(["hold_s", "use_range", "prompt"]);
    let mut spec = UseSpec::default();
    match number(props, "hold_s") {
        Ok(Some(v)) if v < 0.0 => problems.push(format!("`hold_s` must be zero or more (seconds to hold the button), found {v}")),
        Ok(Some(v)) => spec.hold_s = v,
        Ok(None) => {}
        Err(e) => problems.push(e),
    }
    match positive(props, "use_range") {
        Ok(Some(v)) if v > PROBE_RANGE => problems.push(format!("`use_range` must be at most {PROBE_RANGE} (metres, how far a use is looked for), found {v}")),
        Ok(v) => spec.use_range = v,
        Err(e) => problems.push(e),
    }
    match text(props, "prompt") {
        Ok(Some(t)) => spec.prompt = t.to_owned(),
        Ok(None) => {}
        Err(e) => problems.push(e),
    }
    spec
}

/// A component value from JSON: as written, or (Blender's property panel only makes text,
/// numbers and lists) from the JSON a text value holds, or `{}` for a flag on a component
/// with no fields.
fn read_value(registry: &TypeRegistry, reg: &bevy::reflect::TypeRegistration, value: &Value) -> Result<Box<dyn Reflect>, String> {
    let first = component_from_json(registry, reg, value);
    let Err(error) = first else { return first };
    let alternatives: Vec<Value> = match value {
        Value::String(s) if s.trim().is_empty() => vec![json!({})],
        Value::String(s) => parse_json_text(s).into_iter().collect(),
        Value::Bool(true) | Value::Null => vec![json!({})],
        _ => Vec::new(),
    };
    for alternative in alternatives {
        if let Ok(component) = component_from_json(registry, reg, &alternative) {
            return Ok(component);
        }
    }
    Err(error)
}

/// `"{\"current\": 10}"` as a JSON object; other text stays text.
fn parse_json_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if !trimmed.starts_with(['{', '[']) {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// A prefab override: a text that holds a JSON object or list is that object or list.
fn patch_value(value: &Value) -> Value {
    match value {
        Value::String(s) => parse_json_text(s).unwrap_or_else(|| value.clone()),
        other => other.clone(),
    }
}

// --- the whole file --------------------------------------------------------------------

/// Plans every node of the level and collects every problem. `db` lets prefab nodes be
/// checked down to their overrides (without it only the component names are).
pub(crate) fn analyze(level: &LevelAsset, registry: &TypeRegistry, db: Option<&ContentDb>) -> (Vec<NodePlan>, Vec<LevelProblem>) {
    let mut plans = Vec::with_capacity(level.nodes.len());
    let mut problems = Vec::new();
    for (position, node) in level.nodes.iter().enumerate() {
        let (plan, found) = plan_node(node, position, registry);
        problems.extend(found.into_iter().map(|m| LevelProblem::at(node, m)));
        if let (Some(db), Some(prefab)) = (db, &plan.prefab) {
            let overrides = Value::Object(prefab.overrides.clone());
            if let Err(why) = check_prefab(db, registry, &prefab.id, Some(&overrides)) {
                problems.push(LevelProblem::at(node, why));
            }
        }
        if let Some(MarkerSpec { link: Some(target), .. }) = &plan.marker
            && level.node(target).is_none()
        {
            let names: Vec<&str> = level.nodes.iter().map(|n| n.name.as_str()).collect();
            let hint = closest(target, names.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
            problems.push(LevelProblem::at(node, format!("navlink `to` names no node: `{target}`{hint}")));
        }
        if let (Some(db), Some(id)) = (db, &plan.mover)
            && let Some(why) = unknown_mover(db, id)
        {
            problems.push(LevelProblem::at(node, why));
        }
        if let (Some(db), Some(id)) = (db, &plan.light)
            && let Some(why) = unknown_definition::<LightDef>(db, id, "light")
        {
            problems.push(LevelProblem::at(node, why));
        }
        if let (Some(db), Some(id)) = (db, &plan.grading)
            && let Some(why) = unknown_definition::<GradingDef>(db, id, "grading")
        {
            problems.push(LevelProblem::at(node, why));
        }
        plans.push(plan);
    }
    for plan in plans.iter().filter(|p| p.mover.is_some()) {
        if let Some(outer) = mover_above(level, &plans, plan.node) {
            let node = &level.nodes[plan.node];
            problems.push(LevelProblem::at(
                node,
                format!(
                    "a mover inside the mover `{}`: movers do not nest (the nodes below a mover move with it); take `mover` off one of them",
                    level.nodes[outer].path
                ),
            ));
        }
    }
    (plans, problems)
}

/// Why `id` names no `mover` definition of the content, or `None` when it does.
fn unknown_mover(db: &ContentDb, id: &str) -> Option<String> {
    if db.get::<MoverDef>(id).is_some() {
        return None;
    }
    // A hint does not make the level depend on every mover: `peek_ids` is no read.
    let ids: Vec<&str> = db.peek_ids::<MoverDef>().collect();
    let hint = closest(id, ids.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
    let known = if ids.is_empty() { "there are none: add a content/**/*.mover.ron file".to_owned() } else { format!("movers: {}", ids.join(", ")) };
    Some(format!("`mover`: no mover definition `{id}`{hint}; {known}"))
}

/// Why `id` names no definition of the picture's kind `T` (`light`, `grading`), or `None` when it does. These kinds are
/// not the simulation's: the level does not depend on them, so the lookup is no read for the replay check.
fn unknown_definition<T: ContentDef>(db: &ContentDb, id: &str, property: &str) -> Option<String> {
    if db.peek::<T>(id).is_some() {
        return None;
    }
    let ids: Vec<&str> = db.peek_ids::<T>().collect();
    let hint = closest(id, ids.iter().copied()).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
    let known = if ids.is_empty() {
        format!("there are none: add a content/**/*.{}.ron file", T::KIND)
    } else {
        format!("{}s: {}", T::KIND, ids.join(", "))
    };
    Some(format!("`{property}`: no {} definition `{id}`{hint}; {known}", T::KIND))
}

/// The nearest node above the node at `position` (a position in [`LevelAsset::nodes`]) that is a
/// mover, as a position, if any.
pub(crate) fn mover_above(level: &LevelAsset, plans: &[NodePlan], position: usize) -> Option<usize> {
    let mut parent = level.nodes[position].parent;
    // A file's hierarchy is a tree; the bound only guards against a malformed one.
    for _ in 0..level.nodes.len() {
        let at = level.nodes.iter().position(|n| Some(n.index) == parent)?;
        if plans.get(at).is_some_and(|p| p.mover.is_some()) {
            return Some(at);
        }
        parent = level.nodes[at].parent;
    }
    None
}

/// Everything wrong with a level file: properties that do not parse, components that
/// do not exist or do not take the value, prefabs that are missing, navlinks that end
/// nowhere, and `required` markers that are absent. Empty when the file is good.
///
/// `registry` is the app's type registry (the components a game registered); `db`, when
/// given, checks prefab nodes against the content database.
pub fn check_level(level: &LevelAsset, registry: &TypeRegistry, db: Option<&ContentDb>, required: &[RequiredMarker]) -> Vec<LevelProblem> {
    let (_, mut problems) = analyze(level, registry, db);
    let markers = level.markers();
    for need in required {
        let found = markers.iter().filter(|m| m.kind == need.kind && need.name.as_ref().is_none_or(|n| *n == m.name)).count();
        if (found as u32) < need.min {
            let fix = format!(
                "add the custom property marker = \"{}\"{} to an object in Blender",
                need.kind,
                need.name.as_ref().map(|n| format!(" and name = \"{n}\"")).unwrap_or_default()
            );
            let file = if level.path.is_empty() { String::new() } else { format!(" in `{}`", level.path) };
            problems.push(LevelProblem::file(format!(
                "required marker missing: {}{file} (found {found}, need {}); {fix}",
                need.describe(),
                need.min
            )));
        }
    }
    problems
}
