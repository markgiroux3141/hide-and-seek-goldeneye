// Geometry subdivision for baking — splits each quad into NxN sub-quads
// for smoother baked vertex-color lighting.
//
// Assumes geometry follows the GeometryBuilder convention:
//   - Vertices are stored in groups of 4 (one quad each, not shared)
//   - Indices are 6 per quad: (base, base+1, base+2, base, base+2, base+3)

import * as THREE from 'three';

function bilerp3(p0, p1, p2, p3, u, v) {
    return [
        (1 - u) * (1 - v) * p0[0] + u * (1 - v) * p1[0] + u * v * p2[0] + (1 - u) * v * p3[0],
        (1 - u) * (1 - v) * p0[1] + u * (1 - v) * p1[1] + u * v * p2[1] + (1 - u) * v * p3[1],
        (1 - u) * (1 - v) * p0[2] + u * (1 - v) * p1[2] + u * v * p2[2] + (1 - u) * v * p3[2],
    ];
}

function bilerp2(p0, p1, p2, p3, u, v) {
    return [
        (1 - u) * (1 - v) * p0[0] + u * (1 - v) * p1[0] + u * v * p2[0] + (1 - u) * v * p3[0],
        (1 - u) * (1 - v) * p0[1] + u * (1 - v) * p1[1] + u * v * p2[1] + (1 - u) * v * p3[1],
    ];
}

/**
 * Subdivide a BufferGeometry: each quad becomes NxN sub-quads.
 *
 * @param {THREE.BufferGeometry} srcGeo - Source geometry (4 verts per quad)
 * @param {Array} srcFaceIds - Original faceId array (2 entries per quad)
 * @param {number} N - Subdivision level (each quad becomes NxN sub-quads)
 * @returns {{ geometry: THREE.BufferGeometry, faceIds: Array }}
 */
