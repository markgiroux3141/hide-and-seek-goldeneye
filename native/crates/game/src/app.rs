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
use engine::render::camera::ViewAxis;
use crate::radial::{EditorAction, LockRequest, Radial, RadialCtx, SelectionOp, Tool};
use crate::world::{World, PUSH_PULL_STEP};
use engine::geometry::csg_runtime::{Axis, FaceTex, Side};

/// A frame this slow (ms) is worth a line of its own. Two and a half times the 60 Hz
/// budget: past this the drop is visible, and below it the once-per-second average is
/// the better instrument.
const SLOW_FRAME_MS: f32 = 40.0;
/// At most this many per-frame warnings per second. A sustained 5 fps would otherwise
/// print every frame forever, and the per-second line already carries the full count.
const SLOW_FRAME_BURST: u32 = 4;

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

/// A section of the left authoring panel (the OBJECTS/LIGHTING menu), cycled with
/// the `◄ ►` arrows around the title. Circular — advancing past the last wraps to
/// the first. Add a variant + an `ALL` entry to grow the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelTab {
    Objects,
    /// What the stair and platform tools build with — shells and the stair slope.
    /// See [`crate::world::BuildStyle`].
    Tools,
    /// The match setup a hunt starts from — what `G` reads. See
    /// [`crate::world::play_config`].
    Play,
    Lighting,
    Spawns,
    Textures,
    /// One face at a time: override the texture on the surface under the cursor,
    /// and show why the classifier gave it the texture it has. See
    /// [`crate::world::FaceProbe`].
    Paint,
    Nav,
    Levels,
}

impl PanelTab {
    /// Every tab, in display order (also the cycle order).
    pub(crate) const ALL: [PanelTab; 9] = [
        PanelTab::Objects,
        PanelTab::Tools,
        PanelTab::Play,
        PanelTab::Lighting,
        PanelTab::Spawns,
        PanelTab::Textures,
        PanelTab::Paint,
        PanelTab::Nav,
        PanelTab::Levels,
    ];

    /// The header title for this tab.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PanelTab::Objects => "OBJECTS",
            PanelTab::Tools => "TOOLS",
            PanelTab::Play => "PLAY",
            PanelTab::Lighting => "LIGHTING",
            PanelTab::Spawns => "SPAWNS",
            PanelTab::Textures => "TEXTURES",
            PanelTab::Paint => "PAINT",
            PanelTab::Nav => "NAV",
            PanelTab::Levels => "LEVELS",
        }
    }

    /// The next tab (wraps to the first after the last).
    fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The previous tab (wraps to the last before the first).
    fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// A zone slot's display name — the same seven the theme editor exposes, plus the
/// two the classifier can emit that no one authors directly.
///
/// The PAINT tab has to be able to *name* whatever the classifier came back with, so
/// this is a total function over `0..8` rather than a lookup that can miss. Every slot
/// is accounted for now that zone 4 is the cornice, but the fallback stays: the zone
/// table is a contract shared with the classifier, and a label is a poor place to
/// discover it has grown.
fn zone_name(zone: u8) -> &'static str {
    crate::theme_editor::EDITABLE_ZONES
        .iter()
        .find(|(z, _)| *z == zone)
        .map(|(_, label)| *label)
        .unwrap_or("unnamed zone")
}

/// How many zones the theme lists preview as swatches, in bottom-to-top reading order:
/// floor, ceiling, lower wall, upper wall, cornice.
///
/// Deliberately not every zone. Stair/frame (5), doorframe floor (6) and brace (7) are
/// fittings rather than room surfaces and have never been previewed here. The cornice is
/// a room surface, so it belongs — and showing it as an **empty** slot for the 394
/// themes that define none is the point: that is what "this theme has no cornice" looks
/// like, and it is a slot an author can go and fill.
const PREVIEW_ZONES: usize = 5;

// ─── Shop palette (GoldenEye gold-on-black spy-terminal look) ──────────────────
/// Signature gold accent — headings, borders, buy buttons, selection.
pub(crate) const SHOP_GOLD: egui::Color32 = egui::Color32::from_rgb(224, 184, 74);
/// A muted gold for section headers / secondary accents.
pub(crate) const SHOP_GOLD_DIM: egui::Color32 = egui::Color32::from_rgb(150, 122, 60);
/// Primary readable body text.
pub(crate) const SHOP_TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 228);
/// Dimmed text — unaffordable prices / disabled hints.
pub(crate) const SHOP_DIM: egui::Color32 = egui::Color32::from_rgb(110, 110, 118);
/// NAV tab verdicts: a clean finding, and one that means something in the level is
/// unreachable. Green/red rather than gold because these are pass/fail, not emphasis —
/// and the same two colours the 3D overlay uses for reachable / cut-off floor.
const NAV_OK: egui::Color32 = egui::Color32::from_rgb(96, 200, 116);
const NAV_BAD: egui::Color32 = egui::Color32::from_rgb(232, 96, 88);

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
    /// Where the second went, accumulated per frame and reported with the fps line
    /// below. Three numbers rather than one, because "the frame rate is terrible" is
    /// unactionable and "sim 0.4, render 190" names the culprit in one line. `render`
    /// includes submit + present, so GPU backpressure lands there.
    fps_sim_ms: f32,
    fps_render_ms: f32,
    /// Frames over [`SLOW_FRAME_MS`] this second (the per-frame warn is capped, this is not).
    slow_frames: u32,
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
    /// Whether *we* freed the cursor for the room plan tool, plus the lock state to
    /// hand back when it disarms.
    ///
    /// Reconciled once per frame from `World::is_room_tool` rather than at each arm
    /// site: the tool is disarmed from six places (Esc, a mode switch, arming any
    /// other tool, the radial, its own key, the cursor release) and a cursor left
    /// grabbed by a missed one is unrecoverable without knowing why.
    room_freed: bool,
    lock_before_room: bool,
    /// Whether RMB is currently dragging the orthographic drafting view.
    room_panning: bool,
    /// BUILD-mode preference: show real point lighting (`true`) vs the legacy flat
    /// look (`false`). Toggled by the **L** key / the OBJECTS panel checkbox. HUNT
    /// ignores this — it forces real lighting whenever the level has any light.
    build_real_lighting: bool,
    /// Which section of the left authoring panel is showing (Objects / Lighting),
    /// cycled by the `◄ ►` arrows in its header.
    panel_tab: PanelTab,

    // ── TEXTURES tab: browse ~390 extracted themes, apply, keep/reject ──────
    /// Theme index armed for application, if any. Clicking geometry applies it.
    theme_armed: Option<usize>,
    /// PAINT tab: the theme a painted face takes. Independent of
    /// [`Self::theme_armed`] (the room-wide retexture) — arming one disarms the
    /// other, since a click can only mean one thing.
    paint_scheme: usize,
    /// PAINT tab: the zone slot a painted face is forced into, or `None` to keep
    /// whatever the classifier derived (a wall stays a wall, a floor a floor).
    paint_zone: Option<u8>,
    /// PAINT tab: whether a left-click paints the face under the cursor.
    paint_armed: bool,
    /// PAINT tab: what the classifier says about the surface under the cursor,
    /// refreshed each frame the tab is open. Both the readout and the paint target.
    paint_probe: Option<crate::world::FaceProbe>,
    /// Case-insensitive substring filter over theme name/label/group.
    theme_filter: String,
    /// Which verdicts the list shows.
    theme_review_filter: crate::theme_review::ReviewFilter,
    /// Author keep/reject verdicts, persisted on every change.
    theme_review: crate::theme_review::ThemeReview,
    /// Lazily-built egui swatches, keyed by texture name.
    ///
    /// Built through `egui::Context::load_texture` rather than by retaining wgpu
    /// texture views out of `build_materials` and registering them: egui owns the
    /// upload, so this needs no renderer change at all. ~240 distinct 32x32 BMPs
    /// back the whole library, so the memory is trivial.
    theme_swatches: std::collections::HashMap<String, egui::TextureHandle>,
    /// `false` = browse the theme library, `true` = build a custom one.
    theme_edit_mode: bool,
    /// The custom theme under construction, mirrored into the renderer's scratch slot.
    theme_draft: crate::theme_editor::ThemeDraft,
    /// Transient status line under the save button ("saved as …", "all slots full").
    theme_status: String,
    /// Labels for custom slots saved this session. The registry is a `OnceLock`, so a
    /// slot's stored label only refreshes on restart — this makes a just-saved preset
    /// show its real name immediately instead of "(empty 03)".
    theme_slot_labels: std::collections::HashMap<usize, String>,

    // ── LEVELS tab: name, save, load and manage the level files ─────────────
    /// The file this level came from, or was last saved to. `None` for a level that has
    /// never been written — a fresh boot's starting room, or a generated arena — which
    /// is what makes plain Save fall through to Save As.
    current_level: Option<std::path::PathBuf>,
    /// [`World::revision`] as of the last successful save or load. The panel shows the
    /// unsaved marker while the world's revision differs from this.
    saved_revision: u64,
    /// The catalog as last read off disk. Refreshed when the tab opens and after every
    /// file operation, never per frame: it stats and parses every level file.
    level_rows: Vec<crate::world::persist::LevelEntry>,
    /// The row the buttons act on, held **by path** rather than by index. The list is
    /// sorted newest-first, so saving re-orders it — an index would silently come to
    /// point at a different level than the one that was clicked.
    level_sel: Option<std::path::PathBuf>,
    /// The name field's contents (used by Save As, Rename and Duplicate).
    level_name_draft: String,
    /// Transient status line under the buttons ("saved …", "already exists").
    level_status: String,
    /// A delete waiting on its confirming second click. Any other level action clears
    /// it, so an armed delete can't be committed by a later, unrelated click.
    level_confirm_delete: Option<std::path::PathBuf>,
    /// Whether the preview room geometry has been uploaded this session.
    /// The scheme the uploaded theme-preview room is tagged with, or `None` when none
    /// is uploaded.
    ///
    /// A scheme index rather than a "was it built" flag: the preview now serves both
    /// panel modes — the scratch theme while editing, and whichever library theme is
    /// armed while browsing — so it has to be rebuilt when the subject changes, not
    /// merely once.
    theme_preview_scheme: Option<usize>,
    /// Which revision of the NAV overlay is on the GPU (`None` = nothing uploaded).
    /// The mesh is far too big to re-upload per frame — see `World::nav_overlay_rev`.
    nav_overlay_uploaded: Option<u32>,

    // ── Middle-mouse radial menu (BUILD only) ───────────────────────────────
    /// The ring: hold MMB, flick, release. See [`crate::radial`].
    radial: Radial,
    /// Pointer-lock state captured when the ring opened, restored when it closes
    /// (mirrors [`Self::lock_before_shop`]) — so picking a tool from the menu hands
    /// control back exactly as it was.
    lock_before_radial: bool,
    /// Which quick-slots have a file, sampled when the ring opens rather than per
    /// frame: the Level ring wants to say "Load 3" vs "Slot 3", and that is eight
    /// filesystem stats we are not doing 240 times a second.
    radial_slots_used: [bool; 8],
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
            fps_sim_ms: 0.0,
            fps_render_ms: 0.0,
            slow_frames: 0,
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
            room_freed: false,
            lock_before_room: true,
            room_panning: false,
            build_real_lighting: true,
            panel_tab: PanelTab::Objects,
            theme_armed: None,
            paint_scheme: engine::render::textures::default_scheme(),
            paint_zone: None,
            paint_armed: false,
            paint_probe: None,
            theme_filter: String::new(),
            theme_review_filter: crate::theme_review::ReviewFilter::All,
            theme_review: crate::theme_review::ThemeReview::load(),
            theme_swatches: std::collections::HashMap::new(),
            theme_edit_mode: false,
            theme_draft: crate::theme_editor::ThemeDraft::default(),
            theme_status: String::new(),
            theme_slot_labels: std::collections::HashMap::new(),
            current_level: None,
            saved_revision: 0,
            level_rows: Vec::new(),
            level_sel: None,
            level_name_draft: String::new(),
            level_status: String::new(),
            level_confirm_delete: None,
            theme_preview_scheme: None,
            nav_overlay_uploaded: None,
            radial: Radial::default(),
            lock_before_radial: false,
            radial_slots_used: [false; 8],
        }
    }
}

