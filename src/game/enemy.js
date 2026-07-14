// enemy — a "grunt" hunter with perception. Instead of tracking the player
// omnisciently, it must SEE the player (vision cone + line-of-sight). AI states:
//   patrol — wander to random floor cells, scanning; no target.
//   chase  — player currently visible; path to their live position.
//   search — lost sight; go to last-known position, look around, then give up.
// This is what makes hiding and misdirection possible — the core of the game.

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';

const WT = WORLD_SCALE;
const SPEED_CHASE = 2.8;           // m/s
const SPEED_PATROL = 1.5;          // m/s
const REPATH_INTERVAL = 0.4;       // s between path recomputes while chasing
const CATCH_DIST = 1.2 * WT;
const WAYPOINT_EPS = 0.4 * WT;

// Perception
const SIGHT_RANGE = 18;            // m
const HALF_FOV = Math.PI / 3;      // 60° => 120° cone
const EYE_H = 1.3;                 // enemy eye height above feet (m)
const PLAYER_BODY_H = 0.9;         // aim LOS at player chest above their feet (m)
const HEAR_RADIUS = 3.5;           // m — close-range awareness regardless of facing/LOS
const HEAR_VERT = 2.5;             // m — hearing only within ~1 floor vertically
const LOSE_SIGHT_GRACE = 1.3;      // s — stay locked on after losing detection
const SEARCH_DURATION = 6;         // s spent searching last-known before giving up
const TURN_RATE = 7;               // rad/s facing ease
const SCAN_SPEED = 1.8;            // rad/s head-turn while searching/patrolling

function gruntMesh() {
    const geo = new THREE.CapsuleGeometry(1.0 * WT, 4 * WT, 4, 8);
    const mat = new THREE.MeshStandardMaterial({ color: 0x888888, emissive: 0x000000, roughness: 0.6 });
    const body = new THREE.Mesh(geo, mat);
    // A small nose so its facing/vision direction is legible while testing.
    const nose = new THREE.Mesh(
        new THREE.ConeGeometry(0.5 * WT, 1.2 * WT, 6),
        new THREE.MeshStandardMaterial({ color: 0x222222 }),
    );
    nose.rotation.x = Math.PI / 2;
    nose.position.set(0, 1.2 * WT, -1.2 * WT);
    body.add(nose);
    body.userData.mat = mat;
    return body;
}

export class Enemy {
    constructor(scene, nav, feet) {
        this.nav = nav;
        this.scene = scene;
        this.pos = new THREE.Vector3(feet.x, feet.y, feet.z);
        this.mesh = gruntMesh();
        this.mat = this.mesh.userData.mat;
        scene.add(this.mesh);

        this.state = 'patrol';
        this.facing = 0;              // yaw radians, direction the cone points
        this.path = null;
        this.pathIdx = 0;
        // Stagger initial repath so a whole wave doesn't A* on the same frame.
        this.repathTimer = Math.random() * REPATH_INTERVAL;
        this.lastKnown = null;        // {x,y,z} last place player was detected
        this.loseTimer = 0;           // grace countdown after losing detection
        this.searchTimer = 0;
        this.patrolTarget = null;
        this._syncMesh();
    }

    get speed() { return this.state === 'chase' ? SPEED_CHASE : SPEED_PATROL; }

    _syncMesh() {
        this.mesh.position.set(this.pos.x, this.pos.y + 3 * WT, this.pos.z);
        this.mesh.rotation.y = this.facing;
    }

    _standableCells() {
        return (this.nav.__standable ||= this.nav.allStandable());
    }

    // Can this enemy currently see the player? Vision cone + range + LOS.
    _canSee(playerFeet) {
        const eye = { x: this.pos.x, y: this.pos.y + EYE_H, z: this.pos.z };
        const body = { x: playerFeet.x, y: playerFeet.y + PLAYER_BODY_H, z: playerFeet.z };
        const dx = body.x - eye.x, dz = body.z - eye.z;
        const dist = Math.hypot(dx, dz);
        if (dist > SIGHT_RANGE) return false;
        // Facing forward vector is -Z rotated by yaw (matches mesh/nose).
        const fx = -Math.sin(this.facing), fz = -Math.cos(this.facing);
        const dot = (dx * fx + dz * fz) / (dist || 1);
        if (dot < Math.cos(HALF_FOV)) return false;   // outside cone
        return this.nav.losClear(eye, body);
    }

    // Close-range awareness: you can't sneak past a hunter in the same room.
    // Independent of facing/LOS but limited to a small radius and ~1 floor.
    _canHear(playerFeet) {
        const dx = playerFeet.x - this.pos.x, dz = playerFeet.z - this.pos.z;
        if (Math.abs(playerFeet.y - this.pos.y) > HEAR_VERT) return false;
        return Math.hypot(dx, dz) < HEAR_RADIUS;
    }

    _repathTo(goal) {
        const path = this.nav.findPath(this.pos, goal);
        if (path && path.length > 1) { this.path = path; this.pathIdx = 1; return true; }
        this.path = null;
        return false;
    }

