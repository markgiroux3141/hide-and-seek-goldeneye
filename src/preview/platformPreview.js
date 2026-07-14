// Platform tool preview — selection outlines, connect mode visuals, stair preview

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { state } from '../state.js';
import { pickAny, pickFaceAny } from '../raycaster.js';
import { isPointerLocked } from '../input/input.js';
import { snapToWTGrid } from '../actions.js';
import { Platform } from '../core/Platform.js';
import { StairRun } from '../core/StairRun.js';
import { buildPlatformPreviewLines, buildEdgeHighlightLines, buildEdgeSlotLines, buildStairRunPreviewLines } from '../geometry/platformGeometry.js';
import { csgRegionMeshes, platformMeshes } from '../mesh/MeshManager.js';
import { closestPlatformEdge, closestOffsetOnEdge, projectCrosshairOntoEdge } from '../tools/platformEdgeUtils.js';
import { scene } from '../scene/setup.js';

const platformPreviewGroup = new THREE.Group();
let _added = false;
const platformPreviewMat = new THREE.LineBasicMaterial({ color: 0xffff00, linewidth: 2 });
const platformSelectionMat = new THREE.LineBasicMaterial({ color: 0x00ff00, linewidth: 2 });
const platformEdgeHighlightMat = new THREE.LineBasicMaterial({ color: 0x00ffff, linewidth: 3 });

// Filled-quad materials for the simple-stair face gizmo.
const stairFaceFromMat = new THREE.MeshBasicMaterial({
    color: 0x00ff00, transparent: true, opacity: 0.4,
    side: THREE.DoubleSide, depthTest: true,
    polygonOffset: true, polygonOffsetFactor: -2,
});
const stairFaceCursorMat = new THREE.MeshBasicMaterial({
    color: 0xffff00, transparent: true, opacity: 0.4,
    side: THREE.DoubleSide, depthTest: true,
    polygonOffset: true, polygonOffsetFactor: -2,
});

