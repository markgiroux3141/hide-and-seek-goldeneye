# Handoff — Perfect Dark weapons, second pass (fidelity + polish)

State: branch `feat/pd-hunters-ship` @ `170dcd5`, **384 tests green**, release built.

The PD arsenal **works**: 33 guns, two firing functions each, playable by the player and
the hunters, with both models per gun and PD's explosion model. See
`HANDOFF_PD_WEAPONS.md` for what that track landed and why.

This handoff is the **fidelity pass** that playtest asked for. Five items, all reported from
a real session on `ARSENAL=pd`. The recon below is done — every root cause named here was
traced to a decomp line or measured off the assets, so **start from the findings, not from
the symptoms.**

```powershell
$env:ARSENAL = "pd"; $env:OWN_ALL = "1"
.\native\target\release\build-and-hide.exe
```

```
cargo test --release      # 384 green today
python tools/pd-assets/pd_weapons.py rust native/crates/game/src/combat/pd_weapons.rs
python tools/pd-assets/pd_gltf.py guns tools/pd-assets/pd_weapons.json native/assets/weapons/pd
```

**Two standing rules.** Always go back to the decomp (`reference/pd-decomp`, gitignored —
`reference/README.md` says how to re-clone); this track has produced eight cases where the
obvious answer was wrong and the decomp had the real one. And **never launch and drive the
game yourself** — hand the user a specific brief instead.

---

## Do these in this order

The order matters: item 1 unblocks item 2, and item 3 is the one that needs the user's eye
rather than more code.

### 1. Port `tex_inflate_non_zlib` — this is the muzzle flash

**Symptom:** "the muzzle flash is just the entire white square muzzle mesh appearing for a
second, it's not a texture with a transparency that looks like a flame."

**Root cause, measured:** the flash geometry and its material are fine. **All 76 flash
textures across all 17 guns that have one fail to decode** — 100% — because PD's second
texture codec is not ported (`pd_tex.py` raises
`UnsupportedTexture("non-zlib codec (tex_inflate_non_zlib) is not ported")`). Undecodable
textures fall back to the flat per-part debug palette, which is exactly the opaque white
square being described. There is no flame texture on screen because the flame texture never
decoded.

This is not cosmetic polish, it is a hard blocker, and it is **the same gap** behind the
remaining flat patches elsewhere: honouring part visibility already took undecodable
*visible* textures from 117 down to 37, and what is left is all this codec. Worst affected:
MagSec 4 (6), K7 Avenger (7), Rocket Launcher (4), Laser (4).

**The work:** `tex_inflate_non_zlib` is **193 lines** at `game/texdecompress.c:699`, called
from `:2263`. Port it into `tools/pd-assets/pd_tex.py` beside the existing zlib path. Then
re-export. Expect 37 → 0 and the flash to become a real flame.

**Then finish the flash properly** — three things beyond the codec:

* **There are three flashes, not one.** `MODELPART_GUN_MUZZLEFLASH1/2/3` (`0x5a/0x5b/0x5c`,
  `constants.h:2421`) are each annotated `// toggle`, and the Reaper hides all three. PD
  alternates them per shot; we export only `0x5a`. Exporting all three and picking one per
  shot is what stops a repeated flash reading as a stuck decal.
* **The material must not be `MASK`.** `export_gun` writes `alphaMode: "MASK"` with
  `alphaCutoff: 0.5`, which is right for hard-edged N64 gun skins and wrong for a flame. A
  flash wants blending (ideally additive — the viewmodel shader already *adds* its matcap
  term, so there is precedent).
* **`load_flash` filters for `CullBoth`** (`combat/viewmodel.rs:40`) and falls back to
  "keep everything" — which is what our PD flash GLBs hit. Worth naming the flash material
  so the intended path is taken rather than the fallback.

**Verify:** `pd_preview.py` renders a GLB on the CPU and now applies `COLOR_0`, so a decoded
flame is visible without launching anything.

### 2. Hide the hand — `gunviscmds` is not ported, and it is why the Mauler looks wrong

**Symptom:** "guns like the mauler look very off to me, we might have to take some of these
case by case."

**Root cause:** it is not case-by-case. `weapondef` carries **two** visibility tables and we
only ported one:

```c
/*0x3c*/ struct gunviscmd *gunviscmds;            // NOT ported
/*0x40*/ struct modelpartvisibility *partvisibility;  // ported
```

`gunviscmds_mauler` (`invitems.c:1146`) is:

```c
gunviscmd_sethidden(MODELPART_HAND_LEFT)
gunviscmd_sethidden(MODELPART_MAULER_MAGAZINE2)
gunviscmd_end
```

**PD's first-person gun models contain a HAND** (`MODELPART_HAND_LEFT` = `0x35`,
`constants.h:2426`), and `gunviscmds` is what hides it. **14 weapons hide the left hand this
way**, out of 29 `gunviscmd` tables. So those guns are currently rendering with a
disembodied hand attached — which is precisely the `enemy-weapon-hand-artifact` problem from
the GoldenEye set, except PD *tells us which part is the hand by name*, so no material
heuristic is needed this time.

