// csgActions — stateful action handlers for the CSG brush system.
// Ported from spike/csg/main.js (lines ~1097-1575 + helper functions).
//
// All actions read/write state.csg and call rebuildAllCSG when geometry changes.
// Selection state lives in state.csg.selectedFace (with regionId added).
//
// Helpers (worldToFaceUV, getFaceUVInfo, getBakedFaceUVInfo, facesMatch) are
// inlined here to keep the action module self-contained.

import * as THREE from 'three';
import { state } from '../state.js';
import { BrushDef } from '../core/BrushDef.js';
import { csgRegionMeshes, rebuildAllCSG, rebuildAffectedRegions, assignBrushToRegion, assignBrushToRegionDirect, removeBrushFromRegion } from '../mesh/csgMesh.js';
import { findRoomBrushes } from '../core/csg/regions.js';
import {
    WORLD_SCALE, WALL_THICKNESS, WALL_SPLIT_V,
    MIN_BRACE_DIM, MAX_BRACE_DIM, MIN_PILLAR_SIZE, MAX_PILLAR_SIZE,
} from '../core/constants.js';
import { TEXTURE_SCHEMES } from '../scene/textureSchemes.js';
import { showMessage } from '../hud/hud.js';
import { CaveDef } from '../core/cave/CaveDef.js';
import { syncCavesForRegion, seedCuboidCave } from '../mesh/caveMesh.js';

// ─── Constants ──────────────────────────────────────────────────────
const PUSH_PULL_STEP = 4;
const HOLE_WIDTH = 3;
const HOLE_HEIGHT = 3;
const DOOR_WIDTH = 3;
const DOOR_HEIGHT = 7;

// ─── Helpers ────────────────────────────────────────────────────────

export function facesMatch(a, b) {
    if (!a || !b) return false;
    return a.brushId === b.brushId && a.axis === b.axis && a.side === b.side;
}

// Get the per-face U/V bounds for a brush face
export function getFaceUVInfo(brush, axis) {
    if (axis === 'x') return { uMin: brush.z, uMax: brush.z + brush.d, vMin: brush.y, vMax: brush.y + brush.h, uSize: brush.d, vSize: brush.h };
    if (axis === 'y') return { uMin: brush.x, uMax: brush.x + brush.w, vMin: brush.z, vMax: brush.z + brush.d, uSize: brush.w, vSize: brush.d };
    return              { uMin: brush.x, uMax: brush.x + brush.w, vMin: brush.y, vMax: brush.y + brush.h, uSize: brush.w, vSize: brush.h };
}

// Convert a world-space hit point to face-local U,V (in WT units)
export function worldToFaceUV(hitPoint, axis) {
    const p = { x: hitPoint.x / WORLD_SCALE, y: hitPoint.y / WORLD_SCALE, z: hitPoint.z / WORLD_SCALE };
    if (axis === 'x') return { u: p.z, v: p.y };
    if (axis === 'y') return { u: p.x, v: p.z };
    return              { u: p.x, v: p.y };
}

// Compute U/V bounds for a baked face by scanning the region's mesh geometry.
// Used when the selected face has brushId === 0 (no matching brush — it lives
// in the baked CSG geometry).
export function getBakedFaceUVInfo(face) {
    if (face.regionId == null) return null;
    const data = csgRegionMeshes.get(face.regionId);
    if (!data) return null;

    const { mesh, faceIds } = data;
    const pos = mesh.geometry.getAttribute('position');
    const idx = mesh.geometry.index;
    if (!pos) return null;

    const { axis, side, position } = face;
    let uMin = Infinity, uMax = -Infinity, vMin = Infinity, vMax = -Infinity;
    const v = new THREE.Vector3();

    for (let i = 0; i < faceIds.length; i++) {
        const f = faceIds[i];
        if (!f || f.brushId !== 0 || f.axis !== axis || f.side !== side || f.position !== position) continue;

        for (let j = 0; j < 3; j++) {
            const vi = idx ? idx.getX(i * 3 + j) : i * 3 + j;
            v.fromBufferAttribute(pos, vi);
            const uv = worldToFaceUV(v, axis);
            uMin = Math.min(uMin, uv.u); uMax = Math.max(uMax, uv.u);
            vMin = Math.min(vMin, uv.v); vMax = Math.max(vMax, uv.v);
        }
    }

    if (!isFinite(uMin)) return null;
    return {
        uMin: Math.round(uMin), uMax: Math.round(uMax),
        vMin: Math.round(vMin), vMax: Math.round(vMax),
        uSize: Math.round(uMax - uMin),
        vSize: Math.round(vMax - vMin)
    };
}

// Look up a brush by id, falling back to the region's shell.
export function findBrushById(brushId, regionId) {
    if (brushId === 0) return null; // baked
    const userBrush = state.csg.brushes.find(b => b.id === brushId);
    if (userBrush) return userBrush;
    if (regionId != null) {
        const data = csgRegionMeshes.get(regionId);
        if (data && data.region.shell.id === brushId) return data.region.shell;
    }
    return null;
}

export function getSelectedFaceInfo() {
    const sel = state.csg.selectedFace;
    if (!sel) return null;
    if (sel.brushId === 0) return getBakedFaceUVInfo(sel);
    const brush = findBrushById(sel.brushId, sel.regionId);
    if (!brush) return null;
    return getFaceUVInfo(brush, sel.axis);
}

export function isFullFace() {
    const info = getSelectedFaceInfo();
    if (!info) return true;
    const { selSizeU, selSizeV } = state.csg;
    return (selSizeU <= 0 || selSizeU >= info.uSize) &&
           (selSizeV <= 0 || selSizeV >= info.vSize);
}

// Whether a face's current AABB position still matches the {axis,side,position}
// recorded on the picked face. Shift-clicked faces are inherently "full-face"
// (they have no sub-rect state), so only brush-existence needs checking.
function faceBrushStillAligned(face) {
    if (!face || face.brushId === 0) return false;
    const brush = state.csg.brushes.find(b => b.id === face.brushId);
    if (!brush) return false;
    const dimKey = face.axis === 'x' ? 'w' : face.axis === 'y' ? 'h' : 'd';
    const pos = face.side === 'max' ? brush[face.axis] + brush[dimKey] : brush[face.axis];
    return pos === face.position;
}

// ─── Selection ──────────────────────────────────────────────────────

// Called by indoorClick when the user clicks while CSG tool is active.
// `face` is the result of pickCSGFace: { regionId, brushId, axis, side, position, point }
export function selectFaceAtCrosshair(face) {
    if (!face) return;

    if (!facesMatch(state.csg.selectedFace, face)) {
        state.csg.selectedFace = face;
        state.csg.selectedFaces = [];
        state.csg.selSizeU = 0;
        state.csg.selSizeV = 0;
        state.csg.selU0 = 0; state.csg.selU1 = 0; state.csg.selV0 = 0; state.csg.selV1 = 0;
        state.csg.activeBrush = null;
        state.csg.activeOp = null;
        state.csg.activeSide = null;
        state.csg.activeStairOp = null;
        state.csg.pendingStairOp = null;
    } else if (face.triIndex !== undefined) {
        // Same face, different triangle — refresh triIndex/point for Face Paint.
        state.csg.selectedFace.triIndex = face.triIndex;
        state.csg.selectedFace.point = face.point;
    }
}

// Shift+Click handler: toggle a coplanar full-face selection into the multi-set.
// Enforces: primary exists, primary is full-face, neither face is baked, face
// is coplanar with primary (same axis/side/position), brush still aligned.
export function toggleFaceInMultiSelection(face) {
    const csg = state.csg;
    if (!face) return;
    if (!csg.selectedFace) { showMessage('Select a primary face first'); return; }

    const primary = csg.selectedFace;
    if (primary.brushId === 0 || face.brushId === 0) {
        showMessage('Multi-select not available on baked faces');
        return;
    }
    if (face.axis !== primary.axis || face.side !== primary.side || face.position !== primary.position) {
        showMessage('Face must be coplanar with the primary selection');
        return;
    }
    if (!isFullFace()) {
        showMessage('Multi-select requires full-face selections');
        return;
    }
    if (!faceBrushStillAligned(primary) || !faceBrushStillAligned(face)) {
        showMessage('Face no longer aligned with its brush');
        return;
    }
    if (facesMatch(primary, face)) return;  // shift-clicking primary is a no-op

    const idx = csg.selectedFaces.findIndex(f => facesMatch(f, face));
    if (idx >= 0) {
        csg.selectedFaces.splice(idx, 1);
    } else {
        csg.selectedFaces.push(face);
        // Any growth to active brush is no longer valid once multi-select starts.
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    }
}

