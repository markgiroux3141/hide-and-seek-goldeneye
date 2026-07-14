// player — first-person capsule controller for the HUNT phase. Reuses the
// editor camera; drives it with gravity + axis-separated AABB-vs-grid collision
// against the frozen NavWorld. Meters throughout (Three.js world units).

import * as THREE from 'three';
import { isKeyDown, isPointerLocked, consumeMouseDelta } from '../input/input.js';
import { WORLD_SCALE } from '../core/constants.js';

const WT = WORLD_SCALE;              // meters per WT
const RADIUS = 1.0 * WT;             // horizontal half-extent
const HEIGHT = 6 * WT;               // full standing height
const EYE = 5.4 * WT;                // eye offset above feet
const WALK_SPEED = 3.2;              // m/s
const GRAVITY = 20;                  // m/s^2
const JUMP_VELOCITY = 5.5;           // m/s
const LOOK_SPEED = 0.002;

export class Player {
    constructor(camera, nav) {
        this.camera = camera;
        this.nav = nav;
        this.pos = new THREE.Vector3();   // feet position (meters)
        this.vel = new THREE.Vector3();
        this.grounded = false;
        this.yaw = 0;
        this.pitch = 0;
    }

    // Place feet at a meters position (e.g. a cell floor).
    spawnAt(feet) {
        this.pos.set(feet.x, feet.y + 0.02, feet.z);
        this.vel.set(0, 0, 0);
        // Face roughly toward world center-ish; keep current camera yaw.
        const e = new THREE.Euler().setFromQuaternion(this.camera.quaternion, 'YXZ');
        this.yaw = e.y; this.pitch = 0;
        this._syncCamera();
    }

    // True if the player AABB at feet (fx,fy,fz) overlaps any solid cell.
    _collides(fx, fy, fz) {
        const nav = this.nav;
        const minX = fx - RADIUS, maxX = fx + RADIUS;
        const minY = fy,          maxY = fy + HEIGHT - 0.001;
        const minZ = fz - RADIUS, maxZ = fz + RADIUS;
        const step = WT;
        for (let y = minY; y <= maxY; y += step) {
            for (let z = minZ; z <= maxZ; z += step) {
                for (let x = minX; x <= maxX; x += step) {
                    if (nav.isSolidMeters(x, y, z)) return true;
                }
            }
        }
        // Ensure the exact top of the head is tested even if step overshoots.
        for (let z = minZ; z <= maxZ; z += step)
            for (let x = minX; x <= maxX; x += step)
                if (nav.isSolidMeters(x, maxY, z)) return true;
        return false;
    }

    update(dt) {
        // ── Look ──
        if (isPointerLocked()) {
            const { dx, dy } = consumeMouseDelta();
            this.yaw -= dx * LOOK_SPEED;
            this.pitch -= dy * LOOK_SPEED;
            this.pitch = Math.max(-Math.PI / 2, Math.min(Math.PI / 2, this.pitch));
        }

        // ── Horizontal wish direction (yaw only) ──
        const sin = Math.sin(this.yaw), cos = Math.cos(this.yaw);
        // Forward = -Z when yaw 0 (Three.js convention).
        let wishX = 0, wishZ = 0;
        if (isKeyDown('KeyW')) { wishX -= sin; wishZ -= cos; }
        if (isKeyDown('KeyS')) { wishX += sin; wishZ += cos; }
        if (isKeyDown('KeyA')) { wishX -= cos; wishZ += sin; }
        if (isKeyDown('KeyD')) { wishX += cos; wishZ -= sin; }
        const len = Math.hypot(wishX, wishZ);
        if (len > 0) { wishX /= len; wishZ /= len; }

        // ── Gravity + jump ──
        this.vel.y -= GRAVITY * dt;
        if (this.grounded && isKeyDown('Space')) { this.vel.y = JUMP_VELOCITY; this.grounded = false; }

        // ── Axis-separated movement ──
        // Horizontal moves auto-step up to STEP_HEIGHT so the player can walk
        // onto stairs/ledges; vertical is pure gravity/jump with NO nudging
        // (nudging vertically causes floor jitter).
        const p = this.pos;
        const STEP_HEIGHT = 1 * WT;
        const mvX = wishX * WALK_SPEED * dt;
        const mvZ = wishZ * WALK_SPEED * dt;
        const mvY = this.vel.y * dt;

        if (mvX !== 0) {
            if (!this._collides(p.x + mvX, p.y, p.z)) p.x += mvX;
            else if (this.grounded && !this._collides(p.x + mvX, p.y + STEP_HEIGHT, p.z)) { p.x += mvX; p.y += STEP_HEIGHT; }
        }
        if (mvZ !== 0) {
            if (!this._collides(p.x, p.y, p.z + mvZ)) p.z += mvZ;
            else if (this.grounded && !this._collides(p.x, p.y + STEP_HEIGHT, p.z + mvZ)) { p.z += mvZ; p.y += STEP_HEIGHT; }
        }

        this.grounded = false;
        if (!this._collides(p.x, p.y + mvY, p.z)) {
            p.y += mvY;
        } else {
            if (this.vel.y < 0) this.grounded = true;
            this.vel.y = 0;
        }

        this._syncCamera();
    }

    _syncCamera() {
        this.camera.position.set(this.pos.x, this.pos.y + EYE, this.pos.z);
        const e = new THREE.Euler(this.pitch, this.yaw, 0, 'YXZ');
        this.camera.quaternion.setFromEuler(e);
    }
}
