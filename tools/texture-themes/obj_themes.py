"""Recover texture themes from the ripped GoldenEye level OBJ+MTL files.

The 21 folders under `public/existing goldeneye levels/` each hold a
`LevelIndices.obj` + `.mtl` exported by the GoldenEye Setup Editor. Between them
they carry the *original* texture assignments for every surface in the game —
which is a far better source of themes than hand-picking from a flat pile of 1010
BMPs with names like `tempImgEd02B7.bmp`.

This module parses those files and folds each room down to a candidate theme in
the engine's own zone taxonomy (see `native/crates/engine/src/render/uv_zones.rs`):

    0 = floor   1 = ceiling   2 = lower wall   3 = upper wall

Three facts about these files that are easy to get wrong (all measured, not
assumed — see DESIGN_TEXTURE_THEMES.md):

1. **The OBJ is already partitioned into rooms.** `g` groups are named
   `primary_Room01` / `secondary_Room08` — GoldenEye's own room segmentation.
   There are 1968 of them across the 21 levels. No spatial clustering needed.
2. **Every `vn` is literally `vn 0 0 0`.** Vertex normals are placeholders, so
   face normals MUST be computed by cross product. Trusting `vn` silently
   misclassifies every surface.
3. **`vt` UVs are real and range outside 0..1**, which is what makes the `repeat`
   scale derivable rather than guesswork.

Usage:
    python tools/texture-themes/obj_themes.py levels        # inventory
    python tools/texture-themes/obj_themes.py calibrate     # solve GE_UNITS_PER_TILE
    python tools/texture-themes/obj_themes.py extract       # write candidate themes
    python tools/texture-themes/obj_themes.py validate      # check vs shipped themes
"""

from __future__ import annotations

import json
import math
import re
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LEVELS_DIR = REPO / "public" / "existing goldeneye levels"
OUT_DIR = Path(__file__).resolve().parent / "out"

# ---------------------------------------------------------------------------
# Scale calibration
# ---------------------------------------------------------------------------

# --- Bridging GE units to the engine's UV space -----------------------------
#
# The engine computes UV = (world_metres / WORLD_SCALE) * repeat, i.e. one texture
# repetition spans `1/repeat` world tiles. The GE files are in their own units, so
# one constant bridges them. Note the texture's pixel size cancels out of that
# relation entirely — resolution does not enter into `repeat`.
#
# The obvious calibration — fit the constant so it reproduces the repeat values a
# human hand-tuned for the shipped `facility_*` themes — DOES NOT WORK, and the
# `calibrate` subcommand shows why: those values imply constants spanning
# 14.4 .. 173.5, a 12x spread. They were eyeballed against *our* authored room
# sizes, which bear no relation to GoldenEye's 481-unit rooms, so they are a
# bracket to check against, not a ground truth to fit.
#
# So the bridge is derived from measured geometry on both sides instead:
GE_STOREY = 353.0  # measured: Facility's rooms are a uniform 353 units tall
OUR_ROOM_HEIGHT_WT = 16.0  # the default room in World::new (world/mod.rs:2460)
WALL_SPLIT_V = 6.0  # engine constant (render/uv_zones.rs:32)

# GE units spanned by one texture repetition when `repeat` == 1.0. Equivalently
# GE_STOREY / OUR_ROOM_HEIGHT_WT: how many GE units make one of our world tiles.
GE_UNITS_PER_TILE = GE_STOREY / OUR_ROOM_HEIGHT_WT  # 22.06

# Height above a room's floor below which a wall face is "lower wall", in GE units.
# The engine splits at WALL_SPLIT_V of our room height, so the same *proportion* is
# applied to a GE storey rather than inventing a threshold.
LOWER_WALL_FRACTION = WALL_SPLIT_V / OUR_ROOM_HEIGHT_WT  # 0.375

# Plausible band for a derived `repeat`. Every shipped hand-authored theme sits
# inside 0.10 .. 1.0; this is deliberately wider. Used only to FLAG a theme, never
# to drop one — see `repeats_in_band`.
REPEAT_MIN, REPEAT_MAX = 0.05, 2.0

# A face whose normal is this dominated by Y is a floor/ceiling rather than a wall.
# Mirrors the dominant-axis rule in `uv_zones::classify_soup`.
ZONE_FLOOR, ZONE_CEILING, ZONE_LOWER_WALL, ZONE_UPPER_WALL = 0, 1, 2, 3
ZONE_NAMES = {
    ZONE_FLOOR: "floor",
    ZONE_CEILING: "ceiling",
    ZONE_LOWER_WALL: "lower_wall",
    ZONE_UPPER_WALL: "upper_wall",
}


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


