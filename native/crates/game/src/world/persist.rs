//! Level save / load — persist the **authored** geometry to disk and restore it.
//!
//! A level is nothing but a small bag of plain-data structs (the CSG regions,
//! their stairs, the free-standing platforms + stair-runs, and the id
//! allocators). Everything else — meshes, colliders, the nav grid, doors, the
//! hunter wave — is *derived* from that data at bake time and rebuilt on load
//! via the same [`World::rebuild_region`] / [`World::rebuild_structures`] paths
//! the editor already uses. So we never serialize geometry: only the authored
//! source of truth, which keeps files tiny, hand-editable, and robust to engine
//! changes (a new mesh path just re-derives from the same brushes).
//!
//! Format: pretty-printed JSON via serde. A [`LevelFile::version`] tag plus
//! `#[serde(default)]` on later-added fields (see [`Brush`]) means old files
//! keep loading as the schema grows. Files live under `native/levels/` as
//! numbered slots (`slotN.json`); rename/commit them to build a test catalog.

use std::io;
use std::path::{Path, PathBuf};

use engine::render::mesh::{CpuMesh, TexturedMesh};
use serde::{Deserialize, Serialize};

use super::*;

/// On-disk format version. Bump when a change can't be absorbed by
/// `#[serde(default)]` alone; `load_level` can then migrate older files.
const LEVEL_FORMAT_VERSION: u32 = 1;

/// One CSG region's authored data (the shell is derived, so it isn't stored —
/// `refresh_shell` recomputes it on load).
#[derive(Serialize, Deserialize)]
struct RegionData {
    id: u32,
    brushes: Vec<Brush>,
    #[serde(default)]
    stairs: Vec<StairDesc>,
}

/// The complete authored level — everything needed to reconstruct the editable
/// world. Serialized verbatim; deserialized back into the [`World`] on load.
#[derive(Serialize, Deserialize)]
struct LevelFile {
    version: u32,
    /// Enemy ingress marker (WT-world metres). Persisted so the format is ready
    /// for an authorable spawn point even though it's fixed today.
    #[serde(default)]
    spawn_point: [f32; 3],
    regions: Vec<RegionData>,
    #[serde(default)]
    platforms: Vec<Platform>,
    #[serde(default)]
    stair_runs: Vec<StairRun>,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
}

/// The directory levels live in: `native/levels/` (a sibling of `assets/`).
/// Committed files here form the hand-named test-base catalog.
pub fn levels_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../levels")
}

/// Path for a numbered quick-slot (`native/levels/slotN.json`).
pub fn slot_path(slot: u8) -> PathBuf {
    levels_dir().join(format!("slot{slot}.json"))
}

