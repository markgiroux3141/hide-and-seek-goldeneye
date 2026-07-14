// Cave sculpt mode. Entered via K when a cave exists in the selected region;
// takes over mouse/keyboard until exit. While active:
//   • containing region's CSG mesh is hidden by default (H toggles it)
//   • cave chunk group stays visible
//   • LMB applies the current brush at the crosshair target
//   • scroll adjusts radius; [ / ] adjust strength; F/R/E toggle special modes
//
// Performance: strokes update the CaveWorld only — no CSG rebuild fires mid-
// sculpt. The region's mouth brush + shell are synced in a single rebuild on
// exitSculptMode. This keeps the hot path as cheap as the standalone spike.

import * as THREE from 'three';
import { scene } from '../scene/setup.js';
import { WORLD_SCALE } from '../core/constants.js';
import { state } from '../state.js';
import { isPointerLocked, reacquirePointerLock } from '../input/input.js';
import { csgRegionMeshes } from '../mesh/csgMesh.js';
import { getEntry, remeshDirty, buildCaveClipArray } from '../mesh/caveMesh.js';
import { placeExitRoomFromCaveHit, exitRoomAabbFromHit } from '../csg/csgActions.js';

const MODE_CODE = { subtract: 0, add: 1, flatten: 2, smooth: 3, expand: 4 };

const MIN_RADIUS = 0.6, MAX_RADIUS = 10.0;
const MIN_STRENGTH = 0.05, MAX_STRENGTH = 2.0;
const FALLBACK_FWD = 3.0;   // meters ahead of camera when no chunk hit

const sculpt = {
    active: false,
    regionId: null,
    caveId: null,
    hiddenRegionId: null,   // region whose CSG mesh was hidden on enter; restored on exit
    csgVisible: false,      // H toggles; starts hidden for the clean carve view
    mode: 'subtract',
    radius: 1.5,
    strength: 0.4,
    lmbDown: false,
    shiftDown: false,
    gizmoVisible: true,
    placeExitPending: false,   // P submode: next LMB places an exit room
};

// Latest valid exit-placement aim, updated each frame while in the submode.
// Kept module-local so the mousedown handler can commit it immediately.
// { aabb: {x,y,z,w,h,d}, hitPoint, hitNormal } or null when no cave hit.
let _exitAim = null;

const _raycaster = new THREE.Raycaster();
const _center = new THREE.Vector2(0, 0);
const _fwd = new THREE.Vector3();
const _target = new THREE.Vector3();

// ─── Preview gizmos ─────────────────────────────────────────────────
const _sphereGeom = new THREE.SphereGeometry(1, 24, 16);
const _sphereMats = {
    subtract: new THREE.MeshBasicMaterial({ color: 0xff4444, wireframe: true, transparent: true, opacity: 0.6 }),
    add:      new THREE.MeshBasicMaterial({ color: 0x44ff66, wireframe: true, transparent: true, opacity: 0.6 }),
    smooth:   new THREE.MeshBasicMaterial({ color: 0x44ffdd, wireframe: true, transparent: true, opacity: 0.6 }),
    expand:   new THREE.MeshBasicMaterial({ color: 0xff8844, wireframe: true, transparent: true, opacity: 0.6 }),
};
const _spherePreview = new THREE.Mesh(_sphereGeom, _sphereMats.subtract);
_spherePreview.visible = false;

const _ringGeom = new THREE.BufferGeometry();
{
    const segs = 48;
    const pts = new Float32Array(segs * 3);
    for (let i = 0; i < segs; i++) {
        const a = (i / segs) * Math.PI * 2;
        pts[i * 3    ] = Math.cos(a);
        pts[i * 3 + 1] = 0;
        pts[i * 3 + 2] = Math.sin(a);
    }
    _ringGeom.setAttribute('position', new THREE.BufferAttribute(pts, 3));
}
const _ringPreview = new THREE.LineLoop(
    _ringGeom,
    new THREE.LineBasicMaterial({ color: 0x44aaff, transparent: true, opacity: 0.7 })
);
_ringPreview.visible = false;

// Exit-room placement preview — wireframe AABB updated each frame while P
// mode is active. Geometry is a unit cube's edges; scaled + positioned to
// match the snapped placement AABB.
const _exitBoxPreview = new THREE.LineSegments(
    new THREE.EdgesGeometry(new THREE.BoxGeometry(1, 1, 1)),
    new THREE.LineBasicMaterial({ color: 0xffaa44, transparent: true, opacity: 0.85 })
);
_exitBoxPreview.visible = false;

