// Indoor mode key handler

import { state, saveUndoState } from '../state.js';
import { isPointerLocked } from '../input/input.js';
import { showMessage } from '../hud/hud.js';
import { scene, gridHelper } from '../scene/setup.js';
import { TEXTURE_SCHEMES, getSchemeByKey } from '../scene/textureSchemes.js';
import { hotkeyManager } from '../input/HotkeyManager.js';
import {
    undoAction,
    saveLevel, loadLevel,
} from '../actions.js';
import { exportSceneToGLB } from '../io/GLBExporter.js';
import { bakeLevel } from '../io/bakeLevel.js';
import {
    deleteBakedHighlight, clearBakedHighlight, undoBakedDelete,
    toggleBakedPrisms, runBakedPrismCleanup,
} from './indoorClick.js';
import * as csgActions from '../csg/csgActions.js';
import * as caveSculpt from './caveSculpt.js';
import { csgRegionMeshes } from '../mesh/csgMesh.js';
import { rebuildCsgStair } from '../mesh/csgStairMesh.js';
import {
    stairRunMeshes,
    rebuildPlatform, rebuildStairRun, rebuildConnectedStairRuns,
    rebuildAllPlatforms, rebuildAllStairRuns,
    rebuildAll, removePlatformMesh,
    rebuildLight, removeLightMesh, updateLightSelection,
    setAllWireframeVisible,
    rebuildAllCSG,
} from '../mesh/MeshManager.js';
import { toggleEditorMode, setTool, clearPlatformToolState, clearLightToolState } from './ToolManager.js';
import { PointLight } from '../core/PointLight.js';
import { WORLD_SCALE } from '../core/constants.js';

