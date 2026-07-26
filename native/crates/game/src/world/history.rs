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

use engine::render::mesh::{CpuMesh, TexturedMesh};

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
    id: u32,
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
    spawn_point: Vec3,
    next_brush_id: u32,
    next_platform_id: u32,
    next_run_id: u32,
}

impl World {
    /// Capture the current authored state (cheap — clones POD only).
    pub(crate) fn snapshot(&self) -> LevelSnapshot {
        LevelSnapshot {
            regions: self
                .regions
                .iter()
                .map(|r| RegionSnapshot {
                    id: r.id,
                    brushes: r.brushes.clone(),
                    stairs: r.stairs.clone(),
                })
                .collect(),
            platforms: self.platforms.clone(),
            stair_runs: self.stair_runs.clone(),
            spawn_point: self.spawn_point,
            next_brush_id: self.next_brush_id,
            next_platform_id: self.next_platform_id,
            next_run_id: self.next_run_id,
        }
    }

    /// Push `snap` onto the undo stack as the pre-edit state, capping the depth
    /// (oldest dropped) and clearing the redo stack — a new edit forks history,
    /// so anything previously undone can no longer be redone.
    pub(crate) fn commit_snapshot(&mut self, snap: LevelSnapshot) {
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
        // Region ids present before the restore, so any that vanish get cleared.
        let old_ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();

        let mut regions = Vec::with_capacity(snap.regions.len());
        for rs in snap.regions {
            let mut region = Region::new(rs.id);
            region.brushes = rs.brushes;
            region.stairs = rs.stairs;
            region.refresh_shell();
            regions.push(region);
        }
        self.regions = regions;
        self.platforms = snap.platforms;
        self.stair_runs = snap.stair_runs;
        self.spawn_point = snap.spawn_point;
        self.next_brush_id = snap.next_brush_id;
        self.next_platform_id = snap.next_platform_id;
        self.next_run_id = snap.next_run_id;

        // Any selection / armed tool may point at geometry the snapshot lacks.
        self.reset_edit_state_for_load();

        let ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        let new_ids: std::collections::HashSet<u32> = ids.iter().copied().collect();
        let mut meshes: Vec<RegionMesh> = Vec::new();
        for id in ids {
            if let Some(rm) = self.rebuild_region(id) {
                meshes.push(rm);
            }
        }
        for old in old_ids {
            if !new_ids.contains(&old) {
                self.physics.set_region_collider(old, &CpuMesh::default());
                meshes.push(RegionMesh {
                    id: old,
                    mesh: TexturedMesh::default(),
                });
            }
        }
        meshes.push(self.rebuild_structures());
        meshes
    }
}
