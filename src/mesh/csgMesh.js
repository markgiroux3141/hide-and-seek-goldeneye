// CSG mesh lifecycle — cluster brushes into regions, run CSG, swap meshes.
//
// Supports two rebuild modes:
//   1. rebuildAllCSG()   — full teardown + recluster (used by undo, load, delete)
//   2. rebuildAffectedRegions(brushIds) — incremental, only re-evaluates dirty regions

import * as THREE from 'three';
import { state } from '../state.js';
import { scene } from '../scene/setup.js';
import { clusterBrushes, brushesOverlapOrTouch } from '../core/csg/regions.js';
import { CSGRegion } from '../core/csg/CSGRegion.js';
import { assignUVsAndZones } from '../core/csg/uvZones.js';
import { getCSGMaterialsForScheme } from '../scene/materials.js';
import { syncCavesForRegion, disposeCavesForRegion, disposeAllCaves } from './caveMesh.js';
import { isRegionHiddenForSculpt } from '../tools/caveSculpt.js';

// Per-region mesh storage: Map<regionId, { mesh, faceIds, lastEvalMs, region }>
export const csgRegionMeshes = new Map();

// Pending debounced edges-geometry timers, keyed by regionId. Edges are cosmetic
// overlay — recomputing them on every push/pull costs ~60ms at scale. We defer
// until the user pauses (EDGES_DEBOUNCE_MS) so edits feel snappy.
const pendingEdgesTimers = new Map();
const EDGES_DEBOUNCE_MS = 150;

// ─── Stable region tracking ──────────────────────────────────────────
// Persistent maps that survive across incremental rebuilds.
// Only reset by rebuildAllCSG() (undo / load / delete).
const regionMap = new Map();        // regionId -> CSGRegion
const brushToRegion = new Map();    // brushId  -> regionId
let nextStableRegionId = 1;

// Fallback material for grid (non-textured) view mode.
const _gridMaterial = new THREE.MeshStandardMaterial({
    color: 0x6688aa, roughness: 0.7, metalness: 0.1,
    flatShading: true, side: THREE.FrontSide, vertexColors: true,
});

// Disposes only the CSG region's mesh — NOT its cave voxel state. Cave
// lifecycle is decoupled: caves live as long as their CSGRegion object does
// (cleared via disposeAllCaves on full rebuilds, disposeCavesForRegion on
// explicit region removal). Incremental mesh rebuilds preserve voxel data.
function disposeRegion(data) {
    const regionId = data.region?.id;
    if (regionId != null) {
        const t = pendingEdgesTimers.get(regionId);
        if (t) { clearTimeout(t); pendingEdgesTimers.delete(regionId); }
    }
    scene.remove(data.mesh);
    if (data.mesh.geometry) data.mesh.geometry.dispose();
    for (const child of data.mesh.children) {
        if (child.isLineSegments && child.geometry) child.geometry.dispose();
    }
}

// Schedule edges wireframe overlay for a region after the user pauses editing.
// Cancels any prior pending update for the same region.
function scheduleEdgesUpdate(regionId) {
    const prev = pendingEdgesTimers.get(regionId);
    if (prev) clearTimeout(prev);
    const timer = setTimeout(() => {
        pendingEdgesTimers.delete(regionId);
        const data = csgRegionMeshes.get(regionId);
        if (!data) return;
        const { mesh } = data;
        // Remove any existing edges child before recomputing.
        for (let i = mesh.children.length - 1; i >= 0; i--) {
            const c = mesh.children[i];
            if (c.isLineSegments) {
                mesh.remove(c);
                if (c.geometry) c.geometry.dispose();
            }
        }
        const geo = mesh.geometry;
        if (!geo) return;
        if (state.viewMode === 'textured') {
            const edgesGeo = new THREE.EdgesGeometry(geo, 30);
            const edgesMat = new THREE.LineBasicMaterial({ color: 0x000000, transparent: true, opacity: 0.15 });
            mesh.add(new THREE.LineSegments(edgesGeo, edgesMat));
        } else {
            const edgesGeo = new THREE.EdgesGeometry(geo);
            mesh.add(new THREE.LineSegments(edgesGeo, new THREE.LineBasicMaterial({ color: 0x333333 })));
        }
    }, EDGES_DEBOUNCE_MS);
    pendingEdgesTimers.set(regionId, timer);
}

