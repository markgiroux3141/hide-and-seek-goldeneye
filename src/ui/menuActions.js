// Menu action dispatcher — maps action IDs to editor operations
// Listens on EventBus for 'menu:action' events dispatched by RadialMenu

import { on } from '../systems/EventBus.js';
import { state } from '../state.js';
import { TEXTURE_SCHEMES } from '../scene/textureSchemes.js';
import { gridHelper } from '../scene/setup.js';
import { setTool } from '../tools/ToolManager.js';
import { setHoleMode, setFacePaintMode, startCaveFromFace } from '../csg/csgActions.js';
import { PLATFORM_STYLES } from '../geometry/platformStyles.js';
import { exportSceneToGLB } from '../io/GLBExporter.js';
import { bakeLevel } from '../io/bakeLevel.js';

// Callbacks set during init to avoid circular imports with main.js
let callbacks = {};

export function initMenuActions(cbs) {
    callbacks = cbs;

    on('menu:action', ({ actionId }) => {
        if (actionId.startsWith('tool:')) {
            handleToolAction(actionId.slice(5));
        } else if (actionId.startsWith('texture:')) {
            handleTextureAction(actionId.slice(8));
        } else if (actionId.startsWith('view:')) {
            handleViewAction(actionId.slice(5));
        } else if (actionId.startsWith('platform_style:')) {
            handlePlatformStyleAction(actionId.slice(15));
        } else if (actionId === 'file:export_glb') {
            exportSceneToGLB();
            callbacks.showMessage('Exported level.glb');
        } else if (actionId === 'file:bake_glb') {
            bakeLevel();
        } else if (actionId === 'brush:start_cave_from_face') {
            const r = startCaveFromFace();
            if (r.ok) {
                callbacks.showMessage(`Cave ${r.cave.id} started — press K to sculpt`);
            } else if (r.reason === 'no_selection' || r.reason === 'no_brush') {
                callbacks.showMessage('Select a face first to start a cave from it');
            } else if (r.reason === 'not_room_brush') {
                callbacks.showMessage('Caves start from room (subtract) brush faces');
            } else {
                callbacks.showMessage('Cannot start cave here');
            }
        }
    });
}

function handlePlatformStyleAction(styleName) {
    const style = PLATFORM_STYLES[styleName];
    if (!style) return;
    state.platformStyle = styleName;
    callbacks.showMessage('Platform style: ' + style.label);
}

function handleToolAction(toolName) {
    if (toolName === 'csg') {
        setTool('csg');
    } else if (toolName === 'hole') {
        setTool('csg');
        setHoleMode(true, false);
        callbacks.showMessage('HOLE mode — click any face');
    } else if (toolName === 'door') {
        setTool('csg');
        setHoleMode(true, true);
        callbacks.showMessage('DOOR mode — click a wall');
    } else if (toolName === 'platform') {
        setTool('platform');
    } else if (toolName === 'simple_stairs') {
        setTool('platform');
        state.platformPhase = 'simple_stair_from';
        state.simpleStairFrom = null;
        callbacks.showMessage('Click first stair endpoint — Esc to cancel');
    } else if (toolName === 'light') {
        setTool('light');
    } else if (toolName === 'face_paint') {
        setTool('csg');
        setFacePaintMode(true);
        callbacks.showMessage('FACE PAINT mode — click a face, press 1-9 to override scheme (0 to clear)');
    }
}

function handleTextureAction(schemeName) {
    if (!TEXTURE_SCHEMES[schemeName]) return;
    if (!state.csg.selectedFace) {
        callbacks.showMessage('Select a face first to apply texture');
        return;
    }
    // Defer to csgActions.retextureRoom — flood-fills the room and rebuilds.
    import('../csg/csgActions.js').then(mod => {
        mod.retextureRoom(schemeName);
        callbacks.showMessage('Scheme: ' + TEXTURE_SCHEMES[schemeName].label);
    });
}

function handleViewAction(mode) {
    if (mode === 'toggle_grid') {
        state.showGrid = !state.showGrid;
        if (gridHelper) gridHelper.visible = state.showGrid;
        callbacks.showMessage('Grid: ' + (state.showGrid ? 'ON' : 'OFF'));
        return;
    }
    if (mode !== 'grid' && mode !== 'textured') return;
    state.viewMode = mode;
    callbacks.showMessage('View: ' + (mode === 'grid' ? 'Grid' : 'Textured'));
    if (callbacks.rebuildAll) callbacks.rebuildAll();
}
