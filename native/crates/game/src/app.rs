//! Phase 1 app shell: a winit window driving the renderer over a [`World`].
//! Builds one CSG room, flies a first-person camera through it (original editor
//! tuning), and authors live — crosshair face-pick + push/pull re-evaluates the
//! region and updates its mesh + collider in place.
//!
//! Controls (match `src/scene/camera.js` + `src/tools/indoorKeys.js`):
//!   click      grab cursor (pointer lock)      Esc     release cursor
//!   mouse      look                            W/A/S/D move    Space rise
//!   `+`/`=`    push face (carve inward)        `-`     pull face (extend)
//!   Shift+push/pull → fine 1-WT step (default 4).

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use engine::platform::frame::FrameClock;
use engine::platform::input::InputState;
use engine::render::renderer::{EguiFrame, Renderer};
use crate::gamepad::N64Pad;
use crate::world::{World, PUSH_PULL_STEP};

/// Fixed simulation rate (120 Hz), sim-step cap per frame (8), and render FPS
/// cap (240) — driven by the engine [`FrameClock`]. Fixed-timestep sim keeps
/// physics/movement frame-rate independent; the FPS cap stops the loop burning
/// the GPU rendering frames nobody sees.
const SIM_HZ: f32 = 120.0;
const MAX_SUBSTEPS: u32 = 8;
const MAX_FPS: u32 = 240;

/// A purchase the shop UI requested this frame, collected during the egui pass and
/// applied to the `World` afterwards (the UI closure can't hold a `&mut World`).
enum ShopAction {
    /// Buy the weapon at this `config::WEAPONS` index.
    Weapon(usize),
    /// Buy an ammo refill for the weapon at this index.
    Ammo(usize),
}

// ─── Shop palette (GoldenEye gold-on-black spy-terminal look) ──────────────────
/// Signature gold accent — headings, borders, buy buttons, selection.
const SHOP_GOLD: egui::Color32 = egui::Color32::from_rgb(224, 184, 74);
/// A muted gold for section headers / secondary accents.
const SHOP_GOLD_DIM: egui::Color32 = egui::Color32::from_rgb(150, 122, 60);
/// Primary readable body text.
const SHOP_TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 228);
/// Dimmed text — unaffordable prices / disabled hints.
const SHOP_DIM: egui::Color32 = egui::Color32::from_rgb(110, 110, 118);

/// Apply the shop's gold-on-black theme to the egui context (once at startup). Only
/// egui (the menus) is affected — the in-world bitmap HUD is untouched.
fn apply_shop_theme(ctx: &egui::Context) {
    use egui::{Color32, FontFamily, FontId, Stroke, TextStyle};
    let bg = Color32::from_rgb(14, 15, 18);
    let bg_light = Color32::from_rgb(30, 32, 38);
    let bg_hover = Color32::from_rgb(46, 48, 55);

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(SHOP_TEXT);
    v.window_fill = bg;
    v.panel_fill = bg;
    v.window_stroke = Stroke::new(1.0, SHOP_GOLD);
    v.hyperlink_color = SHOP_GOLD;
    v.selection.bg_fill = SHOP_GOLD.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, SHOP_GOLD);
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.inactive.bg_fill = bg_light;
    v.widgets.inactive.weak_bg_fill = bg_light;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, SHOP_TEXT);
    v.widgets.hovered.bg_fill = bg_hover;
    v.widgets.hovered.weak_bg_fill = bg_hover;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, SHOP_GOLD);
    v.widgets.active.bg_fill = SHOP_GOLD;
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.text_styles
            .insert(TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional));
        s.text_styles
            .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
        s.text_styles
            .insert(TextStyle::Button, FontId::new(15.0, FontFamily::Proportional));
        s.text_styles
            .insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    });
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    world: Option<World>,
    input: InputState,
    /// USB-N64 gamepad driver (GoldenEye solitaire scheme), or `None` if no input
    /// subsystem is available — keyboard/mouse still work either way.
    gamepad: Option<N64Pad>,
    /// Fixed-timestep + frame-pacing clock (engine primitive).
    clock: FrameClock,
    // Throttled frame-time telemetry.
    fps_frames: u32,
    fps_elapsed: f32,
    fps_worst_ms: f32,
    /// Last player health/armor uploaded to the radial-HUD texture, so it's only
    /// re-baked + re-uploaded when they change. `-1` forces the first upload.
    last_hud_health: f32,
    last_hud_armor: f32,
    /// egui immediate-mode UI context (the shop / inventory panels). Persistent
    /// across frames (holds widget + layout state).
    egui_ctx: egui::Context,
    /// egui ↔ winit event translation + per-frame input gathering. `None` until the
    /// window exists (created in `resumed`, needs the window for DPI + viewport).
    egui_state: Option<egui_winit::State>,
    /// Whether the shop/inventory menu is open (centered egui panel). Toggled by the
    /// N64 **Start** button or the **M** key.
    shop_open: bool,
    /// The weapon index highlighted in the shop list — drives the left preview pane
    /// (its rotating 3D model + stats). Defaults to the first weapon.
    shop_selected: usize,
    /// Turntable spin angle (radians) for the shop's 3D weapon preview; advances
    /// while the shop is open.
    shop_preview_angle: f32,
    /// Pointer-lock state captured when the shop opened, restored when it closes —
    /// so opening the menu frees the cursor and closing hands control back exactly
    /// as it was (grabbed in gameplay, free in the editor).
    lock_before_shop: bool,
    /// Whether the left object-placement panel is open (BUILD authoring). Toggled by
    /// the **O** key. Frees the cursor while open (like the shop) so its list is
    /// clickable and you can aim the placement crosshair at the floor.
    props_open: bool,
    /// Catalog index of the prop selected in the panel (drives the 3D preview + arms
    /// placement), or `None` if nothing is selected.
    props_selected: Option<usize>,
    /// Turntable spin (radians) for the panel's 3D prop preview; advances while open.
    props_preview_angle: f32,
    /// Pointer-lock state captured when the object panel opened, restored on close
    /// (mirrors [`Self::lock_before_shop`]).
    lock_before_props: bool,
    /// Last known mouse-cursor position (physical pixels), tracked from
    /// `CursorMoved`. Used to unproject a world pick ray for prop placement while the
    /// object panel has the cursor free.
    cursor_pos: (f32, f32),
}

impl App {
    fn new() -> Self {
        App {
            window: None,
            renderer: None,
            world: None,
            input: InputState::default(),
            gamepad: N64Pad::new(),
            clock: FrameClock::new(SIM_HZ, MAX_SUBSTEPS, MAX_FPS),
            fps_frames: 0,
            fps_elapsed: 0.0,
            fps_worst_ms: 0.0,
            last_hud_health: -1.0,
            last_hud_armor: -1.0,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            shop_open: false,
            shop_selected: 0,
            shop_preview_angle: 0.0,
            lock_before_shop: false,
            props_open: false,
            props_selected: None,
            props_preview_angle: 0.0,
            lock_before_props: false,
            cursor_pos: (0.0, 0.0),
        }
    }
}

impl App {
    /// Upload a region's textured mesh + scheme to the renderer (after an edit or
    /// at startup).
    fn upload(&mut self, rm: &crate::world::RegionMesh) {
        if let Some(r) = self.renderer.as_mut() {
            r.set_region_textured(rm.id, &rm.mesh);
        }
    }

    /// Begin a weapon switch (HUNT). This only kicks off the lower→raise dip; the
    /// mesh actually swaps at the bottom of the dip inside `World::combat_step`,
    /// which flips `models_dirty` — drained each frame below via
    /// [`Self::upload_weapon_meshes`] to re-upload the new gun/muzzle to the GPU.
    fn begin_weapon_switch(&mut self) {
        if let Some(world) = self.world.as_mut() {
            world.begin_weapon_switch();
        }
    }

