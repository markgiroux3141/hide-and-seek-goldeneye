// Per-CaveDef CaveWorld lifecycle + chunk mesh rendering.
//
// One entry per CaveDef in a region (`region.caves[]`). Each entry owns a
// CaveWorld + a scene group of voxel chunk meshes. On first encounter, the
// world is seeded from cave.protoInit (proto half-sphere) and the boundary
// clip is set to the region's shell interior.

import * as THREE from 'three';
import { scene } from '../scene/setup.js';
import { WORLD_SCALE } from '../core/constants.js';
import { createCaveWorld } from '../core/cave/wasmCave.js';

const VOXEL_SIZE = WORLD_SCALE * 1.6;   // 0.4m at WORLD_SCALE = 0.25
const DEFAULT_DENSITY = 1.0;

// regionId -> Map<caveId, entry>
// entry: { world, group, chunkMeshes: Map<key, Mesh> }
const regionCaves = new Map();

const _caveMaterial = new THREE.MeshStandardMaterial({
    color: 0xffffff,
    roughness: 0.95,
    metalness: 0.0,
    flatShading: false,
    side: THREE.FrontSide,
});

// Rock texture (same BMP the cave-painting spike uses). Loaded lazily so
// caveMesh can be imported before scene init; cave chunks get the texture
// as soon as it resolves. Nearest-filter matches the pixel-art aesthetic.
new THREE.TextureLoader().load('public/textures/tempImgEd00BA.bmp', (tex) => {
    tex.wrapS = THREE.RepeatWrapping;
    tex.wrapT = THREE.RepeatWrapping;
    tex.magFilter = THREE.NearestFilter;
    tex.minFilter = THREE.NearestFilter;
    tex.generateMipmaps = false;
    tex.colorSpace = THREE.SRGBColorSpace;
    _caveMaterial.map = tex;
    _caveMaterial.needsUpdate = true;
});

function chunkKey(cx, cy, cz) { return `${cx},${cy},${cz}`; }

// Clip envelope = cave.extentAabb (world meters). Decoupled from region.shell
// so sculpting can grow the extent and push a new clip to the CaveWorld
// without triggering a CSG rebuild.
//
// No subtracts — clip.rs's subtract rule forces voxels inside to air, which
// creates iso-surfaces at every subtract boundary (wraps CSG rooms with
// cave mesh). A proper "cave yields to CSG" rule needs a Rust `forbid` list
// that skips MC inside AABBs (deferred).
export function buildCaveClipArray(cave) {
    const e = cave.extentAabb;
    return new Float32Array([e.minX, e.minY, e.minZ, e.maxX, e.maxY, e.maxZ]);
}

function disposeEntry(entry) {
    for (const mesh of entry.chunkMeshes.values()) {
        if (mesh.geometry) mesh.geometry.dispose();
    }
    entry.chunkMeshes.clear();
    scene.remove(entry.group);
    if (entry.world && typeof entry.world.free === 'function') entry.world.free();
}

function createEntry(cave) {
    const world = createCaveWorld(VOXEL_SIZE, DEFAULT_DENSITY);

    // protoInit is optional now — cuboid-seeded caves leave it null and
    // get their voxels filled by seedCuboidCave() after the entry is created.
    const p = cave.protoInit;
    if (p) {
        world.init_hollow_cavity(p.centerX, p.centerY, p.centerZ, p.radius, p.amp, p.freq);
    }
    world.set_boundary_clip(buildCaveClipArray(cave));

    const group = new THREE.Group();
    group.name = `cave_${cave.id}`;
    scene.add(group);

    return { world, group, chunkMeshes: new Map() };
}

function regenerateChunk(entry, cx, cy, cz) {
    const handle = entry.world.mesh_chunk(cx, cy, cz);
    const key = chunkKey(cx, cy, cz);
    const existing = entry.chunkMeshes.get(key);

    if (!handle) {
        if (existing) {
            entry.group.remove(existing);
            existing.geometry.dispose();
            entry.chunkMeshes.delete(key);
        }
        return;
    }

    const positions = handle.positions();
    const normals = handle.normals();
    const uvs = handle.uvs();
    handle.free();

    let mesh = existing;
    if (!mesh) {
        const geom = new THREE.BufferGeometry();
        mesh = new THREE.Mesh(geom, _caveMaterial);
        mesh.name = `cavechunk_${cx}_${cy}_${cz}`;
        mesh.castShadow = false;
        mesh.receiveShadow = true;
        entry.group.add(mesh);
        entry.chunkMeshes.set(key, mesh);
    }

    const g = mesh.geometry;
    g.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    g.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
    g.setAttribute('uv', new THREE.BufferAttribute(uvs, 2));
    g.computeBoundingSphere();
    g.computeBoundingBox();
}

function meshDirtyChunks(entry) {
    const dirty = entry.world.flush_dirty();   // Int32Array stride 3
    for (let i = 0; i < dirty.length; i += 3) {
        regenerateChunk(entry, dirty[i], dirty[i + 1], dirty[i + 2]);
    }
}

