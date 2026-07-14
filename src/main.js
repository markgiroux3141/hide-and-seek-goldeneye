// Main entry point — wires all modules together

import * as THREE from 'three';
import { initScene, scene, renderer, camera, setAmbientIntensity, gridHelper } from './scene/setup.js';
import { initInput, initKeyActions, onKeyDown, isKeyDown, isPointerLocked, consumeMouseDelta, onMiddleClick, reacquirePointerLock, releasePointerLock } from './input/input.js';
import { updateCamera } from './scene/camera.js';
import { WORLD_SCALE } from './core/constants.js';
import { state, deserializeLevel } from './state.js';
import { initMaterials, loadBmpTextures, clearCSGMaterialCache } from './scene/materials.js';
import { loadTextureSchemes } from './scene/textureSchemes.js';
import { showMessage, updateHUD, initHUD } from './hud/hud.js';
import { initSidebar } from './ui/Sidebar.js';
import { loadFromLocalStorage } from './io/LevelStorage.js';
import { PlatformGizmo } from './gizmo.js';
import { FOG_NEAR, FOG_FAR, INDOOR_BG_COLOR, TERRAIN_BG_COLOR, TERRAIN_PERSPECTIVE_BG } from './core/constants.js';
import { createOrthoCamera, updateOrthoCamera, handleOrthoResize } from './terrain/orthographicCamera.js';
import { updateTerrainNormals } from './geometry/terrainGeometry.js';
import { applyBrush } from './terrain/terrainBrush.js';
import { RadialMenu } from './ui/RadialMenu.js';
import { buildMenuTree } from './ui/menuConfig.js';
import { initMenuActions } from './ui/menuActions.js';
import { terrainMeshes, rebuildPlatform, rebuildConnectedStairRuns, rebuildTerrainWalls, generateTerrainMesh, rebuildAll, rebuildLight, updateLightShadowFlag, rebuildAllCSG } from './mesh/MeshManager.js';
import { BrushDef } from './core/BrushDef.js';
import { updatePlatformPreview } from './preview/platformPreview.js';
import { updateTerrainPreview } from './preview/terrainPreview.js';
import { updateCSGSelectionPreview, updateCSGMultiSelectionPreview, updateCSGHolePreview, updateCSGBracePreview, updateCSGPillarPreview, updateCSGStairPreview } from './preview/csgPreviews.js';
import * as csgActions from './csg/csgActions.js';
import { updateTerrainHUD } from './hud/terrainHud.js';
import { initToolManager, toggleEditorMode, getActiveTerrain } from './tools/ToolManager.js';
import { handleIndoorClick } from './tools/indoorClick.js';
import { handleTerrainClick, handleTerrainMouseUp, handleTerrainMouseMove, handleTerrainWheel, getIsSculpting } from './tools/terrainClick.js';
import { handleIndoorKey } from './tools/indoorKeys.js';
import { handleTerrainKey } from './tools/terrainKeys.js';
import { initCSGWasm } from './core/csg/wasmCSG.js';
import { initCaveWasm } from './core/cave/wasmCave.js';
import * as caveSculpt from './tools/caveSculpt.js';
import { initGame, isHuntActive, updateGame, startHunt, getPhase, handleHuntKey } from './game/game.js';

// ============================================================
// INIT
// ============================================================
const __bootT = { start: performance.now() };
const __mark = (label) => { __bootT[label] = performance.now(); };

initScene();
if (gridHelper) gridHelper.visible = state.showGrid;   // start with grid off (see state.js)
initInput(renderer.domElement);
initKeyActions();
initHUD();
initSidebar();
__mark('sceneInputHud');

// Platform gizmo (move arrows + scale handles)
const gizmo = new PlatformGizmo(scene);

initToolManager(gizmo, camera);
initGame(camera, scene);
__mark('gizmoToolMgr');

// Radial menu setup
const radialMenu = new RadialMenu();

onMiddleClick(() => {
    // Don't open menu in terrain mode (middle-click is pan there)
    if (state.editorMode === 'terrain') return;
    if (radialMenu.isOpen()) {
        radialMenu.close();
        return;
    }
    if (!isPointerLocked()) return;

    state.radialMenuOpen = true;
    releasePointerLock();
    const tree = buildMenuTree();
    radialMenu.open(tree, () => {
        state.radialMenuOpen = false;
        reacquirePointerLock();
    }, buildMenuTree);
});