impl App {
    /// An egui swatch for a level texture, decoded from disk on first use.
    ///
    /// Nearest-filtered and un-multiplied: these are 32x32 N64 textures, so any
    /// smoothing turns a swatch into mush. Returns `None` if the BMP is missing —
    /// the caller draws a placeholder rather than the magenta the 3D path uses,
    /// since in a list a gap reads more clearly than a colour.
    fn theme_swatch(&mut self, name: &str) -> Option<egui::TextureHandle> {
        if let Some(h) = self.theme_swatches.get(name) {
            return Some(h.clone());
        }
        let dec = engine::render::textures::try_decode(name)?;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [dec.width as usize, dec.height as usize],
            &dec.rgba,
        );
        let handle =
            self.egui_ctx
                .load_texture(format!("swatch:{name}"), image, egui::TextureOptions::NEAREST);
        self.theme_swatches.insert(name.to_string(), handle.clone());
        Some(handle)
    }

    /// Collect the swatches the TEXTURES tab needs this frame.
    ///
    /// Runs *before* the egui closure because that closure cannot hold `&mut self`
    /// (see `build_egui_frame`), and loading a texture needs the context plus the
    /// cache. Only themes passing the current filter are built, so the cost tracks
    /// what is actually on screen.
    fn collect_theme_swatches(
        &mut self,
        visible: &[usize],
    ) -> Vec<[Option<egui::TextureHandle>; PREVIEW_ZONES]> {
        let names: Vec<[Option<String>; PREVIEW_ZONES]> = visible
            .iter()
            .map(|&i| {
                let s = &engine::render::textures::schemes()[i];
                std::array::from_fn(|z| s.zones[z].and_then(|zd| zd.texture).map(String::from))
            })
            .collect();
        names
            .into_iter()
            .map(|row| {
                std::array::from_fn(|z| {
                    row[z].as_deref().and_then(|n| self.theme_swatch(n))
                })
            })
            .collect()
    }

    /// Push the whole draft into a scheme's live materials.
    ///
    /// Every zone is written even when only one changed: it is a handful of
    /// `write_buffer`s plus at most a bind-group rebuild each, which is far cheaper
    /// than tracking dirty zones, and it keeps "the GPU matches the draft" a single
    /// unconditional statement rather than an invariant to maintain.
    fn push_theme_to(&mut self, scheme: usize) {
        let zones = self.theme_draft.zones;
        // The cornice is the one zone that is not just a material: it moves a band
        // boundary, so it goes to the classifier's table and anything drawn with this
        // scheme has to be re-folded. Everything else is a texture or a UV parameter
        // and the renderer can swap it under the existing geometry.
        let depth = zones[engine::render::textures::CORNICE_ZONE as usize].map(|z| z.height);
        if engine::render::textures::cornice_of(scheme) != depth {
            engine::render::textures::set_cornice(scheme, depth);
            self.theme_preview_scheme = None;
            self.rebuild_all_regions_for_theme_change();
        }
        let Some(r) = self.renderer.as_mut() else { return };
        for (zi, z) in zones.iter().enumerate() {
            let Some(z) = z else { continue };
            r.set_material_texture(scheme, zi as u8, z.texture);
            r.set_material_params(scheme, zi as u8, z.repeat, z.offset);
        }
    }

    /// Re-fold every region and re-upload it, for a change that moved band boundaries
    /// rather than textures.
    ///
    /// Only reachable from the texture editor, and only when a cornice depth actually
    /// changed — a full re-bake of the level is far too expensive to do on every slider
    /// drag, which is why `push_theme_to` guards it on a real difference.
    fn rebuild_all_regions_for_theme_change(&mut self) {
        let Some(w) = self.world.as_mut() else { return };
        let meshes = w.initial_meshes();
        if let Some(r) = self.renderer.as_mut() {
            for m in meshes {
                r.set_region_textured(m.id, &m.mesh);
            }
        }
    }

    /// Finish a theme save: make the slot live and say so.
    ///
    /// Saving to disk cannot update the registry (a `OnceLock`), so the slot's materials
    /// are pushed here and its label remembered for this session; both refresh from the
    /// file on the next run. `push_theme_to` also re-folds the level when the cornice
    /// depth moved, which is the one theme property that changes geometry.
    fn after_theme_saved(&mut self, slot: usize, verb: &str) {
        self.push_theme_to(slot);
        let label = self.theme_draft.save_name.trim().to_string();
        let label = if label.is_empty() {
            engine::render::textures::schemes()[slot].label.to_string()
        } else {
            label
        };
        self.theme_slot_labels.insert(slot, label.clone());
        self.theme_status = format!("{verb} \"{label}\" — usable now");
        log::info!("theme preset {verb} custom slot {slot} as {label:?}");
    }

    /// Mirror the draft into the scratch scheme so the world shows it immediately.
    fn sync_theme_scratch(&mut self) {
        let scratch = engine::render::textures::scratch_scheme();
        self.push_theme_to(scratch);
    }

    /// The theme the preview room should be showing, or `None` to leave the preview
    /// target to whatever else wants it.
    ///
    /// Editing shows the scratch theme. Browsing shows the **armed** theme, which is
    /// what a click on a row selects — without this, leaving the editor dropped you back
    /// to the level's own textures and a 419-theme review list you could not see.
    fn theme_preview_subject(&self) -> Option<usize> {
        if self.panel_tab != PanelTab::Textures || !self.props_open {
            return None;
        }
        if self.theme_edit_mode {
            return Some(engine::render::textures::scratch_scheme());
        }
        self.theme_armed
    }

    /// Upload the preview room tagged with `scheme`, if that is not already what is up.
    ///
    /// Built lazily rather than at startup: it costs a CSG fold and most sessions never
    /// open the panel. The mesh depends on the scheme *index* (zone groups carry it) and
    /// on that scheme's **cornice depth**, which moves a band boundary — so
    /// `push_theme_to` clears this when the depth changes. Every other theme edit
    /// changes materials, not geometry, and needs no rebuild.
    fn ensure_theme_preview_room(&mut self, scheme: usize) {
        if self.theme_preview_scheme == Some(scheme) {
            return;
        }
        let mesh = crate::theme_editor::preview_room_mesh(scheme);
        if let Some(r) = self.renderer.as_mut() {
            r.set_theme_preview_room(&mesh);
            self.theme_preview_scheme = Some(scheme);
        }
    }

    /// Display label for a theme, preferring a name saved this session.
    ///
    /// A preset saved now can't update the registry's label (`OnceLock`), so without
    /// this a just-saved theme would list as "(empty 03)" until restart.
    fn theme_label(&self, scheme: usize) -> String {
        self.theme_slot_labels
            .get(&scheme)
            .cloned()
            .unwrap_or_else(|| {
                engine::render::textures::schemes()
                    .get(scheme)
                    .map(|s| s.label.to_string())
                    .unwrap_or_default()
            })
    }

    /// Theme indices passing the name/verdict filters, in manifest order.
    fn visible_themes(&self) -> Vec<usize> {
        let needle = self.theme_filter.trim().to_ascii_lowercase();
        engine::render::textures::schemes()
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                use engine::render::textures::SchemeKind;
                match s.kind {
                    // Reached through the editor, not the list — it has no stable
                    // identity to review or bind a key to.
                    SchemeKind::Scratch => return false,
                    // An unoccupied slot is a placeholder. One saved *this session*
                    // is real, but the registry still reports it unused (it's a
                    // `OnceLock`), so the session's own record is the authority.
                    SchemeKind::Custom { used: false } => {
                        if !self.theme_slot_labels.contains_key(i) {
                            return false;
                        }
                    }
                    SchemeKind::Custom { used: true } | SchemeKind::Library => {}
                }
                if !self.theme_review_filter.accepts(self.theme_review.get(s.name)) {
                    return false;
                }
                let label = self.theme_label(*i);
                needle.is_empty()
                    || s.name.to_ascii_lowercase().contains(&needle)
                    || label.to_ascii_lowercase().contains(&needle)
                    || s.group.to_ascii_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

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
        self.save_level_to(&crate::world::persist::slot_path(slot));
    }

    /// Load numbered quick-slot `slot`, replacing the editable geometry and
    /// re-uploading every region + structures mesh (stale ones cleared).
    fn load_slot(&mut self, slot: u8) {
        self.load_level_file(&crate::world::persist::slot_path(slot));
    }

    // ─── Level files (the LEVELS tab, and the F-key quick slots) ────────────

    /// Whether the open level has edits that aren't on disk.
    ///
    /// A revision comparison rather than a flag: see [`World::revision`]. With no file
    /// behind the level yet, everything authored since boot is unsaved, so a level that
    /// has been touched at all counts as dirty.
    fn level_dirty(&self) -> bool {
        self.world
            .as_ref()
            .map(|w| w.revision() != self.saved_revision)
            .unwrap_or(false)
    }

    /// Adopt `path` as the open level and mark the world clean against it. Called after
    /// every successful save or load, whichever front-end asked for it.
    fn set_current_level(&mut self, path: std::path::PathBuf) {
        self.saved_revision = self.world.as_ref().map(|w| w.revision()).unwrap_or(0);
        self.level_name_draft = self
            .world
            .as_ref()
            .map(|w| w.level_name().to_string())
            .unwrap_or_default();
        self.level_sel = Some(path.clone());
        self.current_level = Some(path);
        self.level_confirm_delete = None;
        self.refresh_level_rows();
    }

    /// Re-read the level catalog from disk.
    ///
    /// Not per frame — it stats and JSON-parses every level file. Called when the tab
    /// opens and after each file operation, which is exactly when it can have changed.
    fn refresh_level_rows(&mut self) {
        self.level_rows = crate::world::persist::list_levels();
        // A selection whose file is gone (deleted here, or outside the game) would leave
        // the buttons acting on nothing; fall back to the open level.
        if !self
            .level_sel
            .as_ref()
            .is_some_and(|sel| self.level_rows.iter().any(|r| &r.path == sel))
        {
            self.level_sel = self.current_level.clone();
        }
    }

    /// Save the open level back to its own file — `Ctrl+S` and the panel's Save.
    ///
    /// A level with no file yet (a fresh boot's starting room, a generated arena) has
    /// nowhere to go, and says so rather than inventing a filename.
    fn save_current_level(&mut self) {
        match self.current_level.clone() {
            Some(path) => self.save_level_to(&path),
            None => {
                self.level_status = "this level has no file yet — name it, then Save As".into();
                log::warn!("Ctrl+S: no current level file; use Save As");
            }
        }
    }

    /// Write the open level to `path`, overwriting it, and adopt it as the current file.
    /// The shared body of Save, Save-to-slot and `Ctrl+F1..F8`.
    fn save_level_to(&mut self, path: &std::path::Path) {
        // A level that has never been named takes the filename it is being written to
        // ("slot3"), so `Ctrl+F3` on a fresh level lists as something rather than as a
        // blank row. Named levels keep the name they have.
        if let Some(world) = self.world.as_mut() {
            if world.level_name().trim().is_empty() {
                if let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) {
                    world.set_level_name(&stem);
                }
            }
        }
        let Some(world) = self.world.as_ref() else {
            return;
        };
        match world.save_level(path) {
            Ok(()) => {
                let name = world.level_name().to_string();
                log::info!("saved level {name:?} → {}", path.display());
                self.set_current_level(path.to_path_buf());
                self.level_status = format!("saved {}", short_path(path));
            }
            Err(e) => {
                log::warn!("save to {} failed: {e}", path.display());
                self.level_status = format!("save failed: {e}");
            }
        }
    }

    /// Load the level file at `path`, replacing the editable geometry and re-uploading
    /// every region + the structures mesh (stale ones cleared). Returns whether it
    /// loaded.
    ///
    /// The pre-load snapshot is what makes loading over unsaved work survivable: an
    /// accidental Load is one `Ctrl+Z` from being undone, which is why there is no
    /// "discard changes?" prompt in the way.
    fn load_level_file(&mut self, path: &std::path::Path) -> bool {
        let meshes = match self.world.as_mut() {
            Some(world) => {
                // Committed only once the load succeeds — a failed load leaves the
                // world untouched, and must not leave a dead step in the history.
                let snap = world.snapshot();
                match world.load_level(path) {
                    Ok(meshes) => {
                        world.commit_snapshot(snap);
                        meshes
                    }
                    Err(e) => {
                        log::warn!("load {} failed: {e}", path.display());
                        self.level_status = format!("load failed: {e}");
                        return false;
                    }
                }
            }
            None => return false,
        };
        for rm in &meshes {
            self.upload(rm);
        }
        // Selection was cleared by the load — drop any lingering highlight.
        self.refresh_highlight();
        self.set_current_level(path.to_path_buf());
        let name = self
            .world
            .as_ref()
            .map(|w| w.level_name().to_string())
            .unwrap_or_default();
        self.level_status = format!("loaded {name}");
        true
    }

    /// Push the current selection's highlight quad to the renderer.
    fn refresh_highlight(&mut self) {
        if let (Some(world), Some(renderer)) = (self.world.as_ref(), self.renderer.as_mut()) {
            let mesh = world.selection_face_mesh();
            renderer.set_highlight(mesh.as_ref());
        }
    }

    /// Room-tool keys: the numpad view presets and the new-room theme.
    ///
    /// Blender's layout, because it is the one every 3D package's users already have
    /// in their fingers: `7`/`1`/`3` are top/front/right, Ctrl gives the opposite
    /// face, `5` flips between the drafting views and the perspective fly view, and
    /// `0` goes straight to perspective.
    ///
    /// The number *row* stays live and picks the theme new rooms are built with.
    /// `digit_char` normally aliases the numpad to the row for the crosshair
    /// retexture, which is why that has to be bypassed here — and why bypassing it
    /// costs nothing: retexture needs a crosshair, and an orthographic view has none.
    ///
    /// Returns whether the key was consumed.
    fn room_key(&mut self, code: KeyCode) -> bool {
        let ctrl = self.input.key_down(KeyCode::ControlLeft)
            || self.input.key_down(KeyCode::ControlRight);
        let axis = match (code, ctrl) {
            (KeyCode::Numpad7, false) => Some(ViewAxis::Top),
            (KeyCode::Numpad7, true) => Some(ViewAxis::Bottom),
            (KeyCode::Numpad1, false) => Some(ViewAxis::Front),
            (KeyCode::Numpad1, true) => Some(ViewAxis::Back),
            (KeyCode::Numpad3, false) => Some(ViewAxis::Right),
            (KeyCode::Numpad3, true) => Some(ViewAxis::Left),
            _ => None,
        };
        if let Some(axis) = axis {
            if let Some(w) = self.world.as_mut() {
                w.enter_room_view(axis);
            }
            return true;
        }
        match code {
            KeyCode::Numpad5 => {
                if let Some(w) = self.world.as_mut() {
                    w.toggle_room_view();
                }
                true
            }
            KeyCode::Numpad0 => {
                if let Some(w) = self.world.as_mut() {
                    w.leave_room_view();
                }
                true
            }
            // Every other numpad key is swallowed rather than falling through to its
            // number-row twin: the numpad belongs to the views while this is armed,
            // and a stray `Numpad2` silently retexturing something would be baffling.
            KeyCode::Numpad2
            | KeyCode::Numpad4
            | KeyCode::Numpad6
            | KeyCode::Numpad8
            | KeyCode::Numpad9 => true,
            _ => {
                let Some(key) = row_digit_char(code) else {
                    return false;
                };
                let scheme = self.world.as_ref().and_then(|w| w.scheme_for_key(key));
                if let (Some(scheme), Some(w)) = (scheme, self.world.as_mut()) {
                    w.set_room_scheme(scheme);
                }
                true
            }
        }
    }

    /// Give the room plan tool the free cursor it needs, and hand the lock back when
    /// it disarms. Called once per frame — see the `room_freed` field for why this is
    /// a reconcile rather than a call at each arm site.
    fn sync_room_cursor(&mut self) {
        let armed = self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false);
        if armed == self.room_freed {
            return;
        }
        if armed {
            self.lock_before_room = self.input.pointer_locked;
            self.room_freed = true;
            self.set_pointer_lock_keep_tools(false);
        } else {
            self.room_freed = false;
            self.room_panning = false;
            // A panel that took the cursor for itself keeps it; restoring the lock
            // under an open panel would leave it unclickable.
            if !self.props_open && !self.shop_open {
                self.set_pointer_lock_keep_tools(self.lock_before_room);
            }
        }
    }

    fn set_pointer_lock(&mut self, locked: bool) {
        self.set_pointer_lock_inner(locked, true);
    }

    /// Free / re-grab the cursor **without** disarming the armed tool.
    ///
    /// Releasing the cursor normally cancels every modal tool, which is right for
    /// Esc and for the panels — they take the screen over. It is wrong for the
    /// radial: opening a menu to flip the grid view must not throw away the door
    /// tool you had armed. Same window calls, no cancellation.
    fn set_pointer_lock_keep_tools(&mut self, locked: bool) {
        self.set_pointer_lock_inner(locked, false);
    }

    fn set_pointer_lock_inner(&mut self, locked: bool, cancel_tools: bool) {
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
            if !cancel_tools {
                return;
            }
            if let Some(world) = self.world.as_mut() {
                world.cancel_opening();
                world.cancel_place();
                world.cancel_platform_tool();
                world.cancel_draw();
                world.cancel_room();
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

    /// The **use** button, in HUNT: GoldenEye's context-sensitive B. Opens or shuts a
    /// door if one is in reach, otherwise reloads — and skips the death beat first if
    /// the player is down.
    ///
    /// Shared by the keyboard `B` and the N64 pad's B tap so the two can't drift: the
    /// pad previously went straight to `reload_weapon`, which meant doors worked on the
    /// keyboard and not on the controller.
    fn use_or_reload(&mut self) {
        let Some(world) = self.world.as_mut() else { return };
        if world.is_build() {
            return;
        }
        if world.is_player_dead() {
            world.restart_after_death();
            return;
        }
        // Door takes priority over reloading — a door in reach is what you meant.
        if world.use_door() {
            return;
        }
        world.reload_weapon();
    }

    /// Toggle the left object-placement panel (BUILD). Opening frees the cursor (so
    /// its list is clickable and you can aim the floor crosshair); closing restores
    /// the prior lock, disarms any armed prop, and clears the selection.
    fn toggle_props(&mut self) {
        self.props_open = !self.props_open;
        if self.props_open {
            self.lock_before_props = self.input.pointer_locked;
            self.set_pointer_lock(false);
            // The panel reopens on whichever tab it was left on, so O landing straight
            // back on LEVELS has to re-read the catalog too — not just `OpenPanel`.
            if self.panel_tab == PanelTab::Levels {
                self.refresh_level_rows();
            }
        } else {
            self.set_pointer_lock(self.lock_before_props);
            if let Some(w) = self.world.as_mut() {
                w.cancel_prop_placement();
                w.cancel_light_placement();
                w.deselect_prop();
            }
            self.props_selected = None;
            // A theme armed for click-application only makes sense with the panel
            // open and the cursor free; closing must not leave clicks retexturing.
            self.theme_armed = None;
            self.paint_armed = false;
            self.paint_probe = None;
            // Hand the highlight back to face-select — the PAINT tab borrowed it.
            self.refresh_highlight();
        }
    }

    // ─── Radial menu (BUILD only) ──────────────────────────────────────────

    /// Whether the ring is allowed to open right now.
    ///
    /// BUILD only, and not while a panel owns the screen. The object panel is
    /// excluded on purpose: it already *is* the menu for its own contents, so the
    /// radial's only business with it is the entry that opens it.
    fn radial_allowed(&self) -> bool {
        !self.shop_open
            && !self.props_open
            && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false)
    }

    /// Open the ring, centred on the crosshair when the cursor is grabbed or on the
    /// cursor itself when it is free.
    fn open_radial(&mut self) {
        if !self.radial_allowed() {
            return;
        }
        for n in 1..=8u8 {
            self.radial_slots_used[(n - 1) as usize] = crate::world::persist::slot_path(n).exists();
        }
        self.lock_before_radial = self.input.pointer_locked;
        let ppp = self.egui_ctx.pixels_per_point().max(0.01);
        let origin = if self.input.pointer_locked {
            match self.window.as_ref() {
                Some(w) => {
                    let size = w.inner_size();
                    (
                        size.width as f32 * 0.5 / ppp,
                        size.height as f32 * 0.5 / ppp,
                    )
                }
                None => return,
            }
        } else {
            (self.cursor_pos.0 / ppp, self.cursor_pos.1 / ppp)
        };
        let locked = self.input.pointer_locked;
        let req = self.radial.press(origin, locked);
        self.apply_lock_request(req);
        // Opening with a free cursor starts sticky, and sticky drives off the real
        // pointer — seed it so the ring isn't blind until the mouse first moves.
        if self.radial.is_sticky() {
            self.radial.cursor(self.cursor_pos.0 / ppp, self.cursor_pos.1 / ppp);
        }
    }

    /// Close the ring and hand the pointer lock back as it was.
    fn close_radial(&mut self) {
        if !self.radial.is_open() {
            return;
        }
        self.radial.close();
        self.set_pointer_lock_keep_tools(self.lock_before_radial);
    }

    /// Perform what the state machine asked for after an event. It owns no window,
    /// so it names the lock change and this does it.
    fn apply_lock_request(&mut self, req: LockRequest) {
        match req {
            LockRequest::None => {}
            // Sticky wants a real pointer. Keep the armed tool: the menu is a lens
            // over the editor, not a mode change.
            LockRequest::Free => {
                self.set_pointer_lock_keep_tools(false);
                self.warp_cursor_to_radial();
            }
            LockRequest::Restore => self.set_pointer_lock_keep_tools(self.lock_before_radial),
        }
    }

    /// Put the OS cursor on the ring's hub when it goes sticky.
    ///
    /// The grab hides the cursor wherever it last was, so simply un-grabbing hands it
    /// back at a position that has nothing to do with the menu — on a wide screen it
    /// can be most of a monitor away from a ring drawn at the crosshair, and the first
    /// thing you'd have to do is go and find it. Warping it to the hub means the first
    /// movement is already a selection, and the hub is the neutral square: it hovers
    /// nothing.
    fn warp_cursor_to_radial(&mut self) {
        let ppp = self.egui_ctx.pixels_per_point().max(0.01);
        let (ox, oy) = self.radial.origin();
        let (px, py) = (ox * ppp, oy * ppp);
        if let Some(w) = self.window.as_ref() {
            // Best-effort: some platforms refuse to move the pointer, and a menu that
            // still works with a cursor in the wrong place beats no menu.
            let _ = w.set_cursor_position(winit::dpi::PhysicalPosition::new(
                px as f64, py as f64,
            ));
        }
        self.cursor_pos = (px, py);
        self.radial.cursor(ox, oy);
    }

    /// Hand an action back from the ring: the lock is restored *first*, so the action
    /// then runs against exactly the input state its hotkey would have seen. That is
    /// what lets `SetScheme` use the camera crosshair rather than trying to shoot a
    /// ray through wherever the menu left the cursor.
    fn commit_radial(&mut self, action: Option<EditorAction>, req: LockRequest) {
        self.apply_lock_request(req);
        if let Some(action) = action {
            self.apply(action);
        }
    }

    /// The snapshot the menu tables are built from.
    fn radial_ctx(&self) -> RadialCtx {
        let mut c = RadialCtx {
            ctrl: self.input.key_down(KeyCode::ControlLeft)
                || self.input.key_down(KeyCode::ControlRight),
            grid: self.renderer.as_ref().map(|r| r.is_grid_mode()).unwrap_or(false),
            real_lighting: self.build_real_lighting,
            slots: self.radial_slots_used,
            level: self.current_level.as_ref().map(|_| {
                let name = self
                    .world
                    .as_ref()
                    .map(|w| w.level_name().to_string())
                    .unwrap_or_default();
                if name.is_empty() {
                    "this level".to_string()
                } else {
                    name
                }
            }),
            level_dirty: self.level_dirty(),
            ..RadialCtx::default()
        };
        if let Some(w) = self.world.as_ref() {
            c.nav_overlay = w.nav_overlay_on();
            c.proc_preview = w.is_procedural_preview();
            c.invincible = w.is_invulnerable();
            c.invisible = w.is_invisible();
            c.hunters = w.hunters_enabled();
            c.wave = w.wave_size();
            c.has_selection = w.has_selection();
            c.pending_stair = w.has_pending_stair();
            c.patch_scope = w.patch_scope();
            c.patch_len = w.patch_len();
            c.armed = armed_tool(w);
            let bs = w.build_style();
            c.platform_style = bs.platform;
            c.stair_shell = bs.stairs;
            c.schemes = "123456789"
                .chars()
                .filter_map(|d| {
                    let idx = w.scheme_for_key(d)?;
                    Some((d, self.theme_label(idx), idx))
                })
                .collect();
        }
        c
    }

    // ─── One implementation, two front-ends ────────────────────────────────

    /// Do one editor action, whatever asked for it.
    ///
    /// Both `on_key_pressed` and the radial dispatch here. Before this existed, what
    /// a key *did* lived only in the key handler's body — with two front-ends that
    /// body would have been copied, and the copies would have drifted.
    fn apply(&mut self, action: EditorAction) {
        match action {
            EditorAction::ArmTool(tool) => self.arm_tool(tool),

            EditorAction::Selection(op) => self.selection_op(op),
            EditorAction::OpenPanel(tab) => {
                self.panel_tab = tab;
                if tab != PanelTab::Paint {
                    self.paint_armed = false;
                    self.paint_probe = None;
                    self.refresh_highlight();
                }
                if !self.props_open {
                    self.toggle_props();
                }
                // The catalog is read from disk, so it can be stale by the time the tab
                // is opened again (another session saved, a file was deleted outside
                // the game). Re-read on the way in rather than per frame.
                if tab == PanelTab::Levels {
                    self.refresh_level_rows();
                }
            }
            EditorAction::SetScheme(scheme) => {
                if let Some(rm) = self
                    .world
                    .as_mut()
                    .and_then(|w| w.with_undo(|w| w.set_scheme_at_crosshair(scheme)))
                {
                    self.upload(&rm);
                }
            }
            EditorAction::EnterHunt => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_mode();
                }
                self.refresh_highlight(); // cleared when entering HUNT
            }
            EditorAction::LoadSlot(n) => self.load_slot(n),
            EditorAction::SaveSlot(n) => self.save_slot(n),
            EditorAction::SaveCurrentLevel => self.save_current_level(),
            EditorAction::ToggleGrid => {
                if let Some(r) = self.renderer.as_mut() {
                    let grid = !r.is_grid_mode();
                    r.set_grid_mode(grid);
                    log::info!("view: {}", if grid { "grid" } else { "textured" });
                }
            }
            EditorAction::ToggleLighting => {
                self.build_real_lighting = !self.build_real_lighting;
                log::info!(
                    "lighting: {}",
                    if self.build_real_lighting { "real" } else { "flat" }
                );
            }
            EditorAction::ToggleNavOverlay => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_nav_overlay();
                }
            }
            EditorAction::ToggleProcPreview => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_procedural_preview();
                }
            }
            EditorAction::ToggleInvincible => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_invulnerable();
                }
            }
            EditorAction::ToggleInvisible => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_invisible();
                }
            }
            EditorAction::ToggleHunters => {
                if let Some(world) = self.world.as_mut() {
                    world.toggle_hunters();
                }
            }
            EditorAction::WaveSize(d) => {
                if let Some(world) = self.world.as_mut() {
                    world.change_wave_size(d);
                }
            }
            EditorAction::DumpTelemetry => self.dump_telemetry(),
        }
    }

    /// Arm / toggle a modal tool, then deal with the ghost.
    ///
    /// Disarming leaves a stale preview behind, so the highlight has to be cleared;
    /// *arming* must leave it alone, because the next frame's preview repopulates it.
    /// Which of the two happened is only knowable by asking the tool afterwards.
    fn arm_tool(&mut self, tool: Tool) {
        if let Some(world) = self.world.as_mut() {
            match tool {
                Tool::Draw => world.draw_tool_key(),
                Tool::Room => world.room_tool_key(),
                Tool::Door => {
                    world.door_tool_key();
                }
                Tool::Hole => {
                    world.hole_tool_key();
                }
                Tool::Vent => {
                    world.vent_tool_key();
                }
                Tool::Ladder => {
                    world.ladder_tool_key();
                }
                Tool::Pillar => world.pillar_tool_key(),
                Tool::Brace => world.brace_tool_key(),
                Tool::Platform => world.platform_tool_key(),
                Tool::BlockStairs => world.simple_stair_key(),
                Tool::Connect => world.connect_key(),
            }
        }
        let stale = match tool {
            Tool::Draw => self.world.as_ref().map(|w| !w.is_draw_tool()).unwrap_or(true),
            // Owns the selection and the camera outright, so it always refreshes.
            Tool::Room => true,
            Tool::Door | Tool::Hole => self
                .world
                .as_ref()
                .map(|w| !w.is_opening_arming())
                .unwrap_or(true),
            Tool::Vent => self.world.as_ref().map(|w| !w.is_vent_tool()).unwrap_or(true),
            Tool::Ladder => self.world.as_ref().map(|w| !w.is_ladder_tool()).unwrap_or(true),
            Tool::Pillar | Tool::Brace => {
                self.world.as_ref().map(|w| !w.is_placing()).unwrap_or(true)
            }
            // These own the selection outright, so they always refresh: arming the
            // platform tool drops any face selection, and so does a cold `K` (block
            // stairs arms the same phase machine when the platform tool is down).
            Tool::Platform | Tool::BlockStairs => true,
            // Connect never draws a crosshair ghost and never clears the selection —
            // it *needs* the selected platform it starts from.
            Tool::Connect => false,
        };
        if stale {
            self.refresh_highlight();
        }
    }

    /// Act on the current selection. Everything that changes geometry goes through
    /// `with_undo` and uploads the rebuilt region, exactly as the keys always did.
    fn selection_op(&mut self, op: SelectionOp) {
        // Stairs grow a *pending* op rather than editing, so they take their own path.
        if matches!(op, SelectionOp::StairUp | SelectionOp::StairDown) {
            let dir = if op == SelectionOp::StairUp {
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
        // Scope is a selection *setting*, not an edit: no geometry changes, so no
        // undo step — but the highlight has to be redrawn, since flipping scope is
        // exactly the moment the author needs to see what is now selected.
        if op == SelectionOp::ToggleScope {
            if let Some(world) = self.world.as_mut() {
                world.toggle_patch_scope();
            }
            self.refresh_highlight();
            return;
        }
        let fine =
            self.input.key_down(KeyCode::ShiftLeft) || self.input.key_down(KeyCode::ShiftRight);
        let step = if fine { 1.0 } else { PUSH_PULL_STEP };
        // `with_undo_many`, because a patch push/pull can touch brushes in more than
        // one region and every changed region has to be re-uploaded — returning only
        // the first left the rest on screen as stale geometry.
        let rms = self
            .world
            .as_mut()
            .map(|w| {
                w.with_undo_many(|w| match op {
                    SelectionOp::Push => w.push(step),
                    SelectionOp::Pull => w.pull(step),
                    SelectionOp::Delete => w.delete_selected().into_iter().collect(),
                    SelectionOp::Grounded => w.toggle_grounded_key().into_iter().collect(),
                    SelectionOp::Railings => w.toggle_railings_key().into_iter().collect(),
                    SelectionOp::ConfirmStairs => w.confirm_stairs(),
                    SelectionOp::ToggleScope
                    | SelectionOp::StairUp
                    | SelectionOp::StairDown => Vec::new(),
                })
            })
            .unwrap_or_default();
        if !rms.is_empty() {
            for rm in &rms {
                self.upload(rm);
            }
            // The selected face moved with the edit — redraw its highlight.
            self.refresh_highlight();
        }
    }

    /// Capture hunter telemetry to a file — the "it is happening RIGHT NOW" button.
    ///
    /// A frozen hunter tells you nothing from the outside: this writes what each one
    /// thinks it is doing, what it is walking to, which gate refused its last step and
    /// for how long, plus whether A* can even route it to you. Appends, so a session
    /// of presses is one timeline rather than a file you have to catch.
    fn dump_telemetry(&mut self) {
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let dump = world.hunter_telemetry();
        print!("{dump}");
        let path = "hunter_telemetry.log";
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| {
                use std::io::Write as _;
                writeln!(f, "{dump}")
            });
        match wrote {
            Ok(()) => log::info!("hunter telemetry appended to {path}"),
            Err(e) => log::warn!("could not write {path}: {e}"),
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
    /// Paint (or with `None`, clear) the face the PAINT tab's probe is pointing at.
    ///
    /// Routes through the probe rather than re-picking, so the face that changes is
    /// exactly the one the readout named — the tab would be worse than useless if
    /// the thing it explained and the thing it painted could differ.
    fn paint_probe_face(&mut self, tex: Option<FaceTex>) {
        let Some((brush_id, axis, side)) = self.paint_probe.as_ref().and_then(|p| p.target())
        else {
            return;
        };
        let rm = self
            .world
            .as_mut()
            .and_then(|w| w.with_undo(|w| w.set_face_tex(brush_id, axis, side, tex)));
        if let Some(rm) = rm {
            self.upload(&rm);
        }
    }

    fn build_egui_frame(&mut self) -> Option<EguiFrame> {
        // ── PAINT tab prep: re-probe the surface under the cursor.
        //
        // Every frame the tab is open, and only then. It costs one raycast plus a
        // classify of the single triangle that came back — the panel is a live
        // readout, so a stale answer is worse than no answer, and there is nothing
        // to invalidate it against short of doing the work.
        let paint_tab_open = self.props_open && self.panel_tab == PanelTab::Paint;
        if !paint_tab_open {
            self.paint_probe = None;
        } else if !self.egui_ctx.is_pointer_over_area() {
            // Frozen while the pointer is over the panel — last frame's answer, which
            // egui's context is what knows. Without this the readout would change out
            // from under you the moment you reached for a button, and "Paint this"
            // would act on whatever the panel happens to be sitting in front of.
            self.paint_probe = self.mouse_world_ray().and_then(|(o, d)| {
                self.world.as_mut().and_then(|w| w.probe_surface(o, d))
            });
        }
        // Outline the face a click would repaint. While the tab is open this owns the
        // highlight slot outright — the face-select highlight is for push/pull, and
        // showing both would say two different things about what the next click does.
        if paint_tab_open {
            let quad = self
                .paint_probe
                .as_ref()
                .and_then(|p| p.target())
                .and_then(|(id, axis, side)| {
                    self.world.as_ref()?.face_highlight_mesh(id, axis, side)
                });
            if let Some(r) = self.renderer.as_mut() {
                r.set_highlight(quad.as_ref());
            }
        }

        // Its theme label comes from the same `&mut self` window as the swatches.
        let paint_scheme_label = self.theme_label(self.paint_scheme);

        // ── TEXTURES tab prep, which MUST come first.
        //
        // Everything below this holds a shared borrow of `self`, but building a
        // swatch needs `&mut self` (it fills a cache and touches the egui context).
        // So the theme snapshot happens up front, while `self` is still free.
        let theme_visible: Vec<usize> = if self.props_open
            && matches!(self.panel_tab, PanelTab::Textures | PanelTab::Paint)
        {
            self.visible_themes()
        } else {
            Vec::new()
        };
        let theme_swatches = self.collect_theme_swatches(&theme_visible);
        let theme_rows: Vec<ThemeRow> = theme_visible
            .iter()
            .map(|&idx| {
                let s = &engine::render::textures::schemes()[idx];
                ThemeRow {
                    idx,
                    name: s.name,
                    label: self.theme_label(idx),
                    group: s.group,
                    key: s.key,
                    verdict: self.theme_review.get(s.name),
                    editable: matches!(
                        s.kind,
                        engine::render::textures::SchemeKind::Custom { .. }
                    ),
                    repeats: std::array::from_fn(|z| s.zones[z].map(|zd| zd.repeat)),
                }
            })
            .collect();
        let platforms_are_floors = self
            .world
            .as_ref()
            .map(|w| w.platforms_are_floors())
            .unwrap_or(false);
        let theme_armed = self.theme_armed;
        let theme_armed_label = theme_armed.map(|i| self.theme_label(i)).unwrap_or_default();
        // What each quick key resolves to right now, and whether that came from this
        // level's own binding or fell through to the manifest.
        let hotkey_rows: Vec<(char, String)> = if self.props_open
            && self.panel_tab == PanelTab::Textures
        {
            "123456789"
                .chars()
                .map(|d| {
                    let level_bound = self
                        .world
                        .as_ref()
                        .and_then(|w| w.theme_hotkeys().get(&d).cloned());
                    let label = match level_bound {
                        Some(name) => match engine::render::textures::scheme_index(&name) {
                            Some(i) => self.theme_label(i),
                            None => format!("{name} (missing)"),
                        },
                        None => match engine::render::textures::scheme_for_key(d) {
                            Some(i) => format!("{} (default)", self.theme_label(i)),
                            None => "—".to_string(),
                        },
                    };
                    (d, label)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut theme_filter_ui = self.theme_filter.clone();
        let theme_review_filter = self.theme_review_filter;
        let (kept_n, cut_n, new_n) = self.theme_review.tally();
        let theme_total = engine::render::textures::schemes().len();

        // ── Editor snapshot: the draft, plus swatches for the texture picker's
        // currently-selected source level (not all 1016 — only what's on screen).
        let theme_edit_mode = self.theme_edit_mode;
        let mut draft_zone_sel = self.theme_draft.zone_sel;
        let mut draft_save_name = self.theme_draft.save_name.clone();
        // Resolved before the egui closure, which cannot hold `&mut self`.
        let draft_overwrite_label = self
            .theme_draft
            .overwrite_target()
            .map(|slot| self.theme_label(slot));
        let draft_zones = self.theme_draft.zones;
        let draft_dirty = self.theme_draft.dirty;
        let theme_status = self.theme_status.clone();
        let mut draft_level_sel = self.theme_draft.level_sel.clone();
        let level_groups: Vec<String> = engine::render::textures::catalog_by_level()
            .iter()
            .map(|(lv, _)| lv.clone())
            .collect();
        if draft_level_sel.is_empty() {
            draft_level_sel = level_groups.first().cloned().unwrap_or_default();
        }
        let picker_textures: Vec<&'static str> = if theme_edit_mode {
            engine::render::textures::catalog_by_level()
                .iter()
                .find(|(lv, _)| *lv == draft_level_sel)
                .map(|(_, names)| names.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let picker_swatches: Vec<Option<egui::TextureHandle>> =
            picker_textures.iter().map(|n| self.theme_swatch(n)).collect();

        // The radial menu, snapshotted like every other panel here: the egui closure
        // below runs under a live `&mut self.egui_state` borrow and cannot read `self`.
        let radial_view = self.radial.view(&self.radial_ctx());

        let window = self.window.as_ref()?;
        let state = self.egui_state.as_mut()?;
        let raw_input = state.take_egui_input(window);

        let shop_open = self.shop_open;
        // Whether the mouse is grabbed this frame. Read up front like every other
        // snapshot here, so the closure can hide egui's cursor without borrowing `self`.
        let pointer_locked = self.input.pointer_locked;
        let credits = self.world.as_ref().map(|w| w.credits()).unwrap_or(0);
        // egui handle to the offscreen 3D weapon preview (rendered below in the render
        // block). Read here so the closure can draw it as an image.
        let preview_tex = self.renderer.as_ref().map(|r| r.weapon_preview_texture_id());
        // Snapshot each weapon's shop state so the closure needs no `World` borrow.
        // The selected weapon's stat block for the preview pane, snapshotted for the
        // same reason as `rows`: the egui closure holds no `World` borrow.
        let sel_stats: Option<crate::combat::WeaponStats> = match (shop_open, self.world.as_ref()) {
            (true, Some(world)) => {
                let arsenal = world.arsenal_weapons();
                arsenal.get(self.shop_selected.min(arsenal.len().saturating_sub(1))).copied()
            }
            _ => None,
        };
        let rows: Vec<ShopRow> = match (shop_open, self.world.as_ref()) {
            (true, Some(world)) => (0..world.weapon_count())
                .map(|i| {
                    let name = world.arsenal_weapons()[i].name;
                    ShopRow {
                        idx: i,
                        name,
                        price: crate::shop::weapon_price(name),
                        ammo_price: crate::shop::ammo_price(name),
                        owned: world.owns_weapon(i),
                        reserve: world.weapon_ammo(i).map(|(_, r)| r).unwrap_or(0),
                        active: world.active_weapon_index() == i,
                        magazine_size: world.arsenal_weapons()[i].magazine_size,
                    }
                })
                .collect(),
            _ => Vec::new(),
        };

        // PD simulant lab overlay: snapshot each simulant's live model state (same
        // borrow discipline as the shop — read up front, the closure gets values).
        let pd_debug: Vec<crate::world::pd_lab::PdDebug> = self
            .world
            .as_ref()
            .filter(|w| w.pd_lab_active())
            .map(|w| w.pd_debug())
            .unwrap_or_default();

        // PD-style radar — a **player-facing feature now**, not the lab's navigation aid,
        // so it is no longer gated on `pd_lab_active`. Same borrow discipline as the
        // overlay: the whole frame is projected into the radar's own frame by the `World`
        // and handed over as plain values. `radar` returns `None` outside HUNT, which is
        // what keeps it off the BUILD screen.
        let radar: Option<crate::world::RadarView> =
            self.world.as_ref().and_then(|w| w.radar(RADAR_RANGE_M));

        let selected = self.shop_selected.min(rows.len().saturating_sub(1));
        let mut actions: Vec<ShopAction> = Vec::new();
        let mut new_selected: Option<usize> = None;
        let mut close = false;

        // Object-placement panel snapshot + deferred outputs (same borrow discipline
        // as the shop: read state up front, collect the pick/close, apply after).
        let props_open = self.props_open;
        // The room plan tool's read-out, snapshotted like everything else the egui
        // closure shows (it can't also hold a `&World`). `None` unless it is armed.
        let room_status = self.world.as_ref().and_then(|w| w.room_status());
        let prop_sel = self.props_selected;
        let prop_selected = self.world.as_ref().map(|w| w.selected_prop().is_some()).unwrap_or(false);
        let placing_prop = self.world.as_ref().map(|w| w.is_placing_prop()).unwrap_or(false);
        let gizmo_label = self.world.as_ref().map(|w| w.prop_gizmo_label()).unwrap_or("Move");
        let mut new_prop_selected: Option<usize> = None;
        let mut close_props = false;
        let mut ground_prop = false;
        let mut delete_prop = false;
        let mut go_neutral = false;
        // The selected prop's door settings, if it is a door — edited by value in the
        // panel and written back after, matching the light editor's borrow discipline.
        let mut door_edit = self.world.as_ref().and_then(|w| w.selected_door());
        let mut door_changed = false;
        // Pickup settings. One buffer serves two jobs, which is the point: it edits
        // the SELECTED pickup when one is picked in the 3D view, and otherwise the
        // draft that the next placed pickup will carry. So the same three controls
        // author and edit, and there is no second copy of the widget block.
        let selected_pickup = self.world.as_ref().and_then(|w| w.selected_pickup());
        let pickup_armed = self
            .world
            .as_ref()
            .and_then(|w| w.armed_pickup_kind())
            .is_some();
        let mut pickup_edit = selected_pickup
            .or_else(|| self.world.as_ref().map(|w| w.pickup_draft()))
            .unwrap_or_else(|| crate::ecs::Pickup::weapon("PP7"));
        let mut pickup_changed = false;
        // The guns a pickup can name: the live arsenal minus the empty-handed slot,
        // which is not something you can leave on the floor.
        let pickup_weapons: Vec<&'static str> = self
            .world
            .as_ref()
            .map(|w| {
                w.arsenal_weapons()
                    .iter()
                    .filter(|c| !c.is_unarmed())
                    .map(|c| c.name)
                    .collect()
            })
            .unwrap_or_default();
        let mut arm_weapon_pickup: Option<&'static str> = None;
        let armed_weapon = self.world.as_ref().and_then(|w| w.armed_pickup_weapon());

        // Lighting panel snapshot + edit buffers. The selected light's params (if the
        // selection is a light), the level ambient, and the placement/toggle states
        // are read up front; widgets mutate local buffers that are applied after the
        // `state` borrow ends (same discipline as the prop controls above).
        let selected_light = self.world.as_ref().and_then(|w| w.selected_light());
        let placing_light = self.world.as_ref().map(|w| w.is_placing_light()).unwrap_or(false);
        let ambient = self.world.as_ref().map(|w| w.ambient()).unwrap_or_default();
        let mut amb_color = ambient.color;
        let mut amb_level = ambient.level;
        let mut ambient_edited = false;
        let (mut light_color, mut light_intensity, mut light_range) =
            selected_light.unwrap_or(([1.0, 1.0, 1.0], 1.0, 8.0));
        let mut light_edited = false;
        let mut toggle_light_place = false;
        // Spawn-pad snapshot + edit buffer (same read-up-front / apply-after discipline).
        let placing_spawn = self
            .world
            .as_ref()
            .map(|w| w.is_placing_spawn_point())
            .unwrap_or(false);
        let spawn_pad_count = self.world.as_ref().map(|w| w.spawn_pad_count()).unwrap_or(0);
        let mut toggle_spawn_place = false;
        // NAV tab: the cached findings rendered as (text, severity) pairs, plus the two
        // deferred actions. Nothing here is computed per frame — `lines()` reads a cached
        // report and the Calculate that produces it is an explicit button.
        let nav_lines: Vec<(String, crate::world::NavSeverity)> = self
            .world
            .as_ref()
            .and_then(|w| w.nav_issues())
            .map(|i| i.lines().into_iter().map(|l| (l.text, l.sev)).collect())
            .unwrap_or_default();
        // ── LEVELS tab snapshot. Cloned only while the tab is showing: the catalog
        // holds a `String` or two per level, which is nothing, but there is no reason
        // to copy it on every frame of gameplay either.
        let levels_showing = self.props_open && self.panel_tab == PanelTab::Levels;
        let level_rows_ui: Vec<crate::world::persist::LevelEntry> = if levels_showing {
            self.level_rows.clone()
        } else {
            Vec::new()
        };
        let level_sel_ui = self.level_sel.clone();
        let level_current = self.current_level.clone();
        let level_display_name = self
            .world
            .as_ref()
            .map(|w| w.level_name().to_string())
            .unwrap_or_default();
        // Not `self.level_dirty()`: this runs under a live `&mut self.egui_state`
        // borrow, so it has to read the two fields rather than take `&self`.
        let saved_rev = self.saved_revision;
        let level_dirty_ui = self
            .world
            .as_ref()
            .is_some_and(|w| w.revision() != saved_rev);
        let level_status_ui = self.level_status.clone();
        let level_confirm = self.level_confirm_delete.clone();
        let mut level_name_ui = self.level_name_draft.clone();

        // PLAY tab snapshot + edit buffer. Cloned only while the tab is showing (it
        // holds a `Vec<LoadoutSlot>`), edited by value, written back after the `state`
        // borrow ends — the same discipline as the light and door editors.
        let play_showing = self.props_open && self.panel_tab == PanelTab::Play;
        let mut play_ui = self
            .world
            .as_ref()
            .filter(|_| play_showing)
            .map(|w| w.play_config().clone())
            .unwrap_or_default();
        let play_ctx_pins = self.world.as_ref().map(|w| w.play_pins()).unwrap_or_default();
        let play_in_hunt = self.world.as_ref().is_some_and(|w| !w.is_build());
        let mut play_changed = false;
        let mut play_start = false;

        // ── PAINT tab snapshot + deferred actions.
        let paint_probe_ui = self.paint_probe.clone();
        let paint_scheme_ui = self.paint_scheme;
        let paint_zone_ui = self.paint_zone;
        let paint_armed_ui = self.paint_armed;
        let paint_count = self.world.as_ref().map(|w| w.face_tex_count()).unwrap_or(0);
        let mut paint_set_scheme: Option<usize> = None;
        let mut paint_set_zone: Option<Option<u8>> = None;
        let mut paint_toggle_armed = false;
        let mut paint_apply_now = false;
        let mut paint_clear_face = false;
        let mut paint_clear_all = false;

        let mut nav_overlay_ui = self.world.as_ref().is_some_and(|w| w.nav_overlay_on());
        let mut nav_calculate = false;
        let mut nav_toggle_overlay = false;
        let mut real_lighting_ui = self.build_real_lighting;
        let mut set_real_lighting: Option<bool> = None;
        // TOOLS tab: what the stair/platform tools build with. Read once here (the egui
        // closure can't hold a `&World`), written back after it as one deferred action
        // each — a style change can convert the current selection, so it is a real edit.
        let build_style = self
            .world
            .as_ref()
            .map(|w| w.build_style())
            .unwrap_or_default();
        let mut set_platform_style: Option<engine::geometry::structures::PlatformStyle> = None;
        let mut set_platforms_are_floors: Option<bool> = None;
        let mut set_stair_shell: Option<engine::geometry::csg_runtime::StairShell> = None;
        let mut set_stair_run: Option<f32> = None;
        let panel_tab = self.panel_tab;
        let mut new_tab: Option<PanelTab> = None;
        // TEXTURES tab deferred actions. `Some(None)` on `arm` means "disarm".
        let mut new_theme_armed: Option<Option<usize>> = None;
        let mut new_theme_verdict: Option<(&'static str, crate::theme_review::Verdict)> = None;
        let mut new_theme_review_filter: Option<crate::theme_review::ReviewFilter> = None;
        let mut theme_filter_changed = false;
        // Editor deferred actions.
        let mut toggle_edit_mode = false;
        let mut draft_zone_changed = false;
        let mut draft_level_changed = false;
        let mut draft_name_changed = false;
        let mut draft_pick_texture: Option<&'static str> = None;
        let mut draft_new_repeat: Option<f32> = None;
        let mut draft_new_offset: Option<[f32; 2]> = None;
        let mut draft_new_height: Option<f32> = None;
        let mut draft_clear_zone = false;
        let mut draft_seed_from: Option<usize> = None;
        let mut draft_save_over = false;
        let mut draft_save = false;
        let mut draft_arm_scratch = false;
        // `(digit, Some(scheme))` binds, `(digit, None)` clears.
        let mut new_hotkey: Option<(char, Option<usize>)> = None;
        // LEVELS tab deferred actions.
        let mut level_save = false;
        let mut level_save_as = false;
        let mut level_rename = false;
        let mut level_duplicate = false;
        let mut level_refresh = false;
        let mut level_name_changed = false;
        let mut level_select: Option<std::path::PathBuf> = None;
        let mut level_load: Option<std::path::PathBuf> = None;
        let mut level_delete_click: Option<std::path::PathBuf> = None;

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
                            let sel = match sel_stats.as_ref() {
                                Some(s) => s,
                                None => return,
                            };
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
                                                            row.magazine_size
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

            // The room plan tool's status strip. BUILD has no HUD of its own (the
            // quad HUD is HUNT-only), and this tool has numeric state — which storey
            // the drafting plane is on, how tall the room will be — that is guesswork
            // without a read-out.
            //
            // `interactable(false)` is load-bearing: an ordinary egui window under the
            // pointer would swallow the clicks that place corners, and the strip sits
            // at the top of the screen where the author is drawing.
            if let Some(text) = room_status.as_deref() {
                egui::Area::new(egui::Id::new("room_status"))
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 10.0))
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190))
                            .inner_margin(egui::Margin::symmetric(10, 5))
                            .corner_radius(4.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(text).color(SHOP_GOLD).monospace(),
                                );
                            });
                    });
            }

            if props_open {
                egui::SidePanel::left("objects_panel")
                    .resizable(false)
                    .default_width(224.0)
                    .show(ctx, |ui| {
                        ui.add_space(4.0);
                        // ── Tabbed header: ◀ TITLE ▶ cycles the panel section (Objects
                        // / Lighting), wrapping around at either end.
                        ui.horizontal(|ui| {
                            if ui.button(egui::RichText::new("◀").color(SHOP_GOLD).strong()).clicked() {
                                new_tab = Some(panel_tab.prev());
                            }
                            ui.add_space(6.0);
                            ui.heading(egui::RichText::new(panel_tab.title()).color(SHOP_GOLD).strong());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(egui::RichText::new("▶").color(SHOP_GOLD).strong()).clicked() {
                                        new_tab = Some(panel_tab.next());
                                    }
                                },
                            );
                        });
                        ui.separator();

                        // ── Common: the current selection's editor (a light or a prop),
                        // shown on any tab so a click in the 3D view is always editable.
                        if selected_light.is_some() {
                            ui.label(
                                egui::RichText::new("SELECTED LIGHT")
                                    .small()
                                    .strong()
                                    .color(SHOP_GOLD_DIM),
                            );
                            ui.horizontal(|ui| {
                                ui.label("Colour");
                                if ui.color_edit_button_rgb(&mut light_color).changed() {
                                    light_edited = true;
                                }
                            });
                            if ui
                                .add(egui::Slider::new(&mut light_intensity, 0.0..=8.0).text("intensity"))
                                .changed()
                            {
                                light_edited = true;
                            }
                            if ui
                                .add(egui::Slider::new(&mut light_range, 0.5..=40.0).text("range m"))
                                .changed()
                            {
                                light_edited = true;
                            }
                            ui.label(
                                egui::RichText::new("drag the green Y arrow to raise · Del = delete")
                                    .small()
                                    .color(SHOP_DIM),
                            );
                            if ui
                                .button(egui::RichText::new("Delete").color(egui::Color32::from_rgb(230, 90, 90)))
                                .clicked()
                            {
                                delete_prop = true;
                            }
                            ui.separator();
                        } else if prop_selected {
                            ui.label(
                                egui::RichText::new("SELECTED PROP")
                                    .small()
                                    .strong()
                                    .color(SHOP_GOLD_DIM),
                            );
                            ui.label(format!("Gizmo: {gizmo_label}  (T switches)"));
                            ui.label(
                                egui::RichText::new(
                                    "drag handles · Ctrl = snap · Shift+D = duplicate · Del = delete · RMB = look",
                                )
                                .small()
                                .color(SHOP_DIM),
                            );
                            ui.horizontal(|ui| {
                                if ui.button("Ground").clicked() {
                                    ground_prop = true;
                                }
                                if ui
                                    .button(egui::RichText::new("Delete").color(egui::Color32::from_rgb(230, 90, 90)))
                                    .clicked()
                                {
                                    delete_prop = true;
                                }
                            });

                            // ── Pickup settings, when the selection is one ──
                            if selected_pickup.is_some() {
                                pickup_changed |= pickup_settings_ui(
                                    ui,
                                    &mut pickup_edit,
                                    &pickup_weapons,
                                );
                            }

                            // ── Door settings, when the selected prop is a door ──
                            if let Some(d) = door_edit.as_mut() {
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("DOOR")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                let swings = d.opening_type == crate::ecs::OpeningType::Swing;
                                if swings {
                                    ui.horizontal(|ui| {
                                        ui.label("Hinge");
                                        for (label, side) in [
                                            ("Left", crate::ecs::HingeSide::Left),
                                            ("Right", crate::ecs::HingeSide::Right),
                                        ] {
                                            if ui
                                                .selectable_label(d.hinge == side, label)
                                                .clicked()
                                                && d.hinge != side
                                            {
                                                d.hinge = side;
                                                door_changed = true;
                                            }
                                        }
                                    });
                                }
                                ui.horizontal(|ui| {
                                    let verb = if swings { "Swing out" } else { "Reverse" };
                                    if ui.checkbox(&mut d.flip, verb).changed() {
                                        door_changed = true;
                                    }
                                    if ui.checkbox(&mut d.mirrored, "Mirror").changed() {
                                        door_changed = true;
                                    }
                                });
                                if swings {
                                    let mut deg = d.open_angle.to_degrees();
                                    if ui
                                        .add(egui::Slider::new(&mut deg, 30.0..=170.0).text("open °"))
                                        .changed()
                                    {
                                        d.open_angle = deg.to_radians();
                                        door_changed = true;
                                    }
                                }
                                if ui
                                    .add(egui::Slider::new(&mut d.speed, 0.2..=3.0).text("speed"))
                                    .changed()
                                {
                                    door_changed = true;
                                }
                                if ui
                                    .add(
                                        egui::Slider::new(&mut d.auto_close, 0.0..=20.0)
                                            .text("shuts after s"),
                                    )
                                    .changed()
                                {
                                    door_changed = true;
                                }
                                if ui
                                    .add(
                                        egui::Slider::new(&mut d.use_radius, 0.5..=5.0)
                                            .text("reach m"),
                                    )
                                    .changed()
                                {
                                    door_changed = true;
                                }
                                egui::ComboBox::from_label("opens for")
                                    .selected_text(match d.access {
                                        crate::ecs::DoorAccess::Both => "Everyone",
                                        crate::ecs::DoorAccess::PlayerOnly => "Player only",
                                        crate::ecs::DoorAccess::HuntersOnly => "Hunters only",
                                        crate::ecs::DoorAccess::Locked => "Locked",
                                    })
                                    .show_ui(ui, |ui| {
                                        for (label, a) in [
                                            ("Everyone", crate::ecs::DoorAccess::Both),
                                            ("Player only", crate::ecs::DoorAccess::PlayerOnly),
                                            ("Hunters only", crate::ecs::DoorAccess::HuntersOnly),
                                            ("Locked", crate::ecs::DoorAccess::Locked),
                                        ] {
                                            if ui
                                                .selectable_value(&mut d.access, a, label)
                                                .clicked()
                                            {
                                                door_changed = true;
                                            }
                                        }
                                    });
                                ui.label(
                                    egui::RichText::new(
                                        "0 s = stays open · Mirror flips the handle side (double doors)",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                            }
                            ui.separator();
                        }

                        // Leave placement / clear selection so clicks select existing
                        // objects (also the Q / Esc key).
                        if placing_prop || placing_light || placing_spawn || prop_selected {
                            let label = if placing_prop || placing_light || placing_spawn {
                                "Stop placing (Q)"
                            } else {
                                "Deselect (Q)"
                            };
                            if ui.button(label).clicked() {
                                go_neutral = true;
                            }
                        }
                        if ui.button("CLOSE (O)").clicked() {
                            close_props = true;
                        }
                        ui.separator();

                        // ── Per-tab content.
                        match panel_tab {
                            PanelTab::Play if play_showing => {
                                // The loadout combo lists the same guns a pickup can
                                // name: the live arsenal minus the empty-handed slot.
                                let ctx = PlayTabCtx {
                                    pins: play_ctx_pins,
                                    pads: spawn_pad_count,
                                    weapons: &pickup_weapons,
                                    in_hunt: play_in_hunt,
                                };
                                let (ch, st) = play_tab_ui(ui, &mut play_ui, &ctx);
                                play_changed |= ch;
                                play_start |= st;
                            }
                            PanelTab::Tools => {
                                use engine::geometry::csg_runtime::StairShell;
                                use engine::geometry::structures::PlatformStyle;
                                let dim = |s: &str| {
                                    egui::RichText::new(s).small().color(SHOP_DIM)
                                };
                                let head = |s: &str| {
                                    egui::RichText::new(s).small().strong().color(SHOP_GOLD_DIM)
                                };

                                ui.label(head("PLATFORMS (T)"));
                                for (style, label, hint) in [
                                    (
                                        PlatformStyle::Solid,
                                        "Slab",
                                        "skirted block, blue platform texture",
                                    ),
                                    (
                                        PlatformStyle::Plane,
                                        "Plane",
                                        "flat, two-sided, the room's floor texture",
                                    ),
                                ] {
                                    if ui
                                        .selectable_label(
                                            build_style.platform == style,
                                            label,
                                        )
                                        .clicked()
                                    {
                                        set_platform_style = Some(style);
                                    }
                                    ui.label(dim(hint));
                                }
                                ui.add_space(6.0);

                                ui.label(head("PLATFORMS AS FLOORS"));
                                let mut decks_are_floors = platforms_are_floors;
                                if ui
                                    .checkbox(
                                        &mut decks_are_floors,
                                        "deck top restarts the wall band",
                                    )
                                    .on_hover_text(
                                        "re-folds every region — a band boundary is \
                                         geometry, not a material",
                                    )
                                    .changed()
                                {
                                    set_platforms_are_floors = Some(decks_are_floors);
                                }
                                ui.label(dim("off: walls band from the carved room"));
                                ui.label(dim("alone, so dropping a platform in never"));
                                ui.label(dim("restyles the wall behind it"));
                                ui.label(dim("on: a deck reads as a mezzanine floor"));
                                ui.add_space(6.0);

                                ui.label(head("STAIRS (K / C / \u{2191}\u{2193})"));
                                for (shell, label, hint) in [
                                    (StairShell::Steps, "Steps", "treads and risers"),
                                    (StairShell::Ramp, "Ramp", "the bare slope, no steps"),
                                ] {
                                    if ui
                                        .selectable_label(build_style.stairs == shell, label)
                                        .clicked()
                                    {
                                        set_stair_shell = Some(shell);
                                    }
                                    ui.label(dim(hint));
                                }
                                ui.add_space(6.0);

                                ui.label(head("SLOPE (\u{2191}\u{2193} STAIRS ONLY)"));
                                ui.label(dim("run per step \u{2014} K and C take their slope"));
                                ui.label(dim("from where you click instead"));
                                ui.add_space(2.0);
                                for run in crate::world::STAIR_RUN_PRESETS {
                                    if ui
                                        .selectable_label(
                                            (build_style.stair_run - run).abs() < 0.01,
                                            crate::world::stair_run_label(run),
                                        )
                                        .clicked()
                                    {
                                        set_stair_run = Some(run);
                                    }
                                }
                                ui.add_space(2.0);
                                ui.label(dim(
                                    "steeper than 45\u{b0} isn't offered: the player can",
                                ));
                                ui.label(dim("only climb 50\u{b0}, and the slope is what"));
                                ui.label(dim("you walk in both shells"));
                                ui.add_space(6.0);
                                ui.label(dim(
                                    "with a platform or stair-run selected, changing",
                                ));
                                ui.label(dim("a shell converts that one too"));
                            }
                            PanelTab::Play => {}
                            PanelTab::Lighting => {
                                if ui
                                    .selectable_label(placing_light, "+ Place Point Light")
                                    .clicked()
                                {
                                    toggle_light_place = true;
                                }
                                ui.label(
                                    egui::RichText::new("click the floor to drop a light")
                                        .small()
                                        .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);
                                if ui
                                    .checkbox(&mut real_lighting_ui, "Real lighting (L)")
                                    .changed()
                                {
                                    set_real_lighting = Some(real_lighting_ui);
                                }
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("AMBIENT (whole level)")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Colour");
                                    if ui.color_edit_button_rgb(&mut amb_color).changed() {
                                        ambient_edited = true;
                                    }
                                });
                                if ui
                                    .add(egui::Slider::new(&mut amb_level, 0.0..=1.0).text("level"))
                                    .changed()
                                {
                                    ambient_edited = true;
                                }
                            }
                            PanelTab::Spawns => {
                                if ui
                                    .selectable_label(placing_spawn, "+ Place Spawn Point")
                                    .clicked()
                                {
                                    toggle_spawn_place = true;
                                }
                                ui.label(
                                    egui::RichText::new(
                                        "click the floor to drop a pad — it faces the way \
                                         the camera is looking",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!("{spawn_pad_count} PAD(S) AUTHORED"))
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "At G everyone — you and the simulants — enters from \
                                         this one shared pool. Click a pad to select it: the \
                                         Move gizmo repositions it, T switches to Rotate to \
                                         re-aim its facing, Del removes it.",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "With no pads authored the level falls back to the \
                                         old fixed red marker and you enter under the camera.",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                            }
                            PanelTab::Nav => {
                                ui.label(
                                    egui::RichText::new(
                                        "Can the hunters walk your level? They move on a \
                                         0.25 m grid that climbs one cell and never jumps \
                                         — you autostep the same, but you also fall and \
                                         jump 0.76 m, so you can reach places they cannot.",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);
                                if ui
                                    .add_sized(
                                        [200.0, 28.0],
                                        egui::Button::new(
                                            egui::RichText::new("CALCULATE")
                                                .color(egui::Color32::BLACK)
                                                .strong(),
                                        )
                                        .fill(SHOP_GOLD),
                                    )
                                    .on_hover_text(
                                        "Bakes the nav grid and checks it — about half a \
                                         second on a big level, which is why it isn't live",
                                    )
                                    .clicked()
                                {
                                    nav_calculate = true;
                                }
                                if ui
                                    .checkbox(&mut nav_overlay_ui, "Show walkable overlay")
                                    .on_hover_text(
                                        "Green = the main walkable area. Any other colour is \
                                         an island nothing can reach. Red posts are corridors \
                                         too narrow for a body; orange posts are steps that \
                                         would reconnect an island.",
                                    )
                                    .changed()
                                {
                                    nav_toggle_overlay = true;
                                }
                                ui.separator();
                                if nav_lines.is_empty() {
                                    ui.label(
                                        egui::RichText::new(
                                            "Nothing calculated yet — press CALCULATE.",
                                        )
                                        .small()
                                        .color(SHOP_DIM),
                                    );
                                } else {
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        ui.set_width(206.0);
                                        for (text, sev) in &nav_lines {
                                            use crate::world::NavSeverity as S;
                                            let col = match sev {
                                                S::Ok => NAV_OK,
                                                S::Info => SHOP_DIM,
                                                S::Warn => SHOP_GOLD,
                                                S::Error => NAV_BAD,
                                            };
                                            ui.label(
                                                egui::RichText::new(text).small().color(col),
                                            );
                                        }
                                    });
                                }
                            }
                            PanelTab::Levels => {
                                ui.label(
                                    egui::RichText::new(
                                        "Every level is a file in native/levels/. \
                                         F1-F8 still load slot1-slot8 \u{2014} those are \
                                         just eight of the files listed below.",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(6.0);

                                // ── What is open, and whether it is saved.
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("OPEN")
                                            .small()
                                            .strong()
                                            .color(SHOP_GOLD_DIM),
                                    );
                                    let (text, col) = match (&level_current, level_dirty_ui) {
                                        (Some(_), true) => {
                                            (format!("{level_display_name} *"), SHOP_GOLD)
                                        }
                                        (Some(_), false) => (level_display_name.clone(), NAV_OK),
                                        (None, _) => {
                                            ("never saved".to_string(), SHOP_GOLD)
                                        }
                                    };
                                    ui.label(egui::RichText::new(text).color(col).strong())
                                        .on_hover_text(
                                            "* means there are edits this file \
                                             doesn't have yet",
                                        );
                                });
                                ui.label(
                                    egui::RichText::new(match &level_current {
                                        Some(p) => short_path(p),
                                        None => "no file behind it \u{2014} name it and Save As"
                                            .to_string(),
                                    })
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);
                                ui.add_enabled_ui(level_current.is_some(), |ui| {
                                    if ui
                                        .add_sized(
                                            [200.0, 26.0],
                                            egui::Button::new(
                                                egui::RichText::new(if level_dirty_ui {
                                                    "SAVE *"
                                                } else {
                                                    "SAVE"
                                                })
                                                .color(egui::Color32::BLACK)
                                                .strong(),
                                            )
                                            .fill(SHOP_GOLD),
                                        )
                                        .on_hover_text("Ctrl+S \u{2014} overwrites this level's own file")
                                        .clicked()
                                    {
                                        level_save = true;
                                    }
                                });

                                // ── The catalog.
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "ON DISK ({})",
                                            level_rows_ui.len()
                                        ))
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                    );
                                    if ui
                                        .small_button("re-read")
                                        .on_hover_text("in case something changed the files")
                                        .clicked()
                                    {
                                        level_refresh = true;
                                    }
                                });
                                if level_rows_ui.is_empty() {
                                    ui.label(
                                        egui::RichText::new(
                                            "No levels saved yet \u{2014} name this one \
                                             below and Save As.",
                                        )
                                        .small()
                                        .color(SHOP_DIM),
                                    );
                                }
                                egui::ScrollArea::vertical()
                                    .max_height(200.0)
                                    .show(ui, |ui| {
                                        ui.set_width(206.0);
                                        for row in &level_rows_ui {
                                            let is_sel =
                                                level_sel_ui.as_ref() == Some(&row.path);
                                            let is_open =
                                                level_current.as_ref() == Some(&row.path);
                                            let title = match row.slot {
                                                Some(n) => format!("{}   F{n}", row.name),
                                                None => row.name.clone(),
                                            };
                                            let col = if row.error.is_some() {
                                                NAV_BAD
                                            } else if is_open {
                                                NAV_OK
                                            } else {
                                                egui::Color32::from_gray(200)
                                            };
                                            let resp = ui.selectable_label(
                                                is_sel,
                                                egui::RichText::new(title).color(col),
                                            );
                                            if resp.clicked() {
                                                level_select = Some(row.path.clone());
                                            }
                                            if resp.double_clicked() && row.error.is_none() {
                                                level_load = Some(row.path.clone());
                                            }
                                            let sub = match &row.error {
                                                Some(e) => format!("unreadable \u{2014} {e}"),
                                                None => format!(
                                                    "{}  \u{b7}  {} brushes  \u{b7}  {} objects{}",
                                                    human_bytes(row.bytes),
                                                    row.brushes,
                                                    row.entities,
                                                    if row.from_newer_build() {
                                                        format!("  \u{b7}  v{} (newer build)", row.version)
                                                    } else {
                                                        String::new()
                                                    }
                                                ),
                                            };
                                            ui.label(
                                                egui::RichText::new(sub).small().color(
                                                    if row.error.is_some() {
                                                        NAV_BAD
                                                    } else {
                                                        SHOP_DIM
                                                    },
                                                ),
                                            );
                                            ui.add_space(2.0);
                                        }
                                    });

                                // ── Act on the selected row.
                                let sel_row = level_rows_ui
                                    .iter()
                                    .find(|r| Some(&r.path) == level_sel_ui.as_ref());
                                let loadable =
                                    sel_row.is_some_and(|r| r.error.is_none());
                                let delete_armed = level_confirm.is_some()
                                    && level_confirm == level_sel_ui;
                                ui.horizontal(|ui| {
                                    ui.add_enabled_ui(loadable, |ui| {
                                        if ui
                                            .add_sized(
                                                [108.0, 24.0],
                                                egui::Button::new(
                                                    egui::RichText::new("LOAD")
                                                        .color(egui::Color32::BLACK)
                                                        .strong(),
                                                )
                                                .fill(SHOP_GOLD),
                                            )
                                            .on_hover_text(
                                                "replaces what you're editing \u{2014} \
                                                 Ctrl+Z undoes it",
                                            )
                                            .clicked()
                                        {
                                            level_load = level_sel_ui.clone();
                                        }
                                    });
                                    ui.add_enabled_ui(sel_row.is_some(), |ui| {
                                        if ui
                                            .add_sized(
                                                [88.0, 24.0],
                                                egui::Button::new(
                                                    egui::RichText::new(if delete_armed {
                                                        "REALLY?"
                                                    } else {
                                                        "Delete"
                                                    })
                                                    .color(if delete_armed {
                                                        NAV_BAD
                                                    } else {
                                                        SHOP_DIM
                                                    }),
                                                ),
                                            )
                                            .on_hover_text(
                                                "click twice \u{2014} deleting the file \
                                                 cannot be undone",
                                            )
                                            .clicked()
                                        {
                                            level_delete_click = level_sel_ui.clone();
                                        }
                                    });
                                });

                                // ── Name it: Save As / Rename / Duplicate.
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("NAME")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut level_name_ui)
                                            .hint_text("level name")
                                            .desired_width(200.0),
                                    )
                                    .changed()
                                {
                                    level_name_changed = true;
                                }
                                let slug =
                                    crate::world::persist::slug_for_name(&level_name_ui);
                                ui.label(
                                    egui::RichText::new(if slug.is_empty() {
                                        "needs a letter or a digit".to_string()
                                    } else {
                                        format!("{slug}.json")
                                    })
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_enabled_ui(!slug.is_empty(), |ui| {
                                    if ui
                                        .add_sized(
                                            [200.0, 24.0],
                                            egui::Button::new("Save As a new level"),
                                        )
                                        .on_hover_text(
                                            "writes a new file, and refuses if that \
                                             name is already taken",
                                        )
                                        .clicked()
                                    {
                                        level_save_as = true;
                                    }
                                    ui.horizontal(|ui| {
                                        let has_sel = level_sel_ui.is_some();
                                        if ui
                                            .add_enabled(
                                                has_sel,
                                                egui::Button::new("Rename"),
                                            )
                                            .on_hover_text(
                                                "renames the selected level and moves \
                                                 its file to match",
                                            )
                                            .clicked()
                                        {
                                            level_rename = true;
                                        }
                                        if ui
                                            .add_enabled(
                                                has_sel,
                                                egui::Button::new("Duplicate"),
                                            )
                                            .on_hover_text(
                                                "copies the selected level to this name",
                                            )
                                            .clicked()
                                        {
                                            level_duplicate = true;
                                        }
                                    });
                                });
                                if !level_status_ui.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&level_status_ui)
                                            .small()
                                            .color(SHOP_GOLD_DIM),
                                    );
                                }
                            }
                            PanelTab::Objects => {
                                // A 3D turntable of the highlighted catalog prop, above
                                // the (scrolling) list.
                                // The hint names the surface the *highlighted* prop
                                // actually mounts on: a sentry gun bolts to a ceiling
                                // and refuses a floor pick, so a blanket "click floor"
                                // reads as the tool being broken.
                                let mounts_overhead = prop_sel
                                    .and_then(|i| crate::props::CATALOG.get(i))
                                    .is_some_and(|d| crate::props::ceiling_mounted(d.mesh));
                                ui.label(
                                    egui::RichText::new(if mounts_overhead {
                                        "PLACE PROP — click a ceiling to hang it"
                                    } else {
                                        "PLACE PROP — click floor to drop"
                                    })
                                    .small()
                                    .strong()
                                    .color(SHOP_GOLD_DIM),
                                );
                                // What the turntable is showing: a catalog prop, or —
                                // for a weapon pickup, which has no catalog row — the
                                // gun the pickup names.
                                let preview_name = armed_weapon
                                    .or_else(|| {
                                        prop_sel
                                            .and_then(|i| crate::props::CATALOG.get(i))
                                            .map(|d| d.name)
                                    });
                                if let Some(name) = preview_name {
                                    ui.group(|ui| {
                                        ui.set_min_size(egui::vec2(196.0, 132.0));
                                        ui.vertical_centered(|ui| match preview_tex {
                                            Some(tex) => {
                                                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                                    tex,
                                                    egui::vec2(128.0, 128.0),
                                                )));
                                            }
                                            None => {
                                                ui.add_space(54.0);
                                                ui.label(
                                                    egui::RichText::new("3D PREVIEW")
                                                        .color(SHOP_GOLD_DIM)
                                                        .strong(),
                                                );
                                            }
                                        });
                                    });
                                    ui.label(egui::RichText::new(name).color(SHOP_GOLD).strong());
                                }
                                // While a pickup is armed, its settings sit above the
                                // list — so the author sets "which gun / how much ammo
                                // / how long until it returns" and then clicks the
                                // floor repeatedly without re-opening anything.
                                if pickup_armed && selected_pickup.is_none() {
                                    pickup_changed |= pickup_settings_ui(
                                        ui,
                                        &mut pickup_edit,
                                        &pickup_weapons,
                                    );
                                    ui.separator();
                                }
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
                                        // The guns are listed under Pickups but come
                                        // from the ARSENAL, not the prop catalog — a
                                        // weapon pickup draws the gun it names rather
                                        // than a prop mesh, so there is no catalog row
                                        // to iterate. One row per weapon, above the two
                                        // ammo crates.
                                        if cat == crate::props::PropCategory::Pickups {
                                            for name in &pickup_weapons {
                                                let on = armed_weapon == Some(*name);
                                                if ui
                                                    .selectable_label(on, format!("▸ {name}"))
                                                    .clicked()
                                                {
                                                    arm_weapon_pickup = Some(name);
                                                }
                                            }
                                        }
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
                            }
                            PanelTab::Paint => {
                                ui.label(
                                    egui::RichText::new("ONE FACE AT A TIME")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "point at a surface — the readout is what the \
                                         renderer decided, not a second guess",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);

                                // ── What is under the cursor, and why.
                                match paint_probe_ui.as_ref() {
                                    None => {
                                        ui.label(
                                            egui::RichText::new("— nothing under the cursor —")
                                                .small()
                                                .color(SHOP_DIM),
                                        );
                                    }
                                    Some(p) => {
                                        ui.label(
                                            egui::RichText::new(p.face_label())
                                                .strong()
                                                .color(SHOP_GOLD),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "region {} · {} · {}",
                                                p.region_id,
                                                zone_name(p.zone),
                                                engine::render::textures::scheme_name(p.scheme),
                                            ))
                                            .small()
                                            .color(SHOP_TEXT),
                                        );
                                        if p.overridden {
                                            ui.label(
                                                egui::RichText::new("✎ painted")
                                                    .small()
                                                    .color(SHOP_GOLD),
                                            );
                                        }
                                        if let Some(why) = p.blocked {
                                            ui.label(
                                                egui::RichText::new(format!("✕ {why}"))
                                                    .small()
                                                    .color(SHOP_DIM),
                                            );
                                        }
                                        // The tell for the defect this tab was built
                                        // for: one fold triangle wearing two themes.
                                        if p.distinct_schemes > 1 {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "⚠ this triangle spans {} themes",
                                                    p.distinct_schemes
                                                ))
                                                .small()
                                                .strong()
                                                .color(egui::Color32::from_rgb(226, 132, 60)),
                                            );
                                        }
                                        egui::CollapsingHeader::new(format!(
                                            "WHY ({} candidate{}, {} fragment{})",
                                            p.candidates.len(),
                                            if p.candidates.len() == 1 { "" } else { "s" },
                                            p.fragments,
                                            if p.fragments == 1 { "" } else { "s" },
                                        ))
                                        .id_salt("paint-why")
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(
                                                    "a brush containing the surface beats \
                                                     one merely near it; then nearest, \
                                                     then smaller",
                                                )
                                                .small()
                                                .color(SHOP_DIM),
                                            );
                                            for c in &p.candidates {
                                                let mark = if c.chosen { "✓" } else { " " };
                                                // Not a disqualification: a face that
                                                // does not contain the surface can still
                                                // win on the slop tier, if nothing does.
                                                let miss =
                                                    if c.contains { "" } else { "  (near)" };
                                                let ovr = if c.overridden { " ✎" } else { "" };
                                                let sign =
                                                    if c.side == Side::Max { '+' } else { '-' };
                                                let axis = match c.axis {
                                                    Axis::X => 'X',
                                                    Axis::Y => 'Y',
                                                    Axis::Z => 'Z',
                                                };
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{mark} brush {} {sign}{axis}  \
                                                         d {:.3}  vol {:.0}  {}{ovr}{miss}",
                                                        c.brush_id,
                                                        c.dist,
                                                        c.volume,
                                                        engine::render::textures::scheme_name(
                                                            c.scheme
                                                        ),
                                                    ))
                                                    .small()
                                                    .monospace()
                                                    .color(if c.chosen {
                                                        SHOP_GOLD
                                                    } else {
                                                        SHOP_DIM
                                                    }),
                                                );
                                            }
                                        });
                                    }
                                }
                                ui.separator();

                                // ── What a click paints with.
                                ui.label(
                                    egui::RichText::new("PAINT WITH")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                ui.label(
                                    egui::RichText::new(format!("theme: {paint_scheme_label}"))
                                        .small()
                                        .color(SHOP_TEXT),
                                );
                                egui::ComboBox::from_label("slot")
                                    .selected_text(match paint_zone_ui {
                                        None => "(keep)".to_string(),
                                        Some(z) => zone_name(z).to_string(),
                                    })
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(paint_zone_ui.is_none(), "(keep)")
                                            .on_hover_text(
                                                "a wall stays a wall, a floor a floor — only \
                                                 the theme changes",
                                            )
                                            .clicked()
                                        {
                                            paint_set_zone = Some(None);
                                        }
                                        for (z, label) in crate::theme_editor::EDITABLE_ZONES {
                                            if ui
                                                .selectable_label(
                                                    paint_zone_ui == Some(z),
                                                    label,
                                                )
                                                .clicked()
                                            {
                                                paint_set_zone = Some(Some(z));
                                            }
                                        }
                                    });
                                if paint_zone_ui.is_some() {
                                    ui.label(
                                        egui::RichText::new(
                                            "forcing a slot flattens a wall's lower/upper band",
                                        )
                                        .small()
                                        .color(SHOP_DIM),
                                    );
                                }
                                ui.add_space(2.0);

                                let can_paint = paint_probe_ui
                                    .as_ref()
                                    .is_some_and(|p| p.target().is_some());
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(
                                            paint_armed_ui,
                                            if paint_armed_ui {
                                                "Armed — click faces (Q to stop)"
                                            } else {
                                                "Arm brush"
                                            },
                                        )
                                        .clicked()
                                    {
                                        paint_toggle_armed = true;
                                    }
                                    if ui
                                        .add_enabled(can_paint, egui::Button::new("Paint this"))
                                        .clicked()
                                    {
                                        paint_apply_now = true;
                                    }
                                });
                                let painted_here = paint_probe_ui
                                    .as_ref()
                                    .is_some_and(|p| p.overridden && p.target().is_some());
                                if ui
                                    .add_enabled(painted_here, egui::Button::new("Clear this face"))
                                    .clicked()
                                {
                                    paint_clear_face = true;
                                }
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{paint_count} painted face{}",
                                            if paint_count == 1 { "" } else { "s" }
                                        ))
                                        .small()
                                        .color(SHOP_TEXT),
                                    );
                                    if ui
                                        .add_enabled(
                                            paint_count > 0,
                                            egui::Button::new("Clear all"),
                                        )
                                        .clicked()
                                    {
                                        paint_clear_all = true;
                                    }
                                });
                                ui.separator();

                                // ── The theme list, shared with the TEXTURES tab.
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut theme_filter_ui)
                                            .hint_text("filter by name or level")
                                            .desired_width(196.0),
                                    )
                                    .changed()
                                {
                                    theme_filter_changed = true;
                                }
                                egui::ScrollArea::vertical()
                                    .id_salt("paint-themes")
                                    .show(ui, |ui| {
                                        ui.set_width(206.0);
                                        for (row, swatches) in
                                            theme_rows.iter().zip(theme_swatches.iter())
                                        {
                                            // An undefined zone still takes its slot,
                                            // or rows of different length would make
                                            // the list jitter as you scroll it.
                                            ui.horizontal(|ui| {
                                                for (zi, sw) in swatches.iter().enumerate() {
                                                    let name = zone_name(zi as u8);
                                                    match sw {
                                                        Some(h) => {
                                                            ui.add(egui::Image::new(
                                                                egui::load::SizedTexture::new(
                                                                    h.id(),
                                                                    egui::vec2(22.0, 22.0),
                                                                ),
                                                            ))
                                                            .on_hover_text(name);
                                                        }
                                                        None => {
                                                            let (rect, _) = ui
                                                                .allocate_exact_size(
                                                                    egui::vec2(22.0, 22.0),
                                                                    egui::Sense::hover(),
                                                                );
                                                            ui.painter().rect_stroke(
                                                                rect,
                                                                0.0,
                                                                egui::Stroke::new(1.0, SHOP_DIM),
                                                                egui::StrokeKind::Inside,
                                                            );
                                                        }
                                                    }
                                                }
                                            });
                                            if ui
                                                .selectable_label(
                                                    paint_scheme_ui == row.idx,
                                                    row.label.as_str(),
                                                )
                                                .on_hover_text(row.name)
                                                .clicked()
                                            {
                                                paint_set_scheme = Some(row.idx);
                                            }
                                        }
                                        if theme_rows.is_empty() {
                                            ui.label(
                                                egui::RichText::new("no themes match")
                                                    .small()
                                                    .color(SHOP_DIM),
                                            );
                                        }
                                    });
                            }
                            PanelTab::Textures if theme_edit_mode => {
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(false, "◄ Library").clicked() {
                                        toggle_edit_mode = true;
                                    }
                                    ui.label(
                                        egui::RichText::new("BUILD A THEME")
                                            .small()
                                            .strong()
                                            .color(SHOP_GOLD),
                                    );
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "edits show instantly — the preview room above \
                                         and any room wearing the scratch theme",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);

                                // ── Which zone are we editing?
                                ui.label(
                                    egui::RichText::new("ZONE")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                egui::ComboBox::from_id_salt("draft-zone")
                                    .width(196.0)
                                    .selected_text(
                                        crate::theme_editor::EDITABLE_ZONES
                                            .iter()
                                            .find(|(z, _)| *z == draft_zone_sel)
                                            .map(|(_, n)| *n)
                                            .unwrap_or("?"),
                                    )
                                    .show_ui(ui, |ui| {
                                        for (z, name) in crate::theme_editor::EDITABLE_ZONES {
                                            if ui
                                                .selectable_value(&mut draft_zone_sel, z, name)
                                                .clicked()
                                            {
                                                draft_zone_changed = true;
                                            }
                                        }
                                    });

                                let cur = draft_zones
                                    .get(draft_zone_sel as usize)
                                    .copied()
                                    .flatten();
                                match cur {
                                    Some(z) => {
                                        ui.label(
                                            egui::RichText::new(
                                                z.texture.replace("tempImgEd", "~"),
                                            )
                                            .color(SHOP_GOLD)
                                            .strong(),
                                        );
                                        let mut rep = z.repeat;
                                        if ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut rep,
                                                    crate::theme_editor::REPEAT_RANGE,
                                                )
                                                .logarithmic(true)
                                                .text("scale"),
                                            )
                                            .changed()
                                        {
                                            draft_new_repeat = Some(rep);
                                        }
                                        let mut off = z.offset;
                                        let mut off_changed = false;
                                        off_changed |= ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut off[0],
                                                    crate::theme_editor::OFFSET_RANGE,
                                                )
                                                .text("shift U"),
                                            )
                                            .changed();
                                        off_changed |= ui
                                            .add(
                                                egui::Slider::new(
                                                    &mut off[1],
                                                    crate::theme_editor::OFFSET_RANGE,
                                                )
                                                .text("shift V"),
                                            )
                                            .changed();
                                        if off_changed {
                                            draft_new_offset = Some(off);
                                        }
                                        // The cornice is the one band whose extent the
                                        // theme decides rather than the geometry, so it
                                        // is the one zone with a depth to drag.
                                        if self.theme_draft.zone_sel
                                            == engine::render::textures::CORNICE_ZONE
                                        {
                                            let mut h = z.height;
                                            if ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut h,
                                                        crate::theme_editor::CORNICE_RANGE,
                                                    )
                                                    .text("depth (WT)"),
                                                )
                                                .changed()
                                            {
                                                draft_new_height = Some(h);
                                            }
                                        }
                                        if ui
                                            .button("Clear zone")
                                            .on_hover_text(
                                                "an undefined zone renders INVISIBLE, \
                                                 not untextured",
                                            )
                                            .clicked()
                                        {
                                            draft_clear_zone = true;
                                        }
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new(
                                                "zone undefined — invisible until you \
                                                 pick a texture",
                                            )
                                            .small()
                                            .color(SHOP_DIM),
                                        );
                                    }
                                }
                                ui.separator();

                                // ── Texture picker, grouped by the GoldenEye level the
                                // texture came from. A flat 1016-row list is unusable;
                                // "which level was this from" is how you actually think.
                                ui.label(
                                    egui::RichText::new("TEXTURE — from level")
                                        .small()
                                        .strong()
                                        .color(SHOP_GOLD_DIM),
                                );
                                egui::ComboBox::from_id_salt("draft-level")
                                    .width(196.0)
                                    .selected_text(draft_level_sel.clone())
                                    .show_ui(ui, |ui| {
                                        for lv in &level_groups {
                                            if ui
                                                .selectable_value(
                                                    &mut draft_level_sel,
                                                    lv.clone(),
                                                    lv,
                                                )
                                                .clicked()
                                            {
                                                draft_level_changed = true;
                                            }
                                        }
                                    });
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} textures",
                                        picker_textures.len()
                                    ))
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                egui::ScrollArea::vertical()
                                    .max_height(210.0)
                                    .id_salt("draft-tex-grid")
                                    .show(ui, |ui| {
                                        ui.set_width(206.0);
                                        let sel_tex = cur.map(|z| z.texture);
                                        // 5 across at 36px — enough to scan a level's
                                        // palette without endless scrolling.
                                        egui::Grid::new("tex-grid").spacing([2.0, 2.0]).show(
                                            ui,
                                            |ui| {
                                                for (n, (name, sw)) in picker_textures
                                                    .iter()
                                                    .zip(picker_swatches.iter())
                                                    .enumerate()
                                                {
                                                    let selected = sel_tex == Some(*name);
                                                    let resp = match sw {
                                                        Some(h) => ui.add(
                                                            egui::ImageButton::new(
                                                                egui::load::SizedTexture::new(
                                                                    h.id(),
                                                                    egui::vec2(36.0, 36.0),
                                                                ),
                                                            )
                                                            .selected(selected),
                                                        ),
                                                        None => ui.add_sized(
                                                            egui::vec2(36.0, 36.0),
                                                            egui::Button::new("?"),
                                                        ),
                                                    };
                                                    if resp
                                                        .on_hover_text(
                                                            name.replace("tempImgEd", "~"),
                                                        )
                                                        .clicked()
                                                    {
                                                        draft_pick_texture = Some(name);
                                                    }
                                                    if n % 5 == 4 {
                                                        ui.end_row();
                                                    }
                                                }
                                            },
                                        );
                                    });
                                ui.separator();

                                // ── Try it, then keep it.
                                if ui
                                    .selectable_label(
                                        theme_armed
                                            == Some(engine::render::textures::scratch_scheme()),
                                        "Apply scratch to a room (click)",
                                    )
                                    .on_hover_text(
                                        "the scratch theme is shared — editing again \
                                         changes every room wearing it",
                                    )
                                    .clicked()
                                {
                                    draft_arm_scratch = true;
                                }
                                ui.add_space(4.0);
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut draft_save_name)
                                            .hint_text("preset name")
                                            .desired_width(196.0),
                                    )
                                    .changed()
                                {
                                    draft_name_changed = true;
                                }
                                // Save back over the slot this draft came from, when
                                // it came from an editable one. Gated on `dirty` so an
                                // unchanged draft cannot overwrite its own source by a
                                // stray click, and labelled with the target so the
                                // button never lands somewhere the text did not say.
                                if let Some(target) = draft_overwrite_label.as_deref() {
                                    if ui
                                        .add_enabled(
                                            draft_dirty,
                                            egui::Button::new(format!("Save to \"{target}\"")),
                                        )
                                        .on_hover_text(if draft_dirty {
                                            "overwrites this preset in user_themes.json"
                                        } else {
                                            "no changes to save"
                                        })
                                        .clicked()
                                    {
                                        draft_save_over = true;
                                    }
                                }
                                let free = engine::render::textures::first_free_custom_slot();
                                if ui
                                    .add_enabled(
                                        free.is_some(),
                                        egui::Button::new(if draft_dirty {
                                            "Save as new preset *"
                                        } else {
                                            "Save as new preset"
                                        }),
                                    )
                                    .on_hover_text(match free {
                                        Some(_) => "writes a new slot in user_themes.json",
                                        None => "all custom slots are full",
                                    })
                                    .clicked()
                                {
                                    draft_save = true;
                                }
                                if !theme_status.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&theme_status)
                                            .small()
                                            .color(SHOP_GOLD_DIM),
                                    );
                                }
                            }
                            PanelTab::Textures => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("APPLY THEME")
                                            .small()
                                            .strong()
                                            .color(SHOP_GOLD_DIM),
                                    );
                                    if ui.selectable_label(false, "Build one ►").clicked() {
                                        toggle_edit_mode = true;
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "the whole room retextures (doors bound it)",
                                    )
                                    .small()
                                    .color(SHOP_DIM),
                                );
                                ui.add_space(4.0);

                                // Review tally + verdict filter. Most of this library
                                // exists to be pruned, so progress is front and centre.
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{theme_total} themes · {kept_n} kept · {cut_n} cut · \
                                         {new_n} unreviewed"
                                    ))
                                    .small()
                                    .color(SHOP_TEXT),
                                );
                                ui.horizontal(|ui| {
                                    for f in crate::theme_review::ReviewFilter::ALL {
                                        if ui
                                            .selectable_label(theme_review_filter == f, f.label())
                                            .clicked()
                                        {
                                            new_theme_review_filter = Some(f);
                                        }
                                    }
                                });
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut theme_filter_ui)
                                            .hint_text("filter by name or level")
                                            .desired_width(196.0),
                                    )
                                    .changed()
                                {
                                    theme_filter_changed = true;
                                }
                                ui.add_space(2.0);
                                if theme_armed.is_some() {
                                    if ui
                                        .button(format!(
                                            "Armed: {theme_armed_label} — disarm (Q)"
                                        ))
                                        .clicked()
                                    {
                                        new_theme_armed = Some(None);
                                    }
                                }

                                // ── This level's quick keys. Collapsed by default: it's
                                // settings you visit occasionally, not part of the
                                // browse-and-judge loop.
                                egui::CollapsingHeader::new("QUICK KEYS (this level)")
                                    .id_salt("theme-hotkeys")
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(
                                                "arm a theme above, then Set — saved with \
                                                 the level",
                                            )
                                            .small()
                                            .color(SHOP_DIM),
                                        );
                                        for (digit, bound) in &hotkey_rows {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!("{digit}"))
                                                        .strong()
                                                        .color(SHOP_GOLD),
                                                );
                                                ui.label(
                                                    egui::RichText::new(bound)
                                                        .small()
                                                        .color(SHOP_TEXT),
                                                );
                                                if ui
                                                    .add_enabled(
                                                        theme_armed.is_some(),
                                                        egui::Button::new("Set").small(),
                                                    )
                                                    .clicked()
                                                {
                                                    new_hotkey = Some((*digit, theme_armed));
                                                }
                                                if ui.small_button("✕").clicked() {
                                                    new_hotkey = Some((*digit, None));
                                                }
                                            });
                                        }
                                    });
                                ui.separator();

                                egui::ScrollArea::vertical().show(ui, |ui| {
                                    ui.set_width(206.0);
                                    let mut last_group = "";
                                    for (row, swatches) in
                                        theme_rows.iter().zip(theme_swatches.iter())
                                    {
                                        if row.group != last_group {
                                            last_group = row.group;
                                            ui.add_space(5.0);
                                            ui.label(
                                                egui::RichText::new(row.group)
                                                    .small()
                                                    .strong()
                                                    .color(SHOP_GOLD_DIM),
                                            );
                                        }
                                        // The wall stack, floor to cornice — see
                                        // `PREVIEW_ZONES`. 24 px rather than 30 so five
                                        // slots occupy the width four used to: the
                                        // keep/cut buttons share this row and the panel
                                        // is a fixed 206 wide.
                                        ui.horizontal(|ui| {
                                            for (zi, sw) in swatches.iter().enumerate() {
                                                let name = zone_name(zi as u8);
                                                match sw {
                                                    Some(h) => {
                                                        ui.add(egui::Image::new(
                                                            egui::load::SizedTexture::new(
                                                                h.id(),
                                                                egui::vec2(24.0, 24.0),
                                                            ),
                                                        ))
                                                        .on_hover_text(name);
                                                    }
                                                    None => {
                                                        let (rect, resp) = ui
                                                            .allocate_exact_size(
                                                                egui::vec2(24.0, 24.0),
                                                                egui::Sense::hover(),
                                                            );
                                                        ui.painter().rect_stroke(
                                                            rect,
                                                            0.0,
                                                            egui::Stroke::new(1.0, SHOP_DIM),
                                                            egui::StrokeKind::Inside,
                                                        );
                                                        // An empty slot is otherwise
                                                        // cryptic; say which zone it is.
                                                        resp.on_hover_text(format!(
                                                            "{name} — not defined"
                                                        ));
                                                    }
                                                }
                                            }
                                            // Keep / cut, showing the current verdict.
                                            let kept = row.verdict
                                                == Some(crate::theme_review::Verdict::Keep);
                                            let cut = row.verdict
                                                == Some(crate::theme_review::Verdict::Reject);
                                            if ui
                                                .selectable_label(kept, "✔")
                                                .on_hover_text("keep")
                                                .clicked()
                                            {
                                                new_theme_verdict = Some((
                                                    row.name,
                                                    crate::theme_review::Verdict::Keep,
                                                ));
                                            }
                                            if ui
                                                .selectable_label(cut, "✕")
                                                .on_hover_text("cut")
                                                .clicked()
                                            {
                                                new_theme_verdict = Some((
                                                    row.name,
                                                    crate::theme_review::Verdict::Reject,
                                                ));
                                            }
                                            // Open this theme in the editor. A custom
                                            // slot can then be saved back over itself;
                                            // a library theme can only become a copy,
                                            // so the hint says which you are getting.
                                            if ui
                                                .selectable_label(false, "✎")
                                                .on_hover_text(if row.editable {
                                                    "edit in place"
                                                } else {
                                                    "edit a copy"
                                                })
                                                .clicked()
                                            {
                                                draft_seed_from = Some(row.idx);
                                            }
                                        });
                                        let key = row
                                            .key
                                            .map(|k| format!("[{k}] "))
                                            .unwrap_or_default();
                                        let mark = match row.verdict {
                                            Some(crate::theme_review::Verdict::Keep) => "✔ ",
                                            Some(crate::theme_review::Verdict::Reject) => "✕ ",
                                            None => "",
                                        };
                                        if ui
                                            .selectable_label(
                                                theme_armed == Some(row.idx),
                                                format!("{mark}{key}{}", row.label),
                                            )
                                            .on_hover_text(format!(
                                                "{}\nrepeats: {}",
                                                row.name,
                                                row.repeats
                                                    .iter()
                                                    .map(|r| r
                                                        .map(|v| format!("{v:.3}"))
                                                        .unwrap_or_else(|| "-".into()))
                                                    .collect::<Vec<_>>()
                                                    .join(" / ")
                                            ))
                                            .clicked()
                                        {
                                            new_theme_armed = Some(Some(row.idx));
                                        }
                                    }
                                    if theme_rows.is_empty() {
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new("no themes match")
                                                .small()
                                                .color(SHOP_DIM),
                                        );
                                    }
                                });
                            }
                        }
                    });
            }
            draw_pd_lab_overlay(ctx, &pd_debug);
            if let Some(r) = radar.as_ref() {
                draw_pd_radar(ctx, r);
            }
            // The ring goes on top of every panel — it is the thing being aimed at.
            if let Some(view) = radial_view.as_ref() {
                crate::radial::paint::draw(ctx, view);
            }

            // ── Who owns the mouse cursor ──
            //
            // Last thing in the pass, so it beats any widget that set a hover cursor.
            //
            // `set_pointer_lock` hides the cursor with `Window::set_cursor_visible(false)`,
            // but that does not stick: this egui pass runs **every frame**, and
            // `egui_winit::State::handle_platform_output` (below) re-applies visibility from
            // egui's own requested icon — `CursorIcon::Default` translates to a real winit
            // cursor, so it calls `set_cursor_visible(true)` again. It early-outs while the
            // requested icon is unchanged, which is why the cursor only reappeared *after*
            // something moved it (hovering a panel widget, or the pointer leaving and
            // re-entering the window) — it looked intermittent rather than broken.
            //
            // So the fix is to agree with egui rather than fight it: while the pointer is
            // locked, ask for `CursorIcon::None`, the one icon `translate_cursor` maps to
            // `None` and therefore to `set_cursor_visible(false)`. egui then records
            // "hidden" as the current state and stops re-showing it. Unlocked (the object
            // panel is open, or Esc released the grab) we say nothing and egui manages the
            // cursor normally — it is genuinely needed for the panel and the gizmos.
            if pointer_locked {
                ctx.set_cursor_icon(egui::CursorIcon::None);
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
        // ── PAINT tab: apply the deferred picks.
        if let Some(sch) = paint_set_scheme {
            self.paint_scheme = sch;
        }
        if let Some(z) = paint_set_zone {
            self.paint_zone = z;
        }
        if paint_toggle_armed {
            self.paint_armed = !self.paint_armed;
            if self.paint_armed {
                // A click can only mean one thing — arming the brush stands down
                // every other click-owner, exactly as arming a theme does.
                self.theme_armed = None;
                if let Some(world) = self.world.as_mut() {
                    world.cancel_prop_placement();
                    world.cancel_light_placement();
                }
                self.props_selected = None;
            }
        }
        if paint_apply_now || paint_clear_face {
            let tex = (!paint_clear_face).then_some(FaceTex {
                scheme: self.paint_scheme,
                zone: self.paint_zone,
            });
            self.paint_probe_face(tex);
        }
        if paint_clear_all {
            let meshes = self
                .world
                .as_mut()
                .map(|w| w.with_undo_many(|w| w.clear_all_face_tex()))
                .unwrap_or_default();
            for rm in &meshes {
                self.upload(rm);
            }
        }
        // TEXTURES tab: apply the deferred picks. Arming a theme cancels any prop /
        // light / spawn placement, since a click can only mean one thing.
        if let Some(armed) = new_theme_armed {
            self.theme_armed = armed;
            if armed.is_some() {
                self.paint_armed = false;
                if let Some(world) = self.world.as_mut() {
                    world.cancel_prop_placement();
                    world.cancel_light_placement();
                }
                self.props_selected = None;
            }
        }
        if let Some((name, verdict)) = new_theme_verdict {
            self.theme_review.toggle(name, verdict);
        }
        if let Some(f) = new_theme_review_filter {
            self.theme_review_filter = f;
        }
        if theme_filter_changed {
            self.theme_filter = theme_filter_ui;
        }
        // ── Theme editor. Every change that alters the draft ends with a push into
        // the scratch scheme's live materials, which is what makes the world (and the
        // preview room) follow a slider drag with no re-bake.
        if toggle_edit_mode {
            self.theme_edit_mode = !self.theme_edit_mode;
            self.theme_status.clear();
            if self.theme_edit_mode {
                self.sync_theme_scratch();
            } else {
                self.theme_armed = None;
            }
        }
        if draft_zone_changed {
            self.theme_draft.zone_sel = draft_zone_sel;
        }
        if draft_level_changed {
            self.theme_draft.level_sel = draft_level_sel;
        }
        if draft_name_changed {
            self.theme_draft.save_name = draft_save_name;
        }
        if let Some(seed) = draft_seed_from {
            self.theme_draft.seed_from(seed);
            self.theme_edit_mode = true;
            self.theme_status = format!("copied {}", self.theme_label(seed));
            self.sync_theme_scratch();
        }
        {
            let mut changed = false;
            if let Some(t) = draft_pick_texture {
                self.theme_draft.set_texture(t);
                changed = true;
            }
            if let Some(r) = draft_new_repeat {
                self.theme_draft.set_repeat(r);
                changed = true;
            }
            if let Some(o) = draft_new_offset {
                self.theme_draft.set_offset(o);
                changed = true;
            }
            if let Some(h) = draft_new_height {
                self.theme_draft.set_height(h);
                changed = true;
            }
            if draft_clear_zone {
                self.theme_draft.clear_zone();
                changed = true;
            }
            if changed {
                self.sync_theme_scratch();
            }
        }
        if let Some((digit, scheme)) = new_hotkey {
            if let Some(w) = self.world.as_mut() {
                w.set_theme_hotkey(digit, scheme);
            }
            self.theme_status = match scheme {
                Some(i) => format!("key {digit} -> {}", self.theme_label(i)),
                None => format!("key {digit} cleared"),
            };
        }
        if draft_arm_scratch {
            let scratch = engine::render::textures::scratch_scheme();
            self.theme_armed = Some(scratch);
            self.sync_theme_scratch();
        }
        if draft_save_over {
            match self.theme_draft.save_over_origin() {
                Ok(slot) => {
                    self.after_theme_saved(slot, "updated");
                }
                Err(e) => {
                    self.theme_status = e.clone();
                    log::warn!("theme preset overwrite failed: {e}");
                }
            }
        }
        if draft_save {
            match self.theme_draft.save_as_preset() {
                Ok(slot) => {
                    self.after_theme_saved(slot, "saved as");
                }
                Err(e) => {
                    self.theme_status = e.clone();
                    log::warn!("theme preset save failed: {e}");
                }
            }
        }
        // \u2500\u2500 LEVELS tab. Ordered so a click reads the way it looks: the selection
        // and the name field settle first, then the operation that uses them.
        if level_name_changed {
            self.level_name_draft = level_name_ui;
        }
        if let Some(path) = level_select {
            // Selecting a different row disarms a pending delete, so a second click
            // somewhere else can never commit the first row's deletion.
            if self.level_confirm_delete.as_ref() != Some(&path) {
                self.level_confirm_delete = None;
            }
            // The name field follows the selection: Rename and Duplicate both read it,
            // and typing the selected level's name back in by hand is pure friction.
            if let Some(row) = self.level_rows.iter().find(|r| r.path == path) {
                self.level_name_draft = row.name.clone();
            }
            self.level_sel = Some(path);
        }
        if level_refresh {
            self.refresh_level_rows();
            self.level_status.clear();
        }
        if level_save {
            self.save_current_level();
        }
        if level_save_as {
            let name = self.level_name_draft.clone();
            match self.world.as_mut().map(|w| w.save_level_as(&name)) {
                Some(Ok(path)) => {
                    self.set_current_level(path);
                    self.level_status = format!("saved as \"{name}\"");
                }
                Some(Err(e)) => {
                    log::warn!("save as {name:?} failed: {e}");
                    self.level_status = format!("{e}");
                }
                None => {}
            }
        }
        if let Some(path) = level_load {
            self.load_level_file(&path);
        }
        if level_rename {
            if let Some(old) = self.level_sel.clone() {
                let name = self.level_name_draft.clone();
                match crate::world::persist::rename_level(&old, &name) {
                    Ok(new_path) => {
                        // Renaming the level that is *open* has to move the world's own
                        // name with it, or the next plain Save would write the old name
                        // straight back into the newly-renamed file.
                        if self.current_level.as_ref() == Some(&old) {
                            if let Some(w) = self.world.as_mut() {
                                w.set_level_name(&name);
                            }
                            // Re-syncs `saved_revision`, so the rename itself doesn't
                            // read as an unsaved edit: the file on disk already has it.
                            self.set_current_level(new_path);
                        } else {
                            self.level_sel = Some(new_path);
                            self.refresh_level_rows();
                        }
                        self.level_status = format!("renamed to \"{name}\"");
                    }
                    Err(e) => {
                        log::warn!("rename {} failed: {e}", old.display());
                        self.level_status = format!("{e}");
                    }
                }
            }
        }
        if level_duplicate {
            if let Some(src) = self.level_sel.clone() {
                let name = self.level_name_draft.clone();
                match crate::world::persist::duplicate_level(&src, &name) {
                    Ok(new_path) => {
                        // The copy is selected but *not* opened: duplicating is how you
                        // fork a level to try something, and that starts from the copy
                        // being there, not from losing your place in the original.
                        self.level_sel = Some(new_path);
                        self.refresh_level_rows();
                        self.level_status = format!("copied to \"{name}\" (not opened)");
                    }
                    Err(e) => {
                        log::warn!("duplicate {} failed: {e}", src.display());
                        self.level_status = format!("{e}");
                    }
                }
            }
        }
        if let Some(path) = level_delete_click {
            if self.level_confirm_delete.as_ref() == Some(&path) {
                self.level_confirm_delete = None;
                match crate::world::persist::delete_level(&path) {
                    Ok(()) => {
                        // The open level's file can be deleted like any other; the level
                        // itself stays loaded and editable, it just has nowhere to Save
                        // to any more, which is exactly the "never saved" state.
                        if self.current_level.as_ref() == Some(&path) {
                            self.current_level = None;
                        }
                        self.level_sel = None;
                        self.refresh_level_rows();
                        self.level_status = format!("deleted {}", short_path(&path));
                    }
                    Err(e) => {
                        log::warn!("delete {} failed: {e}", path.display());
                        self.level_status = format!("{e}");
                    }
                }
            } else {
                self.level_confirm_delete = Some(path);
                self.level_status = "click Delete again to confirm".into();
            }
        }
        // Object panel: apply the selection (arms placement of that prop on the
        // World) + the close, after the `state` borrow ends.
        if let Some(sel) = new_prop_selected {
            self.props_selected = Some(sel);
            if let (Some(world), Some(def)) =
                (self.world.as_mut(), crate::props::CATALOG.get(sel))
            {
                // A pickup arms through its own entry point, which keeps the draft's
                // weapon/ammo/respawn settings across the switch — picking a green
                // crate after a tan one shouldn't reset what's in it.
                if crate::props::pickup_kind(def.mesh).is_some() {
                    world.arm_ammo_pickup(def.mesh);
                } else {
                    world.arm_prop_placement(def.mesh);
                }
            }
        }
        // Arming a weapon pickup: the palette row names a gun, not a prop mesh.
        if let (Some(world), Some(name)) = (self.world.as_mut(), arm_weapon_pickup) {
            world.arm_weapon_pickup(name);
            // No catalog row to highlight — the armed gun is marked by name instead.
            self.props_selected = None;
        }
        // Pickup edits, written back once per frame (the panel edited a copy). A
        // selection wins over the draft, matching which one the block was showing.
        if pickup_changed {
            if let Some(world) = self.world.as_mut() {
                if selected_pickup.is_some() {
                    world.set_selected_pickup(pickup_edit);
                } else {
                    world.set_pickup_draft(pickup_edit);
                }
            }
        }
        // Door inspector edits, written back once per frame (the panel edited a copy).
        if door_changed {
            if let (Some(world), Some(d)) = (self.world.as_mut(), door_edit) {
                world.set_selected_door(d);
            }
        }
        if ground_prop {
            if let Some(world) = self.world.as_mut() {
                world.ground_selected_prop();
            }
        }
        if delete_prop {
            if let Some(world) = self.world.as_mut() {
                world.delete_selected_prop();
            }
        }
        if go_neutral {
            if let Some(world) = self.world.as_mut() {
                world.cancel_prop_placement();
                world.cancel_light_placement();
                world.cancel_spawn_point_placement();
                world.deselect_prop();
            }
        }
        if toggle_spawn_place {
            if let Some(world) = self.world.as_mut() {
                world.arm_spawn_point_placement();
            }
        }
        // NAV tab. Calculate is deliberately the only thing that runs the bake, and the
        // overlay toggle is independent of it — leave the overlay up, edit the level,
        // press Calculate again to see whether the island closed.
        if nav_calculate {
            if let Some(world) = self.world.as_mut() {
                world.calculate_nav_issues();
            }
        }
        if nav_toggle_overlay {
            if let Some(world) = self.world.as_mut() {
                world.toggle_nav_overlay();
            }
        }
        // Lighting edits (arm/disarm light placement, flat/real preference, ambient,
        // selected-light params) — applied after the `state` borrow ends.
        if toggle_light_place {
            if let Some(world) = self.world.as_mut() {
                world.arm_light_placement();
            }
        }
        if let Some(real) = set_real_lighting {
            self.build_real_lighting = real;
        }
        // TOOLS tab. Through `with_undo` because a shell change also converts whatever is
        // selected — a geometry edit like any other, and one you should be able to undo.
        // The slope is a setting only; it changes nothing already built.
        for rm in [
            set_platform_style.and_then(|s| {
                self.world.as_mut().and_then(|w| w.with_undo(|w| w.set_platform_style(s)))
            }),
            set_stair_shell.and_then(|s| {
                self.world.as_mut().and_then(|w| w.with_undo(|w| w.set_stair_shell(s)))
            }),
        ]
        .into_iter()
        .flatten()
        {
            self.upload(&rm);
        }
        if let Some(run) = set_stair_run {
            if let Some(world) = self.world.as_mut() {
                world.set_stair_run(run);
            }
        }
        // Whether a platform deck counts as a floor moves band boundaries, so every
        // region re-folds. Collected before uploading because the world borrow has to
        // end first. Untouched regions hit the memo cache, which is what makes a
        // whole-level re-fold affordable on a checkbox.
        if let Some(on) = set_platforms_are_floors {
            let meshes = self
                .world
                .as_mut()
                .map(|w| w.set_platforms_are_floors(on))
                .unwrap_or_default();
            for rm in &meshes {
                self.upload(rm);
            }
        }
        if let Some(tab) = new_tab {
            self.panel_tab = tab;
            // The paint brush belongs to its own tab: leaving it must not leave
            // clicks quietly repainting faces from a panel that no longer says so,
            // nor keep the paint-target outline up over another tab's work.
            if tab != PanelTab::Paint {
                self.paint_armed = false;
                self.paint_probe = None;
                self.refresh_highlight();
            }
            if tab == PanelTab::Levels {
                self.refresh_level_rows();
            }
        }
        if ambient_edited {
            if let Some(world) = self.world.as_mut() {
                world.set_ambient(crate::ecs::AmbientSettings { color: amb_color, level: amb_level });
            }
        }
        if light_edited {
            if let Some(world) = self.world.as_mut() {
                world.set_selected_light(light_color, light_intensity, light_range);
            }
        }
        // PLAY tab. The config write comes first on purpose: clicking START in the
        // same frame as an edit must enter the hunt with the edit, not without it.
        if play_changed {
            if let Some(world) = self.world.as_mut() {
                world.set_play_config(play_ui);
            }
        }
        if play_start {
            // The panel is an authoring surface and the pointer is unlocked behind it, so
            // it comes down on the way into the hunt — pressing G with it open does the
            // same via `toggle_props`.
            if self.props_open {
                self.toggle_props();
            }
            self.apply(EditorAction::EnterHunt);
        }
        if close_props {
            self.toggle_props();
        }
        Some(frame)
    }
}

