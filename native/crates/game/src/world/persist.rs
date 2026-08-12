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

use serde::{Deserialize, Serialize};

use super::*;

/// On-disk format version. Bump when a change can't be absorbed by
/// `#[serde(default)]` alone; `load_level` can then migrate older files.
///
/// v2 (2026-07-31): added authored `entities` (the ECS prop layer). The new field
/// is `#[serde(default)]`, so v1 files still load — an old file simply has no
/// entities. Bumped so a v2 writer is distinguishable in the version tag.
///
/// v3 (2026-07-31): added level lighting — point lights ride the existing
/// `entities` collection (a light entity is `Transform` + `PointLight`, no schema
/// change), plus a new `#[serde(default)] ambient` global. Both defaults keep v1/v2
/// files loading unchanged (no lights, default ambient).
const LEVEL_FORMAT_VERSION: u32 = 3;

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
    /// Authored ECS entities (props). `#[serde(default)]` keeps v1 files loading
    /// with an empty set. See [`crate::ecs`].
    #[serde(default)]
    entities: Vec<crate::ecs::EntityData>,
    /// Level-wide ambient fill (colour + strength). `#[serde(default)]` keeps v1/v2
    /// files loading with the neutral default. See [`crate::ecs::AmbientSettings`].
    #[serde(default)]
    ambient: crate::ecs::AmbientSettings,
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
            entities: self.ecs.save_authored(),
            ambient: self.ambient,
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

        // Flatten the file's authored brushes/stairs; the on-disk region grouping
        // is ignored — `rebuild_from_flat` re-partitions into connected regions
        // (with fresh stable ids) and rebuilds `brush_to_region`. This also means a
        // level authored under the old single-region system re-clusters correctly.
        let mut max_brush_id = 1u32;
        let mut all_brushes: Vec<Brush> = Vec::new();
        let mut all_stairs: Vec<StairDesc> = Vec::new();
        for rd in file.regions {
            for b in &rd.brushes {
                max_brush_id = max_brush_id.max(b.id);
            }
            all_brushes.extend(rd.brushes);
            all_stairs.extend(rd.stairs);
        }
        self.platforms = file.platforms;
        self.stair_runs = file.stair_runs;
        self.spawn_point = Vec3::from(file.spawn_point);
        self.ambient = file.ambient;

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

        // Re-partition + re-bake, clearing regions from the previously-loaded level.
        let mut meshes = self.rebuild_from_flat(all_brushes, all_stairs, old_ids);
        // Always refresh the combined structures mesh + collider (an empty one
        // when the level has no platforms/stair-runs, which clears any leftover).
        meshes.push(self.rebuild_structures());

        // Restore the authored ECS entities (props). A load fully replaces the
        // authored set; derived per-entity runtime state (colliders, nav overlays,
        // meshes) is re-established at HUNT bake, mirroring how geometry is derived.
        self.ecs.load_authored(&file.entities);

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
    /// Shared by [`load_level`](Self::load_level) and the undo/redo restore path
    /// (see `world::history`).
    pub(crate) fn reset_edit_state_for_load(&mut self) {
        self.selected = None;
        self.active = None;
        self.pending_stair = None;
        self.sel_bounds = None;
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.prop_tool = None;
        self.prop_preview_pos = None;
        self.selected_prop = None;
        self.prop_gizmo_drag = None;
        self.light_tool = false;
        self.light_preview_pos = None;
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

        // All authored brush ids, across every region, as a set (load re-clusters
        // into connected regions, so per-region order/grouping is not preserved —
        // the round-trip guarantee is the *set* of authored brushes).
        let mut before_brushes: Vec<u32> =
            world.regions.iter().flat_map(|r| r.brushes.iter().map(|b| b.id)).collect();
        before_brushes.sort_unstable();

        // Load into a fresh world.
        let mut loaded = World::new();
        let meshes = loaded.load_level(&path).expect("load");
        assert!(!meshes.is_empty(), "load should return meshes to upload");

        // Brush 1 (x∈[0,24]) and brush 2 (x∈[30,42]) are disjoint, so clustering
        // splits them into two independent regions.
        assert_eq!(loaded.regions.len(), 2, "disjoint brushes → two regions");
        let mut after_brushes: Vec<u32> =
            loaded.regions.iter().flat_map(|r| r.brushes.iter().map(|b| b.id)).collect();
        after_brushes.sort_unstable();
        assert_eq!(before_brushes, after_brushes, "brush ids must round-trip");
        // Every brush is mapped to exactly one region.
        assert_eq!(loaded.brush_to_region.len(), after_brushes.len());
        assert_eq!(loaded.platforms.len(), 1);
        assert_eq!(loaded.platforms[0].id, 1);
        assert!(loaded.platforms[0].grounded && loaded.platforms[0].railings);
        assert_eq!(loaded.next_brush_id, 3, "brush allocator preserved");
        assert_eq!(loaded.next_platform_id, 2, "platform allocator preserved");

        let _ = std::fs::remove_file(&path);
    }

    /// A placed prop (an authored ECS entity: Transform + Renderable) survives the
    /// full save → load round-trip through the level file, alongside the geometry.
    #[test]
    fn save_load_round_trips_placed_props() {
        use crate::ecs::{ComponentData, EntityData, MeshId};

        let mut world = World::new();
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: [2.0, 0.0, 3.0],
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::Renderable { mesh: MeshId::WoodenCrate },
            ],
        });

        let path = std::env::temp_dir().join("bah_props_roundtrip.json");
        world.save_level(&path).expect("save");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");

        let props = loaded.ecs.save_authored();
        assert_eq!(props.len(), 1, "the placed prop must round-trip");
        let has_crate = props[0].components.iter().any(|c| {
            matches!(c, ComponentData::Renderable { mesh: MeshId::WoodenCrate })
        });
        assert!(has_crate, "the prop keeps its mesh id");
        let _ = std::fs::remove_file(&path);
    }

    /// A placed point light (an authored entity: Transform + PointLight, no
    /// Renderable) survives the full save → load round-trip, and the level-wide
    /// ambient fill round-trips alongside it.
    #[test]
    fn save_load_round_trips_lights_and_ambient() {
        use crate::ecs::{AmbientSettings, ComponentData, EntityData, PointLight};

        let mut world = World::new();
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: [1.0, 2.5, -3.0],
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::PointLight { color: [1.0, 0.4, 0.1], intensity: 3.0, range: 12.0 },
            ],
        });
        world.set_ambient(AmbientSettings { color: [0.2, 0.3, 0.5], level: 0.4 });

        let path = std::env::temp_dir().join("bah_lights_roundtrip.json");
        world.save_level(&path).expect("save");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");

        // The light entity round-trips with its params intact…
        let lights = loaded.light_draws();
        assert_eq!(lights.len(), 1, "the placed light must round-trip");
        let (pos, color, intensity, range, _shadow) = lights[0];
        assert_eq!(pos.to_array(), [1.0, 2.5, -3.0]);
        assert_eq!(color, [1.0, 0.4, 0.1]);
        assert_eq!(intensity, 3.0);
        assert_eq!(range, 12.0);
        // …and it is NOT a prop (no Renderable), so it stays out of the prop draw list.
        assert!(loaded.prop_draws(1.0).is_empty(), "a light is not drawn as a prop");
        // Ambient round-trips.
        let amb = loaded.ambient();
        assert_eq!(amb.color, [0.2, 0.3, 0.5]);
        assert_eq!(amb.level, 0.4);
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
