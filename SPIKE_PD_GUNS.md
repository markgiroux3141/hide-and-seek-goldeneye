# SPIKE: Perfect Dark's first-person guns and controls

Branch `spike/pd-simulant`, alongside the simulant spike (`SPIKE_PD_SIMULANTS.md`).
It is a standalone firing range where the player's weapons are Perfect Dark's own.

```
cargo run --release --bin pd_range            # the range (window)
cargo run --release --bin pd_gun_snapshot -- <outdir> [--all | weapon names]   # offscreen PNGs, no window
cargo run --release --bin pd_gun_snapshot -- <outdir> --seq <names | all>      # scripted feature sequences
```

Sequences: `smoke explosion grenade cook pinball mines knife nbomb rocket devastator superdragon crossbow slayer phoenix laptop farsight boost cloak hud`.

## What is ported

Everything is ported from the decomp (NTSC final): function by function, with `file:line` citations and in PD's units.

| Area | PD source | Rust |
|---|---|---|
| Gun + hand models, toggles, matrices | `lib/model.c` | `model.rs` |
| Animation decoder, `struct anim`, tick/merge | `lib/anim.c`, `model.c` | `animdata.rs`, `anim.rs` |
| Weapon table, funcdefs, `guncmd` scripts, gunviscmds | `invitems.c`, `gset.c` | `gset.rs` |
| Hand state machine: fire, burst/auto, reload, dry fire, switch, gun function, dual-wield alternation, gun-memory load latency | `bondgun.c` | `bgun.rs`, `bgun_state.rs` |
| On-screen placement: sway blend, crosshair swivel, aim damping, recoil, gangsta tilt, slide, Reaper spin, sniper scope, shotgun star, muzzle flash orient | `bondgun.c:3228–7885` | `bgun_pose.rs` |
| Look/turn/aim (PC port mouse aim), animation-driven walk + head bob, crouch, lean, zoom | `bondmove.c` (+ pcport), `bondwalk.c`, `bondhead.c` | `player.rs` |
| Shots from the camera through crosshair + spread, penetration, hitpos | `prop.c` `shot_create` / `shot_calculate_hits` / `hands_tick_attack` | `sim.rs` |
| Tracers, sparks, bullet holes, casings, ricochet and surface sounds | `gunfx.c`, `sparks.c`, `wallhit.c`, `casingtick.c`, `bgun_play_bg_hit_sound` | `fx.rs`, `sim.rs` |
| N64 display-list state: two-cycle combiner, RSP lighting, texgen chrome, tile shift/clamp/mirror, blender alpha, decal z, cull threading, DUALFLIP mirror, star-gunfire jitter, gun near/far 1.5/1000 | fast3d `gfx_pc.cpp`, `bondgun.c:8180` | `render.rs`, `pdgun.wgsl`, `pdfx.wgsl` |
| Default and Maian sights (R-hold aimer) | `sight.c:615`, `:1278` | `app.rs` |
| Gun smoke, bullet-hole puffs | `smoke.c`, `bondgun.c` `smoke_create_for_hand` | `smoke.rs` |
| Explosions: flare parts, damage, room flash, screen shake, scorch | `explosions.c` (NTSC table, cross-checked against `combat::pd_weapons`) | `explosions.rs` |
| Weapon objects: throw/fire makers, projectile flight/settle/stick, mines, held rocket, Slayer fly-by-wire | `propobj.c`, `bondgun.c`, `player.c:3363` | `props.rs`, `throw.rs`, `sim.rs` |
| N-Bomb storm | `nbomb.c` | `nbomb.rs` |
| Laptop Gun sentry | `propobj.c` `autogun_tick*` | `autogun.rs` |
| Farsight: x-ray (eraser, BG and prop colours), zoom blur, shots through walls, manual zoom | `bg.c:451`–`:5253`, `propobj.c:12720`, `lv.c:1462`, `prop.c:688`, `gset.c:215` | `xray.rs`, `sim.rs`, `player.rs` |
| Combat Boost (slow motion, wipe, heartbeat), RC-P120 cloak | `bondgun.c:10325`, `lv.c:1478`/`:2061`, `chr.c:2043`–`:2265`, `bondgun.c:8050` | `sim.rs`, `render.rs` |
| Framebuffer effects: Slayer interlace, static, zoom blur, fade | `bondview.c`, `player_draw_fade` | `pdpost.wgsl`, `render.rs` |
| Gun HUD: function square, name/function banners, mag/reserve gauges, counts, boost timer; ROM fonts | `bondgun.c:9542`–`:10320`, `text.c` | `hud.rs`, `font.rs` |

Assets come from `tools/pd-assets/pd_fpgun.py all` and land in `native/assets/weapons/pd_fp/`:
- 37 weapons, 95 scripts and 86 animations;
- every gun and hand model, plus the four casings;
- textures, now all PD's own decode (`pd_tex.py` gained the non-zlib codec);
- effect textures in `fx/` (`fx_textures.json`), 13 third-person prop models (`props` subcommand) and `ANIM_0434` (`extra-anims`);
- two ROM fonts copied from the decomp's assets: `fonts/numeric.bin` and `fonts/handelgothicxs.bin`.

