//! Serialize a [`BuiltLevel`] to the on-disk `levels/slotN.json` format so the
//! real game can load a generated level. Mirrors the private `LevelFile` schema
//! in `world::persist` (same field names → identical JSON); the engine's
//! `Brush`/`Platform`/`StairRun` already derive `Serialize`, so we only need the
//! thin envelope structs here.

use std::io;
use std::path::PathBuf;

use engine::geometry::csg_runtime::{Brush, StairDesc};
use engine::geometry::structures::{Platform, StairRun};
use serde::Serialize;

use super::builder::BuiltLevel;

#[derive(Serialize)]
struct RegionData {
    id: u32,
    brushes: Vec<Brush>,
    stairs: Vec<StairDesc>,
}

#[derive(Serialize)]
struct LevelFile {
    version: u32,
    spawn_point: [f32; 3],
    regions: Vec<RegionData>,
    platforms: Vec<Platform>,
    stair_runs: Vec<StairRun>,
    entities: Vec<crate::ecs::EntityData>,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
}

/// `native/levels/slotN.json` (matches `world::persist::slot_path`).
fn slot_path(slot: u8) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../levels")
        .join(format!("slot{slot}.json"))
}

/// Write `built` as a playable level slot; returns the path written.
pub fn write_slot(built: &BuiltLevel, slot: u8) -> io::Result<PathBuf> {
    let file = LevelFile {
        // v2: carries the authored `entities` list (see `world::persist`).
        version: 2,
        spawn_point: built.spawn.to_array(),
        regions: vec![RegionData {
            id: 0,
            brushes: built.brushes.clone(),
            stairs: built.stairs.clone(),
        }],
        platforms: built.platforms.clone(),
        stair_runs: built.stair_runs.clone(),
        entities: built.entities.clone(),
        next_brush_id: built.next_brush_id,
        next_platform_id: built.next_platform_id,
        next_run_id: built.next_run_id,
    };
    let path = slot_path(slot);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}