// Create orthographic camera for terrain mode
const orthoCamera = createOrthoCamera();

// ============================================================
// MOUSE CLICK — delegated to tool handlers
// ============================================================
document.addEventListener('mousedown', (e) => {
    // During the hunt the level is frozen — no editor selection/clicks.
    if (isHuntActive()) return;
    if (state.editorMode === 'terrain') {
        handleTerrainClick(e, generateTerrainMesh);
        return;
    }
    // Sculpt mode eats LMB for brush application — skip normal click handling.
    if (caveSculpt.isSculpting() && e.button === 0) return;
    handleIndoorClick(e, { gizmo, camera });
});

// Terrain mouse handlers (mouseup, mousemove, wheel)
document.addEventListener('mouseup', (e) => {
    if (state.editorMode === 'terrain') handleTerrainMouseUp(e);
});
document.addEventListener('mousemove', (e) => {
    if (state.editorMode === 'terrain') handleTerrainMouseMove(e);
});
document.addEventListener('wheel', (e) => {
    if (state.editorMode === 'terrain') { handleTerrainWheel(e); return; }
    if (!isPointerLocked()) return;

    const delta = e.deltaY > 0 ? -1 : 1;

    // Sculpt P-submode: scroll resizes the exit-room preview. Default = depth
    // (into rock); Shift = width (face u-axis); Ctrl = height (face v-axis).
    if (caveSculpt.isPlacingExit()) {
        e.preventDefault();
        if (e.ctrlKey)       caveSculpt.adjustExitRoomSize('height', delta);
        else if (e.shiftKey) caveSculpt.adjustExitRoomSize('width', delta);
        else                 caveSculpt.adjustExitRoomSize('depth', delta);
        return;
    }

    if (caveSculpt.isSculpting()) {
        e.preventDefault();
        caveSculpt.adjustRadius(delta * 0.15);
        return;
    }

    // Stair placement (simple-stair pre-click + C-connect both phases):
    // scroll adjusts the stair width preview and keeps the HUD input in sync.
    if (state.tool === 'platform' && (
        state.platformPhase === 'simple_stair_from'
        || state.platformPhase === 'connecting_dst'
        || state.platformPhase === 'connecting_src'
    )) {
        e.preventDefault();
        state.stairWidth = Math.max(1, Math.min(32, state.stairWidth + delta));
        const input = document.getElementById('stair-width');
        if (input) input.value = state.stairWidth;
        return;
    }

    if (state.tool !== 'csg') return;

    // Brace mode: scroll adjusts width, Shift+scroll adjusts depth
    if (state.csg.braceMode) {
        e.preventDefault();
        if (e.shiftKey) csgActions.adjustBraceSize(0, delta);
        else            csgActions.adjustBraceSize(delta, 0);
        return;
    }
    // Pillar mode: scroll adjusts the square cross-section size
    if (state.csg.pillarMode) {
        e.preventDefault();
        csgActions.adjustPillarSize(delta);
        return;
    }
    // CSG selection: scroll adjusts selection U size, Shift+scroll adjusts V size
    if (state.csg.selectedFace) {
        e.preventDefault();
        if (e.shiftKey) csgActions.adjustSelectionSize(0, delta);
        else            csgActions.adjustSelectionSize(delta, 0);
    }
}, { passive: false });

// Handle ortho resize
window.addEventListener('resize', () => {
    handleOrthoResize();
});

// Key actions — delegated to tool handlers
onKeyDown((e) => {
    if (state.radialMenuOpen) return;
    // G during the build phase (indoor) freezes the level and starts the hunt.
    if (e.code === 'KeyG' && state.editorMode === 'indoor' && getPhase() === 'build') {
        e.preventDefault();
        startHunt();
        return;
    }
    // During the hunt, route hunt hotkeys (E/K) then swallow editor hotkeys.
    if (isHuntActive()) { handleHuntKey(e); return; }
    if (state.editorMode === 'terrain') {
        handleTerrainKey(e, { generateTerrainMesh, rebuildAll });
        return;
    }
    handleIndoorKey(e, { gizmo, camera });
});

