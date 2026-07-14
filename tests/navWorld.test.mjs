// Unit tests for the pure nav/collision core (navWorld.js). Run: node tests/navWorld.test.mjs
// No browser/Three.js needed — navWorld.js only depends on WORLD_SCALE.

import { NavWorld, regionSolidAt, AGENT_HEIGHT_CELLS, stairSolidBoxes, platformSolidBox, stairRunStepBoxes } from '../src/game/navWorld.js';
import { WORLD_SCALE } from '../src/core/constants.js';

let pass = 0, fail = 0;
function ok(cond, msg) { if (cond) { pass++; } else { fail++; console.error('  FAIL:', msg); } }
function section(name) { console.log('\n' + name); }

const box = (op, x, y, z, w, h, d) => ({ op, x, y, z, w, h, d });
// A room = shell (add, +1 WT margin all round) minus interior subtracts.
function room(subtracts) {
    let minX = Infinity, minY = Infinity, minZ = Infinity, maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;
    for (const b of subtracts) {
        minX = Math.min(minX, b.x); minY = Math.min(minY, b.y); minZ = Math.min(minZ, b.z);
        maxX = Math.max(maxX, b.x + b.w); maxY = Math.max(maxY, b.y + b.h); maxZ = Math.max(maxZ, b.z + b.d);
    }
    const t = 1;
    const shell = box('add', minX - t, minY - t, minZ - t, (maxX - minX) + 2 * t, (maxY - minY) + 2 * t, (maxZ - minZ) + 2 * t);
    return { shell, bakedBrushes: [], brushes: subtracts };
}

// Replicate bakeNavWorld's voxelization for a synthetic region list (+ extra
// solid boxes, exactly as navGrid.bakeNavWorld does).
function bake(regions, extras = []) {
    let minX = Infinity, minY = Infinity, minZ = Infinity, maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;
    const grow = (x, y, z, w, h, d) => {
        minX = Math.min(minX, x); minY = Math.min(minY, y); minZ = Math.min(minZ, z);
        maxX = Math.max(maxX, x + w); maxY = Math.max(maxY, y + h); maxZ = Math.max(maxZ, z + d);
    };
    for (const r of regions) { const s = r.shell; grow(s.x, s.y, s.z, s.w, s.h, s.d); }
    for (const b of extras) grow(b.x, b.y, b.z, b.w, b.h, b.d);
    const x0 = Math.floor(minX), y0 = Math.floor(minY), z0 = Math.floor(minZ);
    const nx = Math.ceil(maxX) - x0, ny = Math.ceil(maxY) - y0, nz = Math.ceil(maxZ) - z0;
    const solid = new Uint8Array(nx * ny * nz);
    const at = (ix, iy, iz) => (iy * nz + iz) * nx + ix;
    for (let iy = 0; iy < ny; iy++)
        for (let iz = 0; iz < nz; iz++)
            for (let ix = 0; ix < nx; ix++) {
                let s = 0;
                for (const r of regions) if (regionSolidAt(r, x0 + ix + 0.5, y0 + iy + 0.5, z0 + iz + 0.5)) { s = 1; break; }
                solid[at(ix, iy, iz)] = s;
            }
    for (const b of extras)
        for (let iy = 0; iy < ny; iy++)
            for (let iz = 0; iz < nz; iz++)
                for (let ix = 0; ix < nx; ix++) {
                    const cx = x0 + ix + 0.5, cy = y0 + iy + 0.5, cz = z0 + iz + 0.5;
                    if (cx >= b.x && cx < b.x + b.w && cy >= b.y && cy < b.y + b.h && cz >= b.z && cz < b.z + b.d)
                        solid[at(ix, iy, iz)] = 1;
                }
    return new NavWorld(x0, y0, z0, nx, ny, nz, solid);
}

const wtToM = (wt) => wt * WORLD_SCALE;

// ── 1. Membership: room interior is air, walls/floor are solid ──
section('1. CSG membership');
{
    const r = room([box('subtract', 0, 0, 0, 16, 12, 16)]);
    ok(regionSolidAt(r, 8, 6, 8) === false, 'room center is air');
    ok(regionSolidAt(r, 8, -0.5, 8) === true, 'floor below room is solid');
    ok(regionSolidAt(r, -0.5, 6, 8) === true, 'wall (-x) is solid');
    ok(regionSolidAt(r, 8, 12.5, 8) === true, 'ceiling above room is solid');
    ok(regionSolidAt(r, 8, 13.5, 8) === false, 'above the shell is empty (outside)');
    ok(regionSolidAt(r, 100, 6, 8) === false, 'far outside shell is not solid');
}

