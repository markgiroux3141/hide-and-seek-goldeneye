# Handoff — Perfect Dark weapons, second pass (fidelity + polish)

State: branch `feat/pd-hunters-ship`, **389 tests green**, release built, assets re-exported.

The PD arsenal **works**: 33 guns, two firing functions each, playable by the player and
the hunters. `HANDOFF_PD_WEAPONS.md` covers what that first track landed.

This pass was the **fidelity** work playtest asked for. Items 1 and 2 are **done**, and
finishing them corrected three things the previous handoff had wrong — read
"What the measurements corrected" before picking up items 3–5, because two of them are
smaller than they looked and one of them is already answered.

```powershell
$env:ARSENAL = "pd"; $env:OWN_ALL = "1"
.\native\target\release\build-and-hide.exe
```

```
cargo test --release
python tools/pd-assets/pd_weapons.py json tools/pd-assets/pd_weapons.json
python tools/pd-assets/pd_gltf.py guns tools/pd-assets/pd_weapons.json native/assets/weapons/pd
python tools/pd-assets/pd_preview.py <gun.glb> out.png --viewmodel   # see it without launching
```

**Two standing rules.** Always go back to the decomp (`reference/pd-decomp`, gitignored —
`reference/README.md` says how to re-clone). And **never launch and drive the game
yourself** — hand the user a specific brief instead.

---

## DONE this pass

### The textures our codec cannot decode now come from the editor dump

PD's second texture codec (`tex_inflate_non_zlib`) is unported, and an undecodable texture
fell back to a flat debug palette. That was **1,063 triangles across the arsenal** — 89.6%
of the MagSec 4, 62% of the Laser, 25.6% of the K7 Avenger, and **100% of all 76 muzzle-flash
textures**. `pd_gltf.py` now falls back to the user's Perfect Gold export (`pd dump/weapons/`,
355 BMPs) keyed by **global pool texture number**, which is why one first-person export also
covers third-person models it never contained.

**61 triangles remain on debug palette, all on five `chr*` third-person models**
(MagSec 4, Shotgun, DY357 ×2 — pool textures `0x934`/`0x96a`–`0x97d` the dump never carried).
Porting `tex_inflate_non_zlib` (193 lines, `game/texdecompress.c:699`) would close them, and
nothing else needs it.

Two traps found by cross-checking, not by reasoning:

* **The editor writes BMP rows top-down while declaring a positive height** (which per spec
  means bottom-up). Measured against the 427 textures both pipelines can produce: 423 agree
  only when read in file order, 4 are symmetric. A wrong choice here is invisible — it just
  makes every skin subtly wrong.
* **Texture `0x606` is 4×16 in the texconfig and 8×16 in the dump.** UVs normalise by the
  shipped image's size now, not the texconfig's, or the used half stretches over the whole.

### The muzzle flash is a flame

Same root cause; nothing was wrong with the geometry or the blend. And **the four textures
are not four frames — they are a 2×2 tiling of one flame**, drawn on two mirrored planes.
Rasterised from the shipped GLB it is a soft yellow-white star (`tools/pd-assets/preview/`).
The additive `SrcAlpha` muzzle pipeline was always right; the alpha was the debug palette's,
which is opaque, which is the white square that was reported.

Regression test: `the_muzzle_flash_is_a_soft_flame_not_a_square` asserts partial alpha in
the flash textures, which is the entire difference between the two pictures.

### Environment mapping — the guns read as metal now

`0x323` and 26 other textures are **reflection maps**, not surfaces: those faces generate
texcoords from the normal on the N64 (`G_TEXTURE_GEN`), so the stored s/t is leftover data
and sampling with it smears a tiled blue-grey flat across the gun. Exporter zeroes the base
factor on an `EnvMapping` material and lets the engine's matcap paint it.