let _previewsAttached = false;
function attachPreviews() {
    if (_previewsAttached) return;
    scene.add(_spherePreview);
    scene.add(_ringPreview);
    scene.add(_exitBoxPreview);
    _previewsAttached = true;
}

// ─── LMB / Shift tracking (module-local; only active in sculpt mode) ─
window.addEventListener('mousedown', (e) => {
    if (!sculpt.active || e.button !== 0) return;
    // In place-exit submode, LMB commits the room instead of carving.
    if (sculpt.placeExitPending) {
        e.preventDefault();
        commitExitPlacement();
        return;
    }
    sculpt.lmbDown = true;
});
window.addEventListener('mouseup', (e) => {
    if (e.button === 0) sculpt.lmbDown = false;
});
window.addEventListener('keydown', (e) => {
    if (e.key === 'Shift') sculpt.shiftDown = true;
});
window.addEventListener('keyup', (e) => {
    if (e.key === 'Shift') sculpt.shiftDown = false;
});

// ─── Public API ─────────────────────────────────────────────────────

export function isSculpting() {
    return sculpt.active;
}

export function getSculptState() {
    return sculpt;
}

// Consulted by csgMesh.buildRegionMesh so a rebuild during sculpt (shouldn't
// happen in the decoupled path, but safe) preserves the hide state.
export function isRegionHiddenForSculpt(regionId) {
    return sculpt.active && sculpt.hiddenRegionId === regionId && !sculpt.csgVisible;
}

export function enterSculptMode(regionId, caveId) {
    if (sculpt.active) return false;
    const entry = getEntry(regionId, caveId);
    if (!entry) return false;

    attachPreviews();

    sculpt.active = true;
    sculpt.regionId = regionId;
    sculpt.caveId = caveId;
    sculpt.hiddenRegionId = regionId;
    sculpt.csgVisible = false;

    const data = csgRegionMeshes.get(regionId);
    if (data && data.mesh) data.mesh.visible = false;

    reacquirePointerLock();
    return true;
}

export function exitSculptMode() {
    if (!sculpt.active) return;

    _spherePreview.visible = false;
    _ringPreview.visible = false;
    _exitBoxPreview.visible = false;
    _exitAim = null;

    const hiddenRegionId = sculpt.hiddenRegionId;

    sculpt.active = false;
    sculpt.regionId = null;
    sculpt.caveId = null;
    sculpt.hiddenRegionId = null;
    sculpt.csgVisible = false;
    sculpt.lmbDown = false;
    sculpt.placeExitPending = false;

    // Restore CSG visibility. No catch-up rebuild: nothing CSG-side changed
    // during sculpt (caves don't own CSG brushes anymore, and exit-room
    // placement triggers its own rebuild inline).
    const data = csgRegionMeshes.get(hiddenRegionId);
    if (data && data.mesh) data.mesh.visible = true;
}

// H hotkey — peek at the player view (or hide it again) without leaving
// sculpt. Does not rebuild CSG; the mesh is stale mouth-sized until exit.
export function toggleCsgVisible() {
    if (!sculpt.active) return sculpt.csgVisible;
    sculpt.csgVisible = !sculpt.csgVisible;
    const data = csgRegionMeshes.get(sculpt.hiddenRegionId);
    if (data && data.mesh) data.mesh.visible = sculpt.csgVisible;
    return sculpt.csgVisible;
}

export function setMode(mode) {
    if (!sculpt.active) return;
    sculpt.mode = mode;
}

export function toggleMode(mode) {
    if (!sculpt.active) return;
    sculpt.mode = sculpt.mode === mode ? 'subtract' : mode;
}

export function adjustRadius(delta) {
    sculpt.radius = Math.max(MIN_RADIUS, Math.min(MAX_RADIUS, sculpt.radius + delta));
}

export function adjustStrength(delta) {
    sculpt.strength = Math.max(MIN_STRENGTH, Math.min(MAX_STRENGTH, sculpt.strength + delta));
}

// Adjust the current exit-room dimensions (used by the P-submode preview +
// the brush placed on commit). `dim` is 'depth' | 'width' | 'height'.
export function adjustExitRoomSize(dim, delta) {
    const MIN = 2, MAX = 32;
    const s = state.csg.exitRoomSize;
    if (!(dim in s)) return;
    s[dim] = Math.max(MIN, Math.min(MAX, s[dim] + delta));
}

