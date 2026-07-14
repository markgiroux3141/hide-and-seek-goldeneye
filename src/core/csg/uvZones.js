// assignUVsAndZones — post-CSG triangle classification + UV assignment.
// Ported from spike/csg/main.js:637-959.
//
// CSG output is a triangle soup with no UVs and no notion of "wall vs floor".
// We classify each triangle by its dominant normal (zone), split it at door/hole
// frame boundaries (so frame interiors get tunnel textures), split walls at the
// vertical zone-2/3 boundary, and compute UVs from world-space positions.
//
// Material layout in the returned array: schemeIndex × 8 + zone, where:
//   zone 0 = floor, 1 = ceiling, 2 = lower wall, 3 = upper wall,
//   zone 5 = tunnel wall (door/hole frame), 6 = tunnel floor (door frame floor)
//
// Parameters:
//   geometry: raw CSG output BufferGeometry
//   faceIds:  per-triangle face identity from buildFaceMap
//   brushes:  array of BrushDefs that produced this geometry (used for frame AABBs and per-brush schemes)
//   getMaterialsForScheme(schemeKey) → array of 8 THREE.Materials (zones 0-7)
//
// Returns: { geometry, faceIds, materials }

import * as THREE from 'three';
import { WORLD_SCALE, WALL_THICKNESS, WALL_SPLIT_V } from '../constants.js';

