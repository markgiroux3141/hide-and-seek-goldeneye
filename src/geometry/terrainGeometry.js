// Terrain mesh builder — creates Three.js BufferGeometry from TerrainMap data

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';

/**
 * Build a Three.js BufferGeometry from a TerrainMap's vertices and triangles.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @returns {THREE.BufferGeometry}
 */
export function buildTerrainGeometry(terrain) {
    const { vertices, triangles } = terrain;
    if (vertices.length === 0 || triangles.length === 0) return new THREE.BufferGeometry();

    const W = WORLD_SCALE;
    const posCount = triangles.length * 3;
    const positions = new Float32Array(posCount * 3);
    const normals = new Float32Array(posCount * 3);
    const uvs = new Float32Array(posCount * 2);
    const colors = new Float32Array(posCount * 3);

    for (let i = 0; i < triangles.length; i++) {
        const tri = triangles[i];
        const a = vertices[tri.a];
        const b = vertices[tri.b];
        const c = vertices[tri.c];

        const base = i * 9; // 3 verts * 3 components
        const uvBase = i * 6; // 3 verts * 2 components

        // Positions (scaled to world coords)
        positions[base]     = a.x * W;
        positions[base + 1] = a.y * W;
        positions[base + 2] = a.z * W;
        positions[base + 3] = b.x * W;
        positions[base + 4] = b.y * W;
        positions[base + 5] = b.z * W;
        positions[base + 6] = c.x * W;
        positions[base + 7] = c.y * W;
        positions[base + 8] = c.z * W;

        // Compute face normal
        const abx = b.x - a.x, aby = b.y - a.y, abz = b.z - a.z;
        const acx = c.x - a.x, acy = c.y - a.y, acz = c.z - a.z;
        let nx = aby * acz - abz * acy;
        let ny = abz * acx - abx * acz;
        let nz = abx * acy - aby * acx;
        const len = Math.sqrt(nx * nx + ny * ny + nz * nz);
        if (len > 1e-8) { nx /= len; ny /= len; nz /= len; }
        else { nx = 0; ny = 1; nz = 0; }

        // All three vertices get the face normal
        for (let v = 0; v < 3; v++) {
            normals[base + v * 3]     = nx;
            normals[base + v * 3 + 1] = ny;
            normals[base + v * 3 + 2] = nz;
        }

        // UVs — world-space tiling (1 WT = 1 UV unit for repeating textures)
        const UV_SCALE = 0.25;
        uvs[uvBase]     = a.x * UV_SCALE;
        uvs[uvBase + 1] = a.z * UV_SCALE;
        uvs[uvBase + 2] = b.x * UV_SCALE;
        uvs[uvBase + 3] = b.z * UV_SCALE;
        uvs[uvBase + 4] = c.x * UV_SCALE;
        uvs[uvBase + 5] = c.z * UV_SCALE;

        // Vertex colors — greenish base, varies by height
        for (let v = 0; v < 3; v++) {
            const vert = [a, b, c][v];
            const heightFactor = Math.max(0, Math.min(1, vert.y / 40));
            colors[base + v * 3]     = 0.2 + heightFactor * 0.3;  // R
            colors[base + v * 3 + 1] = 0.5 - heightFactor * 0.2;  // G
            colors[base + v * 3 + 2] = 0.15 + heightFactor * 0.1; // B
        }
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geometry.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
    geometry.setAttribute('uv', new THREE.BufferAttribute(uvs, 2));
    geometry.setAttribute('color', new THREE.BufferAttribute(colors, 3));

    return geometry;
}

/**
 * Build smooth per-vertex normals for terrain.
 * Call after sculpting to update normals based on adjacent face normals.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @param {THREE.BufferGeometry} geometry
 */
export function updateTerrainNormals(terrain, geometry) {
    const { vertices, triangles } = terrain;
    const W = WORLD_SCALE;

    // Accumulate face normals per vertex
    const vertexNormals = new Array(vertices.length);
    for (let i = 0; i < vertices.length; i++) vertexNormals[i] = { x: 0, y: 0, z: 0 };

    for (const tri of triangles) {
        const a = vertices[tri.a];
        const b = vertices[tri.b];
        const c = vertices[tri.c];

        const abx = b.x - a.x, aby = b.y - a.y, abz = b.z - a.z;
        const acx = c.x - a.x, acy = c.y - a.y, acz = c.z - a.z;
        let nx = aby * acz - abz * acy;
        let ny = abz * acx - abx * acz;
        let nz = abx * acy - aby * acx;

        vertexNormals[tri.a].x += nx; vertexNormals[tri.a].y += ny; vertexNormals[tri.a].z += nz;
        vertexNormals[tri.b].x += nx; vertexNormals[tri.b].y += ny; vertexNormals[tri.b].z += nz;
        vertexNormals[tri.c].x += nx; vertexNormals[tri.c].y += ny; vertexNormals[tri.c].z += nz;
    }

    // Normalize
    for (const n of vertexNormals) {
        const len = Math.sqrt(n.x * n.x + n.y * n.y + n.z * n.z);
        if (len > 1e-8) { n.x /= len; n.y /= len; n.z /= len; }
        else { n.x = 0; n.y = 1; n.z = 0; }
    }

    // Write back to geometry
    const posAttr = geometry.getAttribute('position');
    const normAttr = geometry.getAttribute('normal');
    const colorAttr = geometry.getAttribute('color');

    for (let i = 0; i < triangles.length; i++) {
        const tri = triangles[i];
        const indices = [tri.a, tri.b, tri.c];
        for (let v = 0; v < 3; v++) {
            const vi = indices[v];
            const vert = vertices[vi];
            const n = vertexNormals[vi];
            const idx = i * 3 + v;

            posAttr.setXYZ(idx, vert.x * W, vert.y * W, vert.z * W);
            normAttr.setXYZ(idx, n.x, n.y, n.z);

            // Update vertex colors based on height
            const heightFactor = Math.max(0, Math.min(1, vert.y / 40));
            colorAttr.setXYZ(idx,
                0.2 + heightFactor * 0.3,
                0.5 - heightFactor * 0.2,
                0.15 + heightFactor * 0.1
            );
        }
    }

    posAttr.needsUpdate = true;
    normAttr.needsUpdate = true;
    colorAttr.needsUpdate = true;
    geometry.computeBoundingSphere();
}

/**
 * Build line segments showing the terrain boundary in the top-down view.
 * @param {Array<{x: number, z: number}>} boundary
 * @param {boolean} closed - Whether the polygon is closed
 * @returns {Float32Array} positions for LineSegments
 */
export function buildBoundaryLines(boundary, closed = false) {
    if (boundary.length < 2) return new Float32Array(0);

    const W = WORLD_SCALE;
    const segCount = closed ? boundary.length : boundary.length - 1;
    const positions = new Float32Array(segCount * 6); // 2 points * 3 coords per segment

    for (let i = 0; i < segCount; i++) {
        const a = boundary[i];
        const b = boundary[(i + 1) % boundary.length];
        const base = i * 6;
        positions[base]     = a.x * W;
        positions[base + 1] = 0.1; // slightly above ground
        positions[base + 2] = a.z * W;
        positions[base + 3] = b.x * W;
        positions[base + 4] = 0.1;
        positions[base + 5] = b.z * W;
    }

    return positions;
}

/**
 * Build vertex markers (small crosses) for terrain boundary vertices.
 * @param {Array<{x: number, z: number}>} vertices
 * @param {number} size - marker size in WT units
 * @returns {Float32Array} positions for LineSegments
 */
export function buildVertexMarkers(vertices, size = 0.5) {
    const W = WORLD_SCALE;
    const positions = new Float32Array(vertices.length * 12); // 2 lines * 2 points * 3 coords

    for (let i = 0; i < vertices.length; i++) {
        const v = vertices[i];
        const base = i * 12;
        const s = size * W;
        const y = 0.15;

        // Horizontal line
        positions[base]     = v.x * W - s;
        positions[base + 1] = y;
        positions[base + 2] = v.z * W;
        positions[base + 3] = v.x * W + s;
        positions[base + 4] = y;
        positions[base + 5] = v.z * W;

        // Vertical line
        positions[base + 6]  = v.x * W;
        positions[base + 7]  = y;
        positions[base + 8]  = v.z * W - s;
        positions[base + 9]  = v.x * W;
        positions[base + 10] = y;
        positions[base + 11] = v.z * W + s;
    }

    return positions;
}

/**
 * Build a circle indicator for the brush, draped on terrain surface.
 * @param {number} cx - center X in WT units
 * @param {number} cy - center Y in WT units (fallback height)
 * @param {number} cz - center Z in WT units
 * @param {number} radius - radius in WT units
 * @param {import('../core/TerrainMap.js').TerrainMap} [terrain] - if provided, circle follows terrain height
 * @param {number} segments - number of line segments
 * @returns {Float32Array} positions for LineSegments
 */
export function buildBrushCircle(cx, cy, cz, radius, terrain = null, segments = 32) {
    const W = WORLD_SCALE;
    const positions = new Float32Array(segments * 6);

    function getY(px, pz) {
        if (terrain) {
            const h = terrain.getHeightAt(px, pz);
            return (h + 0.3) * W; // slight offset above surface
        }
        return (cy + 0.5) * W;
    }

    for (let i = 0; i < segments; i++) {
        const a1 = (i / segments) * Math.PI * 2;
        const a2 = ((i + 1) / segments) * Math.PI * 2;
        const base = i * 6;

        const x1 = cx + Math.cos(a1) * radius;
        const z1 = cz + Math.sin(a1) * radius;
        const x2 = cx + Math.cos(a2) * radius;
        const z2 = cz + Math.sin(a2) * radius;

        positions[base]     = x1 * W;
        positions[base + 1] = getY(x1, z1);
        positions[base + 2] = z1 * W;
        positions[base + 3] = x2 * W;
        positions[base + 4] = getY(x2, z2);
        positions[base + 5] = z2 * W;
    }

    return positions;
}
