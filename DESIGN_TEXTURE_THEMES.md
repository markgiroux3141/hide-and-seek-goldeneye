# Texture Themes

Goal: many good-looking texture themes (floor / ceiling / walls / stairs / trim), authored
through a GUI instead of hand-transcribed into Rust.

Status: **stages 1-4 built, plus the custom theme editor (stage 5).** 392 library themes +
a full 1016-texture browsable library + an in-game editor that builds themes from any
texture with live UV scale/offset, previews them in a stock room, saves presets, and binds
per-level quick keys. 525 tests green, release built. Remaining work is the *curation pass
itself* — a human walking the list.

---

## What already exists

The texture *system* is fully ported. The gap is content and authoring, not plumbing.

> Recon baseline, captured **before** stage 1. Line numbers and the `Scheme` shape below
> describe the pre-stage-1 code; see "Plumbing changes — DONE" for what changed. The zone
> taxonomy, UV generation and retexture backend are untouched and still current.

- `Scheme { name, label, key, zones: [Option<ZoneDef>; 8] }` and the 10 shipped schemes —
  `native/crates/engine/src/render/textures.rs:39-209`
- `ZoneDef { texture, repeat, color }` — `textures.rs:24-29`
- Zone taxonomy (documented `textures.rs:5-9`, `uv_zones.rs:21-23`):

  | zone | role |
  |---|---|
  | 0 | floor |
  | 1 | ceiling |
  | 2 | lower wall (below `WALL_SPLIT_V`, anchored to the brush's `floor_y`) |
  | 3 | upper wall |
  | 4 | legacy tunnel flat-colour — **never emitted**, always `None` |
  | 5 | stair riser / door + hole frame sides and lintel |
  | 6 | doorframe floor |
  | 7 | structural brace (reused as the railing slot in `simple_blue`) |

- Zone classification is automatic from triangle normals, with walls geometrically clipped at
  `WALL_SPLIT_V` — `uv_zones.rs:248-355` (`classify_soup`) and `uv_zones.rs:400+`
  (`emit_wall_split`).
- UVs are world-space planar projection in tile units, so adjacent brushes seam automatically —
  `uv_zones.rs:106-123` (`vertex_uv`).
- Materials are one bind group per `(scheme, zone)`: `materials: Vec<[Option<BindGroup>; 8]>` —
  `renderer.rs:369`, built in `build_materials()` `renderer.rs:3695-3785`.
- **`repeat` is a shader uniform, not baked into the mesh** (`MaterialUniform.params.x`,
  `renderer.rs:56-62`). Switching a room's scheme is a bind-group swap with **no re-bake**.
- Retexture backend: `World::set_scheme_at_crosshair()` — `game/src/world/editing.rs:18-48`.
  Flood-fills the room via `find_room_brushes` (bounded by door/hole frames), sets `b.scheme` on
  every brush reached, then `rebuild_affected_regions`.

### Why authoring *was* painful (all three fixed in stage 1)

1. **Recompile per texture.** `texture_bytes()` (`textures.rs:281-322`) is a hand-written 27-arm
   `match` of `include_bytes!`. New texture = copy the BMP into the flat folder, add an arm, rebuild.
2. **The library is unbrowsable.** 222 of the 238 files in `public/textures/` are named
   `tempImgEd02B7.bmp` — the ripper's temp filenames.
3. **Coverage is tiny.** Only 26 of 238 flat BMPs are referenced by any scheme; only **18** are
   reachable from any shipped level. Schemes exist for Facility (×7), Archives, Bunker, Simple —
   nothing for Caverns / Jungle / Aztec / Statue / Streets / Depot / Control, despite the BMPs
   being present.

---

## Asset inventory (all already in this repo)

| Path | Contents |
|---|---|
| `public/textures/` | 238 BMPs, flat, mostly `tempImgEd*` names |
| `public/transparent_textures/` | 3 BMPs (`railing`, `chainlink`, `toilet_seat`) |
| `public/existing goldeneye levels/<NN - Level>/` | 21 folders; `LevelIndices.obj` + `.mtl` + that level's BMPs. 1629 BMPs, 1010 distinct |
| `public/textureSchemes.json` | the JS scheme spec `textures.rs` was transcribed from |

Sibling repos were checked and are **not** needed: `3DS FPS` has only 18 unrelated PBR files and its
level JSON records no texture data; `GoldenEye Level Editor` holds a byte-identical copy of
`public/textures/`.

All BMPs are 24/32bpp, N64-era sizes — 126 are 32×32, most of the rest 64×64 or 64×32. Sampled
Nearest with `mip_level_count: 1` (`renderer.rs:3725`) for the N64 look.

### `public/textureSchemes.json` has a field the Rust port dropped

Each scheme carries `group` (`"Facility"` ×7, `"Archives"`, `"Bunker"`, `"Simple"`). `Scheme` has no
such field. It is the natural grouping axis for a picker UI and should be added back.

`materials.js` also reads `offsetX`, `offsetY`, `rotation` per zone. No shipped scheme uses them and
the Rust port has no equivalent. Out of scope unless extraction turns up a need.

---

## The OBJ+MTL extraction (the main idea)

Don't hand-author themes. The 21 `LevelIndices.obj` files let us recover GoldenEye's **original**
texture assignments, per room.

`LevelIndices.mtl` is a per-level manifest: `newmtl m0` … `map_Kd tempImgEd00BB.bmp`. The OBJ
references those via `usemtl` runs over triangles that carry real UVs.

### Verified facts (measured, not assumed)

- **The OBJ is already partitioned into rooms.** `g` groups are named `primary_Room01`,
  `secondary_Room08`, … — GoldenEye's own room segmentation, exported by the Setup Editor.
  **1968 room groups across the 21 levels.** No spatial clustering needed; this was going to be the
  riskiest part of the pipeline and it is simply free.
- **Vertex normals are useless.** Every `vn` in every file is `vn 0 0 0` (verified by dedup —
  exactly one distinct value across the file). Face normals **must** be computed from vertex cross
  products. Trusting `vn` would silently misclassify every zone.
- `vt` UVs are real, per-vertex, 3-component (third is `0.0`), and range outside `0..1`
  (e.g. `1.858398`, `-0.902344`) — exactly what is needed to derive `repeat`.
- Each vertex has a `#vcolor` comment: the original baked vertex lighting. Not consumed now, but it
  means source screenshots were colour-modulated, so extracted themes read slightly brighter
  in-engine than in the original game.
- Coordinates are raw GoldenEye units (order 1400, -692). Deriving `repeat` needs the
  GE-units → `WORLD_SCALE = 0.25` tile-unit mapping.
- Caverns sample: 81 rooms, 54 materials, 46 distinct BMPs, 10707 faces, 18241 verts.

### Pipeline

1. Parse `LevelIndices.mtl` → `material id -> BMP name`.
2. Parse the OBJ, tracking the current `g` (room) and `usemtl` (material) per triangle.
3. Compute each face normal by cross product. Classify into the existing zone taxonomy with the same
   dominant-axis rule as `uv_zones.rs:248-355`: `|ny|` largest → floor (0) or ceiling (1) by sign,
   else wall (2/3).
4. Per room, per zone, take the **modal** texture weighted by triangle area (not face count — a
   level is full of tiny slivers).
5. **Derive `repeat` from real data**: for each face, compare UV span to world-space span in tile
   units. Median across the zone. This removes the guesswork that makes hand-authoring tedious.
6. Emit a candidate theme per room: `{ level, room, group, zones: {0..3: {texture, repeat}} }`.
7. Dedupe candidates by their `(floor, ceiling, lower wall, upper wall)` tuple. 1968 rooms will
   collapse hard — most rooms in a level share a palette. The survivors are the theme library.
8. Fill zones 5/6/7 (stair, doorframe floor, brace) from the level's palette by heuristic, or
   inherit the existing Facility values. GoldenEye had no equivalent of our stair/doorframe zones,
   so these cannot be extracted and must be chosen.

### Triage tooling (supports both extraction review and the GUI)

Built: `tools/texture-themes/texlib.py`. See that directory's README for details.

- Contact sheets per level, 8× nearest upscale, labelled, deduped by content hash.
- Content-hash index: 1870 files collapse to **993 distinct images**.
- Seam scores per texture (does it tile): 675 tile on at least one axis, 309 on both.
- Theme sheets: 28 pages showing each candidate theme's four zone textures side by side.

**Correction to the plan above:** tileability does *not* separate material from signage —
only whether an image tiles at all. Three statistics were tried and each failed on real
data; the material/signage call needs a human eye, which is what the sheets are for. See
"What stage 2 measured" below.

---

## Plumbing changes — DONE (stage 1)

All three landed together; 511 tests green, no warnings, release built.

What the code looks like now:

- `native/assets/textures/` holds **all 241 BMPs** (238 flat + 3 transparent, no name
  collisions, 2.3 MB). `native/` no longer references `public/` at all.
- `native/assets/themes.json` is the registry, with the `group` field restored and the
  alpha-key list (`railing`) moved out of Rust into data.
- `textures.rs` loads both at runtime into a `OnceLock<Registry>`. `Scheme`/`ZoneDef` stay
  `Copy` with `&'static str` fields — the manifest strings are `Box::leak`ed, which is the
  honest representation of a process-lifetime registry and meant **zero** churn at the
  ~15 existing call sites beyond renaming.
- `SCHEMES` → `schemes()`; `DEFAULT_SCHEME`/`SIMPLE_SCHEME` (consts) → `default_scheme()` /
  `simple_scheme()` (resolved **by name** at load). `RAILING_ZONE` stays a const — it is
  part of the zone contract with `uv_zones`, not a position in a loaded list.
- `decode()` no longer returns `Option`: a missing BMP warns and yields **magenta**, which
  is the convention `load_crosshair` (`renderer.rs:3787`) already set — "a miss should be
  glaringly visible rather than an invisible surface that reads as a geometry bug".
  `try_decode()` is the Option-returning variant for tests.
- A missing/malformed `themes.json` **panics at startup** with the path and parse error.
  Deliberate asymmetry with the magenta fallback: one absent BMP degrades gracefully, but
  no registry at all means nothing can be textured, and booting into an untextured void
  would present as a rendering bug rather than an asset problem.
- Persistence is name-keyed (`"scheme": "archives_1"`), `LEVEL_FORMAT_VERSION = 4`.

### Two things worth recording

**No migration pass was needed.** `de_scheme` is an untagged enum accepting *either* a
name or a legacy integer, so v1-v3 files load with no version branching at all — the
version bump exists only to mark that writing is one-way. Verified: all 5 committed slot
files still load, and their legacy indices resolve correctly (slot1 → white_tile /
blue_brick / industrial, slot7 → the first five).

**Legacy indices constrain `themes.json` ordering.** A pre-v4 index is read as a position
in the manifest, so the first ten entries must keep their original order. Verified against
`git show HEAD:...textures.rs` — order preserved exactly. New themes must be **appended**
(or the old ten left in place); this is noted in the manifest's own comment block.

One test caught a real inconsistency: the old hard-coded tunnel colour was a 3dp literal
(`[0.545, 0.451, 0.333]`) while parsing `#8B7355` yields full precision. The fallback
constant is now the exact hex division so the two agree bit for bit.

### Original notes, for reference

#### Runtime texture loading

Replace the `include_bytes!` match with runtime loading from `native/assets/textures/`. Runtime file
IO is already established (`native/levels/*.json`). Turns theme iteration into "edit a file, reload"
instead of "edit Rust, rebuild".

The BMP decoder already exists — `textures.rs:243` (`decode`).

#### Schemes as data

Move `SCHEMES` out of `textures.rs` into `native/assets/themes.json` (shape: `textureSchemes.json`
plus `group`). A theme becomes authored content, which is what makes the GUI's save path meaningful.

#### Name-keyed persistence

Schemes are persisted as a **positional integer index** into `SCHEMES` — `Brush.scheme: usize`
(`csg_runtime.rs:155`), `StairDesc.scheme` (`csg_runtime.rs:332`), and on disk as `"scheme": 2`
in `native/levels/slot*.json`. Insert a theme mid-array and **every saved level silently
retextures.**

Migrate persistence to the scheme **name** string, bumping `LEVEL_FORMAT_VERSION` (currently 3 —
`persist.rs:41`) with an index→name map for the 10 existing schemes. Cheap now, with only 5 slot
files; progressively worse the longer it waits.

Note `Platform` (`structures.rs:73-83`) has no scheme field at all — platforms and stair runs are
hardcoded to `SIMPLE_SCHEME` at bake (`game/src/world/tools/platform.rs:657,661,683,697`). Giving
platforms authorable themes is a separate change, out of scope here.

---

## The GUI

A new `PanelTab::Textures` in the existing egui side panel. Chosen over a standalone web tool
because the question is "does this look right *in the game*" — real lighting, real UV anchoring,
real wall split — and the bind-group-swap architecture makes live preview nearly free.

Pattern to follow, all in `native/crates/game/src/app.rs`:

- Add a `PanelTab` variant + `ALL` entry — `app.rs:44-77`. The doc comment there says exactly this.
- Best template is the `PanelTab::Objects` arm — `app.rs:877-931`. It already does the required
  shape: a preview `ui.group` with an `egui::Image`, then a `ScrollArea` of `SHOP_GOLD_DIM` section
  headers with `selectable_label` rows. Map `PropCategory` → the scheme `group` field,
  `CATALOG` → the theme list, `arm_prop_placement` → the retexture call.
- Respect the deferred-action pattern documented at `app.rs:392-399`: the `egui_ctx.run` closure
  cannot hold `&mut World`. Snapshot state before, write to `Option`/`bool` locals inside, apply
  against `self.world` after tessellation (`app.rs:973-1051`).
- Theme it with `apply_shop_theme` — `app.rs:92-129`.

### Two known obstacles

1. **Swatches need retained texture views.** `build_materials` uploads each distinct texture once
   (deduped via `view_by_name`) but drops the `TextureView`s afterward; only
   `_material_keepalive` (`renderer.rs:372`) survives. To show BMP swatches in egui, retain the
   views and expose them via `register_native_texture` (the pattern used for prop/weapon previews —
   `renderer.rs:2037`, `:2704`, `:2528`).
2. **An open panel frees the cursor.** `set_scheme_at_crosshair` is pointer-lock gated
   (`app.rs:2478`), but `toggle_props` releases the lock (`app.rs:349-363`). A panel-driven picker
   must target the room via the mouse ray (`mouse_world_ray()`, `app.rs:369-390`) rather than the
   crosshair.

### Not in scope, but flagged

**There is no pillar zone.** Floor, ceiling, lower/upper wall, stair, doorframe floor and brace all
exist as zones; pillars do not. `pillarMode` in the old JS editor built pillars as *plain brushes* —
no `isPillar` flag, no zone — so they classify as walls and inherit the room's wall texture. Giving
pillars their own texture needs a marker on the brush plus a classifier branch, i.e. a code change,
not a content change. Zone 4 is the one free slot (defined but never emitted).

---

## Decisions

| Question | Decision |
|---|---|
| Where does the GUI live? | In-engine egui panel (`PanelTab::Textures`) |
| How are themes sourced? | Auto-extract from the 21 OBJ+MTL levels, per room group |
| Plumbing? | Runtime texture loading **and** name-keyed persistence, both before adding themes |

## Staging

1. ~~**Plumbing**~~ — **DONE**: runtime texture loading; `SCHEMES` → `themes.json` (+ `group`);
   persistence migrated from index to name, `LEVEL_FORMAT_VERSION` bumped to 4. All 5 committed
   slot files verified loading unchanged. 511 tests green, release built, awaiting playtest.
2. ~~**Extraction + triage**~~ — **DONE**: `tools/texture-themes/` (obj_themes.py + texlib.py).
   1918 room candidates → **662 distinct themes**, 561 with all repeats in a plausible band.
   Validated: 5 of the 9 shipped hand-authored themes are reproduced exactly on all four
   zones. Plus a 993-image content-hash index and browsable contact/theme sheets.
3. ~~**Curate**~~ — **TOOLING DONE, pass pending**: `adopt.py --bulk` loaded **382 generated
   themes** (20 level families) + the original 10 = 392, copying 223 more BMPs. Zones 5/6/7
   filled from the Facility defaults. The judging itself is now an in-game job, by design —
   see stage 4. `adopt.py --prune` cuts the manifest to what you marked Keep.
4. ~~**GUI**~~ — **DONE**: `PanelTab::Textures` — swatch rows grouped by level family, text
   filter, verdict filter (All/New/Kept/Cut), ✔/✕ per theme, arm-and-click to retexture the
   room under the cursor. Verdicts persist to `native/assets/theme_review.json` on every
   click. `repeat` tuning in-panel is NOT built — see "what the panel does not do".

Stage 2 output can be reviewed as rendered PNGs via the headless harness rather than by driving the
game, so stages 2-3 need no playtest turnaround.


---

## What stage 2 measured

The extractor works, and `obj_themes.py validate` is its acceptance test: **5 of the 9
shipped hand-authored themes come back exactly on all four zones**, the other four match
2-3 of 4 with the best candidate in the right level. Those themes were authored by a
human reading these same levels, so reproducing them from raw source is the strongest
available correctness signal.

Four things measured during the build that each contradicted the plan above:

**Compare by content hash, not filename.** Every human-renamed texture in
`public/textures/` has exactly one temp-named twin (`grey_tile_floor` == `tempImgEd02B7`,
`white_tile` == `tempImgEd02CE`, 14 more). The name-based check of the extractor scored
2/9; by hash the same check scores 5/9. The renaming, not the extractor, was the gap.

**Wall faces must be geometrically split, not bucketed.** GoldenEye walls are typically
two full-height triangles, so filing each face into lower/upper by its lowest vertex puts
every wall in "lower" and leaves the upper band empty — which is exactly what the first
run produced. The fix mirrors `emit_wall_split`: clip each triangle at the split height
and contribute area to both bands. This is also what reveals the rooms where GE genuinely
*did* stratify a wall into two textures.

**The hand-tuned repeats cannot be fitted.** They imply constants spanning 14.4 – 173.5,
a 12× spread, because they were eyeballed against our authored room sizes rather than
GoldenEye's. So the scale is derived from geometry measured on both sides instead —
GE's 353-unit storey against our 16 WT default room (`GE_UNITS_PER_TILE = 22.06`) — and
the hand-tuned values serve only as a coarse bracket. Three of four zones land inside
that bracket; upper wall sits ~1.3× below it, which is expected because the lower/upper
split is our own invention and not something GE authored to. The anchor was deliberately
*not* tuned to force a pass.

**No cheap statistic separates material from signage.** Seam score alone passes Caverns'
direction arrows (a symbol on a flat field has trivially matching edges); dominant-colour
share flags diamond-plate and scalloped stone as signage while still passing the arrows;
border uniformity calls low-contrast rock signage while passing a white glow and a metal
ring. At 32×32 the statistic does not exist, so the tool does not claim it — it sorts by
the metrics and leaves the call to a human with the contact sheets.

### What stage 3 inherits

- 662 themes in `tools/texture-themes/out/theme_library.json`, each with provenance
  (level + room + how many rooms share it) and a derived `repeat` per zone.
- **101 flagged `repeats_in_band: false`** — the texture choices are still good, only the
  scale needs an eye.
- Zones 5/6/7 (stair riser, doorframe floor, brace) are unextractable — GoldenEye had no
  equivalent surfaces — and must be chosen during curation.
- Themes must be **appended** to `themes.json`, never inserted, or pre-v4 levels
  retexture (see the legacy-index ordering constraint above).


---

## What stages 3-4 built

`themes.json` now carries **392 themes** (the original 10 + 382 generated across 20 GoldenEye
level families). The set is deliberately over-broad: it exists to be pruned, and the pruning
is a human job the panel exists to serve.

### The loop

1. `adopt.py --bulk` — load the whole extracted library into the game (data only).
2. In BUILD, **O** then cycle to **TEXTURES** — browse, click a theme to arm it, click a wall
   to retexture that room, mark ✔ keep or ✕ cut.
3. `adopt.py --prune` — cut `themes.json` to the kept set, re-key, back up the old manifest.

Verdicts live in `native/assets/theme_review.json`, keyed by theme **name** (never index —
pruning reorders the manifest and a verdict must survive that). It is the only asset file the
game writes; it saves on every click, because losing a 390-theme review pass to a crash would
cost far more than rewriting a few KB of JSON.

### Two design-doc obstacles that turned out cheaper than written

**Swatches needed no renderer change at all.** The plan said to retain the `TextureView`s that
`build_materials` drops and expose them via `register_native_texture`. Unnecessary:
`egui::Context::load_texture` takes CPU pixels and owns the upload itself, so the panel builds
swatches straight from `textures::try_decode` with `renderer.rs` untouched.

**The mouse-ray pick already existed.** `pick_face_hit_from(origin, dir)` was already in
`world/pick.rs`, added for the prop tool for exactly the same reason (an open panel frees the
cursor). Retexturing only needed a `set_scheme_along(ray, scheme)` wrapper beside the existing
`set_scheme_at_crosshair`.

### Cost of 392 themes

2354 defined zones → that many bind groups, and 334 distinct textures decoded and uploaded at
startup. Measured decode cost for all 334: **30 ms**. Not a concern.

### What the panel does not do

- **No in-panel `repeat` tuning.** Judging a theme is keep-or-cut; adjusting a scale is a
  different, slower job. When the kept set is small, tuning it is a `themes.json` edit with no
  rebuild — which is what stage 1 bought. Worth adding to the panel only if the pruning pass
  shows a lot of otherwise-good themes failing on scale alone.
- **No pillar zone.** Still true, still a code change rather than content (see above).
- **Number keys still cap at 9.** After `--bulk` they land on the first theme of the nine
  largest families; everything else is panel-only. Pruning re-keys automatically.


---

## Stage 5: the custom theme editor

Built after the first curation pass showed that picking from generated combinations isn't
enough — an author wants to compose a theme from any texture and dial the UVs.

### What it does

- **Any texture, any zone.** All **1016** distinct images now ship in
  `native/assets/textures/`, with `native/assets/texture_index.json` recording which
  GoldenEye level each came from. The picker groups by that level, because a flat
  1016-row list is unusable and "which level was this from" is how an author thinks.
- **Live UV scale *and* offset.** `repeat` was already a shader uniform; `offset` is new
  (`params.yz`, which were unused — no uniform or bind-group layout change). Dragging
  either slider updates the world at frame rate.
- **A live preview room.** A stock 24x16x24 WT room rendered into the panel from the
  *real* CSG fold, so its wall split, UV projection and zone classification are the
  engine's own rather than a hand-rolled box that could disagree.
- **Presets** save to `native/assets/user_themes.json` — deliberately a separate file, so
  `adopt.py --bulk` (which regenerates the manifest tail) and `--prune` (which drops
  unreviewed entries) cannot destroy hand-made work.
- **Per-level quick keys.** `theme_hotkeys` on the level file maps 1-9 to theme names, and
  wins over the manifest's own `key`. A bunker level and a jungle level want different
  nines.

### Three implementation notes

**Live editing needed the material buffers to become addressable.** They were
`_material_buffers: Vec<Buffer>` — keepalive only. Now `material_params[scheme][zone]`, so
a scale change is one `write_buffer`. Changing a *texture* does need a bind-group rebuild,
which means retaining the `TextureView`s `build_materials` used to drop — the thing the
swatches avoided needing, but the editor genuinely requires.

**Custom slots are pre-allocated, not grown.** The registry is a `OnceLock` handing out
`&'static [Scheme]`; appending at runtime would mean making that mutable and reworking
every caller, plus growing the material table mid-frame. So 24 slots exist from startup
(`CUSTOM_SLOTS`), seeded from `user_themes.json`. A saved preset works *immediately*
because its slot already has bind groups. The one visible seam: its stored label and
`kind.used` only refresh on restart, so the app keeps a session-local label map
(`theme_slot_labels`) to avoid showing a just-saved preset as "(empty 03)".

**The level format did NOT need a bump.** `theme_hotkeys` is `#[serde(default)]`, so v1-v4
files load with none bound and an older build ignores the field — which by this file's own
documented rule ("bump when a change can't be absorbed by `#[serde(default)]` alone") is
not version-worthy. It stays v4.

### Known seams

- The **scratch theme is shared**: applying it to several rooms then editing changes all of
  them. The panel says so; "Save as preset" is how a look becomes stable.
- Clearing a zone makes it **invisible**, not untextured (the renderer skips undefined
  zones). The button warns, but it is a real foot-gun.
- Preview and prop turntable share one offscreen target. Fine today — they're different
  tabs — but a second simultaneous preview would need a second target.
