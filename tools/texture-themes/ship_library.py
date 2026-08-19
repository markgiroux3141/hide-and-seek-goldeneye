"""Ship the whole GoldenEye texture library into the game, with provenance.

The theme editor lets an author pick *any* texture for any zone, so every distinct
image has to be resident in `native/assets/textures/` — not just the ~330 the
generated themes happen to reference. And a flat list of 993 files named
`tempImgEd02B7` is unusable, so this also emits a manifest recording, per texture,
which GoldenEye levels it appears in. The picker groups by that.

Writes:
  native/assets/textures/*.bmp        every distinct image, canonical name
  native/assets/texture_index.json    name -> { levels, size, tiles }

Canonical naming: a texture with a human-given name in `public/textures/` keeps it
(`grey_tile_floor`, not `tempImgEd02B7`) — the two are the same bytes, and the
readable name is the better handle. Every alias is recorded so a theme referencing
either name resolves.

Usage:
    python tools/texture-themes/ship_library.py --dry-run
    python tools/texture-themes/ship_library.py
"""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import texlib  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
TEXTURES_DIR = REPO / "native" / "assets" / "textures"
INDEX_JSON = REPO / "native" / "assets" / "texture_index.json"

# Source-directory label used for the flat pile, which has no single level of origin.
FLAT_LABEL = "flat"


def main() -> None:
    dry = "--dry-run" in sys.argv
    index = texlib.load_index()

    # Which themes are already referenced? Those names must keep resolving, so if a
    # theme names an alias we make sure that exact filename stays on disk too.
    manifest_path = REPO / "native" / "assets" / "themes.json"
    referenced: set[str] = set()
    if manifest_path.exists():
        m = json.loads(manifest_path.read_text(encoding="utf-8"))
        referenced = {
            z["texture"] for s in m["schemes"] for z in s["zones"].values() if z.get("texture")
        }

    out: dict[str, dict] = {}
    copies: list[tuple[Path, str]] = []
    collisions: list[str] = []

    for entry in index.values():
        src = texlib.find_file(entry)
        if src is None:
            continue
        # Ship under the canonical name, plus any alias a theme already references
        # (so no existing theme breaks).
        names = {entry.canonical} | (set(entry.names) & referenced)
        levels = sorted(lv for lv in entry.levels if lv != FLAT_LABEL)
        for name in names:
            dest = TEXTURES_DIR / f"{name}.bmp"
            if dest.exists():
                if hashlib.sha1(dest.read_bytes()).hexdigest() != entry.hash:
                    collisions.append(name)
                    continue
            else:
                copies.append((src, name))
            out[name] = {
                "levels": levels or [FLAT_LABEL],
                "w": entry.width,
                "h": entry.height,
                # Whether it tiles on both axes. Advisory only — see the note in
                # texlib.py about why "material vs signage" is not derivable.
                "tiles": bool(entry.tiles_both),
                "aliases": sorted(n for n in entry.names if n != name),
            }

    if collisions:
        raise SystemExit(
            f"ABORT: {len(collisions)} name(s) already on disk with different bytes: "
            f"{collisions[:8]} — shipping would change existing themes"
        )

    by_level: dict[str, int] = defaultdict(int)
    for meta in out.values():
        for lv in meta["levels"]:
            by_level[lv] += 1

    print(f"{len(out)} textures to ship ({len(copies)} new), grouped by source level:")
    for lv, n in sorted(by_level.items()):
        print(f"  {lv:20s} {n:4d}")
    missing = referenced - set(out)
    if missing:
        print(f"\n  WARNING: {len(missing)} referenced textures not in the index: {sorted(missing)[:8]}")

    if dry:
        print("\n--dry-run: nothing written")
        return
    TEXTURES_DIR.mkdir(parents=True, exist_ok=True)
    for src, name in copies:
        shutil.copy2(src, TEXTURES_DIR / f"{name}.bmp")
    INDEX_JSON.write_text(json.dumps(out, indent=1, sort_keys=True), encoding="utf-8")
    print(f"\n  copied {len(copies)} BMPs into {TEXTURES_DIR.relative_to(REPO)}")
    print(f"  wrote {INDEX_JSON.relative_to(REPO)} ({len(out)} entries)")


if __name__ == "__main__":
    main()