// Adjust the selection rectangle size on the current face (scroll wheel)
export function adjustSelectionSize(deltaU, deltaV) {
    if (!state.csg.selectedFace) return;
    if (state.csg.selectedFaces.length > 0) {
        showMessage('Scroll disabled during multi-face selection');
        return;
    }
    const info = getSelectedFaceInfo();
    if (!info) return;

    if (deltaU !== 0) {
        if (state.csg.selSizeU <= 0) state.csg.selSizeU = info.uSize;
        state.csg.selSizeU = Math.max(1, Math.min(info.uSize, state.csg.selSizeU + deltaU));
    }
    if (deltaV !== 0) {
        if (state.csg.selSizeV <= 0) state.csg.selSizeV = info.vSize;
        state.csg.selSizeV = Math.max(1, Math.min(info.vSize, state.csg.selSizeV + deltaV));
    }
    state.csg.activeBrush = null;
    state.csg.activeOp = null;
    state.csg.activeSide = null;
    state.csg.activeStairOp = null;
    state.csg.pendingStairOp = null;
}

// Adjust brace dimensions before placement (scroll wheel in brace mode).
// deltaW = width along the wall, deltaD = depth into the room.
export function adjustBraceSize(deltaW, deltaD) {
    const csg = state.csg;
    if (deltaW) csg.braceWidth = Math.max(MIN_BRACE_DIM, Math.min(MAX_BRACE_DIM, csg.braceWidth + deltaW));
    if (deltaD) csg.braceDepth = Math.max(MIN_BRACE_DIM, Math.min(MAX_BRACE_DIM, csg.braceDepth + deltaD));
}

// Adjust pillar cross-section size before placement (scroll wheel in pillar mode).
// Pillars stay square, so a single delta scales both X and Z.
export function adjustPillarSize(delta) {
    const csg = state.csg;
    csg.pillarSize = Math.max(MIN_PILLAR_SIZE, Math.min(MAX_PILLAR_SIZE, csg.pillarSize + delta));
}

// ─── Push / Pull / Extrude ───────────────────────────────────────────

function ensureSelectionBounds() {
    const csg = state.csg;
    if (csg.selU0 === 0 && csg.selU1 === 0 && csg.selV0 === 0 && csg.selV1 === 0) {
        const info = getSelectedFaceInfo();
        if (info) {
            const sU = csg.selSizeU <= 0 ? info.uSize : Math.min(csg.selSizeU, info.uSize);
            const sV = csg.selSizeV <= 0 ? info.vSize : Math.min(csg.selSizeV, info.vSize);
            csg.selU0 = info.uMin + Math.round((info.uSize - sU) / 2);
            csg.selV0 = info.vMin + Math.round((info.vSize - sV) / 2);
            csg.selU1 = csg.selU0 + sU;
            csg.selV1 = csg.selV0 + sV;
        }
    }
}

function createSubFaceBrush(op, depth) {
    ensureSelectionBounds();
    const sel = state.csg.selectedFace;
    const { axis, side, position } = sel;
    const facePos = position;
    const { selU0, selU1, selV0, selV1 } = state.csg;

    let nx, ny, nz, nw, nh, nd;
    if (axis === 'x') {
        nz = selU0; ny = selV0;
        nd = selU1 - selU0; nh = selV1 - selV0;
        nw = depth;
        nx = side === 'max' ? facePos : facePos - depth;
    } else if (axis === 'y') {
        nx = selU0; nz = selV0;
        nw = selU1 - selU0; nd = selV1 - selV0;
        nh = depth;
        ny = side === 'max' ? facePos : facePos - depth;
    } else {
        nx = selU0; ny = selV0;
        nw = selU1 - selU0; nh = selV1 - selV0;
        nd = depth;
        nz = side === 'max' ? facePos : facePos - depth;
    }

    if (op === 'add') {
        if (axis === 'x') { nx = side === 'max' ? facePos - depth : facePos; }
        else if (axis === 'y') { ny = side === 'max' ? facePos - depth : facePos; }
        else { nz = side === 'max' ? facePos - depth : facePos; }
    }

    const newBrush = new BrushDef(state.csg.nextBrushId++, op, nx, ny, nz, nw, nh, nd);
    const source = findBrushById(sel.brushId, sel.regionId);
    if (source) {
        const overrideKey = `${sel.axis}-${sel.side}`;
        newBrush.schemeKey = source.schemeOverrides?.[overrideKey] || source.schemeKey;
    }
    state.csg.brushes.push(newBrush);
    return newBrush;
}

function getActiveBrushOutwardFace() {
    const csg = state.csg;
    if (!csg.activeBrush || !csg.activeSide) return csg.selectedFace;
    const { axis, regionId } = csg.selectedFace;
    const side = csg.activeSide;
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    return {
        regionId,
        brushId: csg.activeBrush.id, axis, side,
        position: side === 'max' ? csg.activeBrush[axis] + csg.activeBrush[dimKey] : csg.activeBrush[axis]
    };
}

function getActiveBrushInwardFace() {
    const csg = state.csg;
    if (!csg.activeBrush || !csg.activeSide) return csg.selectedFace;
    const { axis, regionId } = csg.selectedFace;
    const side = csg.activeSide;
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    const inwardSide = side === 'max' ? 'min' : 'max';
    return {
        regionId,
        brushId: csg.activeBrush.id, axis, side: inwardSide,
        position: inwardSide === 'max' ? csg.activeBrush[axis] + csg.activeBrush[dimKey] : csg.activeBrush[axis]
    };
}

function growActiveBrush(amount) {
    const csg = state.csg;
    if (!csg.activeBrush || !csg.activeSide) return;
    const { axis } = csg.selectedFace;
    const side = csg.activeSide;
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';

    if (csg.activeOp === 'push' || csg.activeOp === 'extrude') {
        if (side === 'max') {
            csg.activeBrush[dimKey] += amount;
        } else {
            csg.activeBrush[axis] -= amount;
            csg.activeBrush[dimKey] += amount;
            if (axis === 'y') csg.activeBrush.floorY = csg.activeBrush.y;
        }
    } else {
        if (side === 'max') {
            csg.activeBrush[axis] -= amount;
            csg.activeBrush[dimKey] += amount;
            if (axis === 'y') csg.activeBrush.floorY = csg.activeBrush.y;
        } else {
            csg.activeBrush[dimKey] += amount;
        }
    }
}

// Apply a full-face +step grow to a single brush AABB.
function applyFullFacePush(brush, axis, side, step) {
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    if (side === 'max') { brush[dimKey] += step; }
    else { brush[axis] -= step; brush[dimKey] += step; }
    if (axis === 'y' && side === 'min') brush.floorY = brush.y;
}

// Apply a full-face -step shrink to a single brush AABB. Returns false if the
// brush is too thin along `axis` to absorb the shrink.
function applyFullFacePull(brush, axis, side, step) {
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    if (brush[dimKey] <= step) return false;
    if (side === 'max') { brush[dimKey] -= step; }
    else { brush[axis] += step; brush[dimKey] -= step; }
    if (axis === 'y' && side === 'min') brush.floorY = brush.y;
    return true;
}

function newFacePosition(brush, axis, side) {
    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    return side === 'max' ? brush[axis] + brush[dimKey] : brush[axis];
}

export function pushSelectedFace(step = PUSH_PULL_STEP) {
    const csg = state.csg;
    if (!csg.selectedFace) return;

    const sel = csg.selectedFace;
    const brush = state.csg.brushes.find(b => b.id === sel.brushId);
    const isBaked = sel.brushId === 0;
    const hasMulti = csg.selectedFaces.length > 0;

    // Multi-face full-face push: apply to primary + every member in lockstep.
    if (hasMulti && isFullFace() && brush && !isBaked) {
        const { axis, side } = sel;
        const affected = [brush.id];
        applyFullFacePush(brush, axis, side, step);
        sel.position = newFacePosition(brush, axis, side);

        for (const member of csg.selectedFaces) {
            const mb = state.csg.brushes.find(b => b.id === member.brushId);
            if (!mb) continue;
            applyFullFacePush(mb, member.axis, member.side, step);
            member.position = newFacePosition(mb, member.axis, member.side);
            affected.push(mb.id);
        }
        csg.activeBrush = null;
        csg.activeSide = null;
        rebuildAffectedRegions(affected);
        return;
    }

    if (isFullFace() && brush && !isBaked) {
        // Full-face push on a real brush — resize directly
        const { axis, side } = sel;
        applyFullFacePush(brush, axis, side, step);
        sel.position = newFacePosition(brush, axis, side);
        csg.activeBrush = null;
        csg.activeSide = null;
    } else {
        // Sub-face push or baked-face push — create/grow a subtractive brush
        if (csg.activeBrush && csg.activeOp === 'push') {
            growActiveBrush(step);
        } else {
            csg.activeSide = sel.side;
            csg.activeBrush = createSubFaceBrush('subtract', step);
            csg.activeOp = 'push';
        }
        csg.selectedFace = getActiveBrushOutwardFace();
        csg.selSizeU = 0; csg.selSizeV = 0;
    }

    const affectedId = csg.activeBrush ? csg.activeBrush.id : (brush ? brush.id : null);
    if (affectedId != null) rebuildAffectedRegions([affectedId]);
    else rebuildAllCSG();
}

