# texture-themes

Recovers texture themes for the native game from the ripped GoldenEye level files,
and makes the ~1000-image texture library browsable.

Stages 2-4 of the plan in `DESIGN_TEXTURE_THEMES.md`. The extraction and triage here
never touch the game; `adopt.py` is the one script that writes into `native/assets/`.

## Run it

```
python tools/texture-themes/texlib.py index        # hash + tag every BMP
python tools/texture-themes/obj_themes.py extract  # OBJ+MTL -> candidate themes
python tools/texture-themes/obj_themes.py validate # check against shipped themes
python tools/texture-themes/texlib.py sheets       # per-level contact sheets
python tools/texture-themes/texlib.py themes       # browsable theme sheets
```

## The curation loop

`adopt.py` puts themes into the game and takes them back out again:

```
python tools/texture-themes/adopt.py --bulk --dry-run   # preview the full set
python tools/texture-themes/adopt.py --bulk             # load ~380 themes for review
#   ... in game: BUILD -> O -> cycle to TEXTURES -> mark themes with ✔ / ✕ ...
python tools/texture-themes/adopt.py --prune            # cut to the kept set
```

The bulk set is *meant* to be over-broad. Judging which themes look good is a human
job — no statistic in this directory can do it (see below) — so the game's TEXTURES
panel exists to make walking ~390 of them practical. It writes verdicts to
`native/assets/theme_review.json`, keyed by theme name, and `--prune` reads them
back, re-keys the survivors and backs up the previous manifest.

`--prune` never removes the first ten themes whatever their verdict: those are the
positions pre-v4 level files address by index.

`obj_themes.py calibrate` shows the scale derivation and its sanity check.
Extraction reads `out/texture_index.json`, so run `index` first. Total runtime is a
few minutes; `out/` is regenerable and need not be committed.

## Output

| File | What |
|---|---|
| `out/texture_index.json` | 993 distinct images (from 1870 files), keyed by content hash: every filename that points at it, which levels use it, size, seam scores, luma, dominant colour |
| `out/room_candidates.json` | 1918 per-room theme candidates — one per GoldenEye room group |
| `out/theme_library.json` | those collapsed to 662 distinct themes, with provenance and derived `repeat` per zone |
| `out/sheets/<level>.png` | contact sheet per level, 8× nearest upscale, labelled |
| `out/sheets/themes_p*.png` | 28 sheets showing each candidate theme's four zone textures side by side |

## Results

- **1918 room candidates → 662 distinct themes.** 561 have every derived `repeat`
  in a plausible band; 101 are flagged `repeats_in_band: false` and need their
  scale set by eye. The texture *choices* in those are still good — only the scale
  is suspect.
- **5 of the 9 shipped hand-authored themes are reproduced exactly on all four
  zones**, and the other four match 2-3 of 4 with the best candidate in the right
  level. That's the acceptance test: `obj_themes.py validate`.
- **993 distinct images** from 1870 files. 675 tile on at least one axis, 309 on
  both, 318 on neither.

## Things that are easy to get wrong

**Compare textures by content hash, never by filename.** Every human-renamed
texture in `public/textures/` has exactly one temp-named twin — `grey_tile_floor`
== `tempImgEd02B7`, `white_tile` == `tempImgEd02CE`, and 14 more. A name-based
check of the extractor against the shipped themes scores 2/9; the same check by
hash scores 5/9. The renaming, not the extractor, was the difference.

**Every `vn` in every OBJ is `vn 0 0 0`.** Face normals must be computed by cross
product; trusting the file's normals silently misclassifies every surface.

**Wall faces must be geometrically split, not bucketed.** GoldenEye walls are
typically two full-height triangles. Filing each face into lower/upper by its
lowest vertex puts every wall in "lower" and leaves the upper band empty — which is
exactly what the first run produced. `zone_areas` clips each triangle at the split
height and contributes area to both bands, mirroring `emit_wall_split` in the
engine. This is also what surfaces the rooms where GE genuinely *did* stratify a
wall into two textures.

**The hand-tuned repeats are not a ground truth to fit.** They imply calibration
constants spanning 14.4 – 173.5 (a 12× spread) because they were eyeballed against
*our* authored room sizes, not GoldenEye's. The scale is derived from measured
geometry instead — GoldenEye's 353-unit storey against our 16 WT default room — and
the hand-tuned values are used only as a coarse bracket to check it against.

**There is no cheap statistic for "material vs signage".** Three were tried and all
failed on real data (seam score, dominant-colour share, border uniformity — see the
note at the top of `texlib.py` for how each failed). The sheets exist precisely
because that judgement needs a human eye. Seam scores *do* reliably answer the
narrower question of whether an image tiles at all.

## Known limitations

- Zones 5/6/7 (stair riser, doorframe floor, brace) cannot be extracted —
  GoldenEye had no equivalent surfaces. They must be chosen during curation.
- Zone 3 (upper wall) is the least trustworthy of the four: the lower/upper split
  is our own invention, so any "upper wall" measurement is taken over a band we
  defined after the fact. Its derived `repeat` sits ~1.3× below the hand-tuned
  bracket for that reason.
- `#vcolor` per-vertex baked lighting is parsed past, not used. It means the
  original game modulated these textures with vertex colour, so extracted themes
  read slightly brighter in our engine than in GoldenEye.
