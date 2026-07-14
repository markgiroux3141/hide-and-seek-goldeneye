// Destructive level bake:
//   • Strips the auto-resizing CSG shell (brushId === -1) — the editor's
//     outer container, never part of the final level.
//   • Otherwise emits every authored triangle as-is. The user does any
//     further cleanup with the post-bake click-to-select / Delete-to-delete
//     tool (handleIndoorClick + indoorKeys baked branch).
//
// Result is merged into a single mesh group at state.bakedMesh, then the
// source CSG + cave state is torn down. state.isBaked gates further edits.
// GLB export is a separate action (Ctrl+E via GLBExporter) so the user can
// run cleanup between bake and export.

import * as THREE from 'three';
import { state } from '../state.js';
import { scene } from '../scene/setup.js';
import { csgRegionMeshes, rebuildAllCSG } from '../mesh/csgMesh.js';
import { getEntry, disposeAllCaves } from '../mesh/caveMesh.js';
import { initBakedPrisms } from '../tools/indoorClick.js';

// ─── Public entry point ─────────────────────────────────────────────
// Freezes CSG + caves into a single non-editable mesh group at state.bakedMesh.
// Does NOT download a GLB — that's a separate action via GLBExporter so the
// user can run post-bake cleanup (click-to-delete triangles) before export.
export function bakeLevel() {
    if (state.isBaked) {
        alert('Level is already baked. Reload a pre-bake save to edit further.');
        return;
    }
    const confirmed = confirm(
        'Bake will remove the ability to edit CSG and caves. Save first if you haven\'t!\n\nContinue?',
    );
    if (!confirmed) return;

    const merged = buildBakedScene();
    if (!merged) { alert('Nothing to bake.'); return; }

    // Destructive teardown: collapse CSG + caves into a single frozen mesh.
    freezeIntoScene(merged);
}

// ─── Scene builder ──────────────────────────────────────────────────
// Returns a Scene containing one THREE.Mesh per CSG region (shell stripped)
// plus one mesh per cave (full chunk soup, no culling). All cleanup beyond
// the editor-shell strip is the user's job via the post-bake click-select +
// Delete tool. Ancillary systems (platforms, lights, terrain) are left
// untouched in state and re-included at GLB-export time.
function buildBakedScene() {
    const root = new THREE.Scene();

    let any = false;

    for (const data of csgRegionMeshes.values()) {
        const regionMesh = bakeRegionMesh(data);
        if (regionMesh) { root.add(regionMesh); any = true; }

        const region = data.region;
        for (const cave of region.caves) {
            const entry = getEntry(region.id, cave.id);
            if (!entry) continue;
            const caveMesh = bakeCaveEntry(entry);
            if (caveMesh) { root.add(caveMesh); any = true; }
        }
    }

    return any ? root : null;
}

// ─── CSG region freeze ──────────────────────────────────────────────
// Drops only the auto-resizing shell triangles (brushId === -1) — that's the
// editor's outer container, never part of the final level. Every other
// triangle is preserved verbatim so the user can decide what to delete.
function bakeRegionMesh(data) {
    const { mesh, faceIds } = data;
    const geo = mesh.geometry;
    if (!geo || !faceIds) return null;

    const index = geo.index;
    if (!index) return null;

    // Triangle -> material index from existing groups.
    const srcGroups = geo.groups.length > 0
        ? geo.groups
        : [{ start: 0, count: index.count, materialIndex: 0 }];
    const triCount = index.count / 3;
    const triMat = new Int32Array(triCount);
    for (const g of srcGroups) {
        const t0 = g.start / 3, t1 = (g.start + g.count) / 3;
        for (let t = t0; t < t1; t++) triMat[t] = g.materialIndex;
    }

    const keptIdx = [];
    const newGroups = [];
    let runMat = -1, runStart = 0, runCount = 0;
    const flush = () => { if (runCount > 0) newGroups.push({ start: runStart, count: runCount, materialIndex: runMat }); };

    for (let t = 0; t < faceIds.length; t++) {
        const f = faceIds[t];
        if (f && f.brushId === -1) continue;            // shell

        const a = index.getX(t * 3), b = index.getX(t * 3 + 1), c = index.getX(t * 3 + 2);
        const mat = triMat[t];
        if (mat !== runMat) { flush(); runMat = mat; runStart = keptIdx.length; runCount = 0; }
        keptIdx.push(a, b, c);
        runCount += 3;
    }
    flush();
    if (keptIdx.length === 0) return null;

    const out = new THREE.BufferGeometry();
    for (const name of ['position', 'normal', 'uv', 'color']) {
        const attr = geo.getAttribute(name);
        if (attr) out.setAttribute(name, attr);
    }
    out.setIndex(keptIdx);
    for (const g of newGroups) out.addGroup(g.start, g.count, g.materialIndex);
    out.computeBoundingBox();
    out.computeBoundingSphere();
    return new THREE.Mesh(out, Array.isArray(mesh.material) ? mesh.material.slice() : mesh.material);
}

