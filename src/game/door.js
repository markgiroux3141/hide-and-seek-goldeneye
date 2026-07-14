// door — breakable door panels for the HUNT phase.
//
// Doors are authored with the existing door tool (they leave an `isDoorframe`
// brush marking the opening). At hunt start each doorframe becomes a breakable
// panel: a solid-looking mesh + a dynamic cost overlay on the nav grid. Hunters
// prefer open routes but will breach a door when it's the shortest way to the
// player; breaching flips a flag the pathfinder reads live (no nav re-bake).

import * as THREE from 'three';
import { state } from '../state.js';
import { WORLD_SCALE } from '../core/constants.js';

export const DOOR_HP = 2.5;   // seconds of sustained breaching to break a door

function doorMesh(scene, b) {
    const S = WORLD_SCALE;
    const geo = new THREE.BoxGeometry(b.w * S, b.h * S, b.d * S);
    const mat = new THREE.MeshStandardMaterial({ color: 0x7a4a22, roughness: 0.85, transparent: true, opacity: 0.9 });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.set((b.x + b.w / 2) * S, (b.y + b.h / 2) * S, (b.z + b.d / 2) * S);
    scene.add(mesh);
    return mesh;
}

// Build door records + a doorGrid overlay for `nav` from all isDoorframe brushes,
// then attach them to the nav world. Returns the door records (with .mesh).
export function buildDoors(scene, nav) {
    const frames = (state.csg.brushes || []).filter(b => b.isDoorframe);
    const doors = [];
    const doorGrid = new Uint16Array(nav.nx * nav.ny * nav.nz);

    frames.forEach((b, i) => {
        const door = { id: i + 1, broken: false, hp: DOOR_HP, hpMax: DOOR_HP, mesh: doorMesh(scene, b) };
        doors.push(door);
        const marker = i + 1; // doorGrid stores (index+1); nav reads doors[marker-1]
        const ixLo = Math.max(0, Math.floor(b.x - nav.x0)), ixHi = Math.min(nav.nx - 1, Math.ceil(b.x + b.w - nav.x0) - 1);
        const iyLo = Math.max(0, Math.floor(b.y - nav.y0)), iyHi = Math.min(nav.ny - 1, Math.ceil(b.y + b.h - nav.y0) - 1);
        const izLo = Math.max(0, Math.floor(b.z - nav.z0)), izHi = Math.min(nav.nz - 1, Math.ceil(b.z + b.d - nav.z0) - 1);
        for (let iy = iyLo; iy <= iyHi; iy++)
            for (let iz = izLo; iz <= izHi; iz++)
                for (let ix = ixLo; ix <= ixHi; ix++) {
                    const cx = nav.x0 + ix + 0.5, cy = nav.y0 + iy + 0.5, cz = nav.z0 + iz + 0.5;
                    if (cx >= b.x && cx < b.x + b.w && cy >= b.y && cy < b.y + b.h && cz >= b.z && cz < b.z + b.d)
                        doorGrid[nav.idx(ix, iy, iz)] = marker;
                }
    });

    nav.setDoors(doors, doorGrid);
    return doors;
}
