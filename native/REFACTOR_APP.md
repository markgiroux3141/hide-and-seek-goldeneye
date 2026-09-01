# `crates/game/src/app.rs` — refactor plan

Written 2026-09-01 from a read of the file at 6,899 lines. Nothing here was
implemented; this is the plan to pick up later.

Companion to [REFACTORING.md](REFACTORING.md), which covered the `engine` /
`world.rs` split (since done). This one is about the app layer.

## Snapshot

| Region | Lines | Size |
|---|---|---|
| Consts, `ShopAction`, `PanelTab`, `apply_shop_theme` | 33–200 | 168 |
| `struct App` (≈50 fields) | 202–368 | 167 |
| `App::new` | 369–430 | 61 |
| `impl App` — 40 small helpers | 431–1467 | 1,037 |
| **`App::build_egui_frame`** | **1468–4257** | **2,790** |
| `play_tab_ui` + `pickup_settings_ui` + ctx | 4260–4720 | 461 |
| Radar, PD-lab overlay, row structs, small fns | 4722–5204 | 483 |
| `impl ApplicationHandler` (`resumed` 288, **`window_event` 898**) | 5205–6418 | 1,214 |
| **`App::on_key_pressed`** | 6419–6889 | 471 |

Three functions are 60% of the file. There are **no tests** in `app.rs`.

---

## The root cause

`build_egui_frame` is 2,790 lines **not because the UI is complicated**, but
because of one borrow:

```rust
let state = self.egui_state.as_mut()?;          // 1616 — &mut self, held...
let raw_input = state.take_egui_input(window);
let full_output = self.egui_ctx.run(raw_input, |ctx| { /* cannot touch self */ });
state.handle_platform_output(window, ...);      // 3800 — ...to here
```

The closure can't see `self`, so the function is forced into a
**snapshot → closure → apply** sandwich:

- **139 locals** copied out before the closure (70 of them `mut`), including deep
  clones of the level catalog, the theme rows and the swatch handles;
- **~450 lines** of `if let Some(x) = deferred { self.field = x }` after it
  (`app.rs:3810–4258`) — which is where all the real logic lives.

