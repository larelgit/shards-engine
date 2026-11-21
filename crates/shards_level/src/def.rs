//! The `level` content kind: which level files a game has and what they must contain.
//!
//! ```ron
//! // content/levels/arena.level.ron
//! {
//!     "arena": (
//!         file: "levels/arena.gltf",
//!         required_markers: [(kind: "spawn", name: "start"), (kind: "landmark", min: 1)],
//!         at: (0.0, 0.0, 0.0),
//!         nav: true,
//!     ),
//! }
//! ```
//!
//! `content check` (and `--check-content`, and every content reload) reads the glTF file,
//! so a level that cannot be spawned fails the check: the file is missing or broken, a
//! node names a component that does not exist or does not take its value, a prefab is
//! unknown, a navlink ends nowhere, or a required marker is absent. Each problem names
//! the content file, the level, the node's path in the file and the property:
//!
//! ```text
//! content/levels/arena.level.ron: level `arena`: node `Props/Crate.003`: unknown component `Helth` (did you mean `Health`?)
//! content/levels/arena.level.ron: level `arena`: required marker missing: `spawn` named `start` in `levels/arena.gltf` (found 0, need 1); add the custom property marker = "spawn" and name = "start" to an object in Blender
//! ```

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use shards_content::{ContentDef, Refs};

use crate::asset::{check_relative, LevelAsset};
use crate::marker::RequiredMarker;
use crate::props::check_level;

/// A level file and what it must contain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelDef {
    /// The `.gltf` or `.glb` file, relative to the asset folder: `levels/arena.gltf`.
    pub file: String,
    /// Markers the file must have; checking fails without them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_markers: Vec<RequiredMarker>,
    /// Where the `spawn_level` tool and [`spawn_level_by_id`](crate::spawn_level_by_id) put the
    /// level unless told otherwise.
    #[serde(default)]
    pub at: Vec3,
    /// Whether NPCs get a navmesh of the level: where no region is loaded, the levels placed by
    /// id that say so are baked in memory as they are placed (their static solids; doors and
    /// what the game spawns carve it), and NPCs route on it: up stairs, onto walkways, down
    /// again (the NPC module's `nav`, "Hand-made levels"). Without it NPCs in the level plan on
    /// the local grid, in the plane.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nav: bool,
}

impl ContentDef for LevelDef {
    const KIND: &'static str = "level";

    fn validate(&self, _: &str, refs: &mut Refs) {
        if let Err(why) = check_relative(&self.file) {
            return refs.error(why);
        }
        if !self.at.is_finite() {
            refs.error("`at` must be finite numbers");
        }
        for need in &self.required_markers {
            if need.min == 0 {
                refs.error(format!("required marker {} has min 0, which requires nothing; use 1 or more", need.describe()));
            }
        }
        // Without an asset folder or a type registry (a bare `ContentDb::build`) only the
        // shape of the definition can be checked.
        let (Some(root), Some(registry)) = (refs.asset_root(), refs.type_registry()) else { return };
        let level = match LevelAsset::read(root, &self.file) {
            Ok(level) => level,
            Err(why) => return refs.error(why),
        };
        for problem in check_level(&level, registry, Some(refs.db()), &self.required_markers) {
            refs.error(problem.to_string());
        }
    }
}
