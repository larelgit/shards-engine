//! # shards_level — levels from Blender
//!
//! An artist builds a level in Blender, puts *custom properties* on objects and exports
//! glTF; the engine turns the file into entities: colliders and sensor volumes the physics
//! layer sees, markers (spawn points, sockets, navlinks, triggers, interactables, camps,
//! landmarks), prefabs from the content database, and any component of the game by name.
//! The same file also shows up as the level's visuals when the `render` feature is on.
//!
//! ```text
//! Blender (.blend) ── glTF export ──► levels/arena.gltf ──► LevelAsset ──► spawn_level ──► entities
//!   objects + custom properties        nodes + extras         (no GPU,        top-level,    Collider, Volume,
//!                                                              no buffers)     world space   LevelMarker, prefabs, ...
//! ```
//!
//! ## Making a level in Blender
//!
//! Checked on real exports of Blender 5.2.2 LTS: `tools/blender/outpost_level.py` builds a
//! level the way an artist does and exports it, and the tests read what it wrote.
//!
//! 1. Model the level in meters, Z up as Blender has it: the exporter turns it to +Y up
//!    (Blender `(x, y, z)` is `(x, z, -y)` in the file). Name objects as you like (spaces
//!    and any letters are fine): errors quote them as paths through the hierarchy
//!    (`Props/Crate.003`). Objects of an instanced collection appear below each instancer
//!    (`Barrels.A/Barrel`). Keep the collection itself in another scene: one only excluded
//!    from the level's view layer is exported too, at its own place.
//! 2. Select an object, open *Object Properties → Custom Properties*, add a property
//!    (the `+` button), type its name and value. A Blender 5.2.2 export was checked with text,
//!    numbers, booleans, float and text arrays, *Data-Block* (written as
//!    `{"name": ..., "type": ...}`) and *Python* (a dict or a list, written as JSON); integer
//!    and boolean arrays were not exported. Properties are read from **objects**;
//!    ones put on the mesh data (*Object Data Properties*) are an error that says so.
//! 3. *File → Export → glTF 2.0*. Under *Include*, tick **Custom Properties** (without it
//!    every property is dropped); under *Data → Mesh*, tick **Apply Modifiers** (without it
//!    the file has the meshes before their modifiers, and `auto` colliders fit those).
//!    *Transform → +Y Up* stays on. `.glb` and `.gltf` *Separate* work (*Embedded*, hidden
//!    behind an add-on setting in Blender 5.2, was not exported), and so do lights and materials; *Draco Compression* and *Meshopt Compression* (either of
//!    its extensions) do not (Bevy cannot draw them): the error says which box to untick.
//! 4. Put the file in the game's asset folder, list it in a content file
//!    (`content/levels/arena.level.ron`, see [`LevelDef`]), and run `shards content check`.
//!
//! ## Properties
//!
//! A node's properties are of two kinds: **level properties**, lowercase words the engine
//! reads, and **components**, `CamelCase` names of the game's components.
//!
//! | property | value | what it does |
//! |---|---|---|
//! | `marker` | `spawn`, `socket`, `navlink`, `trigger`, `interactable`, `camp`, `landmark`, `custom:<name>` | the node is a [`LevelMarker`] of that kind |
//! | `name` | text | the marker's name (`start`); the node's name when left out |
//! | `socket` | text | shorthand for `marker = socket`; the value is the socket type (`road`) |
//! | `to` | text, or the object (*Data-Block*) | for a `navlink`: the node where it ends |
//! | `collider` | `box`, `sphere`, `capsule`, `cylinder`, `cone`, `auto` | a solid [`Collider`](shards_motion::Collider); `auto` is a box fitted to the mesh |
//! | `size` | `[x, y, z]` or one number | box size (full extents, meters); the mesh bounds when left out |
//! | `radius`, `height` | numbers | sphere, capsule, cylinder, cone; the mesh bounds when left out |
//! | `offset` | `[x, y, z]` | the collider's center in the node's space; the middle of the mesh bounds when left out |
//! | `layers` | `world`, `character`, `volume`, `use`, `game0`..`game15`, ... (a name or a list) | what the collider is; solids default to `world`, volumes to `volume` |
//! | `collides_with` | the same names | what it reacts to (solids: everything, volumes: characters) |
//! | `volume` | `water`, `updraft`, `ladder`, `trigger` | a sensor [`Volume`](shards_motion::Volume), shaped by `collider` (default `auto`) |
//! | `accel`, `max_speed` | numbers | an updraft's pull (m/s²) and top speed (m/s) |
//! | `face` | `[x, y, z]` | a ladder's open side: the way out of the ladder toward the side it is climbed from (away from the wall it hangs on), in the node's space (+Y up, as the file has it: Blender's `(x, y, z)` is `(x, z, -y)`); it must have a level part |
//! | `prefab` | text | spawn the content prefab with that id at the node; component properties patch it |
//! | `hold_s`, `use_range`, `prompt` | seconds (zero or more), metres (above zero, at most 100), text | for an `interactable` (F5): the seconds the button is held for a use (0 is a press), how far from the eye it can be used (the body's own reach when left out), and what a HUD shows (a Fluent `@id` or text; the name when left out) |
//! | `mover` | text | the node becomes a [`Mover`](shards_motion::Mover) going through the keys of that `mover` definition (F6); the nodes below it move with it as its [`MoverPart`](shards_motion::MoverPart)s, and its picture follows it; movers do not nest |
//! | `light` | text | the node shines by that `light` definition ([`ContentLight`](shards_light::ContentLight), F13): a Blender lamp, whose own glTF light the picture drops for it, or an empty; a spot points along the node's -Z |
//! | `grading` | text | on a `marker` = `trigger` node: the picture blends toward that `grading` definition ([`GradingVolume`](shards_motion::camera::GradingVolume)) while the local player is inside |
//!
//! Every other key is a **component**: its short name (`Health`) or full type path
//! (`my_game::combat::Health`), and its value as JSON in the shape `inspect` and the `get`
//! tool show:
//!
//! ```text
//! Health  = {"current": 40.0, "max": 40.0}
//! Faction = "bandits"
//! Loot    = {"table": "chest_a", "rolls": 2, "tags": ["gold", "rare"]}
//! Locked  = true                              (a component with no fields)
//! ```
//!
//! Blender's panel only makes text, numbers and lists, so a JSON object can be typed as
//! *text* (`{"current": 40.0, "max": 40.0}`): a text value that holds a JSON object or list
//! is read as that. A component must be registered for reflection
//! (`#[derive(Reflect)]`, `#[reflect(Component)]`), exactly as for prefabs. A node with a
//! `prefab` treats its component properties as overrides of the prefab's (merged field by
//! field: `Health = {"current": 10}` keeps the prefab's `max`).
//!
//! A node that has none of these (a group, a plain mesh) spawns nothing: a mesh without a
//! `collider` is visual only. Keys starting with `_` are ignored (Blender's own).
//!
//! ## The engine's markers
//!
//! | `marker` | entity gets | the AI sees |
//! |---|---|---|
//! | `spawn` | [`LevelMarker`] | `SpawnPoint "start"` |
//! | `socket` | [`LevelMarker`] named by the socket type | no (plumbing) |
//! | `navlink` | [`LevelMarker`] with `to` set to the end's world position | no |
//! | `trigger` | [`LevelMarker`], a trigger [`Volume`](shards_motion::Volume) and its collider | `Trigger "goal"` |
//! | `interactable` | [`LevelMarker`], [`Usable`](shards_motion::Usable) and a use area (F5): the node's collider on the `USE` layer, solid too when the node has a `collider` of its own | `Interactable "door"` |
//! | `camp` | [`LevelMarker`] | `Camp "bandits"` |
//! | `landmark` | [`LevelMarker`], `AiLandmark`, `AiKind` named by the marker | on the LANDMARKS line |
//! | `custom:<name>` | [`LevelMarker`] | no (add an `AiKind` component if it should) |
//!
//! A game finds its markers with an ordinary query: `Query<(&LevelMarker, &Transform)>`.
//!
//! ## Reading and spawning
//!
//! - [`LevelAsset`]: the file as data: nodes in file order with composed transforms, names,
//!   properties and mesh bounds. It loads through the asset server
//!   (`server.load::<LevelAsset>("levels/arena.gltf")`; [`LevelLoader`] claims no file
//!   extension, so Bevy's own glTF loader keeps `.gltf` and `.glb`), or directly:
//!   [`LevelAsset::read`], [`LevelAsset::from_slice`].
//! - [`spawn_level`] / [`spawn_level_as`] / [`spawn_level_by_id`]: entities for the gameplay
//!   nodes, in file node order, **top-level with world-space transforms** (physics reads
//!   `Transform`; transforms are not propagated inside ticks). Node scale goes into collider
//!   sizes; entity scale is 1. Nothing is spawned when any node has a problem. Every entity
//!   has [`FromLevel`] (level, instance, node path). [`despawn_level`] and [`reload_level`]
//!   work on an instance.
//! - Content: kind `level` ([`LevelDef`]): `file`, `required_markers`, `at`. The loading gate
//!   holds the simulation until the files are in ([`LevelLibrary`]).
//! - [`check_level`]: everything wrong with a file, as `node `Props/Crate.003`: unknown
//!   component `Helth` (did you mean `Health`?)`. `content check` runs it for every level.
//! - Tools: `level_info` (levels, markers, checks), `spawn_level` (`id`, `x`, `y`, `z`),
//!   `level_reload`, `level_despawn` (`id` or `instance`: a placed level taken away).
//!
//! ## Many levels (tiles, regions)
//!
//! A game that places level files the content does not list (the tiles of a generated
//! region) calls [`request_level`] for each path, holds the simulation with a loading-gate
//! check on [`LevelLibrary::status`], reads sockets and other markers from
//! [`LevelAsset::markers`] without spawning anything, and spawns with [`spawn_level_file`]
//! at the tile's transform (composed with every node's). [`despawn_level`] removes a tile by
//! its instance number.
//!
//! The tiles of `shards_world` regions (`geometry: Level(path: "tiles/ruin.gltf")`) are
//! exactly this; this crate does not depend on `shards_world` (nor the other way round), so
//! the umbrella crate joins them: `shards::TileLevelsPlugin`, part of `EnginePlugins`, requests
//! the files of all tiles while the game loads and spawns them per tile, with the region's
//! tags and a `shards_world::Marker` on every marker node.
//!
//! ## Determinism
//!
//! Spawning is a pure function of the file and the placement, in a fixed order, so `AiId`s
//! and `ColliderId`s are the same in every run. `spawn_level` and `level_reload` are
//! actions, so replays record them. The replay check `level` hashes where each level was
//! placed and the digest of its file: a recording made against another version of a file
//! diverges at its first check (record the reference again after editing a level). The
//! check adds nothing while no level stands, so a game without levels has the replay hashes
//! it had before it had this module, and the hash never remembers levels that came and went.
//! Instance numbers ([`LevelCounter`]) are snapshot state. When a
//! file changes on disk (hot reload) the asset server reloads it and the instances are
//! replaced through `level_reload`, which is journaled and recorded like any action.
//!
//! ## Visuals
//!
//! With the `render` feature each spawn also loads the level's scene of the same file
//! ([`LevelAsset::scene`], the file's default scene) through Bevy's glTF loader under one
//! root entity ([`LevelVisual`]), placed like the level. The root has no gameplay components;
//! gameplay entities are the same with and without it.

