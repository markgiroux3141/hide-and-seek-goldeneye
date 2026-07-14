// Subtracts axis-aligned cleanup prisms from baked-mesh triangles.
//
// For every triangle whose AABB overlaps any prism, the slicer treats it as
// a 3-vertex polygon (with full per-vertex attribute data) and walks each
// prism in turn, splitting against the prism's 6 axis-aligned planes.
// Outside-the-prism slabs are collected as surviving pieces; the leftover
// "inside the prism" piece feeds the next plane. After all 6 planes the
// fully-inside piece is discarded. Surviving polygons cascade through the
// remaining prisms the same way. Each surviving polygon is fan-triangulated;
// the original triangle is collapsed to degenerate; the new fan triangles
// are appended to the geometry's attribute / index buffers in one batched
// rebuild per mesh.
//
// Returns a flat array of undo entries — collapse entries (kind 'index' or
// 'position', one per sliced triangle) plus one 'append' entry per affected
// mesh that snapshots the original buffer sizes and material groups so a
// later undo can trim the appended geometry off cleanly.

import * as THREE from 'three';

export function sliceTrianglesAgainstPrisms(meshes, prisms) {
    if (!prisms || prisms.length === 0) return [];
    const prismAABBs = prisms.map(prismAABB);
    const allEntries = [];
    for (const mesh of meshes) {
        if (!mesh || !mesh.isMesh || !mesh.geometry) continue;
        const meshEntries = sliceOneMesh(mesh, prisms, prismAABBs);
        for (const e of meshEntries) allEntries.push(e);
    }
    return allEntries;
}

// ─── Per-mesh slicer ────────────────────────────────────────────────
function sliceOneMesh(mesh, prisms, prismAABBs) {
    const geo = mesh.geometry;
    const pos = geo.getAttribute('position');
    if (!pos) return [];
    const norm = geo.getAttribute('normal');
    const uv   = geo.getAttribute('uv');
    const col  = geo.getAttribute('color');
    const indexed = !!geo.index;

    const origVertCount  = pos.count;
    const origIndexCount = indexed ? geo.index.count : 0;
    const origGroups = geo.groups.map(g => ({
        start: g.start, count: g.count, materialIndex: g.materialIndex,
    }));

    // Per-triangle material lookup so appended fan triangles can be put back
    // into the right material group.
    let triMatLookup = null;
    if (indexed && origGroups.length > 0) {
        const triCount = origIndexCount / 3;
        triMatLookup = new Int32Array(triCount);
        for (const g of origGroups) {
            const t0 = g.start / 3, t1 = (g.start + g.count) / 3;
            for (let t = t0; t < t1; t++) triMatLookup[t] = g.materialIndex;
        }
    }

    const collapseEntries = [];
    const appendedTrisByMat = new Map();   // materialIndex -> array of [v0,v1,v2]

    const triCount = indexed ? origIndexCount / 3 : pos.count / 3;
    for (let t = 0; t < triCount; t++) {
        let ia, ib, ic;
        if (indexed) {
            ia = geo.index.getX(t * 3);
            ib = geo.index.getX(t * 3 + 1);
            ic = geo.index.getX(t * 3 + 2);
            if (ia === ib && ib === ic) continue;          // already degenerate
        } else {
            ia = t * 3; ib = t * 3 + 1; ic = t * 3 + 2;
        }
        const va = readVertex(ia, pos, norm, uv, col);
        const vb = readVertex(ib, pos, norm, uv, col);
        const vc = readVertex(ic, pos, norm, uv, col);

        const aabb = triangleAABB(va, vb, vc);
        const prismsHere = relevantPrisms(aabb, prisms, prismAABBs);
        if (prismsHere.length === 0) continue;

        const pieces = subtractAllPrismsFromPolygon([va, vb, vc], prismsHere);

        // No surviving pieces — triangle was fully inside some prism. Collapse only.
        if (pieces.length === 0) {
            const ce = collapseTriangleAt(geo, t, indexed);
            if (ce) { ce.mesh = mesh; collapseEntries.push(ce); }
            continue;
        }

        // Triangle untouched (fast path: AABB overlap was false-positive). The
        // slicer hands back the same vertex references when nothing crossed.
        if (pieces.length === 1 && pieces[0].length === 3 &&
            pieces[0][0] === va && pieces[0][1] === vb && pieces[0][2] === vc) {
            continue;
        }

        // Replace original with a fan of the surviving polygons.
        const ce = collapseTriangleAt(geo, t, indexed);
        if (ce) { ce.mesh = mesh; collapseEntries.push(ce); }

        const matIdx = triMatLookup ? triMatLookup[t] : 0;
        let list = appendedTrisByMat.get(matIdx);
        if (!list) { list = []; appendedTrisByMat.set(matIdx, list); }
        for (const piece of pieces) {
            const k = piece.length;
            for (let i = 1; i < k - 1; i++) {
                list.push([piece[0], piece[i], piece[i + 1]]);
            }
        }
    }

    let totalAppended = 0;
    for (const list of appendedTrisByMat.values()) totalAppended += list.length;
    if (totalAppended === 0) return collapseEntries;

    appendNewTriangles(mesh, appendedTrisByMat, indexed, origVertCount, origIndexCount);

    return [
        ...collapseEntries,
        { kind: 'append', mesh, origVertCount, origIndexCount, origGroups },
    ];
}

