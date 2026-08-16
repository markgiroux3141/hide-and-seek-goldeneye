# PerfectGold.exe — Analysis & Replication Notes

## 1. What the binary actually is

`C:\GEEdit4\PerfectGold.exe` is the **GoldenEye Setup Editor 4.30** by SubDrag, Wreck and
Zoinkity (per the shipped `Readme.txt`). It is *not* a bespoke tool — it is the long-standing
community editor for Rare's N64 titles.

| Property | Value |
|---|---|
| Architecture | x64 (PE32+), 6 sections |
| Build timestamp | 2023-12-15 |
| Runtime | MFC140 + MSVCP140 (VS2015+ toolchain) |
| Renderer | **Direct3D 9** (`d3d9.dll` + `d3dx9_30.dll`, 37 D3DX imports) |
| UI skinning | Codejock `SkinFramework1640`, `ChartPro1730` |
| Network | WinInet — update check only |
| Debug info | **Stripped** (no PDB path in the debug directory) |
| `.text` | 26.6 MB |
| `.rsrc` | 5.1 MB |

`runwaysetupeditor.exe` is **byte-identical** (same SHA-256:
`BD7768514E93FBCADD11F5CBD391CE028428A91E061E339E413CF41A77A2EF0C`). It is the same binary
under a second name — the app branches on `argv[0]` / the `game=` ini key.

Games supported: GoldenEye 007, Perfect Dark, Diddy Kong Racing, Mickey's Speedway USA,
Jet Force Gemini, Pokemon Snap, Super Smash Bros., Shadowgate 64.

## 2. Feature map (from PE resources)

Extracted cleanly: **206 dialog templates**, **36 menus**, 20 bitmaps, 32 icons.

Largest dialogs, i.e. where the functional weight sits:

- Replace Texture (247 controls), Edit Model (201/168/167), Light Sources (151)
- Animation Editor (141), PD Pads (97), UV Editor (84), Action Blocks (83)
- Object Editor (78), Modify Presets (71), Portal Editor (36), BSP Dlg
- Light Editor / Light Baking, Visibility Editor, Tile Point Editor, Path Editor
- Vertice Coloring, Clipping Coloring, Collision Index, Triangle Bitflags

The right-click context menu in the visual editor (menu resource 191, 480 items) is the
clearest statement of the editing model:

- Pad / object placement with axis snapping (6 axis presets), rotate to N/E/S/W/NE/SE/SW/NW,
  rotate 45/90/180
- Pad size quantisation to 0x0C / 0x18 / 0x24 / 0x30 / 0x3C / 0x40
- Reference-point system: set reference, offset from reference, copy up/target vectors
  between objects, move along reference target vector
- Prefabs (create / refresh list)
- Path-set links built by click-pairs
- Guard-sim hints attached to pads: duck, drop mid-air, walk straight to, ride lift,
  wait for lift, lift ID 0–F

## 3. Verified finding: the "1172" compression

The strings gave it away — `Not 1172/1173/1F8B0800 Compressed`, `gzip.exe -f -q -9`,
`Temp\temp21990asdvb.1172`. The editor shells out to a bundled `gzip.exe` rather than
implementing the codec.

**I verified this empirically against the real ROM** (`007 - GoldenEye (USA).n64`,
12 MB, byteswapped `.v64` order, internal name `GOLDENEYE`):

Scanning for aligned `11 72` markers and sweeping the deflate start offset:

```
deflate-start offset from marker -> successful inflations (>=512 bytes)
  marker+2   : 3,229      <-- winner
  marker+3   : 2
  marker+6   : 4
  (all others: 0-3, i.e. noise)
```

**Format: `u16 magic 0x1172`, immediately followed by a raw DEFLATE stream (no zlib/gzip
wrapper, window bits -15).** 3,229 of 5,507 candidate markers inflate successfully; the
remainder are coincidental byte pairs.

Sample: ROM offset `0x0021990` inflates to 247,120 bytes — and `0x21990` is exactly the
file-table offset the editor's own warning string names (`edited via 21990/39850`).

`1173` is a second variant and `1F8B0800` is plain gzip. In Rust this is
`flate2::read::DeflateDecoder` over `&rom[off+2..]` — no custom codec needed.

## 4. On-disk format observations (from shipped samples)

**GE setup (`.set`)** — `GE\Setup\UsetupdamZ.set`, 81,808 bytes. Opens with a table of
big-endian u32 file offsets; the first 9 all land in-range, pointing at the section blocks
(objects, pads, intro, AI/path lists, etc.). Big-endian throughout, as expected for N64 data.

**GE background (`.bgf`)** — `GE\BGDataFull\449450.bgf`, 331,584 bytes. Uses **N64 segmented
addressing**: pointers appear as `0x0F......`, i.e. segment `0x0F` plus offset. Any reader
must mask the segment byte and rebase. Interleaved float triples (e.g. `C1900000 C47B4000
41200000`) are room origin / bounding data.