**The work:** transcribe `gunviscmds` in `pd_weapons.py` (the parser already handles
`partvisibility`; this is the same shape) and fold the unconditional `sethidden` entries into
the export's hide list. `struct gunviscmd` is at `types.h:2889` with the condition/op
semantics commented — `type` 0 terminates, 5/6 are hand-specific, `op` 0/1/3 set
visible/hidden/conditional. **Only the unconditional ones are safe to bake**; the
`upgradewant`-conditional entries are runtime state and should be left alone rather than
guessed at.

Expect this to fix most of "looks off" in one change. Re-review the sheet afterwards and only
then treat what remains as per-weapon.

### 3. Viewmodel placement — ask the user, do not keep guessing

**Symptom:** "the gun placement on the screen. Before I placed the guns manually but is
there anything in the pd decomp that tells us where the gun should be placed correctly?"

**Answer: yes, and it is already in use — but only the static third of it.** What is ported
is `weapondef.posx/posy/posz`, and that much is confirmed correct:
`gset_get_xpos` (`gset.c:155`) returns `weapon->posx` for the right hand and `-posx` for the
left, and `bgun_update_hand_pos` (`bondgun.c:7411-7419`) assigns those straight into the view
offset. **Do not re-derive the sign** — it was already got wrong once (negating `posz` put
every gun behind the camera and drew nothing) and the fix is documented in
`combat/arsenal.rs`'s `view_offset`.

What is **not** ported, in rough order of how much it would change the look:

| thing | where | what it does |
|---|---|---|
| `PD_VIEW_SCALE` | `combat/arsenal.rs` | **0.0007, inherited from the character pipeline and never measured for guns.** The most likely reason a gun reads too big or too small. |
| `weapondef.muzzlez` | `types.h:3033` | Per-weapon muzzle depth; transcribed, unused. |
| `weapondef.sway` | `types.h:3037` | Per-weapon sway amount; transcribed, unused. |
| `invaimsettings.guntransup/down/side` | `types.h:2877` | The gun translates as you aim up/down/sideways (default 3 / 8 / 15). This is a large part of why PD's gun feels attached to the view. |
| `hand->damppos` / `adjustpos` | `bondgun.c:7411+` | Per-frame damping and sway. |
| `player->guncloseroffset` | `bondgun.c:7424` | The "hold gun closer" option. |

**The judgement call, and it is the user's:** they hand-placed the GoldenEye guns and were
happy with them. PD's authored numbers are *different*, not obviously better, and they have
never been eyeballed. So do not spend a session tuning 33 offsets before asking. Put the
question to them with the arsenal on screen: keep PD's authored placement and fix the scale,
or treat PD's numbers as a starting point and hand-tune like the GoldenEye set. Porting
`invaimsettings` is worth doing either way — it is behaviour, not placement.

### 4. Reload animations — the `guncmd` bytecode

**Symptom:** "PD has more advanced reloading animations for guns, we should look into this."

**What PD has.** Our reload is a timer plus a viewmodel dip (`RELOAD_DIP` 0.6 in
`combat/viewmodel.rs`). PD's is a real keyframed animation with sound and part visibility
driven off it. `invanim_falcon2_reload_singlewield` (`invitems.c:~395`):

```c
gunscript_playanimation(ANIM_GUN_FALCON2_RELOAD, 0, 10000)
gunscript_showpart(1, MODELPART_HAND_LEFT)        // the hand comes IN for the reload
gunscript_showpart(1, MODELPART_FALCON2_MAGAZINE2)
gunscript_playsound(10, SFXNUM_01D8_RELOAD_REMOVE)
gunscript_hidepart(19, MODELPART_FALCON2_MAGAZINE1)
gunscript_allowfeature(24, GUNFEATURE_RELOAD)
gunscript_playsound(24, SFXMAP_80F6)
gunscript_hidepart(24, MODELPART_FALCON2_MAGAZINE2)
gunscript_playsound(53, SFXNUM_01DB_RELOAD_RACK)
gunscript_allowfeature(53, GUNFEATURE_ATTACKAGAIN)
gunscript_end
```

Note how much this settles at once: **that is where the hand and the spare magazines come
from** (item 2 hides them at rest; the reload shows them), it is where the three reload
sounds and their exact timings come from, and `allowfeature(53, ATTACKAGAIN)` is the authored
answer to "when can the player fire again" — which we currently approximate with
`reload_time`.

`invanim_falcon2_reload` dispatches to **separate single-wield and dual-wield** scripts via
`gunscript_include`, and the scope variant has its own.

