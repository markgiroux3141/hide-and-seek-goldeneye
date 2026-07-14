// Platform geometry builder — generates solid rectangular slab geometry

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { Platform } from '../core/Platform.js';

const S = WORLD_SCALE;

// ============================================================
// GEOMETRY BUILDER
// ============================================================
export class PlatformGeometryBuilder {
    constructor() {
        this.positions = [];
        this.normals = [];
        this.uvs = [];
        this.colors = [];
        this.indices = [];
        this.zones = [];
        this.vertexCount = 0;
        this.quadCount = 0;
    }

    addQuad(p0, p1, p2, p3, flip = false, zone = 0, uv0, uv1, uv2, uv3) {
        const base = this.vertexCount;
        const [vp1, vp3] = flip ? [p3, p1] : [p1, p3];

        this.positions.push(
            p0[0]*S, p0[1]*S, p0[2]*S, vp1[0]*S, vp1[1]*S, vp1[2]*S,
            p2[0]*S, p2[1]*S, p2[2]*S, vp3[0]*S, vp3[1]*S, vp3[2]*S,
        );

        if (uv0 !== undefined) {
            const [vuv1, vuv3] = flip ? [uv3, uv1] : [uv1, uv3];
            this.uvs.push(uv0[0], uv0[1], vuv1[0], vuv1[1], uv2[0], uv2[1], vuv3[0], vuv3[1]);
        } else {
            if (flip) {
                this.uvs.push(0, 0, 0, 1, 1, 1, 1, 0);
            } else {
                this.uvs.push(0, 0, 1, 0, 1, 1, 0, 1);
            }
        }

        const e1x = vp1[0] - p0[0], e1y = vp1[1] - p0[1], e1z = vp1[2] - p0[2];
        const e2x = p2[0] - p0[0], e2y = p2[1] - p0[1], e2z = p2[2] - p0[2];
        let nx = e1y * e2z - e1z * e2y;
        let ny = e1z * e2x - e1x * e2z;
        let nz = e1x * e2y - e1y * e2x;
        const len = Math.sqrt(nx * nx + ny * ny + nz * nz);
        if (len > 0) { nx /= len; ny /= len; nz /= len; }
        for (let i = 0; i < 4; i++) this.normals.push(nx, ny, nz);
        for (let i = 0; i < 4; i++) this.colors.push(1.0, 1.0, 1.0);

        this.indices.push(base, base + 1, base + 2, base, base + 2, base + 3);
        this.zones.push(zone);
        this.vertexCount += 4;
        this.quadCount++;
    }

    build() {
        const geo = new THREE.BufferGeometry();
        geo.setAttribute('position', new THREE.Float32BufferAttribute(this.positions, 3));
        geo.setAttribute('normal', new THREE.Float32BufferAttribute(this.normals, 3));
        geo.setAttribute('uv', new THREE.Float32BufferAttribute(this.uvs, 2));
        geo.setAttribute('color', new THREE.Float32BufferAttribute(this.colors, 3));

        const uniqueZones = new Set(this.zones);
        if (uniqueZones.size <= 1) {
            geo.setIndex(this.indices);
            if (uniqueZones.size === 1) {
                geo.addGroup(0, this.indices.length, this.zones[0]);
            }
            return geo;
        }

        const quads = this.zones.map((z, i) => ({ idx: i, zone: z }));
        quads.sort((a, b) => a.zone - b.zone);

        const newIndices = [];
        for (const q of quads) {
            const srcIdx = q.idx * 6;
            for (let j = 0; j < 6; j++) newIndices.push(this.indices[srcIdx + j]);
        }

        geo.setIndex(newIndices);

        let groupStart = 0;
        let currentZone = quads[0].zone;
        let groupCount = 0;

        for (const q of quads) {
            if (q.zone !== currentZone) {
                geo.addGroup(groupStart, groupCount, currentZone);
                groupStart += groupCount;
                groupCount = 0;
                currentZone = q.zone;
            }
            groupCount += 6;
        }
        geo.addGroup(groupStart, groupCount, currentZone);

        return geo;
    }
}

// ============================================================
// PLATFORM GEOMETRY
// ============================================================

// Texture zones (matching volume material array indices):
//   0 = grey_tile_floor (top surface / tread)
//   3 = brown_wall (sides, bottom)

