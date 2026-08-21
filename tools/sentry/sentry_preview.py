#!/usr/bin/env python3
"""Render the sentry-gun OBJ to a PNG on the CPU — textured, or part-coloured.

This is the "let me see it" half of the auto-turret work. The sentry gun ships as one
undivided GoldenEye mesh, so making the barrel spin and the head track means inventing
a part split; this draws the model, the split, and the split *animating*, without a
build, a GPU or the game running.

Modes:
    --mode textured    the model as the engine draws it (Kd x BMP texel, unlit)
    --mode parts       each connected component in a flat colour, with a legend
    --mode components  one tile per component, each framed on its own bounds
    --mode rig         the assembled turret (see sentry_rig.py), posed by the flags
    --mode anim        a contact sheet of N frames sweeping yaw/pitch/barrel-spin
    --mode spin        a muzzle close-up across one sixth-turn of the bundle

Usage:
    python sentry_preview.py out.png
    python sentry_preview.py out.png --mode parts --views front,side,top,iso
    python sentry_preview.py out.png --mode components --views iso
    python sentry_preview.py out.png --mode rig --pitch -50 --flat
    python sentry_preview.py out.png --mode anim --frames 8

`--obj` points it at any OBJ, which is how a pose dumped by the engine
(`cargo run --release -p game --example turret_pose_dump`) gets rendered through this
same rasteriser and compared against the Python rig.
"""

from __future__ import annotations

import argparse
import math
import os
import struct
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "pd-assets"))

from obj_parts import bounds, components, load_mtl, load_obj  # noqa: E402
from pd_gltf import png_bytes  # noqa: E402

DEFAULT_OBJ = os.path.join(
    HERE, "..", "..", "native", "assets", "props", "sentry_gun", "sentry_gun.obj"
)

#: Flat colours for the part-coloured mode, in part order.
PART_COLORS = [
    (230, 80, 80),    # 0
    (80, 170, 230),   # 1
    (110, 210, 110),  # 2
    (235, 190, 70),   # 3
    (190, 120, 230),  # 4
    (240, 140, 60),   # 5
    (100, 230, 210),  # 6
    (230, 120, 180),  # 7
]
BG = (26, 28, 34)


# ---------------------------------------------------------------------------
# BMP decode (32-bit BI_RGB with real alpha, else 24-bit) — mirrors obj_model.rs
# ---------------------------------------------------------------------------
def load_bmp(path):
    """Return (w, h, rgba bytes, top-left origin) or None."""
    try:
        d = open(path, "rb").read()
    except OSError:
        return None
    if len(d) < 54 or d[:2] != b"BM":
        return None
    data_off = struct.unpack_from("<I", d, 10)[0]
    w = struct.unpack_from("<i", d, 18)[0]
    h_raw = struct.unpack_from("<i", d, 22)[0]
    bpp = struct.unpack_from("<H", d, 28)[0]
    comp = struct.unpack_from("<I", d, 30)[0]
    if w <= 0 or h_raw == 0 or comp not in (0, 3) or bpp not in (24, 32):
        return None
    h = abs(h_raw)
    top_down = h_raw < 0
    bypp = bpp // 8
    stride = (w * bypp + 3) & ~3
    if len(d) < data_off + stride * h:
        return None
    out = bytearray(w * h * 4)
    for y in range(h):
        src_row = y if top_down else h - 1 - y
        base = data_off + src_row * stride
        for x in range(w):
            s = base + x * bypp
            o = (y * w + x) * 4
            out[o] = d[s + 2]
            out[o + 1] = d[s + 1]
            out[o + 2] = d[s]
            out[o + 3] = d[s + 3] if bypp == 4 else 255
    return (w, h, bytes(out))


