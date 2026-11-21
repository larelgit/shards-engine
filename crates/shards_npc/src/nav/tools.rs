//! Navigation tools: `nav_bake` (dev: bake files), `nav_overlay` (dev: draw the navmesh),
//! `nav_info` (the navmesh of the region); and [`bake_files`], what `--bake-nav` runs.

use std::fmt;
use std::sync::Arc;

use ai_core::prelude::*;
use ai_core::tools::{register_tool, ToolKind};
use bevy::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use shards_content::db::closest;
use shards_content::{asset_root, ContentDb};

use super::bake::bake;
use super::region::{refresh, NavBakes, NavRegion};
use super::settings::{bake_targets, NavSettings};

pub(crate) fn register(world: &mut World) {
    world.init_resource::<NavOverlay>();
    register_tool(world, "nav_bake", Scope::Dev, ToolKind::Control, nav_bake);
    register_tool(world, "nav_overlay", Scope::Dev, ToolKind::Query, nav_overlay);
    register_tool(world, "nav_info", Scope::Dev, ToolKind::Query, nav_info);
}

/// What [`bake_files`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BakeReport {
    /// The folder, relative to the assets: `nav`.
    pub dir: String,
    /// The body classes baked for: `agent`, then the others.
    pub bodies: Vec<String>,
    /// `(id, file, summary)` of every file written, the file inside the folder
    /// (`large/grass.navtile.ron` for the class `large`).
    pub written: Vec<(String, String, String)>,
    /// `(id, why)` of the tiles that could not be baked (`grass (body large)` for a class).
    pub failed: Vec<(String, String)>,
}

impl BakeReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }
}

impl fmt::Display for BakeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.written.len();
        if self.bodies.len() > 1 {
            let files = if n == 1 { "file" } else { "files" };
            write!(f, "baked {n} {files} for {} body classes ({}) into {}/", self.bodies.len(), self.bodies.join(", "), self.dir)?;
        } else {
            write!(f, "baked {n} {} into {}/", if n == 1 { "tile" } else { "tiles" }, self.dir)?;
        }
        for (id, file, summary) in &self.written {
            write!(f, "\n  {id}: {file} ({summary})")?;
        }
        for (id, why) in &self.failed {
            write!(f, "\n  {id}: FAILED: {why}")?;
        }
        Ok(())
    }
}

/// Bakes tiles and rooms into their bake files under the asset folder, for every body class
/// of the nav settings: `only` names ids (comma separated), `None` or `all` every tile a
/// region uses and every room a dungeon uses, each at the first tile size its regions have.
/// An unknown id is an error that names the known ones.
pub fn bake_files(world: &mut World, only: Option<&str>) -> Result<BakeReport, String> {
    let db = world.get_resource::<ContentDb>().ok_or("content is not loaded")?;
    if !db.is_loaded() {
        return Err("content is still loading; wait with `shards wait`".into());
    }
    let classes = NavSettings::all(db);
    let targets = bake_targets(db);
    let wanted: Option<Vec<String>> = only.filter(|o| o.trim() != "all").map(|o| o.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect());
    if let Some(ids) = &wanted {
        for id in ids {
            if !targets.iter().any(|t| &t.id == id) {
                let known = db.ids::<shards_world::TileDef>().any(|t| t == id) || db.ids::<shards_world::RoomDef>().any(|t| t == id);
                if known {
                    return Err(format!("`{id}` is in no region (or dungeon): there is no tile size to bake it at; add it to one"));
                }
                let hint = closest(id, targets.iter().map(|t| t.id.as_str())).map(|c| format!(" (did you mean `{c}`?)")).unwrap_or_default();
                return Err(format!("no tile or room `{id}` to bake{hint}; `nav_bake` without `tile` bakes them all"));
            }
        }
    }
    let root = asset_root(world);
    let mut report = BakeReport { dir: classes[0].dir.clone(), bodies: classes.iter().map(|c| c.body.clone()).collect(), ..default() };
    for target in targets.into_iter().filter(|t| wanted.as_ref().is_none_or(|w| w.contains(&t.id))) {
        let size = target.sizes[0];
        for settings in &classes {
            match bake(world, Some(&root), target.kind, &target.id, size, settings) {
                Ok(tile) => {
                    let path = settings.file(&root, target.kind, &target.id);
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
                    }
                    std::fs::write(&path, tile.to_file()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    let summary = tile.summary();
                    let hash = tile.hash.clone();
                    world.get_resource_or_init::<NavBakes>().tiles.insert(hash, Arc::new(tile));
                    report.written.push((target.id.clone(), settings.in_dir(target.kind, &target.id), summary));
                }
                Err(why) if settings.is_default() => report.failed.push((target.id.clone(), why)),
                Err(why) => report.failed.push((format!("{} (body {})", target.id, settings.body), why)),
            }
        }
    }
    Ok(report)
}