export function buildBoxPlatformGeometry(platform, options = {}) {
    const builder = new PlatformGeometryBuilder();

    const { x, y, z, sizeX, sizeZ, thickness, grounded } = platform;
    const floorY = grounded ? findFloorYAt(x + sizeX / 2, z + sizeZ / 2, y, options.brushes) : (y - thickness);
    const effectiveThickness = y - floorY;
    const xMin = x;
    const xMax = x + sizeX;
    const zMin = z;
    const zMax = z + sizeZ;
    const yTop = y;
    const yBot = floorY;

    const textured = options.viewMode === 'textured';
    const treadZone = textured ? 0 : 0;
    const sideZone = textured ? 3 : 0;
    const h = effectiveThickness;
    const w = sizeX;
    const d = sizeZ;

    // Top face (+Y) — tread texture
    builder.addQuad(
        [xMin, yTop, zMin],
        [xMin, yTop, zMax],
        [xMax, yTop, zMax],
        [xMax, yTop, zMin],
        false, treadZone,
        ...(textured ? [[0, 0], [0, d], [w, d], [w, 0]] : []),
    );

    // Bottom face (-Y)
    builder.addQuad(
        [xMin, yBot, zMin],
        [xMax, yBot, zMin],
        [xMax, yBot, zMax],
        [xMin, yBot, zMax],
        false, sideZone,
        ...(textured ? [[0, 0], [w, 0], [w, d], [0, d]] : []),
    );

    // Front face (-Z side, normal -Z)
    builder.addQuad(
        [xMin, yBot, zMin],
        [xMin, yTop, zMin],
        [xMax, yTop, zMin],
        [xMax, yBot, zMin],
        false, sideZone,
        ...(textured ? [[0, 0], [0, h], [w, h], [w, 0]] : []),
    );

    // Back face (+Z side, normal +Z)
    builder.addQuad(
        [xMax, yBot, zMax],
        [xMax, yTop, zMax],
        [xMin, yTop, zMax],
        [xMin, yBot, zMax],
        false, sideZone,
        ...(textured ? [[0, 0], [0, h], [w, h], [w, 0]] : []),
    );

    // Left face (-X side, normal -X)
    builder.addQuad(
        [xMin, yBot, zMax],
        [xMin, yTop, zMax],
        [xMin, yTop, zMin],
        [xMin, yBot, zMin],
        false, sideZone,
        ...(textured ? [[0, 0], [0, h], [d, h], [d, 0]] : []),
    );

    // Right face (+X side, normal +X)
    builder.addQuad(
        [xMax, yBot, zMin],
        [xMax, yTop, zMin],
        [xMax, yTop, zMax],
        [xMax, yBot, zMax],
        false, sideZone,
        ...(textured ? [[0, 0], [0, h], [d, h], [d, 0]] : []),
    );

    return builder.build();
}

// ============================================================
// STAIR RUN GEOMETRY (connecting two platforms or ground)
// ============================================================

export function toWorld(runAxis, runPos, y, perpPos) {
    if (runAxis === 'x') return [runPos, y, perpPos];
    return [perpPos, y, runPos];
}

/**
 * Build geometry for a stair run connecting two platforms (or ground).
 * @param {StairRun} stairRun - the stair run data
 * @param {Platform|null} fromPlatform - source platform (null = ground)
 * @param {Platform|null} toPlatform - destination platform (null = ground)
 * @param {object} options - { viewMode: 'grid'|'textured' }
 */
