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
/// **Still v4 (2026-08-28)** with named levels: [`LevelFile::name`] is a new
/// `#[serde(default)]` display name, so v1-v4 files load with none (the panel falls
/// back to the filename) and an older build silently ignores it. By this file's own
/// rule that is not a version-worthy change.
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
    /// The author's display name for this level ("Bunker Base"), shown in the LEVELS
    /// panel. Kept *inside* the file rather than derived from the filename so it can
    /// hold spaces and capitals that the on-disk slug can't, and so a renamed file
    /// still knows what it is. `#[serde(default)]` — an older file has none, and the
    /// panel then falls back to the filename stem.
    #[serde(default)]
    name: String,
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

// ─── The level catalog (the LEVELS panel's model) ─────────────────────────────

/// A level file described well enough for the browser to list it **without loading
/// it** — loading costs a full re-partition and re-bake of the whole level.
#[derive(Clone, Debug)]
pub struct LevelEntry {
    pub path: PathBuf,
    /// Display name: the file's own `name`, or its filename stem when it has none.
    pub name: String,
    pub brushes: usize,
    /// Authored ECS entities: props, lights, spawn pads, doors, pickups.
    pub entities: usize,
    pub bytes: u64,
    /// On-disk format version, so the panel can flag a file from a newer build.
    pub version: u32,
    /// `Some(n)` for `slotN.json` — the file `Fn` still loads. Named levels have `None`.
    pub slot: Option<u8>,
    /// The file exists but could not be parsed, and the panel refuses to load it.
    /// Listed rather than hidden: a level that silently vanishes from the browser
    /// reads as data loss.
    pub error: Option<String>,
}

impl LevelEntry {
    /// Written by a build newer than this one, so [`World::load_level`] would only
    /// manage it best-effort. Worth saying out loud in the panel before a load quietly
    /// drops half a level's authored data.
    pub fn from_newer_build(&self) -> bool {
        self.version > LEVEL_FORMAT_VERSION
    }
}

/// The subset of a level file the listing needs, parsed **without** touching
/// [`Brush`], [`Platform`] or any other authored type.
///
/// Structural on purpose. Counting brushes through `serde_json::Value` means a file
/// written by a newer build — or one whose brush schema has since drifted — still
/// *lists* correctly instead of dropping out of the browser or, worse, listing as
/// broken. Only [`World::load_level`] needs the real types.
#[derive(Deserialize)]
struct LevelHeader {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    regions: Vec<HeaderRegion>,
    #[serde(default)]
    entities: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct HeaderRegion {
    #[serde(default)]
    brushes: Vec<serde_json::Value>,
}

/// A level's fallback display name: its filename stem, verbatim. Not prettified —
/// when a file carries no name of its own the filename *is* the truth about it, and
/// dressing `slot1` up as "Slot 1" would invent a name stored nowhere.
fn name_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string())
}

/// `Some(n)` if `path` is the quick-slot file `slotN.json` for `n` in `1..=8`.
fn slot_of_path(path: &Path) -> Option<u8> {
    let stem = path.file_stem()?.to_str()?;
    let n: u8 = stem.strip_prefix("slot")?.parse().ok()?;
    (1..=8).contains(&n).then_some(n)
}

/// Every level in [`levels_dir`], **most recently saved first** — so the one you were
/// just working on sits at the top of the panel.
///
/// A missing directory is an empty catalog, not an error: a fresh clone has no levels.
pub fn list_levels() -> Vec<LevelEntry> {
    list_levels_in(&levels_dir())
}

