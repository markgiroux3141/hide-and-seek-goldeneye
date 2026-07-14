// Platform — a rectangular slab at a given height
// Used for landings, balconies, and standalone platforms that stair runs connect to.

export class Platform {
    constructor(id, x, y, z, sizeX, sizeZ, thickness = 1) {
        this.id = id;
        this.x = x;            // min-corner X in WT units
        this.y = y;            // top surface Y in WT units
        this.z = z;            // min-corner Z in WT units
        this.sizeX = sizeX;    // X dimension in WT units (>= 1)
        this.sizeZ = sizeZ;    // Z dimension in WT units (>= 1)
        this.thickness = thickness; // slab depth in WT units (default 1)
        this.grounded = false;  // when true, extends down to Y=0
        this.railings = false;  // when true, adds railings to exposed edges
        this.style = 'default'; // visual style key — see src/geometry/platformStyles.js
    }

    // Computed bounds
    get maxX() { return this.x + this.sizeX; }
    get maxZ() { return this.z + this.sizeZ; }
    get bottomY() { return this.y - this.thickness; }

    // Center point
    get centerX() { return this.x + this.sizeX / 2; }
    get centerZ() { return this.z + this.sizeZ / 2; }

    // Get the world-space line segment for a given edge
    // Returns { start: {x,z}, end: {x,z} } in WT units
    getEdgeLine(edge) {
        switch (edge) {
            case 'xMin': return { start: { x: this.x, z: this.z }, end: { x: this.x, z: this.maxZ } };
            case 'xMax': return { start: { x: this.maxX, z: this.z }, end: { x: this.maxX, z: this.maxZ } };
            case 'zMin': return { start: { x: this.x, z: this.z }, end: { x: this.maxX, z: this.z } };
            case 'zMax': return { start: { x: this.x, z: this.maxZ }, end: { x: this.maxX, z: this.maxZ } };
        }
    }

    // Get the midpoint of an edge in WT units
    getEdgeMidpoint(edge) {
        const line = this.getEdgeLine(edge);
        return {
            x: (line.start.x + line.end.x) / 2,
            z: (line.start.z + line.end.z) / 2,
        };
    }

    // Get a point at offset t (0..1) along an edge, in WT units
    getEdgePointAtOffset(edge, t) {
        const line = this.getEdgeLine(edge);
        return {
            x: line.start.x + (line.end.x - line.start.x) * t,
            z: line.start.z + (line.end.z - line.start.z) * t,
        };
    }

    // Get the length of an edge in WT units
    getEdgeLength(edge) {
        return (edge === 'xMin' || edge === 'xMax') ? this.sizeZ : this.sizeX;
    }

    // Get the outward normal direction for an edge
    static edgeNormal(edge) {
        switch (edge) {
            case 'xMin': return { x: -1, z: 0 };
            case 'xMax': return { x: 1, z: 0 };
            case 'zMin': return { x: 0, z: -1 };
            case 'zMax': return { x: 0, z: 1 };
        }
    }

    toJSON() {
        return {
            id: this.id,
            x: this.x, y: this.y, z: this.z,
            sizeX: this.sizeX, sizeZ: this.sizeZ,
            thickness: this.thickness,
            grounded: this.grounded,
            railings: this.railings,
            style: this.style,
        };
    }

    static fromJSON(j) {
        const p = new Platform(j.id, j.x, j.y, j.z, j.sizeX, j.sizeZ, j.thickness ?? 1);
        p.grounded = j.grounded ?? false;
        p.railings = j.railings ?? false;
        p.style = j.style ?? 'default';
        return p;
    }
}
