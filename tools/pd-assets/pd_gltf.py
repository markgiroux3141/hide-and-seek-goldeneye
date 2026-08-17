#!/usr/bin/env python3
"""Perfect Dark character/animation -> glTF binary (.glb) exporter.

Turns a PD `chrs/*.bin` into a **skinned** GLB and a PD `animations/*.bin` into a
clip-only GLB, in exactly the shape the engine's existing GoldenEye character
pipeline already consumes (`engine/src/skeletal/gltf_skin.rs` for the body,
`engine/src/skeletal/clip.rs` for the clips). No new Rust loader is needed: a PD
character arrives down the same path a GE one does.

Read `START_HERE.md` section 2 for the underlying PD formats; this file only
covers what is specific to *re-expressing* them as glTF.

# The rig is renamed onto GoldenEye's bone names

The game addresses bones by name — `Bone_9` is the weapon hand, `Bone_3` the
head, `Bone_14`/`Bone_15` the feet (see `game/src/combat/enemy_weapons.rs`). PD's
15 joints are the same 15 joints, numbered differently, so this exporter renames
them onto the GE names and every existing system (weapon attach, head look-at,
foot IK, the aim overlay, the upper-body mask) works on a PD body unmodified.

Which PD part is which was **not** guessed from the +/-X sign. `modeldef.parts`
is a sorted lookup table from `MODELPART_CHR_*` to node, and it settles it
outright, identically across a51guard / dark_frock / maian_soldier / mrblonde:

    MODELPART_CHR_RIGHTHAND (3) -> anim part 8   (the -X arm chain 4->6->8)
    MODELPART_CHR_LEFTHAND  (5) -> anim part 7   (the +X arm chain 3->5->7)
    MODELPART_CHR_0006      (6) -> anim part 2   (the head, HEADSPOT's sibling)

So **-X is the character's right**, and `PART_TO_BONE` below follows from that.

# Bind pose: identity inverse-bind matrices, vertices in bone-local space

PD skins per vertex by binding a bone matrix mid-display-list (`G_MTX`) and then
transforming the raw vertex by that bone's *world* matrix — i.e. PD vertices are
already stored in **bone-local space**. glTF computes `jointMatrix = global(joint)
x inverseBind(joint)`, so writing `inverseBind = identity` and keeping vertices
bone-local makes `jointMatrix` exactly PD's bone world matrix. The conversion is
then a pure re-encoding with no change of space, which is the point: there is no
opportunity for a silent transform error.

(The GoldenEye assets use the other convention — model-space vertices, real
inverse-bind matrices, so their bind pose reduces to identity joint matrices. Both
are valid glTF; nothing in the loader assumes either.)

Consequently a PD body's `bounds_min/max` (a bone-local AABB) is meaningless.
`World::new` only falls back to it when no idle clip loaded, and the PD path
always has one.

# Blend joints are real joints here, not an approximation

A PD `POSITION` node owns up to three matrix slots. Slot 0 is the joint; slots 1
and 2 are *blend* matrices, and `lib/modelasm_c.c:367-430` builds them as the
parent matrix composed with **half** the joint's rotation (the quaternion
half-angle dance at :380-425) — a midpoint frame that hides the seam at a joint.

Rather than fold those vertices onto the owning joint (what `pd_pose.py` does,
and which leaves a visible crease), each blend slot becomes its own glTF joint,
`Blend_<bone>`, sharing the owning bone's parent and rest offset and carrying the
half-rotation per frame. That is exact, not approximate, and costs 4 extra joints
on a 15-bone rig. `--no-blend-joints` reverts to the folded behaviour for A/B.

The 15 named `Bone_*` joints keep their names and roles either way, so nothing
that looks a bone up by name notices the extra joints.

# Units

The engine draws characters at `CHAR_SCALE` (`game/src/world/mod.rs`), a single
global constant, so bodies must arrive in the same units GE bodies use rather
than in metres. `EXPORT_SCALE` converts PD millimetres to those units; the result
is that a PD body needs no per-body scale in the engine. `--metres` overrides it
for inspection in an external tool.

Usage:
    python pd_gltf.py char  <chr.bin> <out.glb>
    python pd_gltf.py clip  <ANIM_ID> <out.glb> [--fps 30]
    python pd_gltf.py batch <manifest.json> <outdir>
"""

from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import pd_tex  # noqa: E402
from pd_anim import load_animation  # noqa: E402
from pd_model import ModelDef, load, seg_off, seg_ok  # noqa: E402
from pd_pose import UNITS_PER_METRE, build_skeleton, rotation_matrix  # noqa: E402

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# `CHAR_SCALE` in `native/crates/game/src/world/mod.rs` — the single global scale
# every skinned body is drawn at. Exporting in these units means a PD body drops
# into the roster with no per-body scale of its own.
CHAR_SCALE = 0.000_832
GE_UNITS_PER_METRE = 1.0 / CHAR_SCALE
#: PD model units (millimetres) -> engine character units.
EXPORT_SCALE = GE_UNITS_PER_METRE / UNITS_PER_METRE

#: Animation playback rate, in PD animation frames per second.
#:
#: Derived from `chr_action_go_to_position` (`game/chraction.c:2189`), which sets
#: an ETA in 60Hz frames as
#:
#:     eta60 = distance / (movedist_per_anim_frame * mult)
#:
#: For that to be dimensionally true, animation frames must advance by `mult`
#: per 60Hz frame. Locomotion passes `mult = 0.5`, so 30 animation frames per
#: second. (`model_tick_anim`'s `lvupdate240` loop is the same statement written
#: per-tick.) Every locomotion clip checked is authored in place — part 0 has no
#: translation channel at all — so there is no stride/root-motion cross-check to
#: be had; this is the derivation, and the eye is the confirmation.
DEFAULT_FPS = 30.0

