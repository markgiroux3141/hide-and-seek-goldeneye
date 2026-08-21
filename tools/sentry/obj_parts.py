#!/usr/bin/env python3
"""Parse a GoldenEye-editor OBJ and split it into rigid parts.

The sentry gun ships as one undivided mesh (`g primary` / `g secondary`, no named
sub-objects), so before the barrel can spin or the head can track, the geometry has
to be segmented. This module does that segmentation *outside* the engine, so the
result can be looked at (see `sentry_preview.py`) before any Rust is written.

Segmentation is **connected components over shared vertex positions**: the editor
emits welded verts within a piece and duplicated verts between pieces, so "which
triangles touch each other" recovers the authored parts almost exactly. Components
are then classified by their bounding boxes.
"""

from __future__ import annotations

import math
import os
from collections import defaultdict


def load_obj(path):
    """Return (positions, uvs, faces) with faces as (material, [(vi, ti) x3])."""
    positions, uvs, faces = [], [], []
    cur_mat = ""
    group = ""
    mtllib = None
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            t = line.split()
            if not t:
                continue
            if t[0] == "v":
                positions.append((float(t[1]), float(t[2]), float(t[3])))
            elif t[0] == "vt":
                uvs.append((float(t[1]), float(t[2])))
            elif t[0] == "usemtl":
                cur_mat = t[1] if len(t) > 1 else ""
            elif t[0] == "mtllib":
                mtllib = t[1]
            elif t[0] == "g":
                group = t[1] if len(t) > 1 else ""
            elif t[0] == "f":
                corners = []
                for tok in t[1:]:
                    bits = tok.split("/")
                    vi = int(bits[0]) - 1
                    ti = int(bits[1]) - 1 if len(bits) > 1 and bits[1] else vi
                    corners.append((vi, ti))
                for i in range(1, len(corners) - 1):
                    faces.append(
                        (cur_mat, group, [corners[0], corners[i], corners[i + 1]])
                    )
    return positions, uvs, faces, mtllib


def load_mtl(path):
    """Return {name: (kd_rgb, map_kd)}."""
    out = {}
    cur = None
    if not os.path.exists(path):
        return out
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            t = line.split()
            if not t:
                continue
            if t[0] == "newmtl":
                cur = t[1]
                out[cur] = ([1.0, 1.0, 1.0], None)
            elif t[0] == "Kd" and cur:
                out[cur] = ([float(t[1]), float(t[2]), float(t[3])], out[cur][1])
            elif t[0] == "map_Kd" and cur:
                out[cur] = (out[cur][0], t[-1])
    return out


def components(positions, faces, weld=1e-4):
    """Connected components of triangles, joined through welded vertex positions.

    Returns a list of face-index lists, ordered largest-first.
    """
    # Weld: map each distinct position (rounded) to a canonical id.
    canon = {}
    vid = []
    for p in positions:
        key = (round(p[0] / weld), round(p[1] / weld), round(p[2] / weld))
        if key not in canon:
            canon[key] = len(canon)
        vid.append(canon[key])

    # Union-find over canonical vertex ids.
    parent = list(range(len(canon)))

    def find(a):
        while parent[a] != a:
            parent[a] = parent[parent[a]]
            a = parent[a]
        return a

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for _mat, _grp, corners in faces:
        a, b, c = (vid[ci[0]] for ci in corners)
        union(a, b)
        union(b, c)

    groups = defaultdict(list)
    for fi, (_mat, _grp, corners) in enumerate(faces):
        groups[find(vid[corners[0][0]])].append(fi)
    comps = sorted(groups.values(), key=len, reverse=True)
    return comps


def bounds(positions, faces, face_idx):
    lo = [math.inf] * 3
    hi = [-math.inf] * 3
    for fi in face_idx:
        for ci in faces[fi][2]:
            p = positions[ci[0]]
            for k in range(3):
                lo[k] = min(lo[k], p[k])
                hi[k] = max(hi[k], p[k])
    return lo, hi


if __name__ == "__main__":
    import sys

    path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.abspath(__file__)),
        "..", "..", "native", "assets", "props", "sentry_gun", "sentry_gun.obj",
    )
    pos, uvs, faces, mtllib = load_obj(path)
    print(f"{len(pos)} verts  {len(faces)} tris  mtllib={mtllib}")
    lo = [min(p[k] for p in pos) for k in range(3)]
    hi = [max(p[k] for p in pos) for k in range(3)]
    print(f"whole bounds  X[{lo[0]:8.1f},{hi[0]:8.1f}]  "
          f"Y[{lo[1]:8.1f},{hi[1]:8.1f}]  Z[{lo[2]:8.1f},{hi[2]:8.1f}]")
    print(f"metres        {(hi[0]-lo[0])/1000:.3f} x {(hi[1]-lo[1])/1000:.3f} "
          f"x {(hi[2]-lo[2])/1000:.3f}")
    print()
    comps = components(pos, faces)
    print(f"{len(comps)} connected components:")
    for i, c in enumerate(comps):
        clo, chi = bounds(pos, faces, c)
        mats = sorted({faces[fi][0] for fi in c})
        grps = sorted({faces[fi][1] for fi in c})
        ctr = [(clo[k] + chi[k]) / 2 for k in range(3)]
        print(f"  #{i:2d} tris={len(c):3d}  "
              f"X[{clo[0]:7.0f},{chi[0]:7.0f}] Y[{clo[1]:7.0f},{chi[1]:7.0f}] "
              f"Z[{clo[2]:7.0f},{chi[2]:7.0f}]  "
              f"ctr=({ctr[0]:6.0f},{ctr[1]:6.0f},{ctr[2]:6.0f})  "
              f"{grps} {','.join(mats)[:60]}")