// ============================================================
// INIT — load schemes, then start
// ============================================================
(async () => {
    await Promise.all([initCSGWasm(), initCaveWasm(), loadTextureSchemes()]);
    __mark('wasmAndSchemes');
    initMaterials();
    __mark('initMaterials');

    // Wire radial menu actions to editor operations.
    initMenuActions({
        showMessage,
        rebuildAll,
    });

    // Try loading saved level — if found, skip the mode chooser
    let loadedSave = false;
    try {
        const saved = loadFromLocalStorage();
        if (saved) {
            const data = JSON.parse(saved);
            const hasBrushes = data.csgBrushes && data.csgBrushes.length > 0;
            const hasTerrain = data.terrainMaps && data.terrainMaps.length > 0;
            if (hasBrushes || hasTerrain) {
                deserializeLevel(saved);
                rebuildAll();
                loadedSave = true;
                document.getElementById('lock-prompt').style.display = 'none';
                if (hasTerrain && !hasBrushes) {
                    // Terrain-only save — go to terrain mode
                    toggleEditorMode();
                }
                // Otherwise stay in indoor mode (user clicks to lock)
            }
        }
    } catch (e) { console.warn('Failed to load saved level:', e.message); }

    // Mode chooser buttons
    if (!loadedSave) {
        const btnIndoor = document.getElementById('btn-indoor');
        const btnTerrain = document.getElementById('btn-terrain');

        function startIndoorMode() {
            document.getElementById('lock-prompt').style.display = 'none';
            const firstBrush = new BrushDef(state.csg.nextBrushId++, 'subtract', 0, 0, 0, 16, 12, 16);
            state.csg.brushes.push(firstBrush);
            rebuildAllCSG();
            renderer.domElement.requestPointerLock();
        }

        function startTerrainMode() {
            document.getElementById('lock-prompt').style.display = 'none';
            toggleEditorMode(); // switches to terrain ortho mode
        }

        if (btnIndoor) btnIndoor.addEventListener('click', startIndoorMode);
        if (btnTerrain) btnTerrain.addEventListener('click', startTerrainMode);
    }

    // Terrain settings panel inputs
    const terrainSubdivInput = document.getElementById('terrain-subdivision');
    const terrainWallHeightInput = document.getElementById('terrain-wall-height');
    const terrainBrushRadiusInput = document.getElementById('terrain-brush-radius');
    const terrainBrushStrengthInput = document.getElementById('terrain-brush-strength');

    if (terrainSubdivInput) {
        terrainSubdivInput.addEventListener('change', () => {
            const terrain = getActiveTerrain();
            if (terrain) terrain.subdivisionLevel = Math.max(1, Math.min(20, parseInt(terrainSubdivInput.value) || 8));
        });
    }
    if (terrainWallHeightInput) {
        terrainWallHeightInput.addEventListener('change', () => {
            const terrain = getActiveTerrain();
            if (terrain) {
                const val = Math.max(1, parseInt(terrainWallHeightInput.value) || 20);
                if (terrain.wallStyle === 'rocky') {
                    terrain.rockyWallHeight = val;
                } else {
                    terrain.wallHeight = val;
                }
                if (terrain.hasMesh) rebuildTerrainWalls(terrain);
            }
        });
    }
    if (terrainBrushRadiusInput) {
        terrainBrushRadiusInput.addEventListener('change', () => {
            state.brushRadius = Math.max(1, Math.min(50, parseInt(terrainBrushRadiusInput.value) || 8));
        });
    }
    if (terrainBrushStrengthInput) {
        terrainBrushStrengthInput.addEventListener('change', () => {
            state.brushStrength = Math.max(0.1, Math.min(1, parseFloat(terrainBrushStrengthInput.value) || 0.5));
        });
    }

    // Light settings panel inputs
    const lightColorInput = document.getElementById('light-color');
    const lightIntensityInput = document.getElementById('light-intensity');
    const lightRangeInput = document.getElementById('light-range');

    function getSelectedLight() {
        if (state.selectedLightId == null) return null;
        return state.pointLights.find(l => l.id === state.selectedLightId) || null;
    }

    if (lightColorInput) {
        lightColorInput.addEventListener('input', () => {
            const light = getSelectedLight();
            if (!light) return;
            const hex = lightColorInput.value;
            light.color.r = parseInt(hex.slice(1, 3), 16) / 255;
            light.color.g = parseInt(hex.slice(3, 5), 16) / 255;
            light.color.b = parseInt(hex.slice(5, 7), 16) / 255;
            rebuildLight(light);
        });
    }
    if (lightIntensityInput) {
        lightIntensityInput.addEventListener('change', () => {
            const light = getSelectedLight();
            if (!light) return;
            light.intensity = Math.max(0.1, parseFloat(lightIntensityInput.value) || 1.0);
            rebuildLight(light);
        });
    }
    if (lightRangeInput) {
        lightRangeInput.addEventListener('change', () => {
            const light = getSelectedLight();
            if (!light) return;
            light.range = Math.max(1, parseInt(lightRangeInput.value) || 20);
            rebuildLight(light);
        });
    }

    const lightAmbientInput = document.getElementById('light-ambient');
    if (lightAmbientInput) {
        lightAmbientInput.addEventListener('input', () => {
            const v = Math.max(0, parseFloat(lightAmbientInput.value) || 0);
            state.ambientIntensity = v;
            setAmbientIntensity(v);
        });
    }

    const lightShadowInput = document.getElementById('light-cast-shadow');
    if (lightShadowInput) {
        lightShadowInput.addEventListener('change', () => {
            const light = getSelectedLight();
            if (!light) return;
            light.castShadow = lightShadowInput.checked;
            updateLightShadowFlag(light);
        });
    }

    animate();
    __mark('boot_done');

    // Boot timing summary. __bootT.start is captured at top of main.js
    // (after browser finished downloading + parsing the entire module graph),
    // so its value = time from page navigation to main.js execution start.
    const phases = [
        ['module download + parse', __bootT.start],
        ['scene + input + hud',     __bootT.sceneInputHud - __bootT.start],
        ['gizmo + toolMgr',         __bootT.gizmoToolMgr - __bootT.sceneInputHud],
        ['wasm + schemes',          __bootT.wasmAndSchemes - __bootT.gizmoToolMgr],
        ['initMaterials',           __bootT.initMaterials - __bootT.wasmAndSchemes],
        ['menus + load + rebuild',  __bootT.boot_done - __bootT.initMaterials],
        ['TOTAL to interactive',    __bootT.boot_done],
    ];
    console.table(phases.map(([phase, ms]) => ({ phase, ms: +ms.toFixed(1) })));

    // Defer BMP texture loads off the critical path. Once they finish,
    // clear the CSG material cache and rebuild so any meshes that were
    // built with magenta-fallback materials get the real textures.
    const deferBmpLoad = () => {
        const t0 = performance.now();
        loadBmpTextures().then(() => {
            clearCSGMaterialCache();
            rebuildAll();
            console.log(`[boot] BMP textures loaded in ${(performance.now() - t0).toFixed(1)}ms`);
        });
    };
    if (typeof requestIdleCallback === 'function') {
        requestIdleCallback(deferBmpLoad, { timeout: 1000 });
    } else {
        setTimeout(deferBmpLoad, 0);
    }
})();

