// navWorld — the pure WT-grid solid/air world: membership, standability, A*
// pathfinding, and collision queries. Dependency-free (only WORLD_SCALE) so it
// can be unit-tested in Node without the Three.js/DOM module graph.

import { WORLD_SCALE } from '../core/constants.js';

// Player/enemy vertical clearance in WT cells (agents ~1.5m tall = 6 * 0.25m).
export const AGENT_HEIGHT_CELLS = 6;
// Max vertical step an agent can climb between adjacent cells (stairs rise 1 WT).
const MAX_STEP = 1;

// meters <-> WT
const mToWT = (m) => m / WORLD_SCALE;
const wtToM = (wt) => wt * WORLD_SCALE;

// Is world point (WT) inside a brush's AABB? Taper ignored (coarse nav is fine).
export function pointInBrush(b, x, y, z) {
    return x >= b.x && x < b.x + b.w &&
           y >= b.y && y < b.y + b.h &&
           z >= b.z && z < b.z + b.d;
}

// Replay CSG membership for one region at a WT point: shell (add) then each
// brush in eval order (baked before unbaked), matching CSGRegion.evaluateBrushes.
export function regionSolidAt(region, x, y, z) {
    let solid = pointInBrush(region.shell, x, y, z);
    if (!solid) return false; // outside shell — this region doesn't cover the point
    for (const b of region.bakedBrushes) {
        if (pointInBrush(b, x, y, z)) solid = (b.op === 'add');
    }
    for (const b of region.brushes) {
        if (pointInBrush(b, x, y, z)) solid = (b.op === 'add');
    }
    return solid;
}

// Reconstruct the solid volume (WT AABBs { x,y,z,w,h,d }) of a CSG stair from
// its descriptor — one block per step, from the void floor up to that step's
// tread. Mirrors buildCsgStairGeometry's step layout so collision matches the
// visible treads. Pure so it can be unit-tested.
export function stairSolidBoxes(desc) {
    const { axis, side, facePos, selU0, selU1, floor, direction, stepCount } = desc;
    const dir = side === 'max' ? 1 : -1;
    const voidFloor = direction === 'down' ? floor - stepCount : floor;
    const boxes = [];
    for (let k = 0; k < stepCount; k++) {
        const nLo = dir === 1 ? facePos + k : facePos - (k + 1);
        const stepTop = direction === 'down' ? floor - k : floor + (k + 1);
        const h = stepTop - voidFloor;
        if (h <= 0) continue;
        if (axis === 'x') boxes.push({ x: nLo, y: voidFloor, z: selU0, w: 1, h, d: selU1 - selU0 });
        else              boxes.push({ x: selU0, y: voidFloor, z: nLo, w: selU1 - selU0, h, d: 1 });
    }
    return boxes;
}

// Reconstruct a StairRun's solid step blocks (WT AABBs) from already-resolved
// run parameters (mirrors buildBoxStairGeometry's step loop). Each step is solid
// from floorY up to its tread. Pure so it can be unit-tested; the caller
// (navGrid) resolves anchors via the editor's own functions to avoid drift.
export function stairRunStepBoxes({ runAxis, topRun, stepRun, steps, stepRise, stairBaseY, floorY, perpMin, perpMax }) {
    const boxes = [];
    for (let i = 0; i < steps; i++) {
        const rFront = topRun + (steps - i) * stepRun;
        const rBack = topRun + (steps - i - 1) * stepRun;
        const runLo = Math.min(rBack, rFront), runHi = Math.max(rBack, rFront);
        const stepTopY = stairBaseY + (i + 1) * stepRise;
        const h = stepTopY - floorY;
        if (h <= 0 || runHi - runLo <= 0) continue;
        if (runAxis === 'x') boxes.push({ x: runLo, y: floorY, z: perpMin, w: runHi - runLo, h, d: perpMax - perpMin });
        else                 boxes.push({ x: perpMin, y: floorY, z: runLo, w: perpMax - perpMin, h, d: runHi - runLo });
    }
    return boxes;
}

// Reconstruct a platform's solid slab AABB (WT) from its descriptor, or null if
// it has no volume. Grounded platforms extend down to Y=0.
export function platformSolidBox(p) {
    const yTop = p.y;
    const yBottom = p.grounded ? 0 : (p.y - p.thickness);
    const h = yTop - yBottom;
    if (h <= 0) return null;
    return { x: p.x, y: yBottom, z: p.z, w: p.sizeX, h, d: p.sizeZ };
}

// A* penalty (in cell-cost units) for routing through an intact door — large
// enough to prefer an open detour, finite so a walled-in player is still
// reachable via breaching. Demonstrates that dynamic obstacles ride on the
// static grid as a live overlay (no re-voxelization needed when a door breaks).
export const DOOR_COST = 25;

