// Simple-style platform & stair geometry — single-plane top + skirt + L-shaped
// corner pillars for platforms; tread + riser + diagonal side planes for stairs.
// Reuses PlatformGeometryBuilder and helpers from platformGeometry.js.

import {
    PlatformGeometryBuilder,
    toWorld,
    resolveStairAnchor,
    computeStairRunAxis,
    isEdgeAgainstWall,
    findFloorYAt,
} from './platformGeometry.js';

const PILLAR_WIDTH = 0.5; // WT — width of each L-pillar leg

// Texture zones used by the simple style:
//   0 = top surface / treads (floor_doorframe)
//   3 = vertical surfaces: skirt, risers, stair sides, pillars (blue_stairs)

// ============================================================
// SIMPLE PLATFORM GEOMETRY
// ============================================================

export function buildSimplePlatformGeometry(platform, options = {}) {
    const builder = new PlatformGeometryBuilder();

    const { x, y, z, sizeX, sizeZ, thickness, grounded } = platform;
    const xMin = x;
    const xMax = x + sizeX;
    const zMin = z;
    const zMax = z + sizeZ;
    const yTop = y;
    const yBot = y - thickness;

    const textured = options.viewMode === 'textured';
    const topZone = 0;
    const sideZone = textured ? 3 : 0;
    const w = sizeX;
    const d = sizeZ;

    // UV conventions for the simple style (combined with repeat=1.0 in the
    // simple_blue scheme so 1 UV unit = 1 texture tile):
    //   - Floor texture (top, treads): tile in BOTH directions, scaled by WT.
    //   - Blue texture (vertical surfaces): fit 1 tile in the height direction,
    //     tile along the width based on WT.

    // 1) Top plane (+Y) — floor texture, tile both directions by sizeX × sizeZ
    builder.addQuad(
        [xMin, yTop, zMin],
        [xMin, yTop, zMax],
        [xMax, yTop, zMax],
        [xMax, yTop, zMin],
        false, topZone,
        ...(textured ? [[0, 0], [0, d], [w, d], [w, 0]] : []),
    );

    // 2) Skirt — 4 vertical quads from yTop down to yBot, one per edge.
    //    Each fits 1 tile vertically, tiles horizontally by edge length.
    //    Front (-Z)  width = sizeX
    builder.addQuad(
        [xMin, yBot, zMin],
        [xMin, yTop, zMin],
        [xMax, yTop, zMin],
        [xMax, yBot, zMin],
        false, sideZone,
        ...(textured ? [[0, 0], [0, 1], [w, 1], [w, 0]] : []),
    );
    //    Back (+Z)   width = sizeX
    builder.addQuad(
        [xMax, yBot, zMax],
        [xMax, yTop, zMax],
        [xMin, yTop, zMax],
        [xMin, yBot, zMax],
        false, sideZone,
        ...(textured ? [[0, 0], [0, 1], [w, 1], [w, 0]] : []),
    );
    //    Left (-X)   width = sizeZ
    builder.addQuad(
        [xMin, yBot, zMax],
        [xMin, yTop, zMax],
        [xMin, yTop, zMin],
        [xMin, yBot, zMin],
        false, sideZone,
        ...(textured ? [[0, 0], [0, 1], [d, 1], [d, 0]] : []),
    );
    //    Right (+X)  width = sizeZ
    builder.addQuad(
        [xMax, yBot, zMin],
        [xMax, yTop, zMin],
        [xMax, yTop, zMax],
        [xMax, yBot, zMax],
        false, sideZone,
        ...(textured ? [[0, 0], [0, 1], [d, 1], [d, 0]] : []),
    );

    // 3) Corner pillars — only when grounded; each corner contributes up to two
    //    perpendicular planes from yBot down to Y=0. A plane is omitted when
    //    the platform edge that owns it is against a CSG wall.
    if (grounded) {
        const brushes = options.brushes || [];
        // Probe wider than the railing check so that walls near (but not
        // exactly flush with) the edge still skip the pillar — and probe at
        // mid-pillar height so we test the space the pillar will actually
        // occupy, not the air just under the platform top.
        const wallProbe = { probeDist: 1.5, yProbe: yBot * 0.5 };
        const xMinAgainstWall = isEdgeAgainstWall(platform, 'xMin', brushes, wallProbe);
        const xMaxAgainstWall = isEdgeAgainstWall(platform, 'xMax', brushes, wallProbe);
        const zMinAgainstWall = isEdgeAgainstWall(platform, 'zMin', brushes, wallProbe);
        const zMaxAgainstWall = isEdgeAgainstWall(platform, 'zMax', brushes, wallProbe);

        const yPillarTop = yBot;
        const yPillarBot = findFloorYAt(x + sizeX / 2, z + sizeZ / 2, yBot, brushes);
        const pH = yPillarTop - yPillarBot;

        // Helper: add a vertical plane between two world points facing outward.
        // The natural normal of vertices (ax,yBot,az), (ax,yTop,az), (bx,yTop,bz),
        // (bx,yBot,bz) — computed via e1 × e2 = (0,H,0) × (tx,H,tz) — is
        // (H*tz, 0, -H*tx). Flip when this points opposite to normalOutward.
        //
        // Pillar UVs are 90° rotated relative to the skirt: u runs along the
        // pillar's vertical (p0→p1), v runs across PILLAR_WIDTH (p0→p3). With
        // u tiled by pH and v fitted to 1, the texture's "horizontal" axis
        // tiles up the pillar height while its "vertical" axis fits across
        // the narrow leg.
        const addPillarPlane = (ax, az, bx, bz, normalOutward) => {
            const tx = bx - ax, tz = bz - az;
            const nxNatural = tz, nzNatural = -tx;
            const dot = nxNatural * normalOutward.x + nzNatural * normalOutward.z;
            const flip = dot < 0;
            builder.addQuad(
                [ax, yPillarBot, az],
                [ax, yPillarTop, az],
                [bx, yPillarTop, bz],
                [bx, yPillarBot, bz],
                flip, sideZone,
                ...(textured ? [[0, 0], [pH, 0], [pH, 1], [0, 1]] : []),
            );
        };

        // For each corner, two pillar planes, each owned by one of the two
        // adjacent edges. A plane is built only if its owning edge is NOT
        // against a wall. The plane runs PILLAR_WIDTH along that edge,
        // anchored at the corner.

        // Corner (xMin, zMin) — owned by xMin and zMin
        if (!zMinAgainstWall) {
            // plane along zMin, runs from corner along +X for PILLAR_WIDTH
            addPillarPlane(xMin, zMin, xMin + PILLAR_WIDTH, zMin, { x: 0, z: -1 });
        }
        if (!xMinAgainstWall) {
            addPillarPlane(xMin, zMin, xMin, zMin + PILLAR_WIDTH, { x: -1, z: 0 });
        }

        // Corner (xMax, zMin) — owned by xMax and zMin
        if (!zMinAgainstWall) {
            addPillarPlane(xMax - PILLAR_WIDTH, zMin, xMax, zMin, { x: 0, z: -1 });
        }
        if (!xMaxAgainstWall) {
            addPillarPlane(xMax, zMin, xMax, zMin + PILLAR_WIDTH, { x: 1, z: 0 });
        }

        // Corner (xMax, zMax) — owned by xMax and zMax
        if (!zMaxAgainstWall) {
            addPillarPlane(xMax - PILLAR_WIDTH, zMax, xMax, zMax, { x: 0, z: 1 });
        }
        if (!xMaxAgainstWall) {
            addPillarPlane(xMax, zMax - PILLAR_WIDTH, xMax, zMax, { x: 1, z: 0 });
        }

        // Corner (xMin, zMax) — owned by xMin and zMax
        if (!zMaxAgainstWall) {
            addPillarPlane(xMin, zMax, xMin + PILLAR_WIDTH, zMax, { x: 0, z: 1 });
        }
        if (!xMinAgainstWall) {
            addPillarPlane(xMin, zMax - PILLAR_WIDTH, xMin, zMax, { x: -1, z: 0 });
        }
    }

    return builder.build();
}