/// [`list_levels`] against an explicit directory. Split out so the catalog can be
/// tested against a scratch directory: `levels_dir()` is a compile-time constant
/// pointing at the author's real level library, and tests must neither read it nor
/// write to it.
pub fn list_levels_in(dir: &Path) -> Vec<LevelEntry> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            log::debug!("no levels directory at {} ({e})", dir.display());
            return Vec::new();
        }
    };
    let mut out: Vec<(Option<std::time::SystemTime>, LevelEntry)> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let meta = entry.metadata().ok();
        let bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta.and_then(|m| m.modified().ok());
        let slot = slot_of_path(&path);
        let listing = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<LevelHeader>(&t).map_err(|e| e.to_string()))
        {
            Ok(h) => LevelEntry {
                name: if h.name.trim().is_empty() {
                    name_from_path(&path)
                } else {
                    h.name.trim().to_string()
                },
                brushes: h.regions.iter().map(|r| r.brushes.len()).sum(),
                entities: h.entities.len(),
                version: h.version,
                path,
                bytes,
                slot,
                error: None,
            },
            Err(e) => LevelEntry {
                name: name_from_path(&path),
                brushes: 0,
                entities: 0,
                version: 0,
                path,
                bytes,
                slot,
                error: Some(e),
            },
        };
        out.push((modified, listing));
    }
    // Newest first; anything with no timestamp sinks to the bottom rather than
    // jostling with the files that have one.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, e)| e).collect()
}

/// The on-disk filename stem for a display `name`: lowercased, with every run of
/// non-alphanumerics collapsed to one underscore. `"Bunker Base 2"` → `"bunker_base_2"`.
///
/// Two different names can slug the same, which is exactly why every path below that
/// creates a *new* file refuses to silently overwrite one. Returns empty for a name
/// with nothing usable in it; callers read that as "give it a name first".
pub fn slug_for_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

/// Where a level called `name` lives, or `None` if the name slugs to nothing.
pub fn path_for_name(name: &str) -> Option<PathBuf> {
    path_for_name_in(&levels_dir(), name)
}

/// [`path_for_name`] against an explicit directory.
pub fn path_for_name_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let slug = slug_for_name(name);
    (!slug.is_empty()).then(|| dir.join(format!("{slug}.json")))
}

/// Where a level called `name` would live **beside** the file at `path`.
///
/// Rename and duplicate resolve their destination this way rather than through
/// [`levels_dir`]: a level file's siblings are the other files in its own directory,
/// which for every level the editor touches *is* the levels directory — and making it
/// a function of the argument rather than of a global constant is what lets these
/// operations be tested at all.
fn sibling_path_for_name(path: &Path, name: &str) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    path_for_name_in(dir, name)
}

/// Rewrite the `name` field of the level file at `path` into `dest` — the shared body
/// of both **rename** (`remove_src`) and **duplicate**.
///
/// Edits the JSON as a [`serde_json::Value`] rather than round-tripping through
/// [`LevelFile`], so renaming a v1 file doesn't silently rewrite it as v4, and any
/// field this build doesn't know about survives the trip.
fn rewrite_name(path: &Path, dest: &Path, name: &str, remove_src: bool) -> io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut json: serde_json::Value = serde_json::from_str(&text).map_err(invalid_data)?;
    let obj = json.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "level file is not a JSON object")
    })?;
    obj.insert(
        "name".to_string(),
        serde_json::Value::String(name.trim().to_string()),
    );
    let out = serde_json::to_string_pretty(&json).map_err(invalid_data)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, out)?;
    if remove_src && dest != path {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Fail with `AlreadyExists` if `path` is taken — the guard shared by every path that
/// creates a *new* level file. Overwriting stays deliberate (Save, or `Ctrl+S`).
fn refuse_existing(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "a level file called {} already exists",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
        ));
    }
    Ok(())
}

/// The error for a name that slugs to nothing at all.
fn unusable_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "that name has no letters or digits in it",
    )
}

/// Rename the level at `path` to `name`, moving the file to match the new slug.
/// Returns the new path, which equals `path` when only the display name changed.
pub fn rename_level(path: &Path, name: &str) -> io::Result<PathBuf> {
    let dest = sibling_path_for_name(path, name).ok_or_else(unusable_name)?;
    if dest != path {
        refuse_existing(&dest)?;
    }
    rewrite_name(path, &dest, name, true)?;
    log::info!(
        "renamed level {} → {} ({name:?})",
        path.display(),
        dest.display()
    );
    Ok(dest)
}

/// Copy the level at `path` to a new level called `name`. Returns the new path.
pub fn duplicate_level(path: &Path, name: &str) -> io::Result<PathBuf> {
    let dest = sibling_path_for_name(path, name).ok_or_else(unusable_name)?;
    refuse_existing(&dest)?;
    rewrite_name(path, &dest, name, false)?;
    log::info!("duplicated level {} → {}", path.display(), dest.display());
    Ok(dest)
}