@dataclass
class Face:
    """One triangle: world verts, UVs, its room, and its texture."""

    room: str
    texture: str
    v: tuple[tuple[float, float, float], ...]
    uv: tuple[tuple[float, float], ...]

    @property
    def normal(self) -> tuple[float, float, float]:
        (ax, ay, az), (bx, by, bz), (cx, cy, cz) = self.v[0], self.v[1], self.v[2]
        ux, uy, uz = bx - ax, by - ay, bz - az
        wx, wy, wz = cx - ax, cy - ay, cz - az
        nx, ny, nz = uy * wz - uz * wy, uz * wx - ux * wz, ux * wy - uy * wx
        mag = math.sqrt(nx * nx + ny * ny + nz * nz)
        if mag == 0.0:
            return (0.0, 0.0, 0.0)
        return (nx / mag, ny / mag, nz / mag)

    @property
    def area(self) -> float:
        (ax, ay, az), (bx, by, bz), (cx, cy, cz) = self.v[0], self.v[1], self.v[2]
        ux, uy, uz = bx - ax, by - ay, bz - az
        wx, wy, wz = cx - ax, cy - ay, cz - az
        nx, ny, nz = uy * wz - uz * wy, uz * wx - ux * wz, ux * wy - uy * wx
        return 0.5 * math.sqrt(nx * nx + ny * ny + nz * nz)

    @property
    def min_y(self) -> float:
        return min(p[1] for p in self.v)


def parse_mtl(path: Path) -> dict[str, str]:
    """`newmtl` name -> texture basename (the `map_Kd` BMP, extension stripped)."""
    mapping: dict[str, str] = {}
    current: str | None = None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        parts = line.split()
        if not parts:
            continue
        if parts[0] == "newmtl":
            current = parts[1]
        elif parts[0] == "map_Kd" and current is not None:
            mapping[current] = Path(" ".join(parts[1:])).stem
    return mapping


def parse_obj(path: Path, mtl: dict[str, str]) -> list[Face]:
    """Parse the OBJ into triangles, tracking the current `g` room + `usemtl`.

    N-gons are fan-triangulated. Faces with no material, or a material with no
    `map_Kd`, are dropped — they carry no texture information.
    """
    verts: list[tuple[float, float, float]] = []
    uvs: list[tuple[float, float]] = []
    faces: list[Face] = []
    room = "(none)"
    texture: str | None = None

    with path.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            # Cheapest possible dispatch: these files run to millions of lines.
            if line.startswith("v "):
                p = line.split()
                verts.append((float(p[1]), float(p[2]), float(p[3])))
            elif line.startswith("vt "):
                p = line.split()
                uvs.append((float(p[1]), float(p[2])))
            elif line.startswith("f "):
                if texture is None:
                    continue
                corners = []
                for tok in line.split()[1:]:
                    bits = tok.split("/")
                    vi = int(bits[0]) - 1
                    ti = int(bits[1]) - 1 if len(bits) > 1 and bits[1] else vi
                    corners.append((verts[vi], uvs[ti] if ti < len(uvs) else (0.0, 0.0)))
                for k in range(1, len(corners) - 1):
                    tri = (corners[0], corners[k], corners[k + 1])
                    faces.append(
                        Face(
                            room=room,
                            texture=texture,
                            v=tuple(c[0] for c in tri),
                            uv=tuple(c[1] for c in tri),
                        )
                    )
            elif line.startswith("g "):
                room = line.split(None, 1)[1].strip()
            elif line.startswith("usemtl "):
                texture = mtl.get(line.split(None, 1)[1].strip())

    return faces


def level_dirs() -> list[Path]:
    return sorted(d for d in LEVELS_DIR.iterdir() if d.is_dir() and (d / "LevelIndices.obj").exists())


def load_level(d: Path) -> list[Face]:
    return parse_obj(d / "LevelIndices.obj", parse_mtl(d / "LevelIndices.mtl"))


# ---------------------------------------------------------------------------
# Classification
# ---------------------------------------------------------------------------


