// Editor actions: undo, save/load, grid snap.
//
// Push/pull/door/extrude actions live in src/csg/csgActions.js — this file is
// just the small set of operations that survived the Phase 6 deletion of the
// legacy Volume/Connection system.

import { WORLD_SCALE } from './core/constants.js';
import { undo, serializeLevel, deserializeLevel } from './state.js';
import { saveToLocalStorage } from './io/LevelStorage.js';
import { downloadJson, uploadJson } from './io/LevelFileIO.js';

// ============================================================
// UNDO
// ============================================================
export function undoAction(showMessage, rebuildAllCallback) {
    if (undo()) {
        rebuildAllCallback();
        showMessage('Undo');
    } else {
        showMessage('Nothing to undo');
    }
}

// ============================================================
// SAVE / LOAD
// ============================================================
export function saveLevel(showMessage) {
    const json = serializeLevel();
    saveToLocalStorage(json);
    downloadJson(json);
    showMessage('Level saved');
}

export function loadLevel(showMessage, rebuildAllCallback) {
    uploadJson().then((json) => {
        try {
            deserializeLevel(json);
            rebuildAllCallback();
            showMessage('Level loaded');
        } catch (err) { showMessage('Error loading level: ' + err.message); }
    }).catch(() => { /* user cancelled */ });
}

// ============================================================
// GRID SNAP
// ============================================================

/** Snap a hit point (world coords) to WT grid coordinates. */
export function snapToWTGrid(hitPoint) {
    return {
        x: Math.round(hitPoint.x / WORLD_SCALE),
        y: Math.round(hitPoint.y / WORLD_SCALE),
        z: Math.round(hitPoint.z / WORLD_SCALE),
    };
}