    /// Re-upload the active weapon's gun + muzzle-flash meshes to the renderer,
    /// replacing the GPU viewmodel/muzzle in place (a weapon with no muzzle, e.g.
    /// the sniper, keeps the previous GPU mesh but stays hidden via its `None`
    /// transform each frame). Called after a switch swaps the meshes.
    fn upload_weapon_meshes(&mut self) {
        let (Some(world), Some(renderer)) = (self.world.as_ref(), self.renderer.as_mut()) else {
            return;
        };
        if let Some(g) = world.gun_model() {
            renderer.upload_viewmodel(g);
        }
        if let Some(m) = world.muzzle_model() {
            renderer.upload_muzzle(m);
        }
    }

    /// Save the current editable level to numbered quick-slot `slot`.
    fn save_slot(&mut self, slot: u8) {
        let Some(world) = self.world.as_ref() else {
            return;
        };
        match world.save_slot(slot) {
            Ok(path) => log::info!("saved level → slot {slot} ({})", path.display()),
            Err(e) => log::warn!("save to slot {slot} failed: {e}"),
        }
    }

    /// Load numbered quick-slot `slot`, replacing the editable geometry and
    /// re-uploading every region + structures mesh (stale ones cleared).
    fn load_slot(&mut self, slot: u8) {
        let meshes = match self.world.as_mut() {
            Some(world) => {
                // Snapshot first so an accidental slot-load is undoable; commit it
                // only once the load succeeds (a failed load leaves state as-is).
                let snap = world.snapshot();
                match world.load_slot(slot) {
                    Ok(meshes) => {
                        world.commit_snapshot(snap);
                        meshes
                    }
                    Err(e) => {
                        log::warn!("load slot {slot} failed: {e}");
                        return;
                    }
                }
            }
            None => return,
        };
        for rm in &meshes {
            self.upload(rm);
        }
        // Selection was cleared by the load — drop any lingering highlight.
        self.refresh_highlight();
    }

    /// Push the current selection's highlight quad to the renderer.
    fn refresh_highlight(&mut self) {
        if let (Some(world), Some(renderer)) = (self.world.as_ref(), self.renderer.as_mut()) {
            let mesh = world.selection_face_mesh();
            renderer.set_highlight(mesh.as_ref());
        }
    }

