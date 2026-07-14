// buildFaceMap — recover per-triangle face identity from CSG triangle soup.
// Ported from spike/csg/main.js:297-390.
//
// CSG output has no notion of "this triangle came from brush B's x-min face".
// We recover it via centroid bounding-box matching: classify each triangle by
// its dominant normal, then find the brush face whose tangent-axis bounds
// contain the centroid (preferring smaller brushes when distances tie).

import * as THREE from 'three';
import { WORLD_SCALE, CSG_CENTROID_TOL } from '../constants.js';

export function buildFaceMap(geometry, brushList) {
    const pos = geometry.getAttribute('position');
    const idx = geometry.index;
    const triCount = idx ? idx.count / 3 : pos.count / 3;
    const faceIds = [];

    const vA = new THREE.Vector3(), vB = new THREE.Vector3(), vC = new THREE.Vector3();
    const normal = new THREE.Vector3(), centroid = new THREE.Vector3();
    const edge1 = new THREE.Vector3(), edge2 = new THREE.Vector3();

    const allFaces = [];
    for (const brush of brushList) {
        for (const face of brush.getFaces()) {
            allFaces.push({ ...face, brush });
        }
    }

    function centroidInBrush(brush, axis, cx, cy, cz) {
        // Door/hole frames use strict containment (no tolerance buffer). The
        // tolerance is meant to handle CSG floating-point slop on big brushes,
        // but for the small 1-WT-thick frame brushes it's wide enough to claim
        // ownership of wall triangles JUST outside the cutout — making the
        // frame's schemeKey leak onto the surrounding wall when retexturing.
        // Strict containment still matches genuine tunnel-interior triangles.
        const tol = (brush.isDoorframe || brush.isHoleFrame) ? 0 : CSG_CENTROID_TOL;
        if (axis === 'x') {
            return cz >= brush.minZ - tol && cz <= brush.maxZ + tol &&
                   cy >= brush.minY - tol && cy <= brush.maxY + tol;
        } else if (axis === 'y') {
            return cx >= brush.minX - tol && cx <= brush.maxX + tol &&
                   cz >= brush.minZ - tol && cz <= brush.maxZ + tol;
        } else {
            return cx >= brush.minX - tol && cx <= brush.maxX + tol &&
                   cy >= brush.minY - tol && cy <= brush.maxY + tol;
        }
    }

    for (let t = 0; t < triCount; t++) {
        const i0 = idx ? idx.getX(t * 3) : t * 3;
        const i1 = idx ? idx.getX(t * 3 + 1) : t * 3 + 1;
        const i2 = idx ? idx.getX(t * 3 + 2) : t * 3 + 2;

        vA.fromBufferAttribute(pos, i0);
        vB.fromBufferAttribute(pos, i1);
        vC.fromBufferAttribute(pos, i2);

        edge1.subVectors(vB, vA);
        edge2.subVectors(vC, vA);
        normal.crossVectors(edge1, edge2).normalize();

        centroid.set(
            (vA.x + vB.x + vC.x) / 3,
            (vA.y + vB.y + vC.y) / 3,
            (vA.z + vB.z + vC.z) / 3
        );

        const ax = Math.abs(normal.x), ay = Math.abs(normal.y), az = Math.abs(normal.z);
        let axis, side, posAlongAxis;
        if (ax >= ay && ax >= az) {
            axis = 'x'; side = normal.x > 0 ? 'min' : 'max'; posAlongAxis = centroid.x / WORLD_SCALE;
        } else if (ay >= ax && ay >= az) {
            axis = 'y'; side = normal.y > 0 ? 'min' : 'max'; posAlongAxis = centroid.y / WORLD_SCALE;
        } else {
            axis = 'z'; side = normal.z > 0 ? 'min' : 'max'; posAlongAxis = centroid.z / WORLD_SCALE;
        }

        const cx = centroid.x / WORLD_SCALE, cy = centroid.y / WORLD_SCALE, cz = centroid.z / WORLD_SCALE;

        // Match to the brush whose face is closest AND whose bounding box contains
        // the centroid on the tangent axes. Prefer smaller (more specific) brushes
        // when distances are equal.
        //
        // NOTE: we deliberately do NOT filter by face.side. The normal-derived
        // `side` above is correct for SUBTRACT brushes (cavity walls have outward
        // normals pointing into the cavity, away from the brush volume) but
        // INVERTED for ADDITIVE brushes that protrude into a cavity (the visible
        // face on the brush's 'max' side has +X normal, which the formula above
        // would label 'min'). Position + axis + centroid-containment uniquely
        // identify the face for any non-degenerate brush, so the side label is
        // redundant for matching. The output uses bestFace.side (the brush's
        // canonical side), so face identity downstream is still correct.
        let bestFace = null, bestDist = Infinity, bestVolume = Infinity;
        for (const face of allFaces) {
            if (face.axis !== axis) continue;
            const dist = Math.abs(face.pos - posAlongAxis);
            if (dist > CSG_CENTROID_TOL) continue;
            if (!centroidInBrush(face.brush, axis, cx, cy, cz)) continue;

            const vol = face.brush.w * face.brush.h * face.brush.d;
            if (dist < bestDist || (dist === bestDist && vol < bestVolume)) {
                bestDist = dist; bestFace = face; bestVolume = vol;
            }
        }

        if (bestFace) {
            faceIds.push({
                brushId: bestFace.brushId, axis: bestFace.axis,
                side: bestFace.side, position: bestFace.pos
            });
        } else {
            faceIds.push({
                brushId: 0, axis, side,
                position: Math.round(posAlongAxis)
            });
        }
    }
    return faceIds;
}