// ─── Buffer growth (one rebuild per mesh) ───────────────────────────
function appendNewTriangles(mesh, appendedTrisByMat, indexed, origVertCount, origIndexCount) {
    const geo = mesh.geometry;
    const pos = geo.getAttribute('position');
    const norm = geo.getAttribute('normal');
    const uv   = geo.getAttribute('uv');
    const col  = geo.getAttribute('color');
    const hasNorm = !!norm, hasUV = !!uv, hasCol = !!col;

    // Stable ordering across materials so the new groups are deterministic.
    const matKeys = [...appendedTrisByMat.keys()].sort((a, b) => a - b);
    let totalNewVerts = 0;
    for (const m of matKeys) totalNewVerts += appendedTrisByMat.get(m).length * 3;

    const newPos  = new Float32Array((origVertCount + totalNewVerts) * 3);
    newPos.set(pos.array.subarray(0, origVertCount * 3));
    const newNorm = hasNorm ? new Float32Array((origVertCount + totalNewVerts) * 3) : null;
    if (hasNorm) newNorm.set(norm.array.subarray(0, origVertCount * 3));
    const newUV   = hasUV   ? new Float32Array((origVertCount + totalNewVerts) * 2) : null;
    if (hasUV)   newUV.set(uv.array.subarray(0, origVertCount * 2));
    const newCol  = hasCol  ? new Float32Array((origVertCount + totalNewVerts) * 3) : null;
    if (hasCol)  newCol.set(col.array.subarray(0, origVertCount * 3));

    let writeVi = origVertCount;

    let newIdxArr = null, writeIi = origIndexCount;
    if (indexed) {
        // Promote to Uint32 so we can grow past 65535 verts safely.
        newIdxArr = new Uint32Array(origIndexCount + totalNewVerts);
        const oldIdx = geo.index.array;
        for (let i = 0; i < origIndexCount; i++) newIdxArr[i] = oldIdx[i];
    }

    const newGroups = [];

    for (const m of matKeys) {
        const tris = appendedTrisByMat.get(m);
        const groupStart = writeIi;
        for (const tri of tris) {
            for (const v of tri) {
                newPos[writeVi * 3]     = v.px;
                newPos[writeVi * 3 + 1] = v.py;
                newPos[writeVi * 3 + 2] = v.pz;
                if (hasNorm) {
                    let nx = v.nx, ny = v.ny, nz = v.nz;
                    const nm = Math.hypot(nx, ny, nz);
                    if (nm > 1e-8) { nx /= nm; ny /= nm; nz /= nm; }
                    newNorm[writeVi * 3]     = nx;
                    newNorm[writeVi * 3 + 1] = ny;
                    newNorm[writeVi * 3 + 2] = nz;
                }
                if (hasUV) {
                    newUV[writeVi * 2]     = v.u;
                    newUV[writeVi * 2 + 1] = v.v;
                }
                if (hasCol) {
                    newCol[writeVi * 3]     = v.cr;
                    newCol[writeVi * 3 + 1] = v.cg;
                    newCol[writeVi * 3 + 2] = v.cb;
                }
                if (indexed) newIdxArr[writeIi++] = writeVi;
                writeVi++;
            }
        }
        if (indexed) {
            newGroups.push({ start: groupStart, count: writeIi - groupStart, materialIndex: m });
        }
    }

    geo.setAttribute('position', new THREE.BufferAttribute(newPos, 3));
    if (hasNorm) geo.setAttribute('normal', new THREE.BufferAttribute(newNorm, 3));
    if (hasUV)   geo.setAttribute('uv',     new THREE.BufferAttribute(newUV, 2));
    if (hasCol)  geo.setAttribute('color',  new THREE.BufferAttribute(newCol, 3));
    if (indexed) {
        geo.setIndex(new THREE.BufferAttribute(newIdxArr, 1));
        for (const g of newGroups) geo.addGroup(g.start, g.count, g.materialIndex);
    }
    geo.computeBoundingBox();
    geo.computeBoundingSphere();
}