export function pullSelectedFace(step = PUSH_PULL_STEP) {
    const csg = state.csg;
    if (!csg.selectedFace) return;

    const sel = csg.selectedFace;
    const brush = state.csg.brushes.find(b => b.id === sel.brushId);
    const isBaked = sel.brushId === 0;
    const hasMulti = csg.selectedFaces.length > 0;

    // Multi-face full-face pull: apply to primary + every member in lockstep.
    // Abort atomically if any brush is too thin along the pull axis.
    if (hasMulti && isFullFace() && brush && !isBaked) {
        const { axis, side } = sel;
        const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
        if (brush[dimKey] <= step) { showMessage('Brush too thin to pull'); return; }
        for (const member of csg.selectedFaces) {
            const mb = state.csg.brushes.find(b => b.id === member.brushId);
            const mDim = member.axis === 'x' ? 'w' : member.axis === 'y' ? 'h' : 'd';
            if (!mb || mb[mDim] <= step) { showMessage('A member brush is too thin to pull'); return; }
        }

        const affected = [brush.id];
        applyFullFacePull(brush, axis, side, step);
        sel.position = newFacePosition(brush, axis, side);

        for (const member of csg.selectedFaces) {
            const mb = state.csg.brushes.find(b => b.id === member.brushId);
            applyFullFacePull(mb, member.axis, member.side, step);
            member.position = newFacePosition(mb, member.axis, member.side);
            affected.push(mb.id);
        }
        csg.activeBrush = null;
        csg.activeSide = null;
        rebuildAffectedRegions(affected);
        return;
    }

    if (csg.activeBrush && csg.activeOp === 'pull') {
        growActiveBrush(step);
        csg.selectedFace = getActiveBrushInwardFace();
    } else if (isFullFace() && brush && !isBaked) {
        const { axis, side } = sel;
        if (!applyFullFacePull(brush, axis, side, step)) return;
        sel.position = newFacePosition(brush, axis, side);
        csg.activeBrush = null;
        csg.activeSide = null;
    } else {
        csg.activeSide = sel.side;
        csg.activeBrush = createSubFaceBrush('add', step);
        csg.activeOp = 'pull';
        csg.selectedFace = getActiveBrushInwardFace();
        csg.selSizeU = 0; csg.selSizeV = 0;
    }

    const affectedId = csg.activeBrush ? csg.activeBrush.id : (brush ? brush.id : null);
    if (affectedId != null) rebuildAffectedRegions([affectedId]);
    else rebuildAllCSG();
}

export function extrudeSelectedFace(step = PUSH_PULL_STEP) {
    const csg = state.csg;
    if (!csg.selectedFace) return;
    csg.selectedFaces = [];  // extrude spawns a new brush; multi-select doesn't apply

    const sel = csg.selectedFace;
    const brush = state.csg.brushes.find(b => b.id === sel.brushId);
    const isBaked = sel.brushId === 0;
    const { axis, side, regionId } = sel;

    let faceInfo;
    if (brush) faceInfo = getFaceUVInfo(brush, axis);
    else if (isBaked) faceInfo = getBakedFaceUVInfo(sel);
    if (!faceInfo) return;

    const depth = step;
    let nx, ny, nz, nw, nh, nd;

    if (axis === 'x') {
        nz = faceInfo.uMin; ny = faceInfo.vMin;
        nd = faceInfo.uSize; nh = faceInfo.vSize;
        nw = depth;
        nx = side === 'max' ? sel.position : sel.position - depth;
    } else if (axis === 'y') {
        nx = faceInfo.uMin; nz = faceInfo.vMin;
        nw = faceInfo.uSize; nd = faceInfo.vSize;
        nh = depth;
        ny = side === 'max' ? sel.position : sel.position - depth;
    } else {
        nx = faceInfo.uMin; ny = faceInfo.vMin;
        nw = faceInfo.uSize; nh = faceInfo.vSize;
        nd = depth;
        nz = side === 'max' ? sel.position : sel.position - depth;
    }

    const op = brush ? brush.op : 'subtract';
    const newBrush = new BrushDef(csg.nextBrushId++, op, nx, ny, nz, nw, nh, nd);
    csg.brushes.push(newBrush);

    csg.activeSide = side;
    csg.activeBrush = newBrush;
    csg.activeOp = 'extrude';

    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    csg.selectedFace = {
        regionId,
        brushId: newBrush.id, axis, side,
        position: side === 'max' ? newBrush[axis] + newBrush[dimKey] : newBrush[axis]
    };
    csg.selSizeU = 0; csg.selSizeV = 0;

    assignBrushToRegion(newBrush);
    rebuildAffectedRegions([newBrush.id]);
}

// Continue an active extrude (called when user presses + after extrude)
export function growActiveExtrude(step = 1) {
    const csg = state.csg;
    if (!csg.activeBrush || csg.activeOp !== 'extrude') return false;
    growActiveBrush(step);
    csg.selectedFace = getActiveBrushOutwardFace();
    rebuildAffectedRegions([csg.activeBrush.id]);
    return true;
}

// ─── Stair Push (Arrow keys on a wall face) ─────────────────────────
//
// Pushes the selected wall face into the wall AND simultaneously carves a
// staircase whose step count equals the push depth. Each press grows the op
// by one step; the opposite arrow shrinks by one. Implemented as N abutting
// 1-WT-deep subtractive brushes with progressive floor offsets — the CSG
// evaluator naturally produces tread/riser geometry where they overlap.
//
// Down-stair shape:
//   • Cols 0..N-2 keep the original ceiling H.
//   • Deepest col (k = N-1) drops to ceiling H-N (start of a "lower corridor").
// Up-stair shape:
//   • Entire alcove ceiling raised to H+N from the very first step
//     (head clearance, so the player doesn't bonk going up).

// True iff the V (vertical) bottom of the current selection sits on the face's floor (vMin).
// Only meaningful for walls (axis !== 'y'). Callers should ensure selection bounds are
// populated (either by the preview updater each frame or by ensureSelectionBounds()).
function wallSelectionTouchesFloor() {
    const sel = state.csg.selectedFace;
    if (!sel || sel.axis === 'y') return false;
    const info = getSelectedFaceInfo();
    if (!info) return false;
    if (state.csg.selSizeV <= 0) return true;  // full-V implicitly touches the floor
    return state.csg.selV0 === info.vMin;
}

// Remove brushes by id from the global brush list and region tracking. No rebuild — caller does it.
function removeBrushesByIds(ids) {
    if (!ids || ids.length === 0) return;
    const idSet = new Set(ids);
    state.csg.brushes = state.csg.brushes.filter(b => !idSet.has(b.id));
    for (const id of ids) removeBrushFromRegion(id);
}

// Cancel an active stair op: remove all brushes it created, clear tracking.
// (Legacy path — kept for old activeStairOp in-flight during undo.)
export function cancelStairOp() {
    const csg = state.csg;
    // New deferred path
    if (csg.pendingStairOp) {
        csg.pendingStairOp = null;
        return;
    }
    // Legacy path
    if (!csg.activeStairOp) return;
    removeBrushesByIds(csg.activeStairOp.brushIds);
    csg.activeStairOp = null;
    rebuildAllCSG();
}

