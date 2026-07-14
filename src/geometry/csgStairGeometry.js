// CSG stair geometry — procedural treads/risers/sides for a confirmed CSG stair.
// Uses PlatformGeometryBuilder to emit quads, same pattern as buildBoxStairGeometry
// but anchored to a CSG wall face instead of platform edges.

import { PlatformGeometryBuilder } from './platformGeometry.js';

/**
 * Build stair-step geometry (treads, risers, side walls) for a CSG stair descriptor.
 * The void brush carves the tunnel; this mesh fills in the visible step surfaces.
 *
 * @param {object} desc - CSG stair descriptor from state.csg.csgStairs[]
 * @param {object} options - { viewMode: 'grid'|'textured' }
 * @returns {THREE.BufferGeometry}
 */
export function buildCsgStairGeometry(desc, options = {}) {
    const { axis, side, facePos, selU0, selU1, floor, H, direction, stepCount } = desc;
    const builder = new PlatformGeometryBuilder();
    const dir = side === 'max' ? 1 : -1;
    const textured = options.viewMode === 'textured';
    const treadZone = textured ? 0 : 0;
    const riserZone = textured ? 5 : 0;
    const sideZone  = textured ? 3 : 0;

    const stepWidth = selU1 - selU0;

    // Helper: convert (normalPos, y, uPos) to [x, y, z] based on wall axis.
    function tw(normalPos, y, uPos) {
        if (axis === 'x') return [normalPos, y, uPos];
        return [uPos, y, normalPos];
    }

    // Precompute flip values based on axis (not dir). The tw() mapping
    // produces different vertex windings for axis='x' vs 'z', so flip
    // depends on axis. Only the riser also depends on dir (normal direction).
    const treadFlip = axis === 'x';                       // +Y (upward)
    const ceilFlip  = axis === 'z';                       // -Y (downward)
    const sideFlip  = axis === 'x';                       // outward from sides
    const riserFlip = (axis === 'x') !== (dir > 0);       // toward room/facePos

    for (let k = 0; k < stepCount; k++) {
        // Normal-axis span for this step (1 WT deep each)
        let nLo, nHi;
        if (dir === 1) {
            nLo = facePos + k;
            nHi = facePos + k + 1;
        } else {
            nLo = facePos - (k + 1);
            nHi = facePos - k;
        }

        // Vertical span for this step
        let stepFloor, stepTop;
        if (direction === 'down') {
            stepFloor = floor - (k + 1);
            stepTop = floor - k;
        } else {
            stepFloor = floor + k;
            stepTop = floor + (k + 1);
        }

        // Tread — horizontal top surface of this step
        builder.addQuad(
            tw(nLo, stepTop, selU0),
            tw(nHi, stepTop, selU0),
            tw(nHi, stepTop, selU1),
            tw(nLo, stepTop, selU1),
            treadFlip, treadZone,
            ...(textured ? [[0, 0], [1, 0], [1, stepWidth], [0, stepWidth]] : []),
        );

        // Riser — vertical face at the boundary between adjacent steps,
        // facing toward the room (toward facePos).
        const riserH = stepTop - stepFloor;

        if (direction === 'down') {
            // Down: riser at the front edge of this step (away from wall)
            const riserPos = dir === 1 ? nHi : nLo;
            const riserU = stepWidth / riserH;
            builder.addQuad(
                tw(riserPos, stepFloor, selU0),
                tw(riserPos, stepFloor, selU1),
                tw(riserPos, stepTop,   selU1),
                tw(riserPos, stepTop,   selU0),
                riserFlip, riserZone,
                ...(textured ? [[0, 0], [riserU, 0], [riserU, 1], [0, 1]] : []),
            );
        } else if (direction === 'up') {
            // Up: riser at the front edge of this step (away from wall)
            const riserPos = dir === 1 ? nLo : nHi;
            const riserU = stepWidth / riserH;
            builder.addQuad(
                tw(riserPos, stepFloor, selU0),
                tw(riserPos, stepFloor, selU1),
                tw(riserPos, stepTop,   selU1),
                tw(riserPos, stepTop,   selU0),
                riserFlip, riserZone,
                ...(textured ? [[0, 0], [riserU, 0], [riserU, 1], [0, 1]] : []),
            );
        }

        // Left side wall (selU0 edge)
        builder.addQuad(
            tw(nLo, stepFloor, selU0),
            tw(nHi, stepFloor, selU0),
            tw(nHi, stepTop,   selU0),
            tw(nLo, stepTop,   selU0),
            sideFlip, sideZone,
            ...(textured ? [[0, 0], [1, 0], [1, riserH], [0, riserH]] : []),
        );

        // Right side wall (selU1 edge)
        builder.addQuad(
            tw(nHi, stepFloor, selU1),
            tw(nLo, stepFloor, selU1),
            tw(nLo, stepTop,   selU1),
            tw(nHi, stepTop,   selU1),
            sideFlip, sideZone,
            ...(textured ? [[0, 0], [1, 0], [1, riserH], [0, riserH]] : []),
        );
    }

    // ── Ceiling/floor fill at the far end ───────────────────────────────

    if (direction === 'down' && stepCount > 0) {
        // Last step column bounds
        let lastNLo, lastNHi;
        if (dir === 1) {
            lastNLo = facePos + (stepCount - 1);
            lastNHi = facePos + stepCount;
        } else {
            lastNLo = facePos - stepCount;
            lastNHi = facePos - (stepCount - 1);
        }

        const ceilDrop = H - stepCount;

        // Horizontal ceiling panel at the far-end column (faces downward)
        builder.addQuad(
            tw(lastNLo, ceilDrop, selU0),
            tw(lastNHi, ceilDrop, selU0),
            tw(lastNHi, ceilDrop, selU1),
            tw(lastNLo, ceilDrop, selU1),
            ceilFlip, sideZone,
            ...(textured ? [[0, 0], [1, 0], [1, stepWidth], [0, stepWidth]] : []),
        );

        // Vertical wall dropping from H to H-stepCount (faces toward room)
        const ceilWallPos = dir === 1 ? lastNLo : lastNHi;
        const dropH = H - ceilDrop;
        builder.addQuad(
            tw(ceilWallPos, ceilDrop, selU0),
            tw(ceilWallPos, ceilDrop, selU1),
            tw(ceilWallPos, H,        selU1),
            tw(ceilWallPos, H,        selU0),
            riserFlip, sideZone,
            ...(textured ? [[0, 0], [stepWidth, 0], [stepWidth, dropH], [0, dropH]] : []),
        );
    }

    if (direction === 'up' && stepCount > 0) {
        // Fill the stepped floor underneath the stairs (faces upward)
        for (let k = 0; k < stepCount - 1; k++) {
            let fillNLo, fillNHi;
            if (dir === 1) {
                fillNLo = facePos + k;
                fillNHi = facePos + k + 1;
            } else {
                fillNLo = facePos - (k + 1);
                fillNHi = facePos - k;
            }
            const fillY = floor + (k + 1);
            builder.addQuad(
                tw(fillNLo, fillY, selU0),
                tw(fillNHi, fillY, selU0),
                tw(fillNHi, fillY, selU1),
                tw(fillNLo, fillY, selU1),
                treadFlip, sideZone,
                ...(textured ? [[0, 0], [1, 0], [1, stepWidth], [0, stepWidth]] : []),
            );
        }
    }

    return builder.build();
}
