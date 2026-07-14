// Editor state — single source of truth

import { Platform } from './core/Platform.js';
import { StairRun } from './core/StairRun.js';
import { TerrainMap } from './core/TerrainMap.js';
import { PointLight } from './core/PointLight.js';
import { BrushDef } from './core/BrushDef.js';
import {
    DEFAULT_PLATFORM_SIZE_X, DEFAULT_PLATFORM_SIZE_Z, DEFAULT_PLATFORM_THICKNESS,
    DEFAULT_STAIR_WIDTH, DEFAULT_STAIR_STEP_HEIGHT, DEFAULT_STAIR_RISE_OVER_RUN,
    DEFAULT_BRUSH_RADIUS, DEFAULT_BRUSH_STRENGTH, DEFAULT_BRUSH_NOISE_SCALE, DEFAULT_BRUSH_NOISE_AMP,
    DEFAULT_AMBIENT_INTENSITY, MAX_UNDO,
    DEFAULT_BRACE_WIDTH, DEFAULT_BRACE_DEPTH, DEFAULT_PILLAR_SIZE,
} from './core/constants.js';

export const state = {
    platforms: [],          // Platform[]
    stairRuns: [],          // StairRun[]
    nextPlatformId: 1,
    nextStairRunId: 1,

    // ─── CSG brush system ─────────────────────────────────────────────
    csg: {
        brushes: [],            // BrushDef[] (un-baked)
        nextBrushId: 1,
        totalBakedBrushes: 0,
        // Selection
        selectedFace: null,     // { regionId, brushId, axis, side, position }
        selectedFaces: [],      // additional coplanar full-face faces (shift-clicked); same shape as selectedFace
        selSizeU: 0, selSizeV: 0,  // 0 = full face
        selU0: 0, selU1: 0, selV0: 0, selV1: 0,  // computed each frame
        // Active push/pull/extrude tracking
        activeBrush: null,      // BrushDef being grown by consecutive +/- presses
        activeOp: null,         // 'push' | 'pull' | 'extrude'
        activeSide: null,       // 'min' | 'max' — original face side
        // Active stair-extrude tracking (Arrow keys on a wall face)
        activeStairOp: null,    // { brushIds: number[], direction: 'down'|'up', stepCount, anchorFace, selU0, selU1 }
        // Deferred stair op: arrow keys adjust counter, Enter confirms
        pendingStairOp: null,   // { direction, stepCount, axis, side, facePos, selU0, selU1, floor, H, anchorBrushId, regionId, schemeKey, floorY }
        // Confirmed CSG stairs (void brush + visual mesh descriptors)
        csgStairs: [],          // { id, voidBrushId, direction, stepCount, axis, side, facePos, selU0, selU1, floor, H, schemeKey, floorY }
        nextCsgStairId: 1,
        nextCaveId: 1,          // CaveDef ids (owned by regions, so not a brush id)
        // Hole/door modal tool state
        holeMode: false,
        holeDoor: false,
        doorPreview: null,      // { face, u0, u1, v0, v1 }
        // Brace modal tool state
        braceMode: false,
        bracePreview: null,     // { regionId, wall1, ceiling, wall2 }
        braceWidth: DEFAULT_BRACE_WIDTH,
        braceDepth: DEFAULT_BRACE_DEPTH,
        // Pillar modal tool state
        pillarMode: false,
        pillarPreview: null,    // { regionId, roomBrushId, box }
        pillarSize: DEFAULT_PILLAR_SIZE,
        // Face-paint modal tool state — click a face, press 1-9 to override its texture scheme
        facePaintMode: false,
        // Cave exit-room dimensions (WT). Adjustable via scroll in sculpt P-submode.
        // depth = along hit normal (into rock); width = face u-axis; height = face v-axis.
        exitRoomSize: { depth: 4, width: 4, height: 4 },
    },

    tool: 'csg',            // 'csg' | 'platform' | 'light'
    undoStack: [],
    maxUndo: MAX_UNDO,

    // Stair settings (shared by platform-connect stairs and simple stairs)
    stairWidth: DEFAULT_STAIR_WIDTH,
    stairStepHeight: DEFAULT_STAIR_STEP_HEIGHT,
    stairRiseOverRun: DEFAULT_STAIR_RISE_OVER_RUN,

    // Platform tool state (transient — not serialized or in undo snapshots)
    platformPhase: 'idle',    // 'idle' | 'selected' | 'moving' | 'scaling' | 'connecting_dst' | 'connecting_src' | 'simple_stair_from' | 'simple_stair_to'
    selectedPlatformId: null, // ID of currently selected platform
    selectedStairRunId: null, // ID of currently selected stair run
    platformMoveAxis: null,   // 'x' | 'y' | 'z' — constrained axis during move
    platformScaleAxis: null,  // 'x' | 'z' — constrained axis during scale
    platformConnectFrom: null, // { platformId, edge, offset } — source edge when connecting
    platformConnectTo: null,   // { type: 'ground' } | { type: 'platform', platformId, edge } — destination
    simpleStairFrom: null,     // { x, y, z, axis, side } — first click point + face for simple stairs
    platformSizeX: DEFAULT_PLATFORM_SIZE_X,
    platformSizeZ: DEFAULT_PLATFORM_SIZE_Z,
    platformThickness: DEFAULT_PLATFORM_THICKNESS,
    platformStyle: 'default',  // visual style for new platforms — see src/geometry/platformStyles.js

    // Radial menu state (transient)
    radialMenuOpen: false,

    // Destructive bake state (transient — not serialized):
    // when true, CSG authoring is locked and the scene holds a single frozen
    // mesh group (state.bakedMesh) produced by src/io/bakeLevel.js.
    isBaked: false,
    bakedMesh: null,
    // Snapshot of cave-anchor face descriptors taken at bake time. Drives the
    // post-bake cleanup-prism tool (rectangular regions at every cave/CSG
    // junction whose contents can be auto-deleted).
    bakedAnchors: [],

    // View mode (transient — not serialized or in undo snapshots)
    viewMode: 'textured',     // 'grid' | 'textured'
    showGrid: false,          // grid helper visibility
    showWireframe: true,      // terrain mesh wireframe visibility

    // Editor mode
    editorMode: 'indoor',     // 'indoor' | 'terrain'

    // Terrain data (serialized)
    terrainMaps: [],           // TerrainMap[]
    nextTerrainMapId: 1,

    // Terrain tool state (transient — not serialized or in undo snapshots)
    terrainTool: 'boundary',   // 'boundary' | 'hole' | 'edit' | 'sculpt'
    terrainDrawingPhase: 'idle', // 'idle' | 'drawing' | 'closed'
    terrainDrawingVertices: [], // Current in-progress polygon [{x, z}]
    selectedTerrainId: null,   // ID of active terrain map
    terrainCameraMode: 'ortho', // 'ortho' | 'perspective' — camera in terrain mode

    // Brush state (transient)
    brushType: 'raise',        // 'raise' | 'noise' | 'smooth' | 'flatten'
    brushRadius: DEFAULT_BRUSH_RADIUS,
    brushStrength: DEFAULT_BRUSH_STRENGTH,
    brushNoiseScale: DEFAULT_BRUSH_NOISE_SCALE,
    brushNoiseAmp: DEFAULT_BRUSH_NOISE_AMP,

    // Point lights (serialized)
    pointLights: [],           // PointLight[]
    nextPointLightId: 1,

    // Light tool state (transient)
    selectedLightId: null,
    lightPhase: 'idle',        // 'idle' | 'selected' | 'moving'

    // Realtime lighting state (transient)
    ambientIntensity: DEFAULT_AMBIENT_INTENSITY,
};

