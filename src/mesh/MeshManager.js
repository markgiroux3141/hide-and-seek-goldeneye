// MeshManager — central coordinator for all mesh rebuild/remove operations

import { platformMeshes, rebuildAllPlatforms } from './platformMesh.js';
import { stairRunMeshes, rebuildAllStairRuns } from './stairRunMesh.js';
import { rebuildAllTerrain } from './terrainMesh.js';
import { lightMeshes, rebuildAllLights } from './lightMesh.js';
import { csgRegionMeshes, rebuildAllCSG } from './csgMesh.js';
import { csgStairMeshes, rebuildAllCsgStairs } from './csgStairMesh.js';

// Re-export mesh Maps for external access (raycasting, previews, etc.)
export { platformMeshes } from './platformMesh.js';
export { stairRunMeshes } from './stairRunMesh.js';
export { terrainMeshes, terrainWallMeshes } from './terrainMesh.js';
export { lightMeshes } from './lightMesh.js';
export { csgRegionMeshes } from './csgMesh.js';

// Re-export individual rebuild/remove functions
export { rebuildPlatform, rebuildAllPlatforms, removePlatformMesh } from './platformMesh.js';
export { rebuildStairRun, rebuildAllStairRuns, rebuildConnectedStairRuns } from './stairRunMesh.js';
export { rebuildTerrainMesh, rebuildTerrainWalls, rebuildAllTerrain, generateTerrainMesh } from './terrainMesh.js';
export { rebuildLight, rebuildAllLights, removeLightMesh, updateLightSelection, getLightPickTargets, updateLightShadowFlag } from './lightMesh.js';
export { rebuildAllCSG, rebuildAffectedRegions, removeCSGRegion } from './csgMesh.js';
export { rebuildCsgStair, rebuildAllCsgStairs, removeCsgStairMesh, csgStairMeshes } from './csgStairMesh.js';

// Rebuild everything (CSG + platforms + stair runs + terrain + lights + CSG stairs) — used for undo/load
// CSG runs first so platform/stair placement raycasts can hit it.
export function rebuildAll() {
    rebuildAllCSG();
    rebuildAllCsgStairs();
    rebuildAllPlatforms();
    rebuildAllStairRuns();
    rebuildAllTerrain();
    rebuildAllLights();
}

// Toggle visibility of all indoor meshes (CSG + platforms + stair runs + lights)
export function setIndoorMeshesVisible(visible) {
    for (const [, data] of csgRegionMeshes) data.mesh.visible = visible;
    for (const [, mesh] of platformMeshes) mesh.visible = visible;
    for (const [, mesh] of stairRunMeshes) mesh.visible = visible;
    for (const [, mesh] of csgStairMeshes) mesh.visible = visible;
    for (const [, group] of lightMeshes) group.visible = visible;
}

// Toggle wireframe (LineSegments) visibility on all indoor meshes
export function setAllWireframeVisible(visible) {
    function setWireframe(mesh) {
        for (const child of mesh.children) {
            if (child.isLineSegments) child.visible = visible;
        }
    }
    for (const [, data] of csgRegionMeshes) setWireframe(data.mesh);
    for (const [, mesh] of platformMeshes) setWireframe(mesh);
    for (const [, mesh] of stairRunMeshes) setWireframe(mesh);
    for (const [, mesh] of csgStairMeshes) setWireframe(mesh);
}
