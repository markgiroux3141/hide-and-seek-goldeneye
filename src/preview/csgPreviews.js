// CSG selection + hole/door preview rendering.
// Ported from spike/csg/main.js (updateSelectionPreview, updateHolePreview).
//
// Both previews are rebuilt every frame in the animate loop. They sit slightly
// in front of the selected face (polygonOffset) so they don't z-fight.

import * as THREE from 'three';
import { state } from '../state.js';
import { scene } from '../scene/setup.js';
import { isPointerLocked } from '../input/input.js';
import { csgRegionMeshes } from '../mesh/csgMesh.js';
import { pickCSGFace } from '../raycaster.js';
import {
    facesMatch, getSelectedFaceInfo, getFaceUVInfo, worldToFaceUV, computeHolePreview, computeBracePreview, computePillarPreview,
} from '../csg/csgActions.js';
import { WORLD_SCALE } from '../core/constants.js';

const SEL_OFFSET = 0.002;
const HOLE_OFFSET = 0.003;
const BRACE_OFFSET = 0.003;

let selectionMesh = null;
const selectionMat = new THREE.MeshBasicMaterial({
    color: 0xff6644, transparent: true, opacity: 0.35,
    side: THREE.DoubleSide, depthTest: true,
    polygonOffset: true, polygonOffsetFactor: -2,
});

let multiSelectionMeshes = [];

let holeMesh = null;
const holeMat = new THREE.MeshBasicMaterial({
    color: 0xffcc00, transparent: true, opacity: 0.4,
    side: THREE.DoubleSide, depthTest: true,
    polygonOffset: true, polygonOffsetFactor: -2,
});

let braceMeshes = [];
const braceMat = new THREE.MeshBasicMaterial({
    color: 0xffcc00, transparent: true, opacity: 0.5,
    side: THREE.DoubleSide, depthTest: true,
    polygonOffset: true, polygonOffsetFactor: -2,
});

let pillarMesh = null;

function disposeMesh(mesh) {
    if (!mesh) return;
    scene.remove(mesh);
    if (mesh.geometry) mesh.geometry.dispose();
}

// Build a flat quad on the given face plane spanning u0..u1, v0..v1 (WT units).
// `offset` pushes it slightly off the surface to avoid z-fighting.
function buildFaceQuad(face, u0, u1, v0, v1, offset) {
    const { axis, side, position } = face;
    const pos = position * WORLD_SCALE;
    const o = side === 'min' ? offset : -offset;

    let x0, x1, y0, y1, z0, z1;
    if (axis === 'x') {
        x0 = x1 = pos + o;
        z0 = u0 * WORLD_SCALE; z1 = u1 * WORLD_SCALE;
        y0 = v0 * WORLD_SCALE; y1 = v1 * WORLD_SCALE;
    } else if (axis === 'y') {
        y0 = y1 = pos + o;
        x0 = u0 * WORLD_SCALE; x1 = u1 * WORLD_SCALE;
        z0 = v0 * WORLD_SCALE; z1 = v1 * WORLD_SCALE;
    } else {
        z0 = z1 = pos + o;
        x0 = u0 * WORLD_SCALE; x1 = u1 * WORLD_SCALE;
        y0 = v0 * WORLD_SCALE; y1 = v1 * WORLD_SCALE;
    }

    const positions = new Float32Array(axis === 'x' ? [
        x0, y0, z0,  x0, y1, z0,  x0, y1, z1,
        x0, y0, z0,  x0, y1, z1,  x0, y0, z1,
    ] : axis === 'y' ? [
        x0, y0, z0,  x0, y0, z1,  x1, y0, z1,
        x0, y0, z0,  x1, y0, z1,  x1, y0, z0,
    ] : [
        x0, y0, z0,  x0, y1, z0,  x1, y1, z0,
        x0, y0, z0,  x1, y1, z0,  x1, y0, z0,
    ]);

    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geo.computeVertexNormals();
    return geo;
}

// Build a highlight geometry for the exact triangle at sel.triIndex on the
// region's live CSG mesh. Pushes the verts slightly along the face normal so
// the highlight doesn't z-fight the surface. Returns null if the index is
// stale (e.g. after a rebuild that reordered tris).
function buildTriangleHighlight(sel, offset) {
    if (sel.regionId == null || sel.triIndex == null) return null;
    const data = csgRegionMeshes.get(sel.regionId);
    if (!data) return null;
    const geom = data.mesh.geometry;
    const idx = geom.index;
    const pos = geom.getAttribute('position');
    if (!idx || !pos) return null;
    const ti = sel.triIndex;
    if (ti < 0 || ti * 3 + 2 >= idx.count) return null;
    const i0 = idx.getX(ti * 3);
    const i1 = idx.getX(ti * 3 + 1);
    const i2 = idx.getX(ti * 3 + 2);

    // Normal from the face axis/side — cheaper than computing from verts and
    // matches the same offset direction used for face quads.
    let nx = 0, ny = 0, nz = 0;
    const s = sel.side === 'min' ? 1 : -1;
    if (sel.axis === 'x') nx = s;
    else if (sel.axis === 'y') ny = s;
    else nz = s;

    const positions = new Float32Array(9);
    const indices = [i0, i1, i2];
    for (let k = 0; k < 3; k++) {
        const vi = indices[k];
        positions[k * 3]     = pos.getX(vi) + nx * offset;
        positions[k * 3 + 1] = pos.getY(vi) + ny * offset;
        positions[k * 3 + 2] = pos.getZ(vi) + nz * offset;
    }
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geo.computeVertexNormals();
    return geo;
}