export function buildBoxStairGeometry(stairRun, fromPlatform, toPlatform, options = {}) {
    const builder = new PlatformGeometryBuilder();

    // Resolve anchor points
    const fromPt = resolveStairAnchor(fromPlatform, stairRun.anchorFrom);
    const toPt = resolveStairAnchor(toPlatform, stairRun.anchorTo);

    // Determine which is top and bottom
    const topPt = fromPt.y >= toPt.y ? fromPt : toPt;
    const bottomPt = fromPt.y >= toPt.y ? toPt : fromPt;
    const topPlatform = fromPt.y >= toPt.y ? fromPlatform : toPlatform;
    const bottomPlatform = fromPt.y >= toPt.y ? toPlatform : fromPlatform;
    const topAnchor = fromPt.y >= toPt.y ? stairRun.anchorFrom : stairRun.anchorTo;
    const bottomAnchor = fromPt.y >= toPt.y ? stairRun.anchorTo : stairRun.anchorFrom;

    const rise = topPt.y - bottomPt.y;
    if (rise === 0) {
        // Flat walkway — just build a flat platform-like connection
        // (Could be useful for bridges/catwalks)
        return builder.build();
    }

    // Determine run axis from platform edges
    const { runAxis, runSign } = computeStairRunAxis(topPlatform, topAnchor, bottomPlatform, bottomAnchor, topPt, bottomPt);

    const topRun = runAxis === 'x' ? topPt.x : topPt.z;
    const bottomRun = runAxis === 'x' ? bottomPt.x : bottomPt.z;

    // Compute perpendicular extent (auto-centered)
    const halfWidth = stairRun.width / 2;
    const topPerp = runAxis === 'x' ? topPt.z : topPt.x;
    const perpMin = topPerp - halfWidth;
    const perpMax = topPerp + halfWidth;

    const steps = Math.max(1, Math.round(rise / stairRun.stepHeight));
    const totalRun = bottomRun - topRun;
    const stepRise = rise / steps;
    const stepRun = totalRun / steps;
    const stairBaseY = bottomPt.y;                          // where steps start (unchanged)
    const floorY = stairRun.grounded
        ? findFloorYAt(bottomPt.x, bottomPt.z, bottomPt.y, options.brushes)
        : bottomPt.y;

    const xorFlip = (runAxis === 'x') !== (stepRun < 0);

    const textured = options.viewMode === 'textured';
    const treadZone = textured ? 0 : 0;
    const riserZone = textured ? 5 : 0;
    const sideZone = textured ? 3 : 0;
    const stepWidth = perpMax - perpMin;

    // Build steps
    for (let i = 0; i < steps; i++) {
        const rFront = topRun + (steps - i) * stepRun;
        const rBack = topRun + (steps - i - 1) * stepRun;
        const stepTopY = stairBaseY + (i + 1) * stepRise;
        const stepBotY = stairBaseY + i * stepRise;
        const absStepRun = Math.abs(stepRun);
        const sideH = stepTopY - floorY;

        // Tread (+Y)
        builder.addQuad(
            toWorld(runAxis, rBack, stepTopY, perpMin),
            toWorld(runAxis, rFront, stepTopY, perpMin),
            toWorld(runAxis, rFront, stepTopY, perpMax),
            toWorld(runAxis, rBack, stepTopY, perpMax),
            xorFlip, treadZone,
            ...(textured ? [[0, 0], [absStepRun, 0], [absStepRun, stepWidth], [0, stepWidth]] : []),
        );

        // Riser
        const riserU = stepWidth / stepRise;
        builder.addQuad(
            toWorld(runAxis, rFront, stepBotY, perpMin),
            toWorld(runAxis, rFront, stepBotY, perpMax),
            toWorld(runAxis, rFront, stepTopY, perpMax),
            toWorld(runAxis, rFront, stepTopY, perpMin),
            xorFlip, riserZone,
            ...(textured ? [[0, 0], [riserU, 0], [riserU, 1], [0, 1]] : []),
        );

        // Left side (perpMin)
        const uOff = i * absStepRun;
        builder.addQuad(
            toWorld(runAxis, rFront, floorY, perpMin),
            toWorld(runAxis, rBack, floorY, perpMin),
            toWorld(runAxis, rBack, stepTopY, perpMin),
            toWorld(runAxis, rFront, stepTopY, perpMin),
            !xorFlip, sideZone,
            ...(textured ? [[uOff, 0], [uOff + absStepRun, 0], [uOff + absStepRun, sideH], [uOff, sideH]] : []),
        );

        // Right side (perpMax)
        builder.addQuad(
            toWorld(runAxis, rBack, floorY, perpMax),
            toWorld(runAxis, rFront, floorY, perpMax),
            toWorld(runAxis, rFront, stepTopY, perpMax),
            toWorld(runAxis, rBack, stepTopY, perpMax),
            !xorFlip, sideZone,
            ...(textured ? [[uOff, 0], [uOff + absStepRun, 0], [uOff + absStepRun, sideH], [uOff, sideH]] : []),
        );
    }

    // Bottom face
    const runMin = Math.min(topRun, topRun + steps * stepRun);
    const runMax = Math.max(topRun, topRun + steps * stepRun);
    const runLen = runMax - runMin;
    builder.addQuad(
        toWorld(runAxis, runMin, floorY, perpMin),
        toWorld(runAxis, runMax, floorY, perpMin),
        toWorld(runAxis, runMax, floorY, perpMax),
        toWorld(runAxis, runMin, floorY, perpMax),
        runAxis === 'z', sideZone,
        ...(textured ? [[0, 0], [runLen, 0], [runLen, stepWidth], [0, stepWidth]] : []),
    );

    // Back face (at top)
    const backH = topPt.y - floorY;
    builder.addQuad(
        toWorld(runAxis, topRun, floorY, perpMin),
        toWorld(runAxis, topRun, floorY, perpMax),
        toWorld(runAxis, topRun, topPt.y, perpMax),
        toWorld(runAxis, topRun, topPt.y, perpMin),
        !xorFlip, sideZone,
        ...(textured ? [[0, 0], [stepWidth, 0], [stepWidth, backH], [0, backH]] : []),
    );

    // Front face (at bottom, if there's a gap below the first step)
    if (floorY < stairBaseY) {
        const frontRun = topRun + steps * stepRun;
        const frontH = stairBaseY - floorY;
        builder.addQuad(
            toWorld(runAxis, frontRun, floorY, perpMax),
            toWorld(runAxis, frontRun, floorY, perpMin),
            toWorld(runAxis, frontRun, stairBaseY, perpMin),
            toWorld(runAxis, frontRun, stairBaseY, perpMax),
            !xorFlip, sideZone,
            ...(textured ? [[0, 0], [stepWidth, 0], [stepWidth, frontH], [0, frontH]] : []),
        );
    }

    return builder.build();
}