The scope of that reflection had to change with it: `EnvScope::PerMaterial` for PD.
GoldenEye tags **one** material and means the whole (black-based) gun; PD mixes env-mapped
metal with real skins on one model, and the whole-model rule washed the Shotgun out to
near-white. Seen in `pd_preview.py --viewmodel`, which is a transliteration of
`shader_viewmodel.wgsl` (same matcap, same `1.6`, same base-factor fold) added this pass
specifically so this decision could be made without launching anything.

### Two smaller fixes that came out of the same work

* **Authored-untextured triangles** (`tex == -1`) now draw white × vertex colour, which is
  what PD does with them. Six of them put a garish palette stripe on the **Mauler** — the
  gun the user singled out as "very off".
* **`gunviscmds` is ported** (unconditional rows only; the `upgradewant`/which-hand rows are
  runtime state). Across the whole arsenal it hides exactly two more geometry groups: the
  silenced Falcon 2's `002F` and the **CMP150's open cartridge flap**.
* **Fully transparent texels are discarded** in the viewmodel shader. The gun pass is opaque
  and depth-writing, so a cut-out drew as a black patch that also occluded — visible on the
  K7 Avenger, whose detail decals are ~75% transparent. This also touches the GoldenEye guns
  (11–15% of some of their texels), and is the one change here that alters an asset family
  nobody complained about — worth a glance during playtest.
* **`every_shader_compiles`** (new, engine): naga parses and validates every `.wgsl`. Shaders
  were the one part of the renderer no test touched — a typo was a panic on launch.

## What the measurements corrected

Three claims in the previous handoff were wrong, and each cost less to check than to act on.

1. **"14 weapons render a disembodied hand" — no. No gun model has a hand at all.**
   `MODELPART_HAND_LEFT` appears in 14 `gunviscmds` tables, but checked across all 33
   first-person and all 33 third-person models, **not one carries part `0x35` or `0x36`**.
   PD's first-person hands live in their own model, so those rows address geometry we never
   load. This was expected to "fix most of looks-off in one change"; it fixes nothing, and
   the actual cause of the Mauler was the six untextured triangles above.
2. **"There are three flashes, not one" — only for the Reaper.** `MUZZLEFLASH2/3` exist on
   exactly one model of the 33, and their geometry sits at the **same centroid** as
   `MUZZLEFLASH1`, so they are alternate frames of one flash rather than three barrels.
   Alternating them needs a runtime that can hold more than one flash mesh per weapon
   (`WeaponStats.muzzle_path` is a single `&str`) — for one gun. Deliberately not done.
3. **`PD_VIEW_SCALE` is not the problem.** At 0.0007 the guns come out physically right and
   agree with the GoldenEye set: Falcon 2 0.19 m, MagSec 0.31 m, AR34 **0.778 m** against
   GoldenEye's AR33 **0.776 m**, sniper rifle 0.93 m. If placement still reads wrong it is
   the offsets, not the size.

---

## Remaining, in the order they are worth doing

### 3. Viewmodel placement — a question for the user, not a task

`weapondef.posx/posy/posz` is ported and correct (`gset_get_xpos`, `bgun_update_hand_pos`);
**do not re-derive the sign**, it was got wrong once and put every gun behind the camera.
With the scale question retired (above), what remains unported is **behaviour**, not size:

| thing | where | what it does |
|---|---|---|
| `invaimsettings.guntransup/down/side` | `types.h:2877` | the gun translates as you aim (3 / 8 / 15). A large part of why PD's gun feels attached to the view. |
| `hand->damppos` / `adjustpos` | `bondgun.c:7411+` | per-frame damping and sway |
| `weapondef.sway` | `types.h:3037` | per-weapon sway amount; transcribed, unused |
| `weapondef.muzzlez` | `types.h:3033` | per-weapon muzzle depth; transcribed, unused |
| `player->guncloseroffset` | `bondgun.c:7424` | the "hold gun closer" option |

**The judgement call is the user's:** they hand-placed the GoldenEye guns and were happy.
Keep PD's authored placement, or hand-tune 33 offsets like the GoldenEye set? Porting
`invaimsettings` is worth doing either way — it is behaviour, not placement.