export function toggleGizmoVisible() {
    sculpt.gizmoVisible = !sculpt.gizmoVisible;
    return sculpt.gizmoVisible;
}

// Place-exit submode — the next LMB click on the cave wall drops a 4×4×4 WT
// subtract room at the hit. Toggled with P; cancel with P again or Esc.
export function togglePlaceExitMode() {
    if (!sculpt.active) return false;
    sculpt.placeExitPending = !sculpt.placeExitPending;
    if (!sculpt.placeExitPending) {
        _exitBoxPreview.visible = false;
        _exitAim = null;
    }
    return sculpt.placeExitPending;
}

export function isPlacingExit() {
    return sculpt.active && sculpt.placeExitPending;
}

// Per-frame update: raycast, update preview, apply brush if LMB held.
export function tick(camera, dt) {
    if (!sculpt.active) return;

    // Auto-exit if pointer lock lost (Esc) or the target entry vanished.
    if (!isPointerLocked()) { exitSculptMode(); return; }
    const entry = getEntry(sculpt.regionId, sculpt.caveId);
    if (!entry) { exitSculptMode(); return; }

    // Place-exit submode: hide sculpt gizmos, update exit preview, skip carve.
    if (sculpt.placeExitPending) {
        _spherePreview.visible = false;
        _ringPreview.visible = false;
        updateExitPlacementPreview(camera, entry);
        return;
    } else {
        _exitBoxPreview.visible = false;
    }

    // Resolve brush target: ray from screen-center against cave chunk meshes;
    // fallback to a fixed distance ahead of the camera so the preview stays
    // anchored even when aimed at empty space.
    camera.updateMatrixWorld();
    _raycaster.setFromCamera(_center, camera);
    const chunkList = Array.from(entry.chunkMeshes.values());
    const hits = chunkList.length ? _raycaster.intersectObjects(chunkList, false) : [];
    if (hits.length > 0) {
        _target.copy(hits[0].point);
    } else {
        camera.getWorldDirection(_fwd);
        _target.copy(camera.position).addScaledVector(_fwd, FALLBACK_FWD);
    }

    const mode = sculpt.mode === 'subtract' && sculpt.shiftDown ? 'add' : sculpt.mode;
    const isFlatten = mode === 'flatten';
    const isExpand = mode === 'expand';

    const brushCenter = isExpand ? camera.position : _target;

    const showGizmo = sculpt.gizmoVisible;
    _spherePreview.visible = showGizmo && !isFlatten;
    _spherePreview.position.copy(brushCenter);
    _spherePreview.scale.setScalar(sculpt.radius);
    _spherePreview.material = _sphereMats[mode] || _sphereMats.subtract;

    _ringPreview.visible = showGizmo && isFlatten;
    _ringPreview.position.copy(_target);
    _ringPreview.scale.setScalar(sculpt.radius);

    if (sculpt.lmbDown && isOutsideAnchorRoom(brushCenter)) {
        const changed = entry.world.apply_brush(
            MODE_CODE[mode],
            brushCenter.x, brushCenter.y, brushCenter.z,
            sculpt.radius, sculpt.strength, dt,
        );
        if (changed) {
            remeshDirty(entry);
            growExtentAndRefreshClip(entry, brushCenter, sculpt.radius);
        }
    }
}

