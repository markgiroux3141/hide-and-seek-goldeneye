// regions — spatial relationships and clustering for CSG brushes.
//
// Two operations live here:
//   1. clusterBrushes(brushes) — group brushes into connected regions for
//      independent CSG evaluation. Each region gets its own CSGRegion instance.
//   2. findRoomBrushes(startBrush, brushes) — flood-fill within a room,
//      stopping at door/hole frames. Used for retexturing a connected room.
//
// Both share `brushesTouching(a, b)`, ported from spike/csg/main.js:510.

import { CSGRegion } from './CSGRegion.js';

// Two brushes touch if they overlap on 2 axes and are adjacent on the third.
// (Spike line 510.)
export function brushesTouching(a, b) {
    const axes = ['x', 'y', 'z'];
    const dims = ['w', 'h', 'd'];
    for (let i = 0; i < 3; i++) {
        const aMin = a[axes[i]], aMax = a[axes[i]] + a[dims[i]];
        const bMin = b[axes[i]], bMax = b[axes[i]] + b[dims[i]];
        if (aMax === bMin || bMax === aMin) {
            // Adjacent on this axis — check overlap on other two
            let overlap = true;
            for (let j = 0; j < 3; j++) {
                if (j === i) continue;
                const a0 = a[axes[j]], a1 = a[axes[j]] + a[dims[j]];
                const b0 = b[axes[j]], b1 = b[axes[j]] + b[dims[j]];
                if (a1 <= b0 || b1 <= a0) { overlap = false; break; }
            }
            if (overlap) return true;
        }
    }
    return false;
}

// Two brushes belong to the same region if they overlap *or* share a face.
// Used for region clustering — note this is more permissive than brushesTouching
// because additive brushes inside a room may overlap subtractive room brushes.
export function brushesOverlapOrTouch(a, b) {
    const axes = ['x', 'y', 'z'];
    const dims = ['w', 'h', 'd'];
    // Check for any overlap (inclusive of edge contact)
    for (let i = 0; i < 3; i++) {
        const a0 = a[axes[i]], a1 = a[axes[i]] + a[dims[i]];
        const b0 = b[axes[i]], b1 = b[axes[i]] + b[dims[i]];
        if (a1 < b0 || b1 < a0) return false;
    }
    return true;
}

// Group an array of brushes into connected regions. Brushes that overlap or
// touch (including doorframes that bridge two rooms) end up in the same region.
// A doorframe brush bridges its two rooms by sitting in both — clustering
// follows the contact graph naturally without special-casing the frame flag.
//
// Returns: Array<CSGRegion>, each populated with its brush subset and an
// auto-resized shell.
export function clusterBrushes(brushes) {
    const regions = [];
    const visited = new Set();
    let nextRegionId = 1;

    for (const start of brushes) {
        if (visited.has(start.id)) continue;

        const region = new CSGRegion(nextRegionId++);
        const queue = [start];
        visited.add(start.id);
        region.brushes.push(start);

        while (queue.length > 0) {
            const current = queue.pop();
            for (const other of brushes) {
                if (visited.has(other.id)) continue;
                if (brushesOverlapOrTouch(current, other)) {
                    visited.add(other.id);
                    region.brushes.push(other);
                    queue.push(other);
                }
            }
        }

        region.updateShell();
        regions.push(region);
    }

    return regions;
}

// Flood fill from a brush through touching subtractive brushes within the same
// room, stopping at doorframe/holeframe/stair-step brushes. Returns a Set of
// brush IDs. Used by retextureRoom to apply a scheme consistently across a
// connected room section. Stair steps act as section boundaries so each
// corridor segment retains its own wall-trim height.
// (Spike line 533.)
export function findRoomBrushes(startBrush, brushes) {
    const room = new Set();
    const queue = [startBrush];
    room.add(startBrush.id);

    while (queue.length > 0) {
        const current = queue.pop();
        for (const other of brushes) {
            if (room.has(other.id)) continue;
            if (other.op !== 'subtract') continue;
            if (other.isDoorframe || other.isHoleFrame) continue;
            if (other.isStairStep) continue;
            if (brushesTouching(current, other)) {
                room.add(other.id);
                queue.push(other);
            }
        }
    }
    return room;
}
