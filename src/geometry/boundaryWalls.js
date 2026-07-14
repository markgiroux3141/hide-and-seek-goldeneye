// Boundary wall generation — plane walls and rocky walls
// Wraps the terrain boundary (and hole boundaries) with vertical barrier meshes

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { noise2D } from '../terrain/noise.js';

/**
 * Build a simple plane wall along boundary edges.
 * One vertical quad per edge segment, all connected.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @returns {THREE.BufferGeometry}
 */
export function buildPlaneWallGeometry(terrain) {
    if (!terrain.hasMesh || terrain.boundary.length < 3) return new THREE.BufferGeometry();

    const W = WORLD_SCALE;
    const wallHeight = terrain.wallHeight;
    // Match mid-edge subdivision: each pass doubles edge segments
    const subdivPasses = Math.max(0, terrain.subdivisionLevel - 4);
    const segments = Math.pow(2, subdivPasses);

    // Collect all boundary loops (outer boundary + hole boundaries)
    const loops = [terrain.boundary, ...terrain.holes];
    const allQuads = [];

    for (const loop of loops) {
        for (let i = 0; i < loop.length; i++) {
            const a = loop[i];
            const b = loop[(i + 1) % loop.length];

            // Subdivide this edge to match terrain mesh density
            for (let s = 0; s < segments; s++) {
                const t0 = s / segments;
                const t1 = (s + 1) / segments;
                const x0 = a.x + (b.x - a.x) * t0, z0 = a.z + (b.z - a.z) * t0;
                const x1 = a.x + (b.x - a.x) * t1, z1 = a.z + (b.z - a.z) * t1;
                const h0 = terrain.getHeightAt(x0, z0);
                const h1 = terrain.getHeightAt(x1, z1);

                allQuads.push({
                    bl: { x: x0 * W, y: h0 * W, z: z0 * W },
                    br: { x: x1 * W, y: h1 * W, z: z1 * W },
                    tr: { x: x1 * W, y: (h1 + wallHeight) * W, z: z1 * W },
                    tl: { x: x0 * W, y: (h0 + wallHeight) * W, z: z0 * W },
                });
            }
        }
    }

    const vertCount = allQuads.length * 6; // 2 triangles per quad
    const positions = new Float32Array(vertCount * 3);
    const normals = new Float32Array(vertCount * 3);
    const uvs = new Float32Array(vertCount * 2);
    const colors = new Float32Array(vertCount * 3);

    let vi = 0;
    for (const q of allQuads) {
        // Compute face normal (from first triangle)
        const e1x = q.br.x - q.bl.x, e1y = q.br.y - q.bl.y, e1z = q.br.z - q.bl.z;
        const e2x = q.tl.x - q.bl.x, e2y = q.tl.y - q.bl.y, e2z = q.tl.z - q.bl.z;
        let nx = e1y * e2z - e1z * e2y;
        let ny = e1z * e2x - e1x * e2z;
        let nz = e1x * e2y - e1y * e2x;
        const len = Math.sqrt(nx * nx + ny * ny + nz * nz);
        if (len > 1e-8) { nx /= len; ny /= len; nz /= len; }

        // Triangle 1: bl, br, tl
        const verts1 = [q.bl, q.br, q.tl];
        // Triangle 2: br, tr, tl
        const verts2 = [q.br, q.tr, q.tl];

        for (const verts of [verts1, verts2]) {
            for (const v of verts) {
                positions[vi * 3] = v.x;
                positions[vi * 3 + 1] = v.y;
                positions[vi * 3 + 2] = v.z;
                normals[vi * 3] = nx;
                normals[vi * 3 + 1] = ny;
                normals[vi * 3 + 2] = nz;
                uvs[vi * 2] = v.x / W * 0.25;
                uvs[vi * 2 + 1] = v.y / W * 0.25;
                // Dark green color for tree wall
                colors[vi * 3] = 0.1;
                colors[vi * 3 + 1] = 0.25;
                colors[vi * 3 + 2] = 0.05;
                vi++;
            }
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
 * Build a rocky wall with noised surface along boundary edges.
 * Subdivides each wall segment vertically and applies noise displacement.
 * @param {import('../core/TerrainMap.js').TerrainMap} terrain
 * @returns {THREE.BufferGeometry}
 */
export function buildRockyWallGeometry(terrain) {
    if (!terrain.hasMesh || terrain.boundary.length < 3) return new THREE.BufferGeometry();

    const W = WORLD_SCALE;
    const wallHeight = terrain.rockyWallHeight;
    const rows = terrain.wallSubdivRows;
    const noiseFreq = terrain.wallNoiseFreq;
    const noiseAmp = terrain.wallNoiseAmp;

    // Match mid-edge subdivision: each pass doubles edge segments
    const subdivPasses = Math.max(0, terrain.subdivisionLevel - 4);
    const edgeSegments = Math.pow(2, subdivPasses);

    const loops = [terrain.boundary, ...terrain.holes];
    const allTris = []; // {p0, p1, p2, nx, ny, nz} for each triangle

    for (const loop of loops) {
        for (let i = 0; i < loop.length; i++) {
            const a = loop[i];
            const b = loop[(i + 1) % loop.length];

            // Compute outward normal for this edge (in XZ plane)
            const edx = b.x - a.x, edz = b.z - a.z;
            const edLen = Math.sqrt(edx * edx + edz * edz);
            if (edLen < 1e-6) continue;
            const onx = -edz / edLen, onz = edx / edLen;

            // Create a grid: (edgeSegments+1) across, (rows+1) up
            const cols = edgeSegments;
            const gridW = cols + 1;
            const gridH = rows + 1;
            const grid = [];

            for (let r = 0; r < gridH; r++) {
                const t = r / rows; // 0 = bottom, 1 = top
                for (let c = 0; c < gridW; c++) {
                    const s = c / cols;
                    const baseX = a.x + (b.x - a.x) * s;
                    const baseZ = a.z + (b.z - a.z) * s;
                    const terrainH = terrain.getHeightAt(baseX, baseZ);
                    const baseY = terrainH + wallHeight * t;

                    // No displacement at boundary corners (c=0, c=cols) to prevent gaps between edges
                    let displacement = 0;
                    if (c > 0 && c < cols) {
                        const noiseVal = noise2D(baseX * noiseFreq + r * 0.5, baseZ * noiseFreq + t * 3);
                        displacement = noiseVal * noiseAmp * (0.3 + 0.7 * t);
                    }

                    grid.push({
                        x: (baseX + onx * displacement) * W,
                        y: baseY * W,
                        z: (baseZ + onz * displacement) * W,
                    });
                }
            }

            // Generate triangles from grid
            for (let r = 0; r < rows; r++) {
                for (let c = 0; c < cols; c++) {
                    const i00 = r * gridW + c;
                    const i10 = r * gridW + c + 1;
                    const i01 = (r + 1) * gridW + c;
                    const i11 = (r + 1) * gridW + c + 1;

                    allTris.push(grid[i00], grid[i10], grid[i01]);
                    allTris.push(grid[i10], grid[i11], grid[i01]);
                }
            }
        }
    }

    if (allTris.length === 0) return new THREE.BufferGeometry();

    const vertCount = allTris.length;
    const positions = new Float32Array(vertCount * 3);
    const normals = new Float32Array(vertCount * 3);
    const uvs = new Float32Array(vertCount * 2);
    const colors = new Float32Array(vertCount * 3);

    // Write vertices and compute per-face normals
    for (let i = 0; i < vertCount; i += 3) {
        const p0 = allTris[i], p1 = allTris[i + 1], p2 = allTris[i + 2];

        // Face normal
        const e1x = p1.x - p0.x, e1y = p1.y - p0.y, e1z = p1.z - p0.z;
        const e2x = p2.x - p0.x, e2y = p2.y - p0.y, e2z = p2.z - p0.z;
        let nx = e1y * e2z - e1z * e2y;
        let ny = e1z * e2x - e1x * e2z;
        let nz = e1x * e2y - e1y * e2x;
        const len = Math.sqrt(nx * nx + ny * ny + nz * nz);
        if (len > 1e-8) { nx /= len; ny /= len; nz /= len; }

        for (let v = 0; v < 3; v++) {
            const p = allTris[i + v];
            const idx = i + v;
            positions[idx * 3] = p.x;
            positions[idx * 3 + 1] = p.y;
            positions[idx * 3 + 2] = p.z;
            normals[idx * 3] = nx;
            normals[idx * 3 + 1] = ny;
            normals[idx * 3 + 2] = nz;
            uvs[idx * 2] = p.x / W * 0.25;
            uvs[idx * 2 + 1] = p.y / W * 0.25;
            // Rocky grey-brown color with slight noise variation
            const colorNoise = noise2D(p.x * 0.5, p.z * 0.5) * 0.1;
            colors[idx * 3] = 0.35 + colorNoise;
            colors[idx * 3 + 1] = 0.3 + colorNoise;
            colors[idx * 3 + 2] = 0.25 + colorNoise;
        }
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    geometry.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
    geometry.setAttribute('uv', new THREE.BufferAttribute(uvs, 2));
    geometry.setAttribute('color', new THREE.BufferAttribute(colors, 3));
    return geometry;
}
