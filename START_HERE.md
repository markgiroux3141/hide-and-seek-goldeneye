# START HERE — GoldenEye / Perfect Dark reference material

Master index for the reverse-engineering + asset-extraction work. Read this first when
starting a session in `native/`, then jump to whichever document below matches the task.

Everything here came out of two efforts: analysing the closed-source **GoldenEye Setup Editor**
(`C:\GEEdit4\PerfectGold.exe`), and extracting the **Perfect Dark** ROM via the open-source
decompilation.

---

## Quick orientation

| I want to… | Go to |
|---|---|
| Port Perfect Dark's simulant AI into our hunters | [DESIGN_PD_SIMULANT_AI.md](DESIGN_PD_SIMULANT_AI.md) |
| Know where PD's gun placement / fire timing / damage data lives | [DESIGN_PD_WEAPON_MECHANICS.md](DESIGN_PD_WEAPON_MECHANICS.md) |
| Use ripped PD models / textures / sounds / levels | §2 below |
| Bring a PD character into the engine, skinned + animated | §2, "Engine import" |
| Understand the GE Setup Editor's editing model | [GE_SETUP_EDITOR_ANALYSIS.md](GE_SETUP_EDITOR_ANALYSIS.md) |
| Re-clone or re-extract the reference material | [reference/README.md](reference/README.md) |
| Decompress a raw GE/PD ROM asset | §4 below |

## 1. Important: the big stuff is gitignored

`reference/` holds three upstream decompilations plus ~1.1 GB of extracted assets. It is
**gitignored on purpose** — third-party code under its own licence, plus regenerable output.

**A fresh clone will not have any of it.** Rebuild with the commands in
[reference/README.md](reference/README.md) and §3 below. What *is* committed: this file, the
two analysis documents, and the scripts in [tools/ge-editor-analysis/](tools/ge-editor-analysis/).

## 2. Extracted Perfect Dark assets

Root: `reference/pd-decomp/src/assets/ntsc-final/`

| Category | Path | Count | Size |
|---|---|---|---|
| Levels (geometry + pads) | `files/bgdata/` | 120 | 5.0 MB |
| Level tiles (clipping / collision) | `tiles/` | 60 | 148.7 MB |
| Pads (nav + placement points) | `pads/` | 60 | 24.4 MB |
| Characters | `files/chrs/` | 148 | 4.4 MB |
| Weapons | `files/guns/` | 106 | 1.0 MB |
| Props | `files/props/` | 433 | 2.3 MB |
| Textures | `textures/` | 3,504 | 2.7 MB |
| Animations | `animations/` | 1,207 | 6.2 MB |
| Sound effects (decoded to `.mp3`) | `files/audio/` | 548 | 5.1 MB |
| Music sequences | `sequences/` + `seq.ctl` / `seq.tbl` | 119 | 0.5 MB |
| Text / localisation | `lang/` | 68 | 2.1 MB |

**Use the manifests, not raw indices.** `textures.json`, `animations.json` and
`sequences.json` sit in that same directory and give named lookups. Music sequences carry real
names (`skedarruins-intro`, `mainmenu`, `crashsite-amb`), and level codes match the `bg_*`
names familiar from the Setup Editor.

A second, smaller tree at `reference/pd-decomp/extracted/ntsc-final/` (9.2 MB) holds the raw
segment blobs — `data.bin`, `game.bin`, `mpconfigs.bin` and friends. Use it only when you need
pre-split data.

These are **N64-native formats**, not glTF. Nothing here drops straight into our engine; each
category needs a converter.

### Model conversion — solved for static geometry

[tools/pd-assets/pd_model.py](tools/pd-assets/pd_model.py) parses PD model files and exports
OBJ. **686 of 686 real models parse with zero degenerate triangles** (props, guns, chrs); the
only failure is a 0-byte `explosionbit.bin`. Verified by shape, not just by "it didn't crash":
`a51_crate1` comes out a perfect 1.15³ cube and `ak47` a 0.23 × 0.34 × 0.78 gun.

```sh
python tools/pd-assets/pd_model.py info  <model.bin>          # structure dump
python tools/pd-assets/pd_model.py obj   <model.bin> <out.obj>
python tools/pd-assets/pd_model.py batch <indir> <outdir>
```

Four format facts, all confirmed against the decompilation — the last one is a trap:

