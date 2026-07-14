// Terrain mode mouse event handlers

import { WORLD_SCALE } from '../core/constants.js';
import { state, saveUndoState } from '../state.js';
import { isPointerLocked } from '../input/input.js';
import { showMessage } from '../hud/hud.js';
import { handleOrthoMiddleMouseDown, handleOrthoMiddleMouseUp, handleOrthoMiddleMouseMove, handleOrthoZoom, screenToWorldXZ } from '../terrain/orthographicCamera.js';
import { snapshotHeights } from '../terrain/terrainBrush.js';
import { rebuildTerrainWalls } from '../mesh/MeshManager.js';
import { getActiveTerrain } from './ToolManager.js';

// Sculpting state (module-level, shared with main.js render loop via accessors)
let isSculpting = false;
let brushHeightSnapshot = null;

export function getIsSculpting() { return isSculpting; }
export function setIsSculpting(v) { isSculpting = v; }
export function getBrushHeightSnapshot() { return brushHeightSnapshot; }
export function setBrushHeightSnapshot(v) { brushHeightSnapshot = v; }

export function handleTerrainClick(e, generateTerrainMesh) {
    if (e.button === 1) {
        if (state.terrainCameraMode === 'ortho') {
            handleOrthoMiddleMouseDown(e.clientX, e.clientY);
        }
        return;
    }
    if (e.button !== 0) return;

    const terrain = getActiveTerrain();
    if (!terrain) return;

    // Ortho mode: boundary/hole drawing
    if (state.terrainCameraMode === 'ortho') {
        const worldPos = screenToWorldXZ(e.clientX, e.clientY);
        const snappedX = Math.round(worldPos.x / WORLD_SCALE);
        const snappedZ = Math.round(worldPos.z / WORLD_SCALE);

        if (state.terrainTool === 'boundary' || state.terrainTool === 'hole') {
            const verts = state.terrainTool === 'boundary'
                ? (state.terrainDrawingPhase === 'drawing' ? state.terrainDrawingVertices : [])
                : state.terrainDrawingVertices;

            // Check if clicking near first vertex to close
            if (verts.length >= 3) {
                const first = verts[0];
                const dx = snappedX - first.x, dz = snappedZ - first.z;
                if (Math.abs(dx) <= 1 && Math.abs(dz) <= 1) {
                    saveUndoState();
                    if (state.terrainTool === 'boundary') {
                        terrain.boundary = [...verts];
                        state.terrainDrawingPhase = 'closed';
                        state.terrainDrawingVertices = [];
                        showMessage(`Boundary closed with ${terrain.boundary.length} vertices — press G to generate mesh`);
                    } else {
                        terrain.holes.push([...verts]);
                        state.terrainDrawingPhase = 'idle';
                        state.terrainDrawingVertices = [];
                        showMessage(`Hole added with ${terrain.holes[terrain.holes.length - 1].length} vertices`);
                    }
                    return;
                }
            }

            // Add vertex
            if (state.terrainDrawingPhase !== 'drawing') {
                state.terrainDrawingPhase = 'drawing';
                state.terrainDrawingVertices = [];
            }
            state.terrainDrawingVertices.push({ x: snappedX, z: snappedZ });
            showMessage(`Vertex ${state.terrainDrawingVertices.length} placed — click near first to close`);
            return;
        }

        if (state.terrainTool === 'edit' && terrain.boundary.length > 0) {
            let bestIdx = -1, bestDist = Infinity;
            for (let i = 0; i < terrain.boundary.length; i++) {
                const v = terrain.boundary[i];
                const dist = Math.abs(v.x - snappedX) + Math.abs(v.z - snappedZ);
                if (dist < bestDist) { bestDist = dist; bestIdx = i; }
            }
            if (bestIdx >= 0 && bestDist <= 3) {
                saveUndoState();
                terrain.boundary[bestIdx] = { x: snappedX, z: snappedZ };
                if (terrain.hasMesh) {
                    generateTerrainMesh(terrain);
                }
                showMessage(`Vertex ${bestIdx} moved`);
            }
            return;
        }
        return;
    }

    // Perspective mode: sculpting
    if (state.terrainCameraMode === 'perspective' && state.terrainTool === 'sculpt' && isPointerLocked()) {
        if (terrain.hasMesh) {
            isSculpting = true;
            brushHeightSnapshot = snapshotHeights(terrain);
            saveUndoState();
        }
        return;
    }
}

export function handleTerrainMouseUp(e) {
    if (e.button === 1) {
        handleOrthoMiddleMouseUp();
        return;
    }
    if (e.button === 0 && isSculpting) {
        isSculpting = false;
        brushHeightSnapshot = null;
        const terrain = getActiveTerrain();
        if (terrain && terrain.hasMesh) {
            rebuildTerrainWalls(terrain);
        }
    }
}

export function handleTerrainMouseMove(e) {
    if (state.terrainCameraMode === 'ortho') {
        handleOrthoMiddleMouseMove(e.clientX, e.clientY);
    }
}

export function handleTerrainWheel(e) {
    if (state.terrainCameraMode === 'ortho') {
        e.preventDefault();
        handleOrthoZoom(e.deltaY);
    }
}