// ─── Build mesh for a single region ──────────────────────────────────
function buildRegionMesh(region) {
    const { geometry: rawGeo, faceIds: rawFaceIds, timeMs } = region.evaluateBrushes();

    let finalGeo, finalFaceIds, material;
    let finalTriZones = null, finalTriCentroids = null;
    if (state.viewMode === 'textured') {
        const result = assignUVsAndZones(rawGeo, rawFaceIds, region.brushes, getCSGMaterialsForScheme);
        finalGeo = result.geometry;
        finalFaceIds = result.faceIds;
        finalTriZones = result.triZones;
        finalTriCentroids = result.triCentroids;
        material = result.materials;
        rawGeo.dispose();
    } else {
        finalGeo = rawGeo;
        finalFaceIds = rawFaceIds;
        material = _gridMaterial;
        if (!finalGeo.getAttribute('color')) {
            const vertCount = finalGeo.getAttribute('position').count;
            const whiteColors = new Float32Array(vertCount * 3).fill(1);
            finalGeo.setAttribute('color', new THREE.Float32BufferAttribute(whiteColors, 3));
        }
    }

    const mesh = new THREE.Mesh(finalGeo, material);
    mesh.userData = { regionId: region.id, isCSG: true };
    // CSG region meshes contain both interior walls AND the outer shell brush
    // (brushId === -1) that wraps the editor space. If the shell cast shadows
    // it would block all light below it. Cheapest fix: CSG receives but doesn't
    // cast in the editor preview. The runtime renderer gets correct shadows.
    mesh.castShadow = false;
    mesh.receiveShadow = true;

    csgRegionMeshes.set(region.id, {
        mesh,
        faceIds: finalFaceIds,
        triZones: finalTriZones,
        triCentroids: finalTriCentroids,
        lastEvalMs: timeMs,
        region,
    });
    if (isRegionHiddenForSculpt(region.id)) mesh.visible = false;
    scene.add(mesh);
    scheduleEdgesUpdate(region.id);
    syncCavesForRegion(region);
}

// ─── Full rebuild ────────────────────────────────────────────────────
// Used by undo, load, delete, and any change that may alter clustering.
export function rebuildAllCSG(brushes = state.csg.brushes) {
    // Tear down all existing region meshes
    for (const [, data] of csgRegionMeshes) disposeRegion(data);
    csgRegionMeshes.clear();

    // Full rebuild replaces CSGRegion objects entirely — caves owned by the
    // old region objects become orphaned, so dispose their CaveWorlds too.
    disposeAllCaves();

    // Reset stable tracking maps
    regionMap.clear();
    brushToRegion.clear();

    if (brushes.length === 0) return;

    const regions = clusterBrushes(brushes);

    for (const region of regions) {
        // Assign stable IDs
        region.id = nextStableRegionId++;

        // Populate tracking maps
        regionMap.set(region.id, region);
        for (const b of region.brushes) {
            brushToRegion.set(b.id, region.id);
        }

        buildRegionMesh(region);
    }

}

// ─── Incremental rebuild ─────────────────────────────────────────────
// Only re-evaluates regions that contain the given brush IDs.
// All other region meshes stay untouched in the scene.
export function rebuildAffectedRegions(brushIds) {
    if (!brushIds || brushIds.length === 0) { rebuildAllCSG(); return; }

    // Auto-assign any unmapped brush ids. Push/pull/extrude create sub-face
    // brushes without pre-registering them — without this, any such edit used
    // to silently fall back to a full rebuild (O(n²) reclustering on every
    // keystroke at scale).
    const brushById = new Map();
    for (const b of state.csg.brushes) brushById.set(b.id, b);
    for (const bid of brushIds) {
        if (brushToRegion.has(bid)) continue;
        const brush = brushById.get(bid);
        if (!brush) continue;
        if (assignBrushToRegion(brush)) return;
    }

    // Collect unique dirty region IDs
    const dirtyRegionIds = new Set();
    for (const bid of brushIds) {
        const rid = brushToRegion.get(bid);
        if (rid != null) dirtyRegionIds.add(rid);
    }

    // If we couldn't map any brush to a region, fall back to full rebuild
    if (dirtyRegionIds.size === 0) { rebuildAllCSG(); return; }

    for (const rid of dirtyRegionIds) {
        const region = regionMap.get(rid);
        if (!region) continue;

        // Dispose old mesh for this region
        const oldData = csgRegionMeshes.get(rid);
        if (oldData) disposeRegion(oldData);
        csgRegionMeshes.delete(rid);

        // Rebuild just this region
        buildRegionMesh(region);
    }

}