    fn set_pointer_lock(&mut self, locked: bool) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if locked {
            // Locked is ideal (FPS); fall back to Confined where unsupported.
            let ok = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
                .is_ok();
            window.set_cursor_visible(false);
            self.input.pointer_locked = ok;
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
            self.input.pointer_locked = false;
            // Releasing the cursor cancels any armed tool and clears its ghost.
            if let Some(world) = self.world.as_mut() {
                world.cancel_opening();
                world.cancel_place();
                world.cancel_platform_tool();
            }
            self.refresh_highlight();
        }
    }

    /// Toggle the shop/inventory menu. Opening it frees the cursor (so the panel is
    /// clickable) after remembering the current lock state; closing restores that
    /// state, handing control back to gameplay/editor exactly as it was.
    fn toggle_shop(&mut self) {
        self.shop_open = !self.shop_open;
        if self.shop_open {
            self.lock_before_shop = self.input.pointer_locked;
            self.set_pointer_lock(false);
        } else {
            self.set_pointer_lock(self.lock_before_shop);
        }
    }

    /// Toggle the left object-placement panel (BUILD). Opening frees the cursor (so
    /// its list is clickable and you can aim the floor crosshair); closing restores
    /// the prior lock, disarms any armed prop, and clears the selection.
    fn toggle_props(&mut self) {
        self.props_open = !self.props_open;
        if self.props_open {
            self.lock_before_props = self.input.pointer_locked;
            self.set_pointer_lock(false);
        } else {
            self.set_pointer_lock(self.lock_before_props);
            if let Some(w) = self.world.as_mut() {
                w.cancel_prop_placement();
                w.deselect_prop();
            }
            self.props_selected = None;
        }
    }

    /// Unproject the current mouse-cursor position into a world pick ray
    /// `(origin, dir)` using the active view-projection. `None` before the window /
    /// world / renderer exist or if the window has zero area. Drives prop placement:
    /// the object panel frees the cursor, so props are mouse-picked onto the floor
    /// rather than aimed with the (frozen) camera crosshair.
    fn mouse_world_ray(&self) -> Option<(glam::Vec3, glam::Vec3)> {
        let window = self.window.as_ref()?;
        let world = self.world.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let (mx, my) = self.cursor_pos;
        // Pixels → NDC (wgpu clip: x,y in [-1,1] with y up; z near=0, far=1).
        let nx = 2.0 * mx / size.width as f32 - 1.0;
        let ny = 1.0 - 2.0 * my / size.height as f32;
        let inv = world.view_proj(renderer.aspect()).inverse();
        let unproject = |z: f32| {
            let p = inv * glam::Vec4::new(nx, ny, z, 1.0);
            p.truncate() / p.w
        };
        let near = unproject(0.0);
        let dir = (unproject(1.0) - near).normalize_or_zero();
        (dir != glam::Vec3::ZERO).then_some((near, dir))
    }

    /// Build this frame's egui UI (the shop/inventory menu) and tessellate it into an
    /// [`EguiFrame`] for the renderer to paint. Returns `None` before the window/egui
    /// state exist.
    ///
    /// Buys are collected into a list *during* the UI pass and applied to the `World`
    /// afterwards, because the egui closure can't also hold a `&mut World`. State the
    /// UI reads (credits, ownership, ammo, prices) is snapshotted up front for the
    /// same reason.
    fn build_egui_frame(&mut self) -> Option<EguiFrame> {
        let window = self.window.as_ref()?;
        let state = self.egui_state.as_mut()?;
        let raw_input = state.take_egui_input(window);

        let shop_open = self.shop_open;
        let credits = self.world.as_ref().map(|w| w.credits()).unwrap_or(0);
        // egui handle to the offscreen 3D weapon preview (rendered below in the render
        // block). Read here so the closure can draw it as an image.
        let preview_tex = self.renderer.as_ref().map(|r| r.weapon_preview_texture_id());
        // Snapshot each weapon's shop state so the closure needs no `World` borrow.
        let rows: Vec<ShopRow> = match (shop_open, self.world.as_ref()) {
            (true, Some(world)) => (0..world.weapon_count())
                .map(|i| {
                    let name = crate::combat::config::WEAPONS[i].name;
                    ShopRow {
                        idx: i,
                        name,
                        price: crate::shop::weapon_price(name),
                        ammo_price: crate::shop::ammo_price(name),
                        owned: world.owns_weapon(i),
                        reserve: world.weapon_ammo(i).map(|(_, r)| r).unwrap_or(0),
                        active: world.active_weapon_index() == i,
                    }
                })
                .collect(),
            _ => Vec::new(),
        };

        let selected = self.shop_selected.min(rows.len().saturating_sub(1));
        let mut actions: Vec<ShopAction> = Vec::new();
        let mut new_selected: Option<usize> = None;
        let mut close = false;

        // Object-placement panel snapshot + deferred outputs (same borrow discipline
        // as the shop: read state up front, collect the pick/close, apply after).
        let props_open = self.props_open;
        let prop_sel = self.props_selected;
        let prop_selected = self.world.as_ref().map(|w| w.selected_prop().is_some()).unwrap_or(false);
        let placing_prop = self.world.as_ref().map(|w| w.is_placing_prop()).unwrap_or(false);
        let gizmo_label = self.world.as_ref().map(|w| w.prop_gizmo_label()).unwrap_or("Move");
        let mut new_prop_selected: Option<usize> = None;
        let mut close_props = false;
        let mut ground_prop = false;
        let mut go_neutral = false;

        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            if shop_open {
            // Centered, fixed menu panel (anchored so it stays centered on resize;
            // no OS-style title bar — we draw our own gold header).
            egui::Window::new("SHOP")
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                // A fixed, centered box — without this the scrolling weapon list
                // grabs the whole window height and stretches the panel down-screen.
                .fixed_size(egui::vec2(600.0, 400.0))
                .show(ctx, |ui| {
                    // Header: gold title + right-aligned credit balance.
                    ui.horizontal(|ui| {
                        ui.heading(egui::RichText::new("ARMORY").color(SHOP_GOLD).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.heading(
                                egui::RichText::new(format!("${credits}")).color(SHOP_GOLD),
                            );
                            ui.label(egui::RichText::new("CREDITS").small().color(SHOP_DIM));
                        });
                    });
                    ui.separator();

                    ui.horizontal_top(|ui| {
                        // ── Left: preview pane (stats now; 3D model soon) ──
                        ui.vertical(|ui| {
                            ui.set_width(190.0);
                            let sel = &crate::combat::config::WEAPONS[selected];
                            ui.group(|ui| {
                                ui.set_min_size(egui::vec2(174.0, 174.0));
                                ui.vertical_centered(|ui| match preview_tex {
                                    Some(tex) => {
                                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                            tex,
                                            egui::vec2(168.0, 168.0),
                                        )));
                                    }
                                    None => {
                                        ui.add_space(70.0);
                                        ui.label(
                                            egui::RichText::new("3D PREVIEW")
                                                .color(SHOP_GOLD_DIM)
                                                .strong(),
                                        );
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(sel.name).color(SHOP_GOLD).strong());
                            let kind = if sel.automatic { "AUTO" } else { "SEMI" };
                            ui.label(format!("DMG    {}", sel.damage as i32));
                            ui.label(format!("RANGE  {} m", sel.range as i32));
                            ui.label(format!("MAG    {}", sel.magazine_size));
                            ui.label(format!("TYPE   {kind}"));
                        });

                        ui.separator();

                        // ── Right: categorized, scrollable weapon list ──
                        ui.vertical(|ui| {
                            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                                ui.set_width(320.0);
                                let mut cur_cat = "";
                                for row in &rows {
                                    let cat = crate::shop::weapon_category(row.name);
                                    if cat != cur_cat {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(cat)
                                                .small()
                                                .strong()
                                                .color(SHOP_GOLD_DIM),
                                        );
                                        cur_cat = cat;
                                    }
                                    ui.horizontal(|ui| {
                                        // Selectable name (▶ marks the equipped weapon).
                                        let name = if row.active {
                                            format!("▶ {}", row.name)
                                        } else {
                                            row.name.to_string()
                                        };
                                        if ui
                                            .selectable_label(row.idx == selected, name)
                                            .clicked()
                                        {
                                            new_selected = Some(row.idx);
                                        }
                                        // Right-aligned status + action.
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if row.owned {
                                                    let afford = credits >= row.ammo_price;
                                                    if ui
                                                        .add_enabled(
                                                            afford,
                                                            egui::Button::new("+Ammo"),
                                                        )
                                                        .on_hover_text(format!(
                                                            "{} rounds · ${}",
                                                            crate::combat::config::WEAPONS
                                                                [row.idx]
                                                                .magazine_size
                                                                * crate::shop::AMMO_MAGS_PER_BUY,
                                                            row.ammo_price
                                                        ))
                                                        .clicked()
                                                    {
                                                        actions.push(ShopAction::Ammo(row.idx));
                                                    }
                                                    let tag = if row.active {
                                                        "EQUIPPED"
                                                    } else {
                                                        "OWNED"
                                                    };
                                                    ui.label(
                                                        egui::RichText::new(tag)
                                                            .small()
                                                            .color(SHOP_GOLD),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "x{}",
                                                            row.reserve
                                                        ))
                                                        .small()
                                                        .color(SHOP_DIM),
                                                    );
                                                } else {
                                                    let afford = credits >= row.price;
                                                    let buy = egui::Button::new(
                                                        egui::RichText::new("BUY")
                                                            .color(egui::Color32::BLACK),
                                                    )
                                                    .fill(if afford {
                                                        SHOP_GOLD
                                                    } else {
                                                        SHOP_DIM
                                                    });
                                                    if ui.add_enabled(afford, buy).clicked() {
                                                        actions.push(ShopAction::Weapon(row.idx));
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "${}",
                                                            row.price
                                                        ))
                                                        .color(if afford {
                                                            SHOP_TEXT
                                                        } else {
                                                            SHOP_DIM
                                                        }),
                                                    );
                                                }
                                            },
                                        );
                                    });
                                }
                            });
                        });
                    });

                    ui.separator();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("CLOSE").clicked() {
                            close = true;
                        }
                        ui.label(
                            egui::RichText::new("Start / M to close")
                                .small()
                                .color(SHOP_DIM),
                        );
                    });
                });
            } // end if shop_open

            if props_open {
                egui::SidePanel::left("objects_panel")
                    .resizable(false)
                    .default_width(224.0)
                    .show(ctx, |ui| {
                        ui.add_space(4.0);
                        ui.heading(egui::RichText::new("OBJECTS").color(SHOP_GOLD).strong());
                        ui.separator();
                        // Live 3D turntable of the selected prop (same offscreen
                        // preview target the shop uses; rendered in the render block).
                        if let Some(def) = prop_sel.and_then(|i| crate::props::CATALOG.get(i)) {
                            ui.group(|ui| {
                                ui.set_min_size(egui::vec2(196.0, 196.0));
                                ui.vertical_centered(|ui| match preview_tex {
                                    Some(tex) => {
                                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                            tex,
                                            egui::vec2(188.0, 188.0),
                                        )));
                                    }
                                    None => {
                                        ui.add_space(84.0);
                                        ui.label(
                                            egui::RichText::new("3D PREVIEW")
                                                .color(SHOP_GOLD_DIM)
                                                .strong(),
                                        );
                                    }
                                });
                            });
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(def.name).color(SHOP_GOLD).strong());
                        }
                        // Leave placement / clear selection so clicks select existing
                        // props (also the Q / Esc key).
                        if placing_prop || prop_selected {
                            let label = if placing_prop {
                                "Stop placing (Q)"
                            } else {
                                "Deselect (Q)"
                            };
                            if ui.button(label).clicked() {
                                go_neutral = true;
                            }
                        }
                        ui.add_space(6.0);
                        // Palette: catalog grouped by category; click to arm placement.
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            ui.set_width(206.0);
                            for &cat in crate::props::PropCategory::ALL {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(cat.label())
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                for (i, def) in crate::props::CATALOG.iter().enumerate() {
                                    if def.category != cat {
                                        continue;
                                    }
                                    if ui
                                        .selectable_label(prop_sel == Some(i), def.name)
                                        .clicked()
                                    {
                                        new_prop_selected = Some(i);
                                    }
                                }
                            }
                        });
                        // Edit controls for the selected prop (Esc to deselect).
                        if prop_selected {
                            ui.separator();
                            ui.label(
                                egui::RichText::new("SELECTED")
                                    .small()
                                    .strong()
                                    .color(SHOP_GOLD_DIM),
                            );
                            ui.label(format!("Gizmo: {gizmo_label}  (T switches)"));
                            ui.label(
                                egui::RichText::new(
                                    "drag handles · Ctrl = snap · Shift+D = duplicate · RMB = look",
                                )
                                .small()
                                .color(SHOP_DIM),
                            );
                            if ui.button("Ground").clicked() {
                                ground_prop = true;
                            }
                        }
                        ui.separator();
                        ui.label(
                            egui::RichText::new("Click floor to place · click a prop to edit · O closes")
                                .small()
                                .color(SHOP_DIM),
                        );
                        if ui.button("CLOSE").clicked() {
                            close_props = true;
                        }
                    });
            }
        });

        state.handle_platform_output(window, full_output.platform_output);
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let frame = EguiFrame {
            textures_delta: full_output.textures_delta,
            paint_jobs,
            pixels_per_point: full_output.pixels_per_point,
        };

        // Deferred until here — these borrow the `World` / all of `self`, which can't
        // happen while the `state` (`&mut self.egui_state`) borrow above is live.
        if let Some(sel) = new_selected {
            self.shop_selected = sel;
        }
        if let Some(world) = self.world.as_mut() {
            for action in &actions {
                match *action {
                    ShopAction::Weapon(i) => {
                        world.buy_weapon(i);
                    }
                    ShopAction::Ammo(i) => {
                        world.buy_ammo(i);
                    }
                }
            }
        }
        if close {
            self.toggle_shop();
        }
        // Object panel: apply the selection (arms placement of that prop on the
        // World) + the close, after the `state` borrow ends.
        if let Some(sel) = new_prop_selected {
            self.props_selected = Some(sel);
            if let (Some(world), Some(def)) =
                (self.world.as_mut(), crate::props::CATALOG.get(sel))
            {
                world.arm_prop_placement(def.mesh);
            }
        }
        if ground_prop {
            if let Some(world) = self.world.as_mut() {
                world.ground_selected_prop();
            }
        }
        if go_neutral {
            if let Some(world) = self.world.as_mut() {
                world.cancel_prop_placement();
                world.deselect_prop();
            }
        }
        if close_props {
            self.toggle_props();
        }
        Some(frame)
    }
}

