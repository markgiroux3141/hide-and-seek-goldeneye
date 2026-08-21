#!/usr/bin/env python3
"""The sentry gun's rig: how the six OBJ pieces assemble, and what moves.

The GoldenEye editor exported this prop as an **exploded parts sheet** — six disjoint
pieces parked on three Z shelves (-325 / 0 / +325 GE units), each shelf internally
assembled but the shelves not assembled to each other. So there is no authored
skeleton to recover; the assembly is ours to write, and this file is it.

The shelves, identified by rendering each component on its own (`--mode components`):

    shelf  comps  what it is
    -325   c1+c0  gun housing (0.80 x 0.50 x 0.25 m) + 6-barrel gatling bundle
                  (0.60 m long). Already correctly joined: the bundle abuts the
                  housing's +X face at X=100, so the **bore axis is +X**.
       0   c3+c4  a vertical fin (0.10 x 0.40 x 0.30 m) standing on a long thin
                  prism (0.10 x 0.10 x 0.40 m). The fin is the yaw shaft, the
                  prism the trunnion the gun pitches on.
    +325   c2+c5  a vented wedge housing (0.60 x 0.20 x 0.25 m) with a round-dish
                  decal plane inside it — the ceiling-mounted base.

Assembled into a hanging turret, four nodes deep:

    MOUNT (static, bolted to the ceiling)   cowl c2 + dish c5
      └─ YAW   (spins about +Y)             fin c3 + trunnion c4
           └─ PITCH (spins about +Z)        housing c1
                └─ SPIN (spins about +X)    barrel bundle c0

Everything here is in **GoldenEye units** (1000 = 1 m), matching the raw OBJ, so the
numbers can be read against the component bounds directly.

Two placements were tried and the first was wrong, visibly: hanging the gun from a
pivot at its top-front corner (the obvious reading of "the shaft holds the gun") makes
the 0.8 m housing scythe upward through the ceiling plate the moment it pitches down.
The trunnion belongs on the **bore line**, with the housing's length balanced across
it, which is where a real gatling's trunnion is and which keeps the swept volume under
the mount.
"""

from __future__ import annotations

import math

#: Bore height in assembled units — the barrel bundle's own centreline once the gun
#: shelf is placed. Every part that defines the pitch axis is pinned to this so the
#: gun rotates about the line it shoots along.
BORE_Y = -697.0

#: The rig, one row per OBJ connected component:
#:     comp -> (node, scale(x,y,z), offset(x,y,z))
#: applied as `p' = p * scale + offset`, then the node's animated rotation. Scale is
#: per-axis and about the model origin, which is what lets the yaw fin stretch to close
#: the gap between the ceiling plate and the trunnion without a second mesh.
PARTS = {
    # ── MOUNT: the ceiling plate. Top face (Y=+50) raised to Y=0 and centred on the
    #    hang axis, so it reads as bolted flat to the ceiling.
    2: ("mount", (1.0, 1.0, 1.0), (300.0, -50.0, -325.0)),
    5: ("mount", (1.0, 1.0, 1.0), (300.0, -50.0, -325.0)),  # dish decal, rides the cowl
    # ── YAW: the drop shaft and the trunnion it carries. The fin is stretched 1.24x in
    #    Y so it spans plate-to-trunnion exactly (497 units) instead of leaving an 87
    #    unit gap at its natural 400.
    3: ("yaw", (1.0, 1.2425, 1.0), (300.0, -647.0, 0.0)),   # fin -> Y[-647,-150]
    4: ("yaw", (1.0, 1.0, 1.0), (300.0, -647.0, 0.0)),      # trunnion -> Y[-747,-647]
    # ── PITCH / SPIN: the gun. Shifted +300 in X so the housing's length is balanced
    #    across the trunnion rather than hanging off behind it.
    1: ("pitch", (1.0, 1.0, 1.0), (300.0, -650.0, 325.0)),  # housing -> X[-400,400]
    0: ("spin", (1.0, 1.0, 1.0), (300.0, -650.0, 325.0)),   # barrels -> X[400,1000]
}

#: Component -> node, the flat view of [`PARTS`] the preview colours by.
NODE_OF_COMP = {c: v[0] for c, v in PARTS.items()}

#: The yaw axis: vertical, through the turret's hang point.
YAW_AXIS_POINT = (0.0, 0.0, 0.0)

#: The pitch axis: horizontal, along +Z, through the trunnion — which is on the bore.
PITCH_AXIS_POINT = (0.0, BORE_Y, 0.0)

#: The barrel spin axis: along the bore (+X). Z=-2 is the bundle's own centreline,
#: which the export left 2 units off the housing's.
SPIN_AXIS_POINT = (0.0, BORE_Y, -2.0)

#: The muzzle, in assembled GE units: the +X end of the bundle, on the bore line.
#: Tracers and the muzzle flash spawn here, carried by yaw+pitch at runtime.
MUZZLE = (1000.0, BORE_Y, -2.0)

#: Assembled turret extents: X[-400,1000] Y[-1050,0] Z[-250,250] — 1.40 m long and
#: hanging 1.05 m below the ceiling at raw export scale. A room in this world is 8
#: world-tiles = **2.0 m** tall, so raw scale would drop the gun to head height and
#: make it longer than half the room — the same N64-scale trap the door props hit.
#: 0.45 gives a 0.63 m gun hanging 0.47 m: a turret you duck under.
RIG_SCALE = 0.45

#: Rotation applied to the whole assembly so the finished prop faces the engine's
#: convention (forward = -Z) instead of the OBJ's bore-along-+X. Radians about Y.
BASE_YAW = math.pi / 2

#: Articulation limits, in radians. Yaw is unlimited (it hangs from a ring). Pitch is
#: clamped to what the mount can actually swing: past about -50 the housing's back
#: corner rises into the ceiling plate it hangs from, and above +15 it would be firing
#: at that plate.
PITCH_MIN = math.radians(-50.0)
PITCH_MAX = math.radians(15.0)

#: Barrel spin-up: how fast the bundle turns at full song, in radians/sec.
SPIN_RATE = math.radians(1080.0)
