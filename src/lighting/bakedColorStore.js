// Baked color storage — stores and reapplies baked vertex colors
// Separated from lightBaker.js to avoid circular imports with mesh modules.

// Stored baked color arrays, keyed by mesh type + id
// e.g., 'vol_3' → Float32Array, 'plat_1' → Float32Array
const bakedColorStore = new Map();

export function storeBakedColors(key, geometry) {
    const colors = geometry.getAttribute('color');
    if (!colors) return;
    bakedColorStore.set(key, new Float32Array(colors.array));
}

// Re-apply stored baked colors to a rebuilt geometry.
// Called after rebuildVolume/rebuildPlatform when bakedLighting is true.
export function reapplyBakedColors(key, geometry) {
    const stored = bakedColorStore.get(key);
    if (!stored) return false;

    const colors = geometry.getAttribute('color');
    if (!colors || colors.array.length !== stored.length) return false;

    for (let i = 0; i < colors.count; i++) {
        const r = colors.getX(i);
        const g = colors.getY(i);
        const b = colors.getZ(i);

        // Detect if this vertex was highlighted (green tint: r=0.4, g=1.0, b=0.4)
        const isHighlighted = (r < 0.5 && g > 0.9 && b < 0.5);

        if (isHighlighted) {
            // Blend baked color with highlight
            colors.setXYZ(i,
                stored[i * 3] * 0.4,
                stored[i * 3 + 1] * 1.0,
                stored[i * 3 + 2] * 0.4,
            );
        } else {
            colors.setXYZ(i, stored[i * 3], stored[i * 3 + 1], stored[i * 3 + 2]);
        }
    }

    colors.needsUpdate = true;
    return true;
}

export function clearBakedColorStore() {
    bakedColorStore.clear();
}