/// **The PLAY tab** — the authored match setup a hunt starts from.
///
/// Edits `cfg` in place and returns `(changed, start_hunt)`. Split out of the panel's
/// per-tab `match` for the same reason [`pickup_settings_ui`] is: the arm would
/// otherwise be four hundred lines inside an already-large closure.
///
/// A control whose field an explicit override has claimed ([`PlayPins`]) is drawn
/// **disabled with the reason on hover**, rather than live-but-ignored. A checkbox that
/// silently does nothing is the worst of the three options.
fn play_tab_ui(
    ui: &mut egui::Ui,
    cfg: &mut crate::world::PlayConfig,
    ctx: &PlayTabCtx,
) -> (bool, bool) {
    use crate::world::play_config::{EntryMode, HunterWeapon, LoadoutMode, LoadoutSlot};

    let mut changed = false;
    let mut start = false;

    // ── The button this tab exists for. `G` still does exactly this.
    let label = if ctx.in_hunt { "\u{25a0} RETURN TO BUILD" } else { "\u{25b6} START HUNT" };
    if ui
        .add_sized(
            [200.0, 32.0],
            egui::Button::new(
                egui::RichText::new(label).color(egui::Color32::BLACK).strong(),
            )
            .fill(SHOP_GOLD),
        )
        .on_hover_text("G \u{2014} the key still works, and reads everything below")
        .clicked()
    {
        start = true;
    }
    ui.label(egui::RichText::new(cfg.summary()).small().color(SHOP_GOLD_DIM));
    ui.label(
        egui::RichText::new(
            "Saved with the level, so a level opens with the fight it was designed for.",
        )
        .small()
        .color(SHOP_DIM),
    );

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.set_width(206.0);

        // ── ENTRY ────────────────────────────────────────────────────────────
        play_section(ui, "WHERE YOU COME IN");
        for mode in [EntryMode::Pads, EntryMode::Camera] {
            if ui
                .selectable_label(cfg.entry == mode, mode.label())
                .on_hover_text(match mode {
                    EntryMode::Pads => {
                        "The pads placed in the SPAWNS tab, chosen by Perfect Dark's own \
                         rule. Falls back to the camera when the level has none."
                    }
                    EntryMode::Camera => {
                        "Drop in under the fly-cam, wherever you are looking. For testing \
                         one corner of a big level, or a level with no pads yet."
                    }
                })
                .clicked()
                && cfg.entry != mode
            {
                cfg.entry = mode;
                changed = true;
            }
        }
        if cfg.entry == EntryMode::Pads {
            let (text, col) = match ctx.pads {
                0 => (
                    "no pads authored \u{2014} you will enter under the camera".to_string(),
                    SHOP_GOLD,
                ),
                n => (format!("{n} pad(s) authored"), NAV_OK),
            };
            ui.label(egui::RichText::new(text).small().color(col));
        }

        // ── PLAYER LOADOUT ───────────────────────────────────────────────────
        play_section(ui, "WHAT YOU CARRY");
        for mode in [LoadoutMode::Level, LoadoutMode::Custom, LoadoutMode::Empty] {
            if ui
                .selectable_label(cfg.loadout == mode, mode.label())
                .on_hover_text(match mode {
                    LoadoutMode::Level => {
                        "The guns on the floor are the armoury. A level that places none \
                         hands you the starting sidearm instead."
                    }
                    LoadoutMode::Custom => {
                        "Start with exactly the guns listed below \u{2014} the inventory is \
                         stripped first, so shop purchases do not add themselves."
                    }
                    LoadoutMode::Empty => {
                        "Empty-handed, with no safety-net sidearm \u{2014} even on a level \
                         with no guns to find."
                    }
                })
                .clicked()
                && cfg.loadout != mode
            {
                cfg.loadout = mode;
                changed = true;
            }
        }
        if cfg.loadout == LoadoutMode::Custom {
            ui.add_space(2.0);
            let mut remove: Option<usize> = None;
            let mut equip: Option<usize> = None;
            for (i, slot) in cfg.weapons.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    // The radio is which gun is in hand at G.
                    if ui
                        .radio(slot.equipped, "")
                        .on_hover_text("in your hands when the hunt starts")
                        .clicked()
                    {
                        equip = Some(i);
                    }
                    ui.label(
                        egui::RichText::new(&slot.weapon)
                            .color(if slot.equipped { SHOP_GOLD } else { egui::Color32::GRAY }),
                    );
                    if ui
                        .small_button("\u{2715}")
                        .on_hover_text("drop from the loadout")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                if ui
                    .add(egui::Slider::new(&mut slot.spare_mags, 0..=20).text("spare mags"))
                    .on_hover_text(
                        "Magazines in reserve on top of the full one in the gun. 0 means \
                         one magazine and no refills.",
                    )
                    .changed()
                {
                    changed = true;
                }
            }
            if let Some(i) = equip {
                for (k, s) in cfg.weapons.iter_mut().enumerate() {
                    s.equipped = k == i;
                }
                changed = true;
            }
            if let Some(i) = remove {
                cfg.weapons.remove(i);
                changed = true;
            }
            // Add a gun. Only ones not already listed — a duplicate slot would just
            // stock the same weapon twice, which is what the spare-mags slider is for.
            let addable: Vec<&'static str> = ctx
                .weapons
                .iter()
                .copied()
                .filter(|n| !cfg.weapons.iter().any(|s| s.weapon == *n))
                .collect();
            egui::ComboBox::from_id_salt("play_add_gun")
                .selected_text("+ add a gun")
                .width(190.0)
                .show_ui(ui, |ui| {
                    for name in addable {
                        if ui.selectable_label(false, name).clicked() {
                            cfg.weapons.push(LoadoutSlot::new(name));
                            changed = true;
                        }
                    }
                });
            if cfg.weapons.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "nothing listed \u{2014} the level's own armoury will be used instead",
                    )
                    .small()
                    .color(SHOP_GOLD),
                );
            }
        }
        ui.add_space(2.0);
        if ui
            .add(egui::Slider::new(&mut cfg.health, 1.0..=100.0).text("start health"))
            .changed()
        {
            changed = true;
        }
        if ui
            .add(egui::Slider::new(&mut cfg.armor, 0.0..=100.0).text("start armour"))
            .changed()
        {
            changed = true;
        }

        // ── OPPOSITION ───────────────────────────────────────────────────────
        play_section(ui, "WHO IS HUNTING YOU");
        ui.add_enabled_ui(!ctx.pins.wave, |ui| {
            let r = ui
                .add(
                    egui::Slider::new(&mut cfg.enemy_count, 0..=crate::world::WAVE_SIZE_MAX)
                        .text("hunters"),
                )
                .on_hover_text("0 spawns none \u{2014} an empty level to walk and light");
            if r.changed() {
                changed = true;
            }
        });
        play_pin_note(ui, ctx.pins.wave, "hunter count");
        ui.add_enabled_ui(!ctx.pins.difficulty, |ui| {
            let r = ui
                .add(
                    egui::Slider::new(&mut cfg.difficulty, 0..=crate::world::DIFFICULTY_MAX)
                        .text("difficulty"),
                )
                .on_hover_text(
                    "The same dial the = / - keys sweep. 0 is the original baseline; the \
                     top is Perfect Dark's DarkSim \u{2014} no reaction delay, no aim error.",
                );
            if r.changed() {
                changed = true;
            }
        });
        play_pin_note(ui, ctx.pins.difficulty, "difficulty");

        // ── AI + BODIES ──────────────────────────────────────────────────────
        play_section(ui, "HOW THEY THINK");
        ui.add_enabled_ui(!ctx.pins.ai, |ui| {
            ui.horizontal(|ui| {
                for m in [crate::enemy::AiMode::Ours, crate::enemy::AiMode::Pd] {
                    let label = if m == crate::enemy::AiMode::Ours { "Ours" } else { "Perfect Dark" };
                    if ui
                        .selectable_label(cfg.ai == m, label)
                        .on_hover_text(m.summary())
                        .clicked()
                        && cfg.ai != m
                    {
                        cfg.ai = m;
                        changed = true;
                    }
                }
            });
        });
        play_pin_note(ui, ctx.pins.ai, "AI model");
        ui.add_enabled_ui(!ctx.pins.bodies, |ui| {
            ui.horizontal(|ui| {
                for (b, label) in [
                    (crate::world::BodySet::All, "Both"),
                    (crate::world::BodySet::GoldenEye, "GoldenEye"),
                    (crate::world::BodySet::PerfectDark, "PD"),
                ] {
                    if ui
                        .selectable_label(cfg.bodies == b, label)
                        .on_hover_text("which character models the wave draws from")
                        .clicked()
                        && cfg.bodies != b
                    {
                        cfg.bodies = b;
                        changed = true;
                    }
                }
            });
        });
        play_pin_note(ui, ctx.pins.bodies, "bodies");
        // What they carry: the two policies, then the whole arsenal as "this one gun".
        let current = cfg.hunter_weapon.label().to_string();
        egui::ComboBox::from_id_salt("play_hunter_weapon")
            .selected_text(current)
            .width(190.0)
            .show_ui(ui, |ui| {
                for policy in [HunterWeapon::Loot, HunterWeapon::Roster] {
                    if ui
                        .selectable_label(cfg.hunter_weapon == policy, policy.label())
                        .clicked()
                        && cfg.hunter_weapon != policy
                    {
                        cfg.hunter_weapon = policy;
                        changed = true;
                    }
                }
                ui.separator();
                for name in ctx.weapons.iter().copied() {
                    let sel =
                        matches!(&cfg.hunter_weapon, HunterWeapon::Fixed(n) if n.as_str() == name);
                    if ui.selectable_label(sel, format!("all carry the {name}")).clicked() && !sel {
                        cfg.hunter_weapon = HunterWeapon::Fixed(name.to_string());
                        changed = true;
                    }
                }
            });
        ui.label(
            egui::RichText::new(
                "Loot = they start empty-handed and race you to the guns on the floor \
                 (needs pickups placed).",
            )
            .small()
            .color(SHOP_DIM),
        );

        // ── RULES ────────────────────────────────────────────────────────────
        play_section(ui, "MATCH RULES");
        if ui
            .checkbox(&mut cfg.respawn, "Respawn after dying")
            .on_hover_text(
                "Off is one life each: you dying ends it, and so does the last hunter \
                 falling. That is hide-and-seek rather than deathmatch.",
            )
            .changed()
        {
            changed = true;
        }
        ui.add_enabled_ui(cfg.respawn, |ui| {
            if ui
                .add(egui::Slider::new(&mut cfg.respawn_delay, 0.0..=10.0).text("delay s"))
                .changed()
            {
                changed = true;
            }
        });
        ui.add_enabled_ui(!ctx.pins.score_limit, |ui| {
            let r = ui
                .add(egui::Slider::new(&mut cfg.score_limit, 0..=50).text("kills to win"))
                .on_hover_text("0 = endless, for an open-ended observation run");
            if r.changed() {
                changed = true;
            }
        });
        play_pin_note(ui, ctx.pins.score_limit, "score limit");
        if ui
            .add(egui::Slider::new(&mut cfg.time_limit_min, 0.0..=30.0).text("minutes"))
            .on_hover_text(
                "0 = no limit. When time is up the side ahead on kills wins; a tie goes \
                 to you, for having lasted the round.",
            )
            .changed()
        {
            changed = true;
        }

        // ── DEBUG ────────────────────────────────────────────────────────────
        play_section(ui, "DEBUG");
        ui.add_enabled_ui(!ctx.pins.cheats, |ui| {
            if ui
                .checkbox(&mut cfg.invincible, "Start invincible (I)")
                .on_hover_text("Hunters still aim and fire; you just stop taking damage.")
                .changed()
            {
                changed = true;
            }
            if ui
                .checkbox(&mut cfg.invisible, "Start unseen (N)")
                .on_hover_text(
                    "No hunter can perceive you, so the pack drops to searching \u{2014} \
                     the way to watch the AI work.",
                )
                .changed()
            {
                changed = true;
            }
        });
        play_pin_note(ui, ctx.pins.cheats, "cheats");
    });

    (changed, start)
}