// ============================================================
// SIMPLE STAIR GEOMETRY
// ============================================================

export function buildSimpleStairGeometry(stairRun, fromPlatform, toPlatform, options = {}) {
    const builder = new PlatformGeometryBuilder();

    const fromPt = resolveStairAnchor(fromPlatform, stairRun.anchorFrom);
    const toPt = resolveStairAnchor(toPlatform, stairRun.anchorTo);

    const topPt = fromPt.y >= toPt.y ? fromPt : toPt;
    const bottomPt = fromPt.y >= toPt.y ? toPt : fromPt;
    const topPlatform = fromPt.y >= toPt.y ? fromPlatform : toPlatform;
    const bottomPlatform = fromPt.y >= toPt.y ? toPlatform : fromPlatform;
    const topAnchor = fromPt.y >= toPt.y ? stairRun.anchorFrom : stairRun.anchorTo;
    const bottomAnchor = fromPt.y >= toPt.y ? stairRun.anchorTo : stairRun.anchorFrom;

    const rise = topPt.y - bottomPt.y;
    if (rise === 0) return builder.build();

    const { runAxis } = computeStairRunAxis(topPlatform, topAnchor, bottomPlatform, bottomAnchor, topPt, bottomPt);

    const topRun = runAxis === 'x' ? topPt.x : topPt.z;
    const bottomRun = runAxis === 'x' ? bottomPt.x : bottomPt.z;

    const halfWidth = stairRun.width / 2;
    const topPerp = runAxis === 'x' ? topPt.z : topPt.x;
    const perpMin = topPerp - halfWidth;
    const perpMax = topPerp + halfWidth;

    const steps = Math.max(1, Math.round(rise / stairRun.stepHeight));
    const totalRun = bottomRun - topRun;
    const stepRise = rise / steps;
    const stepRun = totalRun / steps;
    const stairBaseY = bottomPt.y;

    const xorFlip = (runAxis === 'x') !== (stepRun < 0);

    const textured = options.viewMode === 'textured';
    const topZone = 0;
    const sideZone = textured ? 3 : 0;
    const stepWidth = perpMax - perpMin;
    const absStepRun = Math.abs(stepRun);

    // Riser is intentionally shorter than the full step rise so a horizontal
    // slit is visible between consecutive steps when looking from the side —
    // matches the original game stairs.
    const RISER_FRACTION = 0.55;
    const riserHeight = stepRise * RISER_FRACTION;

    // Per step: tread (+Y) and a short front riser. No left/right side walls —
    // the two sloped stringers below carry the visual weight.
    for (let i = 0; i < steps; i++) {
        const rFront = topRun + (steps - i) * stepRun;
        const rBack = topRun + (steps - i - 1) * stepRun;
        const stepTopY = stairBaseY + (i + 1) * stepRise;

        // Tread — floor texture, tile both directions, oriented with u along
        // the perpendicular (width) axis instead of along the run direction
        // (90° from the previous orientation).
        builder.addQuad(
            toWorld(runAxis, rBack, stepTopY, perpMin),
            toWorld(runAxis, rFront, stepTopY, perpMin),
            toWorld(runAxis, rFront, stepTopY, perpMax),
            toWorld(runAxis, rBack, stepTopY, perpMax),
            xorFlip, topZone,
            ...(textured ? [[0, 0], [0, absStepRun], [stepWidth, absStepRun], [stepWidth, 0]] : []),
        );

        // Riser — same convention as platform skirt: u tiles horizontally
        // across the width by stepWidth, v fits 1 tile vertically.
        const riserBotY = stepTopY - riserHeight;
        builder.addQuad(
            toWorld(runAxis, rFront, riserBotY, perpMin),
            toWorld(runAxis, rFront, riserBotY, perpMax),
            toWorld(runAxis, rFront, stepTopY, perpMax),
            toWorld(runAxis, rFront, stepTopY, perpMin),
            xorFlip, sideZone,
            ...(textured ? [[0, 0], [stepWidth, 0], [stepWidth, 1], [0, 1]] : []),
        );
    }

    // Two side "stringer" boards — one at perpMin, one at perpMax. Each is a
    // sloped parallelogram whose top edge passes through the step nosings
    // (front-top corners), so each tread sits on top of the stringer with a
    // visible gap beneath the riser.
    //
    //   back-top  = (topRun + stepRun, topPt.y)               topmost nosing
    //   front-top = (frontRun, stairBaseY + stepRise)         bottom-step nosing
    //   bottom edge offset down by BOARD_DEPTH (= one stepRise),
    //   so front-bottom lands exactly on the floor at stairBaseY.
    //
    // The stringer starts one stepRun in from the upper platform's edge —
    // the topmost tread bridges that small gap to the platform, exactly as in
    // the original game.
    const frontRun = topRun + steps * stepRun;
    const stringerBackRun = topRun + stepRun;
    const stringerFrontRun = frontRun;
    const stringerBackTopY = topPt.y;
    const stringerFrontTopY = stairBaseY + stepRise;
    const BOARD_DEPTH = stepRise;
    const stringerBackBotY = stringerBackTopY - BOARD_DEPTH;
    const stringerFrontBotY = stringerFrontTopY - BOARD_DEPTH;
    const slopeLen = Math.hypot(
        stringerFrontRun - stringerBackRun,
        stringerFrontTopY - stringerBackTopY,
    );

    // Stringer UV: u runs along p0→p1 (the slope direction) and tiles by
    // slopeLen; v runs across the board's vertical depth and fits to 1 tile.
    // The texture is aligned with the runner: long axis along the slope,
    // short axis across the board thickness.

    // perpMin (left) side  —  p0: front-bot, p1: back-bot, p2: back-top, p3: front-top
    builder.addQuad(
        toWorld(runAxis, stringerFrontRun, stringerFrontBotY, perpMin),
        toWorld(runAxis, stringerBackRun, stringerBackBotY, perpMin),
        toWorld(runAxis, stringerBackRun, stringerBackTopY, perpMin),
        toWorld(runAxis, stringerFrontRun, stringerFrontTopY, perpMin),
        !xorFlip, sideZone,
        ...(textured ? [[0, 0], [slopeLen, 0], [slopeLen, 1], [0, 1]] : []),
    );

    // perpMax (right) side  —  p0: back-bot, p1: front-bot, p2: front-top, p3: back-top
    builder.addQuad(
        toWorld(runAxis, stringerBackRun, stringerBackBotY, perpMax),
        toWorld(runAxis, stringerFrontRun, stringerFrontBotY, perpMax),
        toWorld(runAxis, stringerFrontRun, stringerFrontTopY, perpMax),
        toWorld(runAxis, stringerBackRun, stringerBackTopY, perpMax),
        !xorFlip, sideZone,
        ...(textured ? [[0, 0], [slopeLen, 0], [slopeLen, 1], [0, 1]] : []),
    );

    // Bridge sections — fill the small horizontal gap under the topmost tread
    // between the upper platform edge (topRun) and where the sloped stringer
    // begins (topRun + stepRun). Axis-aligned rectangles, top flush with the
    // upper platform top, bottom flush with the stringer's back-bot.
    const bridgeRun = topRun;                        // back end (at upper platform edge)
    const bridgeFrontRun = topRun + stepRun;         // front end (where stringer starts)
    const bridgeTopY = topPt.y;
    const bridgeBotY = topPt.y - BOARD_DEPTH;

    // perpMin (left) side  —  p0: front-bot, p1: back-bot, p2: back-top, p3: front-top
    builder.addQuad(
        toWorld(runAxis, bridgeFrontRun, bridgeBotY, perpMin),
        toWorld(runAxis, bridgeRun, bridgeBotY, perpMin),
        toWorld(runAxis, bridgeRun, bridgeTopY, perpMin),
        toWorld(runAxis, bridgeFrontRun, bridgeTopY, perpMin),
        !xorFlip, sideZone,
        ...(textured ? [[0, 0], [absStepRun, 0], [absStepRun, 1], [0, 1]] : []),
    );

    // perpMax (right) side  —  p0: back-bot, p1: front-bot, p2: front-top, p3: back-top
    builder.addQuad(
        toWorld(runAxis, bridgeRun, bridgeBotY, perpMax),
        toWorld(runAxis, bridgeFrontRun, bridgeBotY, perpMax),
        toWorld(runAxis, bridgeFrontRun, bridgeTopY, perpMax),
        toWorld(runAxis, bridgeRun, bridgeTopY, perpMax),
        !xorFlip, sideZone,
        ...(textured ? [[0, 0], [absStepRun, 0], [absStepRun, 1], [0, 1]] : []),
    );

    return builder.build();
}