mod asset;
mod def;
mod library;
mod marker;
mod props;
mod render;
mod spawn;
mod placed;
mod tools;

use ai_core::AiAppExt;
use bevy::prelude::*;
use shards_content::{ContentAppExt, ContentPlugin};
use shards_core::EngineAppExt;

pub use asset::{Bounds, LevelAsset, LevelLoadError, LevelLoader, LevelNode, MarkerInfo};
pub use def::LevelDef;
pub use library::{held_change, level_asset, request_level, LevelLibrary};
pub use marker::{LevelMarker, MarkerKind, RequiredMarker};
pub use placed::{PlacedLevel, PlacedLevels};
pub use props::{check_level, LevelProblem, PROPERTIES};
pub use render::LevelVisual;
#[cfg(feature = "render")]
pub use render::MoverPicture;
pub use spawn::{
    despawn_level, reload_level, spawn_level, spawn_level_as, FromLevel, LevelCounter, LevelInstance, LevelInstances,
    SpawnedLevel,
};
pub use tools::{spawn_level_by_id, spawn_level_file, LevelDespawnArgs, LevelInfoArgs, LevelReloadArgs, SpawnLevelArgs};

/// Version of the engine crates (they share one).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The types a game module uses most.
pub mod prelude {
    pub use crate::{
        spawn_level, spawn_level_by_id, FromLevel, LevelAsset, LevelDef, LevelMarker, LevelPlugin, MarkerKind,
    };
}

