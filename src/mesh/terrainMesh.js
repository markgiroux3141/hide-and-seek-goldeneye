// Terrain mesh lifecycle — rebuild terrain surfaces and boundary walls

import * as THREE from 'three';
import { state } from '../state.js';
import { buildTerrainGeometry } from '../geometry/terrainGeometry.js';
import { buildPlaneWallGeometry, buildRockyWallGeometry } from '../geometry/boundaryWalls.js';
import { triangulateTerrain } from '../geometry/triangulation.js';
import { showMessage } from '../hud/hud.js';
import { scene } from '../scene/setup.js';

// Terrain mesh storage
export const terrainMeshes = new Map();       // terrainId -> THREE.Mesh
export const terrainWallMeshes = new Map();   // terrainId -> THREE.Mesh

export function rebuildTerrainMesh(terrain) {
    // Remove old mesh
    const old = terrainMeshes.get(terrain.id);
    if (old) { scene.remove(old); old.geometry.dispose(); }

    if (!terrain.hasMesh) { terrainMeshes.delete(terrain.id); return; }

    const geometry = buildTerrainGeometry(terrain);
    const material = new THREE.MeshLambertMaterial({ vertexColors: true, side: THREE.DoubleSide });
    const mesh = new THREE.Mesh(geometry, material);
    mesh.userData = { terrainId: terrain.id };
    mesh.castShadow = true;
    mesh.receiveShadow = true;

    const wire = new THREE.WireframeGeometry(geometry);
    const wireframe = new THREE.LineSegments(wire, new THREE.LineBasicMaterial({ color: 0x000000 }));
    wireframe.visible = state.showWireframe;
    mesh.add(wireframe);

    terrainMeshes.set(terrain.id, mesh);
    scene.add(mesh);
}

export function rebuildTerrainWalls(terrain) {
    const old = terrainWallMeshes.get(terrain.id);
    if (old) { scene.remove(old); old.geometry.dispose(); }

    if (!terrain.hasMesh) { terrainWallMeshes.delete(terrain.id); return; }

    let geometry;
    if (terrain.wallStyle === 'rocky') {
        geometry = buildRockyWallGeometry(terrain);
    } else {
        geometry = buildPlaneWallGeometry(terrain);
    }

    if (!geometry.getAttribute('position') || geometry.getAttribute('position').count === 0) return;

    const material = new THREE.MeshLambertMaterial({ vertexColors: true, side: THREE.DoubleSide });
    const mesh = new THREE.Mesh(geometry, material);
    mesh.userData = { terrainWallId: terrain.id };
    mesh.castShadow = true;
    mesh.receiveShadow = true;

    terrainWallMeshes.set(terrain.id, mesh);
    scene.add(mesh);
}

export function rebuildAllTerrain() {
    for (const [id, mesh] of terrainMeshes) { scene.remove(mesh); mesh.geometry.dispose(); }
    terrainMeshes.clear();
    for (const [id, mesh] of terrainWallMeshes) { scene.remove(mesh); mesh.geometry.dispose(); }
    terrainWallMeshes.clear();

    for (const t of state.terrainMaps) {
        rebuildTerrainMesh(t);
        rebuildTerrainWalls(t);
    }
}

export function generateTerrainMesh(terrain) {
    const result = triangulateTerrain(terrain.boundary, terrain.holes, terrain.subdivisionLevel);
    terrain.vertices = result.vertices;
    terrain.triangles = result.triangles;
    rebuildTerrainMesh(terrain);
    rebuildTerrainWalls(terrain);
    showMessage(`Mesh generated: ${terrain.vertices.length} vertices, ${terrain.triangles.length} triangles`);
}
