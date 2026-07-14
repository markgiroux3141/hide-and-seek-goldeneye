// Terrain mode preview — boundary lines, holes, brush circle

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';
import { state } from '../state.js';
import { isPointerLocked } from '../input/input.js';
import { buildBoundaryLines, buildVertexMarkers, buildBrushCircle } from '../geometry/terrainGeometry.js';
import { terrainMeshes } from '../mesh/MeshManager.js';
import { getActiveTerrain } from '../tools/ToolManager.js';
import { scene } from '../scene/setup.js';

const terrainPreviewGroup = new THREE.Group();
let _added = false;
const terrainBoundaryMat = new THREE.LineBasicMaterial({ color: 0x00ff00, linewidth: 2 });
const terrainDrawingMat = new THREE.LineBasicMaterial({ color: 0xffff00, linewidth: 2 });
const terrainVertexMat = new THREE.LineBasicMaterial({ color: 0x00ffff, linewidth: 2 });
const terrainHoleMat = new THREE.LineBasicMaterial({ color: 0xff4444, linewidth: 2 });
const terrainBrushMat = new THREE.LineBasicMaterial({ color: 0xff8800, linewidth: 2, depthTest: false });

export function updateTerrainPreview(camera) {
    if (!_added) { scene.add(terrainPreviewGroup); _added = true; }
    while (terrainPreviewGroup.children.length > 0) {
        const child = terrainPreviewGroup.children[0];
        terrainPreviewGroup.remove(child);
        if (child.geometry) child.geometry.dispose();
    }

    if (state.editorMode !== 'terrain') return;

    const terrain = getActiveTerrain();
    if (!terrain) return;

    // Draw committed boundary (green)
    if (terrain.boundary.length >= 2) {
        const positions = buildBoundaryLines(terrain.boundary, terrain.isClosed);
        if (positions.length > 0) {
            const geo = new THREE.BufferGeometry();
            geo.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
            terrainPreviewGroup.add(new THREE.LineSegments(geo, terrainBoundaryMat));
        }
        const markers = buildVertexMarkers(terrain.boundary);
        if (markers.length > 0) {
            const markerGeo = new THREE.BufferGeometry();
            markerGeo.setAttribute('position', new THREE.Float32BufferAttribute(markers, 3));
            terrainPreviewGroup.add(new THREE.LineSegments(markerGeo, terrainVertexMat));
        }
    }

    // Draw committed holes (red)
    for (const hole of terrain.holes) {
        if (hole.length >= 2) {
            const positions = buildBoundaryLines(hole, true);
            if (positions.length > 0) {
                const geo = new THREE.BufferGeometry();
                geo.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
                terrainPreviewGroup.add(new THREE.LineSegments(geo, terrainHoleMat));
            }
        }
    }

    // Draw in-progress drawing (yellow)
    if (state.terrainDrawingPhase === 'drawing' && state.terrainDrawingVertices.length >= 1) {
        const verts = state.terrainDrawingVertices;
        if (verts.length >= 2) {
            const positions = buildBoundaryLines(verts, false);
            if (positions.length > 0) {
                const geo = new THREE.BufferGeometry();
                geo.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
                terrainPreviewGroup.add(new THREE.LineSegments(geo, terrainDrawingMat));
            }
        }
        const markers = buildVertexMarkers(verts, 0.7);
        if (markers.length > 0) {
            const markerGeo = new THREE.BufferGeometry();
            markerGeo.setAttribute('position', new THREE.Float32BufferAttribute(markers, 3));
            terrainPreviewGroup.add(new THREE.LineSegments(markerGeo, terrainDrawingMat));
        }
    }

    // Brush circle (sculpt mode, perspective view)
    if (state.terrainTool === 'sculpt' && state.terrainCameraMode === 'perspective' && isPointerLocked() && terrain.hasMesh) {
        const terrainMesh = terrainMeshes.get(terrain.id);
        if (terrainMesh) {
            const raycaster = new THREE.Raycaster();
            raycaster.setFromCamera(new THREE.Vector2(0, 0), camera);
            const intersects = raycaster.intersectObject(terrainMesh, false);
            if (intersects.length > 0) {
                const p = intersects[0].point;
                const W = WORLD_SCALE;
                const cx = p.x / W, cy = p.y / W, cz = p.z / W;
                const circlePositions = buildBrushCircle(cx, cy, cz, state.brushRadius, terrain);
                const circleGeo = new THREE.BufferGeometry();
                circleGeo.setAttribute('position', new THREE.Float32BufferAttribute(circlePositions, 3));
                terrainPreviewGroup.add(new THREE.LineSegments(circleGeo, terrainBrushMat));
            }
        }
    }
}
