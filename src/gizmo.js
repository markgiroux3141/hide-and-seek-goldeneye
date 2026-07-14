// Platform gizmo — Blender-style draggable arrows (move) and edge handles (scale)
// Works with FPS pointer-lock camera: aim crosshair at handle, click to start drag,
// mouse movement moves/scales along that axis (quantized to WT), click again to confirm.

import * as THREE from 'three';
import {
    WORLD_SCALE,
    GIZMO_ARROW_LENGTH, GIZMO_SHAFT_RADIUS, GIZMO_TIP_LENGTH,
    GIZMO_TIP_RADIUS, GIZMO_HANDLE_SIZE, GIZMO_DRAG_SENSITIVITY,
} from './core/constants.js';

const S = WORLD_SCALE;

const ARROW_LENGTH = GIZMO_ARROW_LENGTH;
const SHAFT_RADIUS = GIZMO_SHAFT_RADIUS;
const TIP_LENGTH = GIZMO_TIP_LENGTH;
const TIP_RADIUS = GIZMO_TIP_RADIUS;
const HANDLE_SIZE = GIZMO_HANDLE_SIZE;

const AXIS_COLORS = {
    x: 0xee3333,
    y: 0x33ee33,
    z: 0x3333ee,
};

const AXIS_COLORS_HIGHLIGHT = {
    x: 0xff6666,
    y: 0x66ff66,
    z: 0x6666ff,
};

const raycaster = new THREE.Raycaster();
const screenCenter = new THREE.Vector2(0, 0);

export class PlatformGizmo {
    constructor(scene) {
        this.scene = scene;
        this.group = new THREE.Group();
        this.group.visible = false;
        this.scene.add(this.group);

        this.moveArrows = {};   // { x: Group, y: Group, z: Group }
        this.scaleHandles = {}; // { xMax: Mesh, xMin: Mesh, zMax: Mesh, zMin: Mesh }
        this.allParts = [];     // all meshes for raycasting

        this._createMoveArrows();
        this._createScaleHandles();

        this.hoveredPart = null;
        this.drag = null; // { type, axis, platform, origX, origY, origZ, origSizeX, origSizeZ, accumulated }
    }

    _createMoveArrows() {
        for (const [axis, color] of Object.entries(AXIS_COLORS)) {
            const shaftGeo = new THREE.CylinderGeometry(
                SHAFT_RADIUS * S, SHAFT_RADIUS * S, ARROW_LENGTH * S, 8,
            );
            const tipGeo = new THREE.ConeGeometry(TIP_RADIUS * S, TIP_LENGTH * S, 8);

            const mat = new THREE.MeshLambertMaterial({ color, depthTest: false });
            const shaft = new THREE.Mesh(shaftGeo, mat);
            const tip = new THREE.Mesh(tipGeo, mat.clone());

            // CylinderGeometry is along Y by default — rotate to align with axis
            const arrow = new THREE.Group();
            if (axis === 'x') {
                shaft.rotation.z = -Math.PI / 2;
                shaft.position.set((ARROW_LENGTH / 2) * S, 0, 0);
                tip.rotation.z = -Math.PI / 2;
                tip.position.set((ARROW_LENGTH + TIP_LENGTH / 2) * S, 0, 0);
            } else if (axis === 'y') {
                shaft.position.set(0, (ARROW_LENGTH / 2) * S, 0);
                tip.position.set(0, (ARROW_LENGTH + TIP_LENGTH / 2) * S, 0);
            } else { // z
                shaft.rotation.x = Math.PI / 2;
                shaft.position.set(0, 0, (ARROW_LENGTH / 2) * S);
                tip.rotation.x = Math.PI / 2;
                tip.position.set(0, 0, (ARROW_LENGTH + TIP_LENGTH / 2) * S);
            }

            shaft.userData = { gizmoType: 'move', axis };
            tip.userData = { gizmoType: 'move', axis };
            shaft.renderOrder = 999;
            tip.renderOrder = 999;

            arrow.add(shaft);
            arrow.add(tip);
            this.moveArrows[axis] = arrow;
            this.group.add(arrow);
            this.allParts.push(shaft, tip);
        }
    }