export function handleIndoorKey(e, { gizmo, camera }) {
    // ─── Cave sculpt mode consumes all keys while active ────────────
    if (caveSculpt.isSculpting()) {
        if (hotkeyManager.matches('escape', e)) {
            e.preventDefault();
            // Esc cancels the place-exit submode first, if active.
            if (caveSculpt.isPlacingExit()) {
                caveSculpt.togglePlaceExitMode();
                showMessage('Exit room placement cancelled');
                return;
            }
            caveSculpt.exitSculptMode();
            showMessage('Sculpt mode exited');
            return;
        }
        if (e.code === 'KeyK') {
            e.preventDefault();
            caveSculpt.exitSculptMode();
            showMessage('Sculpt mode exited');
            return;
        }
        if (e.code === 'KeyP') {
            e.preventDefault();
            const on = caveSculpt.togglePlaceExitMode();
            showMessage(on
                ? 'Place Exit Room: aim at cave wall, LMB to place (Esc to cancel)'
                : 'Exit room placement cancelled');
            return;
        }
        if (e.code === 'KeyF') {
            e.preventDefault();
            caveSculpt.toggleMode('flatten');
            showMessage(`Sculpt: ${caveSculpt.getSculptState().mode}`);
            return;
        }
        if (e.code === 'KeyR') {
            e.preventDefault();
            caveSculpt.toggleMode('smooth');
            showMessage(`Sculpt: ${caveSculpt.getSculptState().mode}`);
            return;
        }
        if (e.code === 'KeyE') {
            e.preventDefault();
            caveSculpt.toggleMode('expand');
            showMessage(`Sculpt: ${caveSculpt.getSculptState().mode}`);
            return;
        }
        if (e.code === 'BracketLeft') {
            e.preventDefault();
            caveSculpt.adjustStrength(-0.1);
            return;
        }
        if (e.code === 'BracketRight') {
            e.preventDefault();
            caveSculpt.adjustStrength(0.1);
            return;
        }
        if (e.code === 'KeyG') {
            e.preventDefault();
            const on = caveSculpt.toggleGizmoVisible();
            showMessage(`Brush gizmo: ${on ? 'ON' : 'OFF'}`);
            return;
        }
        if (e.code === 'KeyH') {
            e.preventDefault();
            const on = caveSculpt.toggleCsgVisible();
            showMessage(`CSG mesh: ${on ? 'SHOWN (stale until exit)' : 'HIDDEN'}`);
            return;
        }
        return;  // swallow every other key so CSG handlers don't fire
    }

    // ─── Post-bake cleanup tool ────────────────────────────────────
    // Delete (or 'delete' hotkey) removes the currently highlighted triangle;
    // Esc clears the highlight; Ctrl+Z undoes the last triangle deletion.
    // B toggles the cleanup-prism wireframes; C runs auto-cleanup (collapses
    // every baked triangle whose centroid sits inside any prism, batched as
    // one undo entry). Other actions (Ctrl+E export, Ctrl+S save, Ctrl+O
    // load) fall through to their handlers below.
    if (state.isBaked) {
        if (e.code === 'Delete' || hotkeyManager.matches('delete', e)) {
            e.preventDefault();
            if (deleteBakedHighlight()) showMessage('Deleted triangle');
            return;
        }
        if (hotkeyManager.matches('undo', e)) {
            e.preventDefault();
            showMessage(undoBakedDelete() ? 'Restored triangle' : 'Nothing to undo');
            return;
        }
        if (hotkeyManager.matches('escape', e)) {
            e.preventDefault();
            clearBakedHighlight();
            return;
        }
        if (e.code === 'KeyB' && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            const visible = toggleBakedPrisms();
            showMessage(visible ? 'Cleanup prisms shown' : 'Cleanup prisms hidden');
            return;
        }
        if (e.code === 'KeyC' && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            const n = runBakedPrismCleanup();
            showMessage(n > 0 ? `Cleaned ${n} triangles (Ctrl+Z to undo)` : 'No triangles inside cleanup prisms');
            return;
        }
        // Don't return — let Ctrl+E / save / load reach their handlers below.
    }

    // K = enter sculpt mode on a cave in the selected region. If the selected
    // face matches one of a cave's anchor faces (source room's wall or an
    // exit room's cave-side wall), sculpt that cave; otherwise sculpt the
    // first cave in the region.
    if (e.code === 'KeyK' && isPointerLocked() && state.tool === 'csg') {
        const sel = state.csg.selectedFace;
        if (sel) {
            const regionData = csgRegionMeshes.get(sel.regionId);
            const region = regionData ? regionData.region : null;
            if (region && region.caves.length > 0) {
                let cave = null;
                for (const c of region.caves) {
                    if ((c.anchorFaces || []).some(f =>
                        f.brushId === sel.brushId && f.axis === sel.axis &&
                        f.side === sel.side && f.position === sel.position
                    )) { cave = c; break; }
                }
                if (!cave) cave = region.caves[0];
                e.preventDefault();
                if (caveSculpt.enterSculptMode(sel.regionId, cave.id)) {
                    showMessage('Sculpt mode — LMB carve, Shift+LMB add, F/R/E modes, Esc exit');
                } else {
                    showMessage('Could not enter sculpt mode');
                }
                return;
            }
        }
    }

    // ─── Tool/mode entry hotkeys (Numpad 1-6) ───────────────────────
    // These fire from any current tool, so users can jump directly to any
    // mode without cycling. Each switches state.tool (and any sub-mode flags).
    if (isPointerLocked()) {
        if (hotkeyManager.matches('tool_csg', e)) {
            e.preventDefault();
            // Also drop CSG sub-modes so Numpad 1 reliably returns to plain
            // push/pull selection even when the user is already inside the CSG
            // tool (e.g. coming from Face Paint, Hole, Brace, Pillar).
            if (state.tool === 'csg') {
                csgActions.exitFacePaintMode();
                csgActions.exitHoleMode();
                csgActions.exitBraceMode();
                csgActions.exitPillarMode();
                state.csg.selectedFace = null;
                state.csg.selectedFaces = [];
            }
            setTool('csg');
            return;
        }
        if (hotkeyManager.matches('tool_hole', e)) {
            e.preventDefault();
            setTool('csg');
            csgActions.setHoleMode(true, false);
            showMessage('HOLE mode — click any face');
            return;
        }
        if (hotkeyManager.matches('tool_door', e)) {
            e.preventDefault();
            setTool('csg');
            csgActions.setHoleMode(true, true);
            showMessage('DOOR mode — click a wall');
            return;
        }
        if (hotkeyManager.matches('tool_platform', e)) {
            e.preventDefault();
            setTool('platform');
            return;
        }
        if (hotkeyManager.matches('tool_simple_stairs', e)) {
            e.preventDefault();
            setTool('platform');
            state.platformPhase = 'simple_stair_from';
            state.simpleStairFrom = null;
            showMessage('Click first stair endpoint — Esc to cancel');
            return;
        }
        if (hotkeyManager.matches('tool_light', e)) {
            e.preventDefault();
            setTool('light');
            return;
        }
        if (hotkeyManager.matches('tool_brace', e)) {
            e.preventDefault();
            setTool('csg');
            csgActions.setBraceMode(true);
            showMessage('BRACE mode — aim at a wall, click to place arch');
            return;
        }
        if (hotkeyManager.matches('tool_pillar', e)) {
            e.preventDefault();
            setTool('csg');
            csgActions.setPillarMode(true);
            showMessage('PILLAR mode — aim at floor, scroll to size, click to place');
            return;
        }
        if (hotkeyManager.matches('tool_face_paint', e)) {
            e.preventDefault();
            setTool('csg');
            csgActions.setFacePaintMode(true);
            showMessage('FACE PAINT mode \u2014 click: face, 1-9: scheme, \u2191\u2193: fix triangle zone, 0: clear');
            return;
        }
    }

    // M key to switch to terrain
    if (hotkeyManager.matches('toggle_mode', e) && isPointerLocked()) {
        e.preventDefault();
        toggleEditorMode();
        return;
    }

    // L key — place a light at the camera position from any tool, switch
    // to light tool, and select the new light so the gizmo attaches.
    if (hotkeyManager.matches('place_light_at_camera', e) && isPointerLocked()) {
        e.preventDefault();
        saveUndoState();
        const S = WORLD_SCALE;
        const light = new PointLight(
            state.nextPointLightId++,
            Math.round(camera.position.x / S),
            Math.round(camera.position.y / S),
            Math.round(camera.position.z / S),
        );
        state.pointLights.push(light);
        rebuildLight(light);
        setTool('light');
        state.selectedLightId = light.id;
        state.lightPhase = 'selected';
        updateLightSelection();
        showMessage(`Placed light ${light.id} at camera`);
        return;
    }

    // ─── CSG tool keys ──────────────────────────────────────────────
    if (state.tool === 'csg' && isPointerLocked() && !state.isBaked) {
        // Push/pull (also handles extrude continuation when activeOp === 'extrude').
        // Shift = fine 1-unit step instead of the default 4.
        if (hotkeyManager.matches('push', e)) {
            e.preventDefault();
            const step = e.shiftKey ? 1 : undefined;
            if (!csgActions.growActiveExtrude()) {
                saveUndoState();
                csgActions.pushSelectedFace(step);
            }
            return;
        }
        if (hotkeyManager.matches('pull', e)) {
            e.preventDefault();
            saveUndoState();
            const step = e.shiftKey ? 1 : undefined;
            csgActions.pullSelectedFace(step);
            return;
        }
        // In Face Paint mode, arrow up/down cycles the clicked triangle's zone
        // (floor/ceiling/wall-lower/wall-upper/…) within its scheme. This
        // corrects individual triangles that CSG mis-classified.
        if (state.csg.facePaintMode && (hotkeyManager.matches('stair_up', e) || hotkeyManager.matches('stair_down', e))) {
            e.preventDefault();
            if (!state.csg.selectedFace || state.csg.selectedFace.triIndex == null) {
                showMessage('Face paint: click a triangle first');
                return;
            }
            saveUndoState();
            const direction = hotkeyManager.matches('stair_up', e) ? 1 : -1;
            const newZone = csgActions.cycleTriangleZone(direction);
            if (newZone != null) {
                const zoneNames = { 0: 'floor', 1: 'ceiling', 2: 'wall (lower)', 3: 'wall (upper)', 5: 'tunnel side', 6: 'tunnel floor', 7: 'brace' };
                showMessage(`Triangle zone: ${zoneNames[newZone] || newZone}`);
            } else {
                showMessage('Cannot change zone on this triangle');
            }
            return;
        }
        // Arrow keys: adjust pending stair counter (no CSG rebuild yet)
        if (hotkeyManager.matches('stair_down', e)) {
            e.preventDefault();
            csgActions.pushSelectedFaceAsStairs('down');
            if (state.csg.pendingStairOp) {
                const op = state.csg.pendingStairOp;
                showMessage(`Stairs: ${op.stepCount} step${op.stepCount > 1 ? 's' : ''} ${op.direction} \u2014 Enter to confirm, Esc to cancel`);
            } else {
                const sel = state.csg.selectedFace;
                if (sel && sel.axis !== 'y') showMessage('Stairs need the selection to touch the floor');
            }
            return;
        }
        if (hotkeyManager.matches('stair_up', e)) {
            e.preventDefault();
            csgActions.pushSelectedFaceAsStairs('up');
            if (state.csg.pendingStairOp) {
                const op = state.csg.pendingStairOp;
                showMessage(`Stairs: ${op.stepCount} step${op.stepCount > 1 ? 's' : ''} ${op.direction} \u2014 Enter to confirm, Esc to cancel`);
            } else {
                const sel = state.csg.selectedFace;
                if (sel && sel.axis !== 'y') showMessage('Stairs need the selection to touch the floor');
            }
            return;
        }
        // Enter: confirm pending stair op
        if (e.code === 'Enter' && state.csg.pendingStairOp) {
            e.preventDefault();
            saveUndoState();
            const desc = csgActions.confirmStairOp();
            if (desc) {
                rebuildCsgStair(desc);
                showMessage(`Stairs confirmed: ${desc.stepCount} steps ${desc.direction}`);
            }
            return;
        }
        // E = extrude selected face (Shift+E = 1-unit depth)
        if (e.code === 'KeyE') {
            e.preventDefault();
            saveUndoState();
            const step = e.shiftKey ? 1 : undefined;
            csgActions.extrudeSelectedFace(step);
            return;
        }
        // B = bake current region
        if (e.code === 'KeyB') {
            e.preventDefault();
            saveUndoState();
            csgActions.bakeCurrentRegion();
            showMessage('Baked');
            return;
        }
        // J = start a cave from the selected face (punches a mouth + seeds a
        // proto half-sphere of voxel air outside the wall).
        if (hotkeyManager.matches('start_cave_from_face', e)) {
            e.preventDefault();
            saveUndoState();
            const r = csgActions.startCaveFromFace();
            if (r.ok) {
                showMessage(`Cave ${r.cave.id} started \u2014 press K to sculpt`);
            } else if (r.reason === 'no_selection' || r.reason === 'no_brush') {
                showMessage('Select a face first to start a cave from it');
            } else if (r.reason === 'not_room_brush') {
                showMessage('Caves start from room (subtract) brush faces');
            } else if (r.reason === 'baked_face') {
                showMessage('Cannot start a cave on a baked face');
            } else {
                showMessage('Cannot start cave here');
            }
            return;
        }
        // [ / ] = scale (taper) selected face
        if (e.code === 'BracketLeft') {
            e.preventDefault();
            saveUndoState();
            if (e.shiftKey) csgActions.scaleSelectedFace(1, 0);
            else if (e.ctrlKey) csgActions.scaleSelectedFace(0, 1);
            else csgActions.scaleSelectedFace(1, 1);
            return;
        }
        if (e.code === 'BracketRight') {
            e.preventDefault();
            saveUndoState();
            if (e.shiftKey) csgActions.scaleSelectedFace(-1, 0);
            else if (e.ctrlKey) csgActions.scaleSelectedFace(0, -1);
            else csgActions.scaleSelectedFace(-1, -1);
            return;
        }
        // Main-row digit keys (Digit1..Digit9): retexture room, or in Face Paint
        // mode, apply a per-face scheme override. Digit0 clears an override.
        // Use e.code, NOT e.key, so numpad numbers (Numpad1..Numpad6 — used
        // for tool switching above) don't trigger retexture when NumLock is on.
        if (state.csg.facePaintMode && e.code === 'Digit0' && state.csg.selectedFace) {
            e.preventDefault();
            saveUndoState();
            // Prefer clearing a per-triangle zone override if one exists at the
            // picked triangle; otherwise clear the whole-face scheme override.
            if (csgActions.clearTriangleZoneOverride()) {
                showMessage('Triangle zone override cleared');
            } else if (csgActions.clearFaceSchemeOverride()) {
                showMessage('Face override cleared');
            } else {
                showMessage('No override on this face/triangle');
            }
            return;
        }
        if (e.code >= 'Digit1' && e.code <= 'Digit9') {
            const digit = e.code.slice(5); // 'Digit1' → '1'
            const schemeName = getSchemeByKey(digit);
            if (schemeName && state.csg.selectedFace) {
                e.preventDefault();
                saveUndoState();
                if (state.csg.facePaintMode) {
                    if (csgActions.applyFaceSchemeOverride(schemeName)) {
                        showMessage('Face override: ' + (TEXTURE_SCHEMES[schemeName]?.label || schemeName));
                    } else {
                        showMessage('Cannot override baked face — un-bake the region first');
                    }
                } else {
                    csgActions.retextureRoom(schemeName);
                    showMessage('Scheme: ' + (TEXTURE_SCHEMES[schemeName]?.label || schemeName));
                }
                return;
            }
        }
        // Delete = remove selected brush
        if ((hotkeyManager.matches('delete', e) || e.key === 'Delete') && state.csg.selectedFace) {
            e.preventDefault();
            saveUndoState();
            csgActions.deleteSelectedBrush();
            return;
        }
        // Escape = cancel pending stair / hole / brace mode or deselect
        if (hotkeyManager.matches('escape', e)) {
            e.preventDefault();
            if (state.csg.pendingStairOp) {
                csgActions.cancelStairOp();
                showMessage('Stair cancelled');
            } else if (state.csg.holeMode) {
                csgActions.exitHoleMode();
                showMessage('Hole mode cancelled');
            } else if (state.csg.braceMode) {
                csgActions.exitBraceMode();
                showMessage('Brace mode cancelled');
            } else if (state.csg.pillarMode) {
                csgActions.exitPillarMode();
                showMessage('Pillar mode cancelled');
            } else if (state.csg.facePaintMode) {
                csgActions.exitFacePaintMode();
                showMessage('Face Paint mode cancelled');
            } else {
                state.csg.selectedFace = null;
                state.csg.selectedFaces = [];
                state.csg.activeBrush = null;
                state.csg.activeOp = null;
                state.csg.activeSide = null;
                state.csg.activeStairOp = null;
            }
            return;
        }
        // Fall through to global keys (undo, save, load, view toggles)
    }

    // Platform tool keys
    if (state.tool === 'platform' && isPointerLocked()) {
        const selectedPlat = state.selectedPlatformId != null
            ? state.platforms.find(p => p.id === state.selectedPlatformId)
            : null;

        // Escape = cancel gizmo drag, cancel connect, or deselect
        if (hotkeyManager.matches('escape', e)) {
            e.preventDefault();
            if (gizmo.isDragging()) {
                gizmo.cancelDrag();
                rebuildPlatform(selectedPlat);
                rebuildConnectedStairRuns(selectedPlat.id);
                showMessage('Cancelled');
            } else if (state.platformPhase === 'simple_stair_from' || state.platformPhase === 'simple_stair_to') {
                state.platformPhase = 'idle';
                state.simpleStairFrom = null;
                showMessage('Cancelled');
            } else if (state.platformPhase === 'connecting_src' || state.platformPhase === 'connecting_dst') {
                state.platformPhase = 'selected';
                state.platformConnectFrom = null;
                state.platformConnectTo = null;
                showMessage('Cancelled');
            } else {
                clearPlatformToolState();
                gizmo.update(null, camera);
                showMessage('Platform deselected');
            }
            return;
        }

        // Connect mode
        if (hotkeyManager.matches('connect_stairs', e) && selectedPlat && state.platformPhase === 'selected') {
            e.preventDefault();
            state.platformConnectFrom = { platformId: selectedPlat.id, edge: null, offset: 0.5 };
            state.platformConnectTo = null;
            state.platformPhase = 'connecting_dst';
            showMessage(`Click destination platform or floor — Esc to cancel`);
            return;
        }

        // Simple stair mode
        if (hotkeyManager.matches('simple_stairs', e) && state.platformPhase === 'idle') {
            e.preventDefault();
            state.platformPhase = 'simple_stair_from';
            state.simpleStairFrom = null;
            showMessage('Click first stair endpoint — Esc to cancel');
            return;
        }

        // Toggle grounded on platform + connected stairs
        if (hotkeyManager.matches('toggle_grounded', e) && selectedPlat && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            const newGrounded = !selectedPlat.grounded;
            selectedPlat.grounded = newGrounded;
            rebuildPlatform(selectedPlat);
            const connectedRuns = state.stairRuns.filter(
                r => r.fromPlatformId === selectedPlat.id || r.toPlatformId === selectedPlat.id
            );
            for (const run of connectedRuns) {
                run.grounded = newGrounded;
                rebuildStairRun(run);
            }
            const count = connectedRuns.length;
            const label = newGrounded ? 'grounded' : 'floating';
            showMessage(count > 0
                ? `Platform + ${count} stair run${count > 1 ? 's' : ''} ${label}`
                : `Platform ${label}`);
            return;
        }

        // Toggle railings on platform + connected stairs
        if (hotkeyManager.matches('toggle_railings', e) && selectedPlat && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            const newRailings = !selectedPlat.railings;
            selectedPlat.railings = newRailings;
            rebuildPlatform(selectedPlat);
            const connectedRuns = state.stairRuns.filter(
                r => r.fromPlatformId === selectedPlat.id || r.toPlatformId === selectedPlat.id
            );
            for (const run of connectedRuns) {
                run.railings = newRailings;
                rebuildStairRun(run);
            }
            const count = connectedRuns.length;
            const label = newRailings ? 'ON' : 'OFF';
            showMessage(count > 0
                ? `Railings ${label} (platform + ${count} stair run${count > 1 ? 's' : ''})`
                : `Railings ${label}`);
            return;
        }

        // Delete selected platform
        if ((hotkeyManager.matches('delete', e) || e.key === 'Delete') && selectedPlat && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            const connectedRuns = state.stairRuns.filter(r => r.fromPlatformId === selectedPlat.id || r.toPlatformId === selectedPlat.id);
            for (const run of connectedRuns) {
                const mesh = stairRunMeshes.get(run.id);
                if (mesh) { scene.remove(mesh); mesh.geometry.dispose(); stairRunMeshes.delete(run.id); }
            }
            state.stairRuns = state.stairRuns.filter(r => r.fromPlatformId !== selectedPlat.id && r.toPlatformId !== selectedPlat.id);
            state.platforms = state.platforms.filter(p => p.id !== selectedPlat.id);
            removePlatformMesh(selectedPlat.id);
            clearPlatformToolState();
            showMessage('Platform deleted');
            return;
        }

        // --- Selected stair run keys (F/R/X) ---
        const selectedRun = state.selectedStairRunId != null
            ? state.stairRuns.find(r => r.id === state.selectedStairRunId)
            : null;

        if (hotkeyManager.matches('toggle_grounded', e) && selectedRun && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            selectedRun.grounded = !selectedRun.grounded;
            rebuildStairRun(selectedRun);
            showMessage(`Stair run ${selectedRun.grounded ? 'grounded' : 'floating'}`);
            return;
        }

        if (hotkeyManager.matches('toggle_railings', e) && selectedRun && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            selectedRun.railings = !selectedRun.railings;
            rebuildStairRun(selectedRun);
            showMessage(`Stair run railings ${selectedRun.railings ? 'ON' : 'OFF'}`);
            return;
        }

        if ((hotkeyManager.matches('delete', e) || e.key === 'Delete') && selectedRun && state.platformPhase === 'selected') {
            e.preventDefault();
            saveUndoState();
            const mesh = stairRunMeshes.get(selectedRun.id);
            if (mesh) { scene.remove(mesh); mesh.geometry.dispose(); stairRunMeshes.delete(selectedRun.id); }
            state.stairRuns = state.stairRuns.filter(r => r.id !== selectedRun.id);
            clearPlatformToolState();
            showMessage('Stair run deleted');
            return;
        }
    }

    // Light tool keys
    if (state.tool === 'light' && isPointerLocked()) {
        const selectedLight = state.selectedLightId != null
            ? state.pointLights.find(l => l.id === state.selectedLightId)
            : null;

        // Escape = cancel gizmo drag or deselect
        if (hotkeyManager.matches('escape', e)) {
            e.preventDefault();
            if (gizmo.isDragging()) {
                gizmo.cancelDrag();
                if (selectedLight) rebuildLight(selectedLight);
                showMessage('Cancelled');
            } else {
                clearLightToolState();
                updateLightSelection();
                showMessage('Light deselected');
            }
            return;
        }

        // Delete selected light
        if ((hotkeyManager.matches('delete', e) || e.key === 'Delete') && selectedLight) {
            e.preventDefault();
            saveUndoState();
            state.pointLights = state.pointLights.filter(l => l.id !== selectedLight.id);
            removeLightMesh(selectedLight.id);
            clearLightToolState();
            updateLightSelection();
            showMessage('Light deleted');
            return;
        }
    }

    if (hotkeyManager.matches('toggle_view', e) && isPointerLocked()) {
        e.preventDefault();
        state.viewMode = state.viewMode === 'grid' ? 'textured' : 'grid';
        showMessage('View: ' + (state.viewMode === 'grid' ? 'Grid' : 'Textured'));
        rebuildAllCSG();
        rebuildAllPlatforms();
        rebuildAllStairRuns();
        return;
    }

    if (hotkeyManager.matches('toggle_grid', e)) {
        e.preventDefault();
        state.showGrid = !state.showGrid;
        if (gridHelper) gridHelper.visible = state.showGrid;
        showMessage('Grid: ' + (state.showGrid ? 'ON' : 'OFF'));
        return;
    }

    // Wireframe toggle (was E in legacy code; CSG tool consumed E for extrude above).
    // Use Backslash so it doesn't conflict with CSG extrude.
    if (e.code === 'Backslash' && isPointerLocked()) {
        e.preventDefault();
        state.showWireframe = !state.showWireframe;
        setAllWireframeVisible(state.showWireframe);
        showMessage('Wireframe: ' + (state.showWireframe ? 'ON' : 'OFF'));
        return;
    }

    if (hotkeyManager.matches('undo', e)) {
        e.preventDefault();
        clearPlatformToolState();
        undoAction(showMessage, rebuildAll);
        return;
    }

    if (hotkeyManager.matches('save', e)) {
        e.preventDefault();
        saveLevel(showMessage);
        return;
    }

    if (hotkeyManager.matches('load', e)) {
        e.preventDefault();
        loadLevel(showMessage, rebuildAll);
        return;
    }

    if (hotkeyManager.matches('bake_glb', e)) {
        e.preventDefault();
        bakeLevel();
        return;
    }

    if (hotkeyManager.matches('export_glb', e)) {
        e.preventDefault();
        exportSceneToGLB();
        showMessage('Exported level.glb');
        return;
    }
}
