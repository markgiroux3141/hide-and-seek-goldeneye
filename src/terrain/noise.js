// Simple 2D Perlin-style noise for terrain sculpting and rocky wall generation
// Based on improved Perlin noise with permutation table

const PERM = new Uint8Array(512);
const GRAD = [
    [1, 1], [-1, 1], [1, -1], [-1, -1],
    [1, 0], [-1, 0], [0, 1], [0, -1],
];

// Initialize with a seed
export function seedNoise(seed = 0) {
    const p = new Uint8Array(256);
    for (let i = 0; i < 256; i++) p[i] = i;

    // Fisher-Yates shuffle with simple LCG seeded RNG
    let s = seed;
    for (let i = 255; i > 0; i--) {
        s = (s * 1664525 + 1013904223) & 0xffffffff;
        const j = ((s >>> 0) % (i + 1));
        [p[i], p[j]] = [p[j], p[i]];
    }

    for (let i = 0; i < 512; i++) PERM[i] = p[i & 255];
}

// Initialize with default seed
seedNoise(42);

function fade(t) {
    return t * t * t * (t * (t * 6 - 15) + 10);
}

function lerp(a, b, t) {
    return a + t * (b - a);
}

function grad2d(hash, x, y) {
    const g = GRAD[hash & 7];
    return g[0] * x + g[1] * y;
}

// 2D Perlin noise, returns value in roughly [-1, 1]
export function noise2D(x, y) {
    const xi = Math.floor(x) & 255;
    const yi = Math.floor(y) & 255;
    const xf = x - Math.floor(x);
    const yf = y - Math.floor(y);

    const u = fade(xf);
    const v = fade(yf);

    const aa = PERM[PERM[xi] + yi];
    const ab = PERM[PERM[xi] + yi + 1];
    const ba = PERM[PERM[xi + 1] + yi];
    const bb = PERM[PERM[xi + 1] + yi + 1];

    return lerp(
        lerp(grad2d(aa, xf, yf), grad2d(ba, xf - 1, yf), u),
        lerp(grad2d(ab, xf, yf - 1), grad2d(bb, xf - 1, yf - 1), u),
        v
    );
}

// Fractional Brownian Motion — layered noise for more natural terrain
export function fbm2D(x, y, octaves = 4, lacunarity = 2, gain = 0.5) {
    let value = 0;
    let amplitude = 1;
    let frequency = 1;
    let maxAmplitude = 0;

    for (let i = 0; i < octaves; i++) {
        value += amplitude * noise2D(x * frequency, y * frequency);
        maxAmplitude += amplitude;
        amplitude *= gain;
        frequency *= lacunarity;
    }

    return value / maxAmplitude;
}
