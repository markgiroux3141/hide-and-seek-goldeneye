// Platform edge utilities — pure geometric computations for edge detection and projection

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { Platform } from '../core/Platform.js';

// Find closest platform edge to a world-space point
export function closestPlatformEdge(platform, worldPoint) {
    const W = WORLD_SCALE;
    const px = worldPoint.x / W;
    const pz = worldPoint.z / W;

    const edges = ['xMin', 'xMax', 'zMin', 'zMax'];
    let bestEdge = null;
    let bestDist = Infinity;

    for (const edge of edges) {
        const line = platform.getEdgeLine(edge);
        const dist = distToSegment2D(px, pz, line.start.x, line.start.z, line.end.x, line.end.z);
        if (dist < bestDist) {
            bestDist = dist;
            bestEdge = edge;
        }
    }
    return bestEdge;
}

export function distToSegment2D(px, pz, ax, az, bx, bz) {
    const dx = bx - ax, dz = bz - az;
    const lenSq = dx * dx + dz * dz;
    if (lenSq === 0) return Math.hypot(px - ax, pz - az);
    let t = ((px - ax) * dx + (pz - az) * dz) / lenSq;
    t = Math.max(0, Math.min(1, t));
    return Math.hypot(px - (ax + t * dx), pz - (az + t * dz));
}

// Find the offset (0..1) on a platform edge closest to a world-space point (in WT coords)
export function closestOffsetOnEdge(platform, edge, wtPoint) {
    const line = platform.getEdgeLine(edge);
    const ex = line.end.x - line.start.x;
    const ez = line.end.z - line.start.z;
    const lenSq = ex * ex + ez * ez;
    if (lenSq === 0) return 0.5;
    const t = ((wtPoint.x - line.start.x) * ex + (wtPoint.z - line.start.z) * ez) / lenSq;
    const edgeLen = platform.getEdgeLength(edge);
    const wtPos = Math.round(Math.max(0, Math.min(1, t)) * edgeLen);
    return Math.max(0, Math.min(edgeLen, wtPos)) / edgeLen;
}

// Project the crosshair ray onto a platform edge, returning offset 0..1
export function projectCrosshairOntoEdge(platform, edge, camera) {
    const plane = new THREE.Plane(new THREE.Vector3(0, 1, 0), -platform.y * WORLD_SCALE);
    const raycaster = new THREE.Raycaster();
    raycaster.setFromCamera(new THREE.Vector2(0, 0), camera);
    const intersect = new THREE.Vector3();
    if (!raycaster.ray.intersectPlane(plane, intersect)) return 0.5;

    const px = intersect.x / WORLD_SCALE;
    const pz = intersect.z / WORLD_SCALE;

    const line = platform.getEdgeLine(edge);
    const ex = line.end.x - line.start.x;
    const ez = line.end.z - line.start.z;
    const lenSq = ex * ex + ez * ez;
    if (lenSq === 0) return 0.5;
    const t = ((px - line.start.x) * ex + (pz - line.start.z) * ez) / lenSq;
    const edgeLen = platform.getEdgeLength(edge);
    const wtPos = Math.round(Math.max(0, Math.min(1, t)) * edgeLen);
    return Math.max(0, Math.min(edgeLen, wtPos)) / edgeLen;
}

// Pick the source edge whose outward normal best aligns with a direction (XZ plane)
export function bestEdgeForDirection(platform, direction) {
    const edges = ['xMin', 'xMax', 'zMin', 'zMax'];
    let best = null, bestDot = -Infinity;
    for (const edge of edges) {
        const normal = Platform.edgeNormal(edge);
        const dot = normal.x * direction.x + normal.z * direction.z;
        if (dot > bestDot) { bestDot = dot; best = edge; }
    }
    return best;
}
