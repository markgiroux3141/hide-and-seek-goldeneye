// Texture scheme registry — loaded from public/textureSchemes.json
// Each scheme maps zone indices to texture names + repeat scales.
// Zone indices match the material array:
//   0 = floor, 1 = ceiling, 2 = lower wall, 3 = upper wall,
//   4 = tunnel legacy, 5 = tunnel sides/top, 6 = tunnel floor

export let TEXTURE_SCHEMES = {};

// Load schemes from JSON config. Call at startup before initMaterials().
export async function loadTextureSchemes() {
    const resp = await fetch('public/textureSchemes.json');
    const data = await resp.json();

    // Convert color strings to numeric values for Three.js
    for (const scheme of Object.values(data)) {
        for (const zone of Object.values(scheme.zones)) {
            if (zone.color && typeof zone.color === 'string') {
                zone.color = parseInt(zone.color.replace('#', ''), 16);
            }
        }
    }

    TEXTURE_SCHEMES = data;
}

// Build a lookup from key ('1', '2', ...) to scheme name
export function getSchemeByKey(key) {
    for (const [name, scheme] of Object.entries(TEXTURE_SCHEMES)) {
        if (scheme.key === key) return name;
    }
    return null;
}
