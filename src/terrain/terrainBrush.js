// Terrain sculpting brushes — raise/lower, noise, smooth, flatten
// Operates directly on TerrainMap vertex heights

import { noise2D } from './noise.js';

/**
 * Apply a brush stroke to terrain vertices.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @param {number} cx - brush center X in WT units
 * @param {number} cz - brush center Z in WT units
 * @param {object} brush - { type, radius, strength, noiseScale, noiseAmp }
 * @param {number} dt - frame delta time
 * @param {boolean} invert - if true, invert the brush effect (e.g. lower instead of raise)
 */
export function applyBrush(terrain, cx, cz, brush, dt, invert = false) {
    const { type, radius, strength } = brush;
    const radiusSq = radius * radius;
    const sign = invert ? -1 : 1;

    // Find flatten target height (height at brush center)
    let flattenTarget = 0;
    if (type === 'flatten') {
        flattenTarget = terrain.getHeightAt(cx, cz);
    }

    for (let i = 0; i < terrain.vertices.length; i++) {
        const v = terrain.vertices[i];
        const dx = v.x - cx;
        const dz = v.z - cz;
        const dSq = dx * dx + dz * dz;

        if (dSq > radiusSq) continue;

        // Smooth falloff (cosine)
        const dist = Math.sqrt(dSq);
        const falloff = 0.5 * (1 + Math.cos(Math.PI * dist / radius));
        const amount = falloff * strength * dt * 60; // normalize to ~60fps

        switch (type) {
            case 'raise':
                v.y += sign * amount * 2;
                break;

            case 'noise': {
                const n = noise2D(v.x * brush.noiseScale, v.z * brush.noiseScale);
                v.y += sign * n * brush.noiseAmp * falloff * dt * 60 * strength;
                break;
            }

            case 'smooth': {
                // Average with nearby vertices
                const avg = getNeighborAvgHeight(terrain, i, radius * 0.5);
                v.y += (avg - v.y) * amount * 0.5;
                break;
            }

            case 'flatten': {
                const diff = flattenTarget - v.y;
                v.y += diff * amount * 0.3;
                break;
            }
        }
    }
}

/**
 * Get the average height of vertices near a given vertex.
 */
function getNeighborAvgHeight(terrain, vertexIdx, radius) {
    const v = terrain.vertices[vertexIdx];
    const radiusSq = radius * radius;
    let sum = 0;
    let count = 0;

    for (let i = 0; i < terrain.vertices.length; i++) {
        if (i === vertexIdx) continue;
        const other = terrain.vertices[i];
        const dx = other.x - v.x;
        const dz = other.z - v.z;
        if (dx * dx + dz * dz <= radiusSq) {
            sum += other.y;
            count++;
        }
    }

    return count > 0 ? sum / count : v.y;
}

/**
 * Snapshot terrain vertex heights for undo.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @returns {number[]} array of Y values
 */
export function snapshotHeights(terrain) {
    return terrain.vertices.map(v => v.y);
}

/**
 * Restore terrain vertex heights from a snapshot.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @param {number[]} heights
 */
export function restoreHeights(terrain, heights) {
    for (let i = 0; i < terrain.vertices.length && i < heights.length; i++) {
        terrain.vertices[i].y = heights[i];
    }
}

/**
 * Find vertices within a brush radius.
 * @returns {Array<{index: number, falloff: number}>}
 */
export function getVerticesInBrush(terrain, cx, cz, radius) {
    const radiusSq = radius * radius;
    const result = [];

    for (let i = 0; i < terrain.vertices.length; i++) {
        const v = terrain.vertices[i];
        const dx = v.x - cx;
        const dz = v.z - cz;
        const dSq = dx * dx + dz * dz;

        if (dSq <= radiusSq) {
            const dist = Math.sqrt(dSq);
            const falloff = 0.5 * (1 + Math.cos(Math.PI * dist / radius));
            result.push({ index: i, falloff });
        }
    }

    return result;
}
