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
///
/// **Still v3 (2026-08-18)** with the arrival of authorable spawn pads: a pad entity is
/// `Transform` + `SpawnPoint` and rides the same `entities` collection, exactly as a
/// point light does, so nothing about the file schema moved. The legacy scalar
/// [`LevelFile::spawn_point`] below is now vestigial — kept written so a level saved by
/// this build still opens in an older one at its old fixed ingress.
///
/// v4 (2026-08-19): texture themes are stored by **name** (`"scheme":
/// "archives_1"`) instead of by index (`"scheme": 2`). Themes now load at runtime
/// from `native/assets/themes.json`, so an index is only a position in whatever
/// that file happens to list — inserting or removing a theme would silently
/// retexture every saved level. No migration pass is needed in either direction
/// of *reading*: `de_scheme` in `csg_runtime` accepts a name or a legacy index
/// (see [`engine::geometry::csg_runtime::Brush::scheme`]), so v1–v3 files still
/// open. Writing is one-way, though — a level saved by this build will not open
/// in a pre-v4 build, hence the bump.
///
/// **Still v4 (2026-08-19)** with per-level theme hotkeys: `theme_hotkeys` is a new
/// `#[serde(default)]` map, so v1-v4 files load with none bound (falling through to
/// the manifest's own keys), and an older build silently ignores the extra field.
/// By this file's own rule that is not a version-worthy change.
const LEVEL_FORMAT_VERSION: u32 = 4;

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
    /// This level's number-key → theme-name bindings. `#[serde(default)]`, so older
    /// files load with none bound and fall through to the manifest's own keys.
    #[serde(default)]
    theme_hotkeys: std::collections::BTreeMap<char, String>,
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
            theme_hotkeys: self.theme_hotkeys.clone(),
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
        let meshes = self.apply_level(file);
        log::info!(
            "loaded level {} — {} region(s), {} platform(s), {} stair-run(s)",
            path.display(),
            self.regions.len(),
            self.platforms.len(),
            self.stair_runs.len()
        );
        Ok(meshes)
    }

    /// Load a level built in-process by the [`levelgen`](crate::levelgen) builder,
    /// without a round trip through `levels/slotN.json`.
    ///
    /// Same path as [`load_level`](Self::load_level) — identical re-partition,
    /// re-bake and entity restore — just fed from memory. Used by the PD simulant
    /// lab, which bakes its arena at boot rather than shipping a slot file that a
    /// fresh clone would have to regenerate.
    pub fn load_built_level(
        &mut self,
        built: &crate::levelgen::builder::BuiltLevel,
    ) -> io::Result<Vec<RegionMesh>> {
        if self.mode != Mode::Build {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "cannot load a level during HUNT — return to BUILD first",
            ));
        }
        let file = LevelFile {
            version: LEVEL_FORMAT_VERSION,
            spawn_point: built.spawn.to_array(),
            regions: vec![RegionData {
                id: 0,
                brushes: built.brushes.clone(),
                stairs: built.stairs.clone(),
            }],
            platforms: built.platforms.clone(),
            stair_runs: built.stair_runs.clone(),
            entities: built.entities.clone(),
            ambient: crate::ecs::AmbientSettings::default(),
            // A generated level starts with no quick keys bound; the digits fall
            // through to the manifest's own defaults until an author binds them.
            theme_hotkeys: std::collections::BTreeMap::new(),
            next_brush_id: built.next_brush_id,
            next_platform_id: built.next_platform_id,
            next_run_id: built.next_run_id,
        };
        let meshes = self.apply_level(file);
        log::info!(
            "loaded in-process level — {} region(s), {} platform(s), {} stair-run(s)",
            self.regions.len(),
            self.platforms.len(),
            self.stair_runs.len()
        );
        Ok(meshes)
    }

    /// Install a parsed [`LevelFile`] into the world: re-partition the brushes into
    /// connected regions, re-bake their meshes/colliders, and restore the authored
    /// entity set. Shared by the on-disk and in-memory load paths.
    fn apply_level(&mut self, file: LevelFile) -> Vec<RegionMesh> {
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
        self.theme_hotkeys = file.theme_hotkeys.clone();

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
        self.migrate_legacy_turrets();

        meshes
    }

    /// Lift sentry guns authored **before** the turret rig up out of the floor.
    ///
    /// A sentry gun used to be an ordinary floor prop: dropped on a floor pick, at
    /// `PROP_SCALE`, anchored by the centre of its base. It is now a ceiling fixture
    /// anchored by its mount point ([`crate::props::ceiling_mounted`]), so those old
    /// placements re-read as "hang a turret from this floor point" — the whole gun
    /// ends up *below* the floor it was authored on, invisible and effectively
    /// unclickable.
    ///
    /// There is no honest way to guess which ceiling the author meant, so this does
    /// the one thing that is clearly right: raise the mount by the turret's own drop,
    /// so the gun occupies the space it used to occupy, just hanging from a point
    /// above it instead of standing on one below it. It lands visible, selectable and
    /// working, where the author put it — and if that is not where they want it, the
    /// gizmo and the delete key now reach it.
    ///
    /// Legacy placements are told apart by their authored scale: the catalog is the
    /// only thing that ever sets it (there is no scale gizmo), so a sentry gun not at
    /// [`crate::turret::RIG_SCALE`] predates the rig.
    pub(crate) fn migrate_legacy_turrets(&mut self) {
        let drop = self
            .prop_bounds
            .get(&crate::ecs::MeshId::SentryGun)
            .map(|(min, max)| max.y - min.y)
            .unwrap_or(0.0);
        let mut moved = 0;
        for (t, r) in self
            .ecs
            .world_mut()
            .query_mut::<(&mut crate::ecs::Transform, &crate::ecs::Renderable)>()
        {
            if r.mesh != crate::ecs::MeshId::SentryGun
                || (t.scale.x - crate::turret::RIG_SCALE).abs() < 1e-4
            {
                continue;
            }
            t.scale = Vec3::splat(crate::turret::RIG_SCALE);
            t.pos.y += drop * crate::turret::RIG_SCALE;
            moved += 1;
        }
        if moved > 0 {
            log::info!(
                "migrated {moved} pre-rig sentry gun(s): re-scaled to the turret rig and \
                 raised {:.2} m so they hang above their authored point instead of below it",
                drop * crate::turret::RIG_SCALE
            );
        }
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
        self.clear_draw_state();
        self.prop_tool = None;
        self.prop_preview_pos = None;
        self.selected_prop = None;
        self.prop_gizmo_drag = None;
        self.light_tool = false;
        self.light_preview_pos = None;
        self.spawn_tool = false;
        self.spawn_preview = None;
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

    /// Themes are written as names, not indices — the whole point of v4. A level
    /// saved by this build must be readable by a build whose `themes.json` lists
    /// the same themes in a *different order*, which is only possible if the name
    /// is what's on disk.
    #[test]
    fn scheme_is_persisted_by_name_not_index() {
        use engine::render::textures;

        // Pick a theme that is deliberately *not* index 0, so a stray index would
        // be visible in the JSON as a bare number.
        let scheme = textures::scheme_index("archives_1").expect("archives_1 exists");
        assert_ne!(scheme, textures::default_scheme(), "need a non-default theme");

        let mut world = World::new();
        world.regions[0].brushes[0].scheme = scheme;

        let path = std::env::temp_dir().join("bah_persist_scheme_name.json");
        world.save_level(&path).expect("save");
        let json = std::fs::read_to_string(&path).expect("read back");

        assert!(
            json.contains("\"scheme\": \"archives_1\""),
            "theme must be written by name; got:\n{json}"
        );
        assert!(
            !json.contains(&format!("\"scheme\": {scheme}")),
            "no bare theme index may reach disk"
        );

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(
            loaded.regions[0].brushes[0].scheme, scheme,
            "theme must round-trip through its name"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Pre-v4 files stored a bare theme *index*. They must still open, mapping the
    /// index through the manifest's (deliberately preserved) original order — and an
    /// index past the end of a shrunken manifest must degrade to the default rather
    /// than failing the whole load.
    #[test]
    fn legacy_integer_scheme_still_loads() {
        use engine::render::textures;

        let mut world = World::new();
        let path = std::env::temp_dir().join("bah_persist_scheme_legacy.json");
        world.save_level(&path).expect("save");

        let json = std::fs::read_to_string(&path).expect("read");
        let legacy_index = textures::scheme_index("facility_industrial_room").unwrap();

        // Rewrite the file the way a v3 writer would have: version tag + bare index.
        let v3 = json
            .replace("\"version\": 4", "\"version\": 3")
            .replace("\"scheme\": \"facility_white_tile\"", &format!("\"scheme\": {legacy_index}"));
        assert!(v3.contains(&format!("\"scheme\": {legacy_index}")), "test rewrote the file");
        std::fs::write(&path, &v3).expect("write v3");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("a v3 file must still load");
        assert_eq!(
            loaded.regions[0].brushes[0].scheme, legacy_index,
            "a legacy index maps through the manifest's original order"
        );

        // An index no manifest could satisfy degrades to the default, not an error.
        let bogus = v3.replace(&format!("\"scheme\": {legacy_index}"), "\"scheme\": 9999");
        std::fs::write(&path, bogus).expect("write bogus");
        let mut loaded = World::new();
        loaded.load_level(&path).expect("an out-of-range theme must not fail the load");
        assert_eq!(loaded.regions[0].brushes[0].scheme, textures::default_scheme());

        let _ = std::fs::remove_file(&path);
    }

    /// An unknown theme *name* (a level authored against a manifest that has since
    /// dropped that theme) must also degrade rather than make the level unopenable.
    #[test]
    fn unknown_scheme_name_falls_back_to_the_default() {
        let mut world = World::new();
        let path = std::env::temp_dir().join("bah_persist_scheme_unknown.json");
        world.save_level(&path).expect("save");

        let json = std::fs::read_to_string(&path)
            .expect("read")
            .replace("\"facility_white_tile\"", "\"a_theme_that_was_deleted\"");
        std::fs::write(&path, json).expect("write");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("an unknown theme must not fail the load");
        assert_eq!(
            loaded.regions[0].brushes[0].scheme,
            engine::render::textures::default_scheme()
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Every committed slot file in `native/levels/` must still open. These are the
    /// hand-authored test bases, they predate v4, and they are the real regression
    /// surface for the index → name migration.
    #[test]
    fn every_committed_slot_file_still_loads() {
        let dir = levels_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return; // no catalog checked out — nothing to assert
        };
        let mut checked = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let mut world = World::new();
            world
                .load_level(&path)
                .unwrap_or_else(|e| panic!("{} failed to load: {e}", path.display()));
            // A loaded theme index must be resolvable, or the room renders untextured.
            for region in &world.regions {
                for b in &region.brushes {
                    assert!(
                        b.scheme < engine::render::textures::schemes().len(),
                        "{}: brush {} has unresolvable theme {}",
                        path.display(),
                        b.id,
                        b.scheme
                    );
                }
            }
            checked += 1;
        }
        assert!(checked > 0, "expected committed slot files under {}", dir.display());
    }

    /// Per-level quick keys round-trip, and a level binding beats the manifest's own
    /// default for that digit — which is the whole point of making them per-level.
    #[test]
    fn theme_hotkeys_round_trip_and_override_the_manifest() {
        use engine::render::textures;

        let mut world = World::new();
        // '1' has a manifest default (the default theme). Bind it to something else.
        let manifest_default = textures::scheme_for_key('1');
        let other = textures::scheme_index("bunker_1").expect("bunker_1 exists");
        assert_ne!(manifest_default, Some(other), "need a distinguishable binding");

        world.set_theme_hotkey('1', Some(other));
        assert_eq!(world.scheme_for_key('1'), Some(other), "level binding wins");
        // An unbound digit still falls through to the manifest.
        assert_eq!(world.scheme_for_key('9'), textures::scheme_for_key('9'));

        let path = std::env::temp_dir().join("bah_persist_hotkeys.json");
        world.save_level(&path).expect("save");
        let json = std::fs::read_to_string(&path).expect("read");
        assert!(json.contains("\"bunker_1\""), "binding is stored by name:\n{json}");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(loaded.scheme_for_key('1'), Some(other), "binding must survive a reload");

        // Clearing restores the manifest default.
        loaded.set_theme_hotkey('1', None);
        assert_eq!(loaded.scheme_for_key('1'), manifest_default);

        let _ = std::fs::remove_file(&path);
    }

    /// A binding naming a theme this build doesn't have must be ignored, not resolved
    /// to something arbitrary — a pruned manifest must never silently remap a key.
    #[test]
    fn a_hotkey_naming_a_missing_theme_falls_back() {
        let mut world = World::new();
        let path = std::env::temp_dir().join("bah_persist_hotkeys_missing.json");
        world.set_theme_hotkey('4', Some(engine::render::textures::default_scheme()));
        world.save_level(&path).expect("save");

        let json = std::fs::read_to_string(&path)
            .expect("read")
            .replace("\"facility_white_tile\"", "\"a_theme_that_was_pruned\"");
        std::fs::write(&path, json).expect("write");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(
            loaded.scheme_for_key('4'),
            engine::render::textures::scheme_for_key('4'),
            "an unresolvable binding falls through to the manifest"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Only 1-9 are bindable: those are the digits the retexture handler reads, so
    /// accepting anything else would store a key that can never fire.
    #[test]
    fn only_digits_one_to_nine_are_bindable() {
        let mut world = World::new();
        let scheme = engine::render::textures::default_scheme();
        for bad in ['0', 'a', '-'] {
            world.set_theme_hotkey(bad, Some(scheme));
            assert!(
                !world.theme_hotkeys().contains_key(&bad),
                "{bad:?} should not be bindable"
            );
        }
        world.set_theme_hotkey('5', Some(scheme));
        assert!(world.theme_hotkeys().contains_key(&'5'));
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
        use crate::ecs::{AmbientSettings, ComponentData, EntityData};

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