// Resolve anchor to world-space point in WT units
export function resolveStairAnchor(platform, anchor) {
    if (!platform) {
        return { x: anchor.x, y: anchor.y ?? 0, z: anchor.z };
    }
    // Use offset if provided (0..1 along edge), otherwise center (0.5)
    const t = anchor.offset != null ? anchor.offset : 0.5;
    const pt = platform.getEdgePointAtOffset(anchor.edge, t);
    return { x: pt.x, y: platform.y, z: pt.z };
}

// Determine run axis from platform edges or positions
export function computeStairRunAxis(topPlatform, topAnchor, bottomPlatform, bottomAnchor, topPt, bottomPt) {
    // If top platform has an edge, use its normal direction
    if (topPlatform && topAnchor.edge) {
        const normal = Platform.edgeNormal(topAnchor.edge);
        return {
            runAxis: normal.x !== 0 ? 'x' : 'z',
            runSign: normal.x !== 0 ? normal.x : normal.z,
        };
    }
    // If bottom platform has an edge, use reversed normal
    if (bottomPlatform && bottomAnchor.edge) {
        const normal = Platform.edgeNormal(bottomAnchor.edge);
        return {
            runAxis: normal.x !== 0 ? 'x' : 'z',
            runSign: normal.x !== 0 ? -normal.x : -normal.z,
        };
    }
    // Fall back to dominant axis
    const dx = Math.abs(bottomPt.x - topPt.x);
    const dz = Math.abs(bottomPt.z - topPt.z);
    const runAxis = dx >= dz ? 'x' : 'z';
    const runSign = (runAxis === 'x' ? bottomPt.x - topPt.x : bottomPt.z - topPt.z) >= 0 ? 1 : -1;
    return { runAxis, runSign };
}

// Build stair run preview lines (wireframe)
export function buildStairRunPreviewLines(fromPt, toPt, width, stepHeight, riseOverRun) {
    const topPt = fromPt.y >= toPt.y ? fromPt : toPt;
    const bottomPt = fromPt.y >= toPt.y ? toPt : fromPt;

    const rise = topPt.y - bottomPt.y;
    if (rise === 0) return [];

    const dx = Math.abs(bottomPt.x - topPt.x);
    const dz = Math.abs(bottomPt.z - topPt.z);
    const runAxis = dx >= dz ? 'x' : 'z';
    const topRun = runAxis === 'x' ? topPt.x : topPt.z;
    const bottomRun = runAxis === 'x' ? bottomPt.x : bottomPt.z;

    const topPerp = runAxis === 'x' ? topPt.z : topPt.x;
    const halfW = width / 2;
    const perpMin = topPerp - halfW;
    const perpMax = topPerp + halfW;

    const steps = Math.max(1, Math.round(rise / stepHeight));
    const stepRunLen = (bottomRun - topRun) / steps;
    const stepRise = rise / steps;

    const pts = [];
    function addLine(ax, ay, az, bx, by, bz) {
        pts.push(ax * S, ay * S, az * S, bx * S, by * S, bz * S);
    }

    for (let i = 0; i < steps; i++) {
        const rBack = topRun + (steps - i - 1) * stepRunLen;
        const rFront = topRun + (steps - i) * stepRunLen;
        const stepTopY = bottomPt.y + (i + 1) * stepRise;

        // Tread outline (top edge)
        if (runAxis === 'x') {
            addLine(rBack, stepTopY, perpMin, rFront, stepTopY, perpMin);
            addLine(rFront, stepTopY, perpMin, rFront, stepTopY, perpMax);
            addLine(rFront, stepTopY, perpMax, rBack, stepTopY, perpMax);
            addLine(rBack, stepTopY, perpMax, rBack, stepTopY, perpMin);
        } else {
            addLine(perpMin, stepTopY, rBack, perpMin, stepTopY, rFront);
            addLine(perpMin, stepTopY, rFront, perpMax, stepTopY, rFront);
            addLine(perpMax, stepTopY, rFront, perpMax, stepTopY, rBack);
            addLine(perpMax, stepTopY, rBack, perpMin, stepTopY, rBack);
        }
    }

    return pts;
}

// ============================================================
// RAILING GEOMETRY
// ============================================================

