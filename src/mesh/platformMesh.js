// Platform mesh lifecycle — rebuild, remove

import * as THREE from 'three';
import { state } from '../state.js';
import { buildPlatformRailingGeometry } from '../geometry/platformGeometry.js';
import { getPlatformStyle } from '../geometry/platformStyles.js';
import { getWallMaterial, getTexturedMaterialArrayForScheme, getRailingMaterial, getRailingGridMaterial } from '../scene/materials.js';
import { scene } from '../scene/setup.js';

// Platform mesh storage: Map<platformId, THREE.Mesh>
export const platformMeshes = new Map();

export function rebuildPlatform(plat) {
    const old = platformMeshes.get(plat.id);
    if (old) {
        scene.remove(old);
        old.geometry.dispose();
    }

    const style = getPlatformStyle(plat.style);
    const side = style.doubleSided ? THREE.DoubleSide : THREE.FrontSide;

    const options = { brushes: state.csg.brushes };
    if (state.viewMode === 'textured') {
        options.viewMode = 'textured';
    }
    const geometry = style.buildPlatform(plat, options);

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
    mesh.userData = { platformId: plat.id };
    mesh.castShadow = true;
    mesh.receiveShadow = true;

    const edges = new THREE.EdgesGeometry(geometry);
    const wireframe = new THREE.LineSegments(edges, new THREE.LineBasicMaterial({ color: 0x333333 }));
    mesh.add(wireframe);

    // Add railings if enabled
    if (plat.railings) {
        const connectedRuns = state.stairRuns.filter(
            r => r.fromPlatformId === plat.id || r.toPlatformId === plat.id
        );
        const railGeo = buildPlatformRailingGeometry(plat, connectedRuns, state.csg.brushes, state.platforms);
        if (railGeo.getAttribute('position') && railGeo.getAttribute('position').count > 0) {
            const textured = state.viewMode === 'textured';
            const railMat = textured ? getRailingMaterial() : getRailingGridMaterial();
            const railMesh = new THREE.Mesh(railGeo, railMat);
            railMesh.renderOrder = 1;
            // Only the textured railing has the alpha-keyed customDistanceMaterial
            // for transparent point-light shadows. Grid view's opacity-based
            // railing shouldn't cast a solid-plane shadow.
            railMesh.castShadow = textured;
            railMesh.receiveShadow = textured;
            mesh.add(railMesh);
        }
    }

    platformMeshes.set(plat.id, mesh);
    scene.add(mesh);
}

export function rebuildAllPlatforms() {
    for (const [id, mesh] of platformMeshes) {
        scene.remove(mesh);
        mesh.geometry.dispose();
    }
    platformMeshes.clear();
    for (const plat of state.platforms) {
        rebuildPlatform(plat);
    }
}

export function removePlatformMesh(platId) {
    const data = platformMeshes.get(platId);
    if (data) {
        scene.remove(data);
        data.geometry.dispose();
        platformMeshes.delete(platId);
    }
}