/// Model-space AABB `(min, max)` of a textured model, from its raw vertices — used
/// to anchor/ground a placed prop (and, later, to size its collider). Zero box for
/// an empty model.
fn model_aabb(model: &engine::assets::textured_model::TexturedModel) -> (glam::Vec3, glam::Vec3) {
    let mut min = glam::Vec3::splat(f32::INFINITY);
    let mut max = glam::Vec3::splat(f32::NEG_INFINITY);
    for v in &model.vertices {
        let p = glam::Vec3::from_array(v.pos);
        min = min.min(p);
        max = max.max(p);
    }
    if model.vertices.is_empty() {
        (glam::Vec3::ZERO, glam::Vec3::ZERO)
    } else {
        (min, max)
    }
}

/// A snapshot of one weapon's shop state for a frame's UI (avoids borrowing the
/// `World` inside the egui closure).
struct ShopRow {
    idx: usize,
    name: &'static str,
    price: u32,
    ammo_price: u32,
    owned: bool,
    reserve: u32,
    active: bool,
}

/// Map a number-row / numpad digit key to its '1'..'9' char (for scheme keys).
fn digit_char(code: KeyCode) -> Option<char> {
    Some(match code {
        KeyCode::Digit1 | KeyCode::Numpad1 => '1',
        KeyCode::Digit2 | KeyCode::Numpad2 => '2',
        KeyCode::Digit3 | KeyCode::Numpad3 => '3',
        KeyCode::Digit4 | KeyCode::Numpad4 => '4',
        KeyCode::Digit5 | KeyCode::Numpad5 => '5',
        KeyCode::Digit6 | KeyCode::Numpad6 => '6',
        KeyCode::Digit7 | KeyCode::Numpad7 => '7',
        KeyCode::Digit8 | KeyCode::Numpad8 => '8',
        KeyCode::Digit9 | KeyCode::Numpad9 => '9',
        _ => return None,
    })
}