// Main entry: arrow-key handler. direction = 'down' | 'up'.
// Each press adjusts the pending stair counter (no brushes created yet).
// Press Enter to confirm, Escape to cancel.
export function pushSelectedFaceAsStairs(direction) {
    const csg = state.csg;
    const sel = csg.selectedFace;
    if (!sel) return;
    if (sel.axis === 'y') return;            // floors/ceilings not supported

    // Resolve face V bounds and anchor brush for texture inheritance.
    let info;
    let anchorBrush = null;
    if (sel.brushId === 0) {
        info = getBakedFaceUVInfo(sel);
    } else {
        anchorBrush = findBrushById(sel.brushId, sel.regionId);
        if (!anchorBrush) return;
        info = getFaceUVInfo(anchorBrush, sel.axis);
    }
    if (!info) return;

    ensureSelectionBounds();
    if (!wallSelectionTouchesFloor()) return;  // require bottom of selection at floor

    const floor = info.vMin;
    const H = csg.selSizeV <= 0 ? info.vMax : csg.selV1;
    const facePos = sel.position;

    // Inherit texture scheme from anchor brush
    const schemeKey = anchorBrush ? anchorBrush.schemeKey : 'facility_white_tile';

    // Decide new step count based on existing pending op (if any).
    let newCount;
    const op = csg.pendingStairOp;
    const sameAnchor = op
        && op.axis === sel.axis && op.side === sel.side && op.facePos === facePos
        && op.selU0 === csg.selU0 && op.selU1 === csg.selU1
        && op.regionId === sel.regionId;

    if (sameAnchor && op.direction === direction) {
        newCount = op.stepCount + 1;
    } else if (sameAnchor && op.direction !== direction) {
        newCount = op.stepCount - 1;
        if (newCount <= 0) {
            csg.pendingStairOp = null;
            return;
        }
    } else {
        newCount = 1;
    }

    // Update floorY based on final step count
    const finalDestFloor = direction === 'down' ? (floor - newCount) : (floor + newCount);

    csg.pendingStairOp = {
        direction,
        stepCount: newCount,
        axis: sel.axis,
        side: sel.side,
        facePos,
        selU0: csg.selU0,
        selU1: csg.selU1,
        floor,
        H,
        anchorBrushId: anchorBrush ? anchorBrush.id : null,
        regionId: sel.regionId,
        schemeKey,
        floorY: finalDestFloor,
    };
    // No CSG rebuild — preview is rendered separately in csgPreviews.js
}

// Confirm a pending stair op: create two void brushes + register stair descriptor.
//
// DOWN stairs (2 brushes):
//   Brush 1 (stairwell): full staircase length, floor drops by stepCount, ceiling = H
//   Brush 2 (destination): 1 WT deep at the far end, same low floor, ceiling = H - stepCount
//
// UP stairs (2 brushes):
//   Brush 1 (stairwell): full staircase length, floor = original, ceiling = H + stepCount
//   Brush 2 (destination): 1 WT deep at the far end, same raised ceiling, floor = floor + stepCount
//
export function confirmStairOp() {
    const csg = state.csg;
    const op = csg.pendingStairOp;
    if (!op) return;

    const { axis, side, facePos, selU0, selU1, floor, H, direction, stepCount, schemeKey, floorY, regionId } = op;
    const dir = side === 'max' ? 1 : -1;

    // ── Brush 1: main stairwell ──────────────────────────────────────
    // Starts flush at the wall face — no burial epsilon. If coplanar CSG
    // artifacts appear on sub-face stairs, we can revisit.
    let b1_normalLo, b1_normalHi, b1_yMin, b1_yMax;
    if (dir === 1) {
        b1_normalLo = facePos;
        b1_normalHi = facePos + stepCount;
    } else {
        b1_normalLo = facePos - stepCount;
        b1_normalHi = facePos;
    }
    if (direction === 'down') {
        b1_yMin = floor - stepCount;
        b1_yMax = H;
    } else {
        b1_yMin = floor;
        b1_yMax = H + stepCount;
    }

    const brush1 = makeBrush(axis, b1_normalLo, b1_normalHi, b1_yMin, b1_yMax, selU0, selU1);
    brush1.isStairVoid = true;
    brush1.schemeKey = schemeKey;
    brush1.floorY = floorY;

    // ── Brush 2: destination corridor ────────────────────────────────
    // 1 WT deep at the far end of the stairwell
    let b2_normalLo, b2_normalHi, b2_yMin, b2_yMax;
    if (dir === 1) {
        b2_normalLo = facePos + stepCount;
        b2_normalHi = facePos + stepCount + 1;
    } else {
        b2_normalLo = facePos - stepCount - 1;
        b2_normalHi = facePos - stepCount;
    }
    if (direction === 'down') {
        b2_yMin = floor - stepCount;
        b2_yMax = H - stepCount;       // ceiling drops by stepCount
    } else {
        b2_yMin = floor + stepCount;    // floor raises to top of stairs
        b2_yMax = H + stepCount;
    }

    const brush2 = makeBrush(axis, b2_normalLo, b2_normalHi, b2_yMin, b2_yMax, selU0, selU1);
    brush2.isStairVoid = true;
    brush2.schemeKey = schemeKey;
    brush2.floorY = floorY;

    // Register stair descriptor
    const stairId = csg.nextCsgStairId++;
    brush1.stairDescriptorId = stairId;
    brush2.stairDescriptorId = stairId;

    const descriptor = {
        id: stairId,
        voidBrushIds: [brush1.id, brush2.id],
        direction, stepCount, axis, side,
        facePos, selU0, selU1, floor, H,
        schemeKey, floorY,
    };
    csg.csgStairs.push(descriptor);

    // Add brushes to scene
    const newIds = [brush1.id, brush2.id];
    csg.brushes.push(brush1, brush2);

    // Register in region
    const regionData = csgRegionMeshes.get(regionId);
    for (const brush of [brush1, brush2]) {
        if (regionData && regionData.region) {
            regionData.region.brushes.push(brush);
            assignBrushToRegionDirect(brush.id, regionId);
        } else {
            assignBrushToRegion(brush);
        }
    }

    // Clear pending op
    csg.pendingStairOp = null;

    // Rebuild CSG
    rebuildAffectedRegions(newIds);

    return descriptor;
}

// Helper: build a subtractive BrushDef from normal-axis lo/hi, Y lo/hi, U lo/hi.
function makeBrush(axis, normalLo, normalHi, yMin, yMax, uLo, uHi) {
    const csg = state.csg;
    let nx, ny, nz, nw, nh, nd;
    ny = yMin;
    nh = yMax - yMin;
    if (axis === 'x') {
        nx = normalLo;
        nw = normalHi - normalLo;
        nz = uLo;
        nd = uHi - uLo;
    } else {
        nz = normalLo;
        nd = normalHi - normalLo;
        nx = uLo;
        nw = uHi - uLo;
    }
    return new BrushDef(csg.nextBrushId++, 'subtract', nx, ny, nz, nw, nh, nd);
}

// Delete a confirmed CSG stair by descriptor id: remove void brushes + descriptor.
export function deleteCsgStair(stairId) {
    const csg = state.csg;
    const idx = csg.csgStairs.findIndex(s => s.id === stairId);
    if (idx < 0) return;
    const desc = csg.csgStairs[idx];
    removeBrushesByIds(desc.voidBrushIds || [desc.voidBrushId]);
    csg.csgStairs.splice(idx, 1);
    rebuildAllCSG();
}

export function scaleSelectedFace(deltaU, deltaV) {
    const csg = state.csg;
    if (!csg.selectedFace) return;
    const sel = csg.selectedFace;

    const brush = state.csg.brushes.find(b => b.id === sel.brushId);
    if (!brush) return; // can only taper unbaked brushes

    const { axis, side } = sel;
    const faceKey = `${axis}-${side}`;

    if (!brush.taper[faceKey]) brush.taper[faceKey] = { u: 0, v: 0 };
    const t = brush.taper[faceKey];
    const info = getFaceUVInfo(brush, axis);

    const maxU = Math.floor((info.uSize - 1) / 2);
    const maxV = Math.floor((info.vSize - 1) / 2);
    t.u = Math.max(0, Math.min(maxU, t.u + deltaU));
    t.v = Math.max(0, Math.min(maxV, t.v + deltaV));

    if (t.u === 0 && t.v === 0) delete brush.taper[faceKey];

    rebuildAffectedRegions([brush.id]);
}

// ─── Cave Envelope ──────────────────────────────────────────────────

// Start a cave carved outward from the selected face. Creates a CaveDef +
// cuboid of voxel air whose cross-section matches the clicked face's full
// u/v extents and extends CAVE_INIT_DEPTH_WT into solid rock past the wall.
// No CSG mutation — the wall stays intact during editing. At bake time,
// every triangle coplanar with the anchor face plane (within the rectangle)
// is deleted from both CSG and cave mesh, taking out the wall + the MC cap.
const CAVE_INIT_DEPTH_WT = 4;                   // how far into solid rock the initial cuboid extends