const RAILING_HEIGHT = 3.0;     // height above surface in WT
const HANDRAIL_DEPTH = 0.2;     // perpendicular handrail strip depth in WT

// Highest CSG room floor at (x, z) strictly below `aboveY` (all in WT units).
// Used by "grounded" platforms/stairs to extend walls down to the visible floor
// beneath them, instead of the hardcoded world ground at Y=0. Returns 0 when no
// subtract brush covers that XZ, preserving the old behavior for non-CSG levels.
export function findFloorYAt(x, z, aboveY, brushes) {
    if (!brushes || brushes.length === 0) return 0;
    let bestY = 0;
    let found = false;
    for (const b of brushes) {
        if (b.op !== 'subtract') continue;
        if (x < b.x || x > b.x + b.w) continue;
        if (z < b.z || z > b.z + b.d) continue;
        if (b.y >= aboveY) continue;
        if (!found || b.y > bestY) { bestY = b.y; found = true; }
    }
    return found ? bestY : 0;
}

// Returns true if a (WT-space) point is inside any subtractive brush's interior.
// Doorframes/holeframes are included so railings stay across doorways
// (the doorframe brush carves a tunnel, so the point past the wall is "inside").
function pointInAnySubtractBrush(brushes, x, y, z) {
    for (const brush of brushes) {
        if (brush.op !== 'subtract') continue;
        if (x > brush.x && x < brush.x + brush.w &&
            y > brush.y && y < brush.y + brush.h &&
            z > brush.z && z < brush.z + brush.d) {
            return true;
        }
    }
    return false;
}

// Check if a platform edge is blocked by a CSG wall.
// The edge is "against wall" if every probe point a short distance past the
// edge falls outside all subtractive brushes (i.e., into solid shell material).
// If any probe point hits open air, the edge has at least one open span and
// railings should be drawn — matching the old all-or-nothing semantics.
//
// Optional `opts.probeDist` (default 0.5 WT) controls how far past the edge
// the probe extends — useful when the caller wants to detect walls that are
// near but not flush against the edge.
// Optional `opts.yProbe` (default platform.y - 0.5) overrides the Y at which
// to probe — useful for checks below the slab (e.g. corner pillars).
export function isEdgeAgainstWall(platform, edge, brushes, opts = {}) {
    if (!brushes || brushes.length === 0) return false;

    const PROBE_DIST = opts.probeDist ?? 0.5;
    const yProbe = opts.yProbe ?? (platform.y - 0.5);

    let edgePos, edgeMin, edgeMax;
    let probeAxis;     // 'x' or 'z'
    let probeSign;     // -1 = probe in -axis dir, +1 = +axis dir

    if (edge === 'xMin')      { edgePos = platform.x;    edgeMin = platform.z; edgeMax = platform.maxZ; probeAxis = 'x'; probeSign = -1; }
    else if (edge === 'xMax') { edgePos = platform.maxX; edgeMin = platform.z; edgeMax = platform.maxZ; probeAxis = 'x'; probeSign =  1; }
    else if (edge === 'zMin') { edgePos = platform.z;    edgeMin = platform.x; edgeMax = platform.maxX; probeAxis = 'z'; probeSign = -1; }
    else                      { edgePos = platform.maxZ; edgeMin = platform.x; edgeMax = platform.maxX; probeAxis = 'z'; probeSign =  1; }

    const SAMPLES = 5;
    for (let i = 0; i < SAMPLES; i++) {
        const t = (i + 0.5) / SAMPLES;
        const along = edgeMin + t * (edgeMax - edgeMin);
        const px = probeAxis === 'x' ? edgePos + probeSign * PROBE_DIST : along;
        const pz = probeAxis === 'z' ? edgePos + probeSign * PROBE_DIST : along;
        if (pointInAnySubtractBrush(brushes, px, yProbe, pz)) {
            return false; // found open air past edge → not fully walled
        }
    }
    return true; // all samples sit in solid material → wall covers the edge
}

// Get the ranges along an edge (0..1) that are occupied by stair run widths
function getStairOccupiedRanges(platform, edge, stairRuns) {
    const ranges = [];
    const edgeLen = platform.getEdgeLength(edge);

    for (const run of stairRuns) {
        let anchor = null;
        if (run.fromPlatformId === platform.id && run.anchorFrom.edge === edge) anchor = run.anchorFrom;
        if (run.toPlatformId === platform.id && run.anchorTo.edge === edge) anchor = run.anchorTo;
        if (!anchor) continue;

        // The stair is centered at the anchor point along the edge
        const offset = anchor.offset != null ? anchor.offset : 0.5;
        const halfW = (run.width / 2) / edgeLen; // half-width as fraction of edge length
        const lo = Math.max(0, offset - halfW);
        const hi = Math.min(1, offset + halfW);
        ranges.push([lo, hi]);
    }

    // Sort by start and merge overlapping ranges
    ranges.sort((a, b) => a[0] - b[0]);
    const merged = [];
    for (const r of ranges) {
        if (merged.length > 0 && r[0] <= merged[merged.length - 1][1] + 0.001) {
            merged[merged.length - 1][1] = Math.max(merged[merged.length - 1][1], r[1]);
        } else {
            merged.push([...r]);
        }
    }
    return merged;
}