/// A small gold section heading inside the panel.
fn play_section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).small().strong().color(SHOP_GOLD_DIM));
}

/// The line under a control an explicit override has claimed, naming what is actually
/// in force. Nothing at all when the field is free.
fn play_pin_note(ui: &mut egui::Ui, pinned: bool, what: &str) {
    if pinned {
        ui.label(
            egui::RichText::new(format!(
                "{what} is set for this session (a launch flag or a key) \u{2014} that wins"
            ))
            .small()
            .color(SHOP_GOLD),
        );
    }
}

/// What the PLAY tab needs to know about the world that is not in the config itself.
struct PlayTabCtx<'a> {
    pins: crate::world::play_config::PlayPins,
    /// Authored spawn pads, so the entry section can warn about a pad-entry level with none.
    pads: usize,
    /// The live arsenal's gun names (no empty-handed slot).
    weapons: &'a [&'static str],
    in_hunt: bool,
}

/// The pickup settings block: which weapon, how much ammo, how long until it comes
/// back. Returns `true` if anything changed.
///
/// One function, two call sites, deliberately: it edits the **draft** while a pickup
/// tool is armed and the **selected instance** when one is picked in the 3D view, so
/// authoring and editing can never drift apart the way two copies of a widget block
/// would.
fn pickup_settings_ui(
    ui: &mut egui::Ui,
    p: &mut crate::ecs::Pickup,
    weapons: &[&'static str],
) -> bool {
    use crate::ecs::PickupKind;
    let mut changed = false;
    let is_ammo = p.kind == PickupKind::Ammo;
    ui.label(
        egui::RichText::new(if is_ammo { "AMMO CRATE" } else { "WEAPON PICKUP" })
            .small()
            .strong()
            .color(SHOP_GOLD_DIM),
    );
    // The crate is a visual choice; THIS is what decides what's inside it.
    egui::ComboBox::from_id_salt("pickup-weapon")
        .width(184.0)
        .selected_text(p.weapon)
        .show_ui(ui, |ui| {
            for name in weapons {
                if ui.selectable_label(p.weapon == *name, *name).clicked() && p.weapon != *name {
                    p.weapon = name;
                    changed = true;
                }
            }
        });
    let mut mags = p.mags;
    let label = if is_ammo { "magazines" } else { "spare mags" };
    if ui
        .add(egui::Slider::new(&mut mags, 1..=20).text(label))
        .changed()
    {
        p.mags = mags;
        changed = true;
    }
    let mut respawn = p.respawn;
    if ui
        .add(egui::Slider::new(&mut respawn, 0.0..=60.0).text("returns after s"))
        .changed()
    {
        p.respawn = respawn;
        changed = true;
    }
    ui.label(
        egui::RichText::new(if is_ammo {
            "the crate is a look; the WEAPON above is what's inside it · 0 s = gone \
             for the round"
        } else {
            "arrives loaded, plus the spare mags · 0 s = gone for the round"
        })
        .small()
        .color(SHOP_DIM),
    );
    changed
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

/// Draw the **PD simulant lab** telemetry panel (`PD_LAB=1` only).
///
/// The zeroing model is invisible without this: from outside, a simulant that is
/// half-converged and one that is fully converged look identical until the shots
/// land somewhere. The two rows that matter are ZERO (how far the convergence has
/// got) and AIM ERR (where the barrel actually is, in degrees) — watch AIM ERR
/// swing through zero and back as the bot overshoots, and watch ZERO collapse the
/// moment you break line of sight or make it turn.
// ─── PD-style radar ──────────────────────────────────────────────────────────
/// How far (m) the radar's edge reaches. Perfect Dark's own radar is a fixed-scale
/// local view rather than a whole-level map, and that is the useful property here:
/// it answers "where is the pack right now" at a glance instead of needing to be read.
const RADAR_RANGE_M: f32 = 30.0;
/// Radius of the drawn disc, in points.
const RADAR_RADIUS_PX: f32 = 92.0;

/// Draw the lab radar: the player at the centre facing **up**, the walkable floor of
/// its storey as a faint backdrop, and every hunter as a blip.
///
/// The floor backdrop is the part that earns its place. A blip in a void tells you a
/// hunter is 12 m to your left; a blip against the floor plan tells you it is jammed in
/// a doorway, orbiting a pillar, or stuck on the far side of a wall it will not path
/// around — which is what makes it a navigation tool rather than a curiosity.
///
/// Projection and the yaw convention live in [`World::radar`]; this only maps the unit
/// disc onto pixels.
fn draw_pd_radar(ctx: &egui::Context, view: &crate::world::RadarView) {
    let d = RADAR_RADIUS_PX;
    // Bottom-LEFT, which is where Perfect Dark puts its own radar — and, less
    // romantically, the only free corner: the ammo counter owns bottom-right and the
    // radar was sitting on top of it.
    egui::Window::new("PD RADAR")
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
        .title_bar(false)
        .resizable(false)
        .show(ctx, |ui| {
            let (resp, painter) =
                ui.allocate_painter(egui::vec2(d * 2.0, d * 2.0), egui::Sense::hover());
            let c = resp.rect.center();
            // Radar frame → pixels. `+y` is ahead of the player, and screen y grows
            // downward, so the vertical axis flips.
            let to_px = |v: glam::Vec2| egui::pos2(c.x + v.x * d, c.y - v.y * d);

            painter.circle_filled(c, d, egui::Color32::from_rgba_unmultiplied(6, 12, 8, 210));
            // Range rings at a third and two thirds, so distance is readable.
            for f in [1.0 / 3.0, 2.0 / 3.0, 1.0] {
                painter.circle_stroke(
                    c,
                    d * f,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(30, 70, 45)),
                );
            }

            // Floor backdrop.
            for &p in &view.floor {
                painter.rect_filled(
                    egui::Rect::from_center_size(to_px(p), egui::vec2(2.0, 2.0)),
                    0.0,
                    egui::Color32::from_rgb(28, 62, 42),
                );
            }

            // Blips. Colour carries state, because "where is it" and "what is it doing"
            // are the two questions at once: firing reads hot, engaged amber, idle/
            // searching green, and a corpse is a hollow ring so the living are countable
            // at a glance without the dead vanishing (a hunter that died in a corner is
            // itself a navigation finding).
            for b in &view.blips {
                let p = to_px(b.at);
                if b.dead {
                    painter.circle_stroke(
                        p,
                        4.0,
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 110, 110)),
                    );
                    continue;
                }
                let col = if b.firing {
                    egui::Color32::from_rgb(255, 90, 70)
                } else if b.engaged {
                    egui::Color32::from_rgb(250, 190, 70)
                } else {
                    egui::Color32::from_rgb(90, 220, 120)
                };
                painter.circle_filled(p, 4.5, col);
                // A blip on another storey gets a ring, so "it is right on top of me"
                // and "it is directly above me" don't look identical.
                if b.dy.abs() > 1.5 {
                    painter.circle_stroke(p, 7.0, egui::Stroke::new(1.0, col));
                }
                painter.text(
                    p + egui::vec2(7.0, -7.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{}", b.id),
                    egui::FontId::monospace(10.0),
                    col,
                );
            }

            // The player: a triangle pointing up (the radar rotates with you, PD-style,
            // so "up" is always where you are looking).
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(0.0, -7.0),
                    c + egui::vec2(-5.0, 5.0),
                    c + egui::vec2(5.0, 5.0),
                ],
                egui::Color32::WHITE,
                egui::Stroke::NONE,
            ));

            let alive = view.blips.iter().filter(|b| !b.dead).count();
            ui.label(
                egui::RichText::new(format!(
                    "RADAR  {:.0} m   ·   {alive} live / {} in range",
                    view.range,
                    view.blips.len()
                ))
                .color(SHOP_DIM)
                .monospace()
                .small(),
            );
        });
}

