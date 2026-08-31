//! Region partitioning + incremental re-bake — the port of the original editor's
//! `src/core/csg/regions.js` (clustering) and `src/mesh/csgMesh.js` (the
//! rebuild-all / rebuild-affected split + brush→region tracking + memoization).
//!
//! The engine already models a [`Region`] as "the unit of re-bake"; this layer
//! is what actually splits a level into several of them and, crucially, re-bakes
//! **only the region(s) an edit touched** instead of the whole world. For a fully
//! connected base that's still one region (rooms bridge through doorway cuts), so
//! the concrete wins here are: disconnected geometry re-bakes independently,
//! undo/load re-fold only the region that changed (memoization), and the
//! worst-case "unmapped brush ⇒ full recluster on every keystroke" fallback the
//! original called out is gone.

use std::collections::{HashMap, HashSet, VecDeque};

use engine::geometry::csg_runtime::{brushes_overlap_or_touch, cluster_brush_indices};
use engine::render::mesh::{CpuMesh, TexturedMesh};

use super::*;

/// FIFO cache of CSG results keyed by a hash of a region's authored brushes +
/// stairs (JS `wasmResultCache`). A full recluster re-serializes every region,
/// but only the one whose brushes actually changed misses — the rest return their
/// cached (collider, textured) pair and skip the fold entirely. 128 entries ≈ a
/// few MB of mesh data, plenty for undo/redo churn.
pub(crate) struct CsgCache {
    map: HashMap<u64, (CpuMesh, TexturedMesh)>,
    order: VecDeque<u64>,
    limit: usize,
}

impl CsgCache {
    pub(crate) fn new() -> Self {
        CsgCache {
            map: HashMap::new(),
            order: VecDeque::new(),
            limit: 128,
        }
    }

    pub(crate) fn get(&self, key: u64) -> Option<&(CpuMesh, TexturedMesh)> {
        self.map.get(&key)
    }

    pub(crate) fn insert(&mut self, key: u64, value: (CpuMesh, TexturedMesh)) {
        if self.map.contains_key(&key) {
            return;
        }
        if self.order.len() >= self.limit {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(key);
        self.map.insert(key, value);
    }
}

/// Hash a region's *authored* data (brushes + stairs) into a memo-cache key. The
/// shell is derived from the brushes, so it isn't hashed. f32s go in by bit
/// pattern; the small fieldless enums by discriminant.
pub(crate) fn region_hash(region: &Region, platforms: &[Platform]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Platforms are not region data, but the bake reads them: a deck is a floor, and
    // a floor decides where a wall's texture bands start (`BrushInfo::wall_anchor`).
    // Same reason `face_tex` is hashed below — without this the region hands back its
    // pre-platform bake.
    platforms.len().hash(&mut h);
    for p in platforms {
        p.id.hash(&mut h);
        for f in [p.x, p.y, p.z, p.size_x, p.size_z, p.thickness] {
            f.to_bits().hash(&mut h);
        }
        (p.grounded as u8).hash(&mut h);
    }
    region.brushes.len().hash(&mut h);
    for b in &region.brushes {
        b.id.hash(&mut h);
        (b.op as u8).hash(&mut h);
        for f in [b.x, b.y, b.z, b.w, b.h, b.d, b.floor_y] {
            f.to_bits().hash(&mut h);
        }
        (b.door as u8).hash(&mut h);
        (b.frame as u8).hash(&mut h);
        b.scheme.hash(&mut h);
        // A theme's cornice depth moves a band boundary, so the classified mesh depends
        // on it as surely as on the scheme index. Same reason `face_tex` is hashed: the
        // texture editor can change it without any brush changing, and without this the
        // region hands back its pre-cornice bake.
        engine::render::textures::cornice_of(b.scheme)
            .map(f32::to_bits)
            .hash(&mut h);
        // Painted face overrides change the classified mesh, so a repaint has to
        // miss the memo cache — without this the region returns its pre-paint bake.
        for ft in &b.face_tex {
            match ft {
                Some(t) => {
                    1u8.hash(&mut h);
                    t.scheme.hash(&mut h);
                    t.zone.hash(&mut h);
                }
                None => 0u8.hash(&mut h),
            }
        }
    }
    region.stairs.len().hash(&mut h);
    for s in &region.stairs {
        (s.direction as u8).hash(&mut h);
        s.step_count.hash(&mut h);
        (s.axis as u8).hash(&mut h);
        (s.side as u8).hash(&mut h);
        (s.u_axis as u8).hash(&mut h);
        for f in [s.face_pos, s.u0, s.u1, s.floor, s.ceil, s.floor_y] {
            f.to_bits().hash(&mut h);
        }
        s.scheme.hash(&mut h);
        s.void_ids.hash(&mut h);
    }
    h.finish()
}

impl World {
    /// Re-partition **every** brush into connected regions from scratch (JS
    /// `rebuildAllCSG`). Used on load / undo / redo, where clustering may have
    /// changed. Regions get fresh stable ids; `brush_to_region` is rebuilt; every
    /// region is re-baked (the cache makes unchanged ones cheap) and its collider
    /// set. Region ids that existed before but not after are returned as empty
    /// meshes so the renderer + physics drop them.
    pub(crate) fn recluster_all(&mut self) -> Vec<RegionMesh> {
        let old_ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        // Flatten the authored data out of the current regions and re-partition.
        let mut all_brushes: Vec<Brush> = Vec::new();
        let mut all_stairs: Vec<StairDesc> = Vec::new();
        for r in &self.regions {
            all_brushes.extend_from_slice(&r.brushes);
            all_stairs.extend_from_slice(&r.stairs);
        }
        self.rebuild_from_flat(all_brushes, all_stairs, old_ids)
    }

