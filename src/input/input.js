// Keyboard and mouse input state tracking

import { state } from '../state.js';

const keys = new Set();
let isLocked = false;
let mouseDX = 0;
let mouseDY = 0;
let pointerLockEnabled = true;
let canvasRef = null;

// Callbacks for middle-click menu
const middleClickCallbacks = [];

export function setPointerLockEnabled(enabled) {
    pointerLockEnabled = enabled;
}

export function onMiddleClick(callback) {
    middleClickCallbacks.push(callback);
}

export function reacquirePointerLock() {
    if (canvasRef && !isLocked && pointerLockEnabled) {
        canvasRef.requestPointerLock();
    }
}

export function releasePointerLock() {
    if (document.pointerLockElement) {
        document.exitPointerLock();
    }
}

export function initInput(canvas) {
    canvasRef = canvas;

    document.addEventListener('keydown', (e) => keys.add(e.code));
    document.addEventListener('keyup', (e) => keys.delete(e.code));

    canvas.addEventListener('click', () => {
        if (!isLocked && pointerLockEnabled) canvas.requestPointerLock();
    });

    document.addEventListener('pointerlockchange', () => {
        isLocked = document.pointerLockElement === canvas;
        // When radial menu is open or in terrain ortho mode, keep lock-prompt hidden
        if (state.radialMenuOpen || (state.editorMode === 'terrain' && state.terrainCameraMode === 'ortho')) {
            document.getElementById('lock-prompt').style.display = 'none';
            document.getElementById('crosshair').style.display = 'none';
            return;
        }
        document.getElementById('lock-prompt').style.display = isLocked ? 'none' : 'block';
        document.getElementById('crosshair').style.display = isLocked ? 'block' : 'none';
    });

    // Middle-click detection
    document.addEventListener('mousedown', (e) => {
        if (e.button === 1) {
            e.preventDefault();
            for (const cb of middleClickCallbacks) cb(e);
        }
    });

    // Prevent middle-click auto-scroll
    document.addEventListener('auxclick', (e) => {
        if (e.button === 1) e.preventDefault();
    });

    document.addEventListener('mousemove', (e) => {
        if (!isLocked) return;
        mouseDX += e.movementX;
        mouseDY += e.movementY;
    });
}

export function isKeyDown(code) { return keys.has(code); }
export function isPointerLocked() { return isLocked; }

export function consumeMouseDelta() {
    const dx = mouseDX;
    const dy = mouseDY;
    mouseDX = 0;
    mouseDY = 0;
    return { dx, dy };
}

// Event registration for specific key actions (keydown only, not held)
const keyDownCallbacks = [];

export function onKeyDown(callback) {
    keyDownCallbacks.push(callback);
}

// Must be called once during init
export function initKeyActions() {
    document.addEventListener('keydown', (e) => {
        for (const cb of keyDownCallbacks) cb(e);
    });
}