/// Level files as content: the `level` kind, the asset loader, spawning, the tools and
/// the replay check. Needs the asset plugin and `ContentPlugin` (`shards::EnginePlugins`
/// adds all of them); the physics types come with `MotionPlugin`.
pub struct LevelPlugin;

impl Plugin for LevelPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<ContentPlugin>(),
            "LevelPlugin needs ContentPlugin added first (shards::EnginePlugins adds both)"
        );
        app.init_asset::<LevelAsset>()
            .register_asset_loader(LevelLoader)
            .register_type::<LevelMarker>()
            .register_type::<MarkerKind>()
            .register_type::<FromLevel>()
            .add_content::<LevelDef>()
            .init_resource::<LevelLibrary>()
            .init_resource::<LevelInstances>()
            // The levels placed by id are (`PlacedLevels`): the snapshot's go back where it
            // loads, around the region its prepare builds again (it may have taken their numbers
            // here, they may have taken its). Then their colliders are checked as static
            // colliders, and every level that stands, and its file, with this.
            .add_snapshot_resource::<PlacedLevels>()
            .net_ignore_resource::<PlacedLevels>()
            // Right after content: before the region, whichever module was added first.
            .add_snapshot_prepare_after("levels", "content", &["levels"], placed::take_away)
            .add_snapshot_prepare_after("placed levels", "region", &["levels"], placed::put_in)
            .add_snapshot_check(
                "levels",
                "other levels stand, or their files differ, than when the snapshot was taken (a level the game \
                 spawned itself, not by id, is not in the snapshot): put those files back, or take the snapshot again",
                spawn::level_hash,
            )
            // The numbers are: a level placed after a load gets the original session's.
            // Each machine of a networked game numbers its own (like collider ids).
            .init_resource::<LevelCounter>()
            .add_snapshot_resource::<LevelCounter>()
            .net_ignore_resource::<LevelCounter>()
            .add_optional_replay_check("level", spawn::level_check)
            .require_loaded(library::LEVELS_GATE, library::level_status)
            .add_playback_hook(library::ask_for_levels_ahead)
            .add_engine_module("level", VERSION)
            .register_ai_describer("marker", tools::describe_marker)
            .add_systems(
                Update,
                (library::request_levels, library::collect_changed, library::apply_changed).chain(),
            );
        #[cfg(feature = "render")]
        render::plugin(app);
        tools::register(app.world_mut());
    }
}
