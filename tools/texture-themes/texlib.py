"""Index and triage the GoldenEye texture library.

The raw pile is unbrowsable: 1870 BMP files across `public/textures/` and the 21
`public/existing goldeneye levels/<NN - Level>/` folders, of which 222 in the flat
directory are named `tempImgEd02B7.bmp` — the ripper's temp filenames. This module
turns that into something pickable:

* **Content-hash index.** 1870 files collapse to 993 distinct images. Crucially,
  every human-renamed texture in the flat directory has exactly one temp-named
  twin (`grey_tile_floor` == `tempImgEd02B7`, `white_tile` == `tempImgEd02CE`, …),
  so *only* hashing reveals that a theme naming `white_tile` and a GoldenEye room
  naming `tempImgEd02CE` are talking about the same image.
* **Tileability.** The single most useful tag. A texture that tiles has matching
  opposite edges; one that doesn't cannot be a wall or floor at all. This alone
  cuts the pile by a third. It does *not* tell you whether a tiling image is
  material or signage — see the note below.
* **Contact sheets.** Per-level PNG grids at 8x nearest upscale, labelled, so a
  human can actually look at the library.

Usage:
    python tools/texture-themes/texlib.py index    # write out/texture_index.json
    python tools/texture-themes/texlib.py sheets   # write out/sheets/<level>.png
    python tools/texture-themes/texlib.py themes   # write out/sheets/themes_p*.png
"""

from __future__ import annotations

import hashlib
import json
import sys
from collections import defaultdict
from dataclasses import asdict, dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

REPO = Path(__file__).resolve().parents[2]
LEVELS_DIR = REPO / "public" / "existing goldeneye levels"
FLAT_DIRS = [REPO / "public" / "textures", REPO / "public" / "transparent_textures"]
OUT_DIR = Path(__file__).resolve().parent / "out"

# An edge-difference below this (0..1, mean abs channel delta) counts as tiling.
# Chosen from the measured distribution — see `index` output, which prints how many
# textures land either side.
TILEABLE_THRESHOLD = 0.10

# NOTE ON WHAT THIS MODULE DELIBERATELY DOES NOT CLAIM
#
# Seam scores answer "does this tile", and they answer it well. They do NOT answer
# "is this wall material or a sign", and three attempts to derive that from pixel
# statistics all failed on real data:
#
#   * seam score alone — a symbol on a flat field has trivially matching edges, so
#     Caverns' red and green direction arrows score as perfect tiles;
#   * `flat_fraction` (share of pixels on the dominant quantised colour) — flags
#     diamond-plate and a scalloped stone pattern as signage while still passing
#     the arrows;
#   * border uniformity (stddev of the outer pixel ring) — calls low-contrast grey
#     and dark rock "signage" while passing the arrows, a white glow and a metal
#     ring as "material".
#
# At 32x32 there is no cheap statistic for it, so this module does not pretend
# there is. `flat_fraction` is retained as an advisory sort key only — it does put
# the most texture-like images first, which is useful — and the material/signage
# judgement is left to a human looking at the contact sheets. That is what the
# sheets are FOR.


@dataclass
class TexEntry:
    """One distinct image, however many filenames point at it."""

    hash: str
    canonical: str
    names: list[str]
    levels: list[str]
    width: int
    height: int
    # Mean absolute difference between opposite edges, 0..1. Low = tiles cleanly.
    seam_h: float
    seam_v: float
    mean_luma: float
    # Most common colour as #RRGGBB.
    dominant: str
    # Share of pixels sitting on the single most common quantised colour, 0..1.
    # High = a symbol or sign on a flat field; low = real surface detail.
    flat_fraction: float = 0.0

    @property
    def tiles_h(self) -> bool:
        return self.seam_h <= TILEABLE_THRESHOLD

    @property
    def tiles_v(self) -> bool:
        return self.seam_v <= TILEABLE_THRESHOLD

    @property
    def tiles_both(self) -> bool:
        return self.tiles_h and self.tiles_v


def canonical_name(names: set[str]) -> str:
    """Prefer a human-given name over a ripper temp name."""
    human = sorted(n for n in names if not n.startswith("tempImgEd"))
    return human[0] if human else sorted(names)[0]