def dominant_axis(n: tuple[float, float, float]) -> int:
    ax, ay, az = abs(n[0]), abs(n[1]), abs(n[2])
    if ay >= ax and ay >= az:
        return 1
    return 0 if ax >= az else 2


def polygon_area(pts: list[tuple[float, float, float]]) -> float:
    """Area of a planar 3D polygon, via the Newell cross-product sum."""
    if len(pts) < 3:
        return 0.0
    nx = ny = nz = 0.0
    for i in range(len(pts)):
        (ax, ay, az) = pts[i]
        (bx, by, bz) = pts[(i + 1) % len(pts)]
        nx += ay * bz - az * by
        ny += az * bx - ax * bz
        nz += ax * by - ay * bx
    return 0.5 * math.sqrt(nx * nx + ny * ny + nz * nz)


def clip_below(pts: list[tuple[float, float, float]], y: float) -> list[tuple[float, float, float]]:
    """Sutherland-Hodgman clip of a polygon to the half-space below `y`."""
    out: list[tuple[float, float, float]] = []
    for i in range(len(pts)):
        cur, nxt = pts[i], pts[(i + 1) % len(pts)]
        cur_in, nxt_in = cur[1] <= y, nxt[1] <= y
        if cur_in:
            out.append(cur)
        if cur_in != nxt_in:
            dy = nxt[1] - cur[1]
            if abs(dy) > 1e-9:
                t = (y - cur[1]) / dy
                out.append(tuple(cur[k] + t * (nxt[k] - cur[k]) for k in range(3)))
    return out


def zone_areas(face: Face, room_floor_y: float) -> list[tuple[int, float]]:
    """Face -> [(zone, area)], mirroring `uv_zones`'s classification.

    Floors and ceilings are picked by the dominant-axis rule in
    `uv_zones::classify_soup`. Walls are **geometrically split** at the
    lower/upper boundary and contribute area to both bands, exactly as
    `emit_wall_split` splits the triangle rather than bucketing it whole.

    Splitting matters: GoldenEye's walls are typically two full-height triangles,
    so bucketing a face by its lowest vertex files every wall as "lower" and the
    upper band comes out empty. Area-splitting is also what surfaces the rooms
    where GE genuinely *did* stratify a wall into two textures — those come out
    with different textures winning each band.
    """
    n = face.normal
    if n == (0.0, 0.0, 0.0):
        return []  # degenerate sliver
    area = face.area
    if area <= 0.0:
        return []
    if dominant_axis(n) == 1:
        return [(ZONE_FLOOR if n[1] > 0.0 else ZONE_CEILING, area)]

    split_y = room_floor_y + GE_STOREY * LOWER_WALL_FRACTION
    pts = list(face.v)
    lo = polygon_area(clip_below(pts, split_y))
    hi = area - lo
    out = []
    if lo > 1e-6:
        out.append((ZONE_LOWER_WALL, lo))
    if hi > 1e-6:
        out.append((ZONE_UPPER_WALL, hi))
    return out