# ---------------------------------------------------------------------------
# Tiny 3x5 bitmap font, enough for a legend
# ---------------------------------------------------------------------------
GLYPHS = {
    "0": "111101101101111", "1": "010110010010111", "2": "111001111100111",
    "3": "111001111001111", "4": "101101111001001", "5": "111100111001111",
    "6": "111100111101111", "7": "111001001001001", "8": "111101111101111",
    "9": "111101111001111", "A": "111101111101101", "B": "110101110101110",
    "C": "111100100100111", "D": "110101101101110", "E": "111100110100111",
    "F": "111100110100100", "G": "111100101101111", "H": "101101111101101",
    "I": "111010010010111", "J": "001001001101111", "K": "101101110101101",
    "L": "100100100100111", "M": "101111111101101", "N": "101111111111101",
    "O": "111101101101111", "P": "111101111100100", "Q": "111101101111011",
    "R": "111101110101101", "S": "111100111001111", "T": "111010010010010",
    "U": "101101101101111", "V": "101101101101010", "W": "101101111111101",
    "X": "101101010101101", "Y": "101101010010010", "Z": "111001010100111",
    " ": "000000000000000", "-": "000000111000000", ".": "000000000000010",
    ":": "000010000010000", "+": "000010111010000", "/": "001001010100100",
    "(": "010100100100010", ")": "010001001001010", "=": "000111000111000",
    "*": "000101010101000", "%": "101001010100101", ",": "000000000010100",
}


class Canvas:
    def __init__(self, w, h, bg=BG):
        self.w, self.h = w, h
        self.px = bytearray(w * h * 4)
        for i in range(w * h):
            self.px[i * 4 : i * 4 + 4] = bytes((bg[0], bg[1], bg[2], 255))
        self.depth = [1e30] * (w * h)

    def put(self, x, y, rgb):
        if 0 <= x < self.w and 0 <= y < self.h:
            o = (y * self.w + x) * 4
            self.px[o] = rgb[0]
            self.px[o + 1] = rgb[1]
            self.px[o + 2] = rgb[2]
            self.px[o + 3] = 255

    def blend(self, x, y, rgb, a):
        if 0 <= x < self.w and 0 <= y < self.h and a > 0:
            o = (y * self.w + x) * 4
            for k in range(3):
                self.px[o + k] = int(self.px[o + k] * (1 - a) + rgb[k] * a)
            self.px[o + 3] = 255

    def text(self, x, y, s, rgb=(235, 235, 235), scale=2):
        cx = x
        for ch in s.upper():
            g = GLYPHS.get(ch)
            if g:
                for r in range(5):
                    for c in range(3):
                        if g[r * 3 + c] == "1":
                            for dy in range(scale):
                                for dx in range(scale):
                                    self.put(cx + c * scale + dx, y + r * scale + dy, rgb)
            cx += 4 * scale

    def rect(self, x, y, w, h, rgb):
        for yy in range(y, y + h):
            for xx in range(x, x + w):
                self.put(xx, yy, rgb)

    def png(self, path):
        with open(path, "wb") as fh:
            fh.write(png_bytes(self.w, self.h, bytes(self.px)))


# ---------------------------------------------------------------------------
# Math
# ---------------------------------------------------------------------------
def mat_ident():
    return [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]


def mat_mul(a, b):
    """Row-major 4x4, applied as v' = v * (a then b)? No: returns a@b."""
    o = [0.0] * 16
    for r in range(4):
        for c in range(4):
            o[r * 4 + c] = sum(a[r * 4 + k] * b[k * 4 + c] for k in range(4))
    return o


def mat_translate(t):
    m = mat_ident()
    m[12], m[13], m[14] = t
    return m


def mat_rot(axis, ang):
    c, s = math.cos(ang), math.sin(ang)
    m = mat_ident()
    if axis == "x":
        m[5], m[6], m[9], m[10] = c, s, -s, c
    elif axis == "y":
        m[0], m[2], m[8], m[10] = c, -s, s, c
    else:
        m[0], m[1], m[4], m[5] = c, s, -s, c
    return m


def xform(m, p):
    x, y, z = p
    return (
        x * m[0] + y * m[4] + z * m[8] + m[12],
        x * m[1] + y * m[5] + z * m[9] + m[13],
        x * m[2] + y * m[6] + z * m[10] + m[14],
    )