// Reconcile CaveWorlds for a region. Creates worlds for new CaveDefs, disposes
// those removed, refreshes clip (shell may have grown), and flushes dirty.
export function syncCavesForRegion(region) {
    if (!region) return;
    let entries = regionCaves.get(region.id);
    const presentIds = new Set();

    for (const cave of region.caves) {
        presentIds.add(cave.id);
        if (!entries) { entries = new Map(); regionCaves.set(region.id, entries); }

        let entry = entries.get(cave.id);
        if (!entry) {
            entry = createEntry(cave);
            entries.set(cave.id, entry);
        } else {
            // Extent may have grown — refresh clip so outer voxels re-clamp.
            entry.world.set_boundary_clip(buildCaveClipArray(cave));
        }
        meshDirtyChunks(entry);
    }

    if (!entries) return;
    for (const [caveId, entry] of entries) {
        if (!presentIds.has(caveId)) {
            disposeEntry(entry);
            entries.delete(caveId);
        }
    }
    if (entries.size === 0) regionCaves.delete(region.id);
}

export function disposeCavesForRegion(regionId) {
    if (regionId == null) return;
    const entries = regionCaves.get(regionId);
    if (!entries) return;
    for (const entry of entries.values()) disposeEntry(entry);
    regionCaves.delete(regionId);
}

export function getEntry(regionId, caveId) {
    const entries = regionCaves.get(regionId);
    if (!entries) return null;
    return entries.get(caveId) || null;
}

export function remeshDirty(entry) { meshDirtyChunks(entry); }

// Auto-carve a solid sphere of cave air at (centerMeters, radiusMeters).
// Used on exit-room placement to hollow out the cave voxels inside the new
// CSG subtract so the cave mesh drops out and the room becomes visible.
// Grows cave.extentAabb to include the carve region (with WORLD_SCALE buffer)
// so the clip envelope doesn't force the carved voxels back to solid. Calls
// apply_brush(subtract) in a loop to amortize the strength*dt*falloff so the
// sphere is fully hollowed out in one placement action.
export function carveCaveSphereAt(cave, centerMeters, radiusMeters) {
    const entries = regionCaves.get(cave.regionId);
    const entry = entries ? entries.get(cave.id) : null;
    if (!entry) return;

    const { x: cx, y: cy, z: cz } = centerMeters;
    const r = radiusMeters;
    const buf = WORLD_SCALE;

    if (!cave.extentAabb) {
        cave.extentAabb = {
            minX: cx - r - buf, minY: cy - r - buf, minZ: cz - r - buf,
            maxX: cx + r + buf, maxY: cy + r + buf, maxZ: cz + r + buf,
        };
    } else {
        const e = cave.extentAabb;
        if (cx - r - buf < e.minX) e.minX = cx - r - buf;
        if (cy - r - buf < e.minY) e.minY = cy - r - buf;
        if (cz - r - buf < e.minZ) e.minZ = cz - r - buf;
        if (cx + r + buf > e.maxX) e.maxX = cx + r + buf;
        if (cy + r + buf > e.maxY) e.maxY = cy + r + buf;
        if (cz + r + buf > e.maxZ) e.maxZ = cz + r + buf;
    }
    entry.world.set_boundary_clip(buildCaveClipArray(cave));

    // Mode 0 = subtract. strength=2.0, dt=0.2 per call × 10 calls = 4.0 total
    // at the center, enough to drive full-solid (1.0) voxels to full-air.
    for (let i = 0; i < 10; i++) {
        entry.world.apply_brush(0, cx, cy, cz, r, 2.0, 0.2);
    }
    meshDirtyChunks(entry);
}

// Seed a cuboid region of cave air matching the given world-meter AABB.
// Used by two bake-prep flows:
//   • J (startCaveFromFace)   — cuboid extends from the clicked wall into
//                                solid rock, cross-section = full face.
//   • P (placeExitRoomFromCaveHit) — cuboid fills the new exit room's
//                                interior so MC emits cave-mesh at every
//                                room wall; the bake anchor-rect pass drops
//                                the cave-facing wall's cap, the room AABB
//                                pass drops the rest.
//
// The cuboid shape gives the bake's "delete every triangle coplanar with
// the anchor plane (inside the u/v rect)" pass a clean target — the
// CSG wall and the cuboid's matching MC face sit on that plane and drop
// together.
export function seedCuboidCave(cave, cuboidAabb) {
    const entries = regionCaves.get(cave.regionId);
    const entry = entries ? entries.get(cave.id) : null;
    if (!entry) return;

    const { minX, minY, minZ, maxX, maxY, maxZ } = cuboidAabb;
    const step = VOXEL_SIZE * 0.7;
    const carveR = VOXEL_SIZE * 1.2;

    for (let x = minX + step * 0.5; x < maxX; x += step) {
        for (let y = minY + step * 0.5; y < maxY; y += step) {
            for (let z = minZ + step * 0.5; z < maxZ; z += step) {
                for (let i = 0; i < 10; i++) {
                    entry.world.apply_brush(0, x, y, z, carveR, 2.0, 0.3);
                }
            }
        }
    }
    meshDirtyChunks(entry);
}

export function disposeAllCaves() {
    for (const entries of regionCaves.values()) {
        for (const entry of entries.values()) disposeEntry(entry);
    }
    regionCaves.clear();
}

// Dev-only visibility toggle — call __toggleCaves() in the console.
if (typeof window !== 'undefined') {
    window.__toggleCaves = () => {
        let anyVisible = false;
        for (const entries of regionCaves.values()) {
            for (const entry of entries.values()) {
                if (entry.group.visible) { anyVisible = true; break; }
            }
            if (anyVisible) break;
        }
        const next = !anyVisible;
        for (const entries of regionCaves.values()) {
            for (const entry of entries.values()) entry.group.visible = next;
        }
        console.log(`[caves] visible = ${next}`);
        return next;
    };
}
