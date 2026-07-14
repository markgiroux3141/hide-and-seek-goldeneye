// Face picking via raycaster — uses triangle index to faceId lookup

import * as THREE from 'three';
import { WORLD_SCALE } from './core/constants.js';
import { state } from './state.js';

// True when the world-space ground-plane hit falls inside a subtract brush's
// carved interior — i.e., the Y=0 plane cuts through "air" there, not a real
// floor. Used to reject fake ground hits in pickAny when a CSG room has been
// carved below Y=0 (pits, sunken areas).
function groundPointInCarvedAir(point) {
    const x = point.x / WORLD_SCALE;
    const y = point.y / WORLD_SCALE;
    const z = point.z / WORLD_SCALE;
    for (const b of state.csg.brushes) {
        if (b.op !== 'subtract') continue;
        if (x > b.x && x < b.x + b.w
            && y > b.y && y < b.y + b.h
            && z > b.z && z < b.z + b.d) {
            return true;
        }
    }
    return false;
}

const raycaster = new THREE.Raycaster();
const screenCenter = new THREE.Vector2(0, 0);

// Pick a CSG region face under the crosshair
// csgRegionMeshes: Map<regionId, { mesh, faceIds, ... }>
// Returns: { regionId, brushId, axis, side, position, point } | null
export function pickCSGFace(camera, csgRegionMeshes) {
    raycaster.setFromCamera(screenCenter, camera);

    const meshes = [];
    for (const [, data] of csgRegionMeshes) {
        meshes.push(data.mesh);
    }

    const hits = raycaster.intersectObjects(meshes, false);
    if (hits.length === 0) return null;

    const hit = hits[0];
    const mesh = hit.object;

    let faceIds = null;
    let regionId = null;
    for (const [id, data] of csgRegionMeshes) {
        if (data.mesh === mesh) {
            faceIds = data.faceIds;
            regionId = id;
            break;
        }
    }

    if (!faceIds) return null;

    const triIndex = hit.faceIndex;
    if (triIndex >= 0 && triIndex < faceIds.length) {
        return {
            regionId,
            triIndex,
            ...faceIds[triIndex],
            point: hit.point,
        };
    }

    return null;
}

// Pick a platform mesh under the crosshair
// platformMeshes: Map<platformId, THREE.Mesh>
export function pickPlatform(camera, platformMeshes) {
    raycaster.setFromCamera(screenCenter, camera);

    const meshes = [];
    for (const [, mesh] of platformMeshes) {
        meshes.push(mesh);
    }

    const hits = raycaster.intersectObjects(meshes, false);
    if (hits.length === 0) return null;

    const hit = hits[0];
    const mesh = hit.object;
    const platformId = mesh.userData.platformId;
    if (platformId == null) return null;

    return { platformId, point: hit.point };
}

// Pick whichever platform or stair mesh is closest under the crosshair.
// Returns { type: 'platform', platformId, point } | { type: 'stair', stairRunId, point } | null.
export function pickPlatformOrStair(camera, platformMeshes, stairRunMeshes) {
    raycaster.setFromCamera(screenCenter, camera);
    const meshes = [];
    for (const [, m] of platformMeshes) meshes.push(m);
    for (const [, m] of stairRunMeshes) meshes.push(m);
    const hits = raycaster.intersectObjects(meshes, false);
    if (hits.length === 0) return null;
    const hit = hits[0];
    const ud = hit.object.userData;
    if (ud.platformId != null) return { type: 'platform', platformId: ud.platformId, point: hit.point };
    if (ud.stairRunId != null) return { type: 'stair', stairRunId: ud.stairRunId, point: hit.point };
    return null;
}

// Pick a stair run mesh under the crosshair
// stairRunMeshes: Map<stairRunId, THREE.Mesh>
export function pickStairRun(camera, stairRunMeshes) {
    raycaster.setFromCamera(screenCenter, camera);

    const meshes = [];
    for (const [, mesh] of stairRunMeshes) {
        meshes.push(mesh);
    }

    const hits = raycaster.intersectObjects(meshes, false);
    if (hits.length === 0) return null;

    const hit = hits[0];
    const mesh = hit.object;
    const stairRunId = mesh.userData.stairRunId;
    if (stairRunId == null) return null;

    return { stairRunId, point: hit.point };
}

// Pick only the ground plane (ignoring all meshes)
export function pickGroundOnly(camera) {
    raycaster.setFromCamera(screenCenter, camera);
    const intersect = new THREE.Vector3();
    if (raycaster.ray.intersectPlane(groundPlane, intersect)) {
        return { type: 'ground', point: intersect.clone() };
    }
    return null;
}

