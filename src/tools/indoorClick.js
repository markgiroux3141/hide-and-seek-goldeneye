// Indoor mode mousedown handler

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { state, saveUndoState } from '../state.js';
import { isPointerLocked } from '../input/input.js';
import { showMessage } from '../hud/hud.js';
import { scene } from '../scene/setup.js';
import { pickCSGFace, pickLight, pickAny, pickFaceAny, pickPlatformOrStair } from '../raycaster.js';
import { snapToWTGrid } from '../actions.js';
import { selectFaceAtCrosshair, toggleFaceInMultiSelection, confirmHolePlacement, confirmBracePlacement, confirmPillarPlacement } from '../csg/csgActions.js';
import { csgRegionMeshes } from '../mesh/csgMesh.js';

const _bakedRaycaster = new THREE.Raycaster();
const _bakedScreenCenter = new THREE.Vector2(0, 0);
import { Platform } from '../core/Platform.js';
import { StairRun } from '../core/StairRun.js';
import { PointLight } from '../core/PointLight.js';
import {
    platformMeshes,
    rebuildPlatform, rebuildStairRun, rebuildConnectedStairRuns,
    rebuildLight, updateLightSelection, getLightPickTargets,
} from '../mesh/MeshManager.js';
import { stairRunMeshes } from '../mesh/MeshManager.js';
import { closestPlatformEdge, closestOffsetOnEdge, projectCrosshairOntoEdge, bestEdgeForDirection } from './platformEdgeUtils.js';
import { findFloorYAt } from '../geometry/platformGeometry.js';
import { clearPlatformToolState, clearLightToolState } from './ToolManager.js';
import { DEFAULT_LIGHT_Y_OFFSET } from '../core/constants.js';
import { sliceTrianglesAgainstPrisms } from './bakedTriangleSlicer.js';

