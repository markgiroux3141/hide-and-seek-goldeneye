// CSGRegion — one connected cluster of brushes plus its auto-resized shell.
// Boolean operations are handled by the Rust/WASM CSG module; the post-CSG
// pipeline (faceMap, uvZones, materials) stays in JavaScript.

import * as THREE from 'three';
import { WALL_THICKNESS, WORLD_SCALE } from '../constants.js';
import { BrushDef } from '../BrushDef.js';
import { buildFaceMap } from './faceMap.js';
import { evaluateRegionWasm } from './wasmCSG.js';

// Serialize a BrushDef into the JSON shape the WASM module expects.
function brushToInput(b) {
    return { id: b.id, op: b.op, x: b.x, y: b.y, z: b.z, w: b.w, h: b.h, d: b.d, taper: b.taper };
}

// Memoize WASM evaluation by input JSON. Any rebuildAllCSG triggered by an
// unrelated edit (delete, load, cross-region merge) re-serializes every
// region — regions whose input is unchanged hit the cache and skip the WASM
// call entirely. Simple FIFO eviction; 128 ≈ a few MB of typed-array data.
const WASM_CACHE_LIMIT = 128;
const wasmResultCache = new Map();

export class CSGRegion {
    constructor(id) {
        this.id = id;
        // Shell starts as a 1×1×1 placeholder; updateShell() resizes to fit brushes.
        // Shell uses brushId = -1 (sentinel). buildFaceMap reserves 0 for "baked/unmatched"
        // and user brushes have positive ids assigned from state.csg.nextBrushId.
        this.shell = new BrushDef(-1, 'add', 0, 0, 0, 1, 1, 1);
        this.brushes = [];               // BrushDef[] (the un-baked brushes)
        this.bakedBrushes = [];          // BrushDef[] (previously baked brushes, replayed each eval)
        this.totalBakedBrushes = 0;
        this.caves = [];                 // CaveDef[] — voxel cavities carved outward from a face
    }

    // Auto-resize the shell to fit all subtractive brushes (baked + unbaked),
    // with a WALL_THICKNESS margin so each room has solid walls in every direction.
    updateShell() {
        let minX = Infinity, minY = Infinity, minZ = Infinity;
        let maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;

        for (const b of this.bakedBrushes) {
            if (b.op !== 'subtract') continue;
            minX = Math.min(minX, b.minX); minY = Math.min(minY, b.minY); minZ = Math.min(minZ, b.minZ);
            maxX = Math.max(maxX, b.maxX); maxY = Math.max(maxY, b.maxY); maxZ = Math.max(maxZ, b.maxZ);
        }

        for (const b of this.brushes) {
            if (b.op !== 'subtract') continue;
            minX = Math.min(minX, b.minX); minY = Math.min(minY, b.minY); minZ = Math.min(minZ, b.minZ);
            maxX = Math.max(maxX, b.maxX); maxY = Math.max(maxY, b.maxY); maxZ = Math.max(maxZ, b.maxZ);
        }
        // Pad the shell around each cave's current voxel extent so the cave's
        // far-rock surround remains inside the region's solid volume. Extents
        // are in world meters — convert back to WT (shell coords are WT-space).
        for (const cave of this.caves) {
            const a = cave.extentAabb;
            if (!a) continue;
            const invS = 1 / WORLD_SCALE;
            minX = Math.min(minX, a.minX * invS); minY = Math.min(minY, a.minY * invS); minZ = Math.min(minZ, a.minZ * invS);
            maxX = Math.max(maxX, a.maxX * invS); maxY = Math.max(maxY, a.maxY * invS); maxZ = Math.max(maxZ, a.maxZ * invS);
        }
        if (!isFinite(minX)) return;

        const t = WALL_THICKNESS;
        this.shell.x = minX - t; this.shell.y = minY - t; this.shell.z = minZ - t;
        this.shell.w = (maxX - minX) + t * 2;
        this.shell.h = (maxY - minY) + t * 2;
        this.shell.d = (maxZ - minZ) + t * 2;
    }

    // Run CSG via WASM: shell ± all brushes (baked + unbaked).
    // Returns the result as { geometry, faceIds, timeMs }.
    //
    // The WASM module already implements the pre-merge optimization internally
    // (3+ consecutive subtracts are unioned first, then subtracted once).
    evaluateBrushes() {
        this.updateShell();
        const t0 = performance.now();

        const allBrushes = [...this.bakedBrushes, ...this.brushes];

        const regionJSON = JSON.stringify({
            shell: brushToInput(this.shell),
            brushes: allBrushes.map(brushToInput),
        });

        let positions, normals, indices;
        let wasCached = true;
        const cached = wasmResultCache.get(regionJSON);
        if (cached) {
            positions = cached.positions;
            normals = cached.normals;
            indices = cached.indices;
        } else {
            wasCached = false;
            const result = evaluateRegionWasm(regionJSON, WORLD_SCALE);
            positions = result.get_positions();
            normals = result.get_normals();
            indices = result.get_indices();
            result.free();
            if (wasmResultCache.size >= WASM_CACHE_LIMIT) {
                wasmResultCache.delete(wasmResultCache.keys().next().value);
            }
            wasmResultCache.set(regionJSON, { positions, normals, indices });
        }

        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
        geometry.setAttribute('normal', new THREE.Float32BufferAttribute(normals, 3));
        geometry.setIndex(new THREE.BufferAttribute(indices, 1));

        const elapsed = performance.now() - t0;
        const faceIds = buildFaceMap(geometry, [this.shell, ...allBrushes]);
        return { geometry, timeMs: elapsed, faceIds, cached: wasCached };
    }

    // Merge all unbaked brushes into the baked set, then clear the unbaked list.
    // After bake, push/pull operations create new sub-face brushes against the
    // baked geometry instead of mutating individual brushes.
    bake() {
        if (this.brushes.length === 0 && this.bakedBrushes.length === 0) return;

        const bakedCount = this.brushes.length;
        this.totalBakedBrushes += bakedCount;
        this.bakedBrushes.push(...this.brushes);
        this.brushes.length = 0;
        return bakedCount;
    }
}