export function saveUndoState() {
    const snapshot = JSON.stringify({
        platforms: state.platforms.map(p => p.toJSON()),
        stairRuns: state.stairRuns.map(r => r.toJSON()),
        terrainMaps: state.terrainMaps.map(t => t.toJSON()),
        pointLights: state.pointLights.map(l => l.toJSON()),
        csgBrushes: state.csg.brushes.map(b => b.toJSON()),
        nextBrushId: state.csg.nextBrushId,
        totalBakedBrushes: state.csg.totalBakedBrushes,
        csgStairs: state.csg.csgStairs,
        nextCsgStairId: state.csg.nextCsgStairId,
    });
    state.undoStack.push(snapshot);
    if (state.undoStack.length > state.maxUndo) state.undoStack.shift();
}

export function undo() {
    if (state.undoStack.length === 0) return false;
    let snapshot;
    try {
        snapshot = JSON.parse(state.undoStack.pop());
    } catch (e) {
        console.warn('Failed to parse undo snapshot:', e.message);
        return false;
    }
    state.platforms = (snapshot.platforms || []).map(j => Platform.fromJSON(j));
    state.stairRuns = (snapshot.stairRuns || []).map(j => StairRun.fromJSON(j));
    state.terrainMaps = (snapshot.terrainMaps || []).map(j => TerrainMap.fromJSON(j));
    state.pointLights = (snapshot.pointLights || []).map(j => PointLight.fromJSON(j));
    state.csg.brushes = (snapshot.csgBrushes || []).map(j => BrushDef.fromJSON(j));
    state.csg.nextBrushId = snapshot.nextBrushId || (Math.max(...state.csg.brushes.map(b => b.id), 0) + 1);
    state.csg.totalBakedBrushes = snapshot.totalBakedBrushes || 0;
    state.csg.csgStairs = snapshot.csgStairs || [];
    state.csg.nextCsgStairId = snapshot.nextCsgStairId || (Math.max(...state.csg.csgStairs.map(s => s.id), 0) + 1);
    state.csg.selectedFace = null;
    state.csg.selectedFaces = [];
    state.csg.activeBrush = null;
    state.csg.activeOp = null;
    state.csg.activeSide = null;
    state.csg.activeStairOp = null;
    state.csg.pendingStairOp = null;
    state.nextPlatformId = Math.max(...state.platforms.map(p => p.id), 0) + 1;
    state.nextStairRunId = Math.max(...state.stairRuns.map(r => r.id), 0) + 1;
    state.nextTerrainMapId = Math.max(...state.terrainMaps.map(t => t.id), 0) + 1;
    state.nextPointLightId = Math.max(...state.pointLights.map(l => l.id), 0) + 1;
    state.selectedPlatformId = null;
    state.selectedStairRunId = null;
    state.selectedTerrainId = null;
    state.selectedLightId = null;
    return true;
}