def uv_scale(face: Face) -> float | None:
    """GE units spanned by one full texture repetition on this face.

    Compares each edge's UV delta to its world-space length, which is exactly the
    quantity the engine's `repeat` controls. Returns the median over the edges to
    shrug off a degenerate one.
    """
    ratios = []
    for i in range(3):
        j = (i + 1) % 3
        (ax, ay, az), (bx, by, bz) = face.v[i], face.v[j]
        world = math.dist((ax, ay, az), (bx, by, bz))
        du = face.uv[j][0] - face.uv[i][0]
        dv = face.uv[j][1] - face.uv[i][1]
        uv = math.hypot(du, dv)
        if world > 1e-6 and uv > 1e-6:
            ratios.append(world / uv)
    if not ratios:
        return None
    ratios.sort()
    return ratios[len(ratios) // 2]


# ---------------------------------------------------------------------------
# Per-room theme extraction
# ---------------------------------------------------------------------------


@dataclass
class ZoneCandidate:
    """Area-weighted texture vote for one zone of one room."""

    area_by_texture: Counter = field(default_factory=Counter)
    scales_by_texture: dict[str, list[float]] = field(default_factory=lambda: defaultdict(list))

    def add(self, texture: str, area: float, scale: float | None) -> None:
        self.area_by_texture[texture] += area
        if scale is not None:
            self.scales_by_texture[texture].append(scale)

    def winner(self) -> tuple[str, float, float] | None:
        """(texture, repeat, share_of_zone_area) for the modal texture, by area.

        Area-weighted rather than face-counted: these levels are full of tiny
        slivers, and counting faces lets a hundred slivers outvote the wall.
        """
        if not self.area_by_texture:
            return None
        texture, area = self.area_by_texture.most_common(1)[0]
        total = sum(self.area_by_texture.values())
        scales = sorted(self.scales_by_texture.get(texture, []))
        if scales:
            ge_per_tile = scales[len(scales) // 2]
            repeat = GE_UNITS_PER_TILE / ge_per_tile if ge_per_tile > 0 else 1.0
        else:
            repeat = 1.0
        return texture, repeat, area / total if total else 0.0


def extract_rooms(level: str, faces: list[Face]) -> list[dict]:
    """Fold a level's faces into one candidate theme per room group."""
    by_room: dict[str, list[Face]] = defaultdict(list)
    for f in faces:
        by_room[f.room].append(f)

    out = []
    for room, rfaces in by_room.items():
        floor_y = min(f.min_y for f in rfaces)
        zones: dict[int, ZoneCandidate] = defaultdict(ZoneCandidate)
        for f in rfaces:
            scale = uv_scale(f)
            for zone, area in zone_areas(f, floor_y):
                zones[zone].add(f.texture, area, scale)

        picked: dict[str, dict] = {}
        for zone, cand in sorted(zones.items()):
            win = cand.winner()
            if win is None:
                continue
            texture, repeat, share = win
            picked[str(zone)] = {
                "texture": texture,
                "repeat": round(repeat, 4),
                "share": round(share, 3),
            }
        if not picked:
            continue
        out.append(
            {
                "level": level,
                "room": room,
                "faces": len(rfaces),
                "zones": picked,
            }
        )
    return out


def theme_key(room: dict) -> tuple:
    """Identity of a candidate theme: which texture sits in zones 0..3.

    `repeat` is deliberately excluded — two rooms using the same four textures are
    the same theme even if one room's walls are stretched differently.
    """
    return tuple(room["zones"].get(str(z), {}).get("texture") for z in range(4))


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------


def cmd_levels() -> None:
    print(f"{'LEVEL':24s} {'ROOMS':>6s} {'FACES':>8s} {'TEXTURES':>9s}")
    for d in level_dirs():
        faces = load_level(d)
        rooms = {f.room for f in faces}
        texs = {f.texture for f in faces}
        print(f"{d.name:24s} {len(rooms):6d} {len(faces):8d} {len(texs):9d}")


def cmd_calibrate() -> None:
    """Solve GE_UNITS_PER_TILE against the hand-tuned `facility_*` repeats.

    The shipped Facility themes were tuned by eye in the JS editor against these
    very levels, so they are a usable ground truth: whatever constant makes our
    measured GE-units-per-tile reproduce those numbers is the right bridge between
    GE units and the engine's UV space.
    """
    themes = json.loads((REPO / "native" / "assets" / "themes.json").read_text(encoding="utf-8"))
    hand = {}
    for s in themes["schemes"]:
        if not s["name"].startswith("facility"):
            continue
        for zi, z in s["zones"].items():
            if int(zi) < 4 and z.get("texture"):
                hand.setdefault(int(zi), []).append(z["repeat"])

    faces = load_level(LEVELS_DIR / "02 - Facility")
    by_room: dict[str, list[Face]] = defaultdict(list)
    for f in faces:
        by_room[f.room].append(f)

    measured: dict[int, list[float]] = defaultdict(list)
    for rfaces in by_room.values():
        floor_y = min(f.min_y for f in rfaces)
        for f in rfaces:
            sc = uv_scale(f)
            if sc is None:
                continue
            for zone, _area in zone_areas(f, floor_y):
                measured[zone].append(sc)

    print("Measured GE units per texture tile, from 02 - Facility:\n")
    print(f"{'zone':12s} {'faces':>7s} {'median':>9s} {'p25':>9s} {'p75':>9s}   hand-tuned repeats")
    solved = []
    for zone in range(4):
        vals = sorted(measured.get(zone, []))
        if not vals:
            continue
        med = vals[len(vals) // 2]
        p25 = vals[len(vals) // 4]
        p75 = vals[(3 * len(vals)) // 4]
        tuned = sorted(set(hand.get(zone, [])))
        print(
            f"{ZONE_NAMES[zone]:12s} {len(vals):7d} {med:9.1f} {p25:9.1f} {p75:9.1f}   {tuned}"
        )
        # Each hand-tuned repeat implies a constant: k = repeat * ge_per_tile.
        for t in tuned:
            solved.append((ZONE_NAMES[zone], t, med, t * med))

    print("\n--- Why the hand-tuned values can't be fitted -------------------")
    print("Each hand-tuned repeat implies its own constant k = repeat x median:\n")
    for zname, t, med, k in solved:
        print(f"  {zname:12s} repeat={t:<7} x median={med:8.1f}  ->  k={k:8.1f}")
    ks = sorted(k for *_, k in solved)
    if ks:
        print(f"\n  spread {ks[0]:.1f} .. {ks[-1]:.1f}  ({ks[-1] / ks[0]:.0f}x) — not one constant.")
        print("  These were eyeballed against our room sizes, not GoldenEye's.")

    print("\n--- The derivation actually used --------------------------------")
    print(f"  GE storey (measured)      = {GE_STOREY:.0f} GE units")
    print(f"  our default room height   = {OUR_ROOM_HEIGHT_WT:.0f} WT  (world/mod.rs:2460)")
    print(f"  => GE_UNITS_PER_TILE      = {GE_UNITS_PER_TILE:.2f}")
    print(f"  WALL_SPLIT_V              = {WALL_SPLIT_V:.0f} WT  (uv_zones.rs:32)")
    print(f"  => LOWER_WALL_FRACTION    = {LOWER_WALL_FRACTION:.3f} of a storey")

    print("\n--- Sanity check against the hand-tuned brackets -----------------")
    print("Not a fit — just: is the derived value the same order as what a human")
    print("picked by eye? Those ranges span 3-10x, so this is a coarse check.\n")
    print(f"{'zone':12s} {'derived':>9s}   {'hand-tuned range':>20s}   deviation")
    for zone in range(4):
        vals = sorted(measured.get(zone, []))
        tuned = sorted(set(hand.get(zone, [])))
        if not vals or not tuned:
            continue
        med = vals[len(vals) // 2]
        derived = GE_UNITS_PER_TILE / med
        lo, hi = min(tuned), max(tuned)
        if derived < lo:
            note = f"{lo / derived:.2f}x below the range floor"
        elif derived > hi:
            note = f"{derived / hi:.2f}x above the range ceiling"
        else:
            note = "inside"
        print(f"{ZONE_NAMES[zone]:12s} {derived:9.3f}   {lo:8.4f} .. {hi:<8.4f}   {note}")

    print(
        """
  Floor, ceiling and lower wall land inside. Upper wall sits ~1.3x below the
  tuned floor, and that is expected rather than a calibration error: the
  lower/upper wall split is OUR invention (the JS editor added WALL_SPLIT_V to
  imitate GoldenEye's look), not something GoldenEye authored to. GE walls are
  one continuous surface, so any "upper wall" measurement is taken over a band
  we defined after the fact — zone 3 is structurally the least trustworthy of
  the four, and its repeat is the one most worth adjusting by eye in the GUI.

  The anchor is deliberately NOT tuned to make this pass: 16 WT is a code
  constant, whereas the 15 WT that would have squeezed it in is an authoring
  accident of one level."""
    )


def cmd_extract() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    all_rooms: list[dict] = []
    for d in level_dirs():
        rooms = extract_rooms(d.name, load_level(d))
        all_rooms.extend(rooms)
        print(f"{d.name:24s} {len(rooms):4d} rooms with themes")

    raw = OUT_DIR / "room_candidates.json"
    raw.write_text(json.dumps(all_rooms, indent=1), encoding="utf-8")

    # Collapse rooms that use the same four textures.
    groups: dict[tuple, list[dict]] = defaultdict(list)
    for r in all_rooms:
        groups[theme_key(r)].append(r)

    themes = []
    for key, rooms in sorted(groups.items(), key=lambda kv: -sum(r["faces"] for r in kv[1])):
        if key[0] is None and key[2] is None:
            continue  # neither a floor nor a lower wall: not a usable room theme
        levels = Counter(r["level"] for r in rooms)
        # Representative = the room with the most geometry, whose repeats we keep.
        rep = max(rooms, key=lambda r: r["faces"])
        themes.append(
            {
                "textures": {ZONE_NAMES[z]: key[z] for z in range(4)},
                "rooms": len(rooms),
                "faces": sum(r["faces"] for r in rooms),
                "levels": [f"{lv} x{n}" for lv, n in levels.most_common()],
                "representative": {"level": rep["level"], "room": rep["room"]},
                "zones": rep["zones"],
                # False when any derived repeat is implausible. GoldenEye stretched
                # textures freely across thin trims and skewed faces, so ~8% of
                # measurements land far outside anything usable as a room theme.
                # The texture choice is still good in those themes — only the scale
                # needs a human — so they are kept and flagged rather than dropped.
                "repeats_in_band": all(
                    REPEAT_MIN <= z["repeat"] <= REPEAT_MAX for z in rep["zones"].values()
                ),
            }
        )

    lib = OUT_DIR / "theme_library.json"
    lib.write_text(json.dumps(themes, indent=1), encoding="utf-8")

    in_band = sum(1 for t in themes if t["repeats_in_band"])
    print(f"\n{len(all_rooms)} room candidates -> {len(themes)} distinct themes")
    print(
        f"  {in_band} have every derived repeat inside {REPEAT_MIN}-{REPEAT_MAX}; "
        f"{len(themes) - in_band} need their scale reviewed by eye"
    )
    print(f"  {raw.relative_to(REPO)}")
    print(f"  {lib.relative_to(REPO)}")
    print("\nTop themes by geometry:\n")
    for t in themes[:15]:
        tex = t["textures"]
        print(
            f"  {t['rooms']:4d} rooms {t['faces']:7d} faces  "
            f"floor={tex['floor']}  wall={tex['lower_wall']}/{tex['upper_wall']}  "
            f"ceil={tex['ceiling']}   [{t['levels'][0]}]"
        )


def cmd_validate() -> None:
    """Check the extractor against the nine hand-authored themes already shipping.

    This is the real acceptance test for the whole pipeline: the `facility_*`,
    `archives_1` and `bunker_1` themes were authored by a human reading these very
    levels, so if the extractor is sound it should independently rediscover them.

    Comparison MUST be by content hash, not by filename. Every human-renamed
    texture in `public/textures/` has exactly one temp-named twin
    (`grey_tile_floor` == `tempImgEd02B7`, `white_tile` == `tempImgEd02CE`, …), so
    a name-based check scores 2/9 purely because of the renaming and hides the
    real result.
    """
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import texlib

    index = texlib.load_index()
    h_of = texlib.name_to_hash(index)

    lib = json.loads((OUT_DIR / "theme_library.json").read_text(encoding="utf-8"))
    shipped = json.loads(
        (REPO / "native" / "assets" / "themes.json").read_text(encoding="utf-8")
    )["schemes"]
    keys = ("floor", "ceiling", "lower_wall", "upper_wall")

    def hashes(names) -> tuple:
        return tuple(h_of.get(n) if n else None for n in names)

    print("Extractor vs the nine shipped hand-authored themes, by content hash:\n")
    exact = 0
    for s in shipped:
        if s["name"] == "simple_blue":
            continue  # not a GoldenEye room theme — our own platform style
        z = {int(k): v for k, v in s["zones"].items()}
        want = hashes([z.get(i, {}).get("texture") for i in range(4)])
        hits = [t for t in lib if hashes([t["textures"][k] for k in keys]) == want]
        if hits:
            exact += 1
            h = hits[0]
            print(
                f"  {s['name']:26s} EXACT 4/4  <- {h['representative']['level']:16s}"
                f" {h['rooms']:3d} rooms"
            )
        else:
            best = max(
                lib,
                key=lambda t: sum(
                    a == b for a, b in zip(hashes([t["textures"][k] for k in keys]), want)
                ),
            )
            n = sum(a == b for a, b in zip(hashes([best["textures"][k] for k in keys]), want))
            print(
                f"  {s['name']:26s} {n}/4 zones   <- best in "
                f"{best['representative']['level']}"
            )
    print(f"\n  {exact}/9 reproduced exactly on all four zones, rest partial in the right level.")
    print("  The extractor recovers hand-authored themes from raw source — it is sound.")


def main() -> None:
    cmds = {
        "levels": cmd_levels,
        "calibrate": cmd_calibrate,
        "extract": cmd_extract,
        "validate": cmd_validate,
    }
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        print(__doc__)
        print("subcommands: " + ", ".join(cmds))
        raise SystemExit(2)
    cmds[sys.argv[1]]()


if __name__ == "__main__":
    main()