**The work, and the honest sequencing:** the bytecode is 12 opcodes
(`include/gunscript.h`, `GUNCMD_*` in `constants.h`) and small enough to port properly. But
**it needs a keyframed viewmodel first** — the parts have nowhere to land while the gun is
one static mesh with a dip. The exports were deliberately built for this: part offsets are
baked into the geometry rather than dropped, so **adding articulation is a re-export, not a
re-rip** (`export_gun` already takes `only_parts`, which is how the flash was split out).

So: articulated export → then the `guncmd` interpreter → then reloads, sounds and
`ATTACKAGAIN` all fall out of the same system. Do not try to special-case reload animations
without it.

### 5. PD weapon sounds

**Symptom:** "it seems like we're still using goldeneye sounds, let's see if we can find and
use the pd gun sounds."

**Correct — every PD gun currently borrows the closest GoldenEye sound by role**
(`fire_sound_for` in `combat/arsenal.rs`). That was always a stopgap: the real
`funcdef_shoot.shootsound` ids **are transcribed** and sitting unused in
`combat/pd_weapons.rs` (e.g. the Falcon 2's `SFXMAP_804D`, the silenced variant's
`SFXMAP_8054`), and the reload scripts above name theirs too.

**What is needed:** PD's audio is `sfx.ctl` (195 KB of N64 ALBank instrument metadata) and
`sfx.tbl` (4.99 MB of samples) under
`reference/pd-decomp/src/assets/ntsc-final/`, extracted by `tools/extract` (see its
`sfxctl`/`sfxtbl` offsets). The samples are **VADPCM**, Nintendo's ADPCM codec — a
well-documented format, but it needs a decoder written. Then map `SFXMAP_*` → bank index →
sample, and convert to WAV.

**Precedent for the wiring half:** the repo already converted 375 GoldenEye AIFFs to
`pcm_s16le` WAV for kira (see the `goldeneye-soundpack` memory) — so once decoded, the path
into the engine is known. This is a self-contained sub-project; it touches nothing else and
could be done in isolation.

---

## What NOT to redo

Each of these was measured this session and is settled. Re-deriving them is how the wrong
answer gets reintroduced.

* **`posz` is used directly, not negated.** PD's z runs the same way ours does.
* **The barrel axis is authored** (`CHRGUNFIRE`, −X on all 27 models that have one). Where it
  is absent (17 of 33) PD's own `chr_get_gun_pos` fires **from the grip** — that is the
  answer, not a gap to fill.
* **`Vtx.colour` is a byte offset into a table that is *either* colours *or* normals**,
  per node — 120 colour nodes, 54 normal nodes across the 33 guns. Both are exported now;
  normal nodes are pre-lit into `COLOR_0` because the viewmodel shader is unlit by design.
* **PD's null table slot `(0,0,0,0)` means "unspecified", not black.** Treating it as a
  colour rendered the Remote Mine as a pure black silhouette.
* **Not every gun is shaded.** The Dragon's visible nodes reference only the null slot and
  `(254,254,254)` — authored flat, leaning on its texture. A test asserting all 33 vary was
  asserting something untrue about the source.
* **`WEAPONFLAG_AICANUSE` is not a gun filter** (it is on all 64 real weapons); the AI data is
  `g_BotWeaponConfigs`.
* **1 PD damage unit = 25.0 HP**, derived from shots-to-kill agreeing on both sides.
* **Names collide across families** (7 of them). Resolve by **display** name — resolving by
  source name made GoldenEye's Shotgun pick up PD's −X barrel and aim 83° off.

## Traps that keep firing on this track

1. **CPU-side green does not mean it works.** Every defect in this handoff passed the whole
   suite. Hand off a playtest brief.
2. **A test can fail because of the fix, or pass because of the bug.** Four fired this
   session, including one that silently changed meaning when a default moved.
3. **Assert token counts when transcribing.** Four parser defects surfaced only because the
   generator refuses a column-count mismatch — one of which mis-resolved every weapon past
   `0x4f` into a *plausible* wrong answer.
4. **Measure the invariant you actually have.** The barrel-axis test asserts "dominant
   component is −X" because that is what was measured; an invented angular tolerance failed
   on the Reaper, which is a minigun with the barrel slung under the grip.
5. **An absence renders as a plausible-looking bug.** A missing `COLOR_0` reads as "no
   textures"; an undecodable texture reads as "the flash is a white square". Check whether
   the data arrived before theorising about the renderer.

## Context worth reading

* `HANDOFF_PD_WEAPONS.md` — what the first pass shipped, and its own remaining list.
* `DESIGN_PD_WEAPON_MECHANICS.md` — §2 (no attach offset to tune), §3 (two models per gun),
  §8 (`guncmd`).
* `combat/arsenal.rs` — the bridge, and the documented reasoning for every conversion.
* `tools/pd-assets/pd_gltf.py` — `dl_vertex_table` and `gun_metadata` carry the findings
  above in full.
* Memory: `pd-arsenal-decisions`, `pd-weapon-mechanics`, `goldeneye-soundpack`.