// Get ranges along an edge (0..1) occupied by adjacent platforms at the same height
function getAdjacentPlatformOccupiedRanges(platform, edge, allPlatforms) {
    const ranges = [];
    const edgeLen = platform.getEdgeLength(edge);
    if (edgeLen < 0.001) return ranges;

    for (const other of allPlatforms) {
        if (other.id === platform.id) continue;
        if (Math.abs(other.y - platform.y) > 0.01) continue; // different height

        let overlap = null;
        if (edge === 'xMin' && Math.abs(other.maxX - platform.x) < 0.01) {
            // other's xMax touches our xMin — both run along Z
            const lo = Math.max(platform.z, other.z);
            const hi = Math.min(platform.maxZ, other.maxZ);
            if (hi > lo + 0.001) overlap = [(lo - platform.z) / edgeLen, (hi - platform.z) / edgeLen];
        } else if (edge === 'xMax' && Math.abs(other.x - platform.maxX) < 0.01) {
            const lo = Math.max(platform.z, other.z);
            const hi = Math.min(platform.maxZ, other.maxZ);
            if (hi > lo + 0.001) overlap = [(lo - platform.z) / edgeLen, (hi - platform.z) / edgeLen];
        } else if (edge === 'zMin' && Math.abs(other.maxZ - platform.z) < 0.01) {
            const lo = Math.max(platform.x, other.x);
            const hi = Math.min(platform.maxX, other.maxX);
            if (hi > lo + 0.001) overlap = [(lo - platform.x) / edgeLen, (hi - platform.x) / edgeLen];
        } else if (edge === 'zMax' && Math.abs(other.z - platform.maxZ) < 0.01) {
            const lo = Math.max(platform.x, other.x);
            const hi = Math.min(platform.maxX, other.maxX);
            if (hi > lo + 0.001) overlap = [(lo - platform.x) / edgeLen, (hi - platform.x) / edgeLen];
        }
        if (overlap) ranges.push(overlap);
    }
    return ranges;
}

// Get the free (unoccupied) segments of an edge as t-ranges in [0,1]
function getFreeEdgeSegments(platform, edge, stairRuns, allPlatforms) {
    const occupied = [
        ...getStairOccupiedRanges(platform, edge, stairRuns),
        ...getAdjacentPlatformOccupiedRanges(platform, edge, allPlatforms),
    ];
    // Sort by start and merge overlapping ranges
    occupied.sort((a, b) => a[0] - b[0]);
    const merged = [];
    for (const r of occupied) {
        if (merged.length > 0 && r[0] <= merged[merged.length - 1][1] + 0.001) {
            merged[merged.length - 1][1] = Math.max(merged[merged.length - 1][1], r[1]);
        } else {
            merged.push([...r]);
        }
    }
    const free = [];
    let cursor = 0;
    for (const [lo, hi] of merged) {
        if (lo > cursor + 0.001) free.push([cursor, lo]);
        cursor = hi;
    }
    if (cursor < 1 - 0.001) free.push([cursor, 1]);
    return free;
}

/**
 * Build railing geometry for a platform's exposed edges.
 * Returns a BufferGeometry with simple quads (side plane + handrail).
 * Railings are added to free segments of each edge (not blocked by walls or stairs).
 * `brushes` is `state.csg.brushes` — used for wall-proximity checks.
 */