#: PD animation part number -> GoldenEye bone number.
#:
#: The chain shapes are read straight off the node tree (`pd_pose.py --skeleton`):
#: root 0; spine 1 with head 2; arms 3->5->7 (+X) and 4->6->8 (-X); legs 9->11->13
#: (+X) and 10->12->14 (-X). Sidedness comes from `modeldef.parts` (see the module
#: docstring): -X is the character's right.
#:
#: The GE side is fixed by `game/src/combat/enemy_weapons.rs`: Bone_9 right hand,
#: Bone_8 left hand, Bone_3 head, Bone_1 pelvis, Bone_14 left foot, Bone_15 right
#: foot, with the leg chains 10->12->14 (left) and 11->13->15 (right).
PART_TO_BONE = {
    0: 1,    # CHRINFO root      -> pelvis
    1: 2,    # spine             -> chest
    2: 3,    # head/neck         -> head
    3: 4,   5: 6,   7: 8,    # +X arm  -> LEFT  upper arm / forearm / hand
    4: 5,   6: 7,   8: 9,    # -X arm  -> RIGHT upper arm / forearm / hand
    9: 10, 11: 12, 13: 14,   # +X leg  -> LEFT  thigh / shin / foot
    10: 11, 12: 13, 14: 15,  # -X leg  -> RIGHT thigh / shin / foot
}

#: Per-part debug palette, matching `pd_pose.py`'s so the skinned bodies read the
#: same as the preview props they replace. PD character models are *textured* —
#: `modelrodata_dl.colours` is NULL and every vertex carries real `s,t` — but the
#: textures are an inline N64 blob this pipeline does not decode yet, so parts get
#: flat colours for now. Desaturated on purpose: the silhouette should read as a
#: figure while a mis-parented limb still jumps out.
PALETTE = [
    (0.72, 0.68, 0.62), (0.45, 0.50, 0.58), (0.62, 0.48, 0.42),
    (0.50, 0.58, 0.48), (0.66, 0.60, 0.45), (0.42, 0.46, 0.56),
    (0.70, 0.55, 0.50), (0.52, 0.56, 0.62),
]
#: Side of one palette cell, in texels. Larger than 1x1 so that a linear/mipmapped
#: sampler cannot bleed one part's colour into its neighbour's.
PALETTE_CELL = 8

# glTF component types.
CT_U16 = 5123
CT_U32 = 5125
CT_F32 = 5126
# glTF bufferView targets.
TARGET_ARRAY = 34962
TARGET_ELEMENT = 34963

GLB_MAGIC = 0x46546C67
CHUNK_JSON = 0x4E4F534A
CHUNK_BIN = 0x004E4942


# ---------------------------------------------------------------------------
# Small maths helpers (row-vector PD -> column-vector glTF)
# ---------------------------------------------------------------------------


def quat_from_pd_rotation(rx: float, ry: float, rz: float) -> tuple[float, float, float, float]:
    """PD Euler XYZ -> a glTF `[x, y, z, w]` quaternion.

    `pd_pose.rotation_matrix` returns PD's row-vector matrix (`v' = v @ M`).
    glTF/glam are column-vector (`v' = M @ v`), and the two differ by exactly a
    transpose, so the rotation basis is read out of the *columns* here. Both are
    proper rotations, so the quaternion conversion is well-defined either way —
    getting the transpose backwards inverts every joint rotation, which is
    florid on screen rather than subtle.
    """
    r = rotation_matrix(rx, ry, rz)
    # m[i][j] with the transpose folded in: m_col[i][j] = r[j][i].
    m = [[r[j][i] for j in range(3)] for i in range(3)]
    return quat_from_matrix(m)


def quat_from_matrix(m) -> tuple[float, float, float, float]:
    """Column-vector 3x3 rotation matrix -> `[x, y, z, w]` (Shepperd's method)."""
    trace = m[0][0] + m[1][1] + m[2][2]
    if trace > 0.0:
        s = math.sqrt(trace + 1.0) * 2.0
        w = 0.25 * s
        x = (m[2][1] - m[1][2]) / s
        y = (m[0][2] - m[2][0]) / s
        z = (m[1][0] - m[0][1]) / s
    elif m[0][0] > m[1][1] and m[0][0] > m[2][2]:
        s = math.sqrt(1.0 + m[0][0] - m[1][1] - m[2][2]) * 2.0
        w = (m[2][1] - m[1][2]) / s
        x = 0.25 * s
        y = (m[0][1] + m[1][0]) / s
        z = (m[0][2] + m[2][0]) / s
    elif m[1][1] > m[2][2]:
        s = math.sqrt(1.0 + m[1][1] - m[0][0] - m[2][2]) * 2.0
        w = (m[0][2] - m[2][0]) / s
        x = (m[0][1] + m[1][0]) / s
        y = 0.25 * s
        z = (m[1][2] + m[2][1]) / s
    else:
        s = math.sqrt(1.0 + m[2][2] - m[0][0] - m[1][1]) * 2.0
        w = (m[1][0] - m[0][1]) / s
        x = (m[0][2] + m[2][0]) / s
        y = (m[1][2] + m[2][1]) / s
        z = 0.25 * s
    n = math.sqrt(x * x + y * y + z * z + w * w) or 1.0
    return (x / n, y / n, z / n, w / n)


def quat_halfway(q) -> tuple[float, float, float, float]:
    """Half of a rotation: `slerp(identity, q, 0.5)`, normalised.

    This is what `lib/modelasm_c.c` computes for a `POSITION` node's blend
    matrix. Taking the shorter arc (negating `q` when `w < 0`) matters — the
    other arc halves to a rotation the long way round, which reads as a limb
    snapping through the body.
    """
    x, y, z, w = q
    if w < 0.0:
        x, y, z, w = -x, -y, -z, -w
    w += 1.0  # q + identity; renormalising it is the midpoint of the arc
    n = math.sqrt(x * x + y * y + z * z + w * w)
    if n < 1e-9:
        return (0.0, 0.0, 0.0, 1.0)
    return (x / n, y / n, z / n, w / n)