// ─── Cave freeze ────────────────────────────────────────────────────
// Each cave is a THREE.Group of per-chunk meshes (non-indexed). We merge
// all chunks into one BufferGeometry verbatim — every MC triangle is kept.
function bakeCaveEntry(entry) {
    const positions = [];
    const normals = [];
    const uvs = [];

    for (const mesh of entry.chunkMeshes.values()) {
        const geo = mesh.geometry;
        const p = geo.getAttribute('position');
        const n = geo.getAttribute('normal');
        const u = geo.getAttribute('uv');
        if (!p) continue;
        const triCount = p.count / 3;
        for (let t = 0; t < triCount; t++) {
            for (let i = 0; i < 3; i++) {
                const vi = t * 3 + i;
                positions.push(p.getX(vi), p.getY(vi), p.getZ(vi));
                if (n) normals.push(n.getX(vi), n.getY(vi), n.getZ(vi));
                if (u) uvs.push(u.getX(vi), u.getY(vi));
            }
        }
    }

    if (positions.length === 0) return null;

    const geom = new THREE.BufferGeometry();
    geom.setAttribute('position', new THREE.BufferAttribute(new Float32Array(positions), 3));
    if (normals.length) geom.setAttribute('normal', new THREE.BufferAttribute(new Float32Array(normals), 3));
    if (uvs.length) geom.setAttribute('uv', new THREE.BufferAttribute(new Float32Array(uvs), 2));
    geom.computeBoundingBox();
    geom.computeBoundingSphere();

    // Take the first chunk's material as the cave material.
    const firstChunk = entry.chunkMeshes.values().next().value;
    const mat = firstChunk ? firstChunk.material : new THREE.MeshStandardMaterial({ color: 0x6b5a48 });
    return new THREE.Mesh(geom, mat);
}

// ─── Destructive teardown ───────────────────────────────────────────
// Clone the baked scene into the live scene as state.bakedMesh, then wipe
// all CSG + cave state. Platforms/lights/terrain stay editable.
function freezeIntoScene(bakedScene) {
    // Snapshot cave anchor faces (axis/side/position + u/v rect in WT) for
    // the post-bake cleanup-prism tool. Done first because the next steps
    // dispose region.caves.
    state.bakedAnchors = [];
    for (const data of csgRegionMeshes.values()) {
        for (const cave of data.region.caves) {
            for (const f of (cave.anchorFaces || [])) {
                state.bakedAnchors.push({
                    axis: f.axis, side: f.side, position: f.position,
                    u0: f.u0, u1: f.u1, v0: f.v0, v1: f.v1,
                });
            }
        }
    }

    // Clone the baked meshes so disposal of CSG/cave meshes doesn't kill
    // shared geometry attributes still referenced by bakedScene.
    const bakedGroup = new THREE.Group();
    bakedGroup.name = 'bakedLevel';
    for (const child of bakedScene.children) {
        if (!child.isMesh) continue;
        const cloneGeo = child.geometry.clone();
        const mesh = new THREE.Mesh(cloneGeo, child.material);
        mesh.receiveShadow = true;
        bakedGroup.add(mesh);
    }
    scene.add(bakedGroup);

    // Dispose cave worlds + their chunk meshes.
    disposeAllCaves();

    // Clear CSG authoring state.
    state.csg.brushes = [];
    state.csg.bakedBrushes = [];
    state.csg.nextBrushId = 1;
    state.csg.nextCaveId = 1;
    state.csg.totalBakedBrushes = 0;
    state.csg.csgStairs = [];
    state.csg.nextCsgStairId = 1;
    state.csg.selectedFace = null;
    state.csg.selectedFaces = [];
    state.csg.activeBrush = null;
    state.csg.activeOp = null;
    state.csg.activeSide = null;
    state.csg.pendingStairOp = null;
    state.csg.holeMode = false;
    state.csg.braceMode = false;
    state.csg.pillarMode = false;
    state.csg.facePaintMode = false;

    // rebuildAllCSG with empty brushes tears down the region meshes cleanly.
    rebuildAllCSG();

    state.isBaked = true;
    state.bakedMesh = bakedGroup;

    // Build the cleanup-prism wireframe overlays from the snapshot above.
    initBakedPrisms();
}
