// Constrained Delaunay Triangulation with Ruppert's refinement
// Generates quality triangle meshes from boundary polygons with holes

// ============================================================
// BASIC GEOMETRY UTILITIES
// ============================================================

function cross2D(ox, oy, ax, ay, bx, by) {
    return (ax - ox) * (by - oy) - (ay - oy) * (bx - ox);
}

function distSq(ax, ay, bx, by) {
    const dx = bx - ax, dy = by - ay;
    return dx * dx + dy * dy;
}

function circumcircle(ax, ay, bx, by, cx, cy) {
    const D = 2 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if (Math.abs(D) < 1e-10) return null;
    const ux = ((ax * ax + ay * ay) * (by - cy) + (bx * bx + by * by) * (cy - ay) + (cx * cx + cy * cy) * (ay - by)) / D;
    const uy = ((ax * ax + ay * ay) * (cx - bx) + (bx * bx + by * by) * (ax - cx) + (cx * cx + cy * cy) * (bx - ax)) / D;
    const rSq = distSq(ax, ay, ux, uy);
    return { x: ux, y: uy, rSq };
}

function inCircumcircle(px, py, ax, ay, bx, by, cx, cy) {
    const cc = circumcircle(ax, ay, bx, by, cx, cy);
    if (!cc) return false;
    return distSq(px, py, cc.x, cc.y) < cc.rSq - 1e-8;
}

function triangleArea(ax, ay, bx, by, cx, cy) {
    return Math.abs(cross2D(ax, ay, bx, by, cx, cy)) / 2;
}

function triangleMinAngle(ax, ay, bx, by, cx, cy) {
    const a2 = distSq(bx, by, cx, cy);
    const b2 = distSq(ax, ay, cx, cy);
    const c2 = distSq(ax, ay, bx, by);
    const a = Math.sqrt(a2), b = Math.sqrt(b2), c = Math.sqrt(c2);
    if (a < 1e-10 || b < 1e-10 || c < 1e-10) return 0;
    const cosA = (b2 + c2 - a2) / (2 * b * c);
    const cosB = (a2 + c2 - b2) / (2 * a * c);
    const cosC = (a2 + b2 - c2) / (2 * a * b);
    return Math.min(
        Math.acos(Math.max(-1, Math.min(1, cosA))),
        Math.acos(Math.max(-1, Math.min(1, cosB))),
        Math.acos(Math.max(-1, Math.min(1, cosC)))
    );
}

// ============================================================
// EDGE KEY HELPERS
// ============================================================

function edgeKey(a, b) {
    return a < b ? `${a}_${b}` : `${b}_${a}`;
}

// ============================================================
// BASIC DELAUNAY TRIANGULATION (Bowyer-Watson)
// ============================================================

function bowyerWatson(points) {
    // Create super-triangle that contains all points
    let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (const p of points) {
        if (p.x < minX) minX = p.x;
        if (p.x > maxX) maxX = p.x;
        if (p.y < minY) minY = p.y;
        if (p.y > maxY) maxY = p.y;
    }
    const dx = maxX - minX, dy = maxY - minY;
    const dmax = Math.max(dx, dy, 1);
    const midX = (minX + maxX) / 2, midY = (minY + maxY) / 2;

    // Super-triangle vertices (indices -3, -2, -1 mapped to end of array)
    const stA = { x: midX - 20 * dmax, y: midY - dmax };
    const stB = { x: midX, y: midY + 20 * dmax };
    const stC = { x: midX + 20 * dmax, y: midY - dmax };

    const n = points.length;
    const allPts = [...points, stA, stB, stC];

    // triangles: each is [i, j, k] indices into allPts
    let triangles = [[n, n + 1, n + 2]];

    // Insert each point
    for (let pi = 0; pi < n; pi++) {
        const px = allPts[pi].x, py = allPts[pi].y;

        // Find bad triangles (point inside circumcircle)
        const bad = [];
        for (let ti = 0; ti < triangles.length; ti++) {
            const [a, b, c] = triangles[ti];
            if (inCircumcircle(px, py, allPts[a].x, allPts[a].y, allPts[b].x, allPts[b].y, allPts[c].x, allPts[c].y)) {
                bad.push(ti);
            }
        }

        // Find boundary of polygonal hole (edges that are not shared by two bad triangles)
        const edgeCount = new Map();
        for (const ti of bad) {
            const [a, b, c] = triangles[ti];
            const edges = [[a, b], [b, c], [c, a]];
            for (const [ea, eb] of edges) {
                const key = edgeKey(ea, eb);
                edgeCount.set(key, (edgeCount.get(key) || 0) + 1);
            }
        }

        const boundary = [];
        for (const ti of bad) {
            const [a, b, c] = triangles[ti];
            const edges = [[a, b], [b, c], [c, a]];
            for (const [ea, eb] of edges) {
                const key = edgeKey(ea, eb);
                if (edgeCount.get(key) === 1) {
                    boundary.push([ea, eb]);
                }
            }
        }

        // Remove bad triangles (in reverse order to keep indices valid)
        bad.sort((a, b) => b - a);
        for (const ti of bad) {
            triangles.splice(ti, 1);
        }

        // Create new triangles from boundary edges to new point
        for (const [ea, eb] of boundary) {
            triangles.push([pi, ea, eb]);
        }
    }

    // Remove triangles that share a vertex with the super-triangle
    triangles = triangles.filter(([a, b, c]) => a < n && b < n && c < n);

    return triangles;
}

