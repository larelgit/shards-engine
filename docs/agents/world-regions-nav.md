# World, regions, levels, generation and navmesh

Read this when you touch regions, tiles, levels, the generator, the navmesh or the state they keep in snapshots. Part of the [AGENTS.md](../../AGENTS.md) index.

- **World, regions and levels (phase B).** The things the gameplay takes from tiles, rooms and levels (colliders, markers, prefabs,
  points of interest) are **top-level entities with a world `Transform`**: that is how physics reads them and how `spawn_region` and
  `spawn_level` create them. A region is changed with tools (`regen`, `region_load`) or with `change_region`, through
  `EngineState::RegionLoading`, inside one call; it is not spawned or removed by hand. The starting region
  (`start_region`) is built by the engine (`build_start_region`) before `OnEnter(InGame)`, so its `AiId`s come first; the game's own
  entities still spawn in `OnEnter(InGame)`, after it.
- **Generation is a pure function of (seed, generator version, content).** The randomness of the steps is `GenContext::rng()` (`DetRng`),
  never `SimRng`, never the clock and never the order of `HashMap`/`HashSet` (the order comes from indices and `BTreeMap`). If you change the generator's output
  (steps, weights, solver rules), raise the region's `version`: the old seeds will give new worlds, and the saved manifests
  stay as they are. To rebuild the references: `--worldgen-report` on 1000 seeds, `record_reference_replays`, the manifest hash in
  `region.scenario`.
- **Voxel generation.** A voxel region is made by steps registered by name and version (`add_voxel_step("hills", 1, hills)`;
  `shards_voxel`), run in the order the region lists them, with randomness only from `VoxelGen::rng` / `VoxelGen::rng_keyed` (`DetRng`, keyed by
  seed, region version, attempt and the step's place) and `VoxelGen::noise` (keyed by seed, region version, attempt and a salt: the same salt
  is the same field in every step of an attempt), heights from `Noise` (no `powf`, no `sin`). A step is code the content hash cannot
  see: when you change what a step produces, raise its registered version and the version the content lists, so a client with other step
  code fails content validation instead of desyncing. The volume hash (`VoxelVolume::hash`, `world_hash`) is the backstop; its golden
  tests change only on purpose.
- **A voxel region is a region kind, behind the region API.** A `*.voxel_region.ron` region is generated, loaded, saved, snapshot and
  replayed by the same calls as a region of tiles (`regen`, `change_region`, `region_load`, `RegionManifest`, `ActiveRegion`, `Poi`,
  `Marker`); its manifest has `layout: "volume"` and its cells are chunks. Calls into `shards_voxel` (`VoxelGen`, `VoxelVolume`,
  `VoxelPalette`) stay inside generation steps and checks: the rest of the game asks the region API, which answers with the same types on
  both kinds (the test `the_region_api_answers_with_the_same_types_on_tiles_and_on_a_volume`). There is no runtime voxel editing: a built
  volume is read-only until the region is made again. A new region kind registers itself with `add_region_kind` (`RegionKind`), and
  a new `Layout` fails to compile in `every_layout_is_named_where_the_engine_matches_on_it` until every place that matches on it is
  changed (`docs/manual.md`, "Voxel regions (A2c)").
- **Navmesh.** Baked tiles are in `assets/nav/` (`cargo sandbox --bake-nav`, tool `nav_bake`) and are committed; with
  the `nav` definition, `--check-content` checks them (a stale file is an error that names the command to run). If you change what a tile is baked with
  (the ground probe and the step rule in `shards_motion`, the sampling in `shards_npc::nav`), raise `BAKE_VERSION` and bake everything.
  Editing a tile, its level file or a geometry prefab means baking again. Body classes (`bodies` in `*.nav.ron`: `agent` and,
  for example, `large` for a big enemy) are each baked into their own folder (`assets/nav/large/`); editing the `locomotion` of any class,
  a new class, or its `radius`/`height`, also means baking again (`--bake-nav` bakes all classes) and committing the class's files.
  `--check-content` names the missing or stale file of each class (``… for the body `large` (nav/large/…)``).
- **A voxel region's navmesh is baked in memory**, not from files: `shards_npc::nav::bake_volume` bakes each chunk (a square of the grid
  sampled at its world place, `bake_chunk`) on the compute pool, in chunk order, from the field and the water only, keeps them under `ChunkKey`
  (`NavBakes`, and a process cache), and stitches the chunks across every edge (diagonals too). `--bake-nav` and `--check-content` do not
  see it. A change to the ground probe, the step rule or the sampler changes both kinds of bakes: raise `BAKE_VERSION` for the tiles; the
  chunks have no file to go stale, and the test `chunks_stitched_are_the_grid_baked_in_one_piece` (and the sandbox island's) holds the stitching to the
  grid baked in one piece. Only machines that simulate bake (`NetRole`). See `docs/manual.md`, "Voxel regions", "Navigation".
- **The navmesh of hand-made levels is baked in memory too** (`shards_npc::nav::region::levels`): with no region loaded, the levels
  placed by id whose definition says `nav: true` (`LevelDef::nav`) are baked from their own static solids (movers and what has `Health` left out: they carve),
  in chunks of `LEVEL_CHUNK` columns on a world-aligned grid, each from the solids within the body's reach of it, kept under a `ChunkKey`
  whose hash is those solids and the grid's corner, and stitched as a volume's chunks are. `refresh` makes it again when `PlacedLevels`
  or content changes or a baked solid goes or moves (`levels::current`); a level without `nav` builds nothing and changes nothing, so a
  game's replays hold. A change to the probe, the step rule or the sampler changes these bakes as it changes the others; they have no
  file to go stale. Tests: `crates/shards_npc/tests/npc/level_nav.rs`. See `docs/manual.md`, "The navmesh of hand-made levels".
- **World state in the snapshot.** New region, level or navigation state outside entities with `AiId` goes in `add_snapshot_resource`;
  what is built from the saved data (the region from its manifest, the navmesh) goes in `add_snapshot_prepare` / `add_snapshot_rebuild`; what
  the world is built around goes in `add_snapshot_check`. Cover it with a test modelled on `crates/sandbox/tests/sandbox/snapshots.rs`
  and `crates/shards_world/tests/world/regions.rs` (a snapshot taken in a region, loaded into a game that does not have it).
- **Movers in levels (F6).** A node with `mover=<id>` becomes a `Mover` (content kind `mover`) and the nodes below it its `MoverPart`s; the
  step lives in `shards_motion::movers`, the property and the parts in `shards_level` (`props.rs`, `spawn.rs`), the picture in
  `shards_level::render`. A mover's colliders are static at rest and kinematic while it moves, so the navmesh carves a closed door at its
  next refresh and leaves a moving one out; NPCs do not ride lifts (no navlink yet). A scenario that lays a level takes it away again
  (`level_despawn id=…`): the scenarios share one game. The sandbox's test room is `levels/movers.gltf`, written by
  `crates/sandbox/tests/sandbox/movers.rs` (regenerate with `SHARDS_UPDATE_FIXTURES=1`).
