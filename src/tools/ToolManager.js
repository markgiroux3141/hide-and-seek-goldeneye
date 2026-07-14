// Tool cycling, mode switching, and shared tool state management

import { state } from '../state.js';
import { showMessage } from '../hud/hud.js';
import { setPointerLockEnabled } from '../input/input.js';
import { setIndoorMeshesVisible } from '../mesh/MeshManager.js';
import { TerrainMap } from '../core/TerrainMap.js';

const TOOL_LABELS = { csg: 'CSG', platform: 'Platform', light: 'Light' };
const TERRAIN_TOOL_CYCLE = ['boundary', 'hole', 'edit', 'sculpt'];
const TERRAIN_TOOL_NAMES = { boundary: 'Boundary', hole: 'Hole', edit: 'Edit', sculpt: 'Sculpt' };
const BRUSH_CYCLE = ['raise', 'noise', 'smooth', 'flatten'];
const BRUSH_NAMES = { raise: 'Raise/Lower', noise: 'Noise', smooth: 'Smooth', flatten: 'Flatten' };

// Gizmo and camera references set during init
let _gizmo = null;
let _camera = null;

export function initToolManager(gizmo, camera) {
    _gizmo = gizmo;
    _camera = camera;
}

export function clearPlatformToolState() {
    if (_gizmo.isDragging()) _gizmo.cancelDrag();
    state.platformPhase = 'idle';
    state.selectedPlatformId = null;
    state.selectedStairRunId = null;
    state.platformMoveAxis = null;
    state.platformScaleAxis = null;
    state.platformConnectFrom = null;
    state.platformConnectTo = null;
    state.simpleStairFrom = null;
    _gizmo.update(null, _camera);
}

export function clearLightToolState() {
    if (_gizmo.isDragging()) _gizmo.cancelDrag();
    state.selectedLightId = null;
    state.lightPhase = 'idle';
    _gizmo.update(null, _camera);
}

// Reset all CSG tool sub-state. Called when leaving the CSG tool so the next
// time the user enters it they get a clean slate (no stale selection / hole mode).
export function clearCSGToolState() {
    state.csg.selectedFace = null;
    state.csg.selectedFaces = [];
    state.csg.activeBrush = null;
    state.csg.activeOp = null;
    state.csg.activeSide = null;
    state.csg.holeMode = false;
    state.csg.holeDoor = false;
    state.csg.doorPreview = null;
    state.csg.facePaintMode = false;
    state.csg.selSizeU = 0;
    state.csg.selSizeV = 0;
}

// Single source of truth for switching the indoor tool. Both the radial menu
// and the numpad hotkey handler call this. Pure tool switch — sub-mode entry
// (hole/door/simple_stairs) is composed at the call site.
export function setTool(toolName) {
    if (toolName !== 'csg' && toolName !== 'platform' && toolName !== 'light') {
        console.warn('setTool: unknown tool', toolName);
        return;
    }
    if (state.tool === 'csg' && toolName !== 'csg') clearCSGToolState();
    state.tool = toolName;
    if (toolName !== 'platform') clearPlatformToolState();
    if (toolName !== 'light') clearLightToolState();
    showMessage('Tool: ' + TOOL_LABELS[toolName]);
}

// Terrain mode T-key cycles the terrain tool (boundary/hole/edit/sculpt).
// This is unrelated to indoor tool switching, which is now flat-numpad only.
export function cycleTerrainTool() {
    const idx = TERRAIN_TOOL_CYCLE.indexOf(state.terrainTool);
    state.terrainTool = TERRAIN_TOOL_CYCLE[(idx + 1) % TERRAIN_TOOL_CYCLE.length];
    showMessage('Terrain Tool: ' + TERRAIN_TOOL_NAMES[state.terrainTool]);
}

export function toggleEditorMode() {
    if (state.editorMode === 'indoor') {
        state.editorMode = 'terrain';
        state.terrainCameraMode = 'ortho';
        if (document.pointerLockElement) document.exitPointerLock();
        setPointerLockEnabled(false);
        document.getElementById('lock-prompt').style.display = 'none';
        document.getElementById('crosshair').style.display = 'none';
        setIndoorMeshesVisible(false);
        if (state.terrainMaps.length === 0) {
            const t = new TerrainMap(state.nextTerrainMapId++);
            state.terrainMaps.push(t);
            state.selectedTerrainId = t.id;
        } else {
            state.selectedTerrainId = state.terrainMaps[0].id;
        }
        state.terrainTool = 'boundary';
        state.terrainDrawingPhase = 'idle';
        state.terrainDrawingVertices = [];
        showMessage('TERRAIN MODE — Orthographic top-down view');
    } else {
        state.editorMode = 'indoor';
        state.terrainCameraMode = 'ortho';
        setPointerLockEnabled(true);
        setIndoorMeshesVisible(true);
        showMessage('INDOOR MODE — click to lock cursor');
    }
}

export function clearTerrainDrawingState() {
    state.terrainDrawingPhase = 'idle';
    state.terrainDrawingVertices = [];
}

export function cycleTerrainBrush() {
    const idx = BRUSH_CYCLE.indexOf(state.brushType);
    state.brushType = BRUSH_CYCLE[(idx + 1) % BRUSH_CYCLE.length];
    showMessage('Brush: ' + BRUSH_NAMES[state.brushType]);
}

export function toggleTerrainCamera() {
    if (state.terrainCameraMode === 'ortho') {
        state.terrainCameraMode = 'perspective';
        setPointerLockEnabled(true);
        showMessage('Perspective view — click to lock cursor');
    } else {
        state.terrainCameraMode = 'ortho';
        if (document.pointerLockElement) document.exitPointerLock();
        setPointerLockEnabled(false);
        showMessage('Orthographic top-down view');
    }
}

export function getActiveTerrain() {
    if (state.selectedTerrainId == null) return null;
    return state.terrainMaps.find(t => t.id === state.selectedTerrainId) || null;
}
