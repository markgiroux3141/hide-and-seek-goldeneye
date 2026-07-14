// CSG stair mesh lifecycle — rebuild, remove, rebuildAll.
// Each confirmed CSG stair (state.csg.csgStairs[]) gets a visual mesh
// for treads/risers/sides. The void brush handles the CSG carving.

import * as THREE from 'three';
import { state } from '../state.js';
import { buildCsgStairGeometry } from '../geometry/csgStairGeometry.js';
import { getWallMaterial, getTexturedMaterialArrayForScheme } from '../scene/materials.js';
import { scene } from '../scene/setup.js';

// CSG stair mesh storage: Map<stairDescriptorId, THREE.Mesh>
export const csgStairMeshes = new Map();

export function rebuildCsgStair(descriptor) {
    const old = csgStairMeshes.get(descriptor.id);
    if (old) {
        scene.remove(old);
        old.geometry.dispose();
    }

    const options = {};
    if (state.viewMode === 'textured') {
        options.viewMode = 'textured';
    }
    const geometry = buildCsgStairGeometry(descriptor, options);

    let material;
    if (state.viewMode === 'textured') {
        material = getTexturedMaterialArrayForScheme(descriptor.schemeKey || 'facility_white_tile', THREE.DoubleSide);
    } else {
        material = getWallMaterial();
        material.vertexColors = true;
        material.map.repeat.set(1, 1);
        material.side = THREE.DoubleSide;
    }

    const mesh = new THREE.Mesh(geometry, material);
    mesh.userData = { csgStairId: descriptor.id };
    mesh.castShadow = true;
    mesh.receiveShadow = true;

    const edges = new THREE.EdgesGeometry(geometry);
    const wireframe = new THREE.LineSegments(edges, new THREE.LineBasicMaterial({ color: 0x333333 }));
    mesh.add(wireframe);

    csgStairMeshes.set(descriptor.id, mesh);
    scene.add(mesh);
}

export function removeCsgStairMesh(id) {
    const mesh = csgStairMeshes.get(id);
    if (mesh) {
        scene.remove(mesh);
        mesh.geometry.dispose();
        csgStairMeshes.delete(id);
    }
}

export function rebuildAllCsgStairs() {
    for (const [, mesh] of csgStairMeshes) {
        scene.remove(mesh);
        mesh.geometry.dispose();
    }
    csgStairMeshes.clear();
    for (const desc of state.csg.csgStairs) {
        rebuildCsgStair(desc);
    }
}