/// Bakes every tile (or room) the region in the world uses, once per id and body class, as
/// `--bake-nav` would, and writes nothing: how many bakes there were and how long they took
/// (the performance autotest's `nav_bake_s`).
pub fn bake_region(world: &World) -> Result<(usize, std::time::Duration), String> {
    let region = world.get_resource::<shards_world::ActiveRegion>().ok_or("no region is loaded")?;
    let db = world.get_resource::<ContentDb>().ok_or("content is not loaded")?;
    let manifest = &region.manifest;
    if manifest.layout == shards_world::manifest::Layout::Volume {
        return Err(format!(
            "{} is a voxel volume: it has no tiles to bake, and its navigation is baked in memory when it loads, not into files (`nav_info` says how it was baked)",
            manifest.title()
        ));
    }
    let kind = if manifest.layout == shards_world::manifest::Layout::Rooms { super::tile::TileKind::Room } else { super::tile::TileKind::Tile };
    let mut ids: Vec<&str> = manifest.tiles.iter().map(|t| t.tile.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let root = asset_root(world);
    let classes = NavSettings::all(db);
    let started = std::time::Instant::now();
    for id in &ids {
        for settings in &classes {
            bake(world, Some(&root), kind, id, manifest.tile_size, settings).map_err(|why| format!("{id} (body {}): {why}", settings.body))?;
        }
    }
    Ok((ids.len() * classes.len(), started.elapsed()))
}

/// Bake tiles into their navigation bake files (`<assets>/nav/<tile>.navtile.ron`, and
/// `<assets>/nav/<class>/<tile>.navtile.ron` for every other body class), as `--bake-nav`
/// does: one tile or room, or every tile a region uses. A region loaded after reads them;
/// the one in the world keeps its navmesh.
#[derive(Deserialize, JsonSchema, Default)]
pub struct NavBakeArgs {
    /// A tile or room id, or several separated by commas. Default: all.
    pub tile: Option<String>,
}

fn nav_bake(In(call): In<Call<NavBakeArgs>>, world: &mut World) -> ToolResult {
    let report = bake_files(world, call.args.tile.as_deref())?;
    let mut text = report.to_string();
    // The files are the tiles' and rooms'; a voxel region in the world is not made of them.
    if let Some(region) = world.get_resource::<shards_world::ActiveRegion>().filter(|_| super::region_is_volume(world)) {
        text.push_str(&format!(
            "\nthe region in the world, {}, is a voxel volume: these files are not its navigation (it is baked in memory when it loads; see `nav_info`)",
            region.manifest.title()
        ));
    }
    if report.ok() { Ok(text) } else { Err(text) }
}

/// Whether the navigation overlay is drawn.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct NavOverlay {
    pub enabled: bool,
}

/// Draw the navmesh in the game window: walkable cells (green, water blue, carved red),
/// links across tile edges, navlinks, the routes NPCs follow; and the tile bounds and
/// sockets of the world overlay.
#[derive(Deserialize, JsonSchema)]
pub struct NavOverlayArgs {
    /// Default true.
    pub on: Option<bool>,
}