export function handleIndoorClick(e, { gizmo, camera }) {
    if (!isPointerLocked() || e.button !== 0) return;

    // Post-bake cleanup tool — click on a baked triangle to SELECT it (red
    // overlay). The actual deletion happens when the user presses Delete
    // (handled in indoorKeys.js). Esc clears the selection without deleting.
    // Selection state + the overlay mesh live in this module; indoorKeys
    // imports deleteBakedHighlight + clearBakedHighlight to act on them.
    if (state.isBaked) {
        if (!state.bakedMesh) return;
        _bakedRaycaster.setFromCamera(_bakedScreenCenter, camera);
        const hits = _bakedRaycaster.intersectObjects(state.bakedMesh.children, false);
        if (hits.length === 0) return;     // miss = preserve current highlight
        const hit = hits[0];
        if (hit.faceIndex == null) return;
        setBakedHighlight(hit.object, hit.faceIndex);
        return;
    }

    // CSG tool — click selects faces; in hole/brace/pillar mode, click confirms placement
    if (state.tool === 'csg' && !state.isBaked) {
        if (state.csg.pillarMode) {
            saveUndoState();
            confirmPillarPlacement();
            return;
        }
        if (state.csg.braceMode) {
            saveUndoState();
            confirmBracePlacement();
            return;
        }
        if (state.csg.holeMode) {
            saveUndoState();
            confirmHolePlacement();
            return;
        }
        const csgHit = pickCSGFace(camera, csgRegionMeshes);
        if (e.shiftKey) toggleFaceInMultiSelection(csgHit);
        else selectFaceAtCrosshair(csgHit);
        return;
    }

    // Light tool click handling
    if (state.tool === 'light') {
        // If gizmo is being dragged, click confirms the drag
        if (gizmo.isDragging()) {
            gizmo.endDrag();
            const light = state.pointLights.find(l => l.id === state.selectedLightId);
            if (light) rebuildLight(light);
            showMessage('Confirmed');
            return;
        }

        // If a light is selected, check if clicking a gizmo handle
        if (state.selectedLightId != null) {
            const gizmoHit = gizmo.pick(camera);
            if (gizmoHit && gizmoHit.type === 'move') {
                const light = state.pointLights.find(l => l.id === state.selectedLightId);
                saveUndoState();
                gizmo.startDrag('move', gizmoHit.axis, light);
                showMessage(`Moving ${gizmoHit.axis.toUpperCase()} — move mouse to drag, click to confirm, Esc to cancel`);
                return;
            }
        }

        // Try to select an existing light
        const lightTargets = getLightPickTargets();
        const lightHit = pickLight(camera, lightTargets);
        if (lightHit) {
            state.selectedLightId = lightHit.lightId;
            state.lightPhase = 'selected';
            updateLightSelection();
            const light = state.pointLights.find(l => l.id === lightHit.lightId);
            showMessage(`Selected light ${lightHit.lightId} at (${light.x}, ${light.y}, ${light.z})`);
            return;
        }

        // If already selected and clicked empty space, deselect
        if (state.selectedLightId != null) {
            clearLightToolState();
            updateLightSelection();
            return;
        }

        // Place new light at the hit surface
        const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes, lightTargets);
        if (!anyHit) return;
        const snapped = snapToWTGrid(anyHit.point);

        saveUndoState();
        const light = new PointLight(
            state.nextPointLightId++,
            snapped.x, snapped.y + DEFAULT_LIGHT_Y_OFFSET, snapped.z,
        );
        state.pointLights.push(light);
        rebuildLight(light);
        state.selectedLightId = light.id;
        state.lightPhase = 'selected';
        updateLightSelection();
        showMessage(`Placed light ${light.id} at (${light.x}, ${light.y}, ${light.z})`);
        return;
    }

    // Platform tool click handling
    if (state.tool === 'platform') {
        // If gizmo is being dragged, click confirms the drag
        if (gizmo.isDragging()) {
            gizmo.endDrag();
            rebuildPlatform(state.platforms.find(p => p.id === state.selectedPlatformId));
            rebuildConnectedStairRuns(state.selectedPlatformId);
            showMessage('Confirmed');
            return;
        }

        // Simple stair placement — first click
        if (state.platformPhase === 'simple_stair_from') {
            const faceHit = pickFaceAny(camera, csgRegionMeshes, platformMeshes);
            if (!faceHit) { showMessage('Click a surface'); return; }
            const snapped = snapToWTGrid(faceHit.point);
            state.simpleStairFrom = {
                x: snapped.x, y: snapped.y, z: snapped.z,
                axis: faceHit.axis, side: faceHit.side,
            };
            state.platformPhase = 'simple_stair_to';
            showMessage('Click second stair endpoint — Esc to cancel');
            return;
        }

        // Simple stair placement — second click
        if (state.platformPhase === 'simple_stair_to' && state.simpleStairFrom) {
            const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes);
            if (!anyHit) { showMessage('Click a surface'); return; }
            const snapped = snapToWTGrid(anyHit.point);
            const fromPt = state.simpleStairFrom;
            const toPt = { x: snapped.x, y: snapped.y, z: snapped.z };

            const rise = Math.abs(toPt.y - fromPt.y);
            if (rise === 0) {
                showMessage('Points are at the same height — no stairs needed');
                return;
            }
            const ddx = Math.abs(toPt.x - fromPt.x);
            const ddz = Math.abs(toPt.z - fromPt.z);
            if (ddx < 1 && ddz < 1) {
                showMessage('Need horizontal distance between endpoints');
                return;
            }

            saveUndoState();
            const run = new StairRun(
                state.nextStairRunId++,
                null, null,
                { x: fromPt.x, y: fromPt.y, z: fromPt.z },
                { x: toPt.x, y: toPt.y, z: toPt.z },
                state.stairWidth,
                state.stairStepHeight,
                state.stairRiseOverRun,
            );
            run.style = state.platformStyle;
            state.stairRuns.push(run);
            rebuildStairRun(run);

            const steps = Math.max(1, Math.round(rise / state.stairStepHeight));
            showMessage(`Simple stair run created: ${steps} steps`);

            state.platformPhase = 'idle';
            state.simpleStairFrom = null;
            return;
        }

        if (state.platformPhase === 'idle' || state.platformPhase === 'selected') {
            // Check if clicking a gizmo handle (only when a platform is selected)
            if (state.selectedPlatformId != null) {
                const gizmoHit = gizmo.pick(camera);
                if (gizmoHit) {
                    const plat = state.platforms.find(p => p.id === state.selectedPlatformId);
                    saveUndoState();
                    gizmo.startDrag(gizmoHit.type, gizmoHit.axis, plat);
                    const label = gizmoHit.type === 'move' ? `Moving ${gizmoHit.axis.toUpperCase()}` : `Scaling ${gizmoHit.axis}`;
                    showMessage(`${label} — move mouse to drag, click to confirm, Esc to cancel`);
                    return;
                }
            }

            // Try to select a platform or stair — whichever is closer
            const sel = pickPlatformOrStair(camera, platformMeshes, stairRunMeshes);
            if (sel && sel.type === 'platform') {
                state.selectedPlatformId = sel.platformId;
                state.selectedStairRunId = null;
                state.platformPhase = 'selected';
                const plat = state.platforms.find(p => p.id === sel.platformId);
                showMessage(`Selected platform ${sel.platformId} (${plat.sizeX}x${plat.sizeZ} at Y=${plat.y})`);
                return;
            }
            if (sel && sel.type === 'stair') {
                state.selectedStairRunId = sel.stairRunId;
                state.selectedPlatformId = null;
                state.platformPhase = 'selected';
                const run = state.stairRuns.find(r => r.id === sel.stairRunId);
                const fromPlat = run.fromPlatformId != null ? state.platforms.find(p => p.id === run.fromPlatformId) : null;
                const toPlat = run.toPlatformId != null ? state.platforms.find(p => p.id === run.toPlatformId) : null;
                const fromPtR = StairRun.resolveAnchor(fromPlat, run.anchorFrom);
                const toPtR = StairRun.resolveAnchor(toPlat, run.anchorTo);
                const rise = Math.abs(toPtR.y - fromPtR.y);
                const steps = Math.max(1, Math.round(rise / run.stepHeight));
                showMessage(`Selected stair run ${sel.stairRunId}: ${steps} steps`);
                return;
            }

            // If already selected and clicked empty, deselect
            if (state.platformPhase === 'selected') {
                clearPlatformToolState();
                gizmo.update(null, camera);
                return;
            }

            // Place new platform at the hit surface
            const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes);
            if (!anyHit) return;
            const snapped = snapToWTGrid(anyHit.point);

            // Offset placement so platform edge touches wall instead of centering on click
            let px = snapped.x - Math.floor(state.platformSizeX / 2);
            let py = snapped.y;
            let pz = snapped.z - Math.floor(state.platformSizeZ / 2);

            if (anyHit.type === 'csg' && anyHit.axis !== 'y') {
                const camPos = camera.position;
                if (anyHit.axis === 'x') {
                    const wallX = snapped.x;
                    if (camPos.x / WORLD_SCALE > wallX) {
                        px = wallX;
                    } else {
                        px = wallX - state.platformSizeX;
                    }
                } else {
                    const wallZ = snapped.z;
                    if (camPos.z / WORLD_SCALE > wallZ) {
                        pz = wallZ;
                    } else {
                        pz = wallZ - state.platformSizeZ;
                    }
                }
            }

            saveUndoState();
            const plat = new Platform(
                state.nextPlatformId++,
                px, py, pz,
                state.platformSizeX, state.platformSizeZ, state.platformThickness,
            );
            plat.style = state.platformStyle;
            state.platforms.push(plat);
            rebuildPlatform(plat);
            state.selectedPlatformId = plat.id;
            state.platformPhase = 'selected';
            showMessage(`Placed platform ${plat.id} at (${plat.x}, ${plat.y}, ${plat.z})`);
            return;
        }
        // Phase 1: click to pick destination (floor or another platform)
        if (state.platformPhase === 'connecting_dst' && state.platformConnectFrom) {
            const from = state.platformConnectFrom;
            const fromPlat = state.platforms.find(p => p.id === from.platformId);
            const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes);
            if (!anyHit) { showMessage('Click a platform or the floor'); return; }

            if (anyHit.type === 'platform' && anyHit.platformId !== from.platformId) {
                const toPlat = state.platforms.find(p => p.id === anyHit.platformId);
                const edge = closestPlatformEdge(toPlat, anyHit.point);
                state.platformConnectTo = { type: 'platform', platformId: toPlat.id, edge };
                const dir = { x: toPlat.centerX - fromPlat.centerX, z: toPlat.centerZ - fromPlat.centerZ };
                state.platformConnectFrom.edge = bestEdgeForDirection(fromPlat, dir);
            } else if (anyHit.type === 'ground' || anyHit.type === 'csg') {
                state.platformConnectTo = { type: 'ground' };
                const gp = snapToWTGrid(anyHit.point);
                // If the user aimed at the world ground plane but a CSG room floor
                // exists above it at that XZ, prefer the CSG floor Y.
                let destY = gp.y;
                if (anyHit.type === 'ground') {
                    const csgFloor = findFloorYAt(gp.x, gp.z, fromPlat.y, state.csg.brushes);
                    if (csgFloor > destY) destY = csgFloor;
                }
                state.platformConnectTo.y = destY;
                const dir = { x: gp.x - fromPlat.centerX, z: gp.z - fromPlat.centerZ };
                state.platformConnectFrom.edge = bestEdgeForDirection(fromPlat, dir);
            } else {
                showMessage('Click a platform or the floor');
                return;
            }

            state.platformConnectFrom.offset = 0.5;
            state.platformPhase = 'connecting_src';
            showMessage('Slide along edge — click to place stairs, Esc to cancel');
            return;
        }

        // Phase 2: click to lock source position and create stairs
        if (state.platformPhase === 'connecting_src' && state.platformConnectFrom && state.platformConnectTo) {
            const from = state.platformConnectFrom;
            const to = state.platformConnectTo;
            const fromPlat = state.platforms.find(p => p.id === from.platformId);
            const offset = projectCrosshairOntoEdge(fromPlat, from.edge, camera);

            let toPlatformId = null;
            let anchorTo = null;

            const fromPt = fromPlat.getEdgePointAtOffset(from.edge, offset);
            fromPt.y = fromPlat.y;

            let toPt;
            if (to.type === 'platform') {
                const toPlat = state.platforms.find(p => p.id === to.platformId);
                const destOffset = closestOffsetOnEdge(toPlat, to.edge, fromPt);
                toPlatformId = toPlat.id;
                anchorTo = { edge: to.edge, offset: destOffset };
                toPt = { ...toPlat.getEdgePointAtOffset(to.edge, destOffset), y: toPlat.y };
            } else {
                const normal = Platform.edgeNormal(from.edge);
                const destY = to.y ?? 0;
                const rise = fromPlat.y - destY;
                const run = rise / state.stairRiseOverRun;
                const gx = fromPt.x + normal.x * run;
                const gz = fromPt.z + normal.z * run;
                const snappedX = Math.round(gx);
                const snappedZ = Math.round(gz);
                anchorTo = { x: snappedX, y: destY, z: snappedZ };
                toPt = { x: snappedX, y: destY, z: snappedZ };
            }

            const ddx = Math.abs(toPt.x - fromPt.x);
            const ddz = Math.abs(toPt.z - fromPt.z);
            if (ddx < 1 && ddz < 1) {
                showMessage('Need horizontal distance between endpoints');
                return;
            }

            const rise = Math.abs(toPt.y - fromPt.y);
            if (rise === 0) {
                showMessage('Platforms are at the same height — no stairs needed');
                return;
            }

            saveUndoState();
            const run = new StairRun(
                state.nextStairRunId++,
                from.platformId,
                toPlatformId,
                { edge: from.edge, offset },
                anchorTo,
                state.stairWidth,
                state.stairStepHeight,
                state.stairRiseOverRun,
            );
            // Inherit style from the source platform so connected stairs match.
            run.style = fromPlat.style || 'default';
            state.stairRuns.push(run);
            rebuildStairRun(run);

            const steps = Math.max(1, Math.round(rise / state.stairStepHeight));
            showMessage(`Stair run created: ${steps} steps`);

            state.platformPhase = 'selected';
            state.platformConnectFrom = null;
            state.platformConnectTo = null;
            return;
        }
    }
}

