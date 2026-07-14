// TerrainMap — a terrain surface defined by a boundary polygon, optional holes,
// and a triangulated heightfield mesh for outdoor levels.

export class TerrainMap {
    constructor(id) {
        this.id = id;
        this.boundary = [];          // [{x, z}] outer polygon vertices (clockwise)
        this.holes = [];             // [[{x, z}]] array of hole polygons (counterclockwise)
        this.subdivisionLevel = 8;   // controls max triangle area (higher = more triangles)
        this.vertices = [];          // [{x, y, z}] 3D vertices after triangulation
        this.triangles = [];         // [{a, b, c}] index triples into vertices array
        this.textureScheme = 'terrain_grass';

        // Boundary wall settings
        this.wallStyle = 'plane';    // 'plane' | 'rocky'
        this.wallHeight = 20;        // height above terrain in WT units (plane walls)
        this.rockyWallHeight = 50;   // height above terrain in WT units (rocky walls)
        this.wallNoiseFreq = 0.3;    // noise frequency for rocky walls
        this.wallNoiseAmp = 2;       // noise amplitude for rocky walls
        this.wallSubdivRows = 6;     // vertical subdivision rows for rocky walls
    }

    // Whether the boundary is closed (at least 3 vertices forming a polygon)
    get isClosed() {
        return this.boundary.length >= 3;
    }

    // Whether the mesh has been generated
    get hasMesh() {
        return this.vertices.length > 0 && this.triangles.length > 0;
    }

    // Get the bounding box of the boundary in WT units
    getBounds() {
        if (this.boundary.length === 0) return null;
        let minX = Infinity, maxX = -Infinity;
        let minZ = Infinity, maxZ = -Infinity;
        for (const v of this.boundary) {
            if (v.x < minX) minX = v.x;
            if (v.x > maxX) maxX = v.x;
            if (v.z < minZ) minZ = v.z;
            if (v.z > maxZ) maxZ = v.z;
        }
        return { minX, maxX, minZ, maxZ };
    }

    // Get the center of the boundary
    getCenter() {
        const bounds = this.getBounds();
        if (!bounds) return { x: 0, z: 0 };
        return {
            x: (bounds.minX + bounds.maxX) / 2,
            z: (bounds.minZ + bounds.maxZ) / 2,
        };
    }

    // Check if a point {x, z} is inside the boundary polygon (ray casting algorithm)
    containsPoint(px, pz) {
        return TerrainMap.pointInPolygon(px, pz, this.boundary);
    }

    // Check if a point is inside any hole
    isInHole(px, pz) {
        for (const hole of this.holes) {
            if (TerrainMap.pointInPolygon(px, pz, hole)) return true;
        }
        return false;
    }

    // Static ray-casting point-in-polygon test
    static pointInPolygon(px, pz, polygon) {
        let inside = false;
        for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
            const xi = polygon[i].x, zi = polygon[i].z;
            const xj = polygon[j].x, zj = polygon[j].z;
            if (((zi > pz) !== (zj > pz)) &&
                (px < (xj - xi) * (pz - zi) / (zj - zi) + xi)) {
                inside = !inside;
            }
        }
        return inside;
    }

    // Get the height (Y) at a given XZ position by finding the triangle
    // containing that point and interpolating vertex heights
    getHeightAt(px, pz) {
        if (!this.hasMesh) return 0;

        for (const tri of this.triangles) {
            const a = this.vertices[tri.a];
            const b = this.vertices[tri.b];
            const c = this.vertices[tri.c];

            const bary = TerrainMap.barycentric(px, pz, a.x, a.z, b.x, b.z, c.x, c.z);
            if (bary && bary.u >= 0 && bary.v >= 0 && bary.w >= 0) {
                return bary.u * a.y + bary.v * b.y + bary.w * c.y;
            }
        }
        return 0;
    }

    // Barycentric coordinates of point (px, pz) in triangle (ax,az, bx,bz, cx,cz)
    static barycentric(px, pz, ax, az, bx, bz, cx, cz) {
        const v0x = cx - ax, v0z = cz - az;
        const v1x = bx - ax, v1z = bz - az;
        const v2x = px - ax, v2z = pz - az;

        const dot00 = v0x * v0x + v0z * v0z;
        const dot01 = v0x * v1x + v0z * v1z;
        const dot02 = v0x * v2x + v0z * v2z;
        const dot11 = v1x * v1x + v1z * v1z;
        const dot12 = v1x * v2x + v1z * v2z;

        const denom = dot00 * dot11 - dot01 * dot01;
        if (Math.abs(denom) < 1e-10) return null;

        const invDenom = 1 / denom;
        const u = (dot11 * dot02 - dot01 * dot12) * invDenom;
        const v = (dot00 * dot12 - dot01 * dot02) * invDenom;
        const w = 1 - u - v;

        return { u: w, v, w: u }; // u=weight of A, v=weight of B, w=weight of C
    }

    toJSON() {
        return {
            id: this.id,
            boundary: this.boundary,
            holes: this.holes,
            subdivisionLevel: this.subdivisionLevel,
            vertices: this.vertices,
            triangles: this.triangles,
            textureScheme: this.textureScheme,
            wallStyle: this.wallStyle,
            wallHeight: this.wallHeight,
            rockyWallHeight: this.rockyWallHeight,
            wallNoiseFreq: this.wallNoiseFreq,
            wallNoiseAmp: this.wallNoiseAmp,
            wallSubdivRows: this.wallSubdivRows,
        };
    }

    static fromJSON(j) {
        const t = new TerrainMap(j.id);
        t.boundary = j.boundary || [];
        t.holes = j.holes || [];
        t.subdivisionLevel = j.subdivisionLevel ?? 8;
        t.vertices = j.vertices || [];
        t.triangles = j.triangles || [];
        t.textureScheme = j.textureScheme || 'terrain_grass';
        t.wallStyle = j.wallStyle || 'plane';
        t.wallHeight = j.wallHeight ?? 20;
        t.rockyWallHeight = j.rockyWallHeight ?? 50;
        t.wallNoiseFreq = j.wallNoiseFreq ?? 0.3;
        t.wallNoiseAmp = j.wallNoiseAmp ?? 2;
        t.wallSubdivRows = j.wallSubdivRows ?? 6;
        return t;
    }
}