    _createScaleHandles() {
        const handleGeo = new THREE.BoxGeometry(HANDLE_SIZE * S, HANDLE_SIZE * S, HANDLE_SIZE * S);

        const edges = ['xMax', 'xMin', 'zMax', 'zMin'];
        for (const edge of edges) {
            const axis = edge.startsWith('x') ? 'x' : 'z';
            const mat = new THREE.MeshLambertMaterial({ color: AXIS_COLORS[axis], depthTest: false });
            const mesh = new THREE.Mesh(handleGeo, mat);
            mesh.userData = { gizmoType: 'scale', axis: edge };
            mesh.renderOrder = 999;
            this.scaleHandles[edge] = mesh;
            this.group.add(mesh);
            this.allParts.push(mesh);
        }
    }

    // Position gizmo to match a target, update hover highlight.
    // target can be a Platform (has centerX, centerZ, sizeX, sizeZ) or
    // a PointLight (has x, y, z only — scale handles are hidden).
    update(target, camera) {
        if (!target) {
            this.group.visible = false;
            return;
        }

        this.group.visible = true;

        const hasDimensions = target.sizeX != null;

        // Position move arrows at target center
        const cx = hasDimensions ? target.centerX * S : target.x * S;
        const cy = target.y * S;
        const cz = hasDimensions ? target.centerZ * S : target.z * S;

        for (const arrow of Object.values(this.moveArrows)) {
            arrow.position.set(cx, cy, cz);
        }

        // Scale handles: show only for targets with dimensions (platforms)
        if (hasDimensions) {
            const hY = (target.y + HANDLE_SIZE / 2) * S;
            this.scaleHandles.xMax.position.set(target.maxX * S, hY, (target.centerZ) * S);
            this.scaleHandles.xMin.position.set(target.x * S, hY, (target.centerZ) * S);
            this.scaleHandles.zMax.position.set((target.centerX) * S, hY, target.maxZ * S);
            this.scaleHandles.zMin.position.set((target.centerX) * S, hY, target.z * S);
        }
        for (const handle of Object.values(this.scaleHandles)) {
            handle.visible = hasDimensions;
        }

        // Update hover highlight
        const hit = this._raycast(camera);
        if (hit !== this.hoveredPart) {
            // Unhighlight old
            if (this.hoveredPart) {
                const ud = this.hoveredPart.userData;
                const baseAxis = ud.axis.replace('Min', '').replace('Max', '');
                this.hoveredPart.material.color.setHex(AXIS_COLORS[baseAxis]);
                this.hoveredPart.material.emissive?.setHex(0x000000);
            }
            // Highlight new
            if (hit) {
                const ud = hit.userData;
                const baseAxis = ud.axis.replace('Min', '').replace('Max', '');
                hit.material.color.setHex(AXIS_COLORS_HIGHLIGHT[baseAxis]);
            }
            this.hoveredPart = hit;
        }
    }

    _raycast(camera) {
        raycaster.setFromCamera(screenCenter, camera);
        const hits = raycaster.intersectObjects(this.allParts, false);
        return hits.length > 0 ? hits[0].object : null;
    }

    // Check if crosshair is pointing at a gizmo part
    // Returns { type: 'move'|'scale', axis: string } or null
    pick(camera) {
        const hit = this._raycast(camera);
        if (!hit) return null;
        return { type: hit.userData.gizmoType, axis: hit.userData.axis };
    }

    isDragging() {
        return this.drag !== null;
    }

    // Start a drag operation. target can be a Platform or PointLight.
    startDrag(type, axis, target) {
        this.drag = {
            type,
            axis,
            platform: target,
            origX: target.x,
            origY: target.y,
            origZ: target.z,
            origSizeX: target.sizeX,
            origSizeZ: target.sizeZ,
            accumulated: 0,
        };
    }