export function updatePlatformPreview(camera) {
    if (!_added) { scene.add(platformPreviewGroup); _added = true; }
    while (platformPreviewGroup.children.length > 0) {
        const child = platformPreviewGroup.children[0];
        platformPreviewGroup.remove(child);
        if (child.geometry) child.geometry.dispose();
    }

    if (state.tool !== 'platform' || !isPointerLocked()) return;

    // Show green wireframe on selected platform
    if (state.selectedPlatformId != null) {
        const plat = state.platforms.find(p => p.id === state.selectedPlatformId);
        if (plat) {
            const pts = buildPlatformPreviewLines(plat.x, plat.y, plat.z, plat.sizeX, plat.sizeZ, plat.thickness);
            const positions = new Float32Array(pts);
            const geo = new THREE.BufferGeometry();
            geo.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
            platformPreviewGroup.add(new THREE.LineSegments(geo, platformSelectionMat));
        }
    }

    // Connect mode visuals — phase 1: choosing destination
    if (state.platformPhase === 'connecting_dst' && state.platformConnectFrom) {
        const fromPlat = state.platforms.find(p => p.id === state.platformConnectFrom.platformId);
        if (fromPlat) {
            const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes);
            if (anyHit) {
                if (anyHit.type === 'platform' && anyHit.platformId !== fromPlat.id) {
                    const toPlat = state.platforms.find(p => p.id === anyHit.platformId);
                    if (toPlat) {
                        const edge = closestPlatformEdge(toPlat, anyHit.point);
                        const edgePts = buildEdgeHighlightLines(toPlat, edge);
                        const edgeGeo = new THREE.BufferGeometry();
                        edgeGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(edgePts), 3));
                        platformPreviewGroup.add(new THREE.LineSegments(edgeGeo, platformEdgeHighlightMat));
                    }
                }
            }
        }
    }

    // Connect mode visuals — phase 2: sliding source slot + stair preview
    if (state.platformPhase === 'connecting_src' && state.platformConnectFrom && state.platformConnectTo) {
        const from = state.platformConnectFrom;
        const to = state.platformConnectTo;
        const fromPlat = state.platforms.find(p => p.id === from.platformId);
        if (fromPlat) {
            const edgePts = buildEdgeHighlightLines(fromPlat, from.edge);
            const edgeGeo = new THREE.BufferGeometry();
            edgeGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(edgePts), 3));
            platformPreviewGroup.add(new THREE.LineSegments(edgeGeo, platformEdgeHighlightMat));

            const offset = projectCrosshairOntoEdge(fromPlat, from.edge, camera);
            const slotPts = buildEdgeSlotLines(fromPlat, from.edge, offset, state.stairWidth);
            const slotGeo = new THREE.BufferGeometry();
            slotGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(slotPts), 3));
            platformPreviewGroup.add(new THREE.LineSegments(slotGeo, platformSelectionMat));

            const fromPt = { ...fromPlat.getEdgePointAtOffset(from.edge, offset), y: fromPlat.y };
            let destPt = null;

            if (to.type === 'platform') {
                const toPlat = state.platforms.find(p => p.id === to.platformId);
                if (toPlat) {
                    const destOffset = closestOffsetOnEdge(toPlat, to.edge, fromPt);
                    destPt = { ...toPlat.getEdgePointAtOffset(to.edge, destOffset), y: toPlat.y };

                    const destSlotPts = buildEdgeSlotLines(toPlat, to.edge, destOffset, state.stairWidth);
                    const destGeo = new THREE.BufferGeometry();
                    destGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(destSlotPts), 3));
                    platformPreviewGroup.add(new THREE.LineSegments(destGeo, platformEdgeHighlightMat));
                }
            } else {
                const normal = Platform.edgeNormal(from.edge);
                const destY = to.y ?? 0;
                const rise = fromPlat.y - destY;
                const run = rise / state.stairRiseOverRun;
                const gx = fromPt.x + normal.x * run;
                const gz = fromPt.z + normal.z * run;
                destPt = { x: Math.round(gx), y: destY, z: Math.round(gz) };
            }

            if (destPt) {
                const ddx = Math.abs(destPt.x - fromPt.x);
                const ddz = Math.abs(destPt.z - fromPt.z);
                if ((ddx >= 1 || ddz >= 1) && fromPt.y !== destPt.y) {
                    const stairPts = buildStairRunPreviewLines(
                        fromPt, destPt, state.stairWidth, state.stairStepHeight, state.stairRiseOverRun,
                    );
                    if (stairPts.length > 0) {
                        const stairGeo = new THREE.BufferGeometry();
                        stairGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(stairPts), 3));
                        platformPreviewGroup.add(new THREE.LineSegments(stairGeo, platformSelectionMat));
                    }
                }
            }
        }
    }

    // Selection highlight for selected stair run
    if (state.selectedStairRunId != null && state.selectedPlatformId == null && state.platformPhase === 'selected') {
        const run = state.stairRuns.find(r => r.id === state.selectedStairRunId);
        if (run) {
            const fromPlat = run.fromPlatformId != null ? state.platforms.find(p => p.id === run.fromPlatformId) : null;
            const toPlat = run.toPlatformId != null ? state.platforms.find(p => p.id === run.toPlatformId) : null;
            const fromPt = StairRun.resolveAnchor(fromPlat, run.anchorFrom);
            const toPt = StairRun.resolveAnchor(toPlat, run.anchorTo);
            const stairPts = buildStairRunPreviewLines(fromPt, toPt, run.width, run.stepHeight, run.riseOverRun);
            if (stairPts.length > 0) {
                const stairGeo = new THREE.BufferGeometry();
                stairGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(stairPts), 3));
                platformPreviewGroup.add(new THREE.LineSegments(stairGeo, platformSelectionMat));
            }
        }
    }

    // Simple stair preview — flat rectangular face gizmo that snaps to the hovered face
    if (state.platformPhase === 'simple_stair_from' || state.platformPhase === 'simple_stair_to') {
        renderSimpleStairPreview(camera);
    }

    // Hover preview when idle
    if (state.platformPhase === 'idle') {
        const anyHit = pickAny(camera, csgRegionMeshes, platformMeshes);
        if (anyHit) {
            const snapped = snapToWTGrid(anyHit.point);
            let px = snapped.x - Math.floor(state.platformSizeX / 2);
            let pz = snapped.z - Math.floor(state.platformSizeZ / 2);

            // Snap edge to wall (match placement logic in indoorClick.js)
            if (anyHit.type === 'csg' && anyHit.axis !== 'y') {
                const camPos = camera.position;
                if (anyHit.axis === 'x') {
                    const wallX = snapped.x;
                    px = (camPos.x / WORLD_SCALE > wallX) ? wallX : wallX - state.platformSizeX;
                } else {
                    const wallZ = snapped.z;
                    pz = (camPos.z / WORLD_SCALE > wallZ) ? wallZ : wallZ - state.platformSizeZ;
                }
            }

            const pts = buildPlatformPreviewLines(
                px, snapped.y, pz,
                state.platformSizeX, state.platformSizeZ, state.platformThickness,
            );
            const positions = new Float32Array(pts);
            const geo = new THREE.BufferGeometry();
            geo.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
            platformPreviewGroup.add(new THREE.LineSegments(geo, platformPreviewMat));
        }
    }
}

const _FACE_NORMALS = {
    x_max: new THREE.Vector3(1, 0, 0),
    x_min: new THREE.Vector3(-1, 0, 0),
    y_max: new THREE.Vector3(0, 1, 0),
    y_min: new THREE.Vector3(0, -1, 0),
    z_max: new THREE.Vector3(0, 0, 1),
    z_min: new THREE.Vector3(0, 0, -1),
};

function faceNormal(face) {
    return _FACE_NORMALS[`${face.axis}_${face.side}`] || _FACE_NORMALS.y_max;
}