    /// Cluster a flat brush + stair list into connected regions, replace
    /// `self.regions` with them (fresh stable ids), rebuild `brush_to_region`,
    /// re-bake every region, and emit empty meshes for each `old_ids` entry that
    /// no longer exists. The shared core of [`recluster_all`], `load_level`, and
    /// the undo/redo restore — each supplies its own brush list + prior id set.
    pub(crate) fn rebuild_from_flat(
        &mut self,
        all_brushes: Vec<Brush>,
        all_stairs: Vec<StairDesc>,
        old_ids: Vec<u32>,
    ) -> Vec<RegionMesh> {
        self.brush_to_region.clear();

        if all_brushes.is_empty() {
            self.regions.clear();
        } else {
            let groups = cluster_brush_indices(&all_brushes);
            let mut new_regions: Vec<Region> = Vec::with_capacity(groups.len());
            for group in groups {
                let rid = self.next_region_id;
                self.next_region_id += 1;
                let mut region = Region::new(rid);
                let mut ids_in: HashSet<u32> = HashSet::new();
                for &bi in &group {
                    let b = all_brushes[bi];
                    ids_in.insert(b.id);
                    self.brush_to_region.insert(b.id, rid);
                    region.brushes.push(b);
                }
                // **Restore authoring order.** `evaluate` folds a region's brushes in
                // slice order and that order is load-bearing: a `Subtract` after an `Add`
                // carves the added geometry away, which is what makes "punch a hole
                // through a pillar" expressible. But `cluster_brush_indices` is a
                // stack-based DFS, so the order it hands back is neither the authored
                // order nor even stable — it visits the *last* touching neighbour first.
                //
                // Brush ids are allocated monotonically at creation, so ascending id **is**
                // authoring order, and it is exactly the order the incremental edit path
                // produces (tools append). Sorting here is therefore what makes a reclustered
                // fold identical to the fold that was authored.
                //
                // Without this, load / undo / redo silently re-fold a level differently from
                // how it was built: an additive brush spanning two subtract brushes (a
                // drawn extrude across a widened room, a pillar in a room later extended)
                // loses whichever part lies inside a subtract that DFS happened to order
                // after it. It renders correctly until you save and reload.
                region.brushes.sort_by_key(|b| b.id);
                // A stair belongs to the region that owns its void brushes.
                for s in &all_stairs {
                    if s.void_ids.iter().any(|id| ids_in.contains(id)) {
                        region.stairs.push(*s);
                    }
                }
                region.refresh_shell();
                new_regions.push(region);
            }
            self.regions = new_regions;
        }

        self.rebake_and_clear(old_ids)
    }