// ============================================================
// CONSTRAINED DELAUNAY TRIANGULATION
// Enforces that specified edges appear in the triangulation
// ============================================================

function constrainEdge(points, triangles, ei, ej) {
    // Check if edge already exists in triangulation
    for (const tri of triangles) {
        const [a, b, c] = tri;
        if ((a === ei && b === ej) || (b === ei && c === ej) || (c === ei && a === ej) ||
            (a === ej && b === ei) || (b === ej && c === ei) || (c === ej && a === ei)) {
            return triangles; // edge already present
        }
    }

    // Find triangles that the constrained edge crosses and flip/split
    // Simple approach: find and remove intersecting triangles, re-triangulate the cavity
    const px1 = points[ei].x, py1 = points[ei].y;
    const px2 = points[ej].x, py2 = points[ej].y;

    // Collect triangles whose interior intersects the constrained edge
    const intersecting = [];
    for (let ti = 0; ti < triangles.length; ti++) {
        const [a, b, c] = triangles[ti];
        if (a === ei || b === ei || c === ei || a === ej || b === ej || c === ej) continue;
        const edges = [[a, b], [b, c], [c, a]];
        for (const [ea, eb] of edges) {
            if (segmentsIntersect(px1, py1, px2, py2, points[ea].x, points[ea].y, points[eb].x, points[eb].y)) {
                intersecting.push(ti);
                break;
            }
        }
    }

    if (intersecting.length === 0) return triangles;

    // Collect all vertices from intersecting triangles + the constrained edge endpoints
    const vertexSet = new Set([ei, ej]);
    for (const ti of intersecting) {
        const [a, b, c] = triangles[ti];
        vertexSet.add(a);
        vertexSet.add(b);
        vertexSet.add(c);
    }

    // Remove intersecting triangles
    const remaining = triangles.filter((_, i) => !intersecting.includes(i));

    // Split the vertex set into two sides of the constrained edge
    const above = [], below = [];
    for (const vi of vertexSet) {
        if (vi === ei || vi === ej) continue;
        const side = cross2D(px1, py1, px2, py2, points[vi].x, points[vi].y);
        if (side > 0) above.push(vi);
        else below.push(vi);
    }

    // Triangulate each side with the constrained edge
    const newTris = [];
    triangulateCavitySide(points, ei, ej, above, newTris);
    triangulateCavitySide(points, ej, ei, below, newTris);

    return [...remaining, ...newTris];
}