- A model file **is** a `struct modeldef` at offset 0; every internal pointer is a segmented
  address based at VMA `0x05000000`, so `file_offset = addr & 0xffffff`
  (`model_promote_offsets_to_pointers`, `lib/model.c:3968`).
- `modeldef.skel` under `0x10000` is **not a pointer** — it is an index into a shared skeleton
  table resolved at load (`model_promote_type_to_pointer`, `game/modeldef.c:167`). PD character
  models share a handful of skeletons.
- Rare shipped **custom microcode**. `Vtx` is 12 bytes, not 16 (`s16 x,y,z / u8 flags / u8
  colour / s16 s,t`), and `G_TRI4` (`0xb1`) packs up to four triangles into one 8-byte command
  with 4-bit indices, an all-zero triangle meaning "unused slot" (`src/include/gbiex.h:22`).
- **Addresses inside a display list are not all segment 5.** The renderer rebinds the RSP
  segment table per drawn node (`lib/model.c:3234`, `constants.h:3919`): segment 3 = matrices,
  **segment 4 = that node's own vertex array** (what `G_VTX` reads), segment 5 = the model base
  (so it doubles as the plain file-offset segment), segment 6 = the colour array following the
  vertices. Reading `G_VTX` as a segment-5 offset yields silent garbage for chrs and props —
  guns happen to survive it, which makes it an easy mistake to ship.

### Animation + skeleton — also solved

[tools/pd-assets/pd_anim.py](tools/pd-assets/pd_anim.py) decodes the bit-packed animation
format; [tools/pd-assets/pd_pose.py](tools/pd-assets/pd_pose.py) assembles a character skeleton,
applies an animation frame, and exports a posed OBJ.

**There is no separate bind pose to find**, which was the earlier worry. The bone hierarchy *is*
the model's node tree — `POSITION` nodes are joints carrying a rest offset and a `part` number,
`CHRINFO` is the root. The animation supplies the missing *rotations*, so a rest pose is simply
"any animation, frame 0". Per `model_update_position_node_mtx` (`lib/model.c:1052`):

```
local = rotation(anim_rot)  with translation = node.pos + anim_translate (when present)
world = local x parent_world                      (row-vector, mtx_c.c:107)
```

Animation files are `[header][frame 0][frame 1]…`. The header is one record per bone giving a
**base value and bit length** per channel; each frame stores only small bit-packed deltas added
to those bases — which is why a 163-frame animation fits in 11 bytes per frame. Rotations are
Euler XYZ, packed at `framelen` bits (usually 12) and scaled by `BADDTOR(360)/65536`.
`animations.json` exposes `headerlen` as `unk08` and `framelen` as `unk0a`.

Verified across the whole set: **65 characters carry the shared 15-bone rig**, every animation
header parses to exactly the declared file size, and posing puts the head on top, the shoulders
symmetric, and the feet level in a standing clip but staggered mid-stride in a walk.

#### Skinning is per-vertex, via `G_MTX`

**A mesh node is not a single bone.** Segment 3 is bound to the model's whole matrix array
(`gSPSegment(..., SPSEGMENT_MODEL_MTX, model->matrices)`, `lib/model.c:3525`), and a display
list issues `G_MTX` commands mid-stream to switch bones — so one `DL` node's vertices can span
a hip and a knee. The bone for a vertex is whichever matrix was bound when it was loaded;
`matrix index = (w1 & 0xffffff) / 64`.

Getting this wrong is very visible but easy to misread as a coordinate bug: the figure is
recognisable and correctly proportioned, but limbs tear off at the joints. Measured on the
a51guard, transforming per mesh node instead of per vertex leaves 60 triangle edges stretched
past 25 cm; per vertex it is zero, and the longest edge drops from 0.371 m to 0.249 m.

`POSITION` nodes own three matrix slots (`mtxindexes[3]`) — index 0 is the joint, 1 and 2 are
blend matrices the engine slerps for smooth deformation across the joint. Mapping all three to
the owning joint is exact for index 0 and a good approximation for the blends.

#### Scale: 1000 model units per metre