def pivot_rot(axis, ang, pivot):
    """Rotate about `axis` through world point `pivot`."""
    return mat_mul(
        mat_mul(mat_translate((-pivot[0], -pivot[1], -pivot[2])), mat_rot(axis, ang)),
        mat_translate(pivot),
    )


# ---------------------------------------------------------------------------
# Camera / views
# ---------------------------------------------------------------------------
VIEWS = {
    "front": (0.0, 0.0),      # looking down -Z at the model's front
    "back": (math.pi, 0.0),
    "side": (math.pi / 2, 0.0),
    "top": (0.0, -math.pi / 2 + 0.001),
    "iso": (math.radians(35), math.radians(-22)),
    "iso2": (math.radians(-130), math.radians(-18)),
}


def view_matrix(yaw, pitch):
    return mat_mul(mat_rot("y", yaw), mat_rot("x", pitch))


def draw_model(cv, tris, ox, oy, size, center, radius, view, shade=True):
    """Rasterise triangles (list of (pts3, colorfn)) orthographically into `cv`.

    `colorfn(u, v, w)` returns (r,g,b,a) for the barycentric point, or a flat tuple.
    """
    vm = view_matrix(*view)
    scale = size * 0.42 / radius
    proj = []
    for pts, cf, nrm in tris:
        vs = []
        for p in pts:
            q = xform(vm, (p[0] - center[0], p[1] - center[1], p[2] - center[2]))
            vs.append((ox + size / 2 + q[0] * scale, oy + size / 2 - q[1] * scale, q[2]))
        n = xform(vm, nrm)
        proj.append((vs, cf, n))

    for vs, cf, n in proj:
        # Flat lambert-ish shade from a fixed key light in view space.
        lam = 0.45 + 0.55 * max(0.0, min(1.0, n[2] * 0.6 + n[1] * 0.45 + 0.35))
        if not shade:
            lam = 1.0
        (x0, y0, z0), (x1, y1, z1), (x2, y2, z2) = vs
        minx = max(int(min(x0, x1, x2)), ox)
        maxx = min(int(max(x0, x1, x2)) + 1, ox + size)
        miny = max(int(min(y0, y1, y2)), oy)
        maxy = min(int(max(y0, y1, y2)) + 1, oy + size)
        area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0)
        if abs(area) < 1e-9:
            continue
        for py in range(miny, maxy):
            for px in range(minx, maxx):
                cx, cy = px + 0.5, py + 0.5
                w0 = ((x1 - cx) * (y2 - cy) - (x2 - cx) * (y1 - cy)) / area
                w1 = ((x2 - cx) * (y0 - cy) - (x0 - cx) * (y2 - cy)) / area
                w2 = 1.0 - w0 - w1
                if w0 < -1e-6 or w1 < -1e-6 or w2 < -1e-6:
                    continue
                z = w0 * z0 + w1 * z1 + w2 * z2
                di = py * cv.w + px
                if z >= cv.depth[di]:
                    continue
                col = cf(w0, w1, w2)
                if col is None:
                    continue
                r, g, b, a = col
                if a <= 2:
                    continue
                rgb = (
                    min(255, int(r * lam)),
                    min(255, int(g * lam)),
                    min(255, int(b * lam)),
                )
                if a >= 250:
                    cv.depth[di] = z
                    cv.put(px, py, rgb)
                else:
                    cv.blend(px, py, rgb, a / 255.0)