// Update the orange selection preview (rectangle on the selected face).
// In Face Paint mode, draws just the clicked triangle instead.
// Called every frame from the indoor animate loop.
export function updateCSGSelectionPreview(camera) {
    disposeMesh(selectionMesh);
    selectionMesh = null;

    const sel = state.csg.selectedFace;
    if (!sel || !isPointerLocked()) return;
    if (state.csg.holeMode) return; // hole preview takes over while in hole mode

    // Face Paint: highlight the exact triangle the user picked (stays locked
    // regardless of where the cursor currently points), so arrow up/down has
    // an obvious target.
    if (state.csg.facePaintMode) {
        const geo = buildTriangleHighlight(sel, SEL_OFFSET * WORLD_SCALE);
        if (!geo) return;
        selectionMesh = new THREE.Mesh(geo, selectionMat);
        scene.add(selectionMesh);
        return;
    }

    const faceInfo = getSelectedFaceInfo();
    if (!faceInfo) return;

    // Only show preview if the user is currently looking at the selected face
    const hit = pickCSGFace(camera, csgRegionMeshes);
    if (!hit || !facesMatch(hit, sel)) return;

    const { axis } = sel;
    const uv = worldToFaceUV(hit.point, axis);

    const sU = state.csg.selSizeU <= 0 ? faceInfo.uSize : Math.min(state.csg.selSizeU, faceInfo.uSize);
    const sV = state.csg.selSizeV <= 0 ? faceInfo.vSize : Math.min(state.csg.selSizeV, faceInfo.vSize);

    let u0 = Math.round(uv.u - sU / 2);
    let v0 = Math.round(uv.v - sV / 2);
    u0 = Math.max(faceInfo.uMin, Math.min(u0, faceInfo.uMax - sU));
    v0 = Math.max(faceInfo.vMin, Math.min(v0, faceInfo.vMax - sV));
    const u1 = u0 + sU;
    const v1 = v0 + sV;

    state.csg.selU0 = u0; state.csg.selU1 = u1;
    state.csg.selV0 = v0; state.csg.selV1 = v1;

    const geo = buildFaceQuad(sel, u0, u1, v0, v1, SEL_OFFSET);
    selectionMesh = new THREE.Mesh(geo, selectionMat);
    scene.add(selectionMesh);
}

// Persistent full-face highlights for every face in state.csg.selectedFaces.
// Unlike the primary preview, these are drawn even when the crosshair is not
// on them, so the user can see which faces are locked into the multi-push.
export function updateCSGMultiSelectionPreview() {
    for (const m of multiSelectionMeshes) disposeMesh(m);
    multiSelectionMeshes = [];

    const faces = state.csg.selectedFaces;
    if (!faces || faces.length === 0) return;

    for (const f of faces) {
        const brush = state.csg.brushes.find(b => b.id === f.brushId);
        if (!brush) continue;
        const info = getFaceUVInfo(brush, f.axis);
        if (!info) continue;
        const geo = buildFaceQuad(f, info.uMin, info.uMax, info.vMin, info.vMax, SEL_OFFSET);
        const mesh = new THREE.Mesh(geo, selectionMat);
        scene.add(mesh);
        multiSelectionMeshes.push(mesh);
    }
}

// Update the yellow hole/door preview while in hole mode.
export function updateCSGHolePreview(camera) {
    disposeMesh(holeMesh);
    holeMesh = null;

    if (!state.csg.holeMode || !isPointerLocked()) return;

    const hit = pickCSGFace(camera, csgRegionMeshes);
    if (!hit) {
        state.csg.doorPreview = null;
        return;
    }

    const preview = computeHolePreview(hit, hit.point);
    if (!preview) return;

    const geo = buildFaceQuad(preview.face, preview.u0, preview.u1, preview.v0, preview.v1, HOLE_OFFSET);
    holeMesh = new THREE.Mesh(geo, holeMat);
    scene.add(holeMesh);
}

// Build a translucent box for an arch segment given WT-space {x,y,z,w,h,d}.
// `inset` shrinks the box slightly inside its bounds so it doesn't z-fight
// with whatever wall/ceiling face it sits flush against.
function buildBraceBox(r, inset) {
    const sx = r.w * WORLD_SCALE - 2 * inset;
    const sy = r.h * WORLD_SCALE - 2 * inset;
    const sz = r.d * WORLD_SCALE - 2 * inset;
    const geo = new THREE.BoxGeometry(sx, sy, sz);
    const cx = (r.x + r.w / 2) * WORLD_SCALE;
    const cy = (r.y + r.h / 2) * WORLD_SCALE;
    const cz = (r.z + r.d / 2) * WORLD_SCALE;
    geo.translate(cx, cy, cz);
    return geo;
}