export function startCaveFromFace() {
    const sel = state.csg.selectedFace;
    if (!sel) return { ok: false, reason: 'no_selection' };
    if (sel.brushId === 0) return { ok: false, reason: 'baked_face' };

    const anchorBrush = findBrushById(sel.brushId, sel.regionId);
    if (!anchorBrush) return { ok: false, reason: 'no_brush' };
    if (!state.csg.brushes.includes(anchorBrush)) return { ok: false, reason: 'not_user_brush' };
    if (anchorBrush.op !== 'subtract') return { ok: false, reason: 'not_room_brush' };

    const regionData = csgRegionMeshes.get(sel.regionId);
    if (!regionData) return { ok: false, reason: 'no_region' };
    const region = regionData.region;

    // Anchor rect = the entire face. This rect is what bake deletes (CSG
    // tile wall + MC cap on the cave side).
    const cs = state.csg;
    const info = getFaceUVInfo(anchorBrush, sel.axis);
    const u0 = info.uMin, u1 = info.uMax;
    const v0 = info.vMin, v1 = info.vMax;

    const { axis, side, position } = sel;

    const cave = new CaveDef(cs.nextCaveId++, sel.regionId);
    cave.anchorFaces.push({ brushId: anchorBrush.id, axis, side, position, u0, u1, v0, v1 });

    // Cuboid bounds in world meters.
    const S = WORLD_SCALE;
    const dir = side === 'max' ? 1 : -1;
    const depthM = CAVE_INIT_DEPTH_WT * S;
    const planeM = position * S;
    const nFar = planeM + dir * depthM;
    const nMinM = Math.min(planeM, nFar);
    const nMaxM = Math.max(planeM, nFar);
    const uMinM = u0 * S, uMaxM = u1 * S;
    const vMinM = v0 * S, vMaxM = v1 * S;
    const buf = S;                              // 1 WT buffer for the clip envelope

    // cuboidAabb is the voxel-air volume; extentAabb wraps it with a buffer.
    let cuboidAabb;
    if (axis === 'x') {
        cuboidAabb = { minX: nMinM, maxX: nMaxM, minY: vMinM, maxY: vMaxM, minZ: uMinM, maxZ: uMaxM };
    } else if (axis === 'y') {
        cuboidAabb = { minX: uMinM, maxX: uMaxM, minY: nMinM, maxY: nMaxM, minZ: vMinM, maxZ: vMaxM };
    } else {
        cuboidAabb = { minX: uMinM, maxX: uMaxM, minY: vMinM, maxY: vMaxM, minZ: nMinM, maxZ: nMaxM };
    }
    cave.extentAabb = {
        minX: cuboidAabb.minX - buf, maxX: cuboidAabb.maxX + buf,
        minY: cuboidAabb.minY - buf, maxY: cuboidAabb.maxY + buf,
        minZ: cuboidAabb.minZ - buf, maxZ: cuboidAabb.maxZ + buf,
    };

    region.caves.push(cave);

    // Materialise the CaveWorld (voxels default-solid), then carve the cuboid.
    syncCavesForRegion(region);
    seedCuboidCave(cave, cuboidAabb);
    return { ok: true, cave };
}

// Place an "exit room" subtract brush at the cave-wall hit position. The
// room's W×H×D is read from state.csg.exitRoomSize (user-adjustable via
// scroll in sculpt P-submode). The new room joins the cave's anchor region
// (no new shell — the existing region shell auto-grows via updateShell to
// contain it). Room is offset: its face closest to the cave is coincident
// with the (axis-snapped) hit plane; the rest extends into the rock beyond.
// At bake, the cave-facing face is culled from both CSG and cave mesh.

export function placeExitRoomFromCaveHit(cave, hitPoint, hitNormal) {
    if (!cave) return { ok: false, reason: 'no_cave' };
    const regionData = csgRegionMeshes.get(cave.regionId);
    if (!regionData) return { ok: false, reason: 'no_region' };
    const region = regionData.region;

    // Snap the surface normal to the nearest cardinal axis — CSG brushes are
    // AABBs, so the room's entry face has to be axis-aligned.
    const nx = Math.abs(hitNormal.x), ny = Math.abs(hitNormal.y), nz = Math.abs(hitNormal.z);
    let axis, sign;
    if (nx >= ny && nx >= nz)      { axis = 'x'; sign = hitNormal.x >= 0 ? 1 : -1; }
    else if (ny >= nz)             { axis = 'y'; sign = hitNormal.y >= 0 ? 1 : -1; }
    else                           { axis = 'z'; sign = hitNormal.z >= 0 ? 1 : -1; }

    const aabb = exitRoomAabbFromHit(hitPoint, axis, sign);

    const room = new BrushDef(
        state.csg.nextBrushId++, 'subtract',
        aabb.x, aabb.y, aabb.z, aabb.w, aabb.h, aabb.d,
    );
    room.schemeKey = 'facility_white_tile';

    state.csg.brushes.push(room);
    region.brushes.push(room);
    assignBrushToRegionDirect(room.id, region.id);

    // Record the exit room's cave-facing face so bake knows to delete +
    // stitch it. side/position/u0/u1/v0/v1 follow the same convention
    // getFaceUVInfo uses (axis='x' → u=z,v=y; axis='y' → u=x,v=z; axis='z'
    // → u=x,v=y).
    const faceSide = sign > 0 ? 'max' : 'min';
    let fPosition, u0, u1, v0, v1;
    if (axis === 'x') {
        fPosition = sign > 0 ? aabb.x + aabb.w : aabb.x;
        u0 = aabb.z; u1 = aabb.z + aabb.d;
        v0 = aabb.y; v1 = aabb.y + aabb.h;
    } else if (axis === 'y') {
        fPosition = sign > 0 ? aabb.y + aabb.h : aabb.y;
        u0 = aabb.x; u1 = aabb.x + aabb.w;
        v0 = aabb.z; v1 = aabb.z + aabb.d;
    } else {
        fPosition = sign > 0 ? aabb.z + aabb.d : aabb.z;
        u0 = aabb.x; u1 = aabb.x + aabb.w;
        v0 = aabb.y; v1 = aabb.y + aabb.h;
    }
    cave.anchorFaces.push({ brushId: room.id, axis, side: faceSide, position: fPosition, u0, u1, v0, v1 });

    // Seed a cave-air cuboid on the CAVE side of the anchor plane — mirror of
    // startCaveFromFace's entrance seed. Cross-section matches the exit room's
    // cave-facing face; depth = CAVE_INIT_DEPTH_WT into the cave region.
    // The room interior itself stays voxel-solid — CSG's subtract brush is
    // what makes the room visible; we don't want cave mesh wrapping it.
    const s = WORLD_SCALE;
    const dir = faceSide === 'max' ? 1 : -1;
    const depthM = CAVE_INIT_DEPTH_WT * s;
    const planeM = fPosition * s;
    const nFar = planeM + dir * depthM;
    const nMinM = Math.min(planeM, nFar);
    const nMaxM = Math.max(planeM, nFar);
    const uMinM = u0 * s, uMaxM = u1 * s;
    const vMinM = v0 * s, vMaxM = v1 * s;

    let anchorCuboid;
    if (axis === 'x') {
        anchorCuboid = { minX: nMinM, maxX: nMaxM, minY: vMinM, maxY: vMaxM, minZ: uMinM, maxZ: uMaxM };
    } else if (axis === 'y') {
        anchorCuboid = { minX: uMinM, maxX: uMaxM, minY: nMinM, maxY: nMaxM, minZ: vMinM, maxZ: vMaxM };
    } else {
        anchorCuboid = { minX: uMinM, maxX: uMaxM, minY: vMinM, maxY: vMaxM, minZ: nMinM, maxZ: nMaxM };
    }

    // Grow the cave's clip envelope to cover the new cuboid (+ 1 WT buffer).
    const buf = s;
    if (!cave.extentAabb) {
        cave.extentAabb = {
            minX: anchorCuboid.minX - buf, maxX: anchorCuboid.maxX + buf,
            minY: anchorCuboid.minY - buf, maxY: anchorCuboid.maxY + buf,
            minZ: anchorCuboid.minZ - buf, maxZ: anchorCuboid.maxZ + buf,
        };
    } else {
        const e = cave.extentAabb;
        e.minX = Math.min(e.minX, anchorCuboid.minX - buf);
        e.minY = Math.min(e.minY, anchorCuboid.minY - buf);
        e.minZ = Math.min(e.minZ, anchorCuboid.minZ - buf);
        e.maxX = Math.max(e.maxX, anchorCuboid.maxX + buf);
        e.maxY = Math.max(e.maxY, anchorCuboid.maxY + buf);
        e.maxZ = Math.max(e.maxZ, anchorCuboid.maxZ + buf);
    }
    // Push the new clip envelope to the cave's WASM world, then carve.
    syncCavesForRegion(region);
    seedCuboidCave(cave, anchorCuboid);

    rebuildAffectedRegions([room.id]);
    return { ok: true, brush: room, axis, side: faceSide };
}

