// CaveDef — a single voxel-carved cavity anchored to one or more CSG faces.
// Owned by a CSGRegion (`region.caves[]`); dies with the region.
//
// Voxel data lives in the CaveWorld (Rust/WASM) — CaveDef only holds metadata
// the JS side needs:
//   • anchorFaces[]: faces to delete + stitch at bake time. First entry is
//     the source-room face (from startCaveFromFace); each exit room placed
//     via P appends its own cave-side face.
//   • extentAabb: live AABB of carved voxels (world meters). Drives the
//     cave's boundary clip envelope.
//   • protoInit: seed params consumed by caveMesh on first-time world creation.

export class CaveDef {
    constructor(id, regionId) {
        this.id = id;
        this.regionId = regionId;

        // Every cave-anchored face in the region that should be deleted +
        // stitched to cave mesh at bake. Entries shape (WT coords):
        //   { brushId, axis, side, position, u0, u1, v0, v1 }
        this.anchorFaces = [];

        // Live voxel extent in world meters. Updated by caveSculpt as the
        // user carves. Used to size the cave's boundary clip envelope.
        this.extentAabb = null;

        // Seed cavity params (world meters) for first-time world creation.
        // { centerX, centerY, centerZ, radius, amp, freq }
        this.protoInit = null;
    }

    toJSON() {
        return {
            id: this.id,
            regionId: this.regionId,
            anchorFaces: this.anchorFaces,
            // extentAabb + voxel data rebuilt from WASM on load (Phase 6).
        };
    }

    static fromJSON(j) {
        const c = new CaveDef(j.id, j.regionId);
        c.anchorFaces = Array.isArray(j.anchorFaces) ? j.anchorFaces : [];
        return c;
    }
}