export function subdivideGeometry(srcGeo, srcFaceIds, N) {
    if (N <= 1) return { geometry: srcGeo, faceIds: srcFaceIds };

    const srcPos = srcGeo.getAttribute('position');
    const srcNor = srcGeo.getAttribute('normal');
    const srcUv = srcGeo.getAttribute('uv');
    const srcIdx = srcGeo.getIndex();

    const numQuads = srcPos.count / 4;

    // Map each quad to its material group index
    const quadGroupMap = new Int32Array(numQuads);
    const groups = srcGeo.groups;
    if (groups && groups.length > 0 && srcIdx) {
        const idxArr = srcIdx.array;
        for (const group of groups) {
            for (let i = group.start; i < group.start + group.count; i += 6) {
                const vertIdx = idxArr[i];
                const quadIdx = Math.floor(vertIdx / 4);
                quadGroupMap[quadIdx] = group.materialIndex;
            }
        }
    }

    // Map each quad to its faceId
    const quadFaceIdMap = new Array(numQuads);
    if (srcFaceIds && srcIdx) {
        const idxArr = srcIdx.array;
        for (let triIdx = 0; triIdx < srcFaceIds.length; triIdx++) {
            const idxStart = triIdx * 3;
            if (idxStart < idxArr.length) {
                const vertIdx = idxArr[idxStart];
                const quadIdx = Math.floor(vertIdx / 4);
                quadFaceIdMap[quadIdx] = srcFaceIds[triIdx];
            }
        }
    }

    // Pre-allocate output arrays (each quad becomes N*N sub-quads, each with 4 verts)
    const totalSubQuads = numQuads * N * N;
    const positions = new Float32Array(totalSubQuads * 4 * 3);
    const normals = new Float32Array(totalSubQuads * 4 * 3);
    const uvArr = new Float32Array(totalSubQuads * 4 * 2);
    const colorArr = new Float32Array(totalSubQuads * 4 * 3);
    const indices = new Uint32Array(totalSubQuads * 6);
    const faceIds = new Array(totalSubQuads * 2);
    const subQuadZones = new Int32Array(totalSubQuads);

    let vOff = 0; // vertex offset (counts vertices, not floats)
    let sqIdx = 0; // sub-quad index

    for (let q = 0; q < numQuads; q++) {
        const base = q * 4;

        const p0 = [srcPos.getX(base), srcPos.getY(base), srcPos.getZ(base)];
        const p1 = [srcPos.getX(base + 1), srcPos.getY(base + 1), srcPos.getZ(base + 1)];
        const p2 = [srcPos.getX(base + 2), srcPos.getY(base + 2), srcPos.getZ(base + 2)];
        const p3 = [srcPos.getX(base + 3), srcPos.getY(base + 3), srcPos.getZ(base + 3)];

        const nx = srcNor.getX(base), ny = srcNor.getY(base), nz = srcNor.getZ(base);

        const uv0 = [srcUv.getX(base), srcUv.getY(base)];
        const uv1 = [srcUv.getX(base + 1), srcUv.getY(base + 1)];
        const uv2 = [srcUv.getX(base + 2), srcUv.getY(base + 2)];
        const uv3 = [srcUv.getX(base + 3), srcUv.getY(base + 3)];

        const zone = quadGroupMap[q];
        const faceId = quadFaceIdMap[q] || null;

        for (let j = 0; j < N; j++) {
            for (let i = 0; i < N; i++) {
                const u0 = i / N, u1 = (i + 1) / N;
                const v0 = j / N, v1 = (j + 1) / N;

                const corners = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
                for (let k = 0; k < 4; k++) {
                    const [u, v] = corners[k];
                    const sp = bilerp3(p0, p1, p2, p3, u, v);
                    const pIdx = (vOff + k) * 3;
                    positions[pIdx] = sp[0]; positions[pIdx + 1] = sp[1]; positions[pIdx + 2] = sp[2];
                    normals[pIdx] = nx; normals[pIdx + 1] = ny; normals[pIdx + 2] = nz;

                    const suv = bilerp2(uv0, uv1, uv2, uv3, u, v);
                    const uvIdx = (vOff + k) * 2;
                    uvArr[uvIdx] = suv[0]; uvArr[uvIdx + 1] = suv[1];

                    const cIdx = (vOff + k) * 3;
                    colorArr[cIdx] = 1; colorArr[cIdx + 1] = 1; colorArr[cIdx + 2] = 1;
                }

                const iIdx = sqIdx * 6;
                indices[iIdx] = vOff; indices[iIdx + 1] = vOff + 1; indices[iIdx + 2] = vOff + 2;
                indices[iIdx + 3] = vOff; indices[iIdx + 4] = vOff + 2; indices[iIdx + 5] = vOff + 3;

                faceIds[sqIdx * 2] = faceId;
                faceIds[sqIdx * 2 + 1] = faceId;
                subQuadZones[sqIdx] = zone;

                vOff += 4;
                sqIdx++;
            }
        }
    }

    // Build new geometry
    const newGeo = new THREE.BufferGeometry();
    newGeo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    newGeo.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
    newGeo.setAttribute('uv', new THREE.BufferAttribute(uvArr, 2));
    newGeo.setAttribute('color', new THREE.BufferAttribute(colorArr, 3));
    newGeo.setIndex(new THREE.BufferAttribute(indices, 1));

    // Rebuild material groups sorted by zone
    const zoneSet = new Set(subQuadZones);
    if (zoneSet.size > 0) {
        const subQuadOrder = Array.from(subQuadZones).map((z, i) => ({ idx: i, zone: z }));
        subQuadOrder.sort((a, b) => a.zone - b.zone);

        const sortedIndices = new Uint32Array(totalSubQuads * 6);
        const sortedFaceIds = new Array(totalSubQuads * 2);
        for (let si = 0; si < subQuadOrder.length; si++) {
            const sq = subQuadOrder[si];
            const srcStart = sq.idx * 6;
            const dstStart = si * 6;
            for (let j = 0; j < 6; j++) sortedIndices[dstStart + j] = indices[srcStart + j];
            sortedFaceIds[si * 2] = faceIds[sq.idx * 2];
            sortedFaceIds[si * 2 + 1] = faceIds[sq.idx * 2 + 1];
        }

        newGeo.setIndex(new THREE.BufferAttribute(sortedIndices, 1));

        let groupStart = 0;
        let currentZone = subQuadOrder[0].zone;
        let groupCount = 0;
        for (const sq of subQuadOrder) {
            if (sq.zone !== currentZone) {
                newGeo.addGroup(groupStart, groupCount, currentZone);
                groupStart += groupCount;
                groupCount = 0;
                currentZone = sq.zone;
            }
            groupCount += 6;
        }
        newGeo.addGroup(groupStart, groupCount, currentZone);

        return { geometry: newGeo, faceIds: sortedFaceIds };
    }

    return { geometry: newGeo, faceIds };
}