    /// Re-bake only the regions that own the given brush ids (JS
    /// `rebuildAffectedRegions`) — the incremental edit path. Any id not yet in
    /// `brush_to_region` is auto-assigned first ([`assign_brush_to_region`]); if
    /// that can't place it, we fall back to a full recluster. Returns the meshes
    /// for the touched regions to upload.
    pub(crate) fn rebuild_affected_regions(&mut self, brush_ids: &[u32]) -> Vec<RegionMesh> {
        if brush_ids.is_empty() {
            return Vec::new();
        }

        // Auto-assign any unmapped brush (a tool that created one without
        // registering). A merge across regions returns `None` → recluster.
        for &bid in brush_ids {
            if self.brush_to_region.contains_key(&bid) {
                continue;
            }
            if self.assign_brush_to_region(bid).is_none() {
                return self.recluster_all();
            }
        }

        let mut dirty: Vec<u32> = Vec::new();
        for &bid in brush_ids {
            if let Some(&rid) = self.brush_to_region.get(&bid) {
                if !dirty.contains(&rid) {
                    dirty.push(rid);
                }
            }
        }
        if dirty.is_empty() {
            return self.recluster_all();
        }

        dirty
            .into_iter()
            .filter_map(|rid| self.rebuild_region(rid))
            .collect()
    }

    /// Register a brush that already lives in some region's `brushes` list into
    /// `brush_to_region` by testing it against every other brush's AABB (JS
    /// `assignBrushToRegion`). Overlaps exactly one region → join it. Overlaps
    /// several → they must merge, which reshapes region ids, so we signal the
    /// caller (`None`) to recluster instead. Overlaps none → it stays in whatever
    /// region currently holds it (a tool always creates a brush inside one).
    ///
    /// Returns the owning region id, or `None` if a merge is required.
    pub(crate) fn assign_brush_to_region(&mut self, brush_id: u32) -> Option<u32> {
        // Find the brush (a copy) and the region it currently sits in.
        let mut this: Option<Brush> = None;
        let mut current_region: Option<u32> = None;
        for r in &self.regions {
            if let Some(b) = r.brushes.iter().find(|b| b.id == brush_id) {
                this = Some(*b);
                current_region = Some(r.id);
                break;
            }
        }
        let this = this?;

        // Which regions does it touch (besides via itself)?
        let mut touched: Vec<u32> = Vec::new();
        for r in &self.regions {
            let hit = r
                .brushes
                .iter()
                .any(|b| b.id != brush_id && brushes_overlap_or_touch(&this, b));
            if hit && !touched.contains(&r.id) {
                touched.push(r.id);
            }
        }

        match touched.len() {
            0 => {
                // Isolated — belongs to whatever region already holds it.
                let rid = current_region?;
                self.brush_to_region.insert(brush_id, rid);
                Some(rid)
            }
            1 => {
                let rid = touched[0];
                self.brush_to_region.insert(brush_id, rid);
                Some(rid)
            }
            _ => None, // multi-region merge — caller reclusters
        }
    }

    /// Drop a brush from region tracking + its region's brush list (JS
    /// `removeBrushFromRegion`). No re-bake — the caller re-bakes. Kept for the
    /// forthcoming delete-brush tool; brush creation is the only mutator today.
    #[allow(dead_code)]
    pub(crate) fn remove_brush_from_region(&mut self, brush_id: u32) {
        if let Some(rid) = self.brush_to_region.remove(&brush_id) {
            if let Some(region) = self.regions.iter_mut().find(|r| r.id == rid) {
                region.brushes.retain(|b| b.id != brush_id);
            }
        }
    }

    /// Shared tail of the full-rebuild paths: re-bake every current region and
    /// return an empty mesh for any `old_ids` entry that no longer exists.
    fn rebake_and_clear(&mut self, old_ids: Vec<u32>) -> Vec<RegionMesh> {
        let ids: Vec<u32> = self.regions.iter().map(|r| r.id).collect();
        let live: HashSet<u32> = ids.iter().copied().collect();
        let mut meshes: Vec<RegionMesh> = Vec::new();
        for id in ids {
            if let Some(rm) = self.rebuild_region(id) {
                meshes.push(rm);
            }
        }
        for old in old_ids {
            if !live.contains(&old) {
                self.physics.set_region_collider(old, &CpuMesh::default());
                meshes.push(RegionMesh {
                    id: old,
                    mesh: TexturedMesh::default(),
                });
            }
        }
        meshes
    }
}