// ── 2. Standability + collision ──
section('2. standability & collision');
const single = bake([room([box('subtract', 0, 0, 0, 16, 12, 16)])]);
{
    // Floor air cell: room interior floor sits at WT y in [0,12); cell iy with center 0.5.
    const c = single.cellAt(wtToM(8), wtToM(0.5), wtToM(8));
    ok(c !== null, 'cellAt finds a standable floor cell in the room');
    ok(single.isStandable(c.ix, c.iy, c.iz), 'that cell is standable');
    ok(!single.isStandable(c.ix, c.iy - 1, c.iz), 'the floor itself is not standable (it is solid)');
    // Collision in meters: inside room = free, inside wall = blocked.
    ok(single.isSolidMeters(wtToM(8), wtToM(6), wtToM(8)) === false, 'player collision: room center free');
    ok(single.isSolidMeters(wtToM(-0.5), wtToM(6), wtToM(8)) === true, 'player collision: wall blocks');
    ok(AGENT_HEIGHT_CELLS === 6, 'agent height constant sane');
}

// ── 3. Pathfinding across an open room ──
section('3. A* across a room');
{
    const a = single.cellFloorMeters(...Object.values(single.cellAt(wtToM(2), wtToM(0.5), wtToM(2))));
    const b = single.cellFloorMeters(...Object.values(single.cellAt(wtToM(14), wtToM(0.5), wtToM(14))));
    const path = single.findPath(a, b);
    ok(path && path.length > 1, 'path found corner-to-corner');
    // Endpoints should be near the requested cells.
    const last = path[path.length - 1];
    ok(Math.hypot(last.x - b.x, last.z - b.z) < wtToM(1.5), 'path ends near goal');
}

// ── 4. Walls block; doorways connect ──
section('4. walls block, doorways connect');
{
    // Two 8-wide rooms with a solid wall between them (gap in x from 8..12).
    const roomA = box('subtract', 0, 0, 0, 8, 12, 8);
    const roomB = box('subtract', 12, 0, 0, 8, 12, 8);
    const sealed = bake([room([roomA, roomB])]);
    const pa = sealed.cellFloorMeters(...Object.values(sealed.cellAt(wtToM(4), wtToM(0.5), wtToM(4))));
    const pb = sealed.cellFloorMeters(...Object.values(sealed.cellAt(wtToM(16), wtToM(0.5), wtToM(4))));
    ok(sealed.findPath(pa, pb) === null, 'no path between rooms separated by a wall');

    // Add a doorway subtract bridging the wall (x 8..12, standable height).
    const door = box('subtract', 8, 0, 3, 4, 8, 2);
    const connected = bake([room([roomA, roomB, door])]);
    const path = connected.findPath(pa, pb);
    ok(path && path.length > 1, 'path found once a doorway connects the rooms');
}

// ── 5. Stairs: A* climbs 1-WT steps ──
section('5. A* climbs stairs');
{
    // A room whose floor rises one WT per column via stacked subtract steps,
    // mimicking the editor's stair carve (each step raises the floor by 1).
    const steps = [];
    const H = 14;
    for (let i = 0; i < 6; i++) {
        // Air column above step i: floor at y=i, up to ceiling.
        steps.push(box('subtract', 4 + i, i, 4, 1, H - i, 8));
    }
    // A landing at the bottom and top for standing room.
    steps.push(box('subtract', 0, 0, 4, 4, H, 8));       // bottom landing (floor y=0)
    steps.push(box('subtract', 10, 6, 4, 4, H - 6, 8));  // top landing (floor y=6)
    const stairWorld = bake([room(steps)]);
    const bottom = stairWorld.cellFloorMeters(...Object.values(stairWorld.cellAt(wtToM(2), wtToM(0.5), wtToM(8))));
    const topCell = stairWorld.cellAt(wtToM(12), wtToM(6.5), wtToM(8));
    ok(topCell !== null, 'top landing has a standable cell');
    const top = stairWorld.cellFloorMeters(topCell.ix, topCell.iy, topCell.iz);
    const path = stairWorld.findPath(bottom, top);
    ok(path && path.length > 1, 'path found from bottom to top of stairs');
    if (path) {
        const climbed = path[path.length - 1].y - path[0].y;
        ok(climbed > wtToM(4), `path climbs vertically (~${(climbed / WORLD_SCALE).toFixed(1)} WT)`);
    }
}