/// Delete the level file at `path`. Guarded to [`levels_dir`] so a stale selection can
/// never turn into a delete somewhere else on disk.
pub fn delete_level(path: &Path) -> io::Result<()> {
    delete_level_fenced(path, &levels_dir())
}

/// [`delete_level`] with the fence directory supplied, so the guard can be tested in
/// both directions without deleting anything in the real level library.
fn delete_level_fenced(path: &Path, fence: &Path) -> io::Result<()> {
    let inside = path
        .canonicalize()
        .ok()
        .zip(fence.canonicalize().ok())
        .map(|(p, d)| p.starts_with(d))
        .unwrap_or(false);
    if !inside {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to delete a file outside the levels directory",
        ));
    }
    std::fs::remove_file(path)?;
    log::info!("deleted level {}", path.display());
    Ok(())
}

impl World {
    /// This level's display name ("Bunker Base"), or empty if it has never been named.
    pub fn level_name(&self) -> &str {
        &self.level_name
    }

    /// Set the display name without writing anything to disk. Counts as an edit, so the
    /// panel's unsaved marker appears until the level is saved under it.
    pub fn set_level_name(&mut self, name: &str) {
        if self.level_name != name.trim() {
            self.level_name = name.trim().to_string();
            self.bump_revision();
        }
    }