function triangulateCavitySide(points, edgeA, edgeB, sideVerts, outTris) {
    if (sideVerts.length === 0) return;
    if (sideVerts.length === 1) {
        outTris.push([edgeA, sideVerts[0], edgeB]);
        return;
    }

    // Fan triangulation from the constrained edge through sorted vertices
    // Sort by angle from edge midpoint
    const mx = (points[edgeA].x + points[edgeB].x) / 2;
    const my = (points[edgeA].y + points[edgeB].y) / 2;
    const edgeAngle = Math.atan2(points[edgeB].y - points[edgeA].y, points[edgeB].x - points[edgeA].x);

    sideVerts.sort((a, b) => {
        const angA = Math.atan2(points[a].y - my, points[a].x - mx) - edgeAngle;
        const angB = Math.atan2(points[b].y - my, points[b].x - mx) - edgeAngle;
        return angA - angB;
    });

    // Simple ear-clipping from the sorted list
    // Connect: edgeA → v0, v0 → v1, ..., vn → edgeB
    const all = [edgeA, ...sideVerts, edgeB];
    for (let i = 1; i < all.length - 1; i++) {
        outTris.push([edgeA, all[i], all[i + 1]]);
    }
}

function segmentsIntersect(ax, ay, bx, by, cx, cy, dx, dy) {
    const d1 = cross2D(cx, cy, dx, dy, ax, ay);
    const d2 = cross2D(cx, cy, dx, dy, bx, by);
    const d3 = cross2D(ax, ay, bx, by, cx, cy);
    const d4 = cross2D(ax, ay, bx, by, dx, dy);

    if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) &&
        ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
        return true;
    }

    return false; // Ignoring collinear cases for simplicity
}

// ============================================================
// REMOVE EXTERIOR AND HOLE TRIANGLES
// ============================================================

function removeExteriorTriangles(points, triangles, boundary, holes) {
    return triangles.filter(([a, b, c]) => {
        // Triangle centroid
        const cx = (points[a].x + points[b].x + points[c].x) / 3;
        const cy = (points[a].y + points[b].y + points[c].y) / 3;

        // Must be inside boundary
        if (!pointInPolygon(cx, cy, boundary)) return false;

        // Must not be inside any hole
        for (const hole of holes) {
            if (pointInPolygon(cx, cy, hole)) return false;
        }

        return true;
    });
}

function pointInPolygon(px, py, polygon) {
    let inside = false;
    for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
        const xi = polygon[i].x, yi = polygon[i].y;
        const xj = polygon[j].x, yj = polygon[j].y;
        if (((yi > py) !== (yj > py)) &&
            (px < (xj - xi) * (py - yi) / (yj - yi) + xi)) {
            inside = !inside;
        }
    }
    return inside;
}

// ============================================================
// RUPPERT'S REFINEMENT
// Improves triangle quality by inserting circumcenter points
// Used for base mesh quality (levels 1-4)
// ============================================================