function axisAlignedLongDir(face, camera) {
    if (face.axis === 'y') {
        const fwd = new THREE.Vector3();
        camera.getWorldDirection(fwd);
        // Long axis (stairWidth) perpendicular to the camera's dominant horizontal direction.
        if (Math.abs(fwd.x) >= Math.abs(fwd.z)) return new THREE.Vector3(0, 0, 1);
        return new THREE.Vector3(1, 0, 0);
    }
    // Wall: long axis is horizontal on the wall; short axis is vertical (world Y).
    if (face.axis === 'x') return new THREE.Vector3(0, 0, 1);
    return new THREE.Vector3(1, 0, 0);
}

function orientedLongDir(face, centerWT, targetWT, camera) {
    const n = faceNormal(face);
    const dir = new THREE.Vector3(
        targetWT.x - centerWT.x,
        targetWT.y - centerWT.y,
        targetWT.z - centerWT.z,
    );
    dir.addScaledVector(n, -dir.dot(n));
    if (dir.lengthSq() < 1e-6) return axisAlignedLongDir(face, camera);
    dir.normalize();
    const long = new THREE.Vector3().crossVectors(n, dir);
    if (long.lengthSq() < 1e-6) return axisAlignedLongDir(face, camera);
    return long.normalize();
}

function addFaceRect(face, centerWT, longDir, width, depth, material) {
    const n = faceNormal(face);
    const cx = centerWT.x * WORLD_SCALE + n.x * 0.002;
    const cy = centerWT.y * WORLD_SCALE + n.y * 0.002;
    const cz = centerWT.z * WORLD_SCALE + n.z * 0.002;

    const shortDir = new THREE.Vector3().crossVectors(n, longDir);
    if (shortDir.lengthSq() < 1e-6) return;
    shortDir.normalize();

    const hw = (width * WORLD_SCALE) / 2;
    const hd = (depth * WORLD_SCALE) / 2;

    const c0x = cx + longDir.x * -hw + shortDir.x * -hd;
    const c0y = cy + longDir.y * -hw + shortDir.y * -hd;
    const c0z = cz + longDir.z * -hw + shortDir.z * -hd;
    const c1x = cx + longDir.x *  hw + shortDir.x * -hd;
    const c1y = cy + longDir.y *  hw + shortDir.y * -hd;
    const c1z = cz + longDir.z *  hw + shortDir.z * -hd;
    const c2x = cx + longDir.x *  hw + shortDir.x *  hd;
    const c2y = cy + longDir.y *  hw + shortDir.y *  hd;
    const c2z = cz + longDir.z *  hw + shortDir.z *  hd;
    const c3x = cx + longDir.x * -hw + shortDir.x *  hd;
    const c3y = cy + longDir.y * -hw + shortDir.y *  hd;
    const c3z = cz + longDir.z * -hw + shortDir.z *  hd;

    const positions = new Float32Array([
        c0x, c0y, c0z,  c1x, c1y, c1z,  c2x, c2y, c2z,
        c0x, c0y, c0z,  c2x, c2y, c2z,  c3x, c3y, c3z,
    ]);
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geo.computeVertexNormals();
    platformPreviewGroup.add(new THREE.Mesh(geo, material));
}

function renderSimpleStairPreview(camera) {
    const cursorHit = pickFaceAny(camera, csgRegionMeshes, platformMeshes);
    const from = state.simpleStairFrom;

    if (state.platformPhase === 'simple_stair_from') {
        if (!cursorHit) return;
        const snapped = snapToWTGrid(cursorHit.point);
        const longDir = axisAlignedLongDir(cursorHit, camera);
        addFaceRect(cursorHit, snapped, longDir, state.stairWidth, 1, stairFaceCursorMat);
        return;
    }

    // Phase 2: simple_stair_to — from is locked
    if (!from) return;
    const fromFace = { axis: from.axis || 'y', side: from.side || 'max' };

    if (cursorHit) {
        const cursorSnap = snapToWTGrid(cursorHit.point);

        const fromLongDir = orientedLongDir(fromFace, from, cursorSnap, camera);
        addFaceRect(fromFace, from, fromLongDir, state.stairWidth, 1, stairFaceFromMat);

        const cursorLongDir = orientedLongDir(cursorHit, cursorSnap, from, camera);
        addFaceRect(cursorHit, cursorSnap, cursorLongDir, state.stairWidth, 1, stairFaceCursorMat);

        const rise = Math.abs(cursorSnap.y - from.y);
        const ddx = Math.abs(cursorSnap.x - from.x);
        const ddz = Math.abs(cursorSnap.z - from.z);
        if (rise > 0 && (ddx >= 1 || ddz >= 1)) {
            const stairPts = buildStairRunPreviewLines(
                from, cursorSnap, state.stairWidth, state.stairStepHeight, state.stairRiseOverRun,
            );
            if (stairPts.length > 0) {
                const stairGeo = new THREE.BufferGeometry();
                stairGeo.setAttribute('position', new THREE.Float32BufferAttribute(new Float32Array(stairPts), 3));
                platformPreviewGroup.add(new THREE.LineSegments(stairGeo, platformSelectionMat));
            }
        }
    } else {
        const longDir = axisAlignedLongDir(fromFace, camera);
        addFaceRect(fromFace, from, longDir, state.stairWidth, 1, stairFaceFromMat);
    }
}