// ── 6. CSG stairs: reconstructed treads are solid (no walk-through) & climbable ──
section('6. CSG stair solidity');
{
    // UP stair on the +x wall: 5 steps rising 1 WT each, carved into a tall room.
    const desc = { axis: 'x', side: 'max', facePos: 4, selU0: 2, selU1: 6, floor: 0, H: 14, direction: 'up', stepCount: 5 };
    const boxes = stairSolidBoxes(desc);
    ok(boxes.length === 5, `stair produced ${boxes.length} step boxes (expected 5)`);
    ok(boxes[0].h === 1 && boxes[4].h === 5, 'step heights ramp 1..5 WT');

    const w = bake([room([box('subtract', 0, 0, 0, 20, 14, 10)])], boxes);
    // Regression: mid-step volume is SOLID (previously walked through).
    ok(w.isSolidMeters(wtToM(6.5), wtToM(1.5), wtToM(4)) === true, 'inside a step block is solid (no walk-through)');
    // Each tread is standable at its rising height.
    ok(w.cellAt(wtToM(4.5), wtToM(1.5), wtToM(4)) !== null, 'step 0 tread standable');
    ok(w.cellAt(wtToM(8.5), wtToM(5.5), wtToM(4)) !== null, 'step 4 tread standable');
    // A* climbs from the room floor up the stairs.
    const bottom = w.cellFloorMeters(...Object.values(w.cellAt(wtToM(1), wtToM(0.5), wtToM(4))));
    const topCell = w.cellAt(wtToM(8.5), wtToM(5.5), wtToM(4));
    const top = w.cellFloorMeters(topCell.ix, topCell.iy, topCell.iz);
    const path = w.findPath(bottom, top);
    ok(path && path.length > 1, 'A* finds a path up the CSG stairs');
    if (path) ok(path[path.length - 1].y - path[0].y > wtToM(3), 'that path climbs');
}

// ── 7. Platforms: slab is solid and its top is standable ──
section('7. platform solidity');
{
    const p = { x: 2, y: 6, z: 2, sizeX: 4, sizeZ: 4, thickness: 1, grounded: false };
    const b = platformSolidBox(p);
    ok(b && b.y === 5 && b.h === 1, 'platform slab reconstructed just below top surface');
    const w = bake([room([box('subtract', 0, 0, 0, 16, 16, 16)])], [b]);
    ok(w.isSolidMeters(wtToM(4), wtToM(5.5), wtToM(4)) === true, 'platform slab is solid');
    ok(w.cellAt(wtToM(4), wtToM(6.5), wtToM(4)) !== null, 'platform top surface is standable');
    ok(platformSolidBox({ x: 0, y: 4, z: 0, sizeX: 2, sizeZ: 2, thickness: 0, grounded: false }) === null, 'zero-thickness platform has no volume');
}

// ── 8. Line of sight through a wall vs a doorway ──
section('8. line of sight');
{
    // Two rooms split by a wall (x 8..12 solid), doorway at z 3..5.
    const roomA = box('subtract', 0, 0, 0, 8, 12, 8);
    const roomB = box('subtract', 12, 0, 0, 8, 12, 8);
    const door = box('subtract', 8, 0, 3, 4, 8, 2);
    const w = bake([room([roomA, roomB, door])]);
    const eyeA = { x: wtToM(4), y: wtToM(4), z: wtToM(1) };   // in room A, off the door line
    const eyeB = { x: wtToM(16), y: wtToM(4), z: wtToM(1) };  // in room B, behind the wall
    ok(w.losClear(eyeA, eyeB) === false, 'LOS blocked through a solid wall');
    const doorA = { x: wtToM(4), y: wtToM(3), z: wtToM(4) };  // aligned with doorway
    const doorB = { x: wtToM(16), y: wtToM(3), z: wtToM(4) };
    ok(w.losClear(doorA, doorB) === true, 'LOS clear straight through the doorway');
    ok(w.losClear(eyeA, { x: wtToM(6), y: wtToM(4), z: wtToM(4) }) === true, 'LOS clear within the same room');
}