export function buildPlatformRailingGeometry(platform, stairRuns, brushes, allPlatforms = []) {
    const builder = new PlatformGeometryBuilder();
    const yTop = platform.y;
    const railTop = yTop + RAILING_HEIGHT;

    const edges = ['xMin', 'xMax', 'zMin', 'zMax'];
    for (const edge of edges) {
        if (isEdgeAgainstWall(platform, edge, brushes)) continue;

        const line = platform.getEdgeLine(edge);
        const edgeNorm = Platform.edgeNormal(edge);
        const edgeLen = platform.getEdgeLength(edge);
        const freeSegments = getFreeEdgeSegments(platform, edge, stairRuns, allPlatforms);

        for (const [tStart, tEnd] of freeSegments) {
            const segLen = (tEnd - tStart) * edgeLen;
            if (segLen < 0.1) continue; // skip tiny slivers

            // Interpolate start/end points along the edge
            const x0 = line.start.x + (line.end.x - line.start.x) * tStart;
            const z0 = line.start.z + (line.end.z - line.start.z) * tStart;
            const x1 = line.start.x + (line.end.x - line.start.x) * tEnd;
            const z1 = line.start.z + (line.end.z - line.start.z) * tEnd;

            const uTiles = segLen / 1.5;
            builder.addQuad(
                [x0, yTop, z0],
                [x1, yTop, z1],
                [x1, railTop, z1],
                [x0, railTop, z0],
                false, 0,
                [0, 0], [uTiles, 0], [uTiles, 1], [0, 1],
            );

            // Handrail plane
            const dx = edgeNorm.x * HANDRAIL_DEPTH;
            const dz = edgeNorm.z * HANDRAIL_DEPTH;
            builder.addQuad(
                [x0, railTop, z0],
                [x1, railTop, z1],
                [x1 + dx, railTop, z1 + dz],
                [x0 + dx, railTop, z0 + dz],
                true, 0,
                [0, 0.95], [uTiles, 0.95], [uTiles, 1.0], [0, 1.0],
            );
        }
    }

    return builder.build();
}

/**
 * Build railing geometry for a stair run (left and right side slopes).
 * `brushes` is `state.csg.brushes` — used for wall-proximity checks on each side.
 * Returns a BufferGeometry.
 */
export function buildStairRunRailingGeometry(stairRun, fromPlatform, toPlatform, brushes) {
    const builder = new PlatformGeometryBuilder();

    const fromPt = resolveStairAnchor(fromPlatform, stairRun.anchorFrom);
    const toPt = resolveStairAnchor(toPlatform, stairRun.anchorTo);
    const topPt = fromPt.y >= toPt.y ? fromPt : toPt;
    const bottomPt = fromPt.y >= toPt.y ? toPt : fromPt;
    const topPlatform = fromPt.y >= toPt.y ? fromPlatform : toPlatform;
    const bottomPlatform = fromPt.y >= toPt.y ? toPlatform : fromPlatform;
    const topAnchor = fromPt.y >= toPt.y ? stairRun.anchorFrom : stairRun.anchorTo;
    const bottomAnchor = fromPt.y >= toPt.y ? stairRun.anchorTo : stairRun.anchorFrom;

    const rise = topPt.y - bottomPt.y;
    if (rise === 0) return builder.build();

    const { runAxis, runSign } = computeStairRunAxis(topPlatform, topAnchor, bottomPlatform, bottomAnchor, topPt, bottomPt);

    const topRun = runAxis === 'x' ? topPt.x : topPt.z;
    const bottomRun = runAxis === 'x' ? bottomPt.x : bottomPt.z;
    const halfWidth = stairRun.width / 2;
    const topPerp = runAxis === 'x' ? topPt.z : topPt.x;
    const perpMin = topPerp - halfWidth;
    const perpMax = topPerp + halfWidth;

    const totalRun = Math.abs(bottomRun - topRun);
    const slopeLen = Math.sqrt(totalRun * totalRun + rise * rise);

    // Bottom and top of the railing along the slope
    const botY = bottomPt.y;
    const topY = topPt.y;
    const botRun = bottomRun;
    const topRunPos = topRun;

    // Check each side against walls
    const sides = [
        { perp: perpMin, normalSign: -1 },  // left side
        { perp: perpMax, normalSign: 1 },    // right side
    ];

    const RAILING_INSET = 0.05; // push railings slightly inward to avoid z-fighting

    for (const side of sides) {
        // Brush-aware wall check: probe a few points just past the stair side along
        // its perpendicular axis. If every probe sits in solid material, omit the railing.
        let blocked = brushes && brushes.length > 0;
        if (blocked) {
            const PROBE_DIST = 0.5;
            const SAMPLES = 5;
            for (let i = 0; i < SAMPLES; i++) {
                const t = (i + 0.5) / SAMPLES;
                const runPos = botRun + t * (topRunPos - botRun);
                const yProbe = botY + t * (topY - botY) + 0.5; // a hair above the tread
                const perpProbe = side.perp + side.normalSign * PROBE_DIST;
                const px = runAxis === 'x' ? runPos : perpProbe;
                const pz = runAxis === 'x' ? perpProbe : runPos;
                if (pointInAnySubtractBrush(brushes, px, yProbe, pz)) {
                    blocked = false;
                    break;
                }
            }
        }
        if (blocked) continue;

        // Offset the railing slightly inward to avoid z-fighting with stair side faces
        const insetPerp = side.perp + (-side.normalSign * RAILING_INSET);

        // Side railing plane — sloped from bottom to top
        const p0 = toWorld(runAxis, botRun, botY, insetPerp);
        const p1 = toWorld(runAxis, topRunPos, topY, insetPerp);
        const p2 = toWorld(runAxis, topRunPos, topY + RAILING_HEIGHT, insetPerp);
        const p3 = toWorld(runAxis, botRun, botY + RAILING_HEIGHT, insetPerp);

        const uTiles = slopeLen / 1.5;
        builder.addQuad(p0, p1, p2, p3, side.normalSign > 0, 0,
            [0, 0], [uTiles, 0], [uTiles, 1], [0, 1],
        );

        // Handrail plane — horizontal strip at railing top following slope
        const nx = runAxis === 'x' ? 0 : side.normalSign * HANDRAIL_DEPTH;
        const nz = runAxis === 'x' ? side.normalSign * HANDRAIL_DEPTH : 0;
        const p4 = toWorld(runAxis, botRun, botY + RAILING_HEIGHT, insetPerp);
        const p5 = toWorld(runAxis, topRunPos, topY + RAILING_HEIGHT, insetPerp);
        const p6 = [p5[0] + nx, p5[1], p5[2] + nz];
        const p7 = [p4[0] + nx, p4[1], p4[2] + nz];

        builder.addQuad(p4, p5, p6, p7, side.normalSign < 0, 0,
            [0, 0.95], [uTiles, 0.95], [uTiles, 1.0], [0, 1.0],
        );
    }

    return builder.build();
}