    // Walk the current path; returns the horizontal move vector taken (for facing).
    _followPath(dt) {
        if (!this.path || this.pathIdx >= this.path.length) return { mx: 0, mz: 0, done: true };
        const target = this.path[this.pathIdx];
        const dx = target.x - this.pos.x, dz = target.z - this.pos.z;
        const dist = Math.hypot(dx, dz);
        if (dist < WAYPOINT_EPS) {
            this.pos.y = target.y;
            this.pathIdx++;
            return { mx: 0, mz: 0, done: this.pathIdx >= this.path.length };
        }
        const step = Math.min(dist, this.speed * dt);
        const mx = (dx / dist) * step, mz = (dz / dist) * step;
        this.pos.x += mx; this.pos.z += mz;
        this.pos.y += (target.y - this.pos.y) * Math.min(1, dt * 6);
        return { mx, mz, done: false };
    }

    _pickPatrolTarget() {
        const cells = this._standableCells();
        if (!cells.length) return;
        for (let tries = 0; tries < 8; tries++) {
            const c = cells[(Math.random() * cells.length) | 0];
            const goal = this.nav.cellFloorMeters(c.ix, c.iy, c.iz);
            if (this._repathTo(goal)) { this.patrolTarget = goal; return; }
        }
    }

    // Ease facing toward a target yaw.
    _faceToward(yaw, dt) {
        let d = yaw - this.facing;
        while (d > Math.PI) d -= 2 * Math.PI;
        while (d < -Math.PI) d += 2 * Math.PI;
        this.facing += d * Math.min(1, TURN_RATE * dt);
    }

    // Returns true if the player was caught this frame.
    update(dt, playerFeet) {
        const detected = this._canSee(playerFeet) || this._canHear(playerFeet);
        if (detected) {
            this.state = 'chase';
            this.lastKnown = { x: playerFeet.x, y: playerFeet.y, z: playerFeet.z };
            this.loseTimer = LOSE_SIGHT_GRACE;
        } else if (this.state === 'chase') {
            // Keep locked on briefly so momentary occlusion doesn't shake us.
            this.loseTimer -= dt;
            if (this.loseTimer <= 0) { this.state = 'search'; this.searchTimer = SEARCH_DURATION; }
        }

        if (this.state === 'chase') {
            this.mat.color.setHex(0xcc3322); this.mat.emissive.setHex(0x330000);
        }
        if (this.state === 'search') {
            this.mat.color.setHex(0xddaa33); this.mat.emissive.setHex(0x000000);
        }
        if (this.state === 'patrol') {
            this.mat.color.setHex(0x888888); this.mat.emissive.setHex(0x000000);
        }

        // ── Breach: if an intact door blocks the next path segment, stop and
        // break it instead of moving (the delay mechanic; noise on break). ──
        if (this.path && this.pathIdx < this.path.length) {
            const tgt = this.path[this.pathIdx];
            const door = this.nav.doorBlocking(this.pos, tgt);
            if (door && !door.broken) {
                this._faceToward(Math.atan2(-(tgt.x - this.pos.x), -(tgt.z - this.pos.z)), dt);
                door.hp -= dt;
                if (door.hp <= 0) { door.broken = true; this.onBreach?.(door); }
                this._syncMesh();
                const bdx = playerFeet.x - this.pos.x, bdz = playerFeet.z - this.pos.z;
                return Math.hypot(bdx, bdz) < CATCH_DIST && Math.abs(playerFeet.y - this.pos.y) < 3 * WT;
            }
        }

        // ── Movement + facing per state ──
        if (this.state === 'chase') {
            this.repathTimer -= dt;
            if (this.repathTimer <= 0 || !this.path) {
                this.repathTimer = REPATH_INTERVAL;
                this._repathTo(this.lastKnown);
            }
            const mv = this._followPath(dt);
            if (mv.mx || mv.mz) this._faceToward(Math.atan2(-mv.mx, -mv.mz), dt);
        } else if (this.state === 'search') {
            this.searchTimer -= dt;
            if (this.searchTimer <= 0) { this.state = 'patrol'; this.path = null; this.patrolTarget = null; }
            else if (this.path && this.pathIdx < this.path.length) {
                const mv = this._followPath(dt);
                if (mv.mx || mv.mz) this._faceToward(Math.atan2(-mv.mx, -mv.mz), dt);
            } else if (this.lastKnown && !this.path) {
                if (!this._repathTo(this.lastKnown)) this.facing += SCAN_SPEED * dt; // arrived/unreachable: scan
                else this.pathIdx = 1;
            } else {
                this.facing += SCAN_SPEED * dt; // look around at last-known
            }
        } else { // patrol
            if (!this.path || this.pathIdx >= this.path.length) this._pickPatrolTarget();
            const mv = this._followPath(dt);
            if (mv.mx || mv.mz) this._faceToward(Math.atan2(-mv.mx, -mv.mz), dt);
            else this.facing += SCAN_SPEED * 0.5 * dt;
        }

        this._syncMesh();

        const cdx = playerFeet.x - this.pos.x, cdz = playerFeet.z - this.pos.z;
        return Math.hypot(cdx, cdz) < CATCH_DIST && Math.abs(playerFeet.y - this.pos.y) < 3 * WT;
    }

    dispose(scene) {
        scene.remove(this.mesh);
    }
}