// ─── Place-exit submode helpers ────────────────────────────────────
// Raycasts the cave mesh, snaps the hit normal to a cardinal axis, derives
// the 4×4×4 WT AABB via csgActions.exitRoomAabbFromHit, and positions the
// wireframe preview. Stashes the aim in _exitAim for commit.
function updateExitPlacementPreview(camera, entry) {
    camera.updateMatrixWorld();
    _raycaster.setFromCamera(_center, camera);
    const chunkList = Array.from(entry.chunkMeshes.values());
    const hits = chunkList.length ? _raycaster.intersectObjects(chunkList, false) : [];
    if (hits.length === 0 || !hits[0].face) {
        _exitAim = null;
        _exitBoxPreview.visible = false;
        return;
    }

    const hitPoint = hits[0].point;
    const hitNormal = hits[0].face.normal;
    const nx = Math.abs(hitNormal.x), ny = Math.abs(hitNormal.y), nz = Math.abs(hitNormal.z);
    let axis, sign;
    if (nx >= ny && nx >= nz)  { axis = 'x'; sign = hitNormal.x >= 0 ? 1 : -1; }
    else if (ny >= nz)         { axis = 'y'; sign = hitNormal.y >= 0 ? 1 : -1; }
    else                       { axis = 'z'; sign = hitNormal.z >= 0 ? 1 : -1; }

    const aabb = exitRoomAabbFromHit(hitPoint, axis, sign);
    _exitAim = { hitPoint: hitPoint.clone(), hitNormal: hitNormal.clone(), aabb };

    // Position preview in world meters.
    const s = WORLD_SCALE;
    _exitBoxPreview.position.set(
        (aabb.x + aabb.w / 2) * s,
        (aabb.y + aabb.h / 2) * s,
        (aabb.z + aabb.d / 2) * s,
    );
    _exitBoxPreview.scale.set(aabb.w * s, aabb.h * s, aabb.d * s);
    _exitBoxPreview.visible = true;
}

function commitExitPlacement() {
    if (!_exitAim) return;
    const regionData = csgRegionMeshes.get(sculpt.regionId);
    const cave = regionData?.region.caves.find(c => c.id === sculpt.caveId);
    if (!cave) return;
    // placeExitRoomFromCaveHit also auto-carves a cave sphere inside the new
    // room so the cave mesh drops out and the room is visible from inside.
    placeExitRoomFromCaveHit(cave, _exitAim.hitPoint, _exitAim.hitNormal);
    sculpt.placeExitPending = false;
    _exitAim = null;
    _exitBoxPreview.visible = false;
}

// True when the brush center sits on the outward side of the anchor wall —
// i.e. still in the cave's "rock beyond the room" half-space. Prevents the
// user from sculpting back through the mouth into the source room. Only
// checks the center, not the full brush sphere, so a large brush aimed near
// the wall can still bleed slightly inward; that's acceptable for now and
// stays consistent with the extent-based mouth sync.
function isOutsideAnchorRoom(brushCenter) {
    const regionData = csgRegionMeshes.get(sculpt.regionId);
    if (!regionData) return true;
    const cave = regionData.region.caves.find(c => c.id === sculpt.caveId);
    const source = cave && cave.anchorFaces && cave.anchorFaces[0];
    if (!source) return true;
    const { axis, side, position } = source;
    const wallPlane = position * WORLD_SCALE;
    const dir = side === 'max' ? 1 : -1;
    const n = axis === 'x' ? brushCenter.x : axis === 'y' ? brushCenter.y : brushCenter.z;
    return (n - wallPlane) * dir > 0;
}

// ─── Extent growth + live clip refresh ─────────────────────────────
// Expands cave.extentAabb to include the current stroke, then immediately
// pushes the new envelope to the CaveWorld via set_boundary_clip. Cheap —
// no CSG rebuild runs during sculpt. The mouth brush + region shell catch
// up in a single rebuild on exitSculptMode.
const EXTENT_BUFFER = WORLD_SCALE * 2;   // keep clip ~2 WT ahead of the brush surface

function growExtentAndRefreshClip(entry, center, radius) {
    const regionData = csgRegionMeshes.get(sculpt.regionId);
    if (!regionData || !regionData.region) return;
    const region = regionData.region;
    const cave = region.caves.find(c => c.id === sculpt.caveId);
    if (!cave) return;
    if (!cave.extentAabb) {
        cave.extentAabb = {
            minX: center.x, minY: center.y, minZ: center.z,
            maxX: center.x, maxY: center.y, maxZ: center.z,
        };
    }

    const r = radius + EXTENT_BUFFER;
    const e = cave.extentAabb;
    let grew = false;
    if (center.x - r < e.minX) { e.minX = center.x - r; grew = true; }
    if (center.y - r < e.minY) { e.minY = center.y - r; grew = true; }
    if (center.z - r < e.minZ) { e.minZ = center.z - r; grew = true; }
    if (center.x + r > e.maxX) { e.maxX = center.x + r; grew = true; }
    if (center.y + r > e.maxY) { e.maxY = center.y + r; grew = true; }
    if (center.z + r > e.maxZ) { e.maxZ = center.z + r; grew = true; }

    if (grew) {
        entry.world.set_boundary_clip(buildCaveClipArray(cave));
    }
}
