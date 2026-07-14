// navGrid — bakes a NavWorld from the frozen level. The grid/A* logic itself
// lives in navWorld.js (dependency-free, unit-testable); this file pulls the
// live editor state and voxelizes it.
//
// Two solidity sources are combined:
//   1. CSG regions — replayed volumetrically as boxes (rooms, doors, walls,
//      braces, pillars). Reliable for axis-aligned wall volumes.
//   2. "Extra solids" — structures whose visible geometry is a SEPARATE mesh,
//      not part of the CSG brush set: CSG stairs (step treads over a carved
//      void) and platforms (slabs). We reconstruct their solid volume from the
//      descriptors so the player/enemies stand on and are blocked by them.
//
// The level is FROZEN when the hunt starts, so we bake this once.

import { csgRegionMeshes } from '../mesh/csgMesh.js';
import { state } from '../state.js';
import { resolveStairAnchor, computeStairRunAxis } from '../geometry/platformGeometry.js';
import { NavWorld, regionSolidAt, stairSolidBoxes, platformSolidBox, stairRunStepBoxes } from './navWorld.js';

export { NavWorld, AGENT_HEIGHT_CELLS } from './navWorld.js';

// Resolve a StairRun's run parameters exactly as buildBoxStairGeometry does,
// then reconstruct its solid step blocks. Returns [] for degenerate runs.
function stairRunSolids(run) {
    const fromPlat = run.fromPlatformId != null ? state.platforms.find(p => p.id === run.fromPlatformId) : null;
    const toPlat = run.toPlatformId != null ? state.platforms.find(p => p.id === run.toPlatformId) : null;

    const fromPt = resolveStairAnchor(fromPlat, run.anchorFrom);
    const toPt = resolveStairAnchor(toPlat, run.anchorTo);
    const topPt = fromPt.y >= toPt.y ? fromPt : toPt;
    const bottomPt = fromPt.y >= toPt.y ? toPt : fromPt;
    const topPlat = fromPt.y >= toPt.y ? fromPlat : toPlat;
    const bottomPlat = fromPt.y >= toPt.y ? toPlat : fromPlat;
    const topAnchor = fromPt.y >= toPt.y ? run.anchorFrom : run.anchorTo;
    const bottomAnchor = fromPt.y >= toPt.y ? run.anchorTo : run.anchorFrom;

    const rise = topPt.y - bottomPt.y;
    if (rise === 0) return [];

    const { runAxis } = computeStairRunAxis(topPlat, topAnchor, bottomPlat, bottomAnchor, topPt, bottomPt);
    const topRun = runAxis === 'x' ? topPt.x : topPt.z;
    const bottomRun = runAxis === 'x' ? bottomPt.x : bottomPt.z;
    const halfWidth = run.width / 2;
    const topPerp = runAxis === 'x' ? topPt.z : topPt.x;
    const steps = Math.max(1, Math.round(rise / run.stepHeight));

    return stairRunStepBoxes({
        runAxis, topRun,
        stepRun: (bottomRun - topRun) / steps,
        steps,
        stepRise: rise / steps,
        stairBaseY: bottomPt.y,
        floorY: bottomPt.y,
        perpMin: topPerp - halfWidth,
        perpMax: topPerp + halfWidth,
    });
}

// Reconstruct the solid volume (WT AABBs) of geometry that lives outside the
// CSG brush set: CSG stairs, platforms, and stair runs. Boxes are WT AABBs.
function collectExtraSolids() {
    const boxes = [];
    for (const desc of state.csg.csgStairs || []) boxes.push(...stairSolidBoxes(desc));
    for (const p of state.platforms || []) { const b = platformSolidBox(p); if (b) boxes.push(b); }
    for (const run of state.stairRuns || []) boxes.push(...stairRunSolids(run));
    return boxes;
}

// Bake a NavWorld from the current (frozen) level geometry.
export function bakeNavWorld() {
    const regions = [...csgRegionMeshes.values()].map(d => d.region).filter(Boolean);
    const extras = collectExtraSolids();
    if (regions.length === 0 && extras.length === 0) return null;

    // World bounds = union of every region shell AND every extra solid, in WT.
    let minX = Infinity, minY = Infinity, minZ = Infinity;
    let maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;
    const grow = (x, y, z, w, h, d) => {
        minX = Math.min(minX, x); minY = Math.min(minY, y); minZ = Math.min(minZ, z);
        maxX = Math.max(maxX, x + w); maxY = Math.max(maxY, y + h); maxZ = Math.max(maxZ, z + d);
    };
    for (const r of regions) { const s = r.shell; grow(s.x, s.y, s.z, s.w, s.h, s.d); }
    for (const b of extras) grow(b.x, b.y, b.z, b.w, b.h, b.d);

    const x0 = Math.floor(minX), y0 = Math.floor(minY), z0 = Math.floor(minZ);
    const nx = Math.ceil(maxX) - x0;
    const ny = Math.ceil(maxY) - y0;
    const nz = Math.ceil(maxZ) - z0;
    const idx = (ix, iy, iz) => (iy * nz + iz) * nx + ix;

    const solid = new Uint8Array(nx * ny * nz);

    // 1. CSG region volume.
    for (let iy = 0; iy < ny; iy++) {
        const wy = y0 + iy + 0.5;
        for (let iz = 0; iz < nz; iz++) {
            const wz = z0 + iz + 0.5;
            for (let ix = 0; ix < nx; ix++) {
                const wx = x0 + ix + 0.5;
                for (const r of regions) {
                    if (regionSolidAt(r, wx, wy, wz)) { solid[idx(ix, iy, iz)] = 1; break; }
                }
            }
        }
    }

    // 2. Extra solids (stairs, platforms) — mark cells whose center is inside.
    for (const b of extras) {
        const ixLo = Math.max(0, Math.floor(b.x - x0));
        const ixHi = Math.min(nx - 1, Math.ceil(b.x + b.w - x0) - 1);
        const iyLo = Math.max(0, Math.floor(b.y - y0));
        const iyHi = Math.min(ny - 1, Math.ceil(b.y + b.h - y0) - 1);
        const izLo = Math.max(0, Math.floor(b.z - z0));
        const izHi = Math.min(nz - 1, Math.ceil(b.z + b.d - z0) - 1);
        for (let iy = iyLo; iy <= iyHi; iy++)
            for (let iz = izLo; iz <= izHi; iz++)
                for (let ix = ixLo; ix <= ixHi; ix++) {
                    // Cell center must fall within the box (matches nav semantics).
                    const cx = x0 + ix + 0.5, cy = y0 + iy + 0.5, cz = z0 + iz + 0.5;
                    if (cx >= b.x && cx < b.x + b.w && cy >= b.y && cy < b.y + b.h && cz >= b.z && cz < b.z + b.d)
                        solid[idx(ix, iy, iz)] = 1;
                }
    }

    return new NavWorld(x0, y0, z0, nx, ny, nz, solid);
}
