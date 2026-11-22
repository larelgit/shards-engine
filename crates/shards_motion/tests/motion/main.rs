//! `shards_motion` integration tests, one binary: headless worlds driven tick by tick.

mod air_move;
mod camera_content;
mod common;
mod control_lock;
mod controller;
mod controller_parity;
mod course;
mod crouch;
mod drive;
mod edge_cases;
mod engine;
mod ground;
mod input;
mod ladder;
mod launch;
mod modifiers;
mod modifiers_parity;
mod movers;
mod orders;
mod perception;
mod physics;
mod replays;
mod seams;
mod tools;
mod voxel;
#[cfg(feature = "render")]
mod camera;
#[cfg(feature = "render")]
mod camera_big;