export class NavWorld {
    constructor(x0, y0, z0, nx, ny, nz, solid) {
        this.x0 = x0; this.y0 = y0; this.z0 = z0;
        this.nx = nx; this.ny = ny; this.nz = nz;
        this.solid = solid; // Uint8Array, 1 = solid
        this.doors = [];     // plain door records: { id, broken, ... }
        this.doorGrid = null; // Uint16Array cellIdx -> (doorIndex+1), 0 = none
    }

    // Attach a dynamic door overlay. doors[i].broken is read live by A*, so
    // flipping it (when a door is breached) needs no re-bake.
    setDoors(doors, doorGrid) { this.doors = doors; this.doorGrid = doorGrid; }

    _doorAtCellIdx(nk) {
        if (!this.doorGrid) return null;
        const di = this.doorGrid[nk];
        return di ? this.doors[di - 1] : null;
    }

    // Grid cell index for a meters point, or -1 if out of bounds.
    cellIndexMeters(mx, my, mz) {
        const ix = Math.floor(mx / WORLD_SCALE - this.x0);
        const iy = Math.floor(my / WORLD_SCALE - this.y0);
        const iz = Math.floor(mz / WORLD_SCALE - this.z0);
        if (!this.inBounds(ix, iy, iz)) return -1;
        return this.idx(ix, iy, iz);
    }

    // First intact door whose cells the segment from->to passes through, or null.
    doorBlocking(from, to, step = 0.15) {
        if (!this.doorGrid) return null;
        const dx = to.x - from.x, dy = to.y - from.y, dz = to.z - from.z;
        const dist = Math.hypot(dx, dy, dz);
        const n = Math.max(1, Math.ceil(dist / step));
        for (let i = 0; i <= n; i++) {
            const t = i / n;
            const ci = this.cellIndexMeters(from.x + dx * t, from.y + dy * t, from.z + dz * t);
            if (ci < 0) continue;
            const d = this._doorAtCellIdx(ci);
            if (d && !d.broken) return d;
        }
        return null;
    }

    idx(ix, iy, iz) { return (iy * this.nz + iz) * this.nx + ix; }

    inBounds(ix, iy, iz) {
        return ix >= 0 && iy >= 0 && iz >= 0 &&
               ix < this.nx && iy < this.ny && iz < this.nz;
    }

    // Solid at grid cell; out-of-bounds below the world counts as solid ground
    // so agents on the lowest floor still register a floor beneath them.
    isSolidCell(ix, iy, iz) {
        if (!this.inBounds(ix, iy, iz)) return iy < 0; // below world = solid, sides/top = open
        return this.solid[this.idx(ix, iy, iz)] === 1;
    }

    // Solid query in meters (used by player collision).
    isSolidMeters(mx, my, mz) {
        const ix = Math.floor(mToWT(mx) - this.x0);
        const iy = Math.floor(mToWT(my) - this.y0);
        const iz = Math.floor(mToWT(mz) - this.z0);
        return this.isSolidCell(ix, iy, iz);
    }

    // A cell is standable if it's air, the cell below is solid (floor), and
    // there is AGENT_HEIGHT_CELLS of air above for head clearance.
    isStandable(ix, iy, iz) {
        if (this.isSolidCell(ix, iy, iz)) return false;
        if (!this.isSolidCell(ix, iy - 1, iz)) return false;
        for (let h = 1; h < AGENT_HEIGHT_CELLS; h++) {
            if (this.isSolidCell(ix, iy + h, iz)) return false;
        }
        return true;
    }

    // Line-of-sight: true if no solid cell lies between two meters points.
    // Samples interior points (endpoints assumed to be in air).
    losClear(from, to, step = 0.2) {
        const dx = to.x - from.x, dy = to.y - from.y, dz = to.z - from.z;
        const dist = Math.hypot(dx, dy, dz);
        if (dist === 0) return true;
        const n = Math.max(1, Math.ceil(dist / step));
        for (let i = 1; i < n; i++) {
            const t = i / n;
            if (this.isSolidMeters(from.x + dx * t, from.y + dy * t, from.z + dz * t)) return false;
        }
        return true;
    }

    // World meters at the center of a cell's floor (feet position).
    cellFloorMeters(ix, iy, iz) {
        return {
            x: wtToM(this.x0 + ix + 0.5),
            y: wtToM(this.y0 + iy),
            z: wtToM(this.z0 + iz + 0.5),
        };
    }

    // Convert a meters position to the standable cell at/under it. Searches a
    // few cells downward so a spawn point slightly above the floor still snaps.
    cellAt(mx, my, mz) {
        const ix = Math.floor(mToWT(mx) - this.x0);
        const iz = Math.floor(mToWT(mz) - this.z0);
        let iy = Math.floor(mToWT(my) - this.y0);
        for (let dy = 0; dy <= 40; dy++) {
            if (this.isStandable(ix, iy - dy, iz)) return { ix, iy: iy - dy, iz };
        }
        return null;
    }

