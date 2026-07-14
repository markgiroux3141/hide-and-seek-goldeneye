// First-person fly camera — no gravity

import * as THREE from 'three';
import { isKeyDown, isPointerLocked, consumeMouseDelta } from '../input/input.js';

const MOVE_SPEED = 8;
const LOOK_SPEED = 0.002;
const euler = new THREE.Euler(0, 0, 0, 'YXZ');

export function updateCamera(camera, dt) {
    if (!isPointerLocked()) return;

    // Mouse look
    const { dx, dy } = consumeMouseDelta();
    euler.setFromQuaternion(camera.quaternion);
    euler.y -= dx * LOOK_SPEED;
    euler.x -= dy * LOOK_SPEED;
    euler.x = Math.max(-Math.PI / 2, Math.min(Math.PI / 2, euler.x));
    camera.quaternion.setFromEuler(euler);

    // Movement
    const forward = new THREE.Vector3();
    camera.getWorldDirection(forward);
    const right = new THREE.Vector3().crossVectors(forward, camera.up).normalize();
    const speed = MOVE_SPEED * dt;

    if (isKeyDown('KeyW')) camera.position.addScaledVector(forward, speed);
    if (isKeyDown('KeyS')) camera.position.addScaledVector(forward, -speed);
    if (isKeyDown('KeyA')) camera.position.addScaledVector(right, -speed);
    if (isKeyDown('KeyD')) camera.position.addScaledVector(right, speed);
    if (isKeyDown('Space')) camera.position.y += speed;
}