export function serializeLevel() {
    return JSON.stringify({
        version: 2,
        platforms: state.platforms.map(p => p.toJSON()),
        stairRuns: state.stairRuns.map(r => r.toJSON()),
        terrainMaps: state.terrainMaps.map(t => t.toJSON()),
        pointLights: state.pointLights.map(l => l.toJSON()),
        csgBrushes: state.csg.brushes.map(b => b.toJSON()),
        nextBrushId: state.csg.nextBrushId,
        totalBakedBrushes: state.csg.totalBakedBrushes,
        csgStairs: state.csg.csgStairs,
        nextCsgStairId: state.csg.nextCsgStairId,
        nextPlatformId: state.nextPlatformId,
        nextStairRunId: state.nextStairRunId,
        nextTerrainMapId: state.nextTerrainMapId,
        nextPointLightId: state.nextPointLightId,
    }, null, 2);
}

export function deserializeLevel(json) {
    const data = JSON.parse(json);
    if (!data) throw new Error('Invalid level data');
    const version = data.version || 0;
    if (version !== 2) throw new Error('Save v1 no longer supported (Phase 6 dropped legacy Volume/Connection format)');
    state.platforms = (data.platforms || []).map(j => Platform.fromJSON(j));
    state.stairRuns = (data.stairRuns || []).map(j => StairRun.fromJSON(j));
    state.terrainMaps = (data.terrainMaps || []).map(j => TerrainMap.fromJSON(j));
    state.pointLights = (data.pointLights || []).map(j => PointLight.fromJSON(j));
    state.csg.brushes = (data.csgBrushes || []).map(j => BrushDef.fromJSON(j));
    state.csg.nextBrushId = data.nextBrushId || (Math.max(...state.csg.brushes.map(b => b.id), 0) + 1);
    state.csg.totalBakedBrushes = data.totalBakedBrushes || 0;
    state.csg.csgStairs = data.csgStairs || [];
    state.csg.nextCsgStairId = data.nextCsgStairId || (Math.max(...state.csg.csgStairs.map(s => s.id), 0) + 1);
    state.csg.selectedFace = null;
    state.csg.selectedFaces = [];
    state.csg.activeBrush = null;
    state.csg.activeOp = null;
    state.csg.activeSide = null;
    state.csg.activeStairOp = null;
    state.csg.pendingStairOp = null;
    state.nextPlatformId = data.nextPlatformId || (Math.max(...state.platforms.map(p => p.id), 0) + 1);
    state.nextStairRunId = data.nextStairRunId || (Math.max(...state.stairRuns.map(r => r.id), 0) + 1);
    state.nextTerrainMapId = data.nextTerrainMapId || (Math.max(...state.terrainMaps.map(t => t.id), 0) + 1);
    state.nextPointLightId = data.nextPointLightId || (Math.max(...state.pointLights.map(l => l.id), 0) + 1);
    state.selectedPlatformId = null;
    state.selectedStairRunId = null;
    state.selectedTerrainId = null;
    state.selectedLightId = null;
    state.undoStack = [];
}