// Pick a light mesh under the crosshair
// pickTargets: array of Mesh (icon + core parts from getLightPickTargets())
export function pickLight(camera, pickTargets) {
    raycaster.setFromCamera(screenCenter, camera);

    const hits = raycaster.intersectObjects(pickTargets, false);
    if (hits.length === 0) return null;

    const hit = hits[0];
    const lightId = hit.object.userData.lightId;
    if (lightId == null) return null;

    return { lightId, point: hit.point };
}

// Pick any object (CSG regions, platforms, lights) or the ground plane — returns the nearest hit
const groundPlane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0); // Y=0 ground plane
const groundIntersect = new THREE.Vector3();

export function pickAny(camera, csgRegionMeshes, platformMeshes, lightPickTargets) {
    raycaster.setFromCamera(screenCenter, camera);

    const allMeshes = [];
    for (const [, data] of csgRegionMeshes) allMeshes.push(data.mesh);
    for (const [, mesh] of platformMeshes) allMeshes.push(mesh);
    if (lightPickTargets) {
        for (const mesh of lightPickTargets) allMeshes.push(mesh);
    }

    const hits = raycaster.intersectObjects(allMeshes, false);

    // Also check ground plane — but reject it if the Y=0 plane intersects
    // inside a carved-out CSG volume (e.g. the air above a sunken pit floor).
    let groundHit = null;
    let groundDist = Infinity;
    if (raycaster.ray.intersectPlane(groundPlane, groundIntersect)) {
        if (!groundPointInCarvedAir(groundIntersect)) {
            groundDist = groundIntersect.distanceTo(raycaster.ray.origin);
            groundHit = { type: 'ground', point: groundIntersect.clone() };
        }
    }

    if (hits.length === 0) return groundHit;

    const hit = hits[0];
    const mesh = hit.object;

    // If ground is closer than the mesh hit (with tolerance to avoid z-fighting), return ground
    if (groundHit && groundDist < hit.distance - 0.01) {
        return groundHit;
    }

    // Check if it's a light
    if (mesh.userData.lightId != null) {
        return { type: 'light', lightId: mesh.userData.lightId, point: hit.point };
    }

    // Check if it's a platform
    if (mesh.userData.platformId != null) {
        return { type: 'platform', platformId: mesh.userData.platformId, point: hit.point };
    }

    // Check if it's a CSG region mesh
    if (mesh.userData.isCSG) {
        let faceIds = null;
        let regionId = null;
        for (const [id, data] of csgRegionMeshes) {
            if (data.mesh === mesh) { faceIds = data.faceIds; regionId = id; break; }
        }
        if (faceIds) {
            const triIndex = hit.faceIndex;
            if (triIndex >= 0 && triIndex < faceIds.length) {
                return { type: 'csg', regionId, triIndex, ...faceIds[triIndex], point: hit.point };
            }
        }
    }

    return null;
}

// Like pickAny, but always returns unified face info: { axis: 'x'|'y'|'z', side: 'min'|'max', position } in WT units.
// Used by tools that need to align a preview flush with the hit face plane (e.g. simple stairs).
const _tmpNormal = new THREE.Vector3();
export function pickFaceAny(camera, csgRegionMeshes, platformMeshes) {
    const hit = pickAny(camera, csgRegionMeshes, platformMeshes);
    if (!hit) return null;

    if (hit.type === 'csg') {
        // axis/side/position already present from faceIds entry
        return hit;
    }

    if (hit.type === 'ground') {
        return { ...hit, axis: 'y', side: 'max', position: 0 };
    }

    // Platform hit — derive face from raycaster result. We don't have the
    // Three.js Intersection here, so redo a targeted raycast against the one mesh.
    if (hit.type === 'platform') {
        const mesh = platformMeshes.get(hit.platformId);
        if (!mesh) return null;
        raycaster.setFromCamera(screenCenter, camera);
        const hits = raycaster.intersectObject(mesh, false);
        if (hits.length === 0 || !hits[0].face) return null;
        _tmpNormal.copy(hits[0].face.normal).transformDirection(mesh.matrixWorld).normalize();
        const ax = Math.abs(_tmpNormal.x);
        const ay = Math.abs(_tmpNormal.y);
        const az = Math.abs(_tmpNormal.z);
        let axis, signed;
        if (ay >= ax && ay >= az) { axis = 'y'; signed = _tmpNormal.y; }
        else if (ax >= az) { axis = 'x'; signed = _tmpNormal.x; }
        else { axis = 'z'; signed = _tmpNormal.z; }
        const side = signed >= 0 ? 'max' : 'min';
        const coord = axis === 'x' ? hit.point.x : axis === 'y' ? hit.point.y : hit.point.z;
        const position = Math.round(coord / WORLD_SCALE);
        return { ...hit, axis, side, position };
    }

    return null;
}