    /// How many times the authored level state has changed this session. Meaningful only
    /// by comparison: the app remembers the value at the last save and shows an unsaved
    /// marker while the two differ. See [`World::revision`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Record that the authored level state changed. Called from the undo/redo
    /// chokepoints and from the level-wide setters that bypass them (ambient light,
    /// theme hotkeys, the display name) — those are saved data too, so an edit to one
    /// has to raise the unsaved marker.
    pub(crate) fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Serialize the authored geometry (regions/stairs/platforms/stair-runs +
    /// allocators + spawn point) to `path` as pretty JSON, creating the parent
    /// directory if needed. Derived state (meshes/colliders/nav) is never
    /// written — it's re-derived on load.
    pub fn save_level(&self, path: &Path) -> io::Result<()> {
        let file = LevelFile {
            version: LEVEL_FORMAT_VERSION,
            name: self.level_name.clone(),
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
        // A pre-`name` file (or one written by the levelgen harness) has no display
        // name; the filename is then the only thing left that says what it is.
        if self.level_name.trim().is_empty() {
            self.level_name = name_from_path(path);
        }
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
            // Generated in-process, so there is no authored name to carry.
            name: String::new(),
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
        self.level_name = file.name.clone();

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

    /// Save under a **new** display `name`, into `native/levels/<slug>.json`, and adopt
    /// that name as this level's.
    ///
    /// Refuses rather than overwrites when the target file already exists: "Save As"
    /// onto a name you already used is far more likely a slip than an intent to replace
    /// a level, and Load-then-Save covers the deliberate case.
    pub fn save_level_as(&mut self, name: &str) -> io::Result<PathBuf> {
        let path = path_for_name(name).ok_or_else(unusable_name)?;
        self.save_new_level_at(name, &path)
    }

    /// [`save_level_as`](Self::save_level_as) with the destination supplied — the body
    /// of it, split out so it can be tested against a scratch directory.
    pub(crate) fn save_new_level_at(&mut self, name: &str, path: &Path) -> io::Result<PathBuf> {
        if slug_for_name(name).is_empty() {
            return Err(unusable_name());
        }
        refuse_existing(path)?;
        let previous = std::mem::replace(&mut self.level_name, name.trim().to_string());
        match self.save_level(path) {
            Ok(()) => {
                log::info!("saved level as {name:?} \u{2192} {}", path.display());
                Ok(path.to_path_buf())
            }
            // A failed write must not leave the world claiming a name nothing on disk has.
            Err(e) => {
                self.level_name = previous;
                Err(e)
            }
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

    // ─── Named levels (the LEVELS panel's model) ─────────────────────────────

    /// A scratch directory of level files.
    ///
    /// Every test below works against one of these, never against [`levels_dir`]: that
    /// is the author's real level library, `every_committed_slot_file_still_loads`
    /// asserts over its contents, and a test that writes there both corrupts the
    /// library and races that assertion. It is why the catalog operations all take
    /// their directory as an argument.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bah_levels_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Write a level file by hand, at `version`, with the given `name`. Its brushes are
    /// empty objects: enough for the *listing* to count them (which is the point — the
    /// header parse must not need the real [`Brush`] schema), not enough to load.
    fn write_listing_level(path: &Path, name: &str, version: u32) {
        let json = format!(
            r#"{{"version":{version},"name":"{name}","regions":[{{"id":0,"brushes":[{{}},{{}}]}}],"entities":[{{}}],"next_brush_id":3,"next_platform_id":1,"next_run_id":1}}"#
        );
        std::fs::write(path, json).expect("write level");
    }

    /// Save a real, loadable level called `name` into `dir`.
    fn write_real_level(dir: &Path, name: &str) -> PathBuf {
        let mut world = World::new();
        let path = path_for_name_in(dir, name).expect("slug");
        world.save_new_level_at(name, &path).expect("save")
    }

    /// The display name survives a save → load round trip, which is the whole point of
    /// storing it in the file rather than deriving it from the filename.
    #[test]
    fn display_name_round_trips() {
        let dir = scratch("name_roundtrip");
        let path = write_real_level(&dir, "Bunker Base");
        assert_eq!(path.file_name().unwrap(), "bunker_base.json");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(loaded.level_name(), "Bunker Base");
    }

    /// A file with no `name` — every level written before this build — falls back to its
    /// filename rather than loading as nameless.
    #[test]
    fn a_nameless_file_falls_back_to_its_filename() {
        let dir = scratch("name_fallback");
        let path = dir.join("old_base.json");
        std::fs::write(
            &path,
            r#"{"version":3,"regions":[],"next_brush_id":1,"next_platform_id":1,"next_run_id":1}"#,
        )
        .expect("write");

        let mut world = World::new();
        world.load_level(&path).expect("load");
        assert_eq!(world.level_name(), "old_base");
    }

    /// Names become predictable, filesystem-safe slugs, and a name with nothing usable
    /// in it slugs to empty (which the panel reads as "give it a name first").
    #[test]
    fn names_slug_predictably() {
        assert_eq!(slug_for_name("Bunker Base"), "bunker_base");
        assert_eq!(slug_for_name("  Archives 2  "), "archives_2");
        assert_eq!(slug_for_name("Dam/Facility"), "dam_facility");
        assert_eq!(slug_for_name("a---b"), "a_b");
        assert_eq!(slug_for_name("!!!"), "");
        assert_eq!(slug_for_name(""), "");
        // No separator or dot can survive, so a name can never climb out of its
        // directory or land on a path that isn't a level file.
        for hostile in ["../../etc/passwd", "..\\win", "a/b", "."] {
            let slug = slug_for_name(hostile);
            assert!(!slug.contains('/'), "{hostile:?} slugged to {slug:?}");
            assert!(!slug.contains('\\'), "{hostile:?} slugged to {slug:?}");
            assert!(!slug.contains('.'), "{hostile:?} slugged to {slug:?}");
        }
    }

    /// Save As refuses to overwrite an existing file — and leaves the world's name
    /// alone when it does, so a refused save can't rename the level you're editing.
    #[test]
    fn save_as_refuses_an_existing_name_without_renaming_the_level() {
        let dir = scratch("save_as_collide");
        write_real_level(&dir, "Taken");

        let mut world = World::new();
        world.set_level_name("Original");
        let target = path_for_name_in(&dir, "taken").expect("slug");
        let err = world
            .save_new_level_at("Taken", &target)
            .expect_err("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            world.level_name(),
            "Original",
            "a refused save must not leave the world claiming a name nothing on disk has"
        );
    }

    /// Renaming moves the file to the new slug and rewrites the stored name, and the
    /// old file is gone.
    #[test]
    fn rename_moves_the_file_and_rewrites_the_name() {
        let dir = scratch("rename");
        let old = write_real_level(&dir, "First Try");

        let new = rename_level(&old, "Second Try").expect("rename");
        assert_eq!(new.file_name().unwrap(), "second_try.json");
        assert_eq!(new.parent(), old.parent(), "the copy stays beside its source");
        assert!(!old.exists(), "the old file must not be left behind");

        let mut world = World::new();
        world.load_level(&new).expect("load");
        assert_eq!(world.level_name(), "Second Try");
    }

    /// Renaming edits the JSON in place rather than round-tripping through
    /// [`LevelFile`], so an older file keeps its own version tag — and any field this
    /// build doesn't know about — instead of being silently rewritten as current.
    #[test]
    fn rename_does_not_upgrade_an_old_file() {
        let dir = scratch("rename_v1");
        let old = dir.join("ancient.json");
        std::fs::write(
            &old,
            r#"{"version":1,"regions":[],"mystery_field":42,"next_brush_id":1,"next_platform_id":1,"next_run_id":1}"#,
        )
        .expect("write");

        let new = rename_level(&old, "Ancient Base").expect("rename");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&new).expect("read")).expect("parse");
        assert_eq!(json["version"], 1, "the version tag must not be bumped");
        assert_eq!(json["mystery_field"], 42, "unknown fields must survive");
        assert_eq!(json["name"], "Ancient Base");
    }

    /// Duplicating leaves the source alone, beside it, and gives the copy the new name.
    #[test]
    fn duplicate_leaves_the_source_intact() {
        let dir = scratch("duplicate");
        let src = write_real_level(&dir, "Base");

        let copy = duplicate_level(&src, "Base Copy").expect("duplicate");
        assert!(src.exists(), "the original must survive a duplicate");
        assert_eq!(copy.file_name().unwrap(), "base_copy.json");

        let mut world = World::new();
        world.load_level(&src).expect("load src");
        assert_eq!(world.level_name(), "Base");
        world.load_level(&copy).expect("load copy");
        assert_eq!(world.level_name(), "Base Copy");
    }

    /// Both file-creating operations refuse a taken name rather than overwriting it.
    #[test]
    fn rename_and_duplicate_refuse_a_taken_name() {
        let dir = scratch("collide");
        let a = write_real_level(&dir, "A");
        write_real_level(&dir, "B");

        assert_eq!(
            rename_level(&a, "B").expect_err("must refuse").kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            duplicate_level(&a, "B").expect_err("must refuse").kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(a.exists(), "a refused rename must leave the source in place");

        // Renaming a level to the name it already has is *not* a collision: only the
        // display name changed (say, its capitalisation), and the file stays put.
        let same = rename_level(&a, "a").expect("renaming in place must be allowed");
        assert_eq!(same, a);
    }

    /// A name with nothing usable in it is rejected everywhere, rather than producing
    /// a file called `.json`.
    #[test]
    fn an_unusable_name_is_refused() {
        let dir = scratch("unusable");
        let src = write_real_level(&dir, "Base");
        assert!(path_for_name_in(&dir, "!!!").is_none());
        assert_eq!(
            rename_level(&src, "  ").expect_err("must refuse").kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            duplicate_level(&src, "***").expect_err("must refuse").kind(),
            io::ErrorKind::InvalidInput
        );
        let mut world = World::new();
        assert_eq!(
            world
                .save_new_level_at("", &dir.join("x.json"))
                .expect_err("must refuse")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    /// Delete is fenced to the level directory, so a stale panel selection can never
    /// turn into a delete elsewhere on disk.
    #[test]
    fn delete_is_fenced_to_the_level_directory() {
        let dir = scratch("delete_guard");
        let inside = write_real_level(&dir, "Doomed");
        let outside = std::env::temp_dir().join("bah_not_a_level.json");
        write_listing_level(&outside, "Nope", LEVEL_FORMAT_VERSION);

        let err = delete_level_fenced(&outside, &dir).expect_err("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(outside.exists(), "the refused file must still be there");

        delete_level_fenced(&inside, &dir).expect("a level inside the fence deletes");
        assert!(!inside.exists());
        let _ = std::fs::remove_file(&outside);
    }

    /// The catalog reads each level's name and counts out of the file, recognises a
    /// quick-slot filename, and lists newest-saved first.
    #[test]
    fn the_catalog_describes_what_it_lists() {
        let dir = scratch("listing");
        write_listing_level(&dir.join("older.json"), "Older", LEVEL_FORMAT_VERSION);
        write_listing_level(&dir.join("slot3.json"), "Quick Three", LEVEL_FORMAT_VERSION);
        // Written last, after a gap the filesystem's mtime clock can actually resolve,
        // so newest-first is genuinely asserted rather than accidentally satisfied.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_listing_level(&dir.join("newer.json"), "Newer", LEVEL_FORMAT_VERSION);
        std::fs::write(dir.join("notes.txt"), "not a level").expect("write");

        let rows = list_levels_in(&dir);
        assert_eq!(rows.len(), 3, "only .json files are levels: {rows:?}");
        assert_eq!(
            rows.first().map(|r| r.name.as_str()),
            Some("Newer"),
            "most recently saved first: {rows:?}"
        );

        let newer = rows.iter().find(|r| r.name == "Newer").expect("listed");
        assert_eq!(newer.brushes, 2);
        assert_eq!(newer.entities, 1);
        assert_eq!(newer.slot, None, "a named level is not a quick slot");
        assert!(newer.error.is_none());
        assert!(!newer.from_newer_build());
        assert!(newer.bytes > 0);

        let quick = rows.iter().find(|r| r.name == "Quick Three").expect("listed");
        assert_eq!(quick.slot, Some(3), "slot3.json is what F3 loads");
    }

    /// A level file this build can't parse is **listed** — flagged, and not loadable —
    /// rather than dropped: a level that silently vanishes from the browser reads as
    /// data loss.
    #[test]
    fn an_unparseable_file_is_listed_as_broken() {
        let dir = scratch("broken");
        std::fs::write(dir.join("mangled.json"), "{ this is not json").expect("write");

        let rows = list_levels_in(&dir);
        assert_eq!(rows.len(), 1, "a broken level must still be listed");
        assert!(rows[0].error.is_some(), "and must be flagged as unreadable");
        assert_eq!(rows[0].name, "mangled", "named by its file, since it has no other name");
    }

    /// A file from a newer build is flagged, so the panel can warn before a best-effort
    /// load quietly drops authored data.
    #[test]
    fn a_newer_format_version_is_flagged() {
        let dir = scratch("future");
        write_listing_level(&dir.join("f.json"), "From The Future", LEVEL_FORMAT_VERSION + 1);
        write_listing_level(&dir.join("n.json"), "Current", LEVEL_FORMAT_VERSION);

        let rows = list_levels_in(&dir);
        assert!(rows
            .iter()
            .find(|r| r.name == "From The Future")
            .expect("listed")
            .from_newer_build());
        assert!(!rows
            .iter()
            .find(|r| r.name == "Current")
            .expect("listed")
            .from_newer_build());
    }

    /// Editing the level raises the revision and saving does not — which is exactly what
    /// the panel's unsaved marker reads. Covers the level-wide settings too: they are
    /// saved data, so changing one has to count as an edit.
    #[test]
    fn the_revision_tracks_unsaved_edits() {
        let dir = scratch("revision");
        let mut world = World::new();
        let at_start = world.revision();

        world.record_undo();
        let after_edit = world.revision();
        assert!(after_edit > at_start, "an authored edit must bump the revision");

        let path = dir.join("rev.json");
        world.save_level(&path).expect("save");
        assert_eq!(
            world.revision(),
            after_edit,
            "saving must not itself count as an edit, or a level would never read clean"
        );

        world.set_level_name("Renamed");
        assert!(world.revision() > after_edit, "the display name is saved data");
        let after_name = world.revision();

        world.set_ambient(crate::ecs::AmbientSettings::default());
        assert!(world.revision() > after_name, "ambient light is saved data");
        let after_ambient = world.revision();

        world.set_theme_hotkey('1', None);
        assert!(
            world.revision() > after_ambient,
            "this level's theme hotkeys are saved data"
        );

        // Undo is a change too: the authored state moves, so the marker must reappear.
        let before_undo = world.revision();
        world.undo().expect("undo the recorded edit");
        assert!(world.revision() > before_undo);
    }
}