Sounds come from `pd_sfx.py` and land in `native/assets/audio/pd/sfx/`. They play at PD's per-call pitch through the new `AudioManager::play_voice`.

## Engine changes (additive)
- `Renderer::render_with_hook` / `Renderer::gpu()`: a caller-owned pass between the world and overlay passes. `render()` behaves exactly as before.
- `AudioManager::play_voice` / `stop_voice`: playback rate, pan and stoppable loops.
- The surface is configured with `COPY_SRC` when the adapter allows it, and `PassHook` carries `color_texture` (Some when copyable). The framebuffer effects read the frame back through it.

## Scope decisions
- **In:**
  - every standard gun, single and dual;
  - secondary functions via hold-B (E / MMB);
  - melee, the burst and automatic weapons, the Reaper, shotgun and sniper zoom.
- **Also in (the deferred work, done 2026-09-26):** smoke, explosions, every thrown and fired projectile, the N-Bomb, the Laptop sentry, the Farsight, the Combat Boost, the RC-P120 cloak, and PD's gun HUD.
- **Substituted:** PD's rooms/portals, prop hit tests and room lighting. The range is axis-aligned boxes (`range.rs`), and "room light" is a slider feeding PD's `lights_set_for_room` formula. Each substitution is named at its call site. The main ones:
  - the non-sticky projectile collision is a 10 cm cylinder;
  - the throw trajectory clamp is a great-circle slerp;
  - the x-ray tessellates the range's faces into 50 cm cells, standing in for a PD room's vertex density;
  - the zoom blur's "front buffer" is last frame's picture *before* the HUD.
- **Known gaps:**
  - the engine-rendered walls don't take the room-light flash;
  - boltbeams aren't drawn;
  - homing rockets fly straight, because the range has no lock targets;
  - the sentry can't be shot;
  - the player's damage is recorded only (no health);
  - the Farsight's target locator finds no chrs (PD's locator only seeks chrs, and the range has none);
  - only the default and Maian sights are drawn (no zoom/Skedar/classic sights);
  - glass.

## Things the decomp settled that intuition got wrong
- **Joanna's default hands** are `combathandslod`, the same file as the unarmed fists (`BODY_DARK_COMBAT → FILE_GCOMBATHANDSLOD`, `modeldata/robot.c:161`). `hand_joaf1` is the red Negotiator outfit.
- **Tracers are pale:** `G_CC_BLENDIA` = lerp(shade, env, texel) with a white shade, so the orange beam texture only supplies alpha.
- **No reload with the trigger held:** an empty clip with the trigger held dry-fires (`HANDSTATE_ATTACKEMPTY`); PD reloads on release (`bondgun.c:1185`).
- **Unarmed shows nothing at rest:** the fists only appear when you punch.
- **Chrome follows the world:** texgen dots the eye-space normal with the camera's *world* right/up (`guLookAtReflect` → fast3d transposed modelview), so the reflection shifts as you turn. That is PD's look, not a bug.