// Collapse a baked-mesh triangle to a degenerate point so it stops rendering.
// Works for both indexed geometries (CSG region meshes — flatten all 3
// indices to the same value) and non-indexed ones (cave meshes — set all 3
// vertex positions to the first vertex's position). Returns the undo entry
// (or null if the triangle was already collapsed, so callers can skip).
// Buffer length stays constant — group offsets and faceIndex numbering
// stay valid for later clicks.
function collapseTriangleNoUndo(mesh, faceIndex) {
    const geo = mesh.geometry;
    if (!geo) return null;
    if (geo.index) {
        const idx = geo.index;
        const i0 = idx.getX(faceIndex * 3);
        const i1 = idx.getX(faceIndex * 3 + 1);
        const i2 = idx.getX(faceIndex * 3 + 2);
        if (i0 === i1 && i1 === i2) return null;     // already degenerate
        const entry = { kind: 'index', mesh, faceIndex, i0, i1, i2 };
        idx.setX(faceIndex * 3 + 1, i0);
        idx.setX(faceIndex * 3 + 2, i0);
        idx.needsUpdate = true;
        return entry;
    }
    const pos = geo.getAttribute('position');
    if (!pos) return null;
    const i0 = faceIndex * 3, i1 = faceIndex * 3 + 1, i2 = faceIndex * 3 + 2;
    const ax = pos.getX(i0), ay = pos.getY(i0), az = pos.getZ(i0);
    const bx = pos.getX(i1), by = pos.getY(i1), bz = pos.getZ(i1);
    const cx = pos.getX(i2), cy = pos.getY(i2), cz = pos.getZ(i2);
    if (ax === bx && bx === cx && ay === by && by === cy && az === bz && bz === cz) return null;
    const entry = { kind: 'position', mesh, faceIndex, x1: bx, y1: by, z1: bz, x2: cx, y2: cy, z2: cz };
    pos.setXYZ(i1, ax, ay, az);
    pos.setXYZ(i2, ax, ay, az);
    pos.needsUpdate = true;
    return entry;
}