# ---------------------------------------------------------------------------
# Model assembly
# ---------------------------------------------------------------------------
class Model:
    def __init__(self, path):
        self.dir = os.path.dirname(os.path.abspath(path))
        self.pos, self.uvs, self.faces, mtllib = load_obj(path)
        self.mtl = load_mtl(os.path.join(self.dir, mtllib)) if mtllib else {}
        self.tex = {}
        for name, (_kd, mk) in self.mtl.items():
            if mk and mk not in self.tex:
                self.tex[mk] = load_bmp(os.path.join(self.dir, mk))
        self.comps = components(self.pos, self.faces)
        # face index -> component index
        self.comp_of = {}
        for ci, c in enumerate(self.comps):
            for fi in c:
                self.comp_of[fi] = ci

    def bbox(self):
        lo = [min(p[k] for p in self.pos) for k in range(3)]
        hi = [max(p[k] for p in self.pos) for k in range(3)]
        return lo, hi

    def center_radius(self):
        lo, hi = self.bbox()
        c = [(lo[k] + hi[k]) / 2 for k in range(3)]
        r = max(hi[k] - lo[k] for k in range(3)) / 2
        return c, max(r, 1e-3)

    def tris(self, part_matrix=None, part_of=None, flat_color=None, alpha_mul=1.0):
        """Build render triangles.

        `part_of(face_idx) -> part id`; `part_matrix(part_id) -> 4x4` (or None);
        `flat_color(part_id) -> (r,g,b)` forces a flat colour instead of texturing.
        """
        out = []
        for fi, (mat, grp, corners) in enumerate(self.faces):
            pid = part_of(fi) if part_of else 0
            m = part_matrix(pid) if part_matrix else None
            pts = []
            for vi, _ti in corners:
                p = self.pos[vi]
                pts.append(xform(m, p) if m else p)
            e1 = [pts[1][k] - pts[0][k] for k in range(3)]
            e2 = [pts[2][k] - pts[0][k] for k in range(3)]
            n = (
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            )
            ln = math.sqrt(sum(v * v for v in n)) or 1.0
            n = tuple(v / ln for v in n)

            kd, mk = self.mtl.get(mat, ([1.0, 1.0, 1.0], None))
            if flat_color:
                c = flat_color(pid)
                out.append((pts, (lambda c=c: (lambda u, v, w: (c[0], c[1], c[2], 255)))(), n))
                continue
            img = self.tex.get(mk) if mk else None
            uv = [self.uvs[ti] if ti < len(self.uvs) else (0.0, 0.0) for _vi, ti in corners]

            def cf(u, v, w, uv=uv, img=img, kd=kd):
                if img is None:
                    return (int(kd[0] * 255), int(kd[1] * 255), int(kd[2] * 255), 255)
                iw, ih, data = img
                tu = u * uv[0][0] + v * uv[1][0] + w * uv[2][0]
                tv = u * uv[0][1] + v * uv[1][1] + w * uv[2][1]
                px = int(tu * iw) % iw
                py = int((1.0 - tv) * ih) % ih
                o = (py * iw + px) * 4
                return (
                    int(data[o] * kd[0]),
                    int(data[o + 1] * kd[1]),
                    int(data[o + 2] * kd[2]),
                    int(data[o + 3] * alpha_mul),
                )

            out.append((pts, cf, n))
        return out


# ---------------------------------------------------------------------------
# Entry
# ---------------------------------------------------------------------------
def render_sheet(model, out, views, mode, size, part_of=None, part_matrix=None,
                 labels=None, title=""):
    n = len(views)
    cols = min(n, 4)
    rows = (n + cols - 1) // cols
    pad = 8
    head = 34
    legend = 26 if labels else 0
    cv = Canvas(cols * size + pad * (cols + 1), rows * size + pad * (rows + 1) + head + legend)
    if title:
        cv.text(pad, 10, title, (240, 240, 240), 2)
    center, radius = model.center_radius()
    flat = None
    if mode == "parts":
        flat = lambda pid: PART_COLORS[pid % len(PART_COLORS)]
    tris = model.tris(part_matrix=part_matrix, part_of=part_of, flat_color=flat)
    for i, vname in enumerate(views):
        r, c = divmod(i, cols)
        ox = pad + c * (size + pad)
        oy = head + pad + r * (size + pad)
        cv.rect(ox, oy, size, size, (18, 19, 24))
        for x in range(ox, ox + size):
            cv.put(x, oy, (60, 64, 76))
            cv.put(x, oy + size - 1, (60, 64, 76))
        draw_model(cv, tris, ox, oy, size, center, radius, VIEWS[vname])
        cv.text(ox + 4, oy + size - 14, vname, (170, 180, 200), 2)
    if labels:
        x = pad
        y = cv.h - legend + 6
        for pid, name in labels:
            col = PART_COLORS[pid % len(PART_COLORS)]
            cv.rect(x, y, 12, 12, col)
            cv.text(x + 16, y + 1, name, (215, 220, 230), 2)
            x += 16 + len(name) * 8 + 18
    cv.png(out)
    print(f"wrote {out}  ({cv.w}x{cv.h})")