// Compute the exit-room AABB in WT space for a cave hit. Exported for the
// sculpt preview — same math as placement so the wireframe exactly matches
// what gets created. Dimensions come from state.csg.exitRoomSize (adjustable
// via scroll in sculpt P-submode).
export function exitRoomAabbFromHit(hitPoint, axis, sign) {
    const invS = 1 / WORLD_SCALE;
    const hx = Math.round(hitPoint.x * invS);
    const hy = Math.round(hitPoint.y * invS);
    const hz = Math.round(hitPoint.z * invS);
    const { depth, width, height } = state.csg.exitRoomSize;
    const halfW = width / 2, halfH = height / 2;
    let rx, ry, rz, rw, rh, rd;
    if (axis === 'x') {
        ry = hy - halfH; rh = height;
        rz = hz - halfW; rd = width;
        if (sign > 0) { rx = hx - depth; rw = depth; }   // normal +x (air); room in -x (rock)
        else          { rx = hx;         rw = depth; }
    } else if (axis === 'y') {
        rx = hx - halfW; rw = width;
        rz = hz - halfH; rd = height;
        if (sign > 0) { ry = hy - depth; rh = depth; }
        else          { ry = hy;         rh = depth; }
    } else {
        rx = hx - halfW; rw = width;
        ry = hy - halfH; rh = height;
        if (sign > 0) { rz = hz - depth; rd = depth; }
        else          { rz = hz;         rd = depth; }
    }
    return { x: rx, y: ry, z: rz, w: rw, h: rh, d: rd };
}

// ─── Hole / Door Modal Tool ──────────────────────────────────────────

// Legacy: true toggle. No callers remain after the Numpad-tool refactor;
// kept in case future code needs the toggle semantic.
export function toggleHoleMode(door) {
    const csg = state.csg;
    csg.holeDoor = !!door;
    csg.holeMode = !csg.holeMode;
    if (csg.holeMode) {
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    } else {
        csg.doorPreview = null;
    }
}

// Explicit setter — no toggle. Used by Numpad2/Numpad3 hotkeys and by the
// radial menu Hole/Door entries so the user can transition between modes
// without flicker (e.g. Hole → Door without canceling first).
export function setHoleMode(on, door) {
    const csg = state.csg;
    csg.holeMode = !!on;
    csg.holeDoor = !!door;
    csg.doorPreview = null;
    if (on) {
        csg.facePaintMode = false;
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    }
}

export function exitHoleMode() {
    state.csg.holeMode = false;
    state.csg.doorPreview = null;
}

// Compute the hole/door preview rectangle on the face under the crosshair.
// Called by csgPreviews.js each frame to update the yellow outline.
// Returns the preview shape or null if the face is unsuitable.
export function computeHolePreview(hitFace, hitPoint) {
    const csg = state.csg;
    if (!csg.holeMode || !hitFace || !hitPoint) return null;

    const holeW = csg.holeDoor ? DOOR_WIDTH : HOLE_WIDTH;
    const holeH = csg.holeDoor ? DOOR_HEIGHT : HOLE_HEIGHT;

    // Door mode: walls only
    if (csg.holeDoor && hitFace.axis === 'y') return null;

    const brush = findBrushById(hitFace.brushId, hitFace.regionId);
    if (!brush) return null;

    const info = getFaceUVInfo(brush, hitFace.axis);
    if (!info || info.uSize < holeW || info.vSize < holeH) return null;

    const uv = worldToFaceUV(hitPoint, hitFace.axis);

    let u0 = Math.round(uv.u - holeW / 2);
    u0 = Math.max(info.uMin, Math.min(u0, info.uMax - holeW));
    const u1 = u0 + holeW;

    let v0, v1;
    if (csg.holeDoor) {
        v0 = Math.round(uv.v - holeH / 2);
        v0 = Math.max(info.vMin, Math.min(v0, info.vMax - holeH));
        v1 = v0 + holeH;
    } else {
        v0 = Math.round(uv.v - holeH / 2);
        v0 = Math.max(info.vMin, Math.min(v0, info.vMax - holeH));
        v1 = v0 + holeH;
    }

    csg.doorPreview = { face: hitFace, u0, u1, v0, v1 };
    return csg.doorPreview;
}

export function confirmHolePlacement() {
    const csg = state.csg;
    if (!csg.doorPreview) return;

    const { face, u0, u1, v0, v1 } = csg.doorPreview;
    const { axis, side, position, regionId } = face;
    const t = WALL_THICKNESS;
    const uSize = u1 - u0, vSize = v1 - v0;

    let fx, fy, fz, fw, fh, fd;
    let px, py, pz, pw, ph, pd;

    if (axis === 'x') {
        fz = u0; fy = v0; fd = uSize; fh = vSize; fw = t;
        fx = side === 'max' ? position : position - t;
        pz = u0; py = v0; pd = uSize; ph = vSize; pw = t;
        px = side === 'max' ? position + t : position - 2 * t;
    } else if (axis === 'y') {
        fx = u0; fz = v0; fw = uSize; fd = vSize; fh = t;
        fy = side === 'max' ? position : position - t;
        px = u0; pz = v0; pw = uSize; pd = vSize; ph = t;
        py = side === 'max' ? position + t : position - 2 * t;
    } else {
        fx = u0; fy = v0; fw = uSize; fh = vSize; fd = t;
        fz = side === 'max' ? position : position - t;
        px = u0; py = v0; pw = uSize; ph = vSize; pd = t;
        pz = side === 'max' ? position + t : position - 2 * t;
    }

    const frame = new BrushDef(csg.nextBrushId++, 'subtract', fx, fy, fz, fw, fh, fd);
    if (csg.holeDoor) frame.isDoorframe = true;
    else frame.isHoleFrame = true;
    csg.brushes.push(frame);

    const protoroom = new BrushDef(csg.nextBrushId++, 'subtract', px, py, pz, pw, ph, pd);
    csg.brushes.push(protoroom);

    csg.holeMode = false;
    csg.doorPreview = null;

    const dimKey = axis === 'x' ? 'w' : axis === 'y' ? 'h' : 'd';
    csg.selectedFace = {
        regionId,
        brushId: protoroom.id, axis, side,
        position: side === 'max' ? protoroom[axis] + protoroom[dimKey] : protoroom[axis]
    };
    csg.selSizeU = 0; csg.selSizeV = 0;
    csg.activeBrush = null; csg.activeOp = null; csg.activeSide = null;

    assignBrushToRegion(frame);
    assignBrushToRegion(protoroom);
    rebuildAffectedRegions([frame.id, protoroom.id]);
}

// ─── Brace Modal Tool ───────────────────────────────────────────────
//
// A "brace" is a structural decoration shaped like an arch: vertical strip
// up one wall, horizontal strip across the ceiling, vertical strip down the
// opposite wall. Three additive brushes per arch, all marked isBrace so
// uvZones routes every face to zone 7 (the brace texture).

export function setBraceMode(on) {
    const csg = state.csg;
    csg.braceMode = !!on;
    csg.bracePreview = null;
    if (on) {
        csg.holeMode = false;
        csg.doorPreview = null;
        csg.facePaintMode = false;
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    }
}

export function exitBraceMode() {
    state.csg.braceMode = false;
    state.csg.bracePreview = null;
}

