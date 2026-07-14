// BrushDef — a single CSG brush (additive or subtractive) with optional per-face taper.
// Ported from spike/csg/main.js (BrushDef class + applyTaperToBoxGeo).

export class BrushDef {
    constructor(id, op, x, y, z, w, h, d) {
        this.id = id;
        this.op = op;                  // 'add' | 'subtract'
        this.x = x; this.y = y; this.z = z;
        this.w = w; this.h = h; this.d = d;
        // Per-face taper: key = 'x-min'|'x-max'|'y-min'|'y-max'|'z-min'|'z-max'
        // value = { u: number, v: number } — symmetric inset in WT on each edge
        this.taper = {};
        this.isDoorframe = false;      // door frame brush (zone 5 walls + zone 6 floor)
        this.isHoleFrame = false;      // generic hole frame brush (zone 5 all sides)
        this.isBrace = false;          // structural brace brush (all faces zone 7)
        // Stair-step brush (one column of a stair-push operation). The riser
        // face — whose normal sign matches stairCarveSign along stairAxis —
        // gets routed to zone 5 (stair_gradient). Other faces classify normally.
        this.isStairStep = false;
        this.stairAxis = null;         // 'x' | 'z' — wall normal axis being carved
        this.stairCarveSign = 0;       // +1 | -1 — sign of the riser face normal along stairAxis
        // Stair void brush (single envelope brush for deferred stair system)
        this.isStairVoid = false;
        this.stairDescriptorId = null; // links to csgStairs[] entry
        this.schemeKey = 'facility_white_tile';
        // Per-face scheme overrides: key = 'x-min'|'x-max'|'y-min'|'y-max'|'z-min'|'z-max',
        // value = scheme name string. Read by uvZones.js, falls back to schemeKey when absent.
        this.schemeOverrides = {};
        // Per-triangle zone overrides for Face Paint correction. Each entry:
        // { cx, cy, cz, zone } in world space. Matched within TRI_ZONE_EPS during CSG.
        this.triZoneOverrides = [];
        this.floorY = y;               // WT-space anchor for wall texture vertical split
    }

    hasSchemeOverrides() { return Object.keys(this.schemeOverrides).length > 0; }

    hasTriZoneOverrides() { return this.triZoneOverrides.length > 0; }

    hasTaper() { return Object.keys(this.taper).length > 0; }

    getFaces() {
        return [
            { brushId: this.id, axis: 'x', side: 'min', pos: this.x },
            { brushId: this.id, axis: 'x', side: 'max', pos: this.x + this.w },
            { brushId: this.id, axis: 'y', side: 'min', pos: this.y },
            { brushId: this.id, axis: 'y', side: 'max', pos: this.y + this.h },
            { brushId: this.id, axis: 'z', side: 'min', pos: this.z },
            { brushId: this.id, axis: 'z', side: 'max', pos: this.z + this.d },
        ];
    }

    get minX() { return this.x; }  get maxX() { return this.x + this.w; }
    get minY() { return this.y; }  get maxY() { return this.y + this.h; }
    get minZ() { return this.z; }  get maxZ() { return this.z + this.d; }

    clone() {
        const b = new BrushDef(this.id, this.op, this.x, this.y, this.z, this.w, this.h, this.d);
        b.taper = JSON.parse(JSON.stringify(this.taper));
        b.isDoorframe = this.isDoorframe;
        b.isHoleFrame = this.isHoleFrame;
        b.isBrace = this.isBrace;
        b.isStairStep = this.isStairStep;
        b.stairAxis = this.stairAxis;
        b.stairCarveSign = this.stairCarveSign;
        b.isStairVoid = this.isStairVoid;
        b.stairDescriptorId = this.stairDescriptorId;
        b.schemeKey = this.schemeKey;
        b.schemeOverrides = JSON.parse(JSON.stringify(this.schemeOverrides));
        b.triZoneOverrides = this.triZoneOverrides.map(o => ({ ...o }));
        b.floorY = this.floorY;
        return b;
    }

    toJSON() {
        const j = {
            id: this.id, op: this.op,
            x: this.x, y: this.y, z: this.z,
            w: this.w, h: this.h, d: this.d,
        };
        if (this.hasTaper()) j.taper = this.taper;
        if (this.isDoorframe) j.isDoorframe = true;
        if (this.isHoleFrame) j.isHoleFrame = true;
        if (this.isBrace) j.isBrace = true;
        if (this.isStairStep) {
            j.isStairStep = true;
            j.stairAxis = this.stairAxis;
            j.stairCarveSign = this.stairCarveSign;
        }
        if (this.isStairVoid) {
            j.isStairVoid = true;
            j.stairDescriptorId = this.stairDescriptorId;
        }
        if (this.schemeKey !== 'facility_white_tile') j.schemeKey = this.schemeKey;
        if (this.hasSchemeOverrides()) j.schemeOverrides = this.schemeOverrides;
        if (this.hasTriZoneOverrides()) j.triZoneOverrides = this.triZoneOverrides;
        if (this.floorY !== this.y) j.floorY = this.floorY;
        return j;
    }

    static fromJSON(j) {
        const b = new BrushDef(j.id, j.op, j.x, j.y, j.z, j.w, j.h, j.d);
        if (j.taper) b.taper = j.taper;
        if (j.isDoorframe) b.isDoorframe = true;
        if (j.isHoleFrame) b.isHoleFrame = true;
        if (j.isBrace) b.isBrace = true;
        if (j.isStairStep) {
            b.isStairStep = true;
            b.stairAxis = j.stairAxis;
            b.stairCarveSign = j.stairCarveSign;
        }
        if (j.isStairVoid) {
            b.isStairVoid = true;
            b.stairDescriptorId = j.stairDescriptorId;
        }
        if (j.schemeKey) b.schemeKey = j.schemeKey;
        if (j.schemeOverrides) b.schemeOverrides = j.schemeOverrides;
        if (j.triZoneOverrides) b.triZoneOverrides = j.triZoneOverrides;
        if (j.floorY !== undefined) b.floorY = j.floorY;
        return b;
    }
}
