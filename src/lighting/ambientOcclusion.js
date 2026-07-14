// Ambient occlusion — hemisphere sampling for per-vertex darkening in corners/crevices

import * as THREE from 'three';
import { WORLD_SCALE } from '../core/constants.js';

const S = WORLD_SCALE;
const MAX_AO_DIST = 8 * S; // max distance for AO rays (8 WT units)

const _raycaster = new THREE.Raycaster();
const _origin = new THREE.Vector3();
const _sampleDir = new THREE.Vector3();
const _tangent = new THREE.Vector3();
const _bitangent = new THREE.Vector3();

// Pre-computed cosine-weighted hemisphere samples (generated once)
const sampleCache = new Map(); // numSamples → [{x, y, z}]

function generateHemisphereSamples(n) {
    if (sampleCache.has(n)) return sampleCache.get(n);

    const samples = [];
    // Use stratified sampling with cosine weighting
    for (let i = 0; i < n; i++) {
        // Use quasi-random Hammersley sequence for better distribution
        const u1 = i / n;
        let u2 = 0;
        let bits = i;
        for (let j = 0; j < 16; j++) {
            u2 += (bits & 1) / (2 << j);
            bits >>= 1;
        }

        // Cosine-weighted hemisphere: concentrates samples near the normal
        const cosTheta = Math.sqrt(1 - u1);
        const sinTheta = Math.sqrt(u1);
        const phi = 2 * Math.PI * u2;

        samples.push({
            x: sinTheta * Math.cos(phi),
            y: cosTheta,  // up (along normal)
            z: sinTheta * Math.sin(phi),
        });
    }

    sampleCache.set(n, samples);
    return samples;
}

// Build a tangent-space basis from a normal vector
function buildTangentBasis(normal) {
    // Pick a vector not parallel to normal to compute tangent
    const up = Math.abs(normal.y) < 0.999
        ? new THREE.Vector3(0, 1, 0)
        : new THREE.Vector3(1, 0, 0);

    _tangent.crossVectors(normal, up).normalize();
    _bitangent.crossVectors(normal, _tangent).normalize();
}

// Compute AO factor for a single vertex (0 = fully occluded, 1 = fully open)
// vertPos: THREE.Vector3 (world space)
// vertNormal: THREE.Vector3 (normalized)
// occluders: array of THREE.Mesh
// numSamples: number of hemisphere samples (32-64)
export function computeAO(vertPos, vertNormal, occluders, numSamples) {
    const samples = generateHemisphereSamples(numSamples);
    buildTangentBasis(vertNormal);

    // Offset origin along normal
    _origin.copy(vertPos).addScaledVector(vertNormal, 0.02);

    _raycaster.near = 0;
    _raycaster.far = MAX_AO_DIST;

    let occluded = 0;

    for (const sample of samples) {
        // Transform sample from tangent space to world space
        _sampleDir.set(0, 0, 0)
            .addScaledVector(_tangent, sample.x)
            .addScaledVector(vertNormal, sample.y)
            .addScaledVector(_bitangent, sample.z)
            .normalize();

        _raycaster.set(_origin, _sampleDir);
        const hits = _raycaster.intersectObjects(occluders, false);

        if (hits.length > 0) {
            // Weight by proximity — closer occlusion is stronger
            const hitDist = hits[0].distance;
            const weight = 1 - (hitDist / MAX_AO_DIST);
            occluded += weight;
        }
    }

    // Return 0-1 factor (1 = no occlusion, 0 = fully occluded)
    return 1 - (occluded / numSamples);
}