## Verification
- **Tests:** `cargo test -p game --lib pd_guns`, 37 tests (plus 6 ignored `trace_*` diagnostics).
  - The original 10: shared joints, left-hand reload, a headless Falcon hit, CMP150 dry fire, naga-validated shaders.
  - One per deferred feature: smoke, explosions (and their table vs the game's port), every throwable, every fired projectile, the Slayer ride, the Phoenix shells, the sentry, the Farsight (x-ray and through-wall shots), the boost, the cloak, and the HUD (fonts, gauges, function square).
- **Visual:** `pd_gun_snapshot --all` renders every loadout weapon (49 frames, single and dual) offscreen. `--seq all` renders each feature's scripted sequence (75 frames), with the HUD composited in. Every one was checked by eye.

## State (2026-09-26)
- **Status:** the deferred list is done, built and green (`cargo test -p game --lib`: 799 pass).
  - The standard guns were playtested as "pretty much perfect".
  - Steps 1–4 (smoke through the sentry) went out with playtest briefs. Steps 5–7 (Farsight, boost/cloak, HUD) are built but not yet playtested.
- **Not committed:** the spike sits on `spike/pd-simulant` with the simulant spike's work. Commit only when the user asks.
- **Handoff rules:**
  - Hand off with `cargo build --release`.
  - Never drive the game window yourself. Check visuals with `pd_gun_snapshot` (`--all`, `--seq`), which renders offscreen and writes PNGs you can Read.
  - Frame-to-frame effects need every frame drawn. `Snap::live` renders frames without saving them.

## N64 video + CRT (branch `spike/n64-crt`)

The panel's **VIDEO** section, off by default, shows the range the way an N64 on a CRT did. Every stage can be toggled on its own. Code: `n64video.rs` / `n64video.wgsl`.

- **Decomp-sourced:** 320×220 NTSC (hi-res is 640×220, not 440×330), a 16-bit colour image, VI gamma off, the dither filter and divot on, LAN1 (anti-aliased), Bayer as the usual colour dither, `G_TF_BILERP`.
- **The chain:**
  - the world renders into a 320×220 engine scene target (`Renderer::set_scene_size`);
  - guns, PD's post effects, and the HUD with the sight rasterised into it (`n64_hud_canvas`, the real `sight_draw_aimer` / `sight_draw_maian`);
  - RDP store: RGBA5551 + Bayer;
  - VI: the dither ("restore") filter, edge AA and divot;
  - then either the flat 4:3 raster or the CRT: an RGB / S-Video / composite signal (a real YIQ subcarrier encode and decode), a gaussian beam that widens with brightness, a phosphor mask, halation, curvature and overscan.
- **3-point filtering** is in both the engine's `shader_textured.wgsl` (`Lighting.count.y`) and `pdgun.wgsl` (per level, blending levels like TRILERP).
- **From memory, not verified:** the Bayer matrix, the dither rule and the VI filters follow angrylion-rdp-plus as I remember it. They have not been checked against its source.
- **Approximated:** coverage. The RDP stores 3-bit coverage per pixel; we render one sample per pixel, so an edge pixel is the near side of a depth discontinuity (cvg 4). The world's depth is copied out before the gun pass clears it (`PdRenderer::world_depth_copy`).
- **Snapshots:**
  - `pd_gun_snapshot <out> --n64 [weapons]` renders each stage configuration over a test backdrop (gradient, stripes, colour bars);
  - `--crt-still <image>` runs only the CRT half on any image, e.g. an emulator capture, to judge the tube separately.
- **Measured traps:**
  - a gaussian chroma low-pass leaks the subcarrier (−23 dB) and stripes every flat colour; the fix is a box of exactly one subcarrier period (9 taps), tested in `composite_chroma_taps_null_the_subcarrier`;
  - scanline gaps only show at a beam σ below about 0.3 lines, because a gaussian comb's ripple is exp(−2π²σ²).
- **Playtest 1 ("too degraded"; broken grout lines on the wall tiles):** texture aliasing, not a post effect. Every PD texture carries a mip chain (`texdecompress.c:212-300`, `tex_shrink_paletted`); the engine's world textures had none, so at 320 px each pixel landed on one arbitrary texel. Fix:
  - world textures now get a box-filtered mip chain (`upload_material_texture`);
  - the normal view and the shadow pass pin level 0, so they are unchanged;
  - N64 mode blends 3-point across two LODs, like PD's TRILERP.
- **Added after playtest 1:**
  - a resolution option: 320×220, PD hi-res 640×220, and 640×440 beyond N64 on a 480-line raster;
  - a signal-sharpness slider (scales the bandwidth; the composite box stays one period long);
  - TV presets (clean RGB / S-Video / composite); the default is now S-Video.
- **Not done:** the engine walls' per-vertex lighting and fog, PD's TMEM cap on LOD count (distant textures past the last LOD still alias on real hardware), and a measured comparison against real-hardware captures.

### Invariants that bit us (don't regress)
- **GPU model lookup:** `PdRenderer` keys models by **`ModelDef.name`** (e.g. `falcon2.bin`), not the file stem.
- **Default hands:** `combathandslod` (`HAND_MODELS[0]`).
- **Empty clip + trigger held:** dry fire (`ATTACKEMPTY`); the reload happens on release. This is PD-correct.
- **Colour maths is gamma-free:** textures upload as raw `Rgba8Unorm`, the shaders work in N64 display space, and convert to linear only at the end (the swapchain is sRGB).
- **Frame order in `Sim::frame`:** input + `bgun_tick_gameplay` → camera → world ticks → `hands_tick_attack` (sets `hitpos`) → `bgun_tick_gameplay2` (beams read `hitpos`) → events. New effects that need `hitpos` or the muzzle belong after `bgun_tick_gameplay2`.
- **Build version:** NTSC final. Every `#if VERSION >= VERSION_PAL_BETA` takes the `#else`, and PAL tables are not used.
- **Uniform offsets are 256-aligned:** the post pass writes one `PostU` per pass at `i·256` into an 8-slot buffer.
- **The zoom blur reads the last frame:** `post()` copies every finished frame into `post_prev`. The x-ray smear and the boost wipe are feedback effects, so drop that copy and they become plain fades.
- **The screen is 320 PD pixels wide, with the height from the window aspect** (180 at 16:9). `c_lodscalez` is therefore 1.33 at 60° on a wide window, which is also why the Farsight's eraser sits 3.75 m out rather than 5 m. That is PD's widescreen behaviour, not a bug.
- **The boost cap is 4 quarter-ticks** (PD, and the PC port's `LV_SLOMO_TICK_CAP`). At the 60 Hz rate it changes nothing; slow motion only shows at 30 or 20 Hz.
- **X-ray clears the engine's world:** the world pass clears to black and resets depth, then draws PD's x-ray BG. Anything new drawn in the world pass must decide what it looks like in x-ray (`Sim::xray()`).
- **The HUD is drawn by the sim** (`Sim::hud`, a `Canvas` of PD pixels, redrawn every sim frame because `bgun_draw_hud` ticks its own timers). The window uploads it as a nearest-filtered texture, and the snapshot composites it CPU-side.
