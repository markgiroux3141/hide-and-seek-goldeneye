# Radial menu (middle-mouse) — design

**Status:** BUILT + green (640 tests) + release, awaiting playtest. Designed
2026-08-27, built 2026-08-28.

**Scope, as narrowed by the author before the build:** the ring exists in **BUILD
only**, and never while the `O` object panel is open — its only business with that
panel is the entry that opens it. There is no HUNT ring (§4's HUNT table is kept
below as the road not taken). Sticky mode is in.

## 1. The problem, measured

Every binding in the game today, read out of `app.rs::on_key_pressed`
(`native/crates/game/src/app.rs:3752`), `device_event` (`:3057`), the mouse arms
(`:3094`, `:3238`, `:3252`) and the two per-frame movement readers
(`engine/src/render/camera.rs:49`, `game/src/character.rs:100`):

### Held, per-frame (muscle memory — NOT radial candidates)
| Input | Meaning |
|---|---|
| W/A/S/D | move / fly |
| Space | rise (BUILD) / jump (HUNT) |
| Mouse move | look |
| LMB | fire (HUNT) · confirm-or-select (BUILD) · gizmo drag / place (objects) |
| RMB | free-aim (HUNT) · hold-to-look (objects panel) |
| Wheel | sizes whatever tool is armed; Shift+wheel = the V axis |
| Shift | fine 1-WT push/pull step |