/// Whether a boolean boot flag is on: set, and not set to something that plainly means
/// *off*.
///
/// Presence alone is the older convention here (`PD_LAB`, `PD_LAB_FFA`), and it has a sharp
/// edge — `GE_CLIPS=0` would switch the flag **on**, which is the opposite of what anyone
/// typing it means. Reading the value costs nothing and removes the surprise. Note that
/// PowerShell deletes a variable assigned `""`, so `$env:GE_CLIPS = ""` genuinely unsets it
/// there; the empty case is handled anyway for the shells where it does not.
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Err(_) => false,
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
    }
}

fn draw_pd_lab_overlay(ctx: &egui::Context, sims: &[crate::world::pd_lab::PdDebug]) {
    if sims.is_empty() {
        return;
    }
    egui::Window::new("PD SIMULANT LAB")
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .title_bar(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("PD SIMULANT LAB").color(SHOP_GOLD).strong());
            // A six-simulant lab is taller than the window — the last rows were being
            // cut off the bottom of the screen. Cap it and scroll, leaving room for the
            // radar in the corner below.
            let cap = (ctx.screen_rect().height() * 0.55).max(200.0);
            egui::ScrollArea::vertical().max_height(cap).show(ui, |ui| {
            for s in sims.iter() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "#{}  {}  ·  {}",
                            s.id,
                            s.tier.name(),
                            s.bot_type.name()
                        ))
                        .color(SHOP_GOLD_DIM)
                        .strong(),
                    );
                    // Alive/dead up front — the whole row below it is meaningless for
                    // a corpse (its aim model keeps ticking; its body does not).
                    if s.dead {
                        ui.label(
                            egui::RichText::new("DEAD")
                                .color(egui::Color32::from_rgb(150, 150, 150))
                                .strong(),
                        );
                    }
                });

                // Health. Shown against the hunter's SPAWN max rather than a constant,
                // because the difficulty dial scales it — a bar against 100 would read
                // wrong at every level but one.
                let frac = if s.max_health > 0.0 {
                    (s.health / s.max_health).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let hp_color = if s.dead {
                    egui::Color32::from_rgb(90, 90, 90)
                } else if frac > 0.6 {
                    egui::Color32::from_rgb(120, 220, 120)
                } else if frac > 0.3 {
                    SHOP_GOLD
                } else {
                    egui::Color32::from_rgb(220, 110, 90)
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(220.0)
                        .fill(hp_color)
                        .text(format!("HP {:.0} / {:.0}", s.health, s.max_health)),
                );

                if s.dead {
                    continue; // the aim/reaction readouts below describe a corpse
                }

                // What the movement FSM is doing — the first thing to look at when a
                // hunter is not arriving (`Search` in a level it should be crossing
                // means it never acquired; `Chase` while stationary means nav).
                ui.label(
                    egui::RichText::new(format!("{:?}", s.state))
                        .color(SHOP_TEXT)
                        .monospace(),
                );

                // Zeroing progress: full bar = aim has converged as far as this
                // tier ever converges.
                ui.add(
                    egui::ProgressBar::new(s.zero_progress)
                        .desired_width(220.0)
                        .text(format!("ZERO {:.0}%", s.zero_progress * 100.0)),
                );

                // Reaction clock against the tier's requirement. Reaching 100% is
                // what unlocks the trigger, not the zero bar above it.
                let react = if s.shoot_delay_needed > 0.0 {
                    (s.shoot_delay / s.shoot_delay_needed).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                ui.add(
                    egui::ProgressBar::new(react)
                        .desired_width(220.0)
                        .text(format!("REACT {:.2}s / {:.2}s", s.shoot_delay, s.shoot_delay_needed)),
                );

                // The number to actually watch. Signed, so you can see the aim
                // cross the target rather than creep onto it.
                let err = s.aim_error_deg;
                let err_color = if err.abs() < 2.0 {
                    egui::Color32::from_rgb(120, 220, 120)
                } else if err.abs() < 8.0 {
                    SHOP_GOLD
                } else {
                    egui::Color32::from_rgb(220, 110, 90)
                };
                ui.label(
                    egui::RichText::new(format!("AIM ERR  {err:+6.2}°"))
                        .color(err_color)
                        .monospace()
                        .strong(),
                );

                ui.label(
                    egui::RichText::new(format!(
                        "dist {:5.1} m   speed x{:.2}",
                        s.distance, s.speed_mult
                    ))
                    .color(SHOP_TEXT)
                    .monospace(),
                );

                // WHO it is fighting. A packmate here is the free-for-all working: the
                // hunter should also be *moving* toward `#j`, not toward you.
                let who = match s.target_hunter {
                    Some(j) => format!("→ #{j}"),
                    None if s.has_target => "→ PLAYER".to_string(),
                    None => String::new(),
                };
                let target = match (s.has_target, s.sticky) {
                    (false, _) => "no target",
                    (true, true) => "target (held)",
                    (true, false) => "target (acquired)",
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(target).color(SHOP_DIM).monospace());
                    if !who.is_empty() {
                        let c = if s.target_hunter.is_some() {
                            egui::Color32::from_rgb(230, 160, 60) // friendly fire — stands out
                        } else {
                            SHOP_TEXT
                        };
                        ui.label(egui::RichText::new(who).color(c).monospace().strong());
                    }
                    if s.firing {
                        ui.label(
                            egui::RichText::new("FIRING")
                                .color(egui::Color32::from_rgb(240, 90, 70))
                                .strong(),
                        );
                    }
                });

                // Which of Perfect Dark's per-bearing attack animations the current burst
                // is playing, and how far off the body's facing it aims. A non-zero angle
                // is a sideways clip: the torso should visibly be turned away from the
                // target while the arms fire across it.
                if let Some((anim, off)) = s.fire_anim {
                    let c = if off.abs() > 1.0 {
                        egui::Color32::from_rgb(120, 200, 240) // a directional pick
                    } else {
                        SHOP_DIM
                    };
                    let off = if off > 180.0 { off - 360.0 } else { off };
                    ui.label(
                        egui::RichText::new(format!("{anim}  aim {off:+.0}°"))
                            .color(c)
                            .monospace(),
                    );
                }
            }
            });
            ui.separator();
            // Which side everyone is on. With teams on (the default) a hunter targeting
            // a packmate is a bug; with them off it is the mode — so say which.
            if sims.first().is_some_and(|s| s.free_for_all) {
                ui.label(
                    egui::RichText::new("TEAMS OFF - free-for-all (PD_LAB_FFA)")
                        .color(egui::Color32::from_rgb(230, 160, 60))
                        .small()
                        .strong(),
                );
            }
            ui.label(
                egui::RichText::new("= / -  difficulty tier    N  invisible    I  invincible")
                    .color(SHOP_DIM)
                    .small(),
            );
        });
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
    /// Magazine size, snapshotted so the "+Ammo" tooltip needs no `World` borrow
    /// (and so it reads the LIVE arsenal rather than the GoldenEye table, which
    /// would quote the wrong round count for a Perfect Dark gun).
    magazine_size: u32,
}