Derived, not fitted. The engine scales a body by `g_HeadsAndBodies[bodynum].scale * 0.1`
(`body_instantiate_model_to_addr`, `game/body.c:170`) with `scale` = 1 for ordinary bodies, and
**PD world units are centimetres** — the same table gives heights in cm (167, 165, 159…), and
the gameplay constants agree (melee range 210 = 2.1 m, follow distance 300 = 3 m, arrival
deceleration inside 200 = 2 m). So model units are millimetres: `raw * 0.1 = cm`, i.e. **1000
units per metre**.

Checks out: a51guard 1.73 m against a nominal 167 cm (mesh extents run a little past nominal,
as hair and boots would), mrblonde 1.77 m, maian_soldier 0.99 m because Maians are a metre tall.

Do **not** use `modeldef.scale` for this. It approximates the right value for many characters,
which is what makes it dangerous, but it is not what the engine uses and some models are wild
outliers — `dark_frock` carries 1982.

`g_HeadsAndBodies` (`game/modeldata/robot.c:64`) is worth transcribing wholesale — per body it
gives height, scale, `animscale`, and `handfilenum`, the first-person hand model for that
character.

### Engine import — solved, via glTF

[tools/pd-assets/pd_gltf.py](tools/pd-assets/pd_gltf.py) exports a chr as a **skinned
GLB** and an animation as a **clip GLB**, in the shape the engine's existing GoldenEye
character pipeline already consumes — no new Rust loader. PD's 15 joints are renamed
onto the GE bone roles (`Bone_1..Bone_15`), so weapon attach, head look-at, foot IK and
the aim overlay all work on a PD body unmodified. Heads (`head*.bin`) are grafted at the
`HEADSPOT`; blend matrices become real joints rather than an approximation.

```sh
python tools/pd-assets/pd_gltf.py batch tools/pd-assets/pd_roster.json native/assets/enemies/pd
python tools/pd-assets/pd_preview.py <any.glb> out.png --clip <clip.glb> --frames 6
```

[tools/pd-assets/pd_preview.py](tools/pd-assets/pd_preview.py) is a dependency-free CPU
renderer for any GLB (GE assets included). It exists because the two worst bugs in this
conversion passed every numeric check and were only visible on screen — see
[HANDOFF_PD_ASSETS.md](HANDOFF_PD_ASSETS.md).

### Still to do

Textures, levels (`bgdata`), tiles and pads are untouched. Character textures are the
biggest remaining visual gap: the data is **inline in each model file**, referenced by
`modeldef.texconfigs`, and the bodies currently ship with a flat per-part debug palette.

## 3. Regenerating everything

```sh
# 1. Clone the decompilations (shallow — we only read them)
git clone --depth 1 https://github.com/n64decomp/007            reference/ge-decomp
git clone --depth 1 https://github.com/n64decomp/perfect_dark   reference/pd-decomp
git clone --depth 1 https://github.com/fgsfdsfgs/perfect_dark   reference/pd-pcport

# 2. Stage the ROM under the name the extractor expects
cp "<pd rom>.z64" reference/pd-decomp/pd.ntsc-final.z64

# 3. Extract
cd reference/pd-decomp && ROMID=ntsc-final python tools/extract
```

Do **not** try to `make` these repos — they want a matching baserom and a MIPS toolchain. We
only read them.

**ROM requirements.** The extractor is version-specific. Ours is Perfect Dark **US V1.1 =
`ntsc-final`**, md5 `e03b088b6ac9e0080440efed07c1e40f`, already big-endian (`80371240`), so no
byte-swapping needed. Other regions need a different `ROMID` — the offset table is at the
bottom of `tools/extract`.

**PowerShell gotcha:** the ROM filename contains `[!]`, which PowerShell treats as a wildcard.
Use `-LiteralPath` on every `Get-Item` / `Copy-Item` touching it, or the path silently resolves
to nothing.

## 4. Rare compression (`1172` / `1173`)

Needed for reading any raw asset straight out of a ROM. Confirmed two independent ways — by
offset-sweeping the real GE ROM (3,229 successful inflations at `+2`, noise elsewhere), and by
`reference/pd-decomp/tools/rareunzip`, which agrees exactly:

- **GoldenEye:** magic `0x1172`, then **raw DEFLATE from byte +2**
- **Perfect Dark:** magic `0x1173`, then **raw DEFLATE from byte +5**

No custom codec. In Rust that's `flate2` raw-deflate (window bits `-15`) over
`&rom[off+2..]` / `&rom[off+5..]`.

## 5. The AI work