/// Wrap a serde error as an `io::Error` so save/load share one result type.
fn invalid_data(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl World {
    /// Serialize the authored geometry (regions/stairs/platforms/stair-runs +
    /// allocators + spawn point) to `path` as pretty JSON, creating the parent
    /// directory if needed. Derived state (meshes/colliders/nav) is never
    /// written — it's re-derived on load.
    pub fn save_level(&self, path: &Path) -> io::Result<()> {
        let file = LevelFile {
            version: LEVEL_FORMAT_VERSION,
            spawn_point: self.spawn_point.to_array(),
            regions: self
                .regions
                .iter()
                .map(|r| RegionData {
                    id: r.id,
                    brushes: r.brushes.clone(),
                    stairs: r.stairs.clone(),
                })
                .collect(),
            platforms: self.platforms.clone(),
            stair_runs: self.stair_runs.clone(),
            next_brush_id: self.next_brush_id,
            next_platform_id: self.next_platform_id,
            next_run_id: self.next_run_id,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&file).map_err(invalid_data)?;
        std::fs::write(path, json)
    }

    /// Replace the authored geometry with the level at `path` and rebuild every
    /// derived mesh + collider, returning the meshes for the app to upload.
    ///
    /// Returned meshes cover three cases so the GPU/physics never keep stale
    /// geometry from the previously-loaded level:
    ///   * each new region (freshly re-baked, collider set in place),
    ///   * every region id that existed before but not now — returned as an
    ///     *empty* mesh, which the renderer/physics treat as "remove",
    ///   * the combined structures mesh (also empty-clears when there are none).
    ///
    /// BUILD-only: refuses while a hunt is live (the geometry is frozen then).
    pub fn load_level(&mut self, path: &Path) -> io::Result<Vec<RegionMesh>> {
        if self.mode != Mode::Build {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "cannot load a level during HUNT — return to BUILD first",
            ));
        }
        let text = std::fs::read_to_string(path)?;
        let file: LevelFile = serde_json::from_str(&text).map_err(invalid_data)?;
        if file.version > LEVEL_FORMAT_VERSION {
            log::warn!(
                "level {} is format v{} (this build understands up to v{}); loading best-effort",
                path.display(),
                file.version,
                LEVEL_FORMAT_VERSION
            );
        }

        // Region ids present before this load, so we can clear any that vanish.
        let old_ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();

        // Rebuild the authored region list (shell recomputed from the brushes).
        let mut max_brush_id = 1u32;
        let mut regions = Vec::with_capacity(file.regions.len());
        for rd in file.regions {
            for b in &rd.brushes {
                max_brush_id = max_brush_id.max(b.id);
            }
            let mut region = Region::new(rd.id);
            region.brushes = rd.brushes;
            region.stairs = rd.stairs;
            region.refresh_shell();
            regions.push(region);
        }
        self.regions = regions;
        self.platforms = file.platforms;
        self.stair_runs = file.stair_runs;
        self.spawn_point = Vec3::from(file.spawn_point);

        // Restore allocators, but never below one-past the max id actually
        // present — a hand-edited file with a stale counter can't then hand out
        // an id that collides with an existing brush/platform/run.
        let max_plat = self.platforms.iter().map(|p| p.id).max().unwrap_or(0);
        let max_run = self.stair_runs.iter().map(|r| r.id).max().unwrap_or(0);
        self.next_brush_id = file.next_brush_id.max(max_brush_id + 1);
        self.next_platform_id = file.next_platform_id.max(max_plat + 1);
        self.next_run_id = file.next_run_id.max(max_run + 1);

        // Drop any transient editing state — its ids may reference geometry the
        // loaded level doesn't have.
        self.reset_edit_state_for_load();

        // Re-bake every loaded region (sets its collider) + collect its mesh.
        let ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        let new_ids: std::collections::HashSet<u32> = ids.iter().copied().collect();
        let mut meshes: Vec<RegionMesh> = Vec::new();
        for id in ids {
            if let Some(rm) = self.rebuild_region(id) {
                meshes.push(rm);
            }
        }
        // Clear regions that existed before but not in the loaded level: empty
        // mesh removes the renderer entry, empty collider removes the physics one.
        for old in old_ids {
            if !new_ids.contains(&old) {
                self.physics.set_region_collider(old, &CpuMesh::default());
                meshes.push(RegionMesh {
                    id: old,
                    mesh: TexturedMesh::default(),
                });
            }
        }
        // Always refresh the combined structures mesh + collider (an empty one
        // when the level has no platforms/stair-runs, which clears any leftover).
        meshes.push(self.rebuild_structures());

        log::info!(
            "loaded level {} — {} region(s), {} platform(s), {} stair-run(s)",
            path.display(),
            self.regions.len(),
            self.platforms.len(),
            self.stair_runs.len()
        );
        Ok(meshes)
    }

    /// Save to numbered quick-slot `slot`, returning the path written (for the
    /// caller to log).
    pub fn save_slot(&self, slot: u8) -> io::Result<PathBuf> {
        let path = slot_path(slot);
        self.save_level(&path)?;
        Ok(path)
    }

    /// Load numbered quick-slot `slot` (see [`load_level`](Self::load_level)).
    pub fn load_slot(&mut self, slot: u8) -> io::Result<Vec<RegionMesh>> {
        self.load_level(&slot_path(slot))
    }

    /// Clear every transient authoring selection / armed-tool field, so a fresh
    /// load never leaves a selection or gizmo pointing at geometry that's gone.
    fn reset_edit_state_for_load(&mut self) {
        self.selected = None;
        self.active = None;
        self.pending_stair = None;
        self.sel_bounds = None;
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.platform_phase = None;
        self.selected_platform = None;
        self.selected_run = None;
        self.connect_from = None;
        self.connect_to = None;
        self.connect_edge = None;
        self.simple_from = None;
        self.gizmo_drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::geometry::csg_runtime::{Brush, Op};

    /// Save the default world, mutate it, then load the saved file back and
    /// confirm the authored geometry + allocators round-trip exactly.
    #[test]
    fn save_load_round_trips_authored_geometry() {
        let mut world = World::new();
        // Author a second subtract brush + a platform so the file exercises more
        // than the opening room, and bump the allocators the way the tools would.
        world.regions[0]
            .brushes
            .push(Brush::new(2, Op::Subtract, 30.0, 0.0, 0.0, 12.0, 8.0, 12.0));
        world.next_brush_id = 3;
        world.platforms.push(Platform {
            id: 1,
            x: 4.0,
            y: 8.0,
            z: 4.0,
            size_x: 4.0,
            size_z: 4.0,
            thickness: 1.0,
            grounded: true,
            railings: true,
        });
        world.next_platform_id = 2;

        let path = std::env::temp_dir().join("bah_persist_roundtrip.json");
        world.save_level(&path).expect("save");

        let before_brushes: Vec<u32> =
            world.regions[0].brushes.iter().map(|b| b.id).collect();

        // Load into a fresh world.
        let mut loaded = World::new();
        let meshes = loaded.load_level(&path).expect("load");
        assert!(!meshes.is_empty(), "load should return meshes to upload");

        assert_eq!(loaded.regions.len(), 1);
        let after_brushes: Vec<u32> =
            loaded.regions[0].brushes.iter().map(|b| b.id).collect();
        assert_eq!(before_brushes, after_brushes, "brush ids must round-trip");
        assert_eq!(loaded.platforms.len(), 1);
        assert_eq!(loaded.platforms[0].id, 1);
        assert!(loaded.platforms[0].grounded && loaded.platforms[0].railings);
        assert_eq!(loaded.next_brush_id, 3, "brush allocator preserved");
        assert_eq!(loaded.next_platform_id, 2, "platform allocator preserved");

        let _ = std::fs::remove_file(&path);
    }

    /// A hand-edited file with a stale allocator can't hand out an id that
    /// collides with a brush already present.
    #[test]
    fn load_bumps_allocator_past_max_present_id() {
        let mut world = World::new();
        world.regions[0]
            .brushes
            .push(Brush::new(9, Op::Subtract, 30.0, 0.0, 0.0, 8.0, 8.0, 8.0));
        // Deliberately understate the allocator, as a hand-edited file might.
        world.next_brush_id = 2;

        let path = std::env::temp_dir().join("bah_persist_alloc.json");
        world.save_level(&path).expect("save");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(
            loaded.next_brush_id, 10,
            "allocator must clear the max present brush id (9) + 1"
        );
        let _ = std::fs::remove_file(&path);
    }
}