function collapseTriangle(mesh, faceIndex) {
    const entry = collapseTriangleNoUndo(mesh, faceIndex);
    if (entry) _bakedUndoStack.push(entry);
}

// ─── Baked-mesh highlight selection ─────────────────────────────────
// Click → select; Delete → delete; Esc → clear. The red overlay is a
// 3-vertex Mesh on top of state.bakedMesh, depth-test off so it shows
// through any z-fighting MC slivers. Only one triangle at a time.
// _bakedUndoStack: per-deletion undo entries (Ctrl+Z restores LIFO).
let _currentSelection = null;     // { mesh, faceIndex } | null
let _highlightMesh = null;        // lazy-init THREE.Mesh
const _bakedUndoStack = [];

const _hlA = new THREE.Vector3();
const _hlB = new THREE.Vector3();
const _hlC = new THREE.Vector3();

function ensureHighlightMesh() {
    if (_highlightMesh) return _highlightMesh;
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(9), 3));
    const mat = new THREE.MeshBasicMaterial({
        color: 0xff0040,
        side: THREE.DoubleSide,
        depthTest: false,
        depthWrite: false,
        transparent: true,
        opacity: 0.75,
    });
    _highlightMesh = new THREE.Mesh(geo, mat);
    _highlightMesh.renderOrder = 999;
    _highlightMesh.frustumCulled = false;
    _highlightMesh.visible = false;
    scene.add(_highlightMesh);
    return _highlightMesh;
}