/// Map a function key F1–F8 to a level quick-slot number 1–8.
fn slot_for_fkey(code: KeyCode) -> Option<u8> {
    Some(match code {
        KeyCode::F1 => 1,
        KeyCode::F2 => 2,
        KeyCode::F3 => 3,
        KeyCode::F4 => 4,
        KeyCode::F5 => 5,
        KeyCode::F6 => 6,
        KeyCode::F7 => 7,
        KeyCode::F8 => 8,
        _ => return None,
    })
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("BUILD & HIDE (native)")
            .with_inner_size(winit::dpi::LogicalSize::new(1600.0, 900.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()));

        // egui input/event bridge — needs the window (DPI + viewport). The painter
        // lives in the renderer; this half gathers input + translates winit events.
        self.egui_state = Some(egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            None, // native pixels-per-point: let egui read it from the window
            None, // theme: follow the system
            None, // max texture side: egui default
        ));
        apply_shop_theme(&self.egui_ctx);

        // Build the world, upload its initial region meshes.
        let mut world = World::new();
        // Pin the difficulty dial to a fixed level at boot so it doesn't have to be
        // managed by hand while evaluating the AI (the `=`/`-` keys still nudge it).
        // Max = the full expression of the lethality/health/evasion + aim-dodge work.
        world.set_difficulty(crate::world::DIFFICULTY_MAX);
        // Restore a small pack at boot (the code default stays at duel = 1 so the
        // duel-mode tests are unaffected) — the coordinated AI (flanking, squad
        // suppression, cover) only reads with more than one hunter on the field.
        world.set_wave_size(crate::world::PLAYTEST_WAVE_SIZE);
        for rm in world.initial_meshes() {
            renderer.set_region_textured(rm.id, &rm.mesh);
        }
        // Optional: boot straight into a saved level slot (`LOAD_SLOT=N`), so a
        // generated level can be explored immediately without pressing F-keys.
        // Starts in BUILD (fly) mode; press G for HUNT (FPS), I for invincible.
        if let Some(slot) = std::env::var("LOAD_SLOT")
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
        {
            match world.load_slot(slot) {
                Ok(meshes) => {
                    for rm in &meshes {
                        renderer.set_region_textured(rm.id, &rm.mesh);
                    }
                    log::info!("booted into level slot {slot}");
                }
                Err(e) => log::warn!("LOAD_SLOT {slot} failed: {e}"),
            }
        }
        // B1: upload every skinned character body once (geometry + textures) — one GPU
        // mesh per body id; each hunter's pose + body selection is driven per frame.
        for (i, m) in world.character_models().iter().enumerate() {
            renderer.upload_character(i, m);
        }
        // Player Combat P1: upload the weapon viewmodel once (gun geometry +
        // textures); its overlay transform is driven per frame + it's shown only
        // in HUNT.
        if let Some(g) = world.gun_model() {
            renderer.upload_viewmodel(g);
        }
        if let Some(m) = world.muzzle_model() {
            renderer.upload_muzzle(m);
        }
        // A3: upload the enemy weapon render library once (gun + muzzle meshes for
        // the whole arsenal), keyed by weapon name — any hunter can then draw any
        // weapon world-space in HUNT (and the BUILD demo can preview each).
        for w in world.enemy_weapon_lib() {
            renderer.upload_enemy_weapon(w.name, &w.gun);
            if let Some(muzzle) = &w.muzzle {
                renderer.upload_enemy_muzzle(w.name, muzzle);
            }
        }
        // Object palette: load each prop GLB once → upload to the renderer's prop
        // channel (keyed by catalog key) and register its model-space AABB on the
        // World (drives the placement ghost + ground/centre anchor). Textured static
        // meshes load through the same path as the guns.
        for def in crate::props::CATALOG {
            let path = format!("{}/../../assets/props/{}", env!("CARGO_MANIFEST_DIR"), def.glb);
            match crate::combat::load_gun(&path) {
                Ok(model) => {
                    let (min, max) = model_aabb(&model);
                    world.register_prop_bounds(def.mesh, min, max);
                    renderer.upload_prop(def.key, &model);
                    log::info!("loaded prop {} ({} verts)", def.name, model.vertices.len());
                }
                Err(e) => log::warn!("prop '{}' load failed: {e}", def.name),
            }
        }
        // Player Combat P3: upload the code-defined HUD glyph atlas once (the ammo
        // counter's bitmap font); the per-frame text quads are set below.
        let (hw, hh, hpx) = crate::hud::atlas_rgba();
        renderer.upload_hud_atlas(hw, hh, &hpx);
        // P5: bake + upload the initial (full-health) radial HUD texture so it's
        // ready to show on the first hit.
        if let (Some((w, h)), Some(rgba)) = (world.health_hud_dims(), world.health_hud_rgba()) {
            renderer.update_health_texture(w, h, &rgba);
        }
        // Audio: initialize the device (silent if none), then hand it to the world,
        // which preloads the weapon SFX and starts the looping background music.
        if let Some(audio) = engine::audio::AudioManager::new() {
            world.attach_audio(audio);
        }
        log::info!(
            "click=grab/select  WASD+mouse=fly  scroll=size  +/-=carve/extend  B=door  H=hole  P=pillar  R=brace  ↑/↓=stairs(Enter/Esc)  T=platform(select→drag gizmo to move/scale; C=connect K=simple F=ground V=rails X=del)  1-9=room texture  \\=grid/textured  F1-F8=load level slot  Ctrl+F1-F8=save level slot  Y=proc-anim preview(Z=fire)  I=invincible  N=invisible  G=HUNT  M=shop menu (N64 Start)  [HUNT: click=fire  RMB=aim  R=reload  Q=weapon  F=detonate mines]"
        );

        window.request_redraw();
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.world = Some(world);
        // The FrameClock lazily initializes its timing on the first
        // `begin_frame`/`pace`, so there's nothing to seed here.
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        // Raw mouse motion → look. Only meaningful while grabbed.
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.input.pointer_locked {
                self.input.add_mouse(delta.0 as f32, delta.1 as f32);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let egui see every event first (it needs cursor-move/click/scroll/keys for
        // the menus). `consumed` is true when a UI panel handled the event — we then
        // skip the game's own handling of that input so a click on the shop doesn't
        // also fire/select in the world behind it.
        let egui_consumed = if let (Some(state), Some(window)) =
            (self.egui_state.as_mut(), self.window.as_ref())
        {
            state.on_window_event(window, &event).consumed
        } else {
            false
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }

            // Track the cursor (physical pixels) for prop mouse-picking. egui already
            // saw this event above; we just record the latest position.
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x as f32, position.y as f32);
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                // A click egui handled (on a panel) never reaches the world.
                if egui_consumed {
                    return;
                }
                // Record the held state (combat reads it each frame for firing).
                let pressed = state == ElementState::Pressed;
                self.input.set_mouse_left(pressed);
                if !pressed {
                    // Release ends any in-progress prop gizmo drag.
                    if let Some(w) = self.world.as_mut() {
                        w.end_prop_gizmo_drag();
                    }
                    return;
                }
                // Object palette: while a prop is armed, a left-click drops it at the
                // crosshair floor hit. Handled before the pointer-lock re-grab so it
                // works with the cursor free (the panel frees it).
                if self.world.as_ref().map(|w| w.is_placing_prop()).unwrap_or(false) {
                    // Refresh the pick at the click position, then drop the prop there.
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(world) = self.world.as_mut() {
                            world.update_prop_preview(o, d);
                            world.confirm_prop_placement();
                        }
                    }
                    return;
                }
                // Object mode (panel open, free cursor, not RMB-looking): grab a gizmo
                // handle on the selected prop, else select the prop under the cursor.
                // Never grabs the cursor here — RMB owns looking.
                if self.props_open
                    && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false)
                    && !self.input.pointer_locked
                {
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(w) = self.world.as_mut() {
                            if !w.start_prop_gizmo_drag(o, d) {
                                w.select_prop_at(o, d);
                            }
                        }
                    }
                    return;
                }
                // In object mode we never fall through to the editor's face-select
                // (e.g. an LMB click while RMB-looking) — objects own the input here.
                if self.props_open {
                    return;
                }
                if !self.input.pointer_locked {
                    self.set_pointer_lock(true);
                    return;
                }
                // Grabbed + HUNT: left-click FIRES (handled per-frame in
                // `combat_step`), so authoring is skipped here.
                if self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false) {
                    return;
                }
                // Grabbed + BUILD: confirm an armed opening (door/hole) or
                // placement (pillar/brace), else select the crosshair face.
                let opening = self.world.as_ref().map(|w| w.is_opening_arming()).unwrap_or(false);
                let placing = self.world.as_ref().map(|w| w.is_placing()).unwrap_or(false);
                let platform = self.world.as_ref().map(|w| w.is_platform_tool()).unwrap_or(false);
                let rm = if opening {
                    self.world.as_mut().and_then(|w| w.with_undo(|w| w.confirm_opening()))
                } else if placing {
                    self.world.as_mut().and_then(|w| w.with_undo(|w| w.confirm_place()))
                } else if platform {
                    // `platform_click` may start a gizmo drag (records its own undo
                    // in `gizmo_start`) or place/connect a structure; `with_undo`
                    // only commits when it actually returns a rebuilt mesh.
                    self.world.as_mut().and_then(|w| w.with_undo(|w| w.platform_click()))
                } else {
                    if let Some(world) = self.world.as_mut() {
                        world.select_at_crosshair();
                    }
                    None
                };
                if let Some(rm) = rm {
                    self.upload(&rm);
                }
                self.refresh_highlight();
            }

            // Right mouse = the GoldenEye free-aim modifier (hold in HUNT). Just
            // record the held state; `World::look` reads it each frame.
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                self.input.set_mouse_right(pressed);
                // Object mode (BUILD, panel open): hold RMB to mouse-look. Grabbing
                // hides+centres the cursor and enables the raw-motion camera look
                // (the free cursor otherwise drives the panel + gizmo); releasing
                // frees it again. In HUNT, RMB stays the free-aim modifier (unchanged).
                if self.props_open && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false) {
                    self.set_pointer_lock(pressed);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // egui ate the scroll (e.g. a scrollable shop list) → don't also size
                // the editor selection.
                if egui_consumed {
                    return;
                }
                // Scroll sizes the selection sub-rect: plain = U (width),
                // Shift+scroll = V (height). Scroll up grows, down shrinks
                // (JS main.js wheel handler). BUILD, grabbed, with a face selected.
                if !self.input.pointer_locked {
                    return;
                }
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if dy == 0.0 {
                    return;
                }
                let step = if dy > 0.0 { 1.0 } else { -1.0 };
                let shift = self.input.key_down(KeyCode::ShiftLeft)
                    || self.input.key_down(KeyCode::ShiftRight);
                let (du, dv) = if shift { (0.0, step) } else { (step, 0.0) };
                if let Some(world) = self.world.as_mut() {
                    // Scroll routes to whichever tool is armed: the connect-slide
                    // (attach point along the edge), else platform footprint, else
                    // placement (pillar/brace) sizing, else hole sizing, else the
                    // sub-face selection.
                    if world.is_connect_sliding() {
                        world.adjust_connect_slide(step);
                    } else if world.is_platform_placing() {
                        world.adjust_platform_size(du, dv);
                    } else if world.is_placing() {
                        world.adjust_place_size(du, dv);
                    } else if world.is_hole_arming() {
                        world.adjust_opening_size(du, dv);
                    } else {
                        world.adjust_selection_size(du, dv);
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // egui has keyboard focus (e.g. typing in a UI field) → don't route
                // the key to gameplay/authoring.
                if egui_consumed {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                match event.state {
                    ElementState::Pressed => {
                        self.input.press(code);
                        self.on_key_pressed(code);
                    }
                    ElementState::Released => self.input.release(code),
                }
            }

            WindowEvent::RedrawRequested => {
                // Fixed-timestep simulation (via the engine clock): look once per
                // frame (crisp aim), movement/physics in discrete fixed steps.
                let dt = self.clock.begin_frame(Instant::now());
                let fixed_dt = self.clock.fixed_dt();
                let steps = self.clock.take_fixed_steps();

                // Spin the shop's 3D weapon preview while the menu is open (~1 turn / 9s).
                if self.shop_open {
                    self.shop_preview_angle += dt * 0.7;
                }
                // Same for the object panel's prop turntable.
                if self.props_open {
                    self.props_preview_angle += dt * 0.7;
                }

                // USB-N64 gamepad: poll + apply the solitaire scheme. This injects
                // held buttons + analog move into `input` and drives HUNT look/aim
                // directly, so it runs before mouse-look (which the pad supersedes
                // only while actively used — see `pad_actions.active`). Keyboard/mouse
                // remain live in BUILD, when unplugged, and whenever the pad is idle.
                let mut pad_actions = crate::gamepad::PadActions::default();
                if let (Some(pad), Some(world)) = (self.gamepad.as_mut(), self.world.as_mut()) {
                    pad_actions = pad.update(dt, &mut self.input, world);
                }
                if pad_actions.just_connected {
                    // Drop straight into gameplay — no mouse click needed to grab.
                    self.set_pointer_lock(true);
                }
                if pad_actions.menu {
                    self.toggle_shop();
                }
                if pad_actions.reload {
                    if let Some(world) = self.world.as_mut() {
                        if !world.is_build() {
                            if world.is_player_dead() {
                                world.restart_after_death();
                            } else {
                                world.reload_weapon();
                            }
                        }
                    }
                }
                if pad_actions.cycle {
                    self.begin_weapon_switch();
                }
                if pad_actions.detonate {
                    if let Some(world) = self.world.as_mut() {
                        world.detonate_remote_mines(); // HUNT-gated inside
                    }
                }

                // Apply mouse-look — unless a gizmo drag is active, in which case
                // the mouse motion drives the drag (move/scale) instead of the cam.
                let dragging = self
                    .world
                    .as_ref()
                    .map(|w| w.is_gizmo_dragging())
                    .unwrap_or(false);
                if dragging {
                    let (mdx, mdy) = self.input.take_mouse_delta();
                    let rm = self.world.as_mut().and_then(|w| w.gizmo_drag_delta(mdx, mdy));
                    if let Some(rm) = rm {
                        self.upload(&rm);
                    }
                } else if let Some(world) = self.world.as_mut() {
                    // The pad drives HUNT look/aim only while it's actively used;
                    // mouse-look runs in BUILD and whenever the pad is idle (so a
                    // connected-but-untouched pad doesn't disable the mouse).
                    if world.is_build() || !pad_actions.active {
                        world.look(&mut self.input, dt);
                    }
                }
                if let Some(world) = self.world.as_mut() {
                    for _ in 0..steps {
                        world.fixed_step(fixed_dt, &self.input);
                    }
                    // Advance the skinned character's animation once per frame
                    // (visual; JS mixer.update(delta) cadence, real dt).
                    world.advance_animation(dt);
                    // Player Combat: advance the weapon + fire on trigger (HUNT
                    // only; JS WeaponSystem.update(dt) cadence, real dt).
                    world.combat_step(dt, &self.input);
                    // A3: pump the hunter's rifle shots (FIRE_TIMING window) + decay
                    // the player damage-flash / HUD-pop timers (HUNT only).
                    world.enemy_combat_step(dt);
                }
                // A weapon switch swaps the gun/muzzle meshes at the bottom of its
                // dip (mid-`combat_step`); re-upload them to the GPU when it does.
                if self.world.as_mut().map(|w| w.take_models_dirty()).unwrap_or(false) {
                    self.upload_weapon_meshes();
                }
                // Per-frame highlight in BUILD while grabbed: the door ghost, or
                // the crosshair-tracked selection sub-rect (camera look was
                // applied above this frame).
                // Object-palette ghost: the floor box tracks the crosshair whether or
                // not the cursor is grabbed (the panel frees it), so you can aim a
                // placement while picking from the list. Takes priority over the
                // grabbed-only editor highlights.
                let prop_placing = self
                    .world
                    .as_ref()
                    .map(|w| w.is_build() && w.is_placing_prop())
                    .unwrap_or(false);
                if prop_placing {
                    let ray = self.mouse_world_ray();
                    let mesh = ray
                        .and_then(|(o, d)| self.world.as_mut().and_then(|w| w.update_prop_preview(o, d)));
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(mesh.as_ref());
                    }
                } else if self.props_open {
                    // Object mode (not placing): no editor face highlight at all — the
                    // gizmo is the only overlay. Clear any stale highlight.
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(None);
                    }
                } else if self.input.pointer_locked
                    && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false)
                {
                    let opening = self.world.as_ref().map(|w| w.is_opening_arming()).unwrap_or(false);
                    let placing = self.world.as_ref().map(|w| w.is_placing()).unwrap_or(false);
                    let platform = self.world.as_ref().map(|w| w.is_platform_tool()).unwrap_or(false);
                    let pending_stair =
                        self.world.as_ref().map(|w| w.has_pending_stair()).unwrap_or(false);
                    // A pending stair suppresses the face highlight; its x-ray
                    // ghost (set below in the render section) owns the feedback.
                    let mesh = self.world.as_mut().and_then(|w| {
                        if pending_stair {
                            None
                        } else if opening {
                            w.update_opening_preview()
                        } else if placing {
                            w.update_place_preview()
                        } else if platform {
                            w.update_platform_preview()
                        } else {
                            w.update_selection_preview()
                        }
                    });
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(mesh.as_ref());
                    }
                }
                // Object mode: push this frame's cursor ray + snap modifier (Ctrl) to
                // the world (prop gizmo pick/hover/drag read them) and advance any
                // active gizmo drag.
                if let Some((o, d)) = self.mouse_world_ray() {
                    let snap = self.input.key_down(KeyCode::ControlLeft)
                        || self.input.key_down(KeyCode::ControlRight);
                    if let Some(w) = self.world.as_mut() {
                        w.set_mouse_ray(o, d);
                        w.set_gizmo_snap(snap);
                        w.update_prop_gizmo_drag();
                    }
                }
                // Build this frame's egui menus before the render borrow block (it
                // needs its own &mut self), then hand the tessellated UI to render().
                let egui_frame = self.build_egui_frame();

                if let (Some(world), Some(renderer)) =
                    (self.world.as_ref(), self.renderer.as_mut())
                {
                    renderer.set_entity_mesh(world.enemy_mesh().as_ref());
                    // Drive every skinned character (each hunter, or the BUILD demo)
                    // — its pose + death-fade opacity.
                    renderer.set_character_instances(&world.character_instances());
                    // Player Combat: drive the gun + muzzle-flash overlay
                    // transforms (shown only in HUNT; `None` hides them) and the
                    // live hit-spark markers.
                    let aspect = renderer.aspect();
                    renderer.set_viewmodel_transform(world.viewmodel_transform(aspect));
                    renderer.set_muzzle_transform(world.muzzle_transform(aspect));
                    // A3: the hunters' guns + muzzle flashes (world-space; two draws
                    // for a dual-wielder). Empty lists when nothing is shown.
                    renderer.set_enemy_weapon_draws(&world.enemy_weapon_draws(aspect));
                    renderer.set_enemy_muzzle_draws(&world.enemy_muzzle_draws(aspect));
                    // Placed props (the object palette): world-space textured draws,
                    // one per authored prop entity (white tint in M1).
                    renderer.set_prop_draws(&world.prop_draws(aspect));
                    // Crosshair: BUILD shows the small white editor cross (while
                    // grabbed, so it marks the face-pick centre); HUNT shows the
                    // GoldenEye reticle only while aiming, and nothing otherwise.
                    if world.is_build() {
                        if self.input.pointer_locked {
                            renderer.set_build_crosshair();
                        } else {
                            renderer.set_crosshair_offset(None);
                        }
                    } else {
                        let crosshair = world
                            .crosshair_visible()
                            .then(|| world.aim_offset(aspect));
                        renderer.set_crosshair_offset(crosshair);
                    }
                    // The fixed enemy spawn-point marker (colored floor square) —
                    // drawn in both modes so the builder can author around it.
                    renderer.set_marker_mesh(world.spawn_marker_mesh().as_ref());
                    renderer.set_spark_mesh(world.spark_mesh().as_ref());
                    // Explosion fireballs (additive textured billboards, world-space).
                    renderer.set_blast_mesh(world.blast_mesh().as_ref());
                    // Player Combat P3: the ammo-counter HUD, or the YOU DIED text
                    // when dead (HUNT only; `None` in BUILD).
                    renderer.set_hud_mesh(world.hud_mesh(aspect).as_deref());
                    // P5: re-bake the radial health texture only when health/armor
                    // changed, then drive the health HUD opacity + red damage flash
                    // + death dimmer (all HUNT-only).
                    let (hp, ap) = (world.player_health(), world.player_armor());
                    if hp != self.last_hud_health || ap != self.last_hud_armor {
                        if let (Some((w, h)), Some(rgba)) =
                            (world.health_hud_dims(), world.health_hud_rgba())
                        {
                            renderer.update_health_texture(w, h, &rgba);
                        }
                        self.last_hud_health = hp;
                        self.last_hud_armor = ap;
                    }
                    if world.is_build() {
                        renderer.set_health_hud(None);
                        renderer.set_damage_flash(0.0);
                        renderer.set_death_screen(false);
                    } else {
                        renderer.set_health_hud(Some(world.hud_alpha()));
                        renderer.set_damage_flash(world.damage_flash());
                        renderer.set_death_screen(world.is_player_dead());
                    }
                    renderer.set_door_mesh(world.door_mesh().as_ref());
                    // Pending-stair ghost — `None` (auto-clears) unless a stair op
                    // is in progress in BUILD.
                    renderer.set_stair_ghost(world.stair_preview_mesh().as_ref());
                    // Platform gizmo — `None` unless a platform is selected in BUILD.
                    renderer.set_gizmo_mesh(world.gizmo_mesh().as_ref());
                    // Shop open → render the selected gun into the offscreen preview
                    // texture (submitted before the main frame, whose egui pass samples
                    // it) so the panel shows a live rotating 3D model.
                    if self.shop_open {
                        let sel = self
                            .shop_selected
                            .min(crate::combat::config::WEAPONS.len() - 1);
                        let name = crate::combat::config::WEAPONS[sel].name;
                        renderer.render_weapon_preview(name, self.shop_preview_angle);
                    }
                    // Object panel open → render the selected prop into the same
                    // offscreen preview texture (the panel samples it as an image).
                    if self.props_open {
                        if let Some(def) =
                            self.props_selected.and_then(|i| crate::props::CATALOG.get(i))
                        {
                            renderer.render_prop_preview(def.key, self.props_preview_angle);
                        }
                    }
                    let view_proj = world.view_proj(renderer.aspect());
                    renderer.render(view_proj, egui_frame);
                }

                // Frame-time telemetry, logged once per second.
                self.fps_frames += 1;
                self.fps_elapsed += dt;
                self.fps_worst_ms = self.fps_worst_ms.max(dt * 1000.0);
                if self.fps_elapsed >= 1.0 {
                    let avg_ms = self.fps_elapsed * 1000.0 / self.fps_frames as f32;
                    log::info!(
                        "{:.0} fps (avg {:.2} ms/frame, worst {:.2} ms)",
                        self.fps_frames as f32 / self.fps_elapsed,
                        avg_ms,
                        self.fps_worst_ms
                    );
                    self.fps_frames = 0;
                    self.fps_elapsed = 0.0;
                    self.fps_worst_ms = 0.0;
                }
            }
            _ => {}
        }
    }

    /// Pace rendering via the engine clock: request a redraw when the frame
    /// budget has elapsed, then sleep the loop until the next deadline (no CPU
    /// busy-spin).
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let (redraw, wait_until) = self.clock.pace(Instant::now());
        if redraw {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
    }
}