// ─── Polygon-vs-prism subtraction ───────────────────────────────────
function subtractAllPrismsFromPolygon(poly, prisms) {
    let pieces = [poly];
    for (const prism of prisms) {
        const next = [];
        for (const piece of pieces) {
            const subtracted = subtractPrismFromPolygon(piece, prism);
            for (const p of subtracted) next.push(p);
        }
        pieces = next;
        if (pieces.length === 0) return pieces;
    }
    return pieces;
}

// One prism = 6 axis-aligned planes. Each plane splits the polygon into an
// "outside this plane" part (guaranteed outside the prism, kept) and an
// "inside this plane" part (still possibly inside the prism, fed to the next
// plane). After all 6 planes, anything still in `remaining` is fully inside
// the prism and gets discarded.
function subtractPrismFromPolygon(poly, prism) {
    const planes = prismPlanes(prism);
    const result = [];
    let remaining = poly;
    for (const plane of planes) {
        if (remaining.length < 3) return result;
        const { outside, inside } = splitPolygon(remaining, plane);
        if (outside.length >= 3) result.push(outside);
        if (inside.length < 3) return result;
        remaining = inside;
    }
    return result;
}

// Standard polygon split — for each edge, emit the leading vertex to its
// side and (if the edge crosses the plane) the intersection point to both.
// "Inside" the half-space is dCur <= 0 so on-plane verts stay inside.
function splitPolygon(poly, plane) {
    if (poly.length < 3) return { outside: [], inside: [] };
    const outside = [];
    const inside = [];
    const n = poly.length;
    for (let i = 0; i < n; i++) {
        const cur  = poly[i];
        const next = poly[(i + 1) % n];
        const dCur  = plane.sign * (vCoord(cur,  plane.axis) - plane.c);
        const dNext = plane.sign * (vCoord(next, plane.axis) - plane.c);
        const curIn  = dCur  <= 0;
        const nextIn = dNext <= 0;
        if (curIn) inside.push(cur); else outside.push(cur);
        if (curIn !== nextIn) {
            let t = dCur / (dCur - dNext);
            if (t < 0) t = 0; else if (t > 1) t = 1;
            const ip = lerpVertex(cur, next, t);
            inside.push(ip);
            outside.push(ip);
        }
    }
    return {
        outside: outside.length >= 3 ? outside : [],
        inside:  inside.length  >= 3 ? inside  : [],
    };
}

// ─── Prism planes / AABBs ───────────────────────────────────────────
function prismPlanes(p) {
    let axisU, axisV;
    if      (p.axis === 'x') { axisU = 'z'; axisV = 'y'; }
    else if (p.axis === 'y') { axisU = 'x'; axisV = 'z'; }
    else                     { axisU = 'x'; axisV = 'y'; }
    return [
        { axis: p.axis, sign: +1, c: p.nMaxM },
        { axis: p.axis, sign: -1, c: p.nMinM },
        { axis: axisU,  sign: +1, c: p.uMaxM },
        { axis: axisU,  sign: -1, c: p.uMinM },
        { axis: axisV,  sign: +1, c: p.vMaxM },
        { axis: axisV,  sign: -1, c: p.vMinM },
    ];
}

function prismAABB(p) {
    if (p.axis === 'x') return {
        xMin: p.nMinM, xMax: p.nMaxM,
        zMin: p.uMinM, zMax: p.uMaxM,
        yMin: p.vMinM, yMax: p.vMaxM,
    };
    if (p.axis === 'y') return {
        yMin: p.nMinM, yMax: p.nMaxM,
        xMin: p.uMinM, xMax: p.uMaxM,
        zMin: p.vMinM, zMax: p.vMaxM,
    };
    return {
        zMin: p.nMinM, zMax: p.nMaxM,
        xMin: p.uMinM, xMax: p.uMaxM,
        yMin: p.vMinM, yMax: p.vMaxM,
    };
}