function refineTriangulation(points, triangles, constrainedEdges, boundary, holes, maxArea, minAngle) {
    const MIN_ANGLE_RAD = minAngle * Math.PI / 180;
    const MAX_INSERTIONS = 5000;
    let insertions = 0;
    let rejects = 0;
    const MAX_REJECTS = 1000;

    points = [...points];
    triangles = [...triangles];

    while (insertions < MAX_INSERTIONS) {
        let worstIdx = -1;
        let worstScore = Infinity;

        for (let i = 0; i < triangles.length; i++) {
            const [a, b, c] = triangles[i];
            const area = triangleArea(points[a].x, points[a].y, points[b].x, points[b].y, points[c].x, points[c].y);
            const minAng = triangleMinAngle(points[a].x, points[a].y, points[b].x, points[b].y, points[c].x, points[c].y);

            if (minAng < MIN_ANGLE_RAD || area > maxArea) {
                const score = minAng < MIN_ANGLE_RAD ? minAng : (MIN_ANGLE_RAD + 1 / area);
                if (score < worstScore) {
                    worstScore = score;
                    worstIdx = i;
                }
            }
        }

        if (worstIdx === -1) break;

        const [a, b, c] = triangles[worstIdx];
        const cc = circumcircle(points[a].x, points[a].y, points[b].x, points[b].y, points[c].x, points[c].y);
        if (!cc) break;

        if (!pointInPolygon(cc.x, cc.y, boundary)) { rejects++; if (rejects > MAX_REJECTS) break; continue; }
        let inHole = false;
        for (const hole of holes) {
            if (pointInPolygon(cc.x, cc.y, hole)) { inHole = true; break; }
        }
        if (inHole) { rejects++; if (rejects > MAX_REJECTS) break; continue; }

        rejects = 0;
        insertions++;
        const newIdx = points.length;
        points.push({ x: cc.x, y: cc.y });

        const bad = [];
        for (let ti = 0; ti < triangles.length; ti++) {
            const [ta, tb, tc] = triangles[ti];
            if (inCircumcircle(cc.x, cc.y, points[ta].x, points[ta].y, points[tb].x, points[tb].y, points[tc].x, points[tc].y)) {
                bad.push(ti);
            }
        }

        const edgeCount = new Map();
        for (const ti of bad) {
            const [ta, tb, tc] = triangles[ti];
            const edges = [[ta, tb], [tb, tc], [tc, ta]];
            for (const [ea, eb] of edges) {
                const key = edgeKey(ea, eb);
                edgeCount.set(key, (edgeCount.get(key) || 0) + 1);
            }
        }

        const bdry = [];
        for (const ti of bad) {
            const [ta, tb, tc] = triangles[ti];
            const edges = [[ta, tb], [tb, tc], [tc, ta]];
            for (const [ea, eb] of edges) {
                const key = edgeKey(ea, eb);
                if (edgeCount.get(key) === 1) {
                    bdry.push([ea, eb]);
                }
            }
        }

        const badSet = new Set(bad);
        triangles = triangles.filter((_, i) => !badSet.has(i));

        for (const [ea, eb] of bdry) {
            triangles.push([newIdx, ea, eb]);
        }

        triangles = removeExteriorTriangles(points, triangles, boundary, holes);
    }

    return { points, triangles };
}

// ============================================================
// MID-EDGE SUBDIVISION
// Splits each triangle into 4 by inserting edge midpoints
// Same technique used by 3D modeling programs
// ============================================================

function midEdgeSubdivide(points, triangles) {
    const newPoints = [...points];
    const newTriangles = [];
    const edgeMidpoints = new Map(); // edgeKey -> midpoint vertex index

    function getOrCreateMidpoint(i, j) {
        const key = i < j ? `${i}_${j}` : `${j}_${i}`;
        if (edgeMidpoints.has(key)) return edgeMidpoints.get(key);
        const idx = newPoints.length;
        newPoints.push({
            x: (points[i].x + points[j].x) / 2,
            y: (points[i].y + points[j].y) / 2,
        });
        edgeMidpoints.set(key, idx);
        return idx;
    }

    for (const [a, b, c] of triangles) {
        const mab = getOrCreateMidpoint(a, b);
        const mbc = getOrCreateMidpoint(b, c);
        const mca = getOrCreateMidpoint(c, a);
        newTriangles.push(
            [a, mab, mca],
            [b, mbc, mab],
            [c, mca, mbc],
            [mab, mbc, mca],
        );
    }

    return { points: newPoints, triangles: newTriangles };
}

// ============================================================
// COMPACT MESH
// Removes orphaned vertices not used by any triangle
// ============================================================

function compactMesh(points, triangles) {
    const used = new Set();
    for (const [a, b, c] of triangles) {
        used.add(a); used.add(b); used.add(c);
    }

    const oldToNew = new Map();
    const newPoints = [];
    for (const idx of [...used].sort((a, b) => a - b)) {
        oldToNew.set(idx, newPoints.length);
        newPoints.push(points[idx]);
    }

    const newTriangles = triangles.map(([a, b, c]) => [oldToNew.get(a), oldToNew.get(b), oldToNew.get(c)]);
    return { points: newPoints, triangles: newTriangles };
}

// ============================================================
// MAIN ENTRY POINT
// ============================================================

/**
 * Generate a quality triangle mesh from a boundary polygon and optional holes.
 *
 * @param {Array<{x: number, z: number}>} boundary - Outer polygon vertices (uses x,z from TerrainMap)
 * @param {Array<Array<{x: number, z: number}>>} holes - Array of hole polygons
 * @param {number} subdivisionLevel - Controls mesh density (1-20, higher = more triangles)
 * @returns {{ vertices: Array<{x: number, y: number, z: number}>, triangles: Array<{a: number, b: number, c: number}> }}
 */