def render_component_sheet(model, out, view, size):
    """One tile per connected component, textured, each framed on its own bounds —
    the "what IS this piece?" view. Framing is per-component so a 2-tri plane is as
    legible as the 18-tri barrel bundle."""
    n = len(model.comps)
    cols = min(n, 3)
    rows = (n + cols - 1) // cols
    pad, head = 8, 34
    cv = Canvas(cols * size + pad * (cols + 1), rows * (size + 18) + pad * (rows + 1) + head)
    cv.text(pad, 10, "sentry gun - components (each framed on itself)", (240, 240, 240), 2)
    for i, comp in enumerate(model.comps):
        r, c = divmod(i, cols)
        ox = pad + c * (size + pad)
        oy = head + pad + r * (size + 18 + pad)
        cv.rect(ox, oy, size, size, (18, 19, 24))
        lo, hi = bounds(model.pos, model.faces, comp)
        ctr = [(lo[k] + hi[k]) / 2 for k in range(3)]
        rad = max(max(hi[k] - lo[k] for k in range(3)) / 2, 1.0)
        keep = set(comp)
        tris = [t for fi, t in enumerate(model.tris()) if fi in keep]
        draw_model(cv, tris, ox, oy, size, ctr, rad, VIEWS[view])
        col = PART_COLORS[i % len(PART_COLORS)]
        cv.rect(ox, oy + size + 2, 12, 12, col)
        dims = f"{(hi[0]-lo[0])/1000:.2f}x{(hi[1]-lo[1])/1000:.2f}x{(hi[2]-lo[2])/1000:.2f}M"
        cv.text(ox + 16, oy + size + 3,
                f"C{i} {len(comp)}T {dims} CTR {ctr[0]:.0f},{ctr[1]:.0f},{ctr[2]:.0f}",
                (215, 220, 230), 2)
    cv.png(out)
    print(f"wrote {out}  ({cv.w}x{cv.h})")


# ---------------------------------------------------------------------------
# The rig: assembled + articulated
# ---------------------------------------------------------------------------
NODE_ORDER = ["mount", "yaw", "pitch", "spin"]


def rig_matrices(rig, yaw, pitch, spin):
    """The 4x4 for each rig node at a given (yaw, pitch, spin), in GE units.

    Each node's matrix is: slide the part's shelf into place, then apply this node's
    own rotation and every rotation above it in the chain. Composed parent-last so a
    yaw carries the pitch and spin with it, exactly as the Rust side will.
    """
    m_yaw = pivot_rot("y", yaw, rig.YAW_AXIS_POINT)
    # The pitch pivot must itself be yawed, so pitch happens about the *turned*
    # trunnion, not the model-space one.
    m_pitch = mat_mul(pivot_rot("z", pitch, rig.PITCH_AXIS_POINT), m_yaw)
    m_spin = mat_mul(
        mat_mul(pivot_rot("x", spin, rig.SPIN_AXIS_POINT),
                pivot_rot("z", pitch, rig.PITCH_AXIS_POINT)),
        m_yaw,
    )
    return {"mount": mat_ident(), "yaw": m_yaw, "pitch": m_pitch, "spin": m_spin}


def mat_scale(s):
    m = mat_ident()
    m[0], m[5], m[10] = s
    return m