export function assignUVsAndZones(geometry, faceIds, brushes, getMaterialsForScheme) {
    const pos = geometry.getAttribute('position');
    const idx = geometry.index;
    const triCount = idx ? idx.count / 3 : pos.count / 3;

    // We need per-vertex UVs. CSG may share vertices between triangles,
    // so we un-index the geometry to allow per-triangle UV assignment.
    const newPos = [];
    const newNormals = [];
    const newUVs = [];
    const newColors = [];   // white (1,1,1) baseline; lighting baker overwrites later
    const newFaceIds = [];
    const triZones = [];
    const triCentroids = [];   // flat xyz per tri, world space — used for per-tri overrides
    const triSchemes = [];

    // Closure-scoped owner brush for the triangle currently being emitted.
    // Set in the outer loop before each run of emitTri calls so emitTri can
    // look up brush.triZoneOverrides without threading the brush through every
    // call site (there are ~20).
    let currentOwnerBrush = null;
    const TRI_OVERRIDE_EPS = WORLD_SCALE * 0.5;

    // Helper: compute UV from world position for a given face axis.
    // originY shifts the V coordinate of wall faces so the wall texture
    // anchors to the room's floor instead of world Y=0.
    function vertexUV(v, axis, rotated = false, originY = 0) {
        const wx = v.x / WORLD_SCALE, wy = v.y / WORLD_SCALE - originY, wz = v.z / WORLD_SCALE;
        if (rotated) {
            if (axis === 'x') return [wy, wz];
            if (axis === 'z') return [wy, wx];
            return [wz, wx];
        }
        if (axis === 'x') return [wz, wy];
        if (axis === 'y') return [wx, wz];
        return [wx, wy];
    }

    // Helper: emit a triangle with a given zone, axis, normal, faceId, and scheme.
    // Checks winding matches the intended normal — swaps B/C if flipped.
    const _e1 = new THREE.Vector3(), _e2 = new THREE.Vector3(), _cross = new THREE.Vector3();
    function emitTri(pA, pB, pC, nx, ny, nz, axis, zone, faceId, schemeKey, rotated = false, originY = 0) {
        _e1.subVectors(pB, pA);
        _e2.subVectors(pC, pA);
        _cross.crossVectors(_e1, _e2);
        const dot = _cross.x * nx + _cross.y * ny + _cross.z * nz;
        const [vB, vC] = dot < 0 ? [pC, pB] : [pB, pC];

        const cx = (pA.x + vB.x + vC.x) / 3;
        const cy = (pA.y + vB.y + vC.y) / 3;
        const cz = (pA.z + vB.z + vC.z) / 3;

        let finalZone = zone;
        const ovs = currentOwnerBrush && currentOwnerBrush.triZoneOverrides;
        if (ovs && ovs.length > 0) {
            for (let i = 0; i < ovs.length; i++) {
                const o = ovs[i];
                if (Math.abs(o.cx - cx) < TRI_OVERRIDE_EPS &&
                    Math.abs(o.cy - cy) < TRI_OVERRIDE_EPS &&
                    Math.abs(o.cz - cz) < TRI_OVERRIDE_EPS) {
                    finalZone = o.zone;
                    break;
                }
            }
        }

        triZones.push(finalZone);
        triCentroids.push(cx, cy, cz);
        triSchemes.push(schemeKey);
        newFaceIds.push(faceId);
        for (const v of [pA, vB, vC]) {
            newPos.push(v.x, v.y, v.z);
            newNormals.push(nx, ny, nz);
            const [u, uv_v] = vertexUV(v, axis, rotated, originY);
            newUVs.push(u, uv_v);
            newColors.push(1, 1, 1);   // white baseline; lighting bake overwrites
        }
    }

    // Helper: interpolate between two Vector3s at a given y
    function lerpAtY(a, b, y) {
        const t = (y - a.y) / (b.y - a.y);
        return new THREE.Vector3(
            a.x + (b.x - a.x) * t,
            y,
            a.z + (b.z - a.z) * t
        );
    }

    // Helper: interpolate between two Vector3s at a given axis value (x, y, or z)
    function lerpAtAxis(a, b, splitAxis, val) {
        const av = splitAxis === 'x' ? a.x : splitAxis === 'y' ? a.y : a.z;
        const bv = splitAxis === 'x' ? b.x : splitAxis === 'y' ? b.y : b.z;
        const t = (val - av) / (bv - av);
        return new THREE.Vector3(
            a.x + (b.x - a.x) * t,
            a.y + (b.y - a.y) * t,
            a.z + (b.z - a.z) * t
        );
    }

    // Helper: split an array of triangles along an axis=value plane.
    function splitTrisAtAxis(tris, splitAxis, val) {
        const result = [];
        const getVal = splitAxis === 'x' ? v => v.x : splitAxis === 'y' ? v => v.y : v => v.z;
        for (const tri of tris) {
            const verts = [tri.a, tri.b, tri.c];
            const vals = verts.map(getVal);
            const minV = Math.min(...vals), maxV = Math.max(...vals);
            if (maxV <= val + 1e-6 || minV >= val - 1e-6) {
                result.push(tri);
                continue;
            }
            const sorted = verts.slice().sort((a, b) => getVal(a) - getVal(b));
            const [lo, mid, hi] = sorted;
            const pLoHi = lerpAtAxis(lo, hi, splitAxis, val);
            if (getVal(mid) <= val) {
                const pMidHi = lerpAtAxis(mid, hi, splitAxis, val);
                result.push({ a: lo, b: mid, c: pLoHi });
                result.push({ a: mid, b: pMidHi, c: pLoHi });
                result.push({ a: pLoHi, b: pMidHi, c: hi });
            } else {
                const pLoMid = lerpAtAxis(lo, mid, splitAxis, val);
                result.push({ a: lo, b: pLoMid, c: pLoHi });
                result.push({ a: pLoMid, b: mid, c: pLoHi });
                result.push({ a: mid, b: hi, c: pLoHi });
            }
        }
        return result;
    }

    // Collect frame (door + hole) 3D AABBs in world-space for boundary splitting
    const frameAABBs = brushes
        .filter(b => b.isDoorframe || b.isHoleFrame)
        .map(b => ({
            minX: b.minX * WORLD_SCALE, maxX: b.maxX * WORLD_SCALE,
            minY: b.minY * WORLD_SCALE, maxY: b.maxY * WORLD_SCALE,
            minZ: b.minZ * WORLD_SCALE, maxZ: b.maxZ * WORLD_SCALE,
            brush: b
        }));

    const vA = new THREE.Vector3(), vB = new THREE.Vector3(), vC = new THREE.Vector3();
    const edge1 = new THREE.Vector3(), edge2 = new THREE.Vector3();
    const normal = new THREE.Vector3();

    // O(1) brush lookup: the per-triangle loop used to do brushes.find(...)
    // which is O(brushes) per triangle — ~1.4M comparisons at 37k tris × 37 brushes.
    const brushById = new Map();
    for (const b of brushes) brushById.set(b.id, b);

    // Fast-path: a tri whose bbox overlaps NO frame can skip every
    // splitTrisAtAxis call (walls run 6 splits × frames per tri otherwise).
    const hasAnyFrames = frameAABBs.length > 0;
    function triOverlapsAnyFrame(minX, maxX, minY, maxY, minZ, maxZ) {
        for (let i = 0; i < frameAABBs.length; i++) {
            const db = frameAABBs[i];
            if (maxX >= db.minX && minX <= db.maxX &&
                maxY >= db.minY && minY <= db.maxY &&
                maxZ >= db.minZ && minZ <= db.maxZ) return true;
        }
        return false;
    }

    // Per-scheme cache: does the scheme define its own zone-7 brace texture?
    // If yes, brace brushes route every face to zone 7 (white_brace etc).
    // If no, brace brushes fall through to normal wall/floor/ceiling
    // classification so they inherit the room's wall split — useful for
    // schemes (like facility_white_tile) where the brace should look like
    // an integral part of the wall.
    const schemeBraceCache = new Map();
    function schemeHasBraceZone(schemeKey) {
        if (!schemeBraceCache.has(schemeKey)) {
            const mats = getMaterialsForScheme(schemeKey);
            schemeBraceCache.set(schemeKey, !!(mats && mats[7]));
        }
        return schemeBraceCache.get(schemeKey);
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

        const ax = Math.abs(normal.x), ay = Math.abs(normal.y), az = Math.abs(normal.z);
        const faceId = faceIds[t] || { brushId: 0, axis: 'x', side: 'min', position: 0 };
        const nx = normal.x, ny = normal.y, nz = normal.z;

        const ownerBrush = (faceId.brushId !== 0) ? brushById.get(faceId.brushId) : null;
        currentOwnerBrush = ownerBrush;
        const overrideKey = faceId.axis + '-' + faceId.side;
        const scheme = ownerBrush
            ? ((ownerBrush.schemeOverrides && ownerBrush.schemeOverrides[overrideKey]) || ownerBrush.schemeKey)
            : 'facility_white_tile';
        const originY = ownerBrush ? ownerBrush.floorY : 0;

        // Brace brushes: if the scheme defines a dedicated zone-7 brace texture,
        // route every face there regardless of normal direction. Otherwise fall
        // through to normal wall/floor/ceiling classification so the brace
        // inherits the room's wall split (useful for schemes where the brace
        // should look like an integral part of the wall).
        if (ownerBrush && ownerBrush.isBrace && schemeHasBraceZone(scheme)) {
            const braceAxis = ay >= ax && ay >= az ? 'y' : (ax >= az ? 'x' : 'z');
            emitTri(vA.clone(), vB.clone(), vC.clone(), nx, ny, nz, braceAxis, 7, faceId, scheme, false, originY);
            continue;
        }

        // Precompute tri bbox once so the fast-path + slow-path both use it.
        const triMinX = Math.min(vA.x, vB.x, vC.x);
        const triMaxX = Math.max(vA.x, vB.x, vC.x);
        const triMinY = Math.min(vA.y, vB.y, vC.y);
        const triMaxY = Math.max(vA.y, vB.y, vC.y);
        const triMinZ = Math.min(vA.z, vB.z, vC.z);
        const triMaxZ = Math.max(vA.z, vB.z, vC.z);
        const nearAnyFrame = hasAnyFrames && triOverlapsAnyFrame(triMinX, triMaxX, triMinY, triMaxY, triMinZ, triMaxZ);

        if (ay >= ax && ay >= az) {
            // Floor or ceiling
            const axis = 'y';
            const floorOrCeilZone = normal.y > 0 ? 0 : 1;

            // Fast path: no frame touches this tri → emit as plain floor/ceiling.
            if (!nearAnyFrame) {
                emitTri(vA.clone(), vB.clone(), vC.clone(), nx, ny, nz, axis, floorOrCeilZone, faceId, scheme);
                continue;
            }

            if (normal.y > 0) {
                // Floor — split along doorframe XZ boundaries, classify inside/outside
                let floorTris = [{ a: vA.clone(), b: vB.clone(), c: vC.clone() }];
                for (const db of frameAABBs) {
                    floorTris = splitTrisAtAxis(floorTris, 'x', db.minX);
                    floorTris = splitTrisAtAxis(floorTris, 'x', db.maxX);
                    floorTris = splitTrisAtAxis(floorTris, 'z', db.minZ);
                    floorTris = splitTrisAtAxis(floorTris, 'z', db.maxZ);
                }
                for (const tri of floorTris) {
                    const cx = (tri.a.x + tri.b.x + tri.c.x) / 3;
                    const cy = (tri.a.y + tri.b.y + tri.c.y) / 3;
                    const cz = (tri.a.z + tri.b.z + tri.c.z) / 3;
                    let dfBrush = null;
                    for (const db of frameAABBs) {
                        if (cx >= db.minX && cx <= db.maxX && cy >= db.minY && cy <= db.maxY && cz >= db.minZ && cz <= db.maxZ) {
                            dfBrush = db.brush; break;
                        }
                    }
                    if (dfBrush) {
                        const floorZone = dfBrush.isDoorframe ? 6 : 5;
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, floorZone, faceId, scheme, dfBrush.w === WALL_THICKNESS);
                    } else {
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 0, faceId, scheme);
                    }
                }
            } else {
                // Ceiling — split along frame XZ boundaries, classify lintel vs room ceiling
                let ceilTris = [{ a: vA.clone(), b: vB.clone(), c: vC.clone() }];
                for (const db of frameAABBs) {
                    ceilTris = splitTrisAtAxis(ceilTris, 'x', db.minX);
                    ceilTris = splitTrisAtAxis(ceilTris, 'x', db.maxX);
                    ceilTris = splitTrisAtAxis(ceilTris, 'z', db.minZ);
                    ceilTris = splitTrisAtAxis(ceilTris, 'z', db.maxZ);
                }
                for (const tri of ceilTris) {
                    const cx = (tri.a.x + tri.b.x + tri.c.x) / 3;
                    const cy = (tri.a.y + tri.b.y + tri.c.y) / 3;
                    const cz = (tri.a.z + tri.b.z + tri.c.z) / 3;
                    let dfBrush = null;
                    for (const db of frameAABBs) {
                        if (cx >= db.minX && cx <= db.maxX && cy >= db.minY && cy <= db.maxY && cz >= db.minZ && cz <= db.maxZ) {
                            dfBrush = db.brush; break;
                        }
                    }
                    if (dfBrush) {
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 5, faceId, scheme, dfBrush.w === WALL_THICKNESS);
                    } else {
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 1, faceId, scheme);
                    }
                }
            }
        } else {
            // Wall — split along doorframe boundaries on tangent axes, classify inside/outside
            const axis = ax >= az ? 'x' : 'z';

            // Stair-step riser routing — when this triangle belongs to a
            // stair-push brush AND its dominant wall axis matches the stair's
            // carve axis AND its normal sign matches the carve direction (i.e.
            // it's the face pointing back toward the corridor), route it to
            // zone 5 (stair_gradient). This catches the visible riser strips
            // between adjacent stair columns; the side walls (perpendicular
            // axis) and ceiling-drop face (opposite normal sign) fall through
            // to the normal wall classification below.
            if (ownerBrush && ownerBrush.isStairStep && axis === ownerBrush.stairAxis) {
                const normalAlongAxis = axis === 'x' ? nx : nz;
                if (normalAlongAxis * ownerBrush.stairCarveSign > 0.9) {
                    emitTri(vA.clone(), vB.clone(), vC.clone(), nx, ny, nz, axis, 5, faceId, scheme, false, originY);
                    continue;
                }
            }

            // Fast path: wall tri far from every frame — skip all 6 per-frame
            // splitTrisAtAxis calls and go straight to the vertical zone 2/3
            // split. This is the dominant case for large levels.
            if (!nearAnyFrame) {
                const splitY = (originY + WALL_SPLIT_V) * WORLD_SCALE;
                if (triMaxY <= splitY) {
                    emitTri(vA.clone(), vB.clone(), vC.clone(), nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                } else if (triMinY >= splitY) {
                    emitTri(vA.clone(), vB.clone(), vC.clone(), nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                } else {
                    const verts = [vA.clone(), vB.clone(), vC.clone()];
                    verts.sort((a, b) => a.y - b.y);
                    const [lo, mid, hi] = verts;
                    const pLoHi = lerpAtY(lo, hi, splitY);
                    if (mid.y <= splitY) {
                        const pMidHi = lerpAtY(mid, hi, splitY);
                        emitTri(lo, mid, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                        emitTri(mid, pMidHi, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                        emitTri(pLoHi, pMidHi, hi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                    } else {
                        const pLoMid = lerpAtY(lo, mid, splitY);
                        emitTri(lo, pLoMid, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                        emitTri(pLoMid, mid, pLoHi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                        emitTri(mid, hi, pLoHi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                    }
                }
                continue;
            }

            let wallTris = [{ a: vA.clone(), b: vB.clone(), c: vC.clone() }];
            for (const db of frameAABBs) {
                if (axis === 'x') {
                    wallTris = splitTrisAtAxis(wallTris, 'z', db.minZ);
                    wallTris = splitTrisAtAxis(wallTris, 'z', db.maxZ);
                } else {
                    wallTris = splitTrisAtAxis(wallTris, 'x', db.minX);
                    wallTris = splitTrisAtAxis(wallTris, 'x', db.maxX);
                }
                wallTris = splitTrisAtAxis(wallTris, 'y', db.minY);
                wallTris = splitTrisAtAxis(wallTris, 'y', db.maxY);
            }
            for (const tri of wallTris) {
                const cx = (tri.a.x + tri.b.x + tri.c.x) / 3;
                const cy = (tri.a.y + tri.b.y + tri.c.y) / 3;
                const cz = (tri.a.z + tri.b.z + tri.c.z) / 3;
                let dfBrush = null;
                for (const db of frameAABBs) {
                    if (cx >= db.minX && cx <= db.maxX && cy >= db.minY && cy <= db.maxY && cz >= db.minZ && cz <= db.maxZ) {
                        dfBrush = db.brush; break;
                    }
                }
                if (dfBrush) {
                    // Frame wall — zone 5, rotate UVs only for wall-axis holes (not Y-axis)
                    const rotateWall = dfBrush.h !== WALL_THICKNESS;
                    emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 5, faceId, scheme, rotateWall);
                } else {
                    // Room wall — split at WALL_SPLIT_V above the brush's floorY for zone 2/3.
                    const splitY = (originY + WALL_SPLIT_V) * WORLD_SCALE;
                    const minY = Math.min(tri.a.y, tri.b.y, tri.c.y);
                    const maxY = Math.max(tri.a.y, tri.b.y, tri.c.y);

                    if (maxY <= splitY) {
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                    } else if (minY >= splitY) {
                        emitTri(tri.a, tri.b, tri.c, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                    } else {
                        // Triangle crosses the split — clip into sub-triangles
                        const verts = [tri.a, tri.b, tri.c];
                        verts.sort((a, b) => a.y - b.y);
                        const [lo, mid, hi] = verts;
                        const pLoHi = lerpAtY(lo, hi, splitY);

                        if (mid.y <= splitY) {
                            const pMidHi = lerpAtY(mid, hi, splitY);
                            emitTri(lo, mid, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                            emitTri(mid, pMidHi, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                            emitTri(pLoHi, pMidHi, hi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                        } else {
                            const pLoMid = lerpAtY(lo, mid, splitY);
                            emitTri(lo, pLoMid, pLoHi, nx, ny, nz, axis, 2, faceId, scheme, false, originY);
                            emitTri(pLoMid, mid, pLoHi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                            emitTri(mid, hi, pLoHi, nx, ny, nz, axis, 3, faceId, scheme, false, originY);
                        }
                    }
                }
            }
        }
    }

    // Build new un-indexed geometry
    const newGeo = new THREE.BufferGeometry();
    newGeo.setAttribute('position', new THREE.Float32BufferAttribute(newPos, 3));
    newGeo.setAttribute('normal', new THREE.Float32BufferAttribute(newNormals, 3));
    newGeo.setAttribute('uv', new THREE.Float32BufferAttribute(newUVs, 2));
    newGeo.setAttribute('color', new THREE.Float32BufferAttribute(newColors, 3));

    // Build combined material array for all schemes in use.
    // Layout: schemeIndex * 8 + zone. Zones 5,6 are shared (fixed tunnel textures); zone 7 = brace.
    const uniqueSchemes = [...new Set(triSchemes)].sort();
    const schemeIndexMap = {};
    const combinedMaterials = [];

    for (let si = 0; si < uniqueSchemes.length; si++) {
        schemeIndexMap[uniqueSchemes[si]] = si;
        const mats = getMaterialsForScheme(uniqueSchemes[si]);
        for (let z = 0; z <= 7; z++) {
            // Pad missing zones (e.g. schemes without a brace zone 7) with a
            // magenta sentinel so the stride is consistent. Sentinel slots
            // should never actually be referenced — uvZones routes brace
            // triangles elsewhere when the scheme has no zone 7.
            const m = mats && mats[z];
            combinedMaterials.push(m || new THREE.MeshLambertMaterial({ color: 0xff00ff, side: THREE.FrontSide }));
        }
    }

    // Compute material index per triangle
    const triMatIndices = triZones.map((zone, i) => {
        const si = schemeIndexMap[triSchemes[i]] || 0;
        return si * 8 + zone;
    });

    // Sort triangles by material index and emit groups
    const triOrder = triMatIndices.map((matIdx, i) => ({ matIdx, idx: i }));
    triOrder.sort((a, b) => a.matIdx - b.matIdx);

    const sortedIndices = [];
    const sortedFaceIds = [];
    const sortedTriZones = new Uint8Array(triOrder.length);
    const sortedTriCentroids = new Float32Array(triOrder.length * 3);
    for (let i = 0; i < triOrder.length; i++) {
        const ti = triOrder[i].idx;
        const base = ti * 3;
        sortedIndices.push(base, base + 1, base + 2);
        sortedFaceIds.push(newFaceIds[ti]);
        sortedTriZones[i] = triZones[ti];
        sortedTriCentroids[i * 3] = triCentroids[ti * 3];
        sortedTriCentroids[i * 3 + 1] = triCentroids[ti * 3 + 1];
        sortedTriCentroids[i * 3 + 2] = triCentroids[ti * 3 + 2];
    }
    newGeo.setIndex(sortedIndices);

    // Emit groups
    let groupStart = 0, currentMatIdx = triOrder[0]?.matIdx, groupCount = 0;
    for (const { matIdx } of triOrder) {
        if (matIdx !== currentMatIdx) {
            newGeo.addGroup(groupStart, groupCount, currentMatIdx);
            groupStart += groupCount;
            groupCount = 0;
            currentMatIdx = matIdx;
        }
        groupCount += 3;
    }
    if (groupCount > 0) newGeo.addGroup(groupStart, groupCount, currentMatIdx);

    return {
        geometry: newGeo,
        faceIds: sortedFaceIds,
        triZones: sortedTriZones,
        triCentroids: sortedTriCentroids,
        materials: combinedMaterials,
    };
}