fn nav_overlay(In(call): In<Call<NavOverlayArgs>>, world: &mut World) -> ToolResult {
    let on = call.args.on.unwrap_or(true);
    world.resource_mut::<NavOverlay>().enabled = on;
    #[cfg(feature = "render")]
    if let Some(mut tiles) = world.get_resource_mut::<shards_world::TileOverlay>() {
        tiles.enabled = on;
    }
    Ok(format!("nav overlay {}", if on { "on" } else { "off" }))
}

/// The navmesh of the region in the world (or, without a region, of the levels that ask for one,
/// `levels`): its tiles, nodes, links across tile edges,
/// navlinks, carved cells, and which tiles were baked while loading; and its body classes
/// (`bodies`: each class's body and navmesh). Read-only.
fn nav_info(In(_): In<Call<NoArgs>>, world: &mut World) -> ToolResult<Value> {
    refresh(world);
    let unplanned = world.get_resource::<NavRegion>().is_none() && super::region_is_volume(world) && !super::simulates(world);
    if let Some(region) = world.get_resource::<shards_world::ActiveRegion>().filter(|_| unplanned) {
        return Ok(json!({
            "region": region.manifest.title(),
            "navmesh": "none",
            "bake": super::VOXEL_NOT_SIMULATED,
            "why": "NPCs are the host's: this machine receives their positions and plans no routes",
        }));
    }
    let Some(nav) = world.get_resource::<NavRegion>() else {
        return Err("no navmesh: no region is loaded and no level placed asks for one (NPCs and `move_to path=true` plan on the local grid); \
                    load a region with `regen`, or give a level `nav: true` in its `level` definition"
            .into());
    };
    let links: Vec<Value> = nav
        .navlinks
        .iter()
        .map(|l| {
            let mut link = json!({
                "name": l.name,
                "from": ai_core::spatial::fmt_pos(l.from),
                "to": ai_core::spatial::fmt_pos(l.to),
                "used": l.nodes.is_some(),
            });
            if let Some(why) = &l.unused {
                link["why"] = json!(why);
            }
            link
        })
        .collect();
    // Meters to the centimeter, printed as written (`1.2`, not `1.2000000476837158`).
    let cm = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
    let timed = world.get_resource::<ai_core::perf::AiPerf>().is_some_and(|p| p.report);
    let r = &nav.report;
    let bodies: Vec<Value> = nav
        .classes()
        .map(|c| {
            let mut body = json!({
                "body": c.body,
                "radius": cm(c.agent_radius),
                "height": cm(c.agent_height),
                "step_height": cm(c.agent.step_height),
                "summary": c.summary(),
                "baked_while_loading": c.report.missing.iter().chain(&c.report.stale).collect::<Vec<_>>(),
                "failed": c.report.failed.iter().map(|(t, why)| format!("{t}: {why}")).collect::<Vec<_>>(),
            });
            if let Some(bake) = c.voxel.as_ref().or(c.levels.as_ref().map(|l| &l.bake)) {
                body["bake"] = json!(bake.describe(timed));
            }
            body
        })
        .collect();
    let mut info = json!({
        "region": nav.title,
        "summary": nav.summary(),
        "cell": nav.cell,
        "agent": {"radius": nav.agent_radius, "height": nav.agent_height},
        "from_files": r.from_files,
        "baked_while_loading": r.missing.iter().chain(&r.stale).collect::<Vec<_>>(),
        "failed": r.failed.iter().map(|(t, why)| format!("{t}: {why}")).collect::<Vec<_>>(),
        "closed_edges": r.closed_edges,
        "carved_cells": r.carved,
        "navlinks": links,
        "bodies": bodies,
    });
    if let Some(bake) = nav.voxel.as_ref().or(nav.levels.as_ref().map(|l| &l.bake)) {
        // `in memory, 16 chunks, baked in 123 ms`: the loaded machine's time, not state, so shown
        // only where timing is reported (as `voxel_info` does).
        info["bake"] = json!(bake.describe(timed));
    }
    if let Some(levels) = &nav.levels {
        info["levels"] = json!(levels.levels);
    }
    Ok(info)
}
