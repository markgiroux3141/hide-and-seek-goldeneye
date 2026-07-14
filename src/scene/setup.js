// Three.js scene, renderer, camera, lighting

import * as THREE from 'three';
import { FOG_NEAR, FOG_FAR, INDOOR_BG_COLOR, DEFAULT_AMBIENT_INTENSITY } from '../core/constants.js';

export let scene, renderer, camera, gridHelper, ambientLight;

export function setAmbientIntensity(v) {
    if (ambientLight) ambientLight.intensity = v;
}

export function initScene() {
    scene = new THREE.Scene();
    scene.background = new THREE.Color(INDOOR_BG_COLOR);
    scene.fog = new THREE.Fog(INDOOR_BG_COLOR, FOG_NEAR, FOG_FAR);

    camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.1, 200);
    camera.position.set(2, 1.5, 2);

    renderer = new THREE.WebGLRenderer({ antialias: true });
    renderer.setSize(window.innerWidth, window.innerHeight);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    renderer.shadowMap.enabled = true;
    renderer.shadowMap.type = THREE.PCFShadowMap;
    document.body.appendChild(renderer.domElement);

    ambientLight = new THREE.AmbientLight(0xffffff, DEFAULT_AMBIENT_INTENSITY);
    scene.add(ambientLight);

    // Ground grid — 1 line per WT (0.25m spacing)
    gridHelper = new THREE.GridHelper(100, 400, 0x004400, 0x002200);
    gridHelper.position.y = -0.01;
    scene.add(gridHelper);

    // Resize
    window.addEventListener('resize', () => {
        camera.aspect = window.innerWidth / window.innerHeight;
        camera.updateProjectionMatrix();
        renderer.setSize(window.innerWidth, window.innerHeight);
    });

    return { scene, renderer, camera };
}