    // Find the standable cell closest to a meters position (bounded search).
    // Used to place the player/enemies on valid ground.
    nearestStandable(mx, my, mz, maxR = 24) {
        const cx = Math.floor(mToWT(mx) - this.x0);
        const cy = Math.floor(mToWT(my) - this.y0);
        const cz = Math.floor(mToWT(mz) - this.z0);
        let best = null, bestD = Infinity;
        for (let iy = Math.max(0, cy - maxR); iy < Math.min(this.ny, cy + maxR); iy++) {
            for (let iz = Math.max(0, cz - maxR); iz < Math.min(this.nz, cz + maxR); iz++) {
                for (let ix = Math.max(0, cx - maxR); ix < Math.min(this.nx, cx + maxR); ix++) {
                    if (!this.isStandable(ix, iy, iz)) continue;
                    const d = (ix - cx) ** 2 + (iy - cy) ** 2 + (iz - cz) ** 2;
                    if (d < bestD) { bestD = d; best = { ix, iy, iz }; }
                }
            }
        }
        return best;
    }

    // Collect every standable cell (used to place enemies far from the player).
    allStandable() {
        const out = [];
        for (let iy = 0; iy < this.ny; iy++)
            for (let iz = 0; iz < this.nz; iz++)
                for (let ix = 0; ix < this.nx; ix++)
                    if (this.isStandable(ix, iy, iz)) out.push({ ix, iy, iz });
        return out;
    }

    // A* over standable cells. 4-connected in x/z, allowing +/-MAX_STEP in y for
    // stairs. Returns an array of meters waypoints (feet positions) or null.
    findPath(startMeters, goalMeters) {
        const start = this.cellAt(startMeters.x, startMeters.y, startMeters.z)
            || this.nearestStandable(startMeters.x, startMeters.y, startMeters.z);
        const goal = this.cellAt(goalMeters.x, goalMeters.y, goalMeters.z)
            || this.nearestStandable(goalMeters.x, goalMeters.y, goalMeters.z);
        if (!start || !goal) return null;

        const key = (c) => this.idx(c.ix, c.iy, c.iz);
        const goalKey = key(goal);
        const h = (c) => Math.abs(c.ix - goal.ix) + Math.abs(c.iy - goal.iy) + Math.abs(c.iz - goal.iz);

        const open = [{ ...start, g: 0, f: h(start) }];
        const came = new Map();
        const gScore = new Map([[key(start), 0]]);
        const closed = new Set();

        while (open.length) {
            // Pop lowest f (linear scan; grids here are small enough).
            let bi = 0;
            for (let i = 1; i < open.length; i++) if (open[i].f < open[bi].f) bi = i;
            const cur = open.splice(bi, 1)[0];
            const ck = key(cur);
            if (ck === goalKey) return this._reconstruct(came, cur, key);
            if (closed.has(ck)) continue;
            closed.add(ck);

            for (const [dx, dz] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
                for (let dy = -MAX_STEP; dy <= MAX_STEP; dy++) {
                    const nx = cur.ix + dx, ny = cur.iy + dy, nz = cur.iz + dz;
                    if (!this.isStandable(nx, ny, nz)) continue;
                    // Prevent clipping through a wall corner when stepping up/down.
                    if (dy !== 0 && this.isSolidCell(cur.ix, cur.iy + Math.max(0, dy), cur.iz)) continue;
                    const nk = this.idx(nx, ny, nz);
                    if (closed.has(nk)) continue;
                    const door = this._doorAtCellIdx(nk);
                    const doorPenalty = (door && !door.broken) ? DOOR_COST : 0;
                    const tentative = (gScore.get(ck) ?? Infinity) + 1 + (dy !== 0 ? 0.5 : 0) + doorPenalty;
                    if (tentative < (gScore.get(nk) ?? Infinity)) {
                        gScore.set(nk, tentative);
                        came.set(nk, cur);
                        open.push({ ix: nx, iy: ny, iz: nz, g: tentative, f: tentative + (Math.abs(nx - goal.ix) + Math.abs(ny - goal.iy) + Math.abs(nz - goal.iz)) });
                    }
                }
            }
        }
        return null;
    }

    _reconstruct(came, cur, key) {
        const cells = [cur];
        let k = key(cur);
        while (came.has(k)) {
            cur = came.get(k);
            cells.push(cur);
            k = key(cur);
        }
        cells.reverse();
        return cells.map(c => this.cellFloorMeters(c.ix, c.iy, c.iz));
    }
}