function relevantPrisms(aabb, prisms, prismAABBs) {
    const out = [];
    for (let i = 0; i < prisms.length; i++) {
        const p = prismAABBs[i];
        if (aabb.xMin > p.xMax || aabb.xMax < p.xMin) continue;
        if (aabb.yMin > p.yMax || aabb.yMax < p.yMin) continue;
        if (aabb.zMin > p.zMax || aabb.zMax < p.zMin) continue;
        out.push(prisms[i]);
    }
    return out;
}

// ─── Vertex helpers ─────────────────────────────────────────────────
function readVertex(idx, pos, norm, uv, col) {
    const v = {
        px: pos.getX(idx), py: pos.getY(idx), pz: pos.getZ(idx),
        nx: 0, ny: 0, nz: 0, u: 0, v: 0, cr: 0, cg: 0, cb: 0,
    };
    if (norm) { v.nx = norm.getX(idx); v.ny = norm.getY(idx); v.nz = norm.getZ(idx); }
    if (uv)   { v.u  = uv.getX(idx);   v.v  = uv.getY(idx); }
    if (col)  { v.cr = col.getX(idx);  v.cg = col.getY(idx); v.cb = col.getZ(idx); }
    return v;
}

function lerpVertex(a, b, t) {
    return {
        px: a.px + (b.px - a.px) * t,
        py: a.py + (b.py - a.py) * t,
        pz: a.pz + (b.pz - a.pz) * t,
        nx: a.nx + (b.nx - a.nx) * t,
        ny: a.ny + (b.ny - a.ny) * t,
        nz: a.nz + (b.nz - a.nz) * t,
        u:  a.u  + (b.u  - a.u)  * t,
        v:  a.v  + (b.v  - a.v)  * t,
        cr: a.cr + (b.cr - a.cr) * t,
        cg: a.cg + (b.cg - a.cg) * t,
        cb: a.cb + (b.cb - a.cb) * t,
    };
}

function vCoord(v, axis) {
    return axis === 'x' ? v.px : axis === 'y' ? v.py : v.pz;
}

function triangleAABB(va, vb, vc) {
    return {
        xMin: Math.min(va.px, vb.px, vc.px),
        xMax: Math.max(va.px, vb.px, vc.px),
        yMin: Math.min(va.py, vb.py, vc.py),
        yMax: Math.max(va.py, vb.py, vc.py),
        zMin: Math.min(va.pz, vb.pz, vc.pz),
        zMax: Math.max(va.pz, vb.pz, vc.pz),
    };
}

// ─── Triangle collapse (degenerate, in-place) ───────────────────────
// Same shape as collapseTriangleNoUndo in indoorClick.js — kept local so
// the slicer doesn't need to import its caller.
function collapseTriangleAt(geo, faceIndex, indexed) {
    if (indexed) {
        const idx = geo.index;
        const i0 = idx.getX(faceIndex * 3);
        const i1 = idx.getX(faceIndex * 3 + 1);
        const i2 = idx.getX(faceIndex * 3 + 2);
        if (i0 === i1 && i1 === i2) return null;
        idx.setX(faceIndex * 3 + 1, i0);
        idx.setX(faceIndex * 3 + 2, i0);
        idx.needsUpdate = true;
        return { kind: 'index', faceIndex, i0, i1, i2 };
    }
    const pos = geo.getAttribute('position');
    const j0 = faceIndex * 3, j1 = faceIndex * 3 + 1, j2 = faceIndex * 3 + 2;
    const ax = pos.getX(j0), ay = pos.getY(j0), az = pos.getZ(j0);
    const bx = pos.getX(j1), by = pos.getY(j1), bz = pos.getZ(j1);
    const cx = pos.getX(j2), cy = pos.getY(j2), cz = pos.getZ(j2);
    if (ax === bx && bx === cx && ay === by && by === cy && az === bz && bz === cz) return null;
    pos.setXYZ(j1, ax, ay, az);
    pos.setXYZ(j2, ax, ay, az);
    pos.needsUpdate = true;
    return { kind: 'position', faceIndex, x1: bx, y1: by, z1: bz, x2: cx, y2: cy, z2: cz };
}