def rig_part_matrix(rig, comp_idx, yaw, pitch, spin):
    """`p' = p * scale + offset`, then this part's node rotation."""
    node, scale, off = rig.PARTS[comp_idx]
    place = mat_mul(mat_scale(scale), mat_translate(off))
    return mat_mul(place, rig_matrices(rig, yaw, pitch, spin)[node])


def assembled_frame(model, rig, yaw, pitch, spin, flat=False):
    """Render triangles for the assembled turret at one articulation state."""
    part_of = lambda fi: model.comp_of[fi]
    pm = {}
    for ci in range(len(model.comps)):
        pm[ci] = rig_part_matrix(rig, ci, yaw, pitch, spin)
    color = None
    if flat:
        node_color = {"mount": 0, "yaw": 3, "pitch": 1, "spin": 2}
        color = lambda ci: PART_COLORS[node_color[rig.NODE_OF_COMP[ci]]]
    return model.tris(part_matrix=lambda ci: pm[ci], part_of=part_of, flat_color=color)


def rig_center_radius(model, rig):
    """Frame on the assembled turret at rest, not on the exploded sheet."""
    lo = [math.inf] * 3
    hi = [-math.inf] * 3
    for ci, comp in enumerate(model.comps):
        m = rig_part_matrix(rig, ci, 0.0, 0.0, 0.0)
        for fi in comp:
            for vi, _ti in model.faces[fi][2]:
                p = xform(m, model.pos[vi])
                for k in range(3):
                    lo[k] = min(lo[k], p[k])
                    hi[k] = max(hi[k], p[k])
    c = [(lo[k] + hi[k]) / 2 for k in range(3)]
    return c, max(max(hi[k] - lo[k] for k in range(3)) / 2, 1.0), (lo, hi)


def render_rig(model, rig, out, views, size, yaw, pitch, spin, flat, title):
    n = len(views)
    cols = min(n, 4)
    rows = (n + cols - 1) // cols
    pad, head = 8, 34
    cv = Canvas(cols * size + pad * (cols + 1), rows * size + pad * (rows + 1) + head + 26)
    cv.text(pad, 10, title, (240, 240, 240), 2)
    center, radius, (lo, hi) = rig_center_radius(model, rig)
    tris = assembled_frame(model, rig, yaw, pitch, spin, flat=flat)
    for i, vname in enumerate(views):
        r, c = divmod(i, cols)
        ox = pad + c * (size + pad)
        oy = head + pad + r * (size + pad)
        cv.rect(ox, oy, size, size, (18, 19, 24))
        draw_model(cv, tris, ox, oy, size, center, radius, VIEWS[vname])
        cv.text(ox + 4, oy + size - 14, vname, (170, 180, 200), 2)
    s = rig.RIG_SCALE / 1000.0
    cv.text(pad, cv.h - 20,
            f"assembled {(hi[0]-lo[0])*s:.2f} x {(hi[1]-lo[1])*s:.2f} x "
            f"{(hi[2]-lo[2])*s:.2f} M at scale {rig.RIG_SCALE}  "
            f"(hangs {-lo[1]*s:.2f} M below ceiling)", (170, 180, 200), 2)
    cv.png(out)
    print(f"wrote {out}  ({cv.w}x{cv.h})")


def render_spin(model, rig, out, view, size, frames):
    """A close-up on the muzzle across one sixth-turn — the barrel-spin check. The
    bundle has 6 barrels, so 60 degrees is a full visual cycle; if the hex face does
    not step round evenly here, the spin axis is off the bore."""
    cols = min(frames, 6)
    rows = (frames + cols - 1) // cols
    pad, head = 8, 34
    cv = Canvas(cols * size + pad * (cols + 1), rows * (size + 16) + pad * (rows + 1) + head)
    cv.text(pad, 10, "sentry gun - barrel spin, one sixth-turn (muzzle close-up)",
            (240, 240, 240), 2)
    focus = (rig.MUZZLE[0] - 150.0, rig.MUZZLE[1], rig.MUZZLE[2])
    for f in range(frames):
        spin = math.radians(60.0) * f / frames
        r, c = divmod(f, cols)
        ox = pad + c * (size + pad)
        oy = head + pad + r * (size + 16 + pad)
        cv.rect(ox, oy, size, size, (18, 19, 24))
        draw_model(cv, assembled_frame(model, rig, 0.0, 0.0, spin),
                   ox, oy, size, focus, 260.0, VIEWS[view])
        cv.text(ox + 4, oy + size + 2, f"SPIN {math.degrees(spin):.0f}",
                (170, 180, 200), 2)
    cv.png(out)
    print(f"wrote {out}  ({cv.w}x{cv.h})")


