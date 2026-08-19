"""Adopt extracted candidate themes into the game.

Stage 3 of `DESIGN_TEXTURE_THEMES.md`: takes chosen entries out of
`out/theme_library.json`, copies the BMPs they need into
`native/assets/textures/`, and appends them to `native/assets/themes.json`.

Two rules this script enforces, both of which exist because level files persist a
theme by name (level format v4):

1. **Append only.** Pre-v4 level files read a bare integer `scheme` as a position
   in the manifest, so the existing entries must keep their order or old levels
   silently retexture. New themes go on the end.
2. **Never overwrite a texture with different bytes.** A name collision between a
   per-level BMP and the flat library would change how existing themes render.
   The script aborts rather than clobber.

Zones 5/6/7 (stair riser, doorframe floor, brace) cannot be extracted — GoldenEye
had no equivalent surfaces — so they are filled from the same defaults every
shipped `facility_*` theme uses.

Usage:
    python tools/texture-themes/adopt.py --bulk --dry-run   # the full ~380-theme set
    python tools/texture-themes/adopt.py --bulk
    python tools/texture-themes/adopt.py                    # just the 8 curated picks
    python tools/texture-themes/adopt.py --prune            # cut to what you kept in-game

The intended loop is: `--bulk` to load the whole extracted library, review it in the
game's TEXTURES panel (O, then cycle to TEXTURES) marking themes Keep or Cut, then
`--prune` to cut the manifest down to the keepers.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import texlib  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
THEMES_JSON = REPO / "native" / "assets" / "themes.json"
TEXTURES_DIR = REPO / "native" / "assets" / "textures"
LIBRARY = Path(__file__).resolve().parent / "out" / "theme_library.json"

ZONE_KEYS = ("floor", "ceiling", "lower_wall", "upper_wall")

# Zones GoldenEye has no equivalent for, matching the shipped facility_* themes.
STAIR_ZONE = {"texture": "stair_gradient", "repeat": 1.0}
DOORFRAME_FLOOR_ZONE = {"texture": "floor_doorframe", "repeat": 0.35}

# The curated set: (source level, theme name, label, group, number key).
#
# Chosen by eye from `out/sheets/themes_p*.png`, taking the highest-geometry
# candidate per level family that has all four zones and sane repeats. Two picks
# were rejected on inspection and are recorded here so the reasoning survives:
#
#   11 - Archives — its upper wall resolved to a black oval decal, not a wall.
#   07 - Frigate  — floor and ceiling both resolved to structural detail/decals.
#   16 - Control  — ceiling is high-contrast pink noise, reads as broken.
#   15 - Jungle   — coherent, but duplicates Caverns' rock almost exactly.
#
# That is the material-vs-signage judgement no statistic could make (see the note
# at the top of texlib.py) landing on a real case.
ADOPT = [
    ("17 - Caverns", "caverns_rock", "Caverns Rock", "Caverns", "2"),
    ("19 - Aztec", "aztec_stone", "Aztec Stone", "Aztec", "3"),
    ("20 - Egyptian", "egyptian_sandstone", "Egyptian Sandstone", "Egyptian", "4"),
    ("06 - Silo", "silo_industrial", "Silo Industrial", "Silo", "5"),
    ("13 - Depot", "depot_warehouse", "Depot Warehouse", "Depot", "6"),
    ("12 - Streets", "streets_concrete", "Streets Concrete", "Streets", "7"),
    ("18 - Cradle", "cradle_plate", "Cradle Plate", "Cradle", "8"),
    ("01 - Dam", "dam_concrete", "Dam Concrete", "Dam", "9"),
]


# Entries 0..9 of themes.json are the original hand-authored ten. Pre-v4 level
# files read a bare integer `scheme` as a position in the list, so those ten must
# keep their order forever. Everything from index 10 on was added by this script
# and no legacy file can reference it, so bulk mode is free to regenerate that tail.
FROZEN_PREFIX = 10


def slug(level: str) -> tuple[str, str]:
    """'17 - Caverns' -> ('caverns', 'Caverns')."""
    name = level.split("-", 1)[1].strip()
    return name.lower().replace(" ", "_"), name


def fill_zones(textures: dict) -> dict | None:
    """Complete a theme's four zones with sensible fallbacks, or None if hopeless.

    A theme with an undefined zone renders that geometry *invisible* (the engine
    skips undefined zones), so every adopted theme must define 0..3. Floor and
    lower wall have no substitute. The other two do:

    * missing upper wall -> reuse the lower wall. GoldenEye walls are one
      continuous surface; the split into two bands is our own invention, so an
      unstratified wall is the normal case, not a gap.
    * missing ceiling -> reuse the upper wall. Happens in GE's outdoor rooms,
      which have no ceiling geometry at all.
    """
    tx = dict(textures)
    if not tx.get("floor") or not tx.get("lower_wall"):
        return None
    if not tx.get("upper_wall"):
        tx["upper_wall"] = tx["lower_wall"]
    if not tx.get("ceiling"):
        tx["ceiling"] = tx["upper_wall"]
    return tx


def bulk_selection() -> list[dict]:
    """Every usable theme, deduped after fallback-fill, named per level family.

    This is the "big list" to prune from rather than a curated set: judging 382
    themes is a job for the in-game picker, not for a heuristic.
    """
    lib = json.loads(LIBRARY.read_text(encoding="utf-8"))
    seen: set[tuple] = set()
    per_family: dict[str, int] = {}
    out: list[dict] = []

    for t in sorted(lib, key=lambda t: -t["faces"]):
        if not t["repeats_in_band"]:
            continue
        tx = fill_zones(t["textures"])
        if tx is None:
            continue
        key = tuple(tx[z] for z in ZONE_KEYS)
        if key in seen:
            continue  # the fill collapses some themes onto each other
        seen.add(key)

        fam_slug, fam_label = slug(t["representative"]["level"])
        per_family[fam_slug] = per_family.get(fam_slug, 0) + 1
        n = per_family[fam_slug]

        zones = {}
        for zi, zone in enumerate(ZONE_KEYS):
            src_zone = t["zones"].get(str(zi))
            # A filled-in zone inherits the repeat of the zone it borrowed from.
            if src_zone is None:
                borrowed = "lower_wall" if zone == "upper_wall" else "upper_wall"
                bi = ZONE_KEYS.index(borrowed)
                src_zone = t["zones"].get(str(bi)) or {"repeat": 1.0}
            zones[str(zi)] = {"texture": tx[zone], "repeat": round(src_zone["repeat"], 4)}
        zones["5"] = dict(STAIR_ZONE)
        zones["6"] = dict(DOORFRAME_FLOOR_ZONE)

        out.append(
            {
                "name": f"{fam_slug}_{n:02d}",
                "label": f"{fam_label} {n:02d}",
                "group": fam_label,
                "_source": f"{t['representative']['level']} {t['representative']['room']} "
                f"({t['rooms']} rooms, {t['faces']} faces)",
                "zones": zones,
            }
        )
    return out


def best_per_level() -> dict[str, dict]:
    """Highest-geometry candidate per level with all four zones and sane repeats."""
    lib = json.loads(LIBRARY.read_text(encoding="utf-8"))
    out: dict[str, dict] = {}
    for t in sorted(lib, key=lambda t: -t["faces"]):
        if not t["repeats_in_band"] or not all(t["textures"][k] for k in ZONE_KEYS):
            continue
        out.setdefault(t["representative"]["level"], t)
    return out


def copy_textures(needed: set[str], by_name: dict, dry: bool) -> int:
    """Copy every named BMP into native/assets/textures/, refusing to clobber."""
    copied = 0
    for tex in sorted(needed):
        entry = by_name.get(tex)
        if entry is None:
            raise SystemExit(f"{tex}: not in the texture index")
        src = texlib.find_file(entry)
        if src is None:
            raise SystemExit(f"{tex}: no file on disk")
        dest = TEXTURES_DIR / f"{tex}.bmp"
        if dest.exists():
            if hashlib.sha1(dest.read_bytes()).hexdigest() != hashlib.sha1(
                src.read_bytes()
            ).hexdigest():
                raise SystemExit(
                    f"ABORT: {tex}.bmp already exists with different bytes — "
                    "copying would change how existing themes render"
                )
            continue
        if not dry:
            shutil.copy2(src, dest)
        copied += 1
    return copied


def main_bulk(dry: bool) -> None:
    """Replace everything after the frozen prefix with the full usable theme set."""
    index = texlib.load_index()
    by_name = {n: e for e in index.values() for n in e.names}
    manifest = json.loads(THEMES_JSON.read_text(encoding="utf-8"))

    selection = bulk_selection()
    needed = {z["texture"] for s in selection for z in s["zones"].values()}
    copied = copy_textures(needed, by_name, dry)

    # Give the highest-geometry theme of each family a number key, most-used
    # families first, so the quick keys still reach a good spread without the panel.
    keyed = []
    for s in selection:
        if s["group"] not in keyed:
            keyed.append(s["group"])
    for s in selection:
        s.pop("key", None)
    for digit, group in zip("23456789", keyed):
        first = next(s for s in selection if s["group"] == group)
        first["key"] = digit

    frozen = manifest["schemes"][:FROZEN_PREFIX]
    for s in frozen:
        # Only key '1' survives on the originals; the rest are reachable via the panel.
        if s.get("key") not in (None, "1"):
            s.pop("key", None)
    manifest["schemes"] = frozen + selection

    families = {}
    for s in selection:
        families[s["group"]] = families.get(s["group"], 0) + 1
    print(f"Bulk adopt: {len(selection)} themes across {len(families)} level families")
    for fam, n in sorted(families.items()):
        key = next((s["key"] for s in selection if s["group"] == fam and s.get("key")), "-")
        print(f"  [{key}] {fam:14s} {n:3d}")
    print(f"\n  {copied} textures {'would be' if dry else ''} copied")
    print(f"  themes.json: {len(frozen)} frozen + {len(selection)} generated")
    if dry:
        print("\n--dry-run: nothing written")
        return
    THEMES_JSON.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"  wrote {THEMES_JSON.relative_to(REPO)}")


def main_prune(dry: bool) -> None:
    """Cut themes.json down to the themes marked Keep in the picker panel.

    The bulk set is deliberately over-broad — generated to be pruned. The TEXTURES
    tab in-game writes verdicts to `native/assets/theme_review.json`; this reads
    them back and drops everything not kept.

    The frozen prefix survives regardless of verdict: those ten are what pre-v4
    level files address by index, so removing one would retexture old levels.
    """
    review_path = REPO / "native" / "assets" / "theme_review.json"
    if not review_path.exists():
        raise SystemExit(
            f"{review_path.relative_to(REPO)} not found — review some themes in the "
            "TEXTURES panel first (O, then cycle to TEXTURES)"
        )
    verdicts = json.loads(review_path.read_text(encoding="utf-8")).get("verdicts", {})
    manifest = json.loads(THEMES_JSON.read_text(encoding="utf-8"))

    frozen = manifest["schemes"][:FROZEN_PREFIX]
    tail = manifest["schemes"][FROZEN_PREFIX:]
    kept = [s for s in tail if verdicts.get(s["name"]) == "keep"]
    cut = [s for s in tail if verdicts.get(s["name"]) == "reject"]
    unreviewed = [s for s in tail if s["name"] not in verdicts]

    print(f"themes.json: {len(frozen)} frozen + {len(tail)} reviewable")
    print(f"  keep       {len(kept):4d}")
    print(f"  reject     {len(cut):4d}")
    print(f"  unreviewed {len(unreviewed):4d}  (dropped — mark them Keep to retain)")

    if not kept:
        raise SystemExit("nothing marked Keep; refusing to prune to an empty tail")

    # Re-key: one number key per group, best (earliest, i.e. most geometry) first.
    groups: list[str] = []
    for s in kept:
        if s["group"] not in groups:
            groups.append(s["group"])
    for s in kept:
        s.pop("key", None)
    for digit, group in zip("23456789", groups):
        next(s for s in kept if s["group"] == group)["key"] = digit

    if dry:
        print("\n--dry-run: nothing written")
        return
    backup = THEMES_JSON.with_suffix(".json.prebackup")
    backup.write_text(THEMES_JSON.read_text(encoding="utf-8"), encoding="utf-8")
    manifest["schemes"] = frozen + kept
    THEMES_JSON.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"\n  wrote {THEMES_JSON.relative_to(REPO)} ({len(frozen) + len(kept)} themes)")
    print(f"  previous manifest saved to {backup.name}")
    print(
        "  NOTE: any v4 level already saved with a pruned theme name falls back to\n"
        "  the default theme (with a warning) rather than failing to load."
    )


def main() -> None:
    dry = "--dry-run" in sys.argv
    if "--prune" in sys.argv:
        return main_prune(dry)
    if not LIBRARY.exists():
        raise SystemExit(f"{LIBRARY} not found — run `obj_themes.py extract` first")
    if "--bulk" in sys.argv:
        return main_bulk(dry)

    index = texlib.load_index()
    by_name = {n: e for e in index.values() for n in e.names}
    best = best_per_level()

    manifest = json.loads(THEMES_JSON.read_text(encoding="utf-8"))
    existing = {s["name"] for s in manifest["schemes"]}

    # --- gather textures, refusing to clobber -------------------------------
    to_copy: list[tuple[str, Path]] = []
    for level, name, _label, _group, _key in ADOPT:
        if level not in best:
            raise SystemExit(f"no usable candidate for {level}")
        for zone in ZONE_KEYS:
            tex = best[level]["textures"][zone]
            src = texlib.find_file(by_name[tex])
            if src is None:
                raise SystemExit(f"{tex}: no file on disk")
            dest = TEXTURES_DIR / f"{tex}.bmp"
            if dest.exists():
                if hashlib.sha1(dest.read_bytes()).hexdigest() != hashlib.sha1(
                    src.read_bytes()
                ).hexdigest():
                    raise SystemExit(
                        f"ABORT: {tex}.bmp already exists with different bytes — "
                        "copying would change how existing themes render"
                    )
                continue
            if all(tex != n for n, _ in to_copy):
                to_copy.append((tex, src))

    # --- build the new theme entries ----------------------------------------
    added = []
    for level, name, label, group, key in ADOPT:
        if name in existing:
            print(f"  skip {name} (already in themes.json)")
            continue
        t = best[level]
        zones = {}
        for zi, zone in enumerate(ZONE_KEYS):
            zones[str(zi)] = {
                "texture": t["textures"][zone],
                "repeat": round(t["zones"][str(zi)]["repeat"], 4),
            }
        zones["5"] = dict(STAIR_ZONE)
        zones["6"] = dict(DOORFRAME_FLOOR_ZONE)
        added.append(
            {
                "name": name,
                "label": label,
                "group": group,
                "key": key,
                "_source": f"{level} {t['representative']['room']} "
                f"({t['rooms']} rooms, {t['faces']} faces)",
                "zones": zones,
            }
        )

    # A number key can only select one theme, so anything the new set claims has to
    # give its key up. The theme itself stays — levels reference it by name, and
    # rendering never needs a key.
    claimed = {a["key"] for a in added}
    unbound = [s["name"] for s in manifest["schemes"] if s.get("key") in claimed]

    print(f"Adopting {len(added)} themes, copying {len(to_copy)} textures")
    for a in added:
        print(f"  [{a['key']}] {a['name']:22s} <- {a['_source']}")
    if unbound:
        print(f"\n  keys reassigned away from: {', '.join(unbound)}")
        print("  (those themes remain in themes.json; existing levels still render)")

    if dry:
        print("\n--dry-run: nothing written")
        return

    for tex, src in to_copy:
        shutil.copy2(src, TEXTURES_DIR / f"{tex}.bmp")
    for s in manifest["schemes"]:
        if s.get("key") in claimed:
            s.pop("key", None)
    manifest["schemes"].extend(added)  # APPEND — see rule 1 in the module docstring
    THEMES_JSON.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"\n  wrote {THEMES_JSON.relative_to(REPO)} ({len(manifest['schemes'])} themes)")
    print(f"  copied {len(to_copy)} BMPs into {TEXTURES_DIR.relative_to(REPO)}")


if __name__ == "__main__":
    main()