### 4. Reload animations — the `guncmd` bytecode

Unchanged from the previous handoff, and still gated the same way. PD's reload is a real
keyframed animation with sound and part visibility driven off it
(`invanim_falcon2_reload_singlewield`, `invitems.c:~395`); ours is a timer plus a viewmodel
dip. The bytecode is 12 opcodes (`include/gunscript.h`) and small enough to port properly,
but **it needs an articulated viewmodel first** — the parts have nowhere to land while the
gun is one static mesh. The exports were built for that: part offsets are baked into the
geometry rather than dropped, so articulation is a **re-export, not a re-rip**
(`export_gun` already takes `only_parts`).

Sequence: articulated export → `guncmd` interpreter → reloads, sounds and
`allowfeature(ATTACKAGAIN)` all fall out of the same system.

Note this is also where **the hand and the spare magazines come from** — the reload script
shows them. Which is the other half of finding 1 above: PD only ever draws that hand during
a reload, from a model we do not load.

### 5. PD weapon sounds — self-contained, blocked on a VADPCM decoder

Every PD gun still borrows the closest GoldenEye sound by role (`fire_sound_for`). The real
`funcdef_shoot.shootsound` ids **are transcribed and unused** in `combat/pd_weapons.rs`.
The samples are `sfx.ctl` + `sfx.tbl` (`reference/pd-decomp/src/assets/ntsc-final/`) in
**VADPCM**, which needs a decoder written; then map `SFXMAP_*` → bank index → sample → WAV.
Precedent for the second half: the repo already converted 375 GoldenEye AIFFs for kira
(`goldeneye-soundpack`). Touches nothing else.

### 6. Optional cleanup

* Port `tex_inflate_non_zlib` and the pipeline is self-sufficient from the ROM alone (closes
  the last 61 palette triangles).
* The Reaper's alternating flash (finding 2) if the runtime ever holds more than one flash.

---

## Playtest brief

`ARSENAL=pd OWN_ALL=1`, then cycle the arsenal and look for:

1. **Fire something.** The flash should be a soft yellow-white flame, not a white square.
2. **MagSec 4, Laser, K7 Avenger, Rocket Launcher, Mauler** — these were the worst hit by the
   missing textures. The Mauler in particular should have lost its odd stripe.
3. **Falcon 2, DY357 Magnum, Laptop Gun** — metal now, from the reflection map. Too shiny?
   The knob is the `1.6` in `shader_viewmodel.wgsl` (and `ENV_GAIN` in `pd_preview.py`).
4. **CMP150** — its cartridge flap should be closed.
5. **GoldenEye guns (`ARSENAL=ge`)** — unchanged except for the transparent-texel discard.
   Anything showing a new hole is that change.
6. **Placement** — the question in item 3. Sizes are confirmed right; judge position only.

## Traps that keep firing on this track

1. **CPU-side green does not mean it works.** Every defect in the previous handoff passed the
   whole suite. Hand off a playtest brief.
2. **A test can fail because of the fix.** `every_gun_mesh_carries_per_vertex_shading` failed
   on a deliberately black env material, and the right answer was to teach it the new fact,
   not to revert.
3. **Check the data arrived before theorising about the renderer.** Three separate symptoms
   this pass ("white square flash", "dull blue-grey metal", "the Mauler looks off") were all
   missing or misused *data*, and none was a renderer bug.
4. **Verify a claim against the assets before building on it.** All three corrections above
   took one measurement each and would each have cost a session of work.

## Context worth reading

* `HANDOFF_PD_WEAPONS.md` — what the first pass shipped.
* `DESIGN_PD_WEAPON_MECHANICS.md` — §2, §3, §8 (`guncmd`).
* `tools/pd-assets/pd_gltf.py` — `editor_textures`, `read_bmp`, `resolve_texture`, `export_gun`.
* `combat/arsenal.rs` — the bridge, with the reasoning for every conversion.
* Memory: `pd-arsenal-decisions`, `pd-weapon-mechanics`, `pd-editor-dump-textures`.