def seam_scores(img: Image.Image) -> tuple[float, float]:
    """Mean abs difference between opposite edges, horizontal and vertical.

    A tiling texture wraps, so its left column should continue into its right
    column. Comparing them directly is a cheap, surprisingly reliable test.
    """
    rgb = img.convert("RGB")
    w, h = rgb.size
    px = rgb.load()

    def diff(pairs) -> float:
        if not pairs:
            return 1.0
        total = 0.0
        for (x1, y1), (x2, y2) in pairs:
            a, b = px[x1, y1], px[x2, y2]
            total += sum(abs(a[c] - b[c]) for c in range(3)) / 3.0
        return total / len(pairs) / 255.0

    horiz = [((0, y), (w - 1, y)) for y in range(h)]
    vert = [((x, 0), (x, h - 1)) for x in range(w)]
    return diff(horiz), diff(vert)


def image_stats(img: Image.Image) -> tuple[float, str, float]:
    rgb = img.convert("RGB")
    # tobytes + chunking rather than getdata(): stable across Pillow versions
    # (getdata is deprecated for removal in Pillow 14).
    raw = rgb.tobytes()
    pixels = [tuple(raw[i : i + 3]) for i in range(0, len(raw), 3)]
    luma = sum(0.2126 * r + 0.7152 * g + 0.0722 * b for r, g, b in pixels) / len(pixels) / 255.0
    counts: dict[tuple[int, int, int], int] = defaultdict(int)
    for p in pixels:
        # Quantise so near-identical shades don't split the vote.
        counts[(p[0] // 16, p[1] // 16, p[2] // 16)] += 1
    q = max(counts, key=lambda k: counts[k])
    flat = counts[q] / len(pixels)
    return luma, "#%02X%02X%02X" % (q[0] * 16, q[1] * 16, q[2] * 16), flat


def source_dirs() -> list[tuple[str, Path]]:
    """(label, dir) for every directory holding level textures."""
    out = [("flat", d) for d in FLAT_DIRS if d.exists()]
    out += [(d.name, d) for d in sorted(LEVELS_DIR.iterdir()) if d.is_dir()]
    return out


def build_index() -> dict[str, TexEntry]:
    by_hash: dict[str, TexEntry] = {}
    names_by_hash: dict[str, set[str]] = defaultdict(set)
    levels_by_hash: dict[str, set[str]] = defaultdict(set)
    files = 0

    for label, d in source_dirs():
        for f in sorted(d.glob("*.bmp")):
            digest = hashlib.sha1(f.read_bytes()).hexdigest()
            files += 1
            names_by_hash[digest].add(f.stem)
            levels_by_hash[digest].add(label)
            if digest in by_hash:
                continue
            try:
                with Image.open(f) as img:
                    img.load()
                    sh, sv = seam_scores(img)
                    luma, dom, flat = image_stats(img)
                    by_hash[digest] = TexEntry(
                        hash=digest,
                        canonical="",
                        names=[],
                        levels=[],
                        width=img.width,
                        height=img.height,
                        seam_h=round(sh, 4),
                        seam_v=round(sv, 4),
                        mean_luma=round(luma, 4),
                        dominant=dom,
                        flat_fraction=round(flat, 4),
                    )
            except Exception as e:  # a couple of these rips are malformed
                print(f"  ! {f.name}: {e}")

    for digest, entry in by_hash.items():
        entry.names = sorted(names_by_hash[digest])
        entry.canonical = canonical_name(names_by_hash[digest])
        entry.levels = sorted(levels_by_hash[digest])

    print(f"{files} files -> {len(by_hash)} distinct images")
    return by_hash


def name_to_hash(index: dict[str, TexEntry]) -> dict[str, str]:
    """Every known filename -> its content hash (the rename-collapsing map)."""
    out = {}
    for digest, e in index.items():
        for n in e.names:
            out[n] = digest
    return out


def load_index() -> dict[str, TexEntry]:
    path = OUT_DIR / "texture_index.json"
    if not path.exists():
        raise SystemExit(f"{path} not found — run `texlib.py index` first")
    raw = json.loads(path.read_text(encoding="utf-8"))
    return {h: TexEntry(**e) for h, e in raw.items()}


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------


def cmd_index() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    index = build_index()
    path = OUT_DIR / "texture_index.json"
    path.write_text(
        json.dumps({h: asdict(e) for h, e in index.items()}, indent=1), encoding="utf-8"
    )

    multi = [e for e in index.values() if len(e.names) > 1]
    both = [e for e in index.values() if e.tiles_h and e.tiles_v]
    either = [e for e in index.values() if e.tiles_h or e.tiles_v]
    print(f"  {len(multi)} images carry more than one filename (the rename twins)")
    print(f"  {len(both)} tile on BOTH axes, {len(either)} on at least one")
    print(f"  {len(index) - len(either)} tile on neither — signs, decals, one-off panels")
    print("  (whether a tiling image is MATERIAL or SIGNAGE is a human call —")
    print("   see the note at the top of this file for the three metrics that failed)")

    print("\nSeam-score distribution (lower = tiles more cleanly):")
    for lo, hi in [(0, 0.05), (0.05, 0.10), (0.10, 0.20), (0.20, 0.40), (0.40, 1.01)]:
        n = sum(1 for e in index.values() if lo <= min(e.seam_h, e.seam_v) < hi)
        print(f"  {lo:.2f}-{hi:.2f}  {n:4d}  {'#' * (n // 10)}")
    print(f"\n  {path.relative_to(REPO)}")


def cmd_sheets() -> None:
    """Contact sheet per source directory: 8x nearest upscale, labelled."""
    index = load_index()
    sheets = OUT_DIR / "sheets"
    sheets.mkdir(parents=True, exist_ok=True)

    CELL, PAD, LABEL, COLS, SCALE = 96, 8, 26, 10, 8
    try:
        font = ImageFont.load_default(11)
    except TypeError:  # older Pillow
        font = ImageFont.load_default()

    by_level: dict[str, list[TexEntry]] = defaultdict(list)
    for e in index.values():
        for lv in e.levels:
            by_level[lv].append(e)

    for level, entries in sorted(by_level.items()):
        # Tiling first, then least-flat first: this ordering puts the most
        # material-looking images at the top even though no binary test does.
        entries.sort(key=lambda e: (not e.tiles_both, e.flat_fraction, e.canonical))
        rows = (len(entries) + COLS - 1) // COLS
        W = COLS * (CELL + PAD) + PAD
        H = rows * (CELL + LABEL + PAD) + PAD + 30
        sheet = Image.new("RGB", (W, H), (24, 24, 28))
        draw = ImageDraw.Draw(sheet)
        draw.text(
            (PAD, 8),
            f"{level}  —  {len(entries)} distinct textures  "
            f"(tiling first, then least-flat; * = tiles both axes, h/v = one axis)",
            fill=(230, 200, 120),
            font=font,
        )

        for i, e in enumerate(entries):
            cx = PAD + (i % COLS) * (CELL + PAD)
            cy = 30 + PAD + (i // COLS) * (CELL + LABEL + PAD)
            src = find_file(e)
            if src is not None:
                with Image.open(src) as img:
                    img = img.convert("RGB")
                    up = img.resize(
                        (min(CELL, img.width * SCALE), min(CELL, img.height * SCALE)),
                        Image.NEAREST,
                    )
                sheet.paste(up, (cx + (CELL - up.width) // 2, cy + (CELL - up.height) // 2))
            if e.tiles_both:
                mark = "*"
            else:
                mark = "h" if e.tiles_h else ("v" if e.tiles_v else "")
            label = e.canonical.replace("tempImgEd", "~")
            draw.text((cx, cy + CELL + 2), f"{label}{mark}", fill=(200, 200, 205), font=font)
            draw.text(
                (cx, cy + CELL + 13), f"{e.width}x{e.height}", fill=(120, 120, 130), font=font
            )

        out = sheets / f"{level}.png"
        sheet.save(out)
        print(f"  {out.relative_to(REPO)}  ({len(entries)} textures)")


def cmd_themes() -> None:
    """Render the extracted theme library as browsable sheets.

    One row per candidate theme showing its four zone textures side by side, so
    the output of `obj_themes.py extract` can actually be judged by eye. Sorted by
    total geometry, which puts the themes GoldenEye leaned on hardest first.
    """
    index = load_index()
    by_name = {}
    for e in index.values():
        for n in e.names:
            by_name[n] = e

    lib_path = OUT_DIR / "theme_library.json"
    if not lib_path.exists():
        raise SystemExit(f"{lib_path} not found — run `obj_themes.py extract` first")
    themes = json.loads(lib_path.read_text(encoding="utf-8"))

    sheets = OUT_DIR / "sheets"
    sheets.mkdir(parents=True, exist_ok=True)
    SW, PAD, ROW, PER_PAGE = 72, 10, 84, 24
    ZONES = ("floor", "ceiling", "lower_wall", "upper_wall")
    try:
        font = ImageFont.load_default(11)
        bold = ImageFont.load_default(13)
    except TypeError:
        font = bold = ImageFont.load_default()

    pages = (len(themes) + PER_PAGE - 1) // PER_PAGE
    for page in range(pages):
        chunk = themes[page * PER_PAGE : (page + 1) * PER_PAGE]
        W = PAD + 4 * (SW + PAD) + 430
        H = 40 + len(chunk) * ROW + PAD
        img = Image.new("RGB", (W, H), (24, 24, 28))
        draw = ImageDraw.Draw(img)
        draw.text(
            (PAD, 10),
            f"Candidate themes {page * PER_PAGE + 1}-{page * PER_PAGE + len(chunk)}"
            f" of {len(themes)}   —   floor | ceiling | lower wall | upper wall",
            fill=(230, 200, 120),
            font=bold,
        )
        for i, t in enumerate(chunk):
            y = 40 + i * ROW
            for zi, zone in enumerate(ZONES):
                x = PAD + zi * (SW + PAD)
                name = t["textures"].get(zone)
                if not name or name not in by_name:
                    draw.rectangle([x, y, x + SW, y + SW], outline=(70, 70, 78))
                    draw.text((x + 22, y + SW // 2 - 6), "--", fill=(90, 90, 98), font=font)
                    continue
                src = find_file(by_name[name])
                if src is None:
                    continue
                with Image.open(src) as im:
                    im = im.convert("RGB").resize((SW, SW), Image.NEAREST)
                img.paste(im, (x, y))
                rep = t["zones"].get(str(zi), {}).get("repeat")
                if rep is not None:
                    draw.text((x, y + SW + 1), f"r={rep}", fill=(130, 130, 140), font=font)

            tx = PAD + 4 * (SW + PAD)
            draw.text(
                (tx, y + 4),
                f"{t['representative']['level']}  {t['representative']['room']}",
                fill=(225, 225, 230),
                font=bold,
            )
            draw.text(
                (tx, y + 22),
                f"{t['rooms']} rooms, {t['faces']} faces",
                fill=(150, 150, 160),
                font=font,
            )
            draw.text((tx, y + 36), "  ".join(t["levels"][:3]), fill=(120, 120, 132), font=font)
            names = "  ".join(
                (t["textures"][z] or "--").replace("tempImgEd", "~") for z in ZONES
            )
            draw.text((tx, y + 52), names, fill=(110, 110, 122), font=font)

        out = sheets / f"themes_p{page + 1:02d}.png"
        img.save(out)
    print(f"  {pages} theme sheets -> {(sheets / 'themes_p01.png').relative_to(REPO)} ...")


def find_file(entry: TexEntry) -> Path | None:
    """Locate any file on disk carrying this entry's content."""
    for _label, d in source_dirs():
        for n in entry.names:
            p = d / f"{n}.bmp"
            if p.exists():
                return p
    return None


def main() -> None:
    cmds = {"index": cmd_index, "sheets": cmd_sheets, "themes": cmd_themes}
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        print(__doc__)
        print("subcommands: " + ", ".join(cmds))
        raise SystemExit(2)
    cmds[sys.argv[1]]()


if __name__ == "__main__":
    main()