impl App {
    /// One-shot key actions (edits + cursor release). Held-key movement is read
    /// each frame from `InputState`, not here.
    fn on_key_pressed(&mut self, code: KeyCode) {
        // Esc cancels a pending stair op first (JS ordering); otherwise it
        // releases the cursor.
        if code == KeyCode::Escape {
            // Esc order (JS-faithful): cancel a pending stair op first; else cancel
            // a gizmo drag / back out of a platform sub-phase; else release the
            // cursor (which also disarms every modal tool).
            let mut handled = false;
            let mut changed = None;
            if let Some(w) = self.world.as_mut() {
                if w.has_pending_stair() {
                    w.cancel_stairs();
                    log::info!("stair cancelled");
                    handled = true;
                } else {
                    let (consumed, mesh) = w.platform_escape();
                    handled = consumed;
                    changed = mesh;
                }
            }
            if let Some(rm) = changed {
                self.upload(&rm);
            }
            if !handled {
                self.set_pointer_lock(false);
            }
            return;
        }
        // M toggles the shop/inventory menu (keyboard counterpart of the N64 Start
        // button). Handled early — before the pointer-lock gate — so it works in
        // gameplay (grabbed) and in the editor (free) alike.
        if code == KeyCode::KeyM {
            self.toggle_shop();
            return;
        }
        // O toggles the left object-placement panel (BUILD authoring). Early, like
        // the shop, so it works whether the cursor is grabbed or free.
        if code == KeyCode::KeyO {
            self.toggle_props();
            return;
        }
        // Object-mode edit keys (only while the panel is open):
        //   T    → cycle the prop gizmo (Move ↔ Rotate)
        //   Esc  → disarm placement + deselect the current prop
        if self.props_open {
            if code == KeyCode::KeyT {
                if let Some(w) = self.world.as_mut() {
                    w.cycle_prop_gizmo();
                }
                return;
            }
            // Q / Esc → neutral: stop placing AND deselect, so clicks select existing
            // props (no need to leave + re-enter object mode).
            if code == KeyCode::Escape || code == KeyCode::KeyQ {
                if let Some(w) = self.world.as_mut() {
                    w.cancel_prop_placement();
                    w.deselect_prop();
                }
                return;
            }
            // Shift+D duplicates the selected prop (Blender-style). D still feeds
            // fly-cam strafe via the input state; this only adds the copy action.
            if code == KeyCode::KeyD
                && (self.input.key_down(KeyCode::ShiftLeft)
                    || self.input.key_down(KeyCode::ShiftRight))
            {
                if let Some(w) = self.world.as_mut() {
                    w.duplicate_selected_prop();
                }
                return;
            }
        }
        // Backslash toggles the checkerboard "grid" view vs the textured view
        // (JS `toggle_view`). Works whether or not the cursor is grabbed.
        if code == KeyCode::Backslash {
            if let Some(r) = self.renderer.as_mut() {
                let grid = !r.is_grid_mode();
                r.set_grid_mode(grid);
                log::info!("view: {}", if grid { "grid" } else { "textured" });
            }
            return;
        }
        // Level save/load quick-slots (works grabbed or not, like the grid
        // toggle): F1–F8 LOAD slot 1–8; Ctrl+F1–F8 SAVE slot 1–8. Saving keeps
        // the current editable geometry; loading replaces it (BUILD only).
        if let Some(slot) = slot_for_fkey(code) {
            let ctrl = self.input.key_down(KeyCode::ControlLeft)
                || self.input.key_down(KeyCode::ControlRight);
            if ctrl {
                self.save_slot(slot);
            } else {
                self.load_slot(slot);
            }
            return;
        }
        // Undo / redo (BUILD only — geometry is frozen in HUNT). Ctrl+Z steps back
        // through authored edits, Ctrl+R re-applies. Both re-bake + re-upload every
        // affected region + the structures mesh, then clear any stale highlight.
        // Checked before the bare-key Z (proc-anim preview) / R (brace) handlers,
        // and before the pointer-lock gate, so it works grabbed or not (like the
        // F-key save/load). When Ctrl isn't held, these fall through to those keys.
        if matches!(code, KeyCode::KeyZ | KeyCode::KeyR) {
            let ctrl = self.input.key_down(KeyCode::ControlLeft)
                || self.input.key_down(KeyCode::ControlRight);
            let build = self.world.as_ref().map(|w| w.is_build()).unwrap_or(false);
            if ctrl && build {
                let meshes = if code == KeyCode::KeyZ {
                    self.world.as_mut().and_then(|w| w.undo())
                } else {
                    self.world.as_mut().and_then(|w| w.redo())
                };
                if let Some(meshes) = meshes {
                    for rm in &meshes {
                        self.upload(rm);
                    }
                    // Selection was cleared by the restore — drop any highlight.
                    self.refresh_highlight();
                }
                return;
            }
        }
        // I toggles player invincibility (dev/observe): enemies keep aiming + firing
        // but you take no damage, so you can watch them chase + shoot. Works anytime.
        if code == KeyCode::KeyI {
            if let Some(world) = self.world.as_mut() {
                world.toggle_invulnerable();
            }
            return;
        }
        // N toggles player invisibility (dev/observe): no hunter can perceive you, so
        // the pack drops to searching — walk around and watch them scan for you.
        if code == KeyCode::KeyN {
            if let Some(world) = self.world.as_mut() {
                world.toggle_invisible();
            }
            return;
        }
        // `=` crank difficulty up, `-` down (each is a single key — no Shift needed;
        // NumpadAdd/Subtract too). Changing the dial restarts the duel fresh at the new
        // level (heal + respawn — see `World::change_difficulty`). **HUNT only** — these
        // keys are the BUILD push/pull step (`+`/`=` push, `-` pull), so in BUILD we let
        // them fall through to the authoring handler below instead of eating them here.
        // See `DiffParams`.
        if matches!(
            code,
            KeyCode::Equal | KeyCode::NumpadAdd | KeyCode::Minus | KeyCode::NumpadSubtract
        ) && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                let up = matches!(code, KeyCode::Equal | KeyCode::NumpadAdd);
                world.change_difficulty(if up { 1 } else { -1 });
            }
            return;
        }
        // Authoring only while grabbed (crosshair is meaningful).
        if !self.input.pointer_locked {
            return;
        }
        // Number keys 1-9 retexture the room under the crosshair (flood-fill,
        // bounded by door/hole frames).
        if let Some(key) = digit_char(code) {
            if let Some(scheme) = engine::render::textures::scheme_for_key(key) {
                if let Some(rm) = self
                    .world
                    .as_mut()
                    .and_then(|w| w.with_undo(|w| w.set_scheme_at_crosshair(scheme)))
                {
                    self.upload(&rm);
                }
            }
            return;
        }
        // G toggles BUILD ↔ HUNT (freeze + drop in as the player, or back).
        if code == KeyCode::KeyG {
            if let Some(world) = self.world.as_mut() {
                world.toggle_mode();
            }
            self.refresh_highlight(); // cleared when entering HUNT
            return;
        }
        // Spike: procedural-anim preview (BUILD only). Y toggles a preview character
        // in front of the camera; Z fires a manual recoil kick. See
        // `world::spike_preview`.
        if code == KeyCode::KeyY {
            if let Some(world) = self.world.as_mut() {
                world.toggle_procedural_preview();
            }
            return;
        }
        if code == KeyCode::KeyZ {
            if self.world.as_ref().map(|w| w.is_build()).unwrap_or(false) {
                if let Some(world) = self.world.as_mut() {
                    world.fire_procedural_preview();
                }
                return;
            }
        }
        // Q cycles the player's weapon (HUNT only; the JS `KeyQ` bind). BUILD leaves
        // Q free for future editor use.
        if code == KeyCode::KeyQ {
            if self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false) {
                self.begin_weapon_switch();
            }
            return;
        }
        // B / H toggle the opening tools (door / hole): arm a ghost preview that
        // tracks the crosshair (drawn each frame in RedrawRequested), or turn it
        // back off. Left-click is what cuts (handled in MouseInput).
        if code == KeyCode::KeyB || code == KeyCode::KeyH {
            if let Some(world) = self.world.as_mut() {
                if code == KeyCode::KeyB {
                    world.door_tool_key();
                } else {
                    world.hole_tool_key();
                }
            }
            // Deselecting disarms → clear the ghost; arming leaves the next
            // frame's preview to repopulate the highlight.
            if self.world.as_ref().map(|w| !w.is_opening_arming()).unwrap_or(true) {
                self.refresh_highlight();
            }
            return;
        }
        // F in HUNT: detonate all live remote mines (the keyboard counterpart of the
        // pad's A+B combo — there's no separate Detonator weapon slot). In BUILD, F
        // stays the "toggle grounded" editor key (handled below), so only claim it here
        // when hunting.
        if code == KeyCode::KeyF
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                world.detonate_remote_mines();
            }
            return;
        }
        // R in HUNT: restart from the YOU DIED screen if dead, else reload the
        // weapon (in BUILD it's the brace tool, below).
        if code == KeyCode::KeyR
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                if world.is_player_dead() {
                    world.restart_after_death();
                } else {
                    world.reload_weapon();
                }
            }
            return;
        }
        // P / R toggle the placement tools (pillar / brace): aim + scroll to size,
        // left-click to place. The ghost is drawn each frame in RedrawRequested.
        if code == KeyCode::KeyP || code == KeyCode::KeyR {
            if let Some(world) = self.world.as_mut() {
                if code == KeyCode::KeyP {
                    world.pillar_tool_key();
                } else {
                    world.brace_tool_key();
                }
            }
            if self.world.as_ref().map(|w| !w.is_placing()).unwrap_or(true) {
                self.refresh_highlight();
            }
            return;
        }
        // Platform + stair-run tool. T toggles the tool; the rest act on the
        // current selection / phase. Grounded/railings/delete change geometry, so
        // they return the rebuilt structures mesh to upload.
        if code == KeyCode::KeyT {
            if let Some(world) = self.world.as_mut() {
                world.platform_tool_key();
            }
            self.refresh_highlight();
            return;
        }
        if code == KeyCode::KeyC {
            if let Some(world) = self.world.as_mut() {
                world.connect_key();
            }
            return;
        }
        if code == KeyCode::KeyK {
            if let Some(world) = self.world.as_mut() {
                world.simple_stair_key();
            }
            return;
        }
        if matches!(code, KeyCode::KeyF | KeyCode::KeyV | KeyCode::KeyX | KeyCode::Delete) {
            let rm = self.world.as_mut().and_then(|w| {
                w.with_undo(|w| match code {
                    KeyCode::KeyF => w.toggle_grounded_key(),
                    KeyCode::KeyV => w.toggle_railings_key(),
                    _ => w.delete_selected(),
                })
            });
            if let Some(rm) = rm {
                self.upload(&rm);
                self.refresh_highlight();
            }
            return;
        }
        // Stair tool (JS-faithful): Arrow Up/Down grow a pending up/down-stair
        // op on the selected floor-touching wall face; Enter confirms. No mode.
        if matches!(code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
            let dir = if code == KeyCode::ArrowUp {
                engine::geometry::csg_runtime::StairDir::Up
            } else {
                engine::geometry::csg_runtime::StairDir::Down
            };
            if let Some(world) = self.world.as_mut() {
                if world.push_stairs(dir) {
                    if let Some((n, d)) = world.pending_stair() {
                        log::info!("stairs: {n} step(s) {d:?} — Enter to confirm, Esc to cancel");
                    }
                } else {
                    log::info!("stairs need a wall face whose selection touches the floor");
                }
            }
            return;
        }
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
            if let Some(rm) = self.world.as_mut().and_then(|w| w.with_undo(|w| w.confirm_stairs())) {
                self.upload(&rm);
                self.refresh_highlight();
            }
            return;
        }

        let fine = self.input.key_down(KeyCode::ShiftLeft) || self.input.key_down(KeyCode::ShiftRight);
        let step = if fine { 1.0 } else { PUSH_PULL_STEP };

        let result = match code {
            // `+` and `=` share a key; NumpadAdd for good measure. Each press is
            // one undo step (a no-op push/pull returns `None`, so `with_undo`
            // records nothing).
            KeyCode::Equal | KeyCode::NumpadAdd => {
                self.world.as_mut().and_then(|w| w.with_undo(|w| w.push(step)))
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                self.world.as_mut().and_then(|w| w.with_undo(|w| w.pull(step)))
            }
            _ => None,
        };
        if let Some(rm) = result {
            self.upload(&rm);
            // The selected face moved with the edit — redraw its highlight.
            self.refresh_highlight();
        }
    }
}

/// Entry point: open the window and run the render loop.
pub fn run() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,engine=info,game=info"),
    )
    .init();

    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("run app");
}