// ============================================================
// PREVIEW LINES (wireframe outline for placement preview)
// ============================================================

export function buildPlatformPreviewLines(x, y, z, sizeX, sizeZ, thickness) {
    // Returns array of point pairs [x,y,z, x,y,z, ...] in world coords (WT * WORLD_SCALE)
    const xMin = x * S;
    const xMax = (x + sizeX) * S;
    const zMin = z * S;
    const zMax = (z + sizeZ) * S;
    const yTop = y * S;
    const yBot = (y - thickness) * S;

    const pts = [];

    // Top rectangle
    pts.push(xMin, yTop, zMin, xMax, yTop, zMin);
    pts.push(xMax, yTop, zMin, xMax, yTop, zMax);
    pts.push(xMax, yTop, zMax, xMin, yTop, zMax);
    pts.push(xMin, yTop, zMax, xMin, yTop, zMin);

    // Bottom rectangle
    pts.push(xMin, yBot, zMin, xMax, yBot, zMin);
    pts.push(xMax, yBot, zMin, xMax, yBot, zMax);
    pts.push(xMax, yBot, zMax, xMin, yBot, zMax);
    pts.push(xMin, yBot, zMax, xMin, yBot, zMin);

    // Vertical edges
    pts.push(xMin, yTop, zMin, xMin, yBot, zMin);
    pts.push(xMax, yTop, zMin, xMax, yBot, zMin);
    pts.push(xMax, yTop, zMax, xMax, yBot, zMax);
    pts.push(xMin, yTop, zMax, xMin, yBot, zMax);

    return pts;
}

// Build wireframe for a platform edge highlight (for stair connection mode)
export function buildEdgeHighlightLines(platform, edge) {
    const line = platform.getEdgeLine(edge);
    const y = platform.y;
    return [
        line.start.x * S, y * S, line.start.z * S,
        line.end.x * S, y * S, line.end.z * S,
    ];
}

// Build wireframe rectangle showing a stair-width slot on a platform edge
// offset: 0..1 along the edge, width: stair width in WT
export function buildEdgeSlotLines(platform, edge, offset, width) {
    const edgeLen = platform.getEdgeLength(edge);
    const halfW = width / 2;

    // Clamp offset so the slot fits within the edge
    const minT = halfW / edgeLen;
    const maxT = 1 - minT;
    const t = Math.max(minT, Math.min(maxT, offset));

    // Get the two endpoints of the slot along the edge
    const tStart = t - halfW / edgeLen;
    const tEnd = t + halfW / edgeLen;
    const pStart = platform.getEdgePointAtOffset(edge, tStart);
    const pEnd = platform.getEdgePointAtOffset(edge, tEnd);

    const yTop = platform.y;
    const yBot = platform.y - platform.thickness;

    const pts = [];
    // Top edge
    pts.push(pStart.x * S, yTop * S, pStart.z * S, pEnd.x * S, yTop * S, pEnd.z * S);
    // Bottom edge
    pts.push(pStart.x * S, yBot * S, pStart.z * S, pEnd.x * S, yBot * S, pEnd.z * S);
    // Left vertical
    pts.push(pStart.x * S, yTop * S, pStart.z * S, pStart.x * S, yBot * S, pStart.z * S);
    // Right vertical
    pts.push(pEnd.x * S, yTop * S, pEnd.z * S, pEnd.x * S, yBot * S, pEnd.z * S);

    return pts;
}