// Compute the 3 arch segments (wall1, ceiling, wall2) for a wall hit.
// Returns the preview shape or null if the face is unsuitable. Stores the
// preview on state.csg.bracePreview so renderers and confirm can read it.
export function computeBracePreview(hitFace, hitPoint) {
    const csg = state.csg;
    if (!csg.braceMode || !hitFace || !hitPoint) return null;
    if (hitFace.axis === 'y') return null;  // walls only

    const brush = findBrushById(hitFace.brushId, hitFace.regionId);
    if (!brush || brush.op !== 'subtract') return null;

    const bw = csg.braceWidth;   // size along the wall (U axis)
    const bd = csg.braceDepth;   // inward protrusion + ceiling thickness
    // Burial epsilon: half a WT into surrounding solid. Hides coplanar faces
    // inside the wall/floor/ceiling material without reaching the shell's
    // exterior face (walls are 1 WT thick, so 0.5 WT stays safely buried).
    const E  = WALL_THICKNESS / 2;

    // Use the clicked subtract brush's bounds as the room interior bounds.
    // For a single-brush rectangular room this gives the correct opposite
    // wall positions; L-shapes will end at the brush boundary (deferred).
    const ix0 = brush.minX, ix1 = brush.maxX;
    const iy0 = brush.minY, iy1 = brush.maxY;
    const iz0 = brush.minZ, iz1 = brush.maxZ;

    // Each brace brush is extended 1 WT into the surrounding solid on its
    // hidden faces (the ones flush against walls / floor / ceiling). Burying
    // those faces inside solid material avoids coplanar CSG artifacts where
    // the CSG engine would otherwise emit stray triangles at the seam.
    let wall1, ceiling, wall2;

    if (hitFace.axis === 'x') {
        // Brace runs across X (wall to wall on the X axis).
        // U coordinate of the brace = Z position of cursor, snapped to WT.
        const cursorZ = Math.round(hitPoint.z / WORLD_SCALE) - Math.floor(bw / 2);
        const z0 = Math.max(iz0, Math.min(iz1 - bw, cursorZ));

        // Wall 1: on the min-X wall. Buried into wall (-X), floor (-Y) and ceiling (+Y).
        wall1   = { x: ix0 - E,  y: iy0 - E,  z: z0, w: bd + E,        h: (iy1 - iy0) + 2 * E, d: bw };
        // Ceiling: full X interior. Buried into both walls and into ceiling (+Y).
        ceiling = { x: ix0 - E,  y: iy1 - bd, z: z0, w: (ix1 - ix0) + 2 * E, h: bd + E,        d: bw };
        // Wall 2: on the max-X wall. Buried into wall (+X), floor (-Y), ceiling (+Y).
        wall2   = { x: ix1 - bd, y: iy0 - E,  z: z0, w: bd + E,        h: (iy1 - iy0) + 2 * E, d: bw };
    } else { // axis === 'z'
        // Brace runs across Z. U = X position of cursor.
        const cursorX = Math.round(hitPoint.x / WORLD_SCALE) - Math.floor(bw / 2);
        const x0 = Math.max(ix0, Math.min(ix1 - bw, cursorX));

        wall1   = { x: x0, y: iy0 - E,  z: iz0 - E,  w: bw, h: (iy1 - iy0) + 2 * E, d: bd + E        };
        ceiling = { x: x0, y: iy1 - bd, z: iz0 - E,  w: bw, h: bd + E,              d: (iz1 - iz0) + 2 * E };
        wall2   = { x: x0, y: iy0 - E,  z: iz1 - bd, w: bw, h: (iy1 - iy0) + 2 * E, d: bd + E        };
    }

    csg.bracePreview = {
        regionId: hitFace.regionId,
        roomBrushId: brush.id,
        wall1, ceiling, wall2,
    };
    return csg.bracePreview;
}

export function confirmBracePlacement() {
    const csg = state.csg;
    if (!csg.bracePreview) return;
    const { wall1, ceiling, wall2, roomBrushId, regionId } = csg.bracePreview;

    // Inherit the room brush's scheme so each theme picks up its own zone-7 texture
    const roomBrush = findBrushById(roomBrushId, regionId);
    const schemeKey = (roomBrush && roomBrush.schemeKey) || 'facility_white_tile';
    const floorY    = (roomBrush && roomBrush.floorY)    ?? wall1.y;

    const newBraceIds = [];
    for (const r of [wall1, ceiling, wall2]) {
        const b = new BrushDef(csg.nextBrushId++, 'add', r.x, r.y, r.z, r.w, r.h, r.d);
        b.isBrace = true;
        b.schemeKey = schemeKey;
        b.floorY = floorY;
        state.csg.brushes.push(b);
        assignBrushToRegion(b);
        newBraceIds.push(b.id);
    }

    csg.braceMode = false;
    csg.bracePreview = null;
    rebuildAffectedRegions(newBraceIds);
}

// ─── Pillar Modal Tool ──────────────────────────────────────────────
//
// A "pillar" is a vertical square column from floor to ceiling. Internally
// it's just an isBrace brush — it inherits the brace texturing path so its
// appearance per scheme matches arches (zone 7 if defined, wall-split
// otherwise). The user aims at the floor, clicks, and a single additive
// brush is created.

export function setPillarMode(on) {
    const csg = state.csg;
    csg.pillarMode = !!on;
    csg.pillarPreview = null;
    if (on) {
        csg.braceMode = false;
        csg.bracePreview = null;
        csg.holeMode = false;
        csg.doorPreview = null;
        csg.facePaintMode = false;
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    }
}

// Face-paint mode: click a face, press 1-9 to override that face's scheme.
export function setFacePaintMode(on) {
    const csg = state.csg;
    csg.facePaintMode = !!on;
    if (on) {
        csg.holeMode = false;
        csg.doorPreview = null;
        csg.braceMode = false;
        csg.bracePreview = null;
        csg.pillarMode = false;
        csg.pillarPreview = null;
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
    }
}

export function exitFacePaintMode() {
    state.csg.facePaintMode = false;
}

// Apply a scheme override to the currently-selected face's owning brush.
// Returns true on success, false if no face is selected or it's a baked face.
export function applyFaceSchemeOverride(schemeName) {
    const sel = state.csg.selectedFace;
    if (!sel) return false;
    if (sel.brushId === 0) return false;  // baked faces have no live brush
    const brush = findBrushById(sel.brushId, sel.regionId);
    if (!brush) return false;
    const key = sel.axis + '-' + sel.side;
    if (!brush.schemeOverrides) brush.schemeOverrides = {};
    brush.schemeOverrides[key] = schemeName;
    rebuildAffectedRegions([brush.id]);
    return true;
}

// Clear the override on the currently-selected face (revert to brush.schemeKey).
export function clearFaceSchemeOverride() {
    const sel = state.csg.selectedFace;
    if (!sel) return false;
    if (sel.brushId === 0) return false;
    const brush = findBrushById(sel.brushId, sel.regionId);
    if (!brush || !brush.schemeOverrides) return false;
    const key = sel.axis + '-' + sel.side;
    if (!(key in brush.schemeOverrides)) return false;
    delete brush.schemeOverrides[key];
    rebuildAffectedRegions([brush.id]);
    return true;
}

// ─── Per-triangle zone overrides (Face Paint: arrow up/down) ──────────
// The selected triangle's current zone is looked up from the region's
// triZones array. The cycle list is the zones actually defined in the
// triangle's scheme (typically 0/1/2/3, optionally 5/6/7).
const TRI_OVERRIDE_EPS = WORLD_SCALE * 0.5;

function findTriOverrideIndex(brush, cx, cy, cz) {
    if (!brush.triZoneOverrides) return -1;
    for (let i = 0; i < brush.triZoneOverrides.length; i++) {
        const o = brush.triZoneOverrides[i];
        if (Math.abs(o.cx - cx) < TRI_OVERRIDE_EPS &&
            Math.abs(o.cy - cy) < TRI_OVERRIDE_EPS &&
            Math.abs(o.cz - cz) < TRI_OVERRIDE_EPS) return i;
    }
    return -1;
}

// After a CSG rebuild the triangle sort order may change (zones drive the
// material groups, so changing a zone shuffles the sorted index). Re-link the
// selected triangle by matching its centroid so the highlight and subsequent
// arrow presses keep targeting the same physical triangle.
function relinkSelectedTriangle(regionId, cx, cy, cz) {
    const sel = state.csg.selectedFace;
    if (!sel) return;
    const data = csgRegionMeshes.get(regionId);
    if (!data || !data.triCentroids) return;
    const tc = data.triCentroids;
    for (let i = 0; i < tc.length / 3; i++) {
        if (Math.abs(tc[i * 3] - cx) < TRI_OVERRIDE_EPS &&
            Math.abs(tc[i * 3 + 1] - cy) < TRI_OVERRIDE_EPS &&
            Math.abs(tc[i * 3 + 2] - cz) < TRI_OVERRIDE_EPS) {
            sel.triIndex = i;
            return;
        }
    }
}

function getSelectedTriangleData() {
    const sel = state.csg.selectedFace;
    if (!sel || sel.triIndex == null || sel.regionId == null) return null;
    const data = csgRegionMeshes.get(sel.regionId);
    if (!data || !data.triZones || !data.triCentroids) return null;
    const ti = sel.triIndex;
    if (ti < 0 || ti >= data.triZones.length) return null;
    return {
        zone: data.triZones[ti],
        cx: data.triCentroids[ti * 3],
        cy: data.triCentroids[ti * 3 + 1],
        cz: data.triCentroids[ti * 3 + 2],
    };
}