// ── 9. StairRun (platform-connect flight): solid steps & climbable ──
section('9. StairRun solidity');
{
    // A flight rising from ground (y=0) to a platform edge (y=6) along +x.
    const params = { runAxis: 'x', topRun: 10, stepRun: -10 / 6, steps: 6, stepRise: 1, stairBaseY: 0, floorY: 0, perpMin: 2, perpMax: 6 };
    const boxes = stairRunStepBoxes(params);
    ok(boxes.length === 6, `stair run produced ${boxes.length} step boxes (expected 6)`);
    ok(Math.abs(boxes[0].h - 1) < 1e-6, 'bottom step is 1 WT tall');
    ok(Math.abs(boxes[5].h - 6) < 1e-6, 'top step is 6 WT tall (reaches platform height)');

    const w = bake([room([box('subtract', -2, 0, 0, 14, 16, 8)])], boxes);
    ok(w.isSolidMeters(wtToM(9), wtToM(3), wtToM(4)) === true, 'inside a stair-run step is solid (no walk-through)');
    ok(w.cellAt(wtToM(9), wtToM(6.5), wtToM(4)) !== null, 'top tread is standable');
    const bottom = w.cellFloorMeters(...Object.values(w.cellAt(wtToM(-1), wtToM(0.5), wtToM(4))));
    const topCell = w.cellAt(wtToM(9), wtToM(6.5), wtToM(4));
    const top = w.cellFloorMeters(topCell.ix, topCell.iy, topCell.iz);
    const path = w.findPath(bottom, top);
    ok(path && path.length > 1, 'A* finds a path up the stair run');
    if (path) ok(path[path.length - 1].y - path[0].y > wtToM(4), 'that path climbs to platform height');
}

// ── 10. Breakable door overlay: high-cost detour, breach opens it ──
section('10. door cost overlay');
{
    // Two rooms joined by TWO doorways: a "long" one (far in z) and a "short"
    // one. Put a door on the short doorway; A* should detour to the open one.
    const roomA = box('subtract', 0, 0, 0, 8, 10, 12);
    const roomB = box('subtract', 12, 0, 0, 8, 10, 12);
    const shortDoor = box('subtract', 8, 0, 1, 4, 8, 2);   // z 1..3
    const openDoor = box('subtract', 8, 0, 9, 4, 8, 2);    // z 9..11
    const w = bake([room([roomA, roomB, shortDoor, openDoor])]);

    // Overlay a door on the short doorway cells (z 1..3, x 8..12).
    const doorGrid = new Uint16Array(w.nx * w.ny * w.nz);
    const doorRec = { id: 1, broken: false };
    for (let iy = 0; iy < w.ny; iy++)
        for (let iz = 0; iz < w.nz; iz++)
            for (let ix = 0; ix < w.nx; ix++) {
                const cx = w.x0 + ix + 0.5, cz = w.z0 + iz + 0.5;
                if (cx >= 8 && cx < 12 && cz >= 1 && cz < 3) doorGrid[w.idx(ix, iy, iz)] = 1;
            }
    w.setDoors([doorRec], doorGrid);

    const a = w.cellFloorMeters(...Object.values(w.cellAt(wtToM(4), wtToM(0.5), wtToM(2))));   // room A near short door
    const b = w.cellFloorMeters(...Object.values(w.cellAt(wtToM(16), wtToM(0.5), wtToM(2))));  // room B near short door

    // doorBlocking detects the intact door on the direct segment.
    ok(w.doorBlocking(a, b) === doorRec, 'doorBlocking finds the intact door on the direct route');

    // With the door intact, A* should detour via the open doorway (z ~10),
    // so the path visits high-z cells rather than squeezing through z 1..3.
    const pathClosed = w.findPath(a, b);
    ok(pathClosed && pathClosed.some(p => p.z > wtToM(8)), 'A* detours to the open doorway while the door is intact');

    // Breach it: A* now takes the short route (no high-z detour needed).
    doorRec.broken = true;
    const pathOpen = w.findPath(a, b);
    ok(pathOpen && !pathOpen.some(p => p.z > wtToM(6)), 'A* uses the short route once the door is breached');
    ok(w.doorBlocking(a, b) === null, 'a breached door no longer blocks');
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