[DESIGN_PD_SIMULANT_AI.md](DESIGN_PD_SIMULANT_AI.md) is the porting guide. Three findings drive
it:

1. **Simulants never roll a hit chance.** They aim a real weapon in world space; accuracy
   emerges from how fast aim converges on target — a process called *zeroing*. Full tuning
   table with the original authors' field notes at `pd-decomp/src/game/bot.c:45`.
2. **Difficulty and personality are orthogonal.** Difficulty scales lethality only (reaction,
   convergence rate, speed). The 13 personality types change *target selection* only, never
   accuracy.
3. **Personality is implemented as veto predicates** over one shared targeting algorithm — each
   is ~15 lines. That maps almost directly onto our utility scorer.

**Status: ported.** The model lives in `native/crates/game/src/pdsim/` (difficulty table,
zeroing, personality, targeting) and is wired into the game behind `PD_LAB=1` — see
`native/crates/game/src/world/pd_lab.rs` for exactly what it swaps out and what it leaves
alone. Run it with:

**PowerShell** (note: `PD_LAB=1 cargo run` is bash syntax and fails here — PowerShell has no
inline env-var prefix, so the variable has to be set as its own statement):

```powershell
cd native
$env:PD_LAB = 1
cargo run --release                       # one GeneralSim, tier follows the = / - dial

$env:PD_LAB_DIFFICULTY = "meat"           # or easy / normal / hard / perfect / dark
$env:PD_LAB_TYPE       = "coward"         # or peace / prey / feud / kaze / speed / turtle / …
$env:PD_LAB_COUNT      = 2
cargo run --release
```

`$env:` variables persist for the rest of the shell session, so **clear them to get the normal
game back** (or just open a new terminal):

```powershell
Remove-Item Env:PD_LAB, Env:PD_LAB_DIFFICULTY, Env:PD_LAB_TYPE, Env:PD_LAB_COUNT -ErrorAction SilentlyContinue
```

Bash / WSL:

```sh
cd native && PD_LAB=1 cargo run --release
```

Press `G` for HUNT. The overlay shows ZERO (convergence), REACT (reaction clock) and AIM ERR
(where the barrel actually is, in degrees).

Measured in the headless testbed, and matching the PD table almost exactly: a MeatSim's worst
aim error is **29.5°** (table says 30), it opens fire at **1.50 s** (table says 1.5), and it
takes **6.5 s** to kill a stationary target that a DarkSim kills in **3.1 s** — separation
produced entirely by where the barrel is pointing, with no hit roll anywhere.

Related existing docs: [DESIGN_ENEMY_AI_MODERNIZATION.md](DESIGN_ENEMY_AI_MODERNIZATION.md).

## 6. The GoldenEye Setup Editor

[GE_SETUP_EDITOR_ANALYSIS.md](GE_SETUP_EDITOR_ANALYSIS.md) covers the binary analysis —
identification, 206 recovered dialog templates, 36 menus, the compression proof, and on-disk
format notes for `.set` / `.bgf` / `.clp`.

Reproduction scripts in [tools/ge-editor-analysis/](tools/ge-editor-analysis/): `pe_probe.py`
(PE headers, imports), `rsrc.py` (resource tree → JSON), `strs.py` (string extraction),
`fmt.py` (format hexdumps), `rom1172b.py` (the compression sweep). All target
`C:\GEEdit4\PerfectGold.exe` and the local ROMs by absolute path.

The editing ideas worth stealing, in priority order — **reference-point placement** (set a
reference, then offset from it, copy its up/target vectors onto a selection, move along its
target vector), **pad size quantisation** to fixed classes, **guard hint markers** on nav
positions (duck / drop / walk-straight-to / wait-for-lift), and **prefabs**. The first is the
strongest idea in the tool and needs no external source to implement.

## 7. Related design docs

[DESIGN.md](DESIGN.md) · [DESIGN_IDEAS.md](DESIGN_IDEAS.md) ·
[DESIGN_ENEMY_AI_MODERNIZATION.md](DESIGN_ENEMY_AI_MODERNIZATION.md) ·
[DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md) ·
[DESIGN_BASE_SCALE.md](DESIGN_BASE_SCALE.md) ·
[ASSET_INTEGRATION_PLAN.md](ASSET_INTEGRATION_PLAN.md) · [native/BUILD.md](native/BUILD.md)