/// One row of the TEXTURES tab, snapshotted so the egui closure needs no borrow of
/// the theme registry or the review state.
struct ThemeRow {
    /// Index into `engine::render::textures::schemes()`.
    idx: usize,
    name: &'static str,
    /// Display label, which may be a name saved this session rather than the
    /// registry's (see `App::theme_label`).
    label: String,
    group: &'static str,
    key: Option<char>,
    verdict: Option<crate::theme_review::Verdict>,
    /// Whether this theme can be saved back over itself — true for a custom slot,
    /// false for the read-only library.
    editable: bool,
    /// Per-zone `repeat` for zones 0..3, for the detail readout.
    repeats: [Option<f32>; 4],
}

/// Which modal tool is armed, for the radial's "this one is up" accent.
///
/// Order matters: the opening tools share `is_opening_arming` and the placement
/// tools share `is_placing`, so the specific query has to come first in each pair.
fn armed_tool(w: &crate::world::World) -> Option<Tool> {
    if w.is_draw_tool() {
        Some(Tool::Draw)
    } else if w.is_vent_tool() {
        Some(Tool::Vent)
    } else if w.is_ladder_tool() {
        Some(Tool::Ladder)
    } else if w.is_hole_arming() {
        Some(Tool::Hole)
    } else if w.is_opening_arming() {
        Some(Tool::Door)
    } else if w.is_pillar_arming() {
        Some(Tool::Pillar)
    } else if w.is_placing() {
        Some(Tool::Brace)
    } else if w.is_simple_stair() {
        Some(Tool::BlockStairs)
    } else if w.is_connect_sliding() {
        Some(Tool::Connect)
    } else if w.is_platform_tool() {
        Some(Tool::Platform)
    } else {
        None
    }
}

