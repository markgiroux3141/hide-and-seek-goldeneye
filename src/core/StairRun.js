// StairRun — a flight of stairs connecting two platforms (or ground to platform)
// Anchors to specific edges of platforms. Ground connections use {x, z} position.

import { Platform } from './Platform.js';

export class StairRun {
    constructor(id, fromPlatformId, toPlatformId, anchorFrom, anchorTo, width, stepHeight, riseOverRun = 1) {
        this.id = id;
        this.fromPlatformId = fromPlatformId;   // number | null (null = ground)
        this.toPlatformId = toPlatformId;       // number | null (null = ground)
        this.anchorFrom = anchorFrom;           // { edge, offset } for platform, or { x, z } for ground
        this.anchorTo = anchorTo;               // { edge, offset } for platform, or { x, z } for ground
        this.width = width;                     // perpendicular width in WT units
        this.stepHeight = stepHeight;           // height per step in WT units
        this.riseOverRun = riseOverRun;         // rise/run ratio (1 = 45 deg)
        this.grounded = false;                  // when true, side walls extend to Y=0
        this.railings = false;                  // when true, adds railings to sides
        this.style = 'default';                 // visual style key — see src/geometry/platformStyles.js
    }

    // Resolve the world-space attachment point for one end of the stair run.
    // Returns { x, y, z, runAxis, runSign } in WT units.
    // platform: the Platform object (or null for ground).
    // anchor: the anchor spec for this end.
    static resolveAnchor(platform, anchor) {
        if (!platform) {
            // Ground connection — anchor is { x, z } or { x, y, z }
            return { x: anchor.x, y: anchor.y ?? 0, z: anchor.z };
        }

        // Platform connection — anchor is { edge, offset }
        // The attachment point is at the midpoint of the stair width along the edge,
        // offset from the edge start. Since we auto-center, the midpoint of the edge is used.
        const mid = platform.getEdgeMidpoint(anchor.edge);
        return { x: mid.x, y: platform.y, z: mid.z };
    }

    // Compute the run axis and direction from two resolved anchor points and edge info.
    // Returns { runAxis: 'x'|'z', runSign: 1|-1 }
    static computeRunInfo(fromAnchor, toAnchor, fromPlatform, toPlatform) {
        const fromPt = StairRun.resolveAnchor(fromPlatform, fromAnchor);
        const toPt = StairRun.resolveAnchor(toPlatform, toAnchor);

        // Run axis is determined by the edge normal of the platform anchor,
        // or by the dominant axis between the two points for ground connections.
        if (fromPlatform && fromAnchor.edge) {
            const normal = Platform.edgeNormal(fromAnchor.edge);
            return {
                runAxis: normal.x !== 0 ? 'x' : 'z',
                runSign: normal.x !== 0 ? normal.x : normal.z,
            };
        }
        if (toPlatform && toAnchor.edge) {
            const normal = Platform.edgeNormal(toAnchor.edge);
            // Reverse direction: stairs go from 'from' toward 'to'
            return {
                runAxis: normal.x !== 0 ? 'x' : 'z',
                runSign: normal.x !== 0 ? -normal.x : -normal.z,
            };
        }

        // Both ground — use dominant axis
        const dx = Math.abs(toPt.x - fromPt.x);
        const dz = Math.abs(toPt.z - fromPt.z);
        const runAxis = dx >= dz ? 'x' : 'z';
        const runSign = (runAxis === 'x' ? toPt.x - fromPt.x : toPt.z - fromPt.z) >= 0 ? 1 : -1;
        return { runAxis, runSign };
    }

    toJSON() {
        return {
            id: this.id,
            fromPlatformId: this.fromPlatformId,
            toPlatformId: this.toPlatformId,
            anchorFrom: this.anchorFrom,
            anchorTo: this.anchorTo,
            width: this.width,
            stepHeight: this.stepHeight,
            riseOverRun: this.riseOverRun,
            grounded: this.grounded,
            railings: this.railings,
            style: this.style,
        };
    }

    static fromJSON(j) {
        const r = new StairRun(
            j.id, j.fromPlatformId, j.toPlatformId,
            j.anchorFrom, j.anchorTo,
            j.width, j.stepHeight, j.riseOverRun ?? 1,
        );
        r.grounded = j.grounded ?? false;
        r.railings = j.railings ?? false;
        r.style = j.style ?? 'default';
        return r;
    }
}