def render_anim(model, rig, out, view, size, frames, flat):
    """A contact sheet: the turret tracking through yaw+pitch with barrels spinning."""
    cols = min(frames, 4)
    rows = (frames + cols - 1) // cols
    pad, head = 8, 34
    cv = Canvas(cols * size + pad * (cols + 1), rows * (size + 16) + pad * (rows + 1) + head)
    cv.text(pad, 10, f"sentry gun - tracking sweep ({view})", (240, 240, 240), 2)
    center, radius, _ = rig_center_radius(model, rig)
    for f in range(frames):
        t = f / max(frames - 1, 1)
        yaw = math.radians(-60.0 + 120.0 * t)
        pitch = rig.PITCH_MIN * (0.15 + 0.85 * (0.5 - 0.5 * math.cos(t * math.tau)))
        spin = rig.SPIN_RATE * (t * 0.9)
        r, c = divmod(f, cols)
        ox = pad + c * (size + pad)
        oy = head + pad + r * (size + 16 + pad)
        cv.rect(ox, oy, size, size, (18, 19, 24))
        draw_model(cv, assembled_frame(model, rig, yaw, pitch, spin, flat=flat),
                   ox, oy, size, center, radius, VIEWS[view])
        cv.text(ox + 4, oy + size + 2,
                f"YAW {math.degrees(yaw):+.0f} PITCH {math.degrees(pitch):+.0f} "
                f"SPIN {math.degrees(spin) % 360:.0f}", (170, 180, 200), 2)
    cv.png(out)
    print(f"wrote {out}  ({cv.w}x{cv.h})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--obj", default=DEFAULT_OBJ)
    ap.add_argument("--mode", default="textured",
                    choices=["textured", "parts", "components", "rig", "anim", "spin"])
    ap.add_argument("--views", default="front,side,top,iso")
    ap.add_argument("--size", type=int, default=300)
    ap.add_argument("--yaw", type=float, default=0.0, help="degrees")
    ap.add_argument("--pitch", type=float, default=0.0, help="degrees")
    ap.add_argument("--spin", type=float, default=0.0, help="degrees")
    ap.add_argument("--frames", type=int, default=8)
    ap.add_argument("--flat", action="store_true", help="colour by rig node")
    args = ap.parse_args()

    model = Model(args.obj)
    views = args.views.split(",")
    if args.mode == "components":
        render_component_sheet(model, args.out, views[0], args.size)
        return
    if args.mode in ("rig", "anim", "spin"):
        import importlib

        rig = importlib.import_module("sentry_rig")
        if args.mode == "rig":
            render_rig(model, rig, args.out, views, args.size,
                       math.radians(args.yaw), math.radians(args.pitch),
                       math.radians(args.spin), args.flat,
                       "sentry gun - assembled rig"
                       + (" (coloured by node)" if args.flat else ""))
        elif args.mode == "anim":
            render_anim(model, rig, args.out, views[0], args.size, args.frames,
                        args.flat)
        else:
            render_spin(model, rig, args.out, views[0], args.size, args.frames)
        return
    labels = None
    part_of = None
    if args.mode == "parts":
        part_of = lambda fi: model.comp_of[fi]
        labels = [(i, f"c{i} {len(c)}t") for i, c in enumerate(model.comps)]
    render_sheet(model, args.out, views, args.mode, args.size,
                 part_of=part_of, labels=labels,
                 title=f"sentry gun - {args.mode}")


if __name__ == "__main__":
    main()
