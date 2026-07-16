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
use engine::render::renderer::Renderer;
use crate::world::{World, PUSH_PULL_STEP};

/// Fixed simulation rate (120 Hz), sim-step cap per frame (8), and render FPS
/// cap (240) — driven by the engine [`FrameClock`]. Fixed-timestep sim keeps
/// physics/movement frame-rate independent; the FPS cap stops the loop burning
/// the GPU rendering frames nobody sees.
const SIM_HZ: f32 = 120.0;
const MAX_SUBSTEPS: u32 = 8;
const MAX_FPS: u32 = 240;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    world: Option<World>,
    input: InputState,
    /// Fixed-timestep + frame-pacing clock (engine primitive).
    clock: FrameClock,
    // Throttled frame-time telemetry.
    fps_frames: u32,
    fps_elapsed: f32,
    fps_worst_ms: f32,
}

impl App {
    fn new() -> Self {
        App {
            window: None,
            renderer: None,
            world: None,
            input: InputState::default(),
            clock: FrameClock::new(SIM_HZ, MAX_SUBSTEPS, MAX_FPS),
            fps_frames: 0,
            fps_elapsed: 0.0,
            fps_worst_ms: 0.0,
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

        // Build the world, upload its initial region meshes.
        let mut world = World::new();
        for rm in world.initial_meshes() {
            renderer.set_region_textured(rm.id, &rm.mesh);
        }
        // B1: upload the skinned character once (geometry + textures); its pose is
        // driven per frame below.
        if let Some(m) = world.character_model() {
            renderer.upload_character(m);
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
        // Player Combat P3: upload the code-defined HUD glyph atlas once (the ammo
        // counter's bitmap font); the per-frame text quads are set below.
        let (hw, hh, hpx) = crate::hud::atlas_rgba();
        renderer.upload_hud_atlas(hw, hh, &hpx);
        log::info!(
            "click=grab/select  WASD+mouse=fly  scroll=size  +/-=carve/extend  B=door  H=hole  P=pillar  R=brace  ↑/↓=stairs(Enter/Esc)  T=platform(select→drag gizmo to move/scale; C=connect K=simple F=ground V=rails X=del)  1-9=room texture  \\=grid/textured  L=char walk/jog/run  Z=fire N=hit M=death  G=HUNT  [HUNT: click=fire  RMB=aim  R=reload]"
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
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                // Record the held state (combat reads it each frame for firing).
                let pressed = state == ElementState::Pressed;
                self.input.set_mouse_left(pressed);
                if !pressed {
                    return; // release: state recorded, nothing else to do
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
                    self.world.as_mut().and_then(|w| w.confirm_opening())
                } else if placing {
                    self.world.as_mut().and_then(|w| w.confirm_place())
                } else if platform {
                    self.world.as_mut().and_then(|w| w.platform_click())
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
                self.input.set_mouse_right(state == ElementState::Pressed);
            }

            WindowEvent::MouseWheel { delta, .. } => {
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
                    world.look(&mut self.input, dt);
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
                }
                // Per-frame highlight in BUILD while grabbed: the door ghost, or
                // the crosshair-tracked selection sub-rect (camera look was
                // applied above this frame).
                if self.input.pointer_locked
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
                if let (Some(world), Some(renderer)) =
                    (self.world.as_ref(), self.renderer.as_mut())
                {
                    renderer.set_entity_mesh(world.enemy_mesh().as_ref());
                    // B1: drive the skinned character's pose (bind pose for now).
                    if let Some((model, joints)) = world.character_pose() {
                        renderer.set_character_pose(model, &joints);
                    }
                    // Player Combat: drive the gun + muzzle-flash overlay
                    // transforms (shown only in HUNT; `None` hides them) and the
                    // live hit-spark markers.
                    let aspect = renderer.aspect();
                    renderer.set_viewmodel_transform(world.viewmodel_transform(aspect));
                    renderer.set_muzzle_transform(world.muzzle_transform(aspect));
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
                    renderer.set_spark_mesh(world.spark_mesh().as_ref());
                    // Player Combat P3: the ammo-counter HUD (HUNT only; `None` in BUILD).
                    renderer.set_hud_mesh(world.hud_mesh(aspect).as_deref());
                    renderer.set_door_mesh(world.door_mesh().as_ref());
                    // Pending-stair ghost — `None` (auto-clears) unless a stair op
                    // is in progress in BUILD.
                    renderer.set_stair_ghost(world.stair_preview_mesh().as_ref());
                    // Platform gizmo — `None` unless a platform is selected in BUILD.
                    renderer.set_gizmo_mesh(world.gizmo_mesh().as_ref());
                    let view_proj = world.view_proj(renderer.aspect());
                    renderer.render(view_proj);
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
        // Authoring only while grabbed (crosshair is meaningful).
        if !self.input.pointer_locked {
            return;
        }
        // Number keys 1-9 retexture the room under the crosshair (flood-fill,
        // bounded by door/hole frames).
        if let Some(key) = digit_char(code) {
            if let Some(scheme) = engine::render::textures::scheme_for_key(key) {
                if let Some(rm) = self.world.as_mut().and_then(|w| w.set_scheme_at_crosshair(scheme)) {
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
        // Character animation demo (BUILD only — in HUNT the model is the
        // nav/AI-driven hunter). L cycles locomotion; Z/N/M fire/hit/death.
        if matches!(code, KeyCode::KeyL | KeyCode::KeyZ | KeyCode::KeyN | KeyCode::KeyM) {
            if let Some(world) = self.world.as_mut() {
                if world.is_build() {
                    match code {
                        KeyCode::KeyL => world.cycle_char_speed(),
                        KeyCode::KeyZ => world.char_fire(),
                        KeyCode::KeyN => world.char_hit(),
                        _ => world.char_death(),
                    }
                }
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
        // R in HUNT reloads the weapon (in BUILD it's the brace tool, below).
        if code == KeyCode::KeyR
            && self.world.as_ref().map(|w| !w.is_build()).unwrap_or(false)
        {
            if let Some(world) = self.world.as_mut() {
                world.reload_weapon();
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
            let rm = self.world.as_mut().and_then(|w| match code {
                KeyCode::KeyF => w.toggle_grounded_key(),
                KeyCode::KeyV => w.toggle_railings_key(),
                _ => w.delete_selected(),
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
            if let Some(rm) = self.world.as_mut().and_then(|w| w.confirm_stairs()) {
                self.upload(&rm);
                self.refresh_highlight();
            }
            return;
        }

        let fine = self.input.key_down(KeyCode::ShiftLeft) || self.input.key_down(KeyCode::ShiftRight);
        let step = if fine { 1.0 } else { PUSH_PULL_STEP };

        let result = match code {
            // `+` and `=` share a key; NumpadAdd for good measure.
            KeyCode::Equal | KeyCode::NumpadAdd => {
                self.world.as_mut().and_then(|w| w.push(step))
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                self.world.as_mut().and_then(|w| w.pull(step))
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