    // Process mouse delta during drag. Returns true if platform changed.
    processDrag(dx, dy, camera) {
        if (!this.drag) return false;

        const { type, axis, platform } = this.drag;

        // Get the world-space axis direction for this drag
        let worldAxis;
        if (type === 'move') {
            if (axis === 'x') worldAxis = new THREE.Vector3(1, 0, 0);
            else if (axis === 'y') worldAxis = new THREE.Vector3(0, 1, 0);
            else worldAxis = new THREE.Vector3(0, 0, 1);
        } else { // scale
            if (axis === 'xMax') worldAxis = new THREE.Vector3(1, 0, 0);
            else if (axis === 'xMin') worldAxis = new THREE.Vector3(-1, 0, 0);
            else if (axis === 'zMax') worldAxis = new THREE.Vector3(0, 0, 1);
            else worldAxis = new THREE.Vector3(0, 0, -1);
        }

        // Project world axis onto screen space to get the 2D drag direction
        const camRight = new THREE.Vector3();
        const camUp = new THREE.Vector3();
        camera.getWorldDirection(new THREE.Vector3()); // ensure matrix is up to date
        camRight.setFromMatrixColumn(camera.matrixWorld, 0);
        camUp.setFromMatrixColumn(camera.matrixWorld, 1);

        const projX = worldAxis.dot(camRight);
        const projY = worldAxis.dot(camUp);

        // Scale sensitivity by distance to target
        const hasDimensions = platform.sizeX != null;
        const targetCenter = new THREE.Vector3(
            (hasDimensions ? platform.centerX : platform.x) * S,
            platform.y * S,
            (hasDimensions ? platform.centerZ : platform.z) * S,
        );
        const dist = Math.max(0.5, camera.position.distanceTo(targetCenter));
        const sensitivity = dist * GIZMO_DRAG_SENSITIVITY;

        // Accumulate drag
        this.drag.accumulated += (dx * projX - dy * projY) * sensitivity;

        // Quantize to WT units
        const wtDelta = Math.round(this.drag.accumulated);
        if (wtDelta === 0) return false;

        // Consume the used portion
        this.drag.accumulated -= wtDelta;

        let changed = false;

        if (type === 'move') {
            if (axis === 'x') { platform.x += wtDelta; changed = true; }
            else if (axis === 'y') { platform.y += wtDelta; changed = true; }
            else if (axis === 'z') { platform.z += wtDelta; changed = true; }
        } else { // scale
            if (axis === 'xMax') {
                const newSize = Math.max(1, platform.sizeX + wtDelta);
                changed = newSize !== platform.sizeX;
                platform.sizeX = newSize;
            } else if (axis === 'xMin') {
                const newSize = Math.max(1, platform.sizeX + wtDelta);
                if (newSize !== platform.sizeX) {
                    platform.x -= (newSize - platform.sizeX);
                    platform.sizeX = newSize;
                    changed = true;
                }
            } else if (axis === 'zMax') {
                const newSize = Math.max(1, platform.sizeZ + wtDelta);
                changed = newSize !== platform.sizeZ;
                platform.sizeZ = newSize;
            } else if (axis === 'zMin') {
                const newSize = Math.max(1, platform.sizeZ + wtDelta);
                if (newSize !== platform.sizeZ) {
                    platform.z -= (newSize - platform.sizeZ);
                    platform.sizeZ = newSize;
                    changed = true;
                }
            }
        }

        return changed;
    }

    // Confirm the drag
    endDrag() {
        this.drag = null;
    }

    // Cancel drag, restore original values
    cancelDrag() {
        if (!this.drag) return;
        const { platform, origX, origY, origZ, origSizeX, origSizeZ } = this.drag;
        platform.x = origX;
        platform.y = origY;
        platform.z = origZ;
        if (origSizeX != null) platform.sizeX = origSizeX;
        if (origSizeZ != null) platform.sizeZ = origSizeZ;
        this.drag = null;
    }

    // Get the original values for undo (call before startDrag)
    getOriginalState(target) {
        const s = { x: target.x, y: target.y, z: target.z };
        if (target.sizeX != null) { s.sizeX = target.sizeX; s.sizeZ = target.sizeZ; }
        return s;
    }

    dispose() {
        this.scene.remove(this.group);
        for (const part of this.allParts) {
            part.geometry.dispose();
            part.material.dispose();
        }
    }
}
