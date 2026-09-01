//! Undo / redo for BUILD-phase authoring.
//!
//! A level is a small bag of authored plain-data — the *same* source of truth
//! [`World::save_level`] serializes (each region's brushes + stairs, the
//! free-standing platforms + stair-runs, the spawn point, and the id
//! allocators). Everything else — meshes, colliders, the nav grid — is derived
//! and rebuilt on demand. So undo/redo is just **in-memory save/load**: snapshot
//! that authored state before each edit, and to step back, restore a snapshot
//! and re-bake through the exact same path [`World::load_level`] uses.
//!
//! We snapshot the whole authored state rather than record per-tool inverse
//! commands: the state is tiny POD (a brush is ~80 bytes), the rebuild path
//! already exists, and one snapshot type is far less to maintain than a correct
//! inverse for every one of the dozen authoring tools. At [`MAX_HISTORY`] a
//! marathon session costs a few MB — effectively unlimited in practice.

use super::*;

/// Max undo (and redo) depth. Snapshots hold only authored POD, so this bounds a
/// long editing session to a few MB while feeling limitless during normal use.
/// Bump freely — the cost is linear and small.
pub(crate) const MAX_HISTORY: usize = 100;

/// One region's authored data inside a snapshot (the shell is derived, so it's
/// recomputed on restore — mirrors `persist::RegionData`). [`Region`] itself
/// isn't `Clone` (it owns a private derived shell), so we snapshot its parts.
#[derive(Clone)]
struct RegionSnapshot {
    brushes: Vec<Brush>,
    stairs: Vec<StairDesc>,
}

/// A restorable copy of the authored level — the undo/redo unit. Holds exactly
/// what [`World::save_level`] writes; nothing derived.
#[derive(Clone)]
pub(crate) struct LevelSnapshot {
    regions: Vec<RegionSnapshot>,
    platforms: Vec<Platform>,
    stair_runs: Vec<StairRun>,
    /// Authored ECS props (their plain-data form — the same [`crate::ecs::EntityData`]
    /// the level file stores), so undo/redo covers placement like any other edit.
    entities: Vec<crate::ecs::EntityData>,
    spawn_point: Vec3,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
    // ── The level's identity ──
    //
    // Everything below is saved data that is **not** geometry, and it was missing from
    // this snapshot until `New level` arrived. The gap was invisible while every
    // undoable edit was a geometry edit: those five fields never changed, so never
    // needing to be restored looked like not needing to be captured.
    //
    // Load broke that quietly — `load_level_file` snapshots and commits so that an
    // accidental Load is one Ctrl+Z from being undone, and that undo brought the
    // brushes back with the name blanked, the theme hotkeys gone and the ambient light
    // reset to default. `New level` makes it routine rather than rare, which is what
    // finally paid for the fix. Undo restores the level, not just its shape.
    level_name: String,
    ambient: crate::ecs::AmbientSettings,
    theme_hotkeys: std::collections::BTreeMap<char, String>,
    platforms_are_floors: bool,
    play: PlayConfig,
}

impl World {
    /// Capture the current authored state (cheap — clones POD only).
    pub(crate) fn snapshot(&self) -> LevelSnapshot {
        LevelSnapshot {
            regions: self
                .regions
                .iter()
                .map(|r| RegionSnapshot {
                    brushes: r.brushes.clone(),
                    stairs: r.stairs.clone(),
                })
                .collect(),
            platforms: self.platforms.clone(),
            stair_runs: self.stair_runs.clone(),
            entities: self.ecs.save_authored(),
            spawn_point: self.spawn_point,
            next_brush_id: self.next_brush_id,
            next_platform_id: self.next_platform_id,
            next_run_id: self.next_run_id,
            level_name: self.level_name.clone(),
            ambient: self.ambient,
            theme_hotkeys: self.theme_hotkeys.clone(),
            platforms_are_floors: self.platforms_are_floors,
            play: self.play.clone(),
        }
    }