function readTriangleWorldVertices(mesh, faceIndex, outA, outB, outC) {
    const geo = mesh.geometry;
    if (!geo) return false;
    const pos = geo.getAttribute('position');
    if (!pos) return false;
    let ia, ib, ic;
    if (geo.index) {
        ia = geo.index.getX(faceIndex * 3);
        ib = geo.index.getX(faceIndex * 3 + 1);
        ic = geo.index.getX(faceIndex * 3 + 2);
    } else {
        ia = faceIndex * 3;
        ib = faceIndex * 3 + 1;
        ic = faceIndex * 3 + 2;
    }
    outA.set(pos.getX(ia), pos.getY(ia), pos.getZ(ia));
    outB.set(pos.getX(ib), pos.getY(ib), pos.getZ(ib));
    outC.set(pos.getX(ic), pos.getY(ic), pos.getZ(ic));
    mesh.updateMatrixWorld(false);
    outA.applyMatrix4(mesh.matrixWorld);
    outB.applyMatrix4(mesh.matrixWorld);
    outC.applyMatrix4(mesh.matrixWorld);
    return true;
}

function setBakedHighlight(mesh, faceIndex) {
    if (!readTriangleWorldVertices(mesh, faceIndex, _hlA, _hlB, _hlC)) return;
    const hl = ensureHighlightMesh();
    const arr = hl.geometry.getAttribute('position');
    arr.setXYZ(0, _hlA.x, _hlA.y, _hlA.z);
    arr.setXYZ(1, _hlB.x, _hlB.y, _hlB.z);
    arr.setXYZ(2, _hlC.x, _hlC.y, _hlC.z);
    arr.needsUpdate = true;
    hl.geometry.computeBoundingSphere();
    hl.visible = true;
    _currentSelection = { mesh, faceIndex };
}