// Step the selected triangle's zone to the next/prev entry in its scheme's
// defined zones. Persists as a per-triangle override on the owning brush.
// Returns the new zone on success, or null.
export function cycleTriangleZone(direction) {
    const sel = state.csg.selectedFace;
    if (!sel) return null;
    if (sel.brushId === 0) return null;
    const brush = findBrushById(sel.brushId, sel.regionId);
    if (!brush) return null;
    const tri = getSelectedTriangleData();
    if (!tri) return null;

    const faceKey = sel.axis + '-' + sel.side;
    const schemeKey = (brush.schemeOverrides && brush.schemeOverrides[faceKey]) || brush.schemeKey;
    const scheme = TEXTURE_SCHEMES[schemeKey];
    if (!scheme || !scheme.zones) return null;
    const zoneList = Object.keys(scheme.zones).map(Number).sort((a, b) => a - b);
    if (zoneList.length === 0) return null;

    let curIdx = zoneList.indexOf(tri.zone);
    if (curIdx === -1) curIdx = 0;
    const step = direction >= 0 ? 1 : -1;
    const nextIdx = (curIdx + step + zoneList.length) % zoneList.length;
    const newZone = zoneList[nextIdx];

    if (!brush.triZoneOverrides) brush.triZoneOverrides = [];
    const existing = findTriOverrideIndex(brush, tri.cx, tri.cy, tri.cz);
    if (existing >= 0) {
        brush.triZoneOverrides[existing].zone = newZone;
    } else {
        brush.triZoneOverrides.push({ cx: tri.cx, cy: tri.cy, cz: tri.cz, zone: newZone });
    }
    rebuildAffectedRegions([brush.id]);
    relinkSelectedTriangle(sel.regionId, tri.cx, tri.cy, tri.cz);
    return newZone;
}

// Remove the per-triangle override at the selected triangle's centroid.
// Returns true if an override was cleared.
export function clearTriangleZoneOverride() {
    const sel = state.csg.selectedFace;
    if (!sel) return false;
    if (sel.brushId === 0) return false;
    const brush = findBrushById(sel.brushId, sel.regionId);
    if (!brush || !brush.triZoneOverrides || brush.triZoneOverrides.length === 0) return false;
    const tri = getSelectedTriangleData();
    if (!tri) return false;
    const existing = findTriOverrideIndex(brush, tri.cx, tri.cy, tri.cz);
    if (existing < 0) return false;
    brush.triZoneOverrides.splice(existing, 1);
    rebuildAffectedRegions([brush.id]);
    relinkSelectedTriangle(sel.regionId, tri.cx, tri.cy, tri.cz);
    return true;
}

export function exitPillarMode() {
    state.csg.pillarMode = false;
    state.csg.pillarPreview = null;
}

// Compute the box for a pillar given a floor hit. Returns the preview shape
// or null if the face is unsuitable (must be a floor — axis 'y', side 'min').
export function computePillarPreview(hitFace, hitPoint) {
    const csg = state.csg;
    if (!csg.pillarMode || !hitFace || !hitPoint) return null;
    if (hitFace.axis !== 'y' || hitFace.side !== 'min') return null;

    const brush = findBrushById(hitFace.brushId, hitFace.regionId);
    if (!brush || brush.op !== 'subtract') return null;

    const ps = csg.pillarSize;
    const E = WALL_THICKNESS / 2;  // burial epsilon, same as brace

    // Snap cursor X/Z to integer WT and place the pillar centered there
    // (or as centered as integer offsets allow).
    const cursorX = Math.round(hitPoint.x / WORLD_SCALE) - Math.floor(ps / 2);
    const cursorZ = Math.round(hitPoint.z / WORLD_SCALE) - Math.floor(ps / 2);

    // Clamp so the pillar stays inside the room interior
    const x0 = Math.max(brush.minX, Math.min(brush.maxX - ps, cursorX));
    const z0 = Math.max(brush.minZ, Math.min(brush.maxZ - ps, cursorZ));

    // Y spans floor to ceiling, with 0.5 WT burial into both
    const box = {
        x: x0, y: brush.minY - E, z: z0,
        w: ps, h: (brush.maxY - brush.minY) + 2 * E, d: ps,
    };

    csg.pillarPreview = {
        regionId: hitFace.regionId,
        roomBrushId: brush.id,
        box,
    };
    return csg.pillarPreview;
}

export function confirmPillarPlacement() {
    const csg = state.csg;
    if (!csg.pillarPreview) return;
    const { box, roomBrushId, regionId } = csg.pillarPreview;

    const roomBrush = findBrushById(roomBrushId, regionId);
    const schemeKey = (roomBrush && roomBrush.schemeKey) || 'facility_white_tile';
    const floorY    = (roomBrush && roomBrush.floorY)    ?? box.y;

    const b = new BrushDef(csg.nextBrushId++, 'add', box.x, box.y, box.z, box.w, box.h, box.d);
    b.isBrace = true;       // share the brace texturing path
    b.schemeKey = schemeKey;
    b.floorY = floorY;
    state.csg.brushes.push(b);

    csg.pillarMode = false;
    csg.pillarPreview = null;
    assignBrushToRegion(b);
    rebuildAffectedRegions([b.id]);
}

// ─── Bake / Retexture / Delete ───────────────────────────────────────

// Bake the region containing the currently selected face.
// (Or all regions if nothing is selected — equivalent to "bake all".)
export function bakeCurrentRegion() {
    const csg = state.csg;
    let bakedAny = false;

    for (const [, data] of csgRegionMeshes) {
        if (csg.selectedFace && csg.selectedFace.regionId !== data.region.id) continue;
        const count = data.region.bake();
        if (count) {
            bakedAny = true;
            csg.totalBakedBrushes += count;
        }
    }

    if (bakedAny) {
        // Bake mutates region.brushes by removing them. Sync state.csg.brushes
        // so the user-visible brush list reflects what's still un-baked.
        // Bake removed brushes from the per-region brushes array — but
        // state.csg.brushes is the source of truth. Find which brush ids were baked
        // (i.e. not present in any region's brushes array anymore) and remove them.
        const stillUnbaked = new Set();
        for (const [, data] of csgRegionMeshes) {
            for (const b of data.region.brushes) stillUnbaked.add(b.id);
        }
        state.csg.brushes = state.csg.brushes.filter(b => stillUnbaked.has(b.id));

        csg.selectedFace = null;
        csg.selectedFaces = [];
        csg.activeBrush = null;
        csg.activeOp = null;
        csg.activeSide = null;
        csg.selSizeU = 0;
        csg.selSizeV = 0;

        rebuildAllCSG();
    }
}

// Retexture all brushes in the same room (flood-fill stops at door/hole frames).
export function retextureRoom(schemeKey) {
    const csg = state.csg;
    const sel = csg.selectedFace;
    if (!sel || sel.brushId === 0) return;
    const startBrush = state.csg.brushes.find(b => b.id === sel.brushId);
    if (!startBrush || startBrush.isDoorframe || startBrush.isHoleFrame) return;

    // Stair-step click: just retexture that brush (floorY stays as-is).
    if (startBrush.isStairStep) {
        startBrush.schemeKey = schemeKey;
        rebuildAffectedRegions([startBrush.id]);
        return;
    }

    const roomIds = findRoomBrushes(startBrush, state.csg.brushes);
    const roomBrushes = state.csg.brushes.filter(b => roomIds.has(b.id));
    // floorY is per-brush (pit subtracts anchor to their own minY, stair voids
    // to the adjoining room floor) and is kept in sync on every y-min edit.
    // Don't clobber it here — that's what breaks pit wall splits on retexture.
    for (const b of roomBrushes) {
        b.schemeKey = schemeKey;
    }

    rebuildAffectedRegions([...roomIds]);
}

// Delete the brush whose face is currently selected.
export function deleteSelectedBrush() {
    const csg = state.csg;
    const sel = csg.selectedFace;
    if (!sel || sel.brushId === 0) return;
    const idx = state.csg.brushes.findIndex(b => b.id === sel.brushId);
    if (idx < 0) return;
    state.csg.brushes.splice(idx, 1);

    csg.selectedFace = null;
    csg.selectedFaces = [];
    csg.activeBrush = null;
    csg.activeOp = null;
    csg.activeSide = null;

    rebuildAllCSG();
}