    /// Push `snap` onto the undo stack as the pre-edit state, capping the depth
    /// (oldest dropped) and clearing the redo stack — a new edit forks history,
    /// so anything previously undone can no longer be redone.
    pub(crate) fn commit_snapshot(&mut self, snap: LevelSnapshot) {
        self.bump_revision();
        self.undo_stack.push(snap);
        if self.undo_stack.len() > MAX_HISTORY {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Snapshot the current state and commit it unconditionally — used where the
    /// mutation is certain and doesn't hand back a success signal (a gizmo drag
    /// begins in [`gizmo_start`](Self::gizmo_start)).
    pub(crate) fn record_undo(&mut self) {
        let snap = self.snapshot();
        self.commit_snapshot(snap);
    }

    /// Run a committing edit `f` with an undo checkpoint: snapshot the pre-edit
    /// state, run `f`, and record the checkpoint only if `f` actually changed
    /// geometry (returned `Some`). So a no-op edit — a too-thin pull, a crosshair
    /// that hit nothing — never leaves a dead step in the history. Returns `f`'s
    /// result unchanged, so callers keep uploading the rebuilt mesh as before.
    pub(crate) fn with_undo(
        &mut self,
        f: impl FnOnce(&mut Self) -> Option<RegionMesh>,
    ) -> Option<RegionMesh> {
        let snap = self.snapshot();
        let out = f(self);
        if out.is_some() {
            self.commit_snapshot(snap);
        }
        out
    }

    /// [`with_undo`](Self::with_undo) for an edit that can change **several** regions
    /// at once, so it hands back a `Vec` instead of one mesh. Same contract: the
    /// checkpoint is recorded only if something actually changed (a non-empty result).
    ///
    /// The freeform draw tool needs this — it adds N brushes across a footprint drawn
    /// up against walls, which routinely bridges separate regions and makes
    /// [`rebuild_affected_regions`](Self::rebuild_affected_regions) recluster the level
    /// and return a mesh per region.
    pub(crate) fn with_undo_many(
        &mut self,
        f: impl FnOnce(&mut Self) -> Vec<RegionMesh>,
    ) -> Vec<RegionMesh> {
        let snap = self.snapshot();
        let out = f(self);
        if !out.is_empty() {
            self.commit_snapshot(snap);
        }
        out
    }

    /// Step back one edit (BUILD only — geometry is frozen in HUNT). Moves the
    /// current state onto the redo stack, restores the previous snapshot, and
    /// returns every rebuilt/removed mesh for the app to upload. `None` if the
    /// undo stack is empty or a hunt is live.
    pub fn undo(&mut self) -> Option<Vec<RegionMesh>> {
        if self.mode != Mode::Build {
            return None;
        }
        let prev = self.undo_stack.pop()?;
        let current = self.snapshot();
        self.redo_stack.push(current);
        log::info!("undo ({} left)", self.undo_stack.len());
        Some(self.apply_snapshot(prev))
    }

    /// Re-apply the most recently undone edit (BUILD only). Symmetric with
    /// [`undo`](Self::undo): the current state goes back onto the undo stack.
    pub fn redo(&mut self) -> Option<Vec<RegionMesh>> {
        if self.mode != Mode::Build {
            return None;
        }
        let next = self.redo_stack.pop()?;
        let current = self.snapshot();
        self.undo_stack.push(current);
        log::info!("redo ({} left)", self.redo_stack.len());
        Some(self.apply_snapshot(next))
    }

    /// Replace the authored geometry with `snap` and rebuild every derived mesh +
    /// collider, returning the meshes to upload. Mirrors [`load_level`]'s rebuild
    /// exactly (the three mesh cases: rebuilt regions, empty-clears for region
    /// ids that vanished, and the combined structures mesh).
    ///
    /// [`load_level`]: World::load_level
    fn apply_snapshot(&mut self, snap: LevelSnapshot) -> Vec<RegionMesh> {
        self.bump_revision();
        // Region ids present before the restore, so any that vanish get cleared.
        let old_ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();

        // Flatten the snapshot's authored data and re-partition (clustering may
        // differ from when it was captured; `rebuild_from_flat` handles it).
        let mut all_brushes: Vec<Brush> = Vec::new();
        let mut all_stairs: Vec<StairDesc> = Vec::new();
        for rs in snap.regions {
            all_brushes.extend(rs.brushes);
            all_stairs.extend(rs.stairs);
        }
        self.platforms = snap.platforms;
        self.stair_runs = snap.stair_runs;
        self.ecs.load_authored(&snap.entities);
        self.spawn_point = snap.spawn_point;
        self.next_brush_id = snap.next_brush_id;
        self.next_platform_id = snap.next_platform_id;
        self.next_run_id = snap.next_run_id;
        self.level_name = snap.level_name;
        self.ambient = snap.ambient;
        self.theme_hotkeys = snap.theme_hotkeys;
        self.play = snap.play;
        // **Before `rebuild_from_flat` below**, exactly as in `apply_level`: the wall
        // band probe reads this during the bake, so restoring it afterwards would bake
        // the walls against the *previous* level's answer.
        self.platforms_are_floors = snap.platforms_are_floors;

        // Any selection / armed tool may point at geometry the snapshot lacks.
        self.reset_edit_state_for_load();

        let mut meshes = self.rebuild_from_flat(all_brushes, all_stairs, old_ids);
        meshes.push(self.rebuild_structures());
        meshes
    }
}