export function clearBakedHighlight() {
    _currentSelection = null;
    if (_highlightMesh) _highlightMesh.visible = false;
}

export function deleteBakedHighlight() {
    if (!_currentSelection) return false;
    collapseTriangle(_currentSelection.mesh, _currentSelection.faceIndex);
    clearBakedHighlight();
    return true;
}

// Pop the last triangle deletion (single click OR a whole prism-cleanup
// batch) and restore its original index/position values. Skips entries
// whose mesh has been detached (defensive — shouldn't happen mid-session
// but keeps stale refs from breaking the undo stream).
export function undoBakedDelete() {
    while (_bakedUndoStack.length > 0) {
        const top = _bakedUndoStack.pop();
        if (top.kind === 'group') {
            for (let i = top.entries.length - 1; i >= 0; i--) restoreUndoEntry(top.entries[i]);
            return true;
        }
        if (restoreUndoEntry(top)) return true;
    }
    return false;
}

function restoreUndoEntry(entry) {
    const geo = entry.mesh && entry.mesh.geometry;
    if (!geo || !entry.mesh.parent) return false;
    if (entry.kind === 'index') {
        const idx = geo.index;
        if (!idx) return false;
        idx.setX(entry.faceIndex * 3,     entry.i0);
        idx.setX(entry.faceIndex * 3 + 1, entry.i1);
        idx.setX(entry.faceIndex * 3 + 2, entry.i2);
        idx.needsUpdate = true;
        return true;
    }
    if (entry.kind === 'position') {
        const pos = geo.getAttribute('position');
        if (!pos) return false;
        pos.setXYZ(entry.faceIndex * 3 + 1, entry.x1, entry.y1, entry.z1);
        pos.setXYZ(entry.faceIndex * 3 + 2, entry.x2, entry.y2, entry.z2);
        pos.needsUpdate = true;
        return true;
    }
    if (entry.kind === 'append') {
        // The slicer grew this mesh's attribute / index buffers and added new
        // material groups for the appended fan triangles. Trim everything back
        // to the snapshot taken before the slice. Subsequent (older) collapse
        // entries in this same group then write into the trimmed buffers
        // exactly where the originals lived.
        trimAttribute(geo, 'position', entry.origVertCount, 3);
        trimAttribute(geo, 'normal',   entry.origVertCount, 3);
        trimAttribute(geo, 'uv',       entry.origVertCount, 2);
        trimAttribute(geo, 'color',    entry.origVertCount, 3);
        if (geo.index && geo.index.count > entry.origIndexCount) {
            const arr = geo.index.array.slice(0, entry.origIndexCount);
            geo.setIndex(new THREE.BufferAttribute(arr, 1));
        }
        geo.clearGroups();
        for (const g of entry.origGroups) geo.addGroup(g.start, g.count, g.materialIndex);
        geo.computeBoundingBox();
        geo.computeBoundingSphere();
        return true;
    }
    return false;
}

function trimAttribute(geo, name, count, itemSize) {
    const attr = geo.getAttribute(name);
    if (!attr || attr.count <= count) return;
    const arr = attr.array.slice(0, count * itemSize);
    geo.setAttribute(name, new THREE.BufferAttribute(arr, itemSize));
}

// ─── Cleanup-prism tool ─────────────────────────────────────────────
// One axis-aligned prism per cave anchor face, centred on the wall plane,
// inset slightly inside the rect so we don't touch the surrounding wall.
// 'B' toggles the cyan wireframe overlay; 'C' deletes every baked triangle
// whose centroid sits inside any prism (one undo group, Ctrl+Z reverts the
// whole batch). state.bakedAnchors is the snapshot taken at bake time.

// Prism extents are asymmetric across the wall plane: it should reach a fair
// bit into the room so the cave's MC chunk meshes get sliced properly, but
// only barely poke into the cave so it just clips the wall triangles
// without eating into the cave's interior detail.
const PRISM_INSET_WT = 0.001;       // tiny u/v inset so the prism doesn't quite touch the rect edge
const PRISM_ROOM_DEPTH_WT = 2.0;    // how far the prism extends into the room
const PRISM_CAVE_DEPTH_WT = 0.5;    // how far the prism extends into the cave

let _prismGroup = null;
let _prismsVisible = true;