### Global one-shots
| Key | Action |
|---|---|
| Esc | cancel stair → back out draw → cancel platform phase → release cursor |
| M | shop / inventory |
| O | left authoring panel (5 tabs: OBJECTS / LIGHTING / SPAWNS / TEXTURES / NAV) |
| `\` | grid ↔ textured view |
| L | flat ↔ real point lighting (BUILD preference) |
| F1–F8 | load level slot 1–8 |
| Ctrl+F1–F8 | save level slot 1–8 |
| Ctrl+Z / Ctrl+R | undo / redo (BUILD only) |
| F10 | dump hunter telemetry to `hunter_telemetry.log` |
| I | invincible |
| J | hunters on/off |
| N | invisible |
| `[` / `]` | wave size −/+ |
| G | BUILD ↔ HUNT (needs pointer grabbed) |

### HUNT only
| Key | Action |
|---|---|
| Q | cycle weapon |
| E | primary ↔ secondary weapon function |
| B | use door, else reload |
| F | detonate remote mines |
| R | next round → skip death beat → reload |
| `=` / `-` | difficulty −/+ |

### BUILD only (pointer grabbed)
| Key | Action |
|---|---|
| 1–9 | retexture the room at the crosshair with that quick scheme |
| Q | freeform draw tool |
| B / H | door tool / hole tool |
| P / R | pillar / brace placement |
| T | platform + stair-run tool |
| C | connect |
| K | block-stairs tool |
| F / V | toggle grounded / railings on the selection |
| X / Del | delete selection |
| ↑ / ↓ | grow a pending up/down stair |
| Enter | confirm stairs |
| `+`/`=` / `-` | push / pull the selected face |
| Y / Z | procedural-anim preview toggle / recoil kick |

### Objects panel open (O)
| Key | Action |
|---|---|
| T | cycle prop gizmo (Move ↔ Rotate) |
| Q / Esc | disarm placement + deselect |
| Shift+D | duplicate selected prop |
| Del / Backspace | delete selected prop |

**That is 46 distinct bindings, and eight letters mean different things
depending on mode:** `Q` (weapon cycle / draw tool / deselect), `B` (use door /
door tool), `R` (reload-or-restart / brace / redo), `F` (detonate / grounded),
`T` (platform / prop gizmo), `Z` (recoil / undo), `=`/`-` (difficulty /
push-pull). That overload — not the raw count — is the real cost.

**Middle mouse is completely unbound today.** No `MouseButton::Middle` arm
exists anywhere in the crate.

## 2. What the radial is for

1. **A discoverability layer, never a replacement.** Every key above keeps
   working exactly as it does now. Each radial slot prints its own hotkey in
   small text under the label, so using the menu teaches the key.
2. **It owns the mode-ambiguous verbs.** Tools, panel tabs, view toggles, level
   slots, weapon choice — the things where "which `R` is this?" is the actual
   friction.
3. **It does not own the flow verbs.** Push/pull, wheel sizing, fire, move,
   undo/redo stay keys-only. Ctrl+Z is not a discoverability problem, and a
   menu between you and an undo is worse than a key.
4. **Layout is fixed; entries grey out.** Slot positions never move with
   context — an inapplicable entry is dimmed with its reason ("needs a selected
   face"). Muscle memory is the entire point of a radial; a reshuffling ring
   destroys it.
5. **Toggles show their state** in the label ("Lighting: real", "Hunters: ON").

## 3. The gesture

**Hold middle mouse → flick → release.** No cursor, no travel to a corner.

- **MMB down** opens the ring, centred on the crosshair when the pointer is
  locked, or on the cursor when it is free (after Esc released the grab — the
  panels themselves suppress the ring entirely).
- **While held**, raw mouse motion feeds a *virtual pointer* vector instead of
  the camera. Angle picks the slot; the camera does not move.
- **MMB release** commits the hovered slot. Released inside the dead-zone
  radius → nothing happens (cancel).
- **Quick tap** (released inside the dead zone under ~180 ms) → **sticky mode**:
  the ring stays up, the free cursor drives it, LMB or MMB commits, Esc/RMB
  closes. This is the accessibility path and the "I want to read the labels"
  path.
- **Esc / RMB** always close without acting.

Why virtual-pointer-from-delta and not "release the pointer lock and let egui
hit-test": releasing the lock mid-HUNT is disruptive, re-grabbing snaps the
view, and `set_pointer_lock` (`app.rs:521`) also disarms every modal tool. The
delta path leaves the world exactly as it was.

### Submenus
A slot marked `▸` expands **in place**: its children become the ring, the parent
label sits in the middle, and the breadcrumb reads `Tools ▸ Openings`.
- Keep flicking outward to reach a child.
- Flick back through the centre (inside the inner radius) → up one level.
- **Depth cap: 2.** Anything needing three levels is the wrong shape for a
  radial and belongs in the O panel.
- Max 8 slots per ring (N, NE, E, SE, S, SW, W, NW); 6 preferred where it fits.

## 4. The menus

### BUILD root
| Dir | Slot | Contents |
|---|---|---|
| N | **Tools ▸** | Draw `Q` · Door `B` · Hole `H` · Pillar `P` · Brace `R` · Platform `T` · Block stairs `K` · Connect `C` |
| NE | **Selection ▸** | Push `=` · Pull `-` · Delete `X` · Grounded `F` · Railings `V` · Stairs up `↑` · Stairs down `↓` · Confirm `⏎` |
| E | **Objects ▸** | the five panel tabs — opens the `O` panel directly on the one you picked |
| SE | **Textures ▸** | the 9 quick schemes, **labelled with what they are actually bound to on this level** · Open the TEXTURES editor |
| S | **▶ Enter HUNT** `G` | down = drop into the world |
| SW | **Level ▸** | slots 1–8 with their names, "(empty)" when unused. Flick = **load**; hold Ctrl and flick = **save** — the same Ctrl convention as the F-keys |
| W | **View ▸** | Grid/textured `\` · Flat/real lighting `L` · Nav overlay · Nav validation · Proc-anim preview `Y` |
| NW | **Debug ▸** | Invincible `I` · Invisible `N` · Hunters `J` · Wave size `[` `]` · Telemetry dump `F10` |

Undo/redo are deliberately absent — see principle 3.

### The submenus, as built
| Ring | Slots |
|---|---|
| **Tools** | Draw `Q` · Door `B` · Hole `H` · Pillar `P` · Brace `R` · Platform `T` · Block stairs `K` · Connect `C` — the armed one carries the gold accent |
| **Selection** | Push `=` · Pull `-` · Delete `X` · Grounded `F` · Railings `V` · Stairs up `↑` · Stairs down `↓` · Confirm `⏎`. Dead without a selection; Confirm lights up only with a stair op pending |
| **Objects** | the five panel tabs — OBJECTS `O` / LIGHTING / SPAWNS / TEXTURES / NAV. Opens the panel *on that tab*, which is what replaces cycling the `◄ ►` arrows |
| **Textures** | one slot per digit this level actually binds, labelled with the scheme it resolves to, plus **More…** into the TEXTURES tab |
| **Level** | slots 1–8, "Load n" when the file exists and "Slot n" when it doesn't. **Hold Ctrl and every slot becomes `SAVE → n`** — the ring relabels live |
| **View** | grid/textured `\\` · flat/real lighting `L` · nav overlay · nav validation · anim preview `Y` |
| **Debug** | hunters `J` · wave `]` · invincible `I` · telemetry `F10` · invisible `N` · wave `[`. The two wave steppers are *repeating* slots — they leave the ring up so you can click again |

### The HUNT ring — not built
Kept here as the road not taken. A weapon wheel is the obvious win (`Q`-cycling a
24-gun arsenal is the worst binding in the game), but a menu in a firefight is a
liability, and the author scoped it out before the build. If it comes back:
Weapons ▸ / Fire mode `E` / Reload `R` / Detonate `F` / Back to BUILD `G` / Shop
`M` / Difficulty ▸ / Debug ▸, with no pause.

### Gamepad
Not in v1. The N64 scheme (`gamepad.rs`) has no free button — Start is already
the menu; A/B/Z/L/R and the C-buttons are all committed. If we want it later,
**hold L+R** is the only spare gesture.

## 5. Rendering

**egui, via `Painter` shapes** — not the bitmap HUD. The HUD
(`game/src/hud/mod.rs`) is a font-atlas quad builder that silently drops
unatlased glyphs, and the ring needs arcs, wedges and dimmed fills. egui is
already in the render path (`build_egui_frame`, `app.rs:641`) and already
themed gold-on-black (`apply_shop_theme`, `:115`) — the radial should reuse
that palette so it reads as the same product as the shop.

We do our own hit-testing off the virtual pointer and use egui purely as a
paint surface; we do not hand egui a `Response`-driven widget tree. That keeps
the menu working identically whether the OS cursor is locked or free.

## 6. Architecture — the one change that matters

Right now, what `B` *does* exists only inside `on_key_pressed`'s body. If the
radial re-implements it, the two drift within a month.

**Step one is an `EditorAction` enum**, and it should land as its own commit
with zero behaviour change:

```rust
enum EditorAction {
    ArmTool(Tool),          // Draw, Door, Hole, Pillar, Brace, Platform, BlockStairs, Connect
    Selection(SelectionOp), // Push, Pull, Delete, Grounded, Railings, StairUp, StairDown, Confirm
    Panel(PanelTab),
    SetScheme(usize),
    ToggleMode, ToggleGrid, ToggleLighting, ToggleNavOverlay, ToggleProcPreview,
    Invincible, Invisible, Hunters, Telemetry,
    WaveSize(i32), Difficulty(i32),
    SaveSlot(u8), LoadSlot(u8),
    SelectWeapon(usize), ToggleWeaponFn, Reload, Detonate, UseOrReload,
    OpenShop,
}

impl App { fn apply(&mut self, a: EditorAction) { /* … */ } }
```

`on_key_pressed` becomes a `KeyCode → Option<EditorAction>` map plus
`self.apply(action)`; the radial becomes a `slot → EditorAction` map plus the
same call. One implementation, two front-ends.

Each arm must preserve what the current handler does *around* the World call —
this is not uniform, and it is where a careless extraction breaks things:
- geometry mutations go through `with_undo` (or `with_undo_many` for the draw
  tool, which returns several meshes) and **upload every returned mesh**;
- arming a tool calls `refresh_highlight()` only when the tool ended up
  *disarmed* (the stale-ghost pattern repeated at `:4040`, `:4070`, `:4118`);
- `set_scheme_at_crosshair` when locked vs `set_scheme_along(ray)` when free —
  both already exist because the TEXTURES tab hit this exact problem (`:3145`).

## 7. What was built

All of it, in one pass:

| Where | What |
|---|---|
| [`radial/mod.rs`](native/crates/game/src/radial/mod.rs) | `EditorAction` / `Tool` / `SelectionOp`, the menu tables, and the state machine — no window, no `World`, no egui |
| [`radial/paint.rs`](native/crates/game/src/radial/paint.rs) | the egui foreground layer: backdrop, hub, chips, flick line |
| [`radial/tests.rs`](native/crates/game/src/radial/tests.rs) | 24 tests over the gestures and the tables |
| [`app.rs`](native/crates/game/src/app.rs) | `App::apply` + `arm_tool` / `selection_op` / `dump_telemetry`, and the event wiring |

**Every routed key now goes through `App::apply` too** — 19 of them. The key
handler kept its exact dispatch order and gating; only the bodies moved. That is
the anti-drift guarantee, and it is why the 540-test world suite passing is
meaningful here rather than incidental.

Two additions the design didn't anticipate:

* **`set_pointer_lock_keep_tools`.** `set_pointer_lock(false)` cancels every armed
  tool, which would have meant that opening the ring to flip the grid view threw
  away the door tool you had armed. Freeing the cursor and disarming tools turned
  out to be two things wearing one function; they are now separable, and every
  existing caller kept the old behaviour.
* **Actions run *after* the lock is restored.** `commit_radial` restores the
  pointer lock first and only then applies. That is what lets `SetScheme` use the
  camera crosshair, exactly as `1`–`9` do, instead of trying to shoot a pick ray
  through wherever the menu happened to leave the cursor.

## 8. Gotchas already visible in the code

1. **`build_egui_frame` cannot borrow `&mut self`.** Every piece of state the UI
   reads is snapshotted *before* the closure (`app.rs:641`–`:760`, with the big
   comment explaining why). The radial's labels — weapon names, slot names,
   scheme labels, toggle states — must be gathered the same way, and the chosen
   action collected out of the closure the way `ShopAction` already is.
2. **Raw motion must be diverted, not doubled.** `device_event` (`:3059`) is the
   single funnel — when the ring is open, feed the delta to the radial and skip
   `input.add_mouse`. One `if`, one place.
3. **`egui_consumed` gating.** The new `MouseButton::Middle` arm sits beside the
   Left/Right arms and must return early on `egui_consumed`, or a middle-click
   on the shop opens the ring behind it.
4. **Esc already has a four-deep priority ladder** (`:3755`). The radial goes on
   *top* of it: if the ring is open, Esc closes the ring and nothing else.
5. **The `Vec<RegionMesh>` upload trap** — noted during the draw-tool work: a
   tool that changes several regions must upload all of them. The
   `EditorAction` extraction is the moment to make that uniform rather than
   per-call-site.

## 9. Settled

- **Sticky mode: quick tap**, `TAP_SECS = 0.18`. Released in the dead zone under
  that → the ring stays up on the real cursor. Also entered by releasing on a
  submenu (otherwise the ring would be stranded with nothing driving it) and by
  opening with the cursor already free.
- **Textures ring: the bound digits only**, built from this level's own bindings
  and labelled with what they resolve to, plus **More…** into the panel. The
  ~390-theme library is the wrong shape for a ring.
- **HUNT ring: not built** (see §4).

## 10. For the playtest

The things worth deliberately trying, because they are where the design made a
call rather than followed one:

1. **Flick sensitivity.** `SENSITIVITY = 0.55` maps raw mouse delta to ring
   points. If a comfortable flick undershoots the chips or slams into the clamp,
   this is the one number to move.
2. **`EXPAND_R` for descending.** Push a `▸` chip outward to open it. Too close
   and you descend by accident brushing past; too far and it feels like work.
3. **Does hold-flick or tap-sticky win?** If the tap becomes the only gesture
   anyone uses, the held path is worth simplifying rather than keeping both.
4. **Ctrl on the Level ring.** Hold Ctrl with it open — every slot should
   relabel to `SAVE → n` live, and empty slots should go from dead to live.
