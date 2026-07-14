// Terrain mode key handler

import { state, saveUndoState } from '../state.js';
import { isPointerLocked } from '../input/input.js';
import { showMessage } from '../hud/hud.js';
import { gridHelper } from '../scene/setup.js';
import { hotkeyManager } from '../input/HotkeyManager.js';
import { MIN_BRUSH_RADIUS, MAX_BRUSH_RADIUS, MIN_BRUSH_STRENGTH, MAX_BRUSH_STRENGTH, MIN_SUBDIVISION, MAX_SUBDIVISION } from '../core/constants.js';
import { undoAction, saveLevel, loadLevel } from '../actions.js';
import { exportSceneToGLB } from '../io/GLBExporter.js';
import { rebuildTerrainWalls, terrainMeshes } from '../mesh/MeshManager.js';
import {
    toggleEditorMode, cycleTerrainTool, toggleTerrainCamera,
    cycleTerrainBrush, clearTerrainDrawingState, getActiveTerrain,
} from './ToolManager.js';

export function handleTerrainKey(e, { generateTerrainMesh, rebuildAll }) {
    // M = switch back to indoor mode
    if (e.code === 'KeyM') {
        e.preventDefault();
        toggleEditorMode();
        return;
    }

    // T = cycle terrain tool
    if (e.code === 'KeyT') {
        e.preventDefault();
        cycleTerrainTool();
        return;
    }

    // Tab = toggle ortho/perspective camera
    if (e.code === 'Tab') {
        e.preventDefault();
        toggleTerrainCamera();
        return;
    }

    // B = cycle brush type (in sculpt mode)
    if (e.code === 'KeyB' && state.terrainTool === 'sculpt') {
        e.preventDefault();
        cycleTerrainBrush();
        return;
    }

    // G = generate mesh from boundary
    if (e.code === 'KeyG') {
        e.preventDefault();
        const terrain = getActiveTerrain();
        if (!terrain) return;
        if (!terrain.isClosed) {
            showMessage('Close the boundary first (click near first vertex)');
            return;
        }
        saveUndoState();
        generateTerrainMesh(terrain);
        return;
    }

    // Shift+W = toggle wall style
    if (e.code === 'KeyW' && e.shiftKey) {
        e.preventDefault();
        const terrain = getActiveTerrain();
        if (!terrain) return;
        terrain.wallStyle = terrain.wallStyle === 'plane' ? 'rocky' : 'plane';
        if (terrain.hasMesh) rebuildTerrainWalls(terrain);
        const whInput = document.getElementById('terrain-wall-height');
        if (whInput) whInput.value = terrain.wallStyle === 'rocky' ? terrain.rockyWallHeight : terrain.wallHeight;
        showMessage('Wall style: ' + terrain.wallStyle);
        return;
    }

    // +/- = adjust brush radius (sculpt mode)
    if (state.terrainTool === 'sculpt') {
        if (e.key === '=' || e.key === '+') {
            e.preventDefault();
            state.brushRadius = Math.min(MAX_BRUSH_RADIUS, state.brushRadius + 1);
            showMessage(`Brush radius: ${state.brushRadius}`);
            return;
        }
        if (e.key === '-') {
            e.preventDefault();
            state.brushRadius = Math.max(MIN_BRUSH_RADIUS, state.brushRadius - 1);
            showMessage(`Brush radius: ${state.brushRadius}`);
            return;
        }
        if (e.code === 'BracketRight') {
            e.preventDefault();
            state.brushStrength = Math.min(MAX_BRUSH_STRENGTH, state.brushStrength + 0.1);
            showMessage(`Brush strength: ${state.brushStrength.toFixed(1)}`);
            return;
        }
        if (e.code === 'BracketLeft') {
            e.preventDefault();
            state.brushStrength = Math.max(MIN_BRUSH_STRENGTH, state.brushStrength - 0.1);
            showMessage(`Brush strength: ${state.brushStrength.toFixed(1)}`);
            return;
        }
    }

    // +/- = adjust subdivision level (boundary/hole tool)
    if (state.terrainTool === 'boundary' || state.terrainTool === 'hole') {
        if (e.key === '=' || e.key === '+') {
            e.preventDefault();
            const terrain = getActiveTerrain();
            if (terrain) {
                terrain.subdivisionLevel = Math.min(MAX_SUBDIVISION, terrain.subdivisionLevel + 1);
                showMessage(`Subdivision: ${terrain.subdivisionLevel}`);
            }
            return;
        }
        if (e.key === '-') {
            e.preventDefault();
            const terrain = getActiveTerrain();
            if (terrain) {
                terrain.subdivisionLevel = Math.max(MIN_SUBDIVISION, terrain.subdivisionLevel - 1);
                showMessage(`Subdivision: ${terrain.subdivisionLevel}`);
            }
            return;
        }
    }

    // Backspace = undo last vertex while drawing
    if (e.code === 'Backspace' && state.terrainDrawingPhase === 'drawing') {
        e.preventDefault();
        state.terrainDrawingVertices.pop();
        if (state.terrainDrawingVertices.length === 0) {
            state.terrainDrawingPhase = 'idle';
        }
        showMessage(`Vertices: ${state.terrainDrawingVertices.length}`);
        return;
    }

    // Escape = cancel drawing
    if (e.code === 'Escape') {
        e.preventDefault();
        if (state.terrainDrawingPhase === 'drawing') {
            clearTerrainDrawingState();
            showMessage('Drawing cancelled');
        }
        return;
    }

    // Ctrl+Z undo
    if (e.ctrlKey && e.code === 'KeyZ') {
        e.preventDefault();
        clearTerrainDrawingState();
        undoAction(showMessage, rebuildAll);
        return;
    }

    // Ctrl+S save / Ctrl+O load
    if (e.ctrlKey && e.code === 'KeyS') {
        e.preventDefault();
        saveLevel(showMessage);
        return;
    }
    if (e.ctrlKey && e.code === 'KeyO') {
        e.preventDefault();
        loadLevel(showMessage, rebuildAll);
        return;
    }
    if (hotkeyManager.matches('export_glb', e)) {
        e.preventDefault();
        exportSceneToGLB();
        showMessage('Exported level.glb');
        return;
    }

    // Grid toggle
    if (hotkeyManager.matches('toggle_grid', e)) {
        e.preventDefault();
        state.showGrid = !state.showGrid;
        if (gridHelper) gridHelper.visible = state.showGrid;
        showMessage('Grid: ' + (state.showGrid ? 'ON' : 'OFF'));
        return;
    }

    // E = toggle terrain wireframe edges
    if (e.code === 'KeyE') {
        e.preventDefault();
        state.showWireframe = !state.showWireframe;
        for (const [, mesh] of terrainMeshes) {
            for (const child of mesh.children) {
                if (child.isLineSegments) child.visible = state.showWireframe;
            }
        }
        showMessage('Wireframe: ' + (state.showWireframe ? 'ON' : 'OFF'));
        return;
    }
}