// ─── Brush-to-region assignment (for new brushes) ────────────────────
// O(n) scan: test new brush against all existing brushes to find which
// region(s) it touches, then add it to that region. If it touches
// multiple regions, merge them. If none, create a new region.
// Returns true when a full rebuildAllCSG was triggered internally so callers
// can short-circuit and avoid double work.
export function assignBrushToRegion(brush) {
    const touchedRegionIds = new Set();

    // Build a quick lookup from brush id to BrushDef
    const brushById = new Map();
    for (const b of state.csg.brushes) brushById.set(b.id, b);

    for (const [bid, rid] of brushToRegion) {
        const existing = brushById.get(bid);
        if (!existing) continue;
        if (brushesOverlapOrTouch(brush, existing)) {
            touchedRegionIds.add(rid);
        }
    }

    if (touchedRegionIds.size === 0) {
        // New isolated region
        const rid = nextStableRegionId++;
        const region = new CSGRegion(rid);
        region.brushes.push(brush);
        region.updateShell();
        regionMap.set(rid, region);
        brushToRegion.set(brush.id, rid);
        return false;
    }

    if (touchedRegionIds.size === 1) {
        // Add to existing region
        const rid = touchedRegionIds.values().next().value;
        const region = regionMap.get(rid);
        if (region) {
            region.brushes.push(brush);
            brushToRegion.set(brush.id, rid);
        }
        return false;
    }

    // Touches multiple regions — merge them into the first
    const rids = [...touchedRegionIds];
    const primaryRid = rids[0];
    const primaryRegion = regionMap.get(primaryRid);

    for (let i = 1; i < rids.length; i++) {
        const mergeRid = rids[i];
        const mergeRegion = regionMap.get(mergeRid);
        if (!mergeRegion) continue;

        // Move all brushes from mergeRegion to primaryRegion
        for (const b of mergeRegion.brushes) {
            primaryRegion.brushes.push(b);
            brushToRegion.set(b.id, primaryRid);
        }

        // If mergeRegion had baked brushes, move them over and do a full rebuild
        // to properly merge the baked sets.
        if (mergeRegion.bakedBrushes.length > 0) {
            primaryRegion.bakedBrushes.push(...mergeRegion.bakedBrushes);
            primaryRegion.brushes.push(brush);
            brushToRegion.set(brush.id, primaryRid);
            rebuildAllCSG();
            return true;
        }

        // Dispose the merged region's mesh
        const oldData = csgRegionMeshes.get(mergeRid);
        if (oldData) disposeRegion(oldData);
        csgRegionMeshes.delete(mergeRid);
        regionMap.delete(mergeRid);
    }

    primaryRegion.brushes.push(brush);
    brushToRegion.set(brush.id, primaryRid);
    return false;
}

// ─── Direct brush-to-region registration (no overlap scan) ──────────
// Used when the caller already knows which region the brush belongs to
// (e.g. stair brushes carved from a known wall face).
export function assignBrushToRegionDirect(brushId, regionId) {
    brushToRegion.set(brushId, regionId);
}

// ─── Remove brush from region tracking ───────────────────────────────
export function removeBrushFromRegion(brushId) {
    const rid = brushToRegion.get(brushId);
    if (rid == null) return;

    const region = regionMap.get(rid);
    if (region) {
        region.brushes = region.brushes.filter(b => b.id !== brushId);
    }
    brushToRegion.delete(brushId);
}

// Remove a region mesh by id. Also tears down the region's caves since the
// region itself is being dropped.
export function removeCSGRegion(regionId) {
    const data = csgRegionMeshes.get(regionId);
    if (data) {
        disposeRegion(data);
        csgRegionMeshes.delete(regionId);
    }
    disposeCavesForRegion(regionId);
}
