//! `shards_npc` integration tests, one binary: headless arenas with test content, NPCs
//! driven by their behaviours, players placed (or played through tools) by the tests.

mod common;
mod audit;
mod avoid;
mod bodies;
mod director;
mod duel;
mod level_nav;
mod live;
mod fsm;
mod nav;
mod navmesh;
#[cfg(feature = "render")]
mod overlay;
mod perception;
mod ranged;
mod replay;
mod rules;
mod schedule;
mod sequence;
mod snapshot;
mod tools;
mod voxel_nav;
mod warden;