export function triangulateTerrain(boundary, holes = [], subdivisionLevel = 8) {
    if (boundary.length < 3) return { vertices: [], triangles: [] };

    // Convert from {x, z} (terrain coords) to {x, y} (2D triangulation coords)
    // We use XZ plane, so z maps to y in 2D
    const points2D = [];
    const boundaryIndices = [];

    // Add boundary vertices
    for (const v of boundary) {
        boundaryIndices.push(points2D.length);
        points2D.push({ x: v.x, y: v.z });
    }

    // Add hole vertices
    const holeIndicesArray = [];
    for (const hole of holes) {
        const holeIndices = [];
        for (const v of hole) {
            holeIndices.push(points2D.length);
            points2D.push({ x: v.x, y: v.z });
        }
        holeIndicesArray.push(holeIndices);
    }

    // Build constrained edges
    const constrainedEdges = [];

    // Boundary edges
    for (let i = 0; i < boundaryIndices.length; i++) {
        const next = (i + 1) % boundaryIndices.length;
        constrainedEdges.push([boundaryIndices[i], boundaryIndices[next]]);
    }

    // Hole edges
    for (const holeIndices of holeIndicesArray) {
        for (let i = 0; i < holeIndices.length; i++) {
            const next = (i + 1) % holeIndices.length;
            constrainedEdges.push([holeIndices[i], holeIndices[next]]);
        }
    }

    // Step 1: Basic Delaunay triangulation
    let triangles = bowyerWatson(points2D);

    // Step 2: Enforce constrained edges
    for (const [ei, ej] of constrainedEdges) {
        triangles = constrainEdge(points2D, triangles, ei, ej);
    }

    // Step 3: Remove exterior triangles (outside boundary or inside holes)
    const boundary2D = boundary.map(v => ({ x: v.x, y: v.z }));
    const holes2D = holes.map(h => h.map(v => ({ x: v.x, y: v.z })));
    triangles = removeExteriorTriangles(points2D, triangles, boundary2D, holes2D);

    // Ensure all triangles have consistent (CCW) winding
    for (let i = 0; i < triangles.length; i++) {
        const [a, b, c] = triangles[i];
        if (cross2D(points2D[a].x, points2D[a].y, points2D[b].x, points2D[b].y, points2D[c].x, points2D[c].y) < 0) {
            triangles[i] = [a, c, b]; // flip winding
        }
    }

    // Phase 1: Ruppert's refinement for base mesh quality (capped at level 4)
    const ruppLevel = Math.min(subdivisionLevel, 4);
    const bounds = boundary.reduce((acc, v) => ({
        minX: Math.min(acc.minX, v.x), maxX: Math.max(acc.maxX, v.x),
        minZ: Math.min(acc.minZ, v.z), maxZ: Math.max(acc.maxZ, v.z),
    }), { minX: Infinity, maxX: -Infinity, minZ: Infinity, maxZ: -Infinity });

    const totalArea = (bounds.maxX - bounds.minX) * (bounds.maxZ - bounds.minZ);
    const targetTriCount = ruppLevel * ruppLevel * 8;
    const maxArea = Math.max(totalArea / targetTriCount, 0.01);
    const minAngle = 25;

    let refined = refineTriangulation(points2D, triangles, constrainedEdges, boundary2D, holes2D, maxArea, minAngle);

    // Phase 2: Mid-edge subdivision for levels beyond 4 (4x triangles per pass)
    const subdivPasses = Math.max(0, subdivisionLevel - 4);
    for (let pass = 0; pass < subdivPasses; pass++) {
        refined = midEdgeSubdivide(refined.points, refined.triangles);
    }

    // Clean up orphaned vertices from Ruppert's
    refined = compactMesh(refined.points, refined.triangles);

    // Convert back to 3D vertices (y = 0 initially, height set by sculpting)
    const vertices3D = refined.points.map(p => ({ x: p.x, y: 0, z: p.y }));
    const triangles3D = refined.triangles.map(([a, b, c]) => ({ a, b, c }));

    return { vertices: vertices3D, triangles: triangles3D };
}
