// Stair run mesh lifecycle — rebuild, remove

import * as THREE from 'three';
import { state } from '../state.js';
import { buildStairRunRailingGeometry } from '../geometry/platformGeometry.js';
import { getPlatformStyle } from '../geometry/platformStyles.js';
import { getWallMaterial, getTexturedMaterialArrayForScheme, getRailingMaterial, getRailingGridMaterial } from '../scene/materials.js';
import { scene } from '../scene/setup.js';

// Stair run mesh storage: Map<stairRunId, THREE.Mesh>
export const stairRunMeshes = new Map();

export function rebuildStairRun(run) {
    const old = stairRunMeshes.get(run.id);
    if (old) {
        scene.remove(old);
        old.geometry.dispose();
    }

    const fromPlat = run.fromPlatformId != null ? state.platforms.find(p => p.id === run.fromPlatformId) : null;
    const toPlat = run.toPlatformId != null ? state.platforms.find(p => p.id === run.toPlatformId) : null;

    // Effective style: prefer the run's own style; fall back to its connected
    // platforms (existing saves predate the field, and stairs created from a
    // simple-style platform should pick that up automatically).
    const styleName = run.style || fromPlat?.style || toPlat?.style || 'default';
    const style = getPlatformStyle(styleName);
    const side = style.doubleSided ? THREE.DoubleSide : THREE.FrontSide;

    const options = { brushes: state.csg.brushes };
    if (state.viewMode === 'textured') {
        options.viewMode = 'textured';
    }
    const geometry = style.buildStair(run, fromPlat, toPlat, options);

    let material;
    if (state.viewMode === 'textured') {
        material = getTexturedMaterialArrayForScheme(style.schemeName, side);
    } else {
        material = getWallMaterial();
        material.vertexColors = true;
        material.map.repeat.set(1, 1);
        material.side = side;
    }
    const mesh = new THREE.Mesh(geometry, material);
    mesh.userData = { stairRunId: run.id };
    mesh.castShadow = true;
    mesh.receiveShadow = true;

    const edges = new THREE.EdgesGeometry(geometry);
    const wireframe = new THREE.LineSegments(edges, new THREE.LineBasicMaterial({ color: 0x333333 }));
    mesh.add(wireframe);

    // Add railings if enabled
    if (run.railings) {
        const railGeo = buildStairRunRailingGeometry(run, fromPlat, toPlat, state.csg.brushes);
        if (railGeo.getAttribute('position') && railGeo.getAttribute('position').count > 0) {
            const textured = state.viewMode === 'textured';
            const railMat = textured ? getRailingMaterial() : getRailingGridMaterial();
            const railMesh = new THREE.Mesh(railGeo, railMat);
            railMesh.renderOrder = 1;
            railMesh.castShadow = textured;
            railMesh.receiveShadow = textured;
            mesh.add(railMesh);
        }
    }

    stairRunMeshes.set(run.id, mesh);
    scene.add(mesh);
}

export function rebuildAllStairRuns() {
    for (const [id, mesh] of stairRunMeshes) {
        scene.remove(mesh);
        mesh.geometry.dispose();
    }
    stairRunMeshes.clear();
    for (const run of state.stairRuns) {
        rebuildStairRun(run);
    }
}

// Rebuild all stair runs connected to a specific platform
export function rebuildConnectedStairRuns(platformId) {
    for (const run of state.stairRuns) {
        if (run.fromPlatformId === platformId || run.toPlatformId === platformId) {
            rebuildStairRun(run);
        }
    }
}