# ---------------------------------------------------------------------------
# PNG (palette textures, and reused by pd_preview.py for its output)
# ---------------------------------------------------------------------------


def png_bytes(width: int, height: int, rgba: bytes) -> bytes:
    """Encode tightly-packed RGBA8 as a PNG (no filtering, one IDAT)."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter type 0 (None)
        raw += rgba[y * stride : (y + 1) * stride]

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def palette_png(count: int) -> bytes:
    """A horizontal strip of `count` solid `PALETTE` cells, `PALETTE_CELL` square."""
    w, h = max(count, 1) * PALETTE_CELL, PALETTE_CELL
    row = bytearray()
    for i in range(max(count, 1)):
        r, g, b = PALETTE[i % len(PALETTE)]
        px = bytes((int(r * 255), int(g * 255), int(b * 255), 255))
        row += px * PALETTE_CELL
    return png_bytes(w, h, bytes(row) * h)


def palette_uv(index: int, count: int) -> tuple[float, float]:
    """The centre of palette cell `index` in a `count`-cell strip."""
    return ((index + 0.5) / max(count, 1), 0.5)


# ---------------------------------------------------------------------------
# Textures
# ---------------------------------------------------------------------------

#: `struct textureconfig` (`types.h`) is 12 bytes:
#: `void *texturenum; u8 width, height, level, s, t, x, y, unk0b`.
TEXCONFIG_SIZE = 12
#: Texel size in bytes. Every `G_SETTIMG` in the character display lists declares
#: `fmt=RGBA siz=16b` — checked across the models, with no other combination
#: appearing — so the inline data is RGBA5551.
TEXEL_BYTES = 2


class TexConfig:
    """One entry of `modeldef.texconfigs`: where a texture is and how big."""

    __slots__ = ("index", "ptr", "width", "height", "levels", "wrap_s", "wrap_t", "texnum")

    def __init__(self, index, ptr, width, height, levels, wrap_s, wrap_t, texnum=None):
        self.index = index
        self.ptr = ptr
        self.width = width
        self.height = height
        #: Number of mip levels present. Only level 0 is exported; the engine
        #: builds its own. Confirmed against the data: a 32x48 with `levels == 4`
        #: occupies exactly `(32*48 + 16*24 + 8*12 + 4*6) * 2` bytes.
        self.levels = levels
        self.wrap_s = wrap_s
        self.wrap_t = wrap_t
        #: Global-pool texture number, or `None` when the data is inline.
        self.texnum = texnum

    @property
    def inline(self) -> bool:
        return self.texnum is None


def read_texconfigs(m: ModelDef) -> dict[int, TexConfig]:
    """`modeldef.texconfigs`, keyed by the value the display lists' `G_SETTIMG`
    carries — which is what makes the triangle-to-texture binding exact rather
    than positional.

    `texturenum` is **one of two things**, and the segment nibble says which:

    * a **segment-05 pointer** into the model file, where the data is
      already-decompressed `RGBA5551` (see [`decode_texture`]). Only a51guard,
      dd_shock, elvis and testchr are like this;
    * a plain **index into the global texture pool** (`textures/`, 3,503 files),
      which is compressed — decoded by [`pd_tex`]. Every other body, and all 76
      head models.
    """
    out: dict[int, TexConfig] = {}
    if not (m.numtexconfigs and seg_ok(m.texconfigs)):
        return out
    base = seg_off(m.texconfigs)
    for i in range(m.numtexconfigs):
        off = base + i * TEXCONFIG_SIZE
        if off + TEXCONFIG_SIZE > len(m.data):
            break
        ptr = struct.unpack_from(">I", m.data, off)[0]
        w, h, levels, s, t = struct.unpack_from(">BBBBB", m.data, off + 4)
        if w == 0 or h == 0:
            continue
        if seg_ok(ptr):
            out[ptr] = TexConfig(i, ptr, w, h, levels, s, t)
        elif ptr >> 24 == 0:
            out[ptr] = TexConfig(i, ptr, w, h, levels, s, t, texnum=ptr & 0xFFFFFF)
    return out


def rgba16_row_bytes(width: int) -> int:
    """Bytes per stored row of an `RGBA16` texture — `tex_swizzle`'s `wordsperrow`.

    `texdecompress.c:1927` computes `((width + 3) & 0xffc) >> 1` u32 words for
    `TEXFORMAT_RGBA16`, i.e. rows are padded out to 8 bytes. Reading rows back to
    back at `width * 2` instead shears every non-multiple-of-4 texture — a 38-wide
    one slips a texel per row, which looks like noise rather than like an
    off-by-one.
    """
    return (((width + 3) & 0xFFC) >> 1) * 4


def decode_texture(m: ModelDef, cfg: TexConfig) -> tuple[int, int, bytes]:
    """Level 0 of a texture as `(width, height, RGBA8)`, from wherever it lives.

    Pool textures go through [`pd_tex.decode`]; the rest is the inline path
    below. Raises [`pd_tex.UnsupportedTexture`] when a pool texture uses the
    codec that is not ported, which the caller turns into a palette fallback.

    Note the two storage layouts are **opposite** on swizzling: pool data
    inflates linear (PD swizzles it afterwards, on its way to the RDP), while
    inline data is already swizzled and has to be undone.
    """
    if not cfg.inline:
        t = pd_tex.load(cfg.texnum)
        if (t.width, t.height) != (cfg.width, cfg.height):
            print(
                f"  NOTE: texture {cfg.texnum:#x} is {t.width}x{t.height} but "
                f"{m.name}'s texconfig says {cfg.width}x{cfg.height}",
                file=sys.stderr,
            )
        return t.width, t.height, t.rgba
    return cfg.width, cfg.height, decode_inline_texture(m, cfg)


def decode_inline_texture(m: ModelDef, cfg: TexConfig) -> bytes:
    """Level 0 of an inline RGBA5551 texture, as tightly-packed RGBA8.

    Two storage details, both taken from `tex_swizzle` (`texdecompress.c:1927`)
    and both independently confirmed on screen before that function was found:

    * rows are padded to 8 bytes ([`rgba16_row_bytes`]);
    * **odd rows have adjacent u32 words swapped** — the N64 TMEM interleave. At
      16bpp a u32 is two texels, so the swap is `x ^ 2` within each group of four.
      Skipping it leaves a fine vertical comb over everything; the figure still
      reads, the faces do not.

    The mip levels that follow level 0 are ignored: PD generates them at load
    (`tex_shrink_*`), and so does the engine.

    `RGBA16` is `rrrrrgggggbbbbba` big-endian. The 5-bit channels are scaled by
    `*255//31` rather than `<<3` so full scale stays full scale (`31 -> 255`, not
    `248`), which otherwise greys down a whole character. The single alpha bit
    becomes 0 or 255.
    """
    base = seg_off(cfg.ptr)
    stride = rgba16_row_bytes(cfg.width)
    if base + stride * cfg.height > len(m.data):
        raise SystemExit(f"{m.name}: texture at {cfg.ptr:#x} runs past the end of the file")
    px = bytearray(cfg.width * cfg.height * 4)
    for y in range(cfg.height):
        row = base + y * stride
        swap = y & 1
        for x in range(cfg.width):
            o = row + ((x ^ 2) if swap else x) * TEXEL_BYTES
            v = (m.data[o] << 8) | m.data[o + 1]
            d = (y * cfg.width + x) * 4
            px[d] = ((v >> 11) & 31) * 255 // 31
            px[d + 1] = ((v >> 6) & 31) * 255 // 31
            px[d + 2] = ((v >> 1) & 31) * 255 // 31
            px[d + 3] = 255 if v & 1 else 0
    return bytes(px)


def mip_chain_bytes(cfg: TexConfig) -> int:
    """Total stored size of a texture including its mip levels.

    Only used as a self-check: for every inline texture but the last, this must
    equal the gap to the next one. It does, to the byte, across all 30 of
    a51guard's — which is what pins down the padded row stride independently of
    how the pixels happen to look.
    """
    w, h, total = cfg.width, cfg.height, 0
    for _ in range(max(cfg.levels, 1)):
        total += rgba16_row_bytes(w) * h
        w, h = max(w // 2, 1), max(h // 2, 1)
    return total


# ---------------------------------------------------------------------------
# GLB assembly
# ---------------------------------------------------------------------------


class Glb:
    """Minimal glTF 2.0 binary writer: one buffer, everything in the BIN chunk."""

    def __init__(self, generator: str):
        self.doc: dict = {
            "asset": {"version": "2.0", "generator": generator},
            "scene": 0,
            "scenes": [{"nodes": []}],
            "nodes": [],
        }
        self.bin = bytearray()

    # -- buffer plumbing ---------------------------------------------------

    def _view(self, data: bytes, target: int | None = None, stride: int | None = None) -> int:
        while len(self.bin) % 4:
            self.bin.append(0)
        view = {"buffer": 0, "byteOffset": len(self.bin), "byteLength": len(data)}
        if target is not None:
            view["target"] = target
        if stride is not None:
            view["byteStride"] = stride
        self.bin += data
        return self._append("bufferViews", view)

    def _append(self, key: str, value) -> int:
        arr = self.doc.setdefault(key, [])
        arr.append(value)
        return len(arr) - 1

    def accessor(
        self,
        values,
        comptype: int,
        kind: str,
        target: int | None = None,
        minmax: bool = False,
    ) -> int:
        """Pack a flat list of scalars into a bufferView + accessor.

        `values` is flat (a VEC3 accessor takes `3 * count` numbers). `minmax`
        writes the per-component min/max the spec demands for `POSITION`.
        """
        ncomp = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}[kind]
        fmt = {CT_U16: "<H", CT_U32: "<I", CT_F32: "<f"}[comptype]
        data = b"".join(struct.pack(fmt, v) for v in values)
        count = len(values) // ncomp
        acc = {
            "bufferView": self._view(data, target),
            "componentType": comptype,
            "count": count,
            "type": kind,
        }
        if minmax and count:
            lo = [min(values[i::ncomp]) for i in range(ncomp)]
            hi = [max(values[i::ncomp]) for i in range(ncomp)]
            acc["min"], acc["max"] = lo, hi
        return self._append("accessors", acc)

    def image_png(self, png: bytes) -> int:
        view = self._view(png)
        return self._append("images", {"bufferView": view, "mimeType": "image/png"})

    # -- output ------------------------------------------------------------

    def save(self, path: str) -> None:
        self.doc["buffers"] = [{"byteLength": len(self.bin)}]
        js = json.dumps(self.doc, separators=(",", ":")).encode("utf-8")
        js += b" " * (-len(js) % 4)
        bn = bytes(self.bin) + b"\0" * (-len(self.bin) % 4)
        total = 12 + 8 + len(js) + 8 + len(bn)
        with open(path, "wb") as fh:
            fh.write(struct.pack("<III", GLB_MAGIC, 2, total))
            fh.write(struct.pack("<II", len(js), CHUNK_JSON))
            fh.write(js)
            fh.write(struct.pack("<II", len(bn), CHUNK_BIN))
            fh.write(bn)


# ---------------------------------------------------------------------------
# The rig, as glTF joints
# ---------------------------------------------------------------------------


class RigJoint:
    """One glTF joint: a name, a rest offset, a parent, and where it came from."""

    __slots__ = ("name", "bone", "part", "offset", "parent", "is_blend", "owner")

    def __init__(self, name, bone, part, offset, parent, is_blend=False, owner=None):
        self.name = name
        self.bone = bone            # GE bone number (blends borrow their owner's)
        self.part = part            # PD animation part number
        self.offset = offset        # rest translation, PD units
        self.parent = parent        # index into the rig list, or None
        self.is_blend = is_blend
        self.owner = owner          # rig index of the joint this blend softens


class Rig:
    """The 15 GE-named bones (+ optional blend joints) of one PD character."""

    def __init__(self, model: ModelDef, blend_joints: bool = True):
        joints = build_skeleton(model)
        by_part = {}
        for off, j in joints.items():
            if j.part in by_part:
                raise SystemExit(f"{model.name}: duplicate animation part {j.part}")
            by_part[j.part] = j
        missing = sorted(set(PART_TO_BONE) - set(by_part))
        if missing:
            raise SystemExit(
                f"{model.name}: not the standard 15-bone chr rig — missing parts {missing}"
            )
        extra = sorted(set(by_part) - set(PART_TO_BONE))
        if extra:
            raise SystemExit(f"{model.name}: unexpected extra joints, parts {extra}")

        # Bones first, in Bone_1..Bone_15 order, so joint index == bone number - 1
        # (which is also what `clip.rs`'s index fallback assumes).
        order = sorted(PART_TO_BONE, key=lambda p: PART_TO_BONE[p])
        self.joints: list[RigJoint] = []
        row_of_part: dict[int, int] = {}
        for part in order:
            j = by_part[part]
            row_of_part[part] = len(self.joints)
            self.joints.append(
                RigJoint(f"Bone_{PART_TO_BONE[part]}", PART_TO_BONE[part], part, j.pos, None)
            )
        for part in order:
            j = by_part[part]
            parent_part = joints[j.parent].part if j.parent is not None else None
            if parent_part is not None:
                self.joints[row_of_part[part]].parent = row_of_part[parent_part]

        # A blend joint for EVERY bone, whether this character uses it or not.
        #
        # Which bones carry a blend slot varies per character — 51 of the 65 put
        # them on elbows + knees, but `elvis` uniquely uses shoulders + hips, and
        # six other layouts exist. Clips bind to joints **by name**, and one clip
        # export has to drive every body, so the name set cannot depend on the
        # body: a body with a `Blend_10` the clip has never heard of keeps its bind
        # rotation (that geometry then stays splayed in the rest pose — a flat fin
        # at the hip), and worse, the clip's unmatched `Blend_6` lands on some
        # arbitrary joint through `clip.rs`'s node-index fallback.
        #
        # So all 15 exist on every rig and in every clip. Unused ones carry no
        # vertices and cost a matrix.
        blend_row: dict[int, int] = {}
        for part in order:
            owner = self.joints[row_of_part[part]]
            blend_row[part] = len(self.joints)
            self.joints.append(
                RigJoint(
                    f"Blend_{owner.bone}",
                    owner.bone,
                    owner.part,
                    owner.offset,
                    owner.parent,
                    is_blend=True,
                    owner=row_of_part[part],
                )
            )

        # `matrix slot -> rig joint`. Slot 0 of each joint is the joint itself;
        # slot 1 is its blend frame. `mtxindexes[2]` is never used by any shipped
        # character (checked across all 65) — loud rather than silent if that
        # changes, because a third frame would need its own joint.
        self.slot_to_joint: dict[int, int] = {}
        for part in order:
            j = by_part[part]
            row = row_of_part[part]
            mi0, mi1, mi2 = j.slots
            if mi0 >= 0:
                self.slot_to_joint[mi0] = row
            if mi2 >= 0:
                raise SystemExit(
                    f"{model.name}: part {part} uses mtxindexes[2] ({mi2}), which no "
                    "shipped character does — it needs its own blend joint"
                )
            if mi1 >= 0:
                self.slot_to_joint[mi1] = blend_row[part] if blend_joints else row

    def __len__(self) -> int:
        return len(self.joints)

    def bone_index(self, bone: int) -> int:
        return next(i for i, j in enumerate(self.joints) if j.bone == bone and not j.is_blend)


def rig_nodes(glb: Glb, rig: Rig, scale: float) -> list[int]:
    """Emit `Armature` + one node per rig joint. Returns node index per joint."""
    armature = glb._append("nodes", {"name": "Armature", "children": []})
    glb.doc["scenes"][0]["nodes"].append(armature)
    node_of = [
        glb._append(
            "nodes",
            {
                "name": j.name,
                "translation": [j.offset[0] * scale, j.offset[1] * scale, j.offset[2] * scale],
            },
        )
        for j in rig.joints
    ]
    for i, j in enumerate(rig.joints):
        parent = glb.doc["nodes"][armature if j.parent is None else node_of[j.parent]]
        parent.setdefault("children", []).append(node_of[i])
    return node_of


# ---------------------------------------------------------------------------
# char: skinned character GLB
# ---------------------------------------------------------------------------


HEADSPOT_NODE = 0x17
#: The animation part number of the head joint, and the matrix slot a grafted head
#: model's display lists bind to. See `attach_head`.
HEAD_PART = 2
HEAD_MATRIX_SLOT = 0


def attach_head(body: ModelDef, rig: Rig, head_path: str):
    """Geometry for a separate `head*.bin`, bound to the body's head joint.

    62 of the 65 characters on the shared rig carry a `HEADSPOT` node (type 0x17)
    instead of a built-in head; only a51guard, dd_shock and elvis are
    self-contained. `model_attach_head` (`lib/model.c:4275`) simply reparents the
    head model's root under that node — no transform of its own; `HEADSPOT`'s
    rodata is a lone `rwdataindex`.

    So a head's vertices are already expressed in head-joint space, and its
    display lists bind matrix slot 0 (every head model has `nummatrices == 1`).
    After the graft, segment 3 is the *body's* matrix array, and slot 0 of that is
    the head joint — checked here rather than assumed, because a body that broke
    the convention would silently hang its head off the pelvis.
    """
    head = load(head_path)
    groups = head.geometry()
    if not groups:
        raise SystemExit(f"{head.name}: no geometry")
    joint = rig.slot_to_joint.get(HEAD_MATRIX_SLOT)
    if joint is None or rig.joints[joint].part != HEAD_PART:
        got = rig.joints[joint].name if joint is not None else "nothing"
        raise SystemExit(
            f"{body.name}: matrix slot {HEAD_MATRIX_SLOT} is {got}, not the head joint "
            f"(part {HEAD_PART}) — a head grafted here would land on the wrong bone"
        )
    used = {v.mtx for g in groups for v in g.verts}
    if used - {HEAD_MATRIX_SLOT, -1}:
        raise SystemExit(f"{head.name}: binds matrix slots {sorted(used)}, expected only 0")
    return head, groups, joint


def export_char(
    path: str,
    out: str,
    scale: float,
    blend_joints: bool = True,
    head_path: str | None = None,
) -> dict:
    model = load(path)
    rig = Rig(model, blend_joints=blend_joints)
    # Each entry is (source model, mesh group, forced joint or None). The source
    # model matters because a grafted head brings its own texture table.
    groups = [(model, g, None) for g in model.geometry()]
    if not groups:
        raise SystemExit(f"{model.name}: no geometry")

    has_headspot = any((n.type & 0xFF) == HEADSPOT_NODE for n in model.walk())
    head_name = None
    if head_path:
        head, head_groups, head_joint = attach_head(model, rig, head_path)
        head_name = head.name
        groups += [(head, g, head_joint) for g in head_groups]
        if not has_headspot:
            print(
                f"  NOTE: {model.name} has its own head; {head.name} is grafted on top of it",
                file=sys.stderr,
            )
    elif has_headspot:
        print(
            f"  WARNING: {model.name} has a HEADSPOT node and no --head was given — "
            "it will export headless",
            file=sys.stderr,
        )

    glb = Glb("pd_gltf.py (Perfect Dark -> glTF)")
    node_of = rig_nodes(glb, rig, scale)

    # ── Geometry, batched by texture: one glTF primitive per PD texture, which is
    # also how PD draws it (a `G_SETTIMG` binds a texture, the triangles after it
    # use it). That keeps every texture at its own size and its own wrap mode, with
    # no atlas to pack and no UVs to rescale — and the renderer already uploads one
    # image and one bind group per primitive.
    #
    # Vertices are emitted per (source vertex, texture) rather than shared, because
    # a vertex on a texture seam needs a different normalized UV for each texture it
    # borders. Vertices stay in PD bone-local space (see the module docstring) and
    # are skinned rigidly — PD binds exactly one matrix per vertex via `G_MTX`, so
    # there is nothing to weight.
    texconfigs = {id(m): read_texconfigs(m) for m in {id(m): m for m, _, _ in groups}.values()}
    # batch key -> (source model, TexConfig or None); dict keeps first-seen order.
    batches: dict[tuple, list] = {}
    unbound = 0
    untextured = 0
    # A texture the pool codec cannot decode yet drops that batch to the palette,
    # rather than failing the whole character.
    undecodable: set[tuple[int, int]] = set()
    for gi, (src, g, forced_joint) in enumerate(groups):
        cfgs = texconfigs[id(src)]
        for a, b, c, tex in g.tris:
            cfg = cfgs.get(tex)
            if cfg is not None and (id(src), tex) not in undecodable:
                try:
                    decode_texture(src, cfg)
                except pd_tex.UnsupportedTexture as e:
                    print(f"  NOTE: {src.name} texture {cfg.index}: {e}", file=sys.stderr)
                    undecodable.add((id(src), tex))
            if cfg is not None and (id(src), tex) in undecodable:
                cfg = None
            if cfg is None:
                untextured += 1
            key = (id(src), tex if cfg is not None else None)
            batch = batches.get(key)
            if batch is None:
                batch = batches[key] = [src, cfg, gi, {}, [], [], [], [], []]
            _, _, _, remap, pos, uv, jnt, wgt, idx = batch
            for corner in (a, b, c):
                local = (gi, corner)
                out_i = remap.get(local)
                if out_i is None:
                    vert = g.verts[corner]
                    out_i = remap[local] = len(pos) // 3
                    pos.extend((vert.x * scale, vert.y * scale, vert.z * scale))
                    if cfg is not None:
                        # PD texcoords are S10.5 fixed point in texels; normalize by
                        # this texture's own size.
                        uv.extend((vert.s / 32.0 / cfg.width, vert.t / 32.0 / cfg.height))
                    else:
                        uv.extend(palette_uv(gi, len(groups)))
                    # A grafted head's vertices all ride the head joint
                    # (`attach_head`), including the handful loaded before the head
                    # DL's first `G_MTX`.
                    joint = (
                        forced_joint
                        if forced_joint is not None
                        else rig.slot_to_joint.get(vert.mtx)
                    )
                    if joint is None:
                        # Would mean a `G_MTX` slot no `POSITION` node claims. None of
                        # the 65 shared-rig characters has one; loud rather than silent
                        # if that ever changes, because the vertex would otherwise sit
                        # at the origin.
                        unbound += 1
                        joint = 0
                    jnt.extend((joint, 0, 0, 0))
                    wgt.extend((1.0, 0.0, 0.0, 0.0))
                idx.append(out_i)
    if unbound:
        print(f"  WARNING: {unbound} vertices had no matrix binding", file=sys.stderr)
    if untextured:
        print(
            f"  WARNING: {untextured} triangles had no texture — they fall back to the "
            "per-part debug palette",
            file=sys.stderr,
        )

    sampler = glb._append("samplers", {"magFilter": 9728, "minFilter": 9728})  # NEAREST
    palette_image = None
    primitives = []
    nverts = 0
    ntris = 0
    for src, cfg, gi, _remap, pos, uv, jnt, wgt, idx in batches.values():
        if cfg is not None:
            tw, th, rgba = decode_texture(src, cfg)
            image = glb.image_png(png_bytes(tw, th, rgba))
            mat_name = f"tex{cfg.index:02d}_{tw}x{th}"
        else:
            if palette_image is None:
                palette_image = glb.image_png(palette_png(len(groups)))
            image = palette_image
            mat_name = f"part{gi:02d}_untextured"
        texture = glb._append("textures", {"sampler": sampler, "source": image})
        material = glb._append(
            "materials",
            {
                "name": mat_name,
                # The GE bodies carry no NORMALs either, so the renderer draws base
                # colour straight through — this is an N64 look, not a lit PBR one.
                "pbrMetallicRoughness": {
                    "baseColorTexture": {"index": texture},
                    "metallicFactor": 0.0,
                    "roughnessFactor": 1.0,
                },
                # RGBA5551's single alpha bit is all-or-nothing, which is exactly
                # what MASK means; PD draws these with alpha compare, not blending.
                "alphaMode": "MASK",
                "alphaCutoff": 0.5,
                "doubleSided": True,
            },
        )
        n = len(pos) // 3
        primitives.append(
            {
                "attributes": {
                    "POSITION": glb.accessor(pos, CT_F32, "VEC3", TARGET_ARRAY, minmax=True),
                    "TEXCOORD_0": glb.accessor(uv, CT_F32, "VEC2", TARGET_ARRAY),
                    "JOINTS_0": glb.accessor(jnt, CT_U16, "VEC4", TARGET_ARRAY),
                    "WEIGHTS_0": glb.accessor(wgt, CT_F32, "VEC4", TARGET_ARRAY),
                },
                "indices": glb.accessor(
                    idx, CT_U16 if n <= 0xFFFF else CT_U32, "SCALAR", TARGET_ELEMENT
                ),
                "material": material,
            }
        )
        nverts += n
        ntris += len(idx) // 3

    mesh = glb._append("meshes", {"name": model.name, "primitives": primitives})

    # ── Skin. Identity inverse-bind matrices: PD vertices are already bone-local.
    ident = [1.0 if i % 5 == 0 else 0.0 for i in range(16)]
    a_ibm = glb.accessor(ident * len(rig), CT_F32, "MAT4")
    skin = glb._append(
        "skins", {"name": "pd_rig", "inverseBindMatrices": a_ibm, "joints": node_of}
    )
    mesh_node = glb._append("nodes", {"name": "body", "mesh": mesh, "skin": skin})
    glb.doc["scenes"][0]["nodes"].append(mesh_node)

    glb.save(out)
    stats = {
        "model": model.name,
        "head": head_name,
        "joints": len(rig),
        "blends": sum(1 for j in rig.joints if j.is_blend),
        "verts": nverts,
        "tris": ntris,
        "parts": len(groups),
        "textures": sum(1 for _, cfg, *_ in batches.values() if cfg is not None),
    }
    print(
        f"{model.name}{' + ' + head_name if head_name else ''} -> {out}: "
        f"{stats['verts']} verts, {stats['tris']} tris, {stats['textures']} textures, "
        f"{stats['joints']} joints ({stats['blends']} blend)"
    )
    return stats


# ---------------------------------------------------------------------------
# clip: animation-only GLB
# ---------------------------------------------------------------------------


def export_clip(
    anim_id: str,
    out: str,
    ref_model: str,
    scale: float,
    fps: float = DEFAULT_FPS,
    blend_joints: bool = True,
    name: str | None = None,
) -> dict:
    """Export one PD animation as a clip-only GLB bound to the shared rig.

    A reference character supplies the rig (bone names, rest offsets, which parts
    have blend joints). Every character on the 15-bone rig produces the same joint
    *names*, which is all `clip.rs` binds against, so one export drives them all —
    the rest offsets only matter for the joints whose translation this clip does
    not animate, and those come from the body's own skeleton at runtime anyway.
    """
    model = load(ref_model)
    rig = Rig(model, blend_joints=blend_joints)
    anim = load_animation(anim_id)
    if anim.numframes <= 0:
        raise SystemExit(f"{anim.id} has no frames")

    glb = Glb("pd_gltf.py (Perfect Dark -> glTF)")
    node_of = rig_nodes(glb, rig, scale)

    times = [i / fps for i in range(anim.numframes)]
    a_time = glb.accessor(times, CT_F32, "SCALAR", minmax=True)

    # Sample every part once for the whole clip, then split into channels.
    rots: list[list[float]] = [[] for _ in rig.joints]
    trans: list[list[float]] = [[] for _ in rig.joints]
    animates_translation = [False] * len(rig.joints)
    turned = 0.0
    for frame in range(anim.numframes):
        for i, j in enumerate(rig.joints):
            rot, tra = anim.part_transform(j.part, frame)
            # **Root motion** (`ANIMFIELD_08`) is a separate channel the game reads
            # with `anim_get_pos_angle_as_int` and feeds into the model transform
            # (`model.c:1856`) — it is where the clip's travel lives. Without it a
            # death rotates into a heap while its hips stay at standing height; with
            # it the pelvis drops 1083 mm -> 209 mm and the body lands. It only ever
            # appears on part 0, whose rest offset is the origin, and a blend joint
            # is the owning bone's *sibling*, so adding it to both is not a
            # double-count.
            mx, my, mz, angle = anim.part_motion(j.part, frame)
            turned = max(turned, abs(angle))
            q = quat_from_pd_rotation(*rot)
            if j.is_blend:
                q = quat_halfway(q)
            rots[i] += list(q)
            if tra != (0.0, 0.0, 0.0) or (mx, my, mz) != (0.0, 0.0, 0.0):
                animates_translation[i] = True
            # `model.c:1158` — the animation translation is ADDED to the node's
            # rest offset, it does not replace it.
            trans[i] += [
                (j.offset[0] + tra[0] + mx) * scale,
                (j.offset[1] + tra[1] + my) * scale,
                (j.offset[2] + tra[2] + mz) * scale,
            ]

    samplers: list[dict] = []
    channels: list[dict] = []
    for i, j in enumerate(rig.joints):
        s = len(samplers)
        samplers.append(
            {"input": a_time, "output": glb.accessor(rots[i], CT_F32, "VEC4"), "interpolation": "LINEAR"}
        )
        channels.append({"sampler": s, "target": {"node": node_of[i], "path": "rotation"}})
        # Only emit translation where the clip actually moves the joint; otherwise
        # the bone's bind translation (its rest offset) already says the same
        # thing, and `clip.rs` composes against it.
        if animates_translation[i]:
            s = len(samplers)
            samplers.append(
                {
                    "input": a_time,
                    "output": glb.accessor(trans[i], CT_F32, "VEC3"),
                    "interpolation": "LINEAR",
                }
            )
            channels.append({"sampler": s, "target": {"node": node_of[i], "path": "translation"}})

    glb._append(
        "animations",
        {"name": name or anim.id, "samplers": samplers, "channels": channels},
    )
    glb.save(out)

    moved = sum(1 for i in range(len(rig.joints)) if animates_translation[i])
    duration = (anim.numframes - 1) / fps
    print(
        f"{anim.id} -> {out}: {anim.numframes} frames, {duration:.2f}s @ {fps:g}fps, "
        f"{len(channels)} channels ({moved} translated)"
    )
    # The root-motion field's fourth channel turns the whole character. The engine
    # owns a hunter's facing, so it is deliberately NOT baked into the clip — say so
    # if a clip actually uses it rather than dropping it silently.
    if turned > 1e-3:
        print(
            f"  note: {anim.id} also turns the root by up to {math.degrees(turned):.1f}deg; "
            "that channel is not exported (the game drives facing)"
        )
    return {
        "anim": anim.id,
        "frames": anim.numframes,
        "duration": duration,
        "channels": len(channels),
    }


# ---------------------------------------------------------------------------
# batch
# ---------------------------------------------------------------------------


def export_batch(manifest_path: str, outdir: str, scale: float, blend_joints: bool) -> int:
    """Export everything named in a JSON manifest.

    A character is either a bare body name or a `[body, head]` pair; heads and
    bodies mix freely in PD (`g_HeadsAndBodies`, `game/modeldata/robot.c:64`), so
    the pairing is an authoring choice and belongs here rather than in a rule.

    ```json
    {
      "characters": {"pd_a51guard": "a51guard", "pd_joanna": ["dark_frock", "headdark_frock"]},
      "clips":      {"00-idle": "ANIM_TWO_GUN_HOLD"},
      "clip_rig":   "a51guard",
      "fps": 30
    }
    ```
    """
    with open(manifest_path, encoding="utf-8") as fh:
        man = json.load(fh)
    chrs_dir = os.path.join(assets_root(), "files", "chrs")
    fps = float(man.get("fps", DEFAULT_FPS))

    chr_out = os.path.join(outdir, "characters")
    clip_out = os.path.join(outdir, "animations")
    os.makedirs(chr_out, exist_ok=True)
    os.makedirs(clip_out, exist_ok=True)

    for name, src in man.get("characters", {}).items():
        body, head = (src, None) if isinstance(src, str) else (src[0], src[1])
        export_char(
            os.path.join(chrs_dir, body + ".bin"),
            os.path.join(chr_out, name + ".glb"),
            scale,
            blend_joints,
            os.path.join(chrs_dir, head + ".bin") if head else None,
        )
    ref = os.path.join(chrs_dir, man.get("clip_rig", "a51guard") + ".bin")
    for name, anim_id in man.get("clips", {}).items():
        export_clip(
            anim_id,
            os.path.join(clip_out, name + ".glb"),
            ref,
            scale,
            fps,
            blend_joints,
            name=name,
        )
    return 0


def assets_root() -> str:
    return os.path.join(
        os.path.dirname(os.path.abspath(__file__)),
        "..", "..", "reference", "pd-decomp", "src", "assets", "ntsc-final",
    )


# ---------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--metres", action="store_true", help="export in metres, not engine units")
    ap.add_argument(
        "--no-blend-joints",
        action="store_true",
        help="fold PD blend matrices onto their owning joint (the pd_pose.py approximation)",
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("char", help="skinned character GLB")
    p.add_argument("model")
    p.add_argument("out")
    p.add_argument("--head", default=None, help="head*.bin to graft onto the HEADSPOT")

    p = sub.add_parser("clip", help="animation-only GLB")
    p.add_argument("anim")
    p.add_argument("out")
    p.add_argument("--rig", default=None, help="chr .bin supplying the rig (default a51guard)")
    p.add_argument("--fps", type=float, default=DEFAULT_FPS)
    p.add_argument("--name", default=None)

    p = sub.add_parser("batch", help="export everything in a JSON manifest")
    p.add_argument("manifest")
    p.add_argument("outdir")

    args = ap.parse_args()
    scale = 1.0 / UNITS_PER_METRE if args.metres else EXPORT_SCALE
    blends = not args.no_blend_joints

    if args.cmd == "char":
        export_char(args.model, args.out, scale, blends, args.head)
        return 0
    if args.cmd == "clip":
        rig = args.rig or os.path.join(assets_root(), "files", "chrs", "a51guard.bin")
        export_clip(args.anim, args.out, rig, scale, args.fps, blends, args.name)
        return 0
    return export_batch(args.manifest, args.outdir, scale, blends)


if __name__ == "__main__":
    sys.exit(main())