function getCleanupPrisms() {
    const anchors = state.bakedAnchors;
    if (!anchors || anchors.length === 0) return [];
    const S = WORLD_SCALE;
    const inset = PRISM_INSET_WT * S;
    const roomDepth = PRISM_ROOM_DEPTH_WT * S;
    const caveDepth = PRISM_CAVE_DEPTH_WT * S;
    return anchors.map(a => {
        const planeM = a.position * S;
        // side='max' means the wall faces the +axis direction, so the cave
        // sits at +axis and the room interior at -axis (and vice-versa).
        const towardCave = (a.side === 'max') ? +1 : -1;
        const caveEndM = planeM + towardCave * caveDepth;
        const roomEndM = planeM - towardCave * roomDepth;
        return {
            axis: a.axis,
            nMinM: Math.min(caveEndM, roomEndM),
            nMaxM: Math.max(caveEndM, roomEndM),
            uMinM: a.u0 * S + inset,
            uMaxM: a.u1 * S - inset,
            vMinM: a.v0 * S + inset,
            vMaxM: a.v1 * S - inset,
        };
    });
}

function rebuildPrismOverlays() {
    if (!_prismGroup) return;
    while (_prismGroup.children.length) {
        const c = _prismGroup.children.pop();
        if (c.geometry) c.geometry.dispose();
        if (c.material) c.material.dispose();
    }
    for (const p of getCleanupPrisms()) {
        const uLen = p.uMaxM - p.uMinM;
        const vLen = p.vMaxM - p.vMinM;
        const nLen = p.nMaxM - p.nMinM;
        const uMid = (p.uMinM + p.uMaxM) / 2;
        const vMid = (p.vMinM + p.vMaxM) / 2;
        const nMid = (p.nMinM + p.nMaxM) / 2;
        let dim, center;
        if (p.axis === 'x')      { dim = [nLen, vLen, uLen]; center = [nMid, vMid, uMid]; }
        else if (p.axis === 'y') { dim = [uLen, nLen, vLen]; center = [uMid, nMid, vMid]; }
        else                     { dim = [uLen, vLen, nLen]; center = [uMid, vMid, nMid]; }
        const box = new THREE.BoxGeometry(dim[0], dim[1], dim[2]);
        const edges = new THREE.EdgesGeometry(box);
        box.dispose();
        const mat = new THREE.LineBasicMaterial({
            color: 0x00ddff, depthTest: false, transparent: true, opacity: 0.85,
        });
        const lines = new THREE.LineSegments(edges, mat);
        lines.position.set(center[0], center[1], center[2]);
        lines.renderOrder = 998;
        lines.frustumCulled = false;
        _prismGroup.add(lines);
    }
    _prismGroup.visible = _prismsVisible;
}

// Called from bakeLevel.freezeIntoScene right after state.isBaked flips,
// so the wireframe shows up immediately. Re-runs are safe (rebuilds children).
export function initBakedPrisms() {
    if (!_prismGroup) {
        _prismGroup = new THREE.Group();
        _prismGroup.name = 'cleanupPrisms';
        scene.add(_prismGroup);
    }
    _prismsVisible = true;
    rebuildPrismOverlays();
}

export function toggleBakedPrisms() {
    _prismsVisible = !_prismsVisible;
    if (_prismGroup) _prismGroup.visible = _prismsVisible;
    return _prismsVisible;
}

// Walk every triangle in state.bakedMesh.children and slice each one against
// the cleanup prisms — the inside-prism portion is removed, the outside
// portion is kept (and re-triangulated as a fan if the slice produced a
// polygon). All collapses + buffer appends are batched into one undo-stack
// entry so Ctrl+Z reverts the whole pass. Returns the number of triangles
// affected (those that were either fully removed or sliced).
export function runBakedPrismCleanup() {
    if (!state.bakedMesh) return 0;
    const prisms = getCleanupPrisms();
    if (prisms.length === 0) return 0;
    const meshes = state.bakedMesh.children;
    const entries = sliceTrianglesAgainstPrisms(meshes, prisms);
    if (entries.length === 0) return 0;
    _bakedUndoStack.push({ kind: 'group', entries });
    if (_currentSelection && _currentSelection.mesh && _currentSelection.faceIndex != null) {
        clearBakedHighlight();
    }
    let affected = 0;
    for (const e of entries) if (e.kind === 'index' || e.kind === 'position') affected++;
    return affected;
}
