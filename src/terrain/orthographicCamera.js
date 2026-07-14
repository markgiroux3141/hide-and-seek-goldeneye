// Top-down orthographic camera for terrain boundary drawing
// Provides pan (WASD / middle-mouse drag) and zoom (scroll wheel)

import * as THREE from 'three';

const PAN_SPEED = 15;      // WT units per second for keyboard pan
const ZOOM_SPEED = 0.1;    // zoom factor per scroll notch
const MIN_ZOOM = 5;        // minimum frustum half-height in WT
const MAX_ZOOM = 200;      // maximum frustum half-height in WT

let orthoCamera = null;
let zoomLevel = 60;        // frustum half-height in WT units
let panX = 0;              // camera center X in WT units
let panZ = 0;              // camera center Z in WT units
let isMiddleMouseDown = false;
let lastMouseX = 0;
let lastMouseY = 0;

export function createOrthoCamera() {
    const aspect = window.innerWidth / window.innerHeight;
    orthoCamera = new THREE.OrthographicCamera(
        -zoomLevel * aspect, zoomLevel * aspect,
        zoomLevel, -zoomLevel,
        0.1, 1000
    );
    // Look straight down
    orthoCamera.position.set(0, 100, 0);
    orthoCamera.lookAt(0, 0, 0);
    orthoCamera.up.set(0, 0, -1); // Z-negative is "up" in top-down view

    return orthoCamera;
}

export function getOrthoCamera() {
    return orthoCamera;
}

export function getOrthoCameraState() {
    return { panX, panZ, zoomLevel };
}

export function setOrthoCameraState(px, pz, zoom) {
    panX = px;
    panZ = pz;
    zoomLevel = zoom;
    updateOrthoFrustum();
}

function updateOrthoFrustum() {
    if (!orthoCamera) return;
    const aspect = window.innerWidth / window.innerHeight;
    orthoCamera.left = -zoomLevel * aspect;
    orthoCamera.right = zoomLevel * aspect;
    orthoCamera.top = zoomLevel;
    orthoCamera.bottom = -zoomLevel;
    orthoCamera.position.set(panX, 100, panZ);
    orthoCamera.lookAt(panX, 0, panZ);
    orthoCamera.updateProjectionMatrix();
}

// Call each frame when in terrain mode with ortho camera
export function updateOrthoCamera(dt, keys) {
    let moved = false;
    const speed = PAN_SPEED * dt * (zoomLevel / 60); // scale speed with zoom

    if (keys.has('KeyW') || keys.has('ArrowUp')) { panZ -= speed; moved = true; }
    if (keys.has('KeyS') || keys.has('ArrowDown')) { panZ += speed; moved = true; }
    if (keys.has('KeyA') || keys.has('ArrowLeft')) { panX -= speed; moved = true; }
    if (keys.has('KeyD') || keys.has('ArrowRight')) { panX += speed; moved = true; }

    if (moved) updateOrthoFrustum();
}

// Handle scroll wheel for zoom
export function handleOrthoZoom(deltaY) {
    const factor = deltaY > 0 ? (1 + ZOOM_SPEED) : (1 - ZOOM_SPEED);
    zoomLevel = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoomLevel * factor));
    updateOrthoFrustum();
}

// Handle middle-mouse pan
export function handleOrthoMiddleMouseDown(x, y) {
    isMiddleMouseDown = true;
    lastMouseX = x;
    lastMouseY = y;
}

export function handleOrthoMiddleMouseUp() {
    isMiddleMouseDown = false;
}

export function handleOrthoMiddleMouseMove(x, y) {
    if (!isMiddleMouseDown || !orthoCamera) return;
    const dx = x - lastMouseX;
    const dy = y - lastMouseY;
    lastMouseX = x;
    lastMouseY = y;

    // Convert pixel movement to world units
    const pixelsPerUnit = window.innerWidth / (2 * zoomLevel * (window.innerWidth / window.innerHeight));
    panX -= dx / pixelsPerUnit;
    panZ -= dy / pixelsPerUnit;
    updateOrthoFrustum();
}

// Handle window resize
export function handleOrthoResize() {
    updateOrthoFrustum();
}

// Convert screen coordinates (pixels) to world XZ coordinates (WT units)
export function screenToWorldXZ(screenX, screenY) {
    if (!orthoCamera) return { x: 0, z: 0 };

    // Normalized device coords (-1 to 1)
    const ndcX = (screenX / window.innerWidth) * 2 - 1;
    const ndcY = -(screenY / window.innerHeight) * 2 + 1;

    // Unproject to world
    const vec = new THREE.Vector3(ndcX, ndcY, 0);
    vec.unproject(orthoCamera);

    return { x: vec.x, z: vec.z };
}

// Center the camera on given WT coordinates
export function centerOrthoOn(x, z) {
    panX = x;
    panZ = z;
    updateOrthoFrustum();
}
