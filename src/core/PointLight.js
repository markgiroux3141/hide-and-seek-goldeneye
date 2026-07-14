// PointLight — a positionable light source for baked vertex lighting
// Follows the same pattern as Platform.js

import { DEFAULT_LIGHT_INTENSITY, DEFAULT_LIGHT_RANGE } from './constants.js';

export class PointLight {
    constructor(id, x, y, z) {
        this.id = id;
        this.x = x;            // position X in WT units
        this.y = y;            // position Y in WT units
        this.z = z;            // position Z in WT units
        this.color = { r: 1, g: 1, b: 1 };  // normalized 0-1 RGB
        this.intensity = DEFAULT_LIGHT_INTENSITY;
        this.range = DEFAULT_LIGHT_RANGE;
        this.enabled = true;
        this.castShadow = true;
    }

    toJSON() {
        return {
            id: this.id,
            x: this.x, y: this.y, z: this.z,
            color: { ...this.color },
            intensity: this.intensity,
            range: this.range,
            enabled: this.enabled,
            castShadow: this.castShadow,
        };
    }

    static fromJSON(j) {
        const l = new PointLight(j.id, j.x, j.y, j.z);
        l.color = j.color ? { ...j.color } : { r: 1, g: 1, b: 1 };
        l.intensity = j.intensity ?? DEFAULT_LIGHT_INTENSITY;
        l.range = j.range ?? DEFAULT_LIGHT_RANGE;
        l.enabled = j.enabled ?? true;
        l.castShadow = j.castShadow ?? true;
        return l;
    }
}