**PD clipping (`.clp`)** — `PD\pdclipping\bg_azt_tiles.clp`, 229,584 bytes. A flat directory
of big-endian u32 offsets (`0x00000066, 0x000001A0, ...`) indexing per-room tile blocks.

**PD setup** — separate `.set` + `.pad` file pair, unlike GE's single setup file.

## 5. The shipped plain-text data files (the real prize)

These sit next to the exe and need no reverse engineering at all:

| File | Size | Contents |
|---|---|---|
| `GE\actions.ini` | 58 KB | **Full opcode reference for the guard action-block VM** |
| `GE\writerom.txt` | 32 KB | ROM layout / injection map |
| `GE\images.txt` | 97 KB | Texture table |
| `GE\objectListing.txt` | 12 KB | Object table (file, scale, flags) |
| `GE\hittypes.txt`, `imageTypes.txt`, `guardModelListing.txt` | | Lookup tables |
| `JFG\jfgImageHeaders.txt` | 440 KB | Per-image headers |
| `ModelNodeNames\`, `Prefabs\`, `propimages\` | | Rig node names, prefabs |

`actions.ini` is documentation-grade. It defines the scripting VM the guards run —
`00` goto next label, `01` goto first label, `02` label, `03` sleep one tick, `04` end block,
`05` jump to block, `06`/`07` set/jump return block, `08` animation stop, `09` kneel,
`0A` play animation with keyframe range plus an 8-bit flag field (mirror / loop / hold last
frame / idle-pose-after / no-translation / reverse / translation x4) and a transition-blend
byte. It even records engine behaviour: *offscreen or idle guards tick every 14 game ticks
rather than every tick.*

That last detail is a genuinely useful AI-scheduling idea for our project, and it came free
from a text file.

## 6. Strategic conclusion — do NOT reverse the exe

The decisive finding is that **both games have complete, actively-maintained, open-source
decompilations**:

- GoldenEye 007 — <https://github.com/n64decomp/007> (mirror of
  `gitlab.com/kholdfuzion/goldeneye_src`). Matching-byte-for-byte C.
- Perfect Dark — `n64decomp` org, mirror of `gitlab.com/ryandwyer/perfect-dark`.

`D:\GoldenPerfectModding\decomp.txt` already contains that first URL, so this is a known path.

Every structure PerfectGold manipulates is a *game* structure, and the decomps define those
structures as readable C with real field names. Recovering the same information by
disassembling 26 MB of stripped, statically-linked MFC code would be strictly worse:

- slower, and error-prone (recovered field names are guesses)
- the exe's D3D9 fixed-function renderer is not a model we want to copy into a Rust/wgpu codebase
- the decomps carry licence clarity that a closed binary does not

The exe remains valuable as a **feature-design reference and as an oracle** — run it on a
level, export, and diff against our own output.

## 7. What is worth replicating for our project

Ranked by value-to-effort for the native Rust BUILD & HIDE engine:

1. **Pad/reference-point editing model.** The strongest idea in the tool. A "reference point"
   you set once, then offset from, copy up/target vectors from, and move along — that is a
   much better authoring primitive than free-drag gizmos, and it maps directly onto our
   existing object/prop placement. Plus axis snapping and rotate-to-compass.
2. **Pad size quantisation.** Fixed size classes rather than arbitrary scale — consistent with
   the fixed-45° decision already captured in `DESIGN_CSG_FLEXIBILITY.md`.
3. **Guard hint markers on nav positions** (duck / drop / walk-straight-to / wait-for-lift).
   This is exactly the "off-mesh link on the existing grid" idea from the reverted-navmesh
   memo — the pad-flag approach is the proven shape for it.
4. **Prefabs.** Create-from-selection, refresh-list. Cheap to add on our ECS.
5. **Action-block VM concept.** A small byte-coded behaviour script per enemy, with labels,
   jumps, a return-block register, and sleep-one-tick yielding. Worth comparing against our
   utility-decision layer — the yield/tick-budget model in particular.
6. **1172 decompression** — only if we want to pull original assets. Trivial in Rust now that
   the format is confirmed (`flate2`, raw deflate at magic+2).
7. **BSP/portal/visibility generation and light baking** — interesting but these are solved
   differently and better by modern techniques; not worth porting.

## 8. Reproduction artifacts

Scripts written during this analysis (scratchpad):

- `pe_probe.py` — PE header, debug directory, full import table
- `rsrc.py` — resource tree walker; emits `dialogs.json`, `menus.json`, `strtable.json`
- `strs.py` — string extraction + classification; emits `allstrings.txt` (49,690 strings)
- `fmt.py` — structural hexdump of `.set` / `.bgf` / `.clp` samples
- `rom1172b.py` — the compression-offset sweep that confirmed the 1172 layout