/// [`digit_char`] restricted to the **number row**. The room plan tool binds the
/// numpad to its view presets, so it needs the half of that mapping that does not
/// collide.
fn row_digit_char(code: KeyCode) -> Option<char> {
    Some(match code {
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        _ => return None,
    })
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

/// A file size for the level list: whole KB, or MB once it passes a thousand. Levels
/// run from a couple of KB to a few hundred, so a byte count is noise and one decimal
/// place of MB is all the precision that means anything.
fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", (bytes / 1000).max(1))
    }
}

/// A level file's name for a status line: just the filename, not the whole path,
/// which is long, absolute and the same for every level.
fn short_path(path: &std::path::Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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
        // ── The starting match setup (the PLAY tab's config) ──────────────────
        //
        // These two used to be `set_difficulty` / `set_wave_size` calls right here — a
        // pair of invisible boot pins that no level could state and no player could see.
        // They are the same two values, installed as the *default* PLAY config instead,
        // so the panel shows them, a level can save its own, and `G` reads whatever the
        // open level says. `set_play_config_default` rather than `set_play_config`: this
        // is what a level starts as, not an edit, so it must not raise the unsaved marker.
        //
        // The difficulty is `HUNT_TIER`, not max: the dial selects a Perfect Dark zeroing
        // tier rather than multiplying a hit probability, and max is DarkSim — no reaction
        // delay, no aim error, kills on sight from across the room. The wave is a small
        // pack, because the coordinated AI (flanking, squad suppression, cover) only reads
        // with more than one hunter on the field; the code default stays at duel = 1 so
        // the headless duel-mode tests are unaffected.
        {
            let mut play = world.play_config().clone();
            play.difficulty = crate::world::pd_lab::dial_for_tier(
                crate::world::pd_lab::HUNT_TIER,
                crate::world::DIFFICULTY_MAX,
            );
            play.enemy_count = crate::world::PLAYTEST_WAVE_SIZE;
            world.set_play_config_default(play);
        }
        // `SCORE_LIMIT=n` — kills to win the round; `0` = endless, for an open-ended
        // observation run where you don't want the result screen interrupting.
        if let Ok(v) = std::env::var("SCORE_LIMIT") {
            match v.trim().parse::<u32>() {
                Ok(n) => {
                    world.set_score_limit(n);
                    log::info!(
                        "SCORE_LIMIT={n}{}",
                        if n == 0 { " (endless round)" } else { " kills to win" }
                    );
                }
                Err(_) => log::warn!("SCORE_LIMIT={v:?} is not a number — keeping the default"),
            }
        }
        for rm in world.initial_meshes() {
            renderer.set_region_textured(rm.id, &rm.mesh);
        }
        // PD simulant lab (`PD_LAB=1`): bake the bare lab room in-process, load it,
        // and switch every hunter onto the Perfect Dark bot model. See
        // `world::pd_lab` for what that swaps out. Press G to start the hunt.
        if let Some(cfg) = crate::world::pd_lab::PdLabConfig::from_env() {
            match world.load_built_level(&crate::levelgen::designs::pd_lab()) {
                Ok(meshes) => {
                    for rm in &meshes {
                        renderer.set_region_textured(rm.id, &rm.mesh);
                    }
                    // Loud, because this **replaces the starting room** with the lab's
                    // 4-pillar arena. `$env:PD_LAB` persists for a whole PowerShell
                    // session, so it long outlives the run it was set for, and the
                    // symptom ("why am I not in the plain room?") doesn't point at a
                    // stale env var on its own.
                    log::warn!(
                        "PD_LAB is set — the starting room has been REPLACED by the lab \
                         arena (64 WT, 4 pillars). Clear it with \
                         `Remove-Item Env:PD_LAB` to boot into the plain editor room."
                    );
                    // The tier is already the boot dial's (`HUNT_TIER`), so the lab
                    // inherits it and only `PD_LAB_DIFFICULTY=` changes it.
                    world.enable_pd_lab(cfg);
                }
                Err(e) => log::error!("PD_LAB: could not build the lab room: {e}"),
            }
        }
        // ── The roster, applied LAST so an explicit choice always wins ──
        //
        // Both families by default — 44 GoldenEye bodies plus the 6 Perfect Dark ones,
        // every one of them on Perfect Dark's animations. The two knobs are orthogonal:
        // `BODIES` picks who shows up, `GE_CLIPS` picks what animates them.
        //
        // * `BODIES=ge|pd` — one family only. They look completely different, so this is
        //   an aesthetic switch as much as a debugging one.
        // * `GE_CLIPS=1` — put GoldenEye-bodied hunters back on the legacy GoldenEye clip
        //   set: hand-set `FIRE_TIMING` windows, height-zone hit picks, canned flinches,
        //   no directional fire table. The A/B the whole Perfect Dark track was measured
        //   against. Narrows the bodies to GoldenEye too, since a Perfect Dark body cannot
        //   take a GoldenEye clip.
        //
        // **Order matters, and it used to be wrong.** This block ran *before* the `PD_LAB`
        // block above, and `enable_pd_lab` pins `BodySet::PerfectDark` — so with `PD_LAB`
        // still set in the shell (they persist for a session, and the lab is usable on a
        // real level, not just its own bare room) `BODIES=ge` was silently discarded and
        // the wave stayed Perfect Dark. Applying the explicit choice last is the fix:
        // whatever the environment asks for outranks a mode default.
        let body_env = std::env::var("BODIES").unwrap_or_default();
        match body_env.trim().to_ascii_lowercase().as_str() {
            "" | "all" | "both" => {}
            "ge" | "goldeneye" => {
                log::info!("BODIES=ge: GoldenEye bodies only");
                world.set_body_set(crate::world::BodySet::GoldenEye);
            }
            "pd" | "perfectdark" => {
                log::info!("BODIES=pd: Perfect Dark bodies only");
                world.set_body_set(crate::world::BodySet::PerfectDark);
            }
            other => log::warn!("BODIES='{other}' not recognised — using every body"),
        }
        if env_flag("GE_CLIPS") {
            log::info!("GE_CLIPS: the legacy GoldenEye animation set (GoldenEye bodies)");
            world.set_goldeneye_clips(true);
            world.set_body_set(crate::world::BodySet::GoldenEye);
        }
        // ── The engagement model, applied LAST for the same reason as the roster ──
        //
        // `AI=pd` swaps our hunter's whole post-contact behaviour for Perfect Dark's
        // deathmatch simulant: omniscient, no search, four-mode distance-band combat,
        // no dodge/flank/cover/suppress, PD's reload rule. `AI=ours` (the default) is
        // everything we built. `World::new` already resolved this from the environment;
        // re-applying it here is the belt to that braces — nothing between the two
        // points may pin an AI mode without an explicit `AI=` losing, which is exactly
        // how `PD_LAB` once ate `BODIES=ge` for a whole playtest.
        // `pin_ai_mode` only when `AI=` is actually set: pinning unconditionally would
        // mean the PLAY tab's AI choice could never take effect, which is the same class
        // of silent-override bug this line's own comment is about — in the other
        // direction. With no `AI=`, the level's config decides at `G`.
        match std::env::var("AI") {
            Ok(v) if !v.trim().is_empty() => world.pin_ai_mode(crate::enemy::AiMode::from_env()),
            _ => world.set_ai_mode(crate::enemy::AiMode::from_env()),
        }
        // Say what was actually resolved, unconditionally. The wave itself logs its body
        // spread at spawn, but that is not until G — and "which hunters am I about to get"
        // is exactly the question a boot flag silently losing a fight makes unanswerable.
        log::info!("HUNTERS: {}", world.roster_summary());
        log::info!("{}", world.ai_mode().summary());
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
                    // Booting into a slot opens that level like any other load would, so
                    // Ctrl+S saves straight back to it instead of reporting no file.
                    self.current_level = Some(crate::world::persist::slot_path(slot));
                    self.saved_revision = world.revision();
                    self.level_name_draft = world.level_name().to_string();
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
            // The sentry gun is an articulated prop, not a static one: its export is a
            // parts sheet, so it loads split into its six pieces and uploads one mesh
            // per piece under its own key. The turret then draws as six matrices off
            // one entity (see `crate::turret`), which is what lets the head track and
            // the barrels spin. Its registered AABB is the *assembled* rig, not the
            // sheet's, so placement measures the turret rather than the exploded parts.
            if def.mesh == crate::ecs::MeshId::SentryGun {
                match engine::assets::obj_model::load_obj_components(&path) {
                    Ok(parts) if parts.len() == crate::turret::PARTS.len() => {
                        for (part, model) in crate::turret::PARTS.iter().zip(&parts) {
                            renderer.upload_prop(part.key, model);
                        }
                        let (min, max) = crate::turret::assembled_bounds(&parts);
                        world.register_prop_bounds(def.mesh, min, max);
                        log::info!(
                            "loaded prop {} as {} rigged parts ({} verts)",
                            def.name,
                            parts.len(),
                            parts.iter().map(|p| p.vertices.len()).sum::<usize>()
                        );
                    }
                    Ok(parts) => log::warn!(
                        "prop '{}' split into {} pieces, rig expects {} — turret disabled",
                        def.name,
                        parts.len(),
                        crate::turret::PARTS.len()
                    ),
                    Err(e) => log::warn!("prop '{}' load failed: {e}", def.name),
                }
                continue;
            }
            match crate::props::load_prop_model(&path) {
                Ok(mut model) => {
                    // Consolidate the alpha-cutout "secondary" half (glass/chain-link/
                    // grates) onto the opaque primary, so the prop is one merged mesh.
                    if let Some(sec) = crate::props::secondary_glb(def.mesh) {
                        let spath =
                            format!("{}/../../assets/props/{}", env!("CARGO_MANIFEST_DIR"), sec);
                        match crate::props::load_prop_model(&spath) {
                            Ok(secondary) => model.append(secondary),
                            Err(e) => {
                                log::warn!("prop '{}' secondary '{sec}' load failed: {e}", def.name)
                            }
                        }
                    }
                    let (min, max) = model_aabb(&model);
                    world.register_prop_bounds(def.mesh, min, max);
                    renderer.upload_prop(def.key, &model);
                    log::info!("loaded prop {} ({} verts)", def.name, model.vertices.len());
                }
                Err(e) => log::warn!("prop '{}' load failed: {e}", def.name),
            }
        }
        // A weapon pickup has no prop mesh of its own — it draws whichever gun the
        // pickup names, from the weapon library uploaded just above. What it still
        // needs is model bounds, since those drive the placement ghost, the click
        // target and the gizmo. One nominal gun-sized box for every weapon, so the
        // ghost doesn't jump around as the author flicks through the arsenal.
        let (wp_min, wp_max) = crate::world::weapon_pickup_bounds();
        world.register_prop_bounds(crate::ecs::MeshId::WeaponPickup, wp_min, wp_max);
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
            "click=grab/select  WASD+mouse=fly  scroll=size  +/-=carve/extend  B=door(scroll=single/double)  H=hole  P=pillar  R=brace  ↑/↓=stairs(Enter/Esc)  T=platform(select→drag gizmo to move/scale; C=connect K=block stairs[2 clicks, scroll=slide, Shift+scroll=width] F=ground V=rails X=del)  O\u{2192}TOOLS tab=platform/stair shell + stair slope  1-9=room texture  \\=grid/textured  Ctrl+S=save level  O=LEVELS panel(name/save as/load)  F1-F8=load slot  Ctrl+F1-F8=save slot  Y=proc-anim preview(Z=fire)  I=invincible  N=invisible  [/]=wave size  F10=hunter telemetry  J=hunters on/off  G=HUNT  M=shop menu (N64 Start)  [HUNT: click=fire  RMB=aim  B=use/open door  R=reload  Q=weapon  F=detonate mines]"
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
            // While the ring is held it owns the mouse: motion steers the virtual
            // pointer instead of the camera, so the view doesn't swing behind the
            // menu and the world is exactly where you left it on release.
            if self.radial.is_held() {
                self.radial.motion(delta.0 as f32, delta.1 as f32);
                return;
            }
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
                let (px, py) = (position.x as f32, position.y as f32);
                let (dx, dy) = (px - self.cursor_pos.0, py - self.cursor_pos.1);
                self.cursor_pos = (px, py);
                // RMB drags the orthographic drafting view. Absolute-position deltas
                // rather than the raw device motion `device_event` collects, because
                // panning deliberately does *not* grab the cursor -- the author has to
                // keep it where they put it.
                if self.room_panning {
                    let h = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().height as f32)
                        .unwrap_or(0.0);
                    if let Some(w) = self.world.as_mut() {
                        w.pan_room_view(dx, dy, h);
                    }
                }
                // A sticky ring is driven by the real pointer. `cursor_pos` is
                // physical pixels and the ring lives in egui points.
                if self.radial.is_sticky() {
                    let ppp = self.egui_ctx.pixels_per_point().max(0.01);
                    self.radial
                        .cursor(self.cursor_pos.0 / ppp, self.cursor_pos.1 / ppp);
                }
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
                // The ring is drawn on top and hit-tested by us, so while it is up
                // nothing underneath may act on a click — no face select, no fire, no
                // prop drop.
                if self.radial.is_open() {
                    if state == ElementState::Pressed {
                        let ctx = self.radial_ctx();
                        let (action, req) = self.radial.click(&ctx);
                        self.commit_radial(action, req);
                    }
                    return;
                }
                // Record the held state (combat reads it each frame for firing).
                let pressed = state == ElementState::Pressed;
                self.input.set_mouse_left(pressed);
                if !pressed {
                    // Release ends any in-progress prop gizmo / room-corner drag.
                    if let Some(w) = self.world.as_mut() {
                        w.end_prop_gizmo_drag();
                        w.end_room_drag();
                    }
                    return;
                }
                // Room plan tool: it owns the pointer outright while armed. A click
                // grabs a corner handle if one is under the cursor, and otherwise drops
                // a corner / advances the phase / builds the room. Ahead of everything
                // else because the tool has taken the camera over -- there is no
                // crosshair left for the other branches to mean anything with.
                if self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false) {
                    if let Some((o, d)) = self.mouse_world_ray() {
                        let grabbed = self
                            .world
                            .as_mut()
                            .map(|w| w.start_room_drag(o, d))
                            .unwrap_or(false);
                        if !grabbed {
                            let meshes = self
                                .world
                                .as_mut()
                                .map(|w| w.with_undo_many(|w| w.room_click(o, d)))
                                .unwrap_or_default();
                            for rm in &meshes {
                                self.upload(rm);
                            }
                        }
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
                // Spawn-pad placement: same free-cursor click-to-drop as props.
                if self.world.as_ref().map(|w| w.is_placing_spawn_point()).unwrap_or(false) {
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(world) = self.world.as_mut() {
                            world.update_spawn_point_preview(o, d);
                            world.confirm_spawn_point_placement();
                        }
                    }
                    return;
                }
                // Light placement: same free-cursor click-to-drop as props.
                if self.world.as_ref().map(|w| w.is_placing_light()).unwrap_or(false) {
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(world) = self.world.as_mut() {
                            world.update_light_preview(o, d);
                            world.confirm_light_placement();
                        }
                    }
                    return;
                }
                // PAINT tab: while the brush is armed, a left-click paints the one
                // face under the cursor. Ahead of the armed-theme branch because the
                // two are mutually exclusive by construction (arming either disarms
                // the other) and this is the narrower of the pair.
                if self.paint_armed {
                    // The probe is refreshed in the egui pass, which runs after the
                    // cursor moved — so re-probe here rather than paint whatever the
                    // last frame happened to be looking at.
                    if let Some((o, d)) = self.mouse_world_ray() {
                        self.paint_probe = self
                            .world
                            .as_mut()
                            .and_then(|w| w.probe_surface(o, d));
                    }
                    self.paint_probe_face(Some(FaceTex {
                        scheme: self.paint_scheme,
                        zone: self.paint_zone,
                    }));
                    return;
                }
                // TEXTURES tab: while a theme is armed, a left-click retextures the
                // room under the *cursor*. Ray-aimed rather than crosshair-aimed
                // because the panel frees the cursor, which leaves the camera
                // crosshair frozen wherever it last pointed. Checked before prop
                // selection so an armed theme owns the click.
                if let Some(scheme) = self.theme_armed {
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(rm) = self
                            .world
                            .as_mut()
                            .and_then(|w| w.with_undo(|w| w.set_scheme_along(Some((o, d)), scheme)))
                        {
                            self.upload(&rm);
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
                // Grabbed + BUILD, draw tool armed: a click drops a corner, closes the
                // outline, or (at the depth step) builds. It can change several regions
                // at once, so upload every mesh it returns — see `World::draw_click`.
                if self.world.as_ref().map(|w| w.is_draw_tool()).unwrap_or(false) {
                    let meshes = self
                        .world
                        .as_mut()
                        .map(|w| w.with_undo_many(|w| w.draw_click()))
                        .unwrap_or_default();
                    for rm in &meshes {
                        self.upload(rm);
                    }
                    self.refresh_highlight();
                    return;
                }
                // Grabbed + BUILD, ladder tool armed: a click places one.
                if self.world.as_ref().map(|w| w.is_ladder_tool()).unwrap_or(false) {
                    // A ladder is structures geometry, not a marker, so placing one has
                    // to re-bake that mesh for it to appear — the same thing a platform
                    // or a railing does.
                    let rm = self.world.as_mut().and_then(|w| {
                        w.with_undo(|w| w.confirm_ladder().then(|| w.rebuild_structures()))
                    });
                    if let Some(rm) = rm {
                        self.upload(&rm);
                    }
                    self.refresh_highlight();
                    return;
                }
                // Grabbed + BUILD, vent tool armed: a click carves the previewed duct
                // segment and re-anchors the run to its far end.
                if self.world.as_ref().map(|w| w.is_vent_tool()).unwrap_or(false) {
                    let rm = self
                        .world
                        .as_mut()
                        .map(|w| w.with_undo_many(|w| w.vent_click()))
                        .unwrap_or_default();
                    for rm in &rm {
                        self.upload(rm);
                    }
                    self.refresh_highlight();
                    return;
                }
                // Grabbed + BUILD: confirm an armed opening (door/hole) or
                // placement (pillar/brace), else select the crosshair face.
                let opening = self.world.as_ref().map(|w| w.is_opening_arming()).unwrap_or(false);
                let placing = self.world.as_ref().map(|w| w.is_placing()).unwrap_or(false);
                let platform = self.world.as_ref().map(|w| w.is_platform_tool()).unwrap_or(false);
                // A `Vec`, because an opening cut between two regions merges them and
                // reclusters the level — one mesh per surviving region plus a clear per
                // dead id, and dropping any of those leaves stale geometry on screen.
                let meshes = if opening {
                    self.world
                        .as_mut()
                        .map(|w| w.with_undo_many(|w| w.confirm_opening()))
                        .unwrap_or_default()
                } else if placing {
                    self.world
                        .as_mut()
                        .map(|w| w.with_undo_many(|w| w.confirm_place()))
                        .unwrap_or_default()
                } else if platform {
                    // `platform_click` may start a gizmo drag (records its own undo
                    // in `gizmo_start`) or place/connect a structure; `with_undo`
                    // only commits when it actually returns a rebuilt mesh. It builds
                    // free-standing platforms, which never merge regions, so one mesh
                    // is the whole story here.
                    self.world
                        .as_mut()
                        .and_then(|w| w.with_undo(|w| w.platform_click()))
                        .into_iter()
                        .collect()
                } else {
                    if let Some(world) = self.world.as_mut() {
                        world.select_at_crosshair();
                    }
                    Vec::new()
                };
                for rm in &meshes {
                    self.upload(rm);
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
                // Right-click is the universal "no": it dismisses the ring without
                // acting, and never reaches free-aim / the panel underneath.
                if self.radial.is_open() {
                    if state == ElementState::Pressed {
                        self.close_radial();
                    }
                    return;
                }
                let pressed = state == ElementState::Pressed;
                self.input.set_mouse_right(pressed);
                // Room plan tool: in an orthographic view RMB pans (never grabs -- the
                // cursor is the drafting pointer and must stay put); in the perspective
                // view it is mouse-look, exactly as object mode does it.
                if self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false) {
                    if self.world.as_ref().and_then(|w| w.room_view()).is_some() {
                        self.room_panning = pressed;
                    } else {
                        self.set_pointer_lock_keep_tools(pressed);
                    }
                    return;
                }
                // Object mode (BUILD, panel open): hold RMB to mouse-look. Grabbing
                // hides+centres the cursor and enables the raw-motion camera look
                // (the free cursor otherwise drives the panel + gizmo); releasing
                // frees it again. In HUNT, RMB stays the free-aim modifier (unchanged).
                if self.props_open && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false) {
                    self.set_pointer_lock(pressed);
                }
            }

            // Middle mouse = the radial menu (`crate::radial`). Hold to flick, tap
            // for a sticky ring you can read. It was the one mouse button bound to
            // nothing, in either mode.
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Middle,
                ..
            } => {
                if egui_consumed {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if self.radial.is_open() {
                            // Middle-click on a sticky ring picks, same as a left one.
                            let ctx = self.radial_ctx();
                            let (action, req) = self.radial.click(&ctx);
                            self.commit_radial(action, req);
                        } else {
                            self.open_radial();
                        }
                    }
                    ElementState::Released => {
                        if self.radial.is_held() {
                            let ctx = self.radial_ctx();
                            let (action, req) = self.radial.release(&ctx);
                            self.commit_radial(action, req);
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // egui ate the scroll (e.g. a scrollable shop list) → don't also size
                // the editor selection.
                if egui_consumed {
                    return;
                }
                // Don't let a scroll resize the armed tool from behind the ring.
                if self.radial.is_open() {
                    return;
                }
                // Room plan tool: the wheel does the *current phase's* job -- zoom while
                // the outline is open, then the floor height, then the extrude -- with
                // Ctrl forcing zoom, which is the only way to reach it once the loop
                // closes. Ahead of the pointer-lock gate below because this is the one
                // editor tool that deliberately runs with a free cursor.
                if self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false) {
                    let dy = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                    };
                    if dy != 0.0 {
                        let ctrl = self.input.key_down(KeyCode::ControlLeft)
                            || self.input.key_down(KeyCode::ControlRight);
                        let step = if dy > 0.0 { 1.0 } else { -1.0 };
                        if let Some(w) = self.world.as_mut() {
                            w.room_scroll(step, ctrl);
                        }
                    }
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
                    // placement (pillar/brace) sizing, else opening sizing (hole =
                    // free size, door = single/double width), else the sub-face
                    // selection.
                    if world.is_ladder_tool() {
                        world.adjust_ladder_height(step);
                    } else if world.is_vent_tool() {
                        world.adjust_vent_len(step);
                    } else if world.is_draw_sizing() {
                        world.adjust_draw_depth(step);
                    } else if world.is_draw_choosing_face() {
                        // Before the first corner, scroll disambiguates which surface the
                        // crosshair means — two faces meet on an edge, three on a corner.
                        world.cycle_draw_face(step);
                    } else if world.is_connect_sliding() {
                        world.adjust_connect_slide(step);
                    } else if world.is_simple_stair() {
                        // Plain wheel slides the flight sideways, Shift+wheel widens it.
                        world.adjust_simple_stair(du, dv);
                    } else if world.is_platform_placing() {
                        world.adjust_platform_size(du, dv);
                    } else if world.is_placing() {
                        world.adjust_place_size(du, dv);
                    } else if world.is_opening_arming() {
                        // Both opening tools, not just the hole: the door tool uses the
                        // same scroll to pick single vs double width.
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

                // The ring: age the hold clock and run the radius-driven descend /
                // back-out. Costs nothing while it is closed.
                if self.radial.is_open() {
                    let ctx = self.radial_ctx();
                    self.radial.update(dt, &ctx);
                }

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
                // A B tap on the pad is GoldenEye's use button, exactly as on the
                // keyboard: door first, reload if there's no door in reach.
                if pad_actions.reload {
                    self.use_or_reload();
                }
                // B held past PD's 25-tick threshold → switch firing function. The
                // keyboard counterpart is `E`; inert on a single-function weapon.
                if pad_actions.toggle_function {
                    if let Some(world) = self.world.as_mut() {
                        world.toggle_weapon_function(); // HUNT-gated inside
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
                let mut render_ms = 0.0f32;
                let t_sim = std::time::Instant::now();
                if let Some(world) = self.world.as_mut() {
                    for _ in 0..steps {
                        world.fixed_step(fixed_dt, &self.input);
                    }
                    // Advance the skinned character's animation once per frame
                    // (visual; JS mixer.update(delta) cadence, real dt).
                    world.advance_animation(dt);
                    // Weapon pickups hover + turn on a render-frame clock (like the
                    // animation mixer above) so the motion is smooth at any framerate.
                    world.advance_pickups(dt);
                    // Player Combat: advance the weapon + fire on trigger (HUNT
                    // only; JS WeaponSystem.update(dt) cadence, real dt).
                    world.combat_step(dt, &self.input);
                    // A3: pump the hunter's rifle shots (FIRE_TIMING window) + decay
                    // the player damage-flash / HUD-pop timers (HUNT only).
                    world.enemy_combat_step(dt);
                }
                let sim_ms = t_sim.elapsed().as_secs_f32() * 1000.0;
                self.fps_sim_ms += sim_ms;
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
                // The room plan tool needs the cursor free; reconcile that first so
                // the branches below see the settled state.
                self.sync_room_cursor();
                let room_armed = self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false);
                if room_armed {
                    // Re-cast the pointer every frame rather than only on motion: the
                    // drafting plane moves under a stationary cursor whenever the wheel
                    // changes the base height, and the previewed corner has to follow.
                    if let Some((o, d)) = self.mouse_world_ray() {
                        if let Some(w) = self.world.as_mut() {
                            w.room_hover(o, d);
                        }
                    }
                }
                let prop_placing = self
                    .world
                    .as_ref()
                    .map(|w| w.is_build() && w.is_placing_prop())
                    .unwrap_or(false);
                let light_placing = self
                    .world
                    .as_ref()
                    .map(|w| w.is_build() && w.is_placing_light())
                    .unwrap_or(false);
                let spawn_placing = self
                    .world
                    .as_ref()
                    .map(|w| w.is_build() && w.is_placing_spawn_point())
                    .unwrap_or(false);
                if room_armed {
                    // The plan overlay (drawn through the gizmo channel) is the only
                    // feedback this tool has; a stale crosshair face highlight from
                    // before it was armed would just be a bright rectangle nobody
                    // selected.
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(None);
                    }
                } else if spawn_placing {
                    // Spawn-pad ghost: the marker square at the cursor's floor pick.
                    let ray = self.mouse_world_ray();
                    let mesh = ray.and_then(|(o, d)| {
                        self.world.as_mut().and_then(|w| w.update_spawn_point_preview(o, d))
                    });
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(mesh.as_ref());
                    }
                } else if prop_placing {
                    let ray = self.mouse_world_ray();
                    let mesh = ray
                        .and_then(|(o, d)| self.world.as_mut().and_then(|w| w.update_prop_preview(o, d)));
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_highlight(mesh.as_ref());
                    }
                } else if light_placing {
                    // Light-placement ghost: a marker cube at the floor-pick + height.
                    let ray = self.mouse_world_ray();
                    let mesh = ray
                        .and_then(|(o, d)| self.world.as_mut().and_then(|w| w.update_light_preview(o, d)));
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
                    let drawing = self.world.as_ref().map(|w| w.is_draw_tool()).unwrap_or(false);
                    let venting = self.world.as_ref().map(|w| w.is_vent_tool()).unwrap_or(false);
                    let laddering =
                        self.world.as_ref().map(|w| w.is_ladder_tool()).unwrap_or(false);
                    let pending_stair =
                        self.world.as_ref().map(|w| w.has_pending_stair()).unwrap_or(false);
                    // A pending stair suppresses the face highlight; its x-ray
                    // ghost (set below in the render section) owns the feedback.
                    let mesh = self.world.as_mut().and_then(|w| {
                        if pending_stair {
                            None
                        } else if laddering {
                            w.update_ladder_preview()
                        } else if venting {
                            w.update_vent_preview()
                        } else if drawing {
                            w.update_draw_preview()
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
                // Surface tint: which whole plane the draw tool is on. Its own overlay
                // channel (cool, low alpha) rather than part of the yellow highlight,
                // because the two have to be told apart where the outline crosses its own
                // surface. Cleared whenever the tool isn't armed.
                {
                    let tint = self
                        .world
                        .as_mut()
                        .filter(|w| w.is_build() && w.is_draw_tool())
                        .and_then(|w| w.draw_surface_tint_mesh());
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_surface_tint(tint.as_ref());
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

                // Which theme the preview room should show, resolved (and uploaded)
                // before the render borrow block, which cannot take `&mut self` again.
                let theme_preview_subject = self.theme_preview_subject();
                if let Some(scheme) = theme_preview_subject {
                    self.ensure_theme_preview_room(scheme);
                }

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
                    // one per authored prop entity (white tint in M1). The renderer
                    // combines each world matrix with this frame's clip matrix and
                    // lights the prop in world space.
                    renderer.set_prop_draws(world.view_proj(aspect), &world.prop_draws(aspect));
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
                    // The nav overlay is the one colored channel NOT rebuilt per frame:
                    // it is ~90k vertices and only changes when the author presses
                    // Calculate or toggles it, so it uploads on a revision change.
                    if self.nav_overlay_uploaded != Some(world.nav_overlay_rev()) {
                        renderer.set_nav_overlay_mesh(world.nav_overlay_mesh());
                        self.nav_overlay_uploaded = Some(world.nav_overlay_rev());
                    }
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
                        // The dark overlay backs both end-screens: the death beat and the
                        // round result (whose text `hud_mesh` supplies).
                        renderer.set_death_screen(
                            world.is_player_dead() || world.round_outcome().is_some(),
                        );
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
                        let arsenal = world.arsenal_weapons();
                        let sel = self.shop_selected.min(arsenal.len().saturating_sub(1));
                        let name = arsenal[sel].name;
                        renderer.render_weapon_preview(name, self.shop_preview_angle);
                    }
                    // Object panel open → render the selected prop into the same
                    // offscreen preview texture (the panel samples it as an image).
                    //
                    // The TEXTURES tab draws its preview *room* into that same target,
                    // for the scratch theme while editing and for the armed theme while
                    // browsing. Only one can be on screen (they're different tabs), and
                    // the theme room wins when it has a subject — it is the one that has
                    // to track a slider drag frame by frame.
                    if theme_preview_subject.is_some() {
                        renderer.render_theme_preview(self.props_preview_angle);
                    } else if self.props_open {
                        // A weapon pickup previews through the SHOP's weapon preview
                        // (keyed by gun name) rather than the prop one — it is the same
                        // mesh library the pickup itself draws from, so the panel shows
                        // the actual gun that will land on the floor.
                        if let Some(name) = world.armed_pickup_weapon() {
                            renderer.render_weapon_preview(name, self.props_preview_angle);
                        } else if let Some(def) =
                            self.props_selected.and_then(|i| crate::props::CATALOG.get(i))
                        {
                            renderer.render_prop_preview(def.key, self.props_preview_angle);
                        }
                    }
                    // Scene lighting: authored point lights + level ambient. BUILD
                    // follows the flat/real toggle; HUNT forces real lighting whenever
                    // the level has any light (else falls back to flat).
                    let has_lights = world.has_lights();
                    let real_lighting = if world.is_build() {
                        self.build_real_lighting && has_lights
                    } else {
                        has_lights
                    };
                    let amb = world.ambient();
                    // Shadow casters (the most influential lights) only when real
                    // lighting is on; flat mode renders no shadow cubes.
                    if real_lighting {
                        renderer.set_shadow_casters(&world.shadow_casters());
                    } else {
                        renderer.set_shadow_casters(&[]);
                    }
                    renderer.set_lighting(
                        &world.light_draws(),
                        (amb.color, amb.level),
                        real_lighting,
                    );
                    let view_proj = world.view_proj(renderer.aspect());
                    let t_render = std::time::Instant::now();
                    renderer.render(view_proj, egui_frame);
                    // `render` owns the surface acquire (`get_current_texture`) as well as
                    // submit, so a GPU that cannot keep up blocks in here — which is what
                    // makes this number the one that separates "the game is doing too much"
                    // from "the GPU is busy".
                    render_ms = t_render.elapsed().as_secs_f32() * 1000.0;
                    self.fps_render_ms += render_ms;
                }

                // ── The loud half of the telemetry ──
                // A once-per-second average hides a burst: eight good frames and one 200 ms
                // frame average out to something unremarkable. Any single frame over the
                // threshold says so immediately, with its own phase split, so a slowdown
                // that lasts a few seconds leaves a precise record instead of a mood.
                let frame_ms = sim_ms + render_ms;
                if frame_ms >= SLOW_FRAME_MS {
                    self.slow_frames += 1;
                    if self.slow_frames <= SLOW_FRAME_BURST {
                        log::warn!(
                            "slow frame: {frame_ms:.0} ms — sim {sim_ms:.1}, render+present                              {render_ms:.1} ({})",
                            if render_ms > sim_ms * 2.0 {
                                "GPU/present bound"
                            } else {
                                "CPU bound"
                            }
                        );
                    }
                }

                // Frame-time telemetry, logged once per second.
                self.fps_frames += 1;
                self.fps_elapsed += dt;
                self.fps_worst_ms = self.fps_worst_ms.max(dt * 1000.0);
                if self.fps_elapsed >= 1.0 {
                    let avg_ms = self.fps_elapsed * 1000.0 / self.fps_frames as f32;
                    let n = self.fps_frames as f32;
                    let (sim, render) = (self.fps_sim_ms / n, self.fps_render_ms / n);
                    log::info!(
                        "{:.0} fps (avg {avg_ms:.2} ms/frame, worst {:.2} ms) — sim {sim:.2},                          render+present {render:.2}, other {:.2}; {} slow frame(s); nav {}",
                        self.fps_frames as f32 / self.fps_elapsed,
                        self.fps_worst_ms,
                        (avg_ms - sim - render).max(0.0),
                        self.slow_frames,
                        engine::sim::nav::path_stats(),
                    );
                    engine::sim::nav::reset_path_stats();
                    self.slow_frames = 0;
                    self.fps_frames = 0;
                    self.fps_elapsed = 0.0;
                    self.fps_worst_ms = 0.0;
                    self.fps_sim_ms = 0.0;
                    self.fps_render_ms = 0.0;
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
        // The ring owns the keyboard while it is up: Esc dismisses it, and nothing
        // else fires underneath. Above Esc's own four-deep ladder on purpose — with a
        // menu on screen, Esc means "close the menu" and nothing else.
        if self.radial.is_open() {
            if code == KeyCode::Escape {
                self.close_radial();
            }
            return;
        }
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
                } else if w.is_ladder_tool() {
                    w.cancel_ladder();
                    handled = true;
                } else if w.is_vent_tool() {
                    // Esc *finishes* a duct rather than discarding it — the segments are
                    // already carved and individually undoable, so there is nothing
                    // pending to throw away, and this is where the one-mouth check runs.
                    w.cancel_vent();
                    handled = true;
                } else if w.room_escape() {
                    // Back out one rung of the room ladder (height -> base -> reopen
                    // the outline -> one corner at a time). An empty outline returns
                    // false and falls through to the cursor release, which disarms it.
                    handled = true;
                } else if w.draw_escape() {
                    // Back out one rung of the draw ladder (depth step → outline →
                    // one corner at a time). Idle returns false and falls through to
                    // the cursor release, which disarms the tool.
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
        // The room plan tool owns the numpad while it is armed. Handled up here,
        // above the pointer-lock gate further down, because the tool runs with a
        // free cursor by design.
        if self.world.as_ref().map(|w| w.is_room_tool()).unwrap_or(false) && self.room_key(code) {
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
                self.theme_armed = None;
                self.paint_armed = false;
                if let Some(w) = self.world.as_mut() {
                    w.cancel_prop_placement();
                    w.cancel_light_placement();
                    w.cancel_spawn_point_placement();
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
            // Delete / Backspace removes the selected prop (matches the panel button).
            if code == KeyCode::Delete || code == KeyCode::Backspace {
                if let Some(w) = self.world.as_mut() {
                    w.delete_selected_prop();
                }
                return;
            }
        }
        // Backslash toggles the checkerboard "grid" view vs the textured view
        // (JS `toggle_view`). Works whether or not the cursor is grabbed.
        if code == KeyCode::Backslash {
            self.apply(EditorAction::ToggleGrid);
            return;
        }
        // L toggles real point lighting vs the flat legacy look. This is a BUILD
        // preference — HUNT always shows real lighting when the level has any light.
        if code == KeyCode::KeyL {
            self.apply(EditorAction::ToggleLighting);
            return;
        }
        // Level save/load quick-slots (works grabbed or not, like the grid
        // toggle): F1–F8 LOAD slot 1–8; Ctrl+F1–F8 SAVE slot 1–8. Saving keeps
        // the current editable geometry; loading replaces it (BUILD only).
        if let Some(slot) = slot_for_fkey(code) {
            let ctrl = self.input.key_down(KeyCode::ControlLeft)
                || self.input.key_down(KeyCode::ControlRight);
            self.apply(if ctrl {
                EditorAction::SaveSlot(slot)
            } else {
                EditorAction::LoadSlot(slot)
            });
            return;
        }
        // Ctrl+S saves the open level back to its own file. Sits with the other
        // save/load keys (works grabbed or not) rather than behind the panel, because
        // the whole point of it is not having to open anything.
        if code == KeyCode::KeyS {
            let ctrl = self.input.key_down(KeyCode::ControlLeft)
                || self.input.key_down(KeyCode::ControlRight);
            let build = self.world.as_ref().map(|w| w.is_build()).unwrap_or(false);
            if ctrl && build {
                self.apply(EditorAction::SaveCurrentLevel);
                return;
            }
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
        // F10 captures hunter telemetry to a file — the "it is happening RIGHT NOW"
        // button. A frozen hunter tells you nothing from the outside: this writes what
        // each one thinks it is doing, what it is walking to, which gate refused its last
        // step and for how long, plus whether A* can even route it to you. Appends, so a
        // session of presses is one timeline rather than a file you have to catch.
        if code == KeyCode::F10 {
            self.apply(EditorAction::DumpTelemetry);
            return;
        }
        // I toggles player invincibility (dev/observe): enemies keep aiming + firing
        // but you take no damage, so you can watch them chase + shoot. Works anytime.
        if code == KeyCode::KeyI {
            self.apply(EditorAction::ToggleInvincible);
            return;
        }
        // Backquote toggles hunters entirely (dev): no pack spawns, and flipping it off
        // during a hunt clears the live one. The third of the dev observe toggles beside
        // I (invincible) and N (invisible) — this one is for authoring and testing the
        // level itself, where standing still at a door to work it is the whole point.
        //
        // **Moved off `J`**, which now places ladders. `J` was the only letter left when
        // the ladder tool landed and its radial slot already advertised it, so the tool
        // kept the letter and the dev toggle took the classic dev key. It is still on the
        // radial's Debug ring either way, which is what made the swap cheap.
        if code == KeyCode::Backquote {
            self.apply(EditorAction::ToggleHunters);
            return;
        }
        // N toggles player invisibility (dev/observe): no hunter can perceive you, so
        // the pack drops to searching — walk around and watch them scan for you.
        if code == KeyCode::KeyN {
            self.apply(EditorAction::ToggleInvisible);
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
        // `[` / `]` set how many hunters the wave floods in, live: mid-HUNT it
        // re-floods immediately (like the difficulty dial), in BUILD it takes effect at
        // the next G. Works in both modes — the brackets are bound to nothing else.
        //
        // The reason it is a live dial and not a menu: its first job is bisecting a
        // stall. One hunter that walks a corridor cleanly where four jam is a crowding
        // bug; one that jams either way is not, and that is two keypresses to find out.
        if matches!(code, KeyCode::BracketLeft | KeyCode::BracketRight) {
            self.apply(EditorAction::WaveSize(
                if code == KeyCode::BracketRight { 1 } else { -1 },
            ));
            return;
        }
        // Authoring only while grabbed (crosshair is meaningful).
        if !self.input.pointer_locked {
            return;
        }
        // Number keys 1-9 retexture the room under the crosshair (flood-fill,
        // bounded by door/hole frames).
        if let Some(key) = digit_char(code) {
            // This level's own binding wins over the manifest's default key.
            let scheme = self.world.as_ref().and_then(|w| w.scheme_for_key(key));
            if let Some(scheme) = scheme {
                self.apply(EditorAction::SetScheme(scheme));
            }
            return;
        }
        // G toggles BUILD ↔ HUNT (freeze + drop in as the player, or back).
        if code == KeyCode::KeyG {
            self.apply(EditorAction::EnterHunt);
            return;
        }
        // Spike: procedural-anim preview (BUILD only). Y toggles a preview character
        // in front of the camera; Z fires a manual recoil kick. See
        // `world::spike_preview`.
        if code == KeyCode::KeyY {
            self.apply(EditorAction::ToggleProcPreview);
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
        // Q cycles the player's weapon in HUNT (the JS `KeyQ` bind); in BUILD it arms
        // the freeform draw tool — click out a 90°-snapped outline on a surface, close
        // it, scroll a depth, click to extrude or inset (`world::tools::draw`).
        if code == KeyCode::KeyQ {
            if self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false) {
                self.begin_weapon_switch();
                return;
            }
            self.apply(EditorAction::ArmTool(Tool::Draw));
            return;
        }
        // B in HUNT: the **use** button. GoldenEye and Perfect Dark open a door by
        // pressing B when you're at it — they do not swing open as you walk up — and
        // that button is context-sensitive, so this tries the door first and falls
        // through to reload when there's none in reach. Placed above the BUILD B/H
        // opening-tool handler below, which returns early and would otherwise swallow
        // the key; the two meanings line up nicely anyway (B cuts a doorway in BUILD,
        // B works the door in HUNT).
        if code == KeyCode::KeyB
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            self.use_or_reload();
            return;
        }
        // B / H toggle the opening tools (door / hole): arm a ghost preview that
        // tracks the crosshair (drawn each frame in RedrawRequested), or turn it
        // back off. Left-click is what cuts (handled in MouseInput).
        // U ("dUct") toggles the vent tool. Stateful across clicks unlike the opening
        // tools — see `tools::vent` — so pressing U again *finishes* the duct.
        if code == KeyCode::KeyU {
            self.apply(EditorAction::ArmTool(Tool::Vent));
            return;
        }
        // J places a climbable ladder on a wall (player-only — see `tools::ladder`).
        if code == KeyCode::KeyJ
            && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false)
        {
            self.apply(EditorAction::ArmTool(Tool::Ladder));
            return;
        }
        if code == KeyCode::KeyB || code == KeyCode::KeyH {
            self.apply(EditorAction::ArmTool(if code == KeyCode::KeyB {
                Tool::Door
            } else {
                Tool::Hole
            }));
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
        // E in HUNT: switch the equipped weapon between its primary and secondary
        // firing function — Perfect Dark's `functions[2]`. A mode toggle, not a
        // second trigger, because that is what PD does: the choice is a persistent
        // per-weapon bit (`bgun_is_using_secondary_function`, `bondgun.c:9043`), so
        // it is remembered per weapon rather than held down.
        //
        // `E` and not `F` — F in HUNT already detonates remote mines, and right
        // mouse is the GoldenEye free-aim modifier. Inert on a GoldenEye weapon,
        // which has one function.
        if code == KeyCode::KeyE
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                world.toggle_weapon_function();
            }
            return;
        }
        // R in HUNT, in priority order: start the next round from the result screen,
        // skip the rest of the death beat if dead, else reload the weapon. (In BUILD it's
        // the brace tool, below.)
        if code == KeyCode::KeyR
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                if world.round_outcome().is_some() {
                    world.restart_round();
                } else if world.is_player_dead() {
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
            self.apply(EditorAction::ArmTool(if code == KeyCode::KeyP {
                Tool::Pillar
            } else {
                Tool::Brace
            }));
            return;
        }
        // Platform + stair-run tool. T toggles the tool; the rest act on the
        // current selection / phase. Grounded/railings/delete change geometry, so
        // they return the rebuilt structures mesh to upload.
        // `T` arms the platform tool, and only that. What it *builds* — slab or plane,
        // stairs or ramp, and the stair slope — lives in the panel's TOOLS tab. It was a
        // hotkey and a modified wheel first, and both were wrong for the same reason: a
        // setting you cannot see is one you have to place something to discover, and the
        // wheel was already carrying footprint size and the stair slide.
        if code == KeyCode::KeyT {
            self.apply(EditorAction::ArmTool(Tool::Platform));
            return;
        }
        if code == KeyCode::KeyC {
            self.apply(EditorAction::ArmTool(Tool::Connect));
            return;
        }
        if code == KeyCode::KeyK {
            self.apply(EditorAction::ArmTool(Tool::BlockStairs));
            return;
        }
        // `0` flips the selection between one face and the whole coplanar patch of the
        // room it belongs to (`world::patch`). A toggle rather than a held modifier
        // because the gesture it has to survive is scroll-a-sub-rect *then* push, which
        // a held key cannot span; the highlight draws the live scope, so a persistent
        // mode is still visible rather than modal.
        //
        // `0` and not the obvious `Tab`: egui-winit consumes Tab **unconditionally** to
        // move widget focus ("hence Tab always consumes", its own comment), so a Tab
        // binding here can never fire. `0` sits beside the `-`/`=` it belongs with, and
        // is the one digit `digit_char` leaves free — 1-9 are the theme hotkeys.
        if matches!(code, KeyCode::Digit0 | KeyCode::Numpad0)
            && self.world.as_ref().map(|w| w.is_build()).unwrap_or(false)
        {
            self.apply(EditorAction::Selection(SelectionOp::ToggleScope));
            return;
        }
        if matches!(code, KeyCode::KeyF | KeyCode::KeyV | KeyCode::KeyX | KeyCode::Delete) {
            self.apply(EditorAction::Selection(match code {
                KeyCode::KeyF => SelectionOp::Grounded,
                KeyCode::KeyV => SelectionOp::Railings,
                _ => SelectionOp::Delete,
            }));
            return;
        }
        // Stair tool (JS-faithful): Arrow Up/Down grow a pending up/down-stair
        // op on the selected floor-touching wall face; Enter confirms. No mode.
        if matches!(code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
            self.apply(EditorAction::Selection(if code == KeyCode::ArrowUp {
                SelectionOp::StairUp
            } else {
                SelectionOp::StairDown
            }));
            return;
        }
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
            // While a duct is being run, Enter breaks it out into a protoroom at the
            // open end and finishes the duct - the vent counterpart of the protoroom
            // `cut_opening` seeds beyond every doorway. Otherwise Enter confirms stairs.
            if self.world.as_ref().map(|w| w.is_vent_running()).unwrap_or(false) {
                let rm = self
                    .world
                    .as_mut()
                    .map(|w| w.with_undo_many(|w| w.vent_exit_room()))
                    .unwrap_or_default();
                for rm in &rm {
                    self.upload(rm);
                }
                self.refresh_highlight();
                return;
            }
            self.apply(EditorAction::Selection(SelectionOp::ConfirmStairs));
            return;
        }

        // `+` and `=` share a key; NumpadAdd for good measure. Each press is one
        // undo step (a no-op push/pull returns `None`, so `with_undo` records
        // nothing), and Shift makes it the fine 1-WT step — read in `selection_op`.
        match code {
            KeyCode::Equal | KeyCode::NumpadAdd => {
                self.apply(EditorAction::Selection(SelectionOp::Push))
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                self.apply(EditorAction::Selection(SelectionOp::Pull))
            }
            _ => {}
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
