// Light baker — computes per-vertex diffuse lighting + shadows from point lights
// Subdivides geometry before baking for smooth gradients, then writes vertex colors.

import * as THREE from 'three';
import { state } from '../state.js';
import { WORLD_SCALE } from '../core/constants.js';
import { platformMeshes, stairRunMeshes, csgRegionMeshes } from '../mesh/MeshManager.js';
import { computeAO } from './ambientOcclusion.js';
import { storeBakedColors, clearBakedColorStore } from './bakedColorStore.js';
import { subdivideGeometry } from './subdivide.js';

const S = WORLD_SCALE;

// Ambient base is read from state.ambientIntensity at bake time

// Fixed subdivision level: each quad becomes NxN sub-quads.
// N=2 is a good balance: 4x more vertices for smoother gradients, fast bake.
const BAKE_SUBDIVISIONS = 2;

// Collect all scene meshes for shadow raycasting.
// CSG region meshes are included so platform/stair shadows fall on CSG walls,
// and so CSG self-shadows (one wall blocks another) work too.
function getOccluders() {
    const meshes = [];
    for (const [, mesh] of platformMeshes) meshes.push(mesh);
    for (const [, mesh] of stairRunMeshes) meshes.push(mesh);
    for (const [, data] of csgRegionMeshes) meshes.push(data.mesh);
    return meshes;
}

const _raycaster = new THREE.Raycaster();
const _origin = new THREE.Vector3();
const _dir = new THREE.Vector3();
const _lightPos = new THREE.Vector3();
const _vertPos = new THREE.Vector3();
const _normal = new THREE.Vector3();

// Shadow bias: offset origin along normal and skip near hits to avoid
// self-intersection with adjacent faces of the same mesh.
const SHADOW_BIAS = 0.1;

function isOccluded(vertPos, vertNormal, lightPos, occluders) {
    _origin.copy(vertPos).addScaledVector(vertNormal, SHADOW_BIAS);
    _dir.copy(lightPos).sub(_origin);
    const dist = _dir.length();
    if (dist < 0.01) return false;
    _dir.divideScalar(dist);

    _raycaster.set(_origin, _dir);
    _raycaster.near = SHADOW_BIAS;
    _raycaster.far = dist - SHADOW_BIAS;

    const hits = _raycaster.intersectObjects(occluders, false);
    return hits.length > 0;
}

// Subdivide a mesh's geometry and update faceIds for volumes
function subdivideMesh(mesh, faceIds) {
    const srcGeo = mesh.geometry;
    const { geometry: newGeo, faceIds: newFaceIds } = subdivideGeometry(srcGeo, faceIds, BAKE_SUBDIVISIONS);
    if (newGeo !== srcGeo) {
        mesh.geometry = newGeo;
        srcGeo.dispose();
    }
    return newFaceIds;
}

// Bake lighting for a mesh and all its children (e.g., railings)
function bakeMeshAndChildren(mesh, occluders, lights, aoSamples, keyPrefix, ambient) {
    bakeGeometry(mesh.geometry, occluders, lights, aoSamples, ambient);
    storeBakedColors(keyPrefix, mesh.geometry);

    // Also bake child meshes (railings, etc.)
    for (let i = 0; i < mesh.children.length; i++) {
        const child = mesh.children[i];
        if (!child.isMesh) continue;
        const colors = child.geometry.getAttribute('color');
        if (!colors) continue;

        bakeGeometry(child.geometry, occluders, lights, aoSamples, ambient);
        storeBakedColors(keyPrefix + '_child_' + i, child.geometry);

        if (child.material && !child.material.vertexColors) {
            child.material.vertexColors = true;
            child.material.needsUpdate = true;
        }
    }
}

