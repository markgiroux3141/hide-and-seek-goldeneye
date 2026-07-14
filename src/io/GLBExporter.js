// Export the current level scene to a binary glTF (.glb) file with embedded textures.
//
// Strips the auto-resizing CSG shell (brushId === -1) from CSG meshes — its
// outer faces are an editing-time container, not part of the final level.
// Skips light helpers, gizmos, wireframe overlays, and grid lines.

import * as THREE from 'three';
import { GLTFExporter } from 'three/addons/exporters/GLTFExporter.js';
import { state } from '../state.js';
import { csgRegionMeshes } from '../mesh/csgMesh.js';
import { platformMeshes } from '../mesh/platformMesh.js';
import { stairRunMeshes } from '../mesh/stairRunMesh.js';
import { terrainMeshes, terrainWallMeshes } from '../mesh/terrainMesh.js';
import { csgStairMeshes } from '../mesh/csgStairMesh.js';

export function exportSceneToGLB(filename = 'level.glb') {
    const exportScene = buildExportScene();
    const exporter = new GLTFExporter();
    exporter.parse(
        exportScene,
        (result) => {
            downloadGLB(result, filename);
            const sidecarName = filename.replace(/\.glb$/i, '.lights.json');
            downloadLightSidecar(sidecarName);
        },
        (err) => console.error('GLB export failed:', err),
        { binary: true, embedImages: true, onlyVisible: true },
    );
}

// Sidecar JSON consumed by the in-game runtime. Positions stay in WT units
// (the runtime multiplies by WORLD_SCALE = 0.25 to get Three.js meters).
function downloadLightSidecar(filename) {
    const sidecar = {
        version: 1,
        ambient: { intensity: state.ambientIntensity },
        pointLights: state.pointLights.map(l => l.toJSON()),
    };
    const blob = new Blob([JSON.stringify(sidecar, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
}

function buildExportScene() {
    const root = new THREE.Scene();
    const matCache = new WeakMap();

    if (state.isBaked && state.bakedMesh) {
        // Post-bake path: state.bakedMesh contains the frozen CSG + cave mesh
        // (possibly cleaned up with post-bake triangle-delete clicks). Export
        // it directly — shell stripping already happened at bake time.
        for (const child of state.bakedMesh.children) {
            if (!child.isMesh) continue;
            const exported = cloneMeshTree(child, matCache, null);
            if (exported) root.add(exported);
        }
    } else {
        // Pre-bake path: live CSG meshes with auto-resizing shell stripped.
        for (const { mesh, faceIds } of csgRegionMeshes.values()) {
            const exported = cloneMeshTree(mesh, matCache, faceIds);
            if (exported) root.add(exported);
        }
    }

    for (const map of [platformMeshes, stairRunMeshes, terrainMeshes, terrainWallMeshes, csgStairMeshes]) {
        for (const mesh of map.values()) {
            const exported = cloneMeshTree(mesh, matCache, null);
            if (exported) root.add(exported);
        }
    }

    return root;
}

// Recursively clone a Mesh + its Mesh children (e.g. railings) with PBR
// materials. Skips LineSegments wireframe overlays and any non-Mesh children.
// Only the root CSG mesh gets shell-stripped (faceIds applies to it alone).
function cloneMeshTree(srcMesh, matCache, faceIdsForRoot) {
    if (!srcMesh) return null;

    let out = null;
    if (srcMesh.isMesh) {
        const geometry = faceIdsForRoot
            ? stripShellTriangles(srcMesh.geometry, faceIdsForRoot)
            : srcMesh.geometry;
        if (!geometry) return null;
        const material = Array.isArray(srcMesh.material)
            ? srcMesh.material.map((m) => toStandard(m, matCache))
            : toStandard(srcMesh.material, matCache);
        out = new THREE.Mesh(geometry, material);
    } else {
        out = new THREE.Group();
    }
    out.position.copy(srcMesh.position);
    out.quaternion.copy(srcMesh.quaternion);
    out.scale.copy(srcMesh.scale);

    for (const child of srcMesh.children) {
        if (!child.isMesh) continue; // skip LineSegments and helpers
        const childOut = cloneMeshTree(child, matCache, null);
        if (childOut) out.add(childOut);
    }
    return out;
}

// glTF requires PBR materials. Map Lambert/Standard onto MeshStandardMaterial,
// preserving textures, vertex colors, and side. Cache so the exporter can dedup.
function toStandard(src, cache) {
    if (!src) return new THREE.MeshStandardMaterial({ color: 0xffffff });
    if (cache.has(src)) return cache.get(src);
    if (src.isMeshStandardMaterial) {
        cache.set(src, src);
        return src;
    }
    const std = new THREE.MeshStandardMaterial({
        map: src.map || null,
        alphaMap: src.alphaMap || null,
        color: src.color ? src.color.clone() : new THREE.Color(0xffffff),
        vertexColors: !!src.vertexColors,
        side: src.side ?? THREE.FrontSide,
        transparent: !!src.transparent,
        opacity: src.opacity ?? 1,
        alphaTest: src.alphaTest ?? 0,
        roughness: 1,
        metalness: 0,
    });
    cache.set(src, std);
    return std;
}

// Rebuild a CSG geometry's index buffer with shell triangles removed.
// faceIds[t] corresponds to the t-th index triplet (uvZones.js sorts triangles
// by material then emits both indices and faceIds in lockstep). Vertex buffers
// stay shared; we just drop unwanted indices and recompute material groups.
function stripShellTriangles(srcGeo, faceIds) {
    const srcIndex = srcGeo.index;
    if (!srcIndex || !faceIds) return srcGeo.clone();

    const srcGroups = srcGeo.groups.length > 0
        ? srcGeo.groups
        : [{ start: 0, count: srcIndex.count, materialIndex: 0 }];

    // Map original triangle index -> group materialIndex.
    const triMatIndex = new Int32Array(srcIndex.count / 3);
    for (const g of srcGroups) {
        const startTri = g.start / 3;
        const endTri = (g.start + g.count) / 3;
        for (let t = startTri; t < endTri; t++) triMatIndex[t] = g.materialIndex;
    }

    const keptIndices = [];
    const newGroups = [];
    let runMat = -1;
    let runStart = 0;
    let runCount = 0;

    const flushRun = () => {
        if (runCount > 0) newGroups.push({ start: runStart, count: runCount, materialIndex: runMat });
    };

    for (let t = 0; t < faceIds.length; t++) {
        if (faceIds[t] && faceIds[t].brushId === -1) continue;
        const mat = triMatIndex[t];
        if (mat !== runMat) {
            flushRun();
            runMat = mat;
            runStart = keptIndices.length;
            runCount = 0;
        }
        keptIndices.push(srcIndex.getX(t * 3), srcIndex.getX(t * 3 + 1), srcIndex.getX(t * 3 + 2));
        runCount += 3;
    }
    flushRun();

    if (keptIndices.length === 0) return null;

    const out = new THREE.BufferGeometry();
    for (const name of ['position', 'normal', 'uv', 'color']) {
        const a = srcGeo.getAttribute(name);
        if (a) out.setAttribute(name, a);
    }
    out.setIndex(keptIndices);
    for (const g of newGroups) out.addGroup(g.start, g.count, g.materialIndex);
    out.computeBoundingBox();
    out.computeBoundingSphere();
    return out;
}

function downloadGLB(arrayBuffer, filename) {
    const blob = new Blob([arrayBuffer], { type: 'model/gltf-binary' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
}