// Update the yellow brace arch preview while in brace mode.
export function updateCSGBracePreview(camera) {
    for (const m of braceMeshes) disposeMesh(m);
    braceMeshes = [];

    if (!state.csg.braceMode || !isPointerLocked()) {
        state.csg.bracePreview = null;
        return;
    }

    const hit = pickCSGFace(camera, csgRegionMeshes);
    if (!hit) {
        state.csg.bracePreview = null;
        return;
    }

    const preview = computeBracePreview(hit, hit.point);
    if (!preview) return;

    for (const r of [preview.wall1, preview.ceiling, preview.wall2]) {
        const geo = buildBraceBox(r, BRACE_OFFSET);
        const mesh = new THREE.Mesh(geo, braceMat);
        scene.add(mesh);
        braceMeshes.push(mesh);
    }
}

// Update the yellow pillar preview while in pillar mode.
export function updateCSGPillarPreview(camera) {
    disposeMesh(pillarMesh);
    pillarMesh = null;

    if (!state.csg.pillarMode || !isPointerLocked()) {
        state.csg.pillarPreview = null;
        return;
    }

    const hit = pickCSGFace(camera, csgRegionMeshes);
    if (!hit) {
        state.csg.pillarPreview = null;
        return;
    }

    const preview = computePillarPreview(hit, hit.point);
    if (!preview) return;

    const geo = buildBraceBox(preview.box, BRACE_OFFSET);
    pillarMesh = new THREE.Mesh(geo, braceMat);
    scene.add(pillarMesh);
}

// ─── CSG Stair Preview ─────────────────────────────────────────────
// Shows translucent boxes for the two void brushes while pendingStairOp is active.

let stairPreviewMeshes = [];
let stairPreviewCount = -1;

const stairVoidMat = new THREE.MeshBasicMaterial({
    color: 0x44aaff, transparent: true, opacity: 0.2,
    side: THREE.DoubleSide, depthTest: true,
});
const stairDestMat = new THREE.MeshBasicMaterial({
    color: 0x44ff88, transparent: true, opacity: 0.3,
    side: THREE.DoubleSide, depthTest: true,
});

function disposeStairPreview() {
    for (const m of stairPreviewMeshes) disposeMesh(m);
    stairPreviewMeshes = [];
    stairPreviewCount = -1;
}

// Build a translucent box from WT-space bounds and add to scene.
function addPreviewBox(axis, normalLo, normalHi, yMin, yMax, uLo, uHi, mat) {
    const nw = (normalHi - normalLo) * WORLD_SCALE;
    const vh = (yMax - yMin) * WORLD_SCALE;
    const uw = (uHi - uLo) * WORLD_SCALE;
    const geo = new THREE.BoxGeometry(
        axis === 'x' ? nw : uw, vh, axis === 'x' ? uw : nw,
    );
    const cx = axis === 'x' ? (normalLo + normalHi) / 2 : (uLo + uHi) / 2;
    const cy = (yMin + yMax) / 2;
    const cz = axis === 'x' ? (uLo + uHi) / 2 : (normalLo + normalHi) / 2;
    geo.translate(cx * WORLD_SCALE, cy * WORLD_SCALE, cz * WORLD_SCALE);
    const mesh = new THREE.Mesh(geo, mat);
    scene.add(mesh);
    stairPreviewMeshes.push(mesh);
}

export function updateCSGStairPreview() {
    const op = state.csg.pendingStairOp;
    if (!op) {
        if (stairPreviewMeshes.length) disposeStairPreview();
        return;
    }

    if (op.stepCount === stairPreviewCount) return;
    disposeStairPreview();
    stairPreviewCount = op.stepCount;

    const { axis, side, facePos, selU0, selU1, floor, H, direction, stepCount } = op;
    const dir = side === 'max' ? 1 : -1;

    // Brush 1: main stairwell (same math as confirmStairOp)
    let b1nLo, b1nHi, b1yMin, b1yMax;
    if (dir === 1) { b1nLo = facePos; b1nHi = facePos + stepCount; }
    else           { b1nLo = facePos - stepCount; b1nHi = facePos; }
    if (direction === 'down') { b1yMin = floor - stepCount; b1yMax = H; }
    else                      { b1yMin = floor; b1yMax = H + stepCount; }

    addPreviewBox(axis, b1nLo, b1nHi, b1yMin, b1yMax, selU0, selU1, stairVoidMat);

    // Brush 2: destination corridor
    let b2nLo, b2nHi, b2yMin, b2yMax;
    if (dir === 1) { b2nLo = facePos + stepCount; b2nHi = facePos + stepCount + 1; }
    else           { b2nLo = facePos - stepCount - 1; b2nHi = facePos - stepCount; }
    if (direction === 'down') { b2yMin = floor - stepCount; b2yMax = H - stepCount; }
    else                      { b2yMin = floor + stepCount; b2yMax = H + stepCount; }

    addPreviewBox(axis, b2nLo, b2nHi, b2yMin, b2yMax, selU0, selU1, stairDestMat);
}