The file already documents this at `app.rs:1611` ("Resolved before the egui
closure, which cannot hold `&mut self`") and `app.rs:3810` ("Deferred until here
— these borrow the `World` / all of `self`, which can't happen while the `state`
borrow above is live").

That is roughly **600 lines of pure borrow-checker tax**, and it is what makes
the per-tab arms unextractable: a tab can't become a free function when its
state is twenty scattered `mut` locals owned by the caller.

### Both borrows are avoidable

`egui::Context` is `pub struct Context(Arc<RwLock<ContextImpl>>)` — a cheap
clonable handle (verified in the vendored egui 0.31.1 source,
`egui-0.31.1/src/context.rs:736`). And `egui_state` can simply be moved out for
the duration:

```rust
let window = self.window.clone()?;                 // Arc clone
let mut state = self.egui_state.take()?;           // moved out — self is free
let ctx = self.egui_ctx.clone();                   // Arc clone — self is free
let raw_input = state.take_egui_input(&window);
let full_output = ctx.run(raw_input, |ui_ctx| {
    self.shop_window(ui_ctx);                      // &mut self works now
    self.props_panel(ui_ctx);
});
state.handle_platform_output(&window, full_output.platform_output);
self.egui_state = Some(state);
```

Tabs then mutate `self` directly and the whole snapshot/apply sandwich deletes
itself.

Caveats, both minor: while the closure runs, `self.egui_state` is `None` — fine
so long as no UI code reads it (nothing does today); and a panic between the
`take` and the restore leaks it, which ends the process anyway.

**Do this first. Everything else gets cheaper or unnecessary.**

---

## Plan

### Step 0 — safety net

There are no tests in `app.rs` and the panels are only verifiable by playtest.
Before touching anything, snapshot behaviour: build release, walk all 9 tabs,
save/load a level, arm each tool. Write the list down — it is the acceptance bar
for every step below. Steps 1–3 are pure code motion and shouldn't each need a
playtest; **step 4 does**.

### Step 1 — free the borrow

As above. Mechanical, one commit, no behaviour change. Expect ~600 lines to
vanish and `build_egui_frame` to drop to ~2,200.

### Step 2 — split the file into a module tree

`app.rs` → `app/`, with no logic changes:

```
app/mod.rs        App struct, new(), run(), consts, PanelTab   (~450)
app/handler.rs    impl ApplicationHandler                       (~1,200)
app/keys.rs       on_key_pressed + key helpers                  (~500)
app/frame.rs      build_egui_frame + tick/render orchestration  (~300)
app/ui/shop.rs    the SHOP window                               (~400)
app/ui/panel.rs   the O-panel chrome + tab dispatch             (~250)
app/ui/tabs/*.rs  one file per tab (table below)
app/ui/theme.rs   the theme-editor block (3889–4258)            (~370)
app/ui/overlay.rs radar + PD-lab overlay + row structs          (~480)
```

The external surface is three items — `run()`, `PanelTab`, and the `SHOP_*`
colours, used only by `radial/mod.rs:31` and `radial/paint.rs:18` — so this is
close to zero-risk.

### Step 3 — extract the tab arms

The proven pattern is already in the file: `play_tab_ui` (`app.rs:4269`) is a
free `fn(ui, &mut cfg, &Ctx) -> (changed, start)`, and its doc comment gives
exactly this rationale ("the arm would otherwise be four hundred lines inside an
already-large closure"). After step 1 they can be plain `&mut self` methods
instead, which is simpler still.

| Arm | Lines (pre-refactor) | Size | Notes |
|---|---|---|---|
| Paint | 2922–3522 | 601 | biggest; splits into probe readout / swatch grid / actions |
| Levels | 2527–2817 | 291 | pairs with the 5 level-file helpers |
| Textures | 3523–3813 | 291 | pairs with the theme-editor block |
| Objects | 2818–2921 | 104 | |
| Tools | 2277–2375 | 99 | |
| Nav | 2458–2526 | 69 | |
| Spawns | 2416–2457 | 42 | |
| Lighting | 2377–2415 | 39 | |
| Play | 2376 | — | already delegates to `play_tab_ui` |

### Step 4 — split `window_event` (898) and `on_key_pressed` (471)

`window_event` is really eight handlers; give each its own method
(`on_mouse_left`, `on_mouse_wheel`, …) and let `RedrawRequested` (458 lines)
become an ordered call list:

```
tick_clock → poll_gamepad → apply_look → sim_steps
           → update_highlights → build_ui → render → telemetry
```

For `on_key_pressed`: the radial-menu work already made `EditorAction` +
`App::apply` the single verb seam, so most of the 55 `KeyCode` arms should
collapse into a table `fn(KeyCode, mods) -> Option<EditorAction>`, leaving only
the genuinely stateful ones (Esc's four-deep ladder, the room-key modal) as real
code.

### Step 5 (optional) — group the `App` fields

~50 fields, and the panel-specific clusters are already marked with `// ──`
banners in the struct: `ShopUi`, `ThemeUi`, `LevelsUi`, `PaintUi`, `RoomLock`,
`Telemetry`. Do this **last** — it touches every call site, and after steps 1–4
it may buy less than it costs.

---

## What to skip

- **No trait-object "panel" abstraction.** The tabs share the panel chrome and
  little else; nine `impl Panel for` blocks would be more indirection than the
  `match` costs.
- **Don't split `world/mod.rs` (3,811 lines) in the same pass.** Separate job,
  real coupling — unlike this one.

## Ordering matters

**Step 1 is the whole game.** Steps 2–4 are code motion that is much cheaper once
the fake state (139 locals) is gone. In the other order you would carefully
relocate ~600 lines you are about to delete.