// Bake lighting for a single geometry's vertex colors
function bakeGeometry(geometry, occluders, lights, aoSamples, ambient) {
    const positions = geometry.getAttribute('position');
    const normals = geometry.getAttribute('normal');
    const colors = geometry.getAttribute('color');

    if (!positions || !normals || !colors) return;

    const vertCount = positions.count;

    for (let i = 0; i < vertCount; i++) {
        _vertPos.set(positions.getX(i), positions.getY(i), positions.getZ(i));
        _normal.set(normals.getX(i), normals.getY(i), normals.getZ(i)).normalize();

        let totalR = ambient;
        let totalG = ambient;
        let totalB = ambient;

        for (const light of lights) {
            if (!light.enabled) continue;

            _lightPos.set(light.x * S, light.y * S, light.z * S);
            _dir.copy(_lightPos).sub(_vertPos);
            const dist = _dir.length();
            const rangeWorld = light.range * S;

            if (dist > rangeWorld) continue;

            _dir.divideScalar(dist);
            const NdotL = Math.max(0, _normal.dot(_dir));
            if (NdotL <= 0) continue;

            const t = 1 - (dist / rangeWorld);
            const attenuation = t * t;

            if (isOccluded(_vertPos, _normal, _lightPos, occluders)) continue;

            totalR += light.color.r * light.intensity * NdotL * attenuation;
            totalG += light.color.g * light.intensity * NdotL * attenuation;
            totalB += light.color.b * light.intensity * NdotL * attenuation;
        }

        if (aoSamples > 0) {
            const ao = computeAO(_vertPos, _normal, occluders, aoSamples);
            totalR *= ao;
            totalG *= ao;
            totalB *= ao;
        }

        colors.setXYZ(i,
            Math.min(1, totalR),
            Math.min(1, totalG),
            Math.min(1, totalB),
        );
    }

    colors.needsUpdate = true;
}

// Track whether geometry has already been subdivided
let geometrySubdivided = false;

// Bake all scene geometry
// aoSamples: 0 to skip AO, 32-64 for quality AO
export function bakeAllLighting(aoSamples = 32) {
    const lights = state.pointLights;

    const t0 = performance.now();

    clearBakedColorStore();

    // Subdivide geometry only on first bake — subsequent re-bakes reuse subdivided meshes
    if (!geometrySubdivided) {
        for (const [, mesh] of platformMeshes) {
            subdivideMesh(mesh, null);
        }
        for (const [, mesh] of stairRunMeshes) {
            subdivideMesh(mesh, null);
        }
        for (const [, mesh] of platformMeshes) {
            for (const child of mesh.children) {
                if (child.isMesh && child.geometry.getAttribute('color')) {
                    subdivideMesh(child, null);
                }
            }
        }
        for (const [, mesh] of stairRunMeshes) {
            for (const child of mesh.children) {
                if (child.isMesh && child.geometry.getAttribute('color')) {
                    subdivideMesh(child, null);
                }
            }
        }
        geometrySubdivided = true;
    }

    const occluders = getOccluders();

    // Bake lighting onto subdivided platform/stair geometry.
    const ambient = state.ambientIntensity;
    for (const [id, mesh] of platformMeshes) {
        bakeMeshAndChildren(mesh, occluders, lights, aoSamples, 'plat_' + id, ambient);
    }
    for (const [id, mesh] of stairRunMeshes) {
        bakeMeshAndChildren(mesh, occluders, lights, aoSamples, 'stair_' + id, ambient);
    }

    // Bake CSG region meshes in place. Flavor A: no spatial-hash transfer and no
    // backing store — colors are written directly into the live mesh's color
    // attribute. Any subsequent rebuildAllCSG() (geometry edit, view-mode toggle,
    // retexture) reconstructs the mesh with white vertex colors and clears
    // state.bakedLighting, so the user must press B to re-bake.
    // Subdivision is intentionally skipped: assignUVsAndZones already produces
    // small triangles via splitTrisAtAxis, so a coarse subdivide pass would buy
    // little and would also be invalidated on the next CSG rebuild.
    for (const [, data] of csgRegionMeshes) {
        bakeGeometry(data.mesh.geometry, occluders, lights, aoSamples, ambient);
    }

    state.bakedLighting = true;

    const elapsed = ((performance.now() - t0) / 1000).toFixed(2);
    return elapsed;
}

// Clear bake: reset vertex colors to white on the (already subdivided) geometry.
// Does NOT rebuild meshes — subdivision is kept for fast re-bakes.
export function clearBakedLighting() {
    function resetColors(geometry) {
        const colors = geometry.getAttribute('color');
        if (!colors) return;
        for (let i = 0; i < colors.count; i++) {
            colors.setXYZ(i, 1, 1, 1);
        }
        colors.needsUpdate = true;
    }

    function resetMeshAndChildren(mesh) {
        resetColors(mesh.geometry);
        for (const child of mesh.children) {
            if (!child.isMesh) continue;
            resetColors(child.geometry);
            if (child.material && child.material.vertexColors) {
                child.material.vertexColors = false;
                child.material.needsUpdate = true;
            }
        }
    }

    for (const [, mesh] of platformMeshes) resetMeshAndChildren(mesh);
    for (const [, mesh] of stairRunMeshes) resetMeshAndChildren(mesh);
    for (const [, data] of csgRegionMeshes) resetColors(data.mesh.geometry);

    clearBakedColorStore();
    state.bakedLighting = false;
}