// ============================================================
// RENDER LOOP
// ============================================================
const clock = new THREE.Clock();

function animate() {
    requestAnimationFrame(animate);
    const dt = clock.getDelta();

    // ---- TERRAIN MODE FRAME UPDATE ----
    if (state.editorMode === 'terrain') {
        if (state.terrainCameraMode === 'ortho') {
            // Keyboard pan in ortho mode (no pointer lock needed)
            const keys = new Set();
            if (isKeyDown('KeyW')) keys.add('KeyW');
            if (isKeyDown('KeyS')) keys.add('KeyS');
            if (isKeyDown('KeyA')) keys.add('KeyA');
            if (isKeyDown('KeyD')) keys.add('KeyD');
            if (isKeyDown('ArrowUp')) keys.add('ArrowUp');
            if (isKeyDown('ArrowDown')) keys.add('ArrowDown');
            if (isKeyDown('ArrowLeft')) keys.add('ArrowLeft');
            if (isKeyDown('ArrowRight')) keys.add('ArrowRight');
            updateOrthoCamera(dt, keys);
        } else {
            // Perspective mode in terrain — use normal FPS camera
            if (gizmo.isDragging()) {
                const { dx, dy } = consumeMouseDelta();
                gizmo.processDrag(dx, dy, camera);
            }
            updateCamera(camera, dt);

            // Apply sculpting brush while mouse is held
            if (getIsSculpting() && state.terrainTool === 'sculpt') {
                const terrain = getActiveTerrain();
                if (terrain && terrain.hasMesh) {
                    const terrainMesh = terrainMeshes.get(terrain.id);
                    if (terrainMesh) {
                        const raycaster = new THREE.Raycaster();
                        raycaster.setFromCamera(new THREE.Vector2(0, 0), camera);
                        const intersects = raycaster.intersectObject(terrainMesh, false);
                        if (intersects.length > 0) {
                            const p = intersects[0].point;
                            const W = WORLD_SCALE;
                            const invert = isKeyDown('ShiftLeft') || isKeyDown('ShiftRight');
                            applyBrush(terrain, p.x / W, p.z / W, {
                                type: state.brushType,
                                radius: state.brushRadius,
                                strength: state.brushStrength,
                                noiseScale: state.brushNoiseScale,
                                noiseAmp: state.brushNoiseAmp,
                            }, dt, invert);
                            // Update geometry in-place
                            updateTerrainNormals(terrain, terrainMesh.geometry);
                        }
                    }
                }
            }
        }

        updateTerrainPreview(camera);
        updateTerrainHUD();

        const activeCamera = state.terrainCameraMode === 'ortho' ? orthoCamera : camera;

        // No fog in terrain mode — use appropriate background per camera
        scene.fog = null;
        if (state.terrainCameraMode === 'ortho') {
            scene.background = new THREE.Color(TERRAIN_BG_COLOR);
        } else {
            scene.background = new THREE.Color(TERRAIN_PERSPECTIVE_BG);
        }

        renderer.render(scene, activeCamera);
        return;
    }

    // ---- INDOOR MODE FRAME UPDATE ----
    // Restore fog and background for indoor mode
    if (!scene.fog) {
        scene.fog = new THREE.Fog(INDOOR_BG_COLOR, FOG_NEAR, FOG_FAR);
        scene.background = new THREE.Color(INDOOR_BG_COLOR);
    }

    // ---- HUNT PHASE ---- level is frozen; drive player + enemies, skip editor.
    if (isHuntActive()) {
        updateGame(dt);
        renderer.render(scene, camera);
        return;
    }

    // If gizmo is being dragged, consume mouse delta for the gizmo instead of camera
    if (gizmo.isDragging()) {
        const { dx, dy } = consumeMouseDelta();
        const changed = gizmo.processDrag(dx, dy, camera);
        if (changed) {
            if (state.tool === 'light' && state.selectedLightId != null) {
                const light = state.pointLights.find(l => l.id === state.selectedLightId);
                if (light) rebuildLight(light);
            } else {
                const plat = state.platforms.find(p => p.id === state.selectedPlatformId);
                if (plat) {
                    rebuildPlatform(plat);
                    rebuildConnectedStairRuns(plat.id);
                }
            }
        }
    }

    updateCamera(camera, dt);

    // Update gizmo position and hover state
    let gizmoTarget = null;
    if (state.tool === 'platform' && state.selectedPlatformId != null) {
        gizmoTarget = state.platforms.find(p => p.id === state.selectedPlatformId) || null;
    } else if (state.tool === 'light' && state.selectedLightId != null) {
        gizmoTarget = state.pointLights.find(l => l.id === state.selectedLightId) || null;
    }
    gizmo.update(gizmoTarget, camera);

    caveSculpt.tick(camera, dt);

    updatePlatformPreview(camera);
    updateCSGSelectionPreview(camera);
    updateCSGMultiSelectionPreview();
    updateCSGHolePreview(camera);
    updateCSGBracePreview(camera);
    updateCSGPillarPreview(camera);
    updateCSGStairPreview();
    updateHUD(camera);
    renderer.render(scene, camera);
}
