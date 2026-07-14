// Light mesh lifecycle — visual representation of point lights in the editor.
// Each editor light has both a visual icon (octahedron + core sphere) and an
// active THREE.PointLight that renders the realtime preview.

import * as THREE from 'three';
import { state } from '../state.js';
import { WORLD_SCALE } from '../core/constants.js';
import { scene } from '../scene/setup.js';

const S = WORLD_SCALE;

// Light icon mesh storage: Map<lightId, THREE.Group>
export const lightMeshes = new Map();

// Active THREE.PointLight per editor light: Map<lightId, THREE.PointLight>
const realtimeLights = new Map();

function colorToHex(c) {
    return new THREE.Color(c.r, c.g, c.b).getHex();
}

export function rebuildLight(light) {
    const old = lightMeshes.get(light.id);
    if (old) {
        scene.remove(old);
        old.traverse(child => {
            if (child.geometry) child.geometry.dispose();
            if (child.material) child.material.dispose();
        });
    }

    const group = new THREE.Group();
    const hex = colorToHex(light.color);

    const iconGeo = new THREE.OctahedronGeometry(0.5 * S, 0);
    const iconMat = new THREE.MeshBasicMaterial({ color: hex, wireframe: true, depthTest: false });
    const icon = new THREE.Mesh(iconGeo, iconMat);
    icon.renderOrder = 998;
    icon.userData = { lightId: light.id, part: 'icon' };
    group.add(icon);

    const coreGeo = new THREE.SphereGeometry(0.15 * S, 8, 6);
    const coreMat = new THREE.MeshBasicMaterial({ color: hex, depthTest: false });
    const core = new THREE.Mesh(coreGeo, coreMat);
    core.renderOrder = 998;
    core.userData = { lightId: light.id, part: 'core' };
    group.add(core);

    group.position.set(light.x * S, light.y * S, light.z * S);
    group.userData = { lightId: light.id };

    lightMeshes.set(light.id, group);
    scene.add(group);

    const oldRT = realtimeLights.get(light.id);
    if (oldRT) {
        oldRT.shadow.map?.dispose();
        oldRT.shadow.map = null;
        scene.remove(oldRT);
    }

    const rtLight = new THREE.PointLight(
        new THREE.Color(light.color.r, light.color.g, light.color.b),
        light.intensity,
        light.range * S,
        2,
    );
    rtLight.position.set(light.x * S, light.y * S, light.z * S);
    rtLight.castShadow = light.castShadow && light.enabled;
    rtLight.visible = light.enabled;
    rtLight.shadow.mapSize.set(512, 512);
    rtLight.shadow.bias = -0.001;
    realtimeLights.set(light.id, rtLight);
    scene.add(rtLight);
}

export function rebuildAllLights() {
    for (const [, group] of lightMeshes) {
        scene.remove(group);
        group.traverse(child => {
            if (child.geometry) child.geometry.dispose();
            if (child.material) child.material.dispose();
        });
    }
    lightMeshes.clear();

    for (const [, rt] of realtimeLights) {
        rt.shadow.map?.dispose();
        rt.shadow.map = null;
        scene.remove(rt);
    }
    realtimeLights.clear();

    for (const light of state.pointLights) {
        rebuildLight(light);
    }
}

export function removeLightMesh(lightId) {
    const group = lightMeshes.get(lightId);
    if (group) {
        scene.remove(group);
        group.traverse(child => {
            if (child.geometry) child.geometry.dispose();
            if (child.material) child.material.dispose();
        });
        lightMeshes.delete(lightId);
    }

    const rtLight = realtimeLights.get(lightId);
    if (rtLight) {
        rtLight.shadow.map?.dispose();
        rtLight.shadow.map = null;
        scene.remove(rtLight);
        realtimeLights.delete(lightId);
    }
}

// Toggle a light's shadow casting without rebuilding its PointLight.
// Disposes the shadow map when turning off so old shadows don't linger in some drivers.
export function updateLightShadowFlag(light) {
    const rt = realtimeLights.get(light.id);
    if (!rt) return;
    rt.castShadow = light.castShadow && light.enabled;
    if (!rt.castShadow) {
        rt.shadow.map?.dispose();
        rt.shadow.map = null;
    }
}

export function updateLightSelection() {}

export function getLightPickTargets() {
    const targets = [];
    for (const [, group] of lightMeshes) {
        for (const child of group.children) {
            if (child.userData.part === 'icon' || child.userData.part === 'core') {
                targets.push(child);
            }
        }
    }
    return targets;
}
