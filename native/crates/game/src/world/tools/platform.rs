//! Free-standing platform + stair-run tool: the phase machine, placement,
//! connect flow, simple-stair, grounded/railings/delete, and the structures
//! rebuild + pick.

use super::super::*;
use engine::render::textures::{simple_scheme, RAILING_ZONE};

impl World {
    // ─── Free-standing platform + stair-run tool ────────────────────────

    /// Whether the platform tool is armed (the app routes clicks/scroll/ghost to
    /// it, and Esc backs out of its sub-phases).
    pub fn is_platform_tool(&self) -> bool {
        self.platform_phase.is_some()
    }

    /// Whether the platform tool is in its idle/placement phase (so the app shows
    /// the placement ghost and routes scroll to footprint sizing).
    pub fn is_platform_placing(&self) -> bool {
        self.platform_phase == Some(PlatformPhase::Idle)
    }

    /// Disarm the platform tool entirely (Esc / pointer release), mirroring
    /// `cancel_opening`/`cancel_place`.
    pub fn cancel_platform_tool(&mut self) {
        self.clear_platform_state();
    }

    /// Clear all platform-tool state (turning the tool off).
    pub(crate) fn clear_platform_state(&mut self) {
        self.platform_phase = None;
        self.selected_platform = None;
        self.selected_run = None;
        self.connect_from = None;
        self.connect_to = None;
        self.connect_edge = None;
        self.simple_from = None;
        self.simple_ghost = None;
        self.simple_self_armed = false;
        self.gizmo_drag = None;
    }

    /// Platform tool key (`T`): arm/toggle. Arming disarms the opening/placement
    /// tools + drops any face selection (mutually exclusive modal tools).
    pub fn platform_tool_key(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.platform_phase.is_some() {
            self.clear_platform_state();
        } else {
            self.opening_tool = None;
            self.opening_preview = None;
            self.place_tool = None;
            self.clear_draw_state();
            self.selected = None;
            self.platform_size_x = PLATFORM_SIZE;
            self.platform_size_z = PLATFORM_SIZE;
            self.platform_phase = Some(PlatformPhase::Idle);
            log::info!("platform tool armed — click a surface to place, click a platform to select");
        }
    }

    /// Esc while the platform tool is active: cancel an active gizmo drag
    /// (restoring the platform), else back out of a sub-phase (connect /
    /// simple-stair). Returns `(consumed, changed_mesh)` — `consumed` tells the
    /// app not to also release the pointer; `changed_mesh` is `Some` when the
    /// cancel restored geometry that must be re-uploaded.
    pub fn platform_escape(&mut self) -> (bool, Option<RegionMesh>) {
        if let Some(drag) = self.gizmo_drag.take() {
            // Restore the platform to its pre-drag transform.
            if let Some(p) = self.platforms.iter_mut().find(|p| p.id == drag.platform_id) {
                *p = drag.orig;
            }
            return (true, Some(self.rebuild_structures()));
        }
        match self.platform_phase {
            // Back-out ladder: ConnectSrc → re-pick destination; ConnectDst → done.
            Some(PlatformPhase::ConnectSrc) => {
                self.connect_to = None;
                self.connect_edge = None;
                self.platform_phase = Some(PlatformPhase::ConnectDst);
                (true, None)
            }
            Some(PlatformPhase::ConnectDst) => {
                self.connect_from = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                (true, None)
            }
            Some(PlatformPhase::SimpleFrom) | Some(PlatformPhase::SimpleTo) => {
                self.cancel_simple_stair();
                (true, None)
            }
            _ => (false, None),
        }
    }

    /// Handle a left-click while the platform tool is armed. The gizmo takes
    /// precedence when a platform is selected: a click confirms an active drag,
    /// or starts one if a handle is under the crosshair (JS `gizmo` click flow).
    /// Otherwise dispatch on the phase: place/select, connect, or simple-stair.
    /// Returns the rebuilt structures mesh when geometry changed, else `None`.
    pub fn platform_click(&mut self) -> Option<RegionMesh> {
        if self.gizmo_drag.is_some() {
            self.gizmo_drag = None; // click confirms the drag (geometry already applied)
            return None;
        }
        if self.platform_phase == Some(PlatformPhase::Selected) {
            if let Some(handle) = self.gizmo_pick() {
                self.gizmo_start(handle);
                return None;
            }
        }
        match self.platform_phase? {
            PlatformPhase::Idle | PlatformPhase::Selected => self.place_or_select_click(),
            PlatformPhase::ConnectDst => {
                self.connect_lock_target();
                None
            }
            PlatformPhase::ConnectSrc => self.connect_confirm(),
            PlatformPhase::SimpleFrom => {
                self.simple_stair_first_click();
                None
            }
            PlatformPhase::SimpleTo => self.simple_stair_second_click(),
        }
    }

    /// Idle/Selected click: select the platform/stair-run under the crosshair, or
    /// (nothing hit while selected) deselect, or (nothing hit while idle) place a
    /// new platform. Ports `indoorClick.js` platform idle/selected branch.
    pub(crate) fn place_or_select_click(&mut self) -> Option<RegionMesh> {
        let hit = self.pick_structure_hit();
        if let Some(h) = hit {
            if let Some(pid) = h.platform {
                self.selected_platform = Some(pid);
                self.selected_run = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                log::info!("selected platform {pid}");
                return None;
            }
            if let Some(rid) = h.run {
                self.selected_run = Some(rid);
                self.selected_platform = None;
                self.platform_phase = Some(PlatformPhase::Selected);
                log::info!("selected stair-run {rid}");
                return None;
            }
        }
        // Empty surface (or miss): a selected platform deselects; an idle click
        // places a new platform at the hit.
        if self.platform_phase == Some(PlatformPhase::Selected) {
            self.selected_platform = None;
            self.selected_run = None;
            self.platform_phase = Some(PlatformPhase::Idle);
            log::info!("deselected");
            return None;
        }
        let mut p = self.resolve_platform_placement(hit?)?;
        p.id = self.next_platform_id;
        self.next_platform_id += 1;
        let id = p.id;
        self.platforms.push(p);
        self.selected_platform = Some(id);
        self.selected_run = None;
        self.platform_phase = Some(PlatformPhase::Selected);
        log::info!("placed platform {id}");
        Some(self.rebuild_structures())
    }

    /// Resolve the platform the crosshair would place (id 0 — the caller assigns
    /// the real id). "Aim-point sets top surface": the platform's top Y is the
    /// (WT-snapped) hit Y, centered on the hit in XZ. Aiming at a vertical wall
    /// butts the near edge against the wall (JS `indoorClick.js` wall-offset).
    pub(crate) fn resolve_platform_placement(&self, h: StructureHit) -> Option<Platform> {
        let sx = h.hit_wt.x.round();
        let sy = h.hit_wt.y.round();
        let sz = h.hit_wt.z.round();
        let size_x = self.platform_size_x;
        let size_z = self.platform_size_z;
        let mut px = sx - (size_x / 2.0).floor();
        let mut pz = sz - (size_z / 2.0).floor();
        if h.axis == Axis::X {
            let cam = self.camera.pos.x / WORLD_SCALE;
            px = if cam > sx { sx } else { sx - size_x };
        } else if h.axis == Axis::Z {
            let cam = self.camera.pos.z / WORLD_SCALE;
            pz = if cam > sz { sz } else { sz - size_z };
        }
        Some(Platform {
            id: 0,
            x: px,
            y: sy,
            z: pz,
            size_x,
            size_z,
            thickness: PLATFORM_THICKNESS,
            grounded: false,
            railings: false,
        })
    }

    /// The platform-tool ghost, drawn via the translucent highlight pipeline:
    /// - `Idle` — the to-be-placed platform slab.
    /// - `ConnectDst` — a small marker cube at the destination the crosshair would
    ///   lock (no swinging staircase — the target isn't chosen yet).
    /// - `ConnectSrc` — the stable stair-run ghost; only the attach offset slides
    ///   along the frozen source edge as you aim (JS connect-preview).
    pub fn update_platform_preview(&mut self) -> Option<CpuMesh> {
        // The simple-stair tool draws through the yellow x-ray stair-ghost channel
        // instead (see `simple_stair_ghost_mesh`), so it owns no face highlight.
        // Refreshed here because building it needs `&mut self` (the crosshair pick
        // raycasts) while the render pass only holds `&World`.
        if self.is_simple_stair() {
            self.simple_ghost = self.simple_stair_ghost_mesh();
            return None;
        }
        self.simple_ghost = None;
        match self.platform_phase? {
            PlatformPhase::Idle => {
                let hit = self.pick_structure_hit()?;
                if hit.platform.is_some() || hit.run.is_some() {
                    return None;
                }
                let p = self.resolve_platform_placement(hit)?;
                let brushes = self.all_region_brushes();
                let b = p.solid_box(&brushes)?;
                Some(boxes_mesh(&[b]))
            }
            PlatformPhase::ConnectDst => {
                // A 1-WT marker where the destination will lock (platform edge
                // midpoint, or the snapped floor point) — stable, no staircase yet.
                let hit = self.pick_structure_hit()?;
                let c = match hit.platform.filter(|&id| Some(id) != self.connect_from) {
                    Some(tid) => {
                        let tp = self.platform_by_id(tid)?;
                        let edge = structures::closest_platform_edge(&tp, hit.hit_wt.x, hit.hit_wt.z);
                        let (mx, mz) = tp.edge_point_at_offset(edge, 0.5);
                        [mx, tp.y, mz]
                    }
                    None => [hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round()],
                };
                Some(boxes_mesh(&[[c[0] - 0.5, c[1] - 0.5, c[2] - 0.5, 1.0, 1.0, 1.0]]))
            }
            PlatformPhase::ConnectSrc => {
                let run = self.resolve_connect_run()?;
                let (fp, tp) = self.run_platforms(&run);
                let brushes = self.all_region_brushes();
                let boxes = structures::stair_run_boxes(&run, fp.as_ref(), tp.as_ref(), &brushes);
                Some(boxes_mesh(&boxes))
            }
            _ => None,
        }
    }

    /// Scroll-size the next platform's footprint (idle phase): `du` = X, `dv` = Z.
    pub fn adjust_platform_size(&mut self, du: f32, dv: f32) {
        if self.platform_phase != Some(PlatformPhase::Idle) {
            return;
        }
        if du != 0.0 {
            self.platform_size_x = (self.platform_size_x + du).clamp(PLATFORM_SIZE_MIN, PLATFORM_SIZE_MAX);
        }
        if dv != 0.0 {
            self.platform_size_z = (self.platform_size_z + dv).clamp(PLATFORM_SIZE_MIN, PLATFORM_SIZE_MAX);
        }
    }

    /// Connect key (`C`): arm the stair-connect from the selected platform. The
    /// next click picks the destination (JS `connect_stairs`, phase `connecting_dst`).
    pub fn connect_key(&mut self) {
        if self.platform_phase == Some(PlatformPhase::Selected) {
            if let Some(pid) = self.selected_platform {
                self.connect_from = Some(pid);
                self.platform_phase = Some(PlatformPhase::ConnectDst);
                log::info!("connect: click a destination platform or the floor (Esc cancels)");
                return;
            }
        }
        // Unlike `K`, this guard is real — connect joins a *selected* platform to
        // something else, so there is nothing to arm from cold. Say so rather than
        // returning in silence, which is what made the block-stair defect invisible.
        log::info!("connect (C): select a platform first — T arms the tool, then click one");
    }

    /// Connect step 1 (JS `connecting_dst`): lock the destination the crosshair is
    /// on (a platform's nearest edge, or a floor point) and freeze the source edge
    /// that faces it, then advance to the slide step. No build, no phase change if
    /// nothing valid is under the crosshair.
    pub(crate) fn connect_lock_target(&mut self) {
        let Some(from_id) = self.connect_from else {
            return;
        };
        let Some(from_plat) = self.platform_by_id(from_id) else {
            return;
        };
        let Some(hit) = self.pick_structure_hit() else {
            return;
        };
        let (target, approx) = match hit.platform.filter(|&id| id != from_id) {
            Some(tid) => {
                let Some(to_plat) = self.platform_by_id(tid) else {
                    return;
                };
                let edge = structures::closest_platform_edge(&to_plat, hit.hit_wt.x, hit.hit_wt.z);
                let (tx, tz) = to_plat.edge_point_at_offset(edge, 0.5);
                (
                    ConnectTarget::Platform { id: tid, edge },
                    Vec3::new(tx, to_plat.y, tz),
                )
            }
            None => {
                let (gx, gy, gz) = (hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round());
                (
                    ConnectTarget::Ground { x: gx, y: gy, z: gz },
                    Vec3::new(gx, gy, gz),
                )
            }
        };
        // Source edge = the one whose outward normal best faces the target — chosen
        // ONCE here and frozen, so the ghost can't swing sides while sliding.
        let from_edge = structures::best_edge_for_direction(
            approx.x - from_plat.center_x(),
            approx.z - from_plat.center_z(),
        );
        self.connect_to = Some(target);
        self.connect_edge = Some(from_edge);
        // Start the attach point at the edge midpoint; the wheel slides it in 1-WT
        // steps from there.
        self.connect_slide_wt = (from_plat.edge_length(from_edge) / 2.0).round();
        self.platform_phase = Some(PlatformPhase::ConnectSrc);
        log::info!("connect: destination locked — scroll to slide along the edge, click to place (Esc re-picks)");
    }

    /// Connect step 2 commit (JS `connecting_src` click): build the resolved run
    /// and return to `Selected`. Clears the connect state.
    pub(crate) fn connect_confirm(&mut self) -> Option<RegionMesh> {
        let run = self.resolve_connect_run();
        self.platform_phase = Some(PlatformPhase::Selected);
        self.connect_from = None;
        self.connect_to = None;
        self.connect_edge = None;
        let mut run = match run {
            Some(r) => r,
            None => {
                log::info!("connect: endpoints too close or level — nothing built");
                return None;
            }
        };
        run.id = self.next_run_id;
        self.next_run_id += 1;
        let id = run.id;
        self.stair_runs.push(run);
        log::info!("stair-run {id} created");
        Some(self.rebuild_structures())
    }

    /// Resolve the stair-run for the current slide (id 0), from the locked
    /// destination + source edge and the crosshair-projected attach offset. `None`
    /// if the endpoints are too close horizontally or level. Shared by the stable
    /// ConnectSrc ghost and the commit, so they always agree.
    pub(crate) fn resolve_connect_run(&mut self) -> Option<StairRun> {
        let from_id = self.connect_from?;
        let from_plat = self.platform_by_id(from_id)?;
        let from_edge = self.connect_edge?;
        let target = self.connect_to?;

        // Attach point slides along the (frozen) source edge — driven by the wheel
        // (`connect_slide_wt`), not the aim, so it never twitches with the camera.
        let edge_len = from_plat.edge_length(from_edge);
        let offset = if edge_len > 0.0 {
            (self.connect_slide_wt / edge_len).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let (fx, fz) = from_plat.edge_point_at_offset(from_edge, offset);
        let from_pt = Vec3::new(fx, from_plat.y, fz);

        let (to_platform_id, anchor_to, to_pt): (Option<u32>, Anchor, Vec3) = match target {
            ConnectTarget::Platform { id, edge } => {
                let to_plat = self.platform_by_id(id)?;
                // Align the destination anchor to the slid source point.
                let toff = structures::offset_along_edge(&to_plat, edge, from_pt.x, from_pt.z);
                let (tx, tz) = to_plat.edge_point_at_offset(edge, toff);
                (
                    Some(id),
                    Anchor::Edge { edge, offset: toff },
                    Vec3::new(tx, to_plat.y, tz),
                )
            }
            ConnectTarget::Ground { x, y, z } => {
                (None, Anchor::Ground { x, y, z }, Vec3::new(x, y, z))
            }
        };

        if (to_pt.x - from_pt.x).abs() < 1.0 && (to_pt.z - from_pt.z).abs() < 1.0 {
            return None;
        }
        if (to_pt.y - from_pt.y).abs() == 0.0 {
            return None;
        }

        Some(StairRun {
            id: 0,
            from_platform: Some(from_id),
            to_platform: to_platform_id,
            anchor_from: Anchor::Edge { edge: from_edge, offset },
            anchor_to,
            width: STAIR_WIDTH,
            step_height: STAIR_STEP_HEIGHT,
            rise_over_run: STAIR_RISE_OVER_RUN,
            grounded: false,
            railings: false,
            style: StairStyle::Platform,
        })
    }

    /// Whether the connect tool is in its slide step (so the app routes the scroll
    /// wheel to the attach-point slide instead of platform sizing).
    pub fn is_connect_sliding(&self) -> bool {
        self.platform_phase == Some(PlatformPhase::ConnectSrc)
    }

    /// Slide the attach point along the frozen source edge by `steps` WT (scroll
    /// wheel during `ConnectSrc`), clamped to the edge length.
    pub fn adjust_connect_slide(&mut self, steps: f32) {
        if self.platform_phase != Some(PlatformPhase::ConnectSrc) {
            return;
        }
        let edge_len = self
            .connect_from
            .and_then(|id| self.platform_by_id(id))
            .zip(self.connect_edge)
            .map(|(p, e)| p.edge_length(e))
            .unwrap_or(0.0);
        self.connect_slide_wt = (self.connect_slide_wt + steps).clamp(0.0, edge_len);
    }

    /// Simple-stair key (`K`): arm a two-click free stair-run between any two
    /// surface points (JS `simple_stairs`). Available from Idle or Selected.
    ///
    /// Both endpoints are aimed with the crosshair; while aiming, the scroll wheel
    /// slides the flight sideways and Shift+scroll sets its width, and a yellow
    /// x-ray ghost (marker cubes + the flight itself) shows exactly what a second
    /// click will build. Runs from this tool are
    /// [`StairStyle::Block`](structures::StairStyle::Block) — solid block stairs in
    /// the room's own theme — and `F` afterwards grounds one into a single mass.
    pub fn simple_stair_key(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        // Already aiming → toggle off, like every other tool key.
        if self.is_simple_stair() {
            self.cancel_simple_stair();
            return;
        }
        // Arm from wherever we are. The phase machine belongs to the platform tool,
        // but block stairs is not a platform sub-verb — it takes two crosshair
        // points and needs neither a platform nor a selection — so a cold `K` turns
        // that machine on itself, evicting the mutually-exclusive modal tools
        // exactly as `platform_tool_key` does. Before this, a cold `K` fell through
        // an `Idle | Selected` guard and did nothing at all, which is why the tool
        // appeared to work only after `T`.
        self.simple_self_armed = self.platform_phase.is_none();
        if self.simple_self_armed {
            self.opening_tool = None;
            self.opening_preview = None;
            self.place_tool = None;
            self.clear_draw_state();
            self.selected = None;
            self.platform_size_x = PLATFORM_SIZE;
            self.platform_size_z = PLATFORM_SIZE;
        }
        self.simple_from = None;
        self.simple_ghost = None;
        self.simple_offset_wt = 0.0;
        self.simple_width_wt = STAIR_WIDTH;
        self.selected_platform = None;
        self.selected_run = None;
        self.platform_phase = Some(PlatformPhase::SimpleFrom);
        log::info!("simple stair: click the bottom endpoint (scroll = slide, Shift+scroll = width)");
    }

    /// Back out of a simple-stair, returning to whatever the tool interrupted: the
    /// idle platform tool if that was already up, or nothing at all if `K` armed the
    /// phase machine itself. Shared by the `K` toggle and `Esc`.
    pub(crate) fn cancel_simple_stair(&mut self) {
        self.simple_from = None;
        self.simple_ghost = None;
        if self.simple_self_armed {
            self.simple_self_armed = false;
            self.clear_platform_state();
        } else {
            self.platform_phase = Some(PlatformPhase::Idle);
        }
    }

    /// Whether the simple-stair tool is aiming an endpoint, so the app routes the
    /// scroll wheel to its slide/width instead of platform footprint sizing.
    pub fn is_simple_stair(&self) -> bool {
        matches!(
            self.platform_phase,
            Some(PlatformPhase::SimpleFrom) | Some(PlatformPhase::SimpleTo)
        )
    }

    /// Scroll during a simple-stair: `du` (plain wheel) slides the flight sideways
    /// in 1-WT steps perpendicular to its run; `dv` (Shift+wheel) sets its width.
    ///
    /// The slide is what lets a flight be nudged off the exact line through the two
    /// clicked points — clicking a tread-accurate start and end is fiddly, and
    /// re-picking both endpoints to move a flight one tile sideways is worse.
    pub fn adjust_simple_stair(&mut self, du: f32, dv: f32) {
        if !self.is_simple_stair() {
            return;
        }
        if du != 0.0 {
            self.simple_offset_wt = (self.simple_offset_wt + du).clamp(-64.0, 64.0);
        }
        if dv != 0.0 {
            self.simple_width_wt = (self.simple_width_wt + dv)
                .clamp(SIMPLE_STAIR_WIDTH_MIN, SIMPLE_STAIR_WIDTH_MAX);
        }
        log::info!(
            "simple stair: width {} WT, slide {:+} WT",
            self.simple_width_wt,
            self.simple_offset_wt
        );
    }

    /// The stair-run a pair of endpoints would build — the single source of truth
    /// for both the ghost and the committed run, so the preview cannot disagree with
    /// the result.
    ///
    /// `None` for a degenerate flight: no rise (nothing to climb) or no horizontal
    /// distance (a vertical shaft, not stairs). The scroll-wheel slide is folded into
    /// the anchors here rather than carried on the run, so what lands in
    /// `stair_runs` is an ordinary flight between two ground points.
    pub(crate) fn resolve_simple_run(&self, from: Vec3, to: Vec3) -> Option<StairRun> {
        if (to.y - from.y).abs() == 0.0 {
            return None;
        }
        if (to.x - from.x).abs() < 1.0 && (to.z - from.z).abs() < 1.0 {
            return None;
        }
        let along_x = structures::free_run_is_x([from.x, from.y, from.z], [to.x, to.y, to.z]);
        let off = self.simple_offset_wt;
        let (ox, oz) = if along_x { (0.0, off) } else { (off, 0.0) };
        Some(StairRun {
            id: 0,
            from_platform: None,
            to_platform: None,
            anchor_from: Anchor::Ground {
                x: from.x + ox,
                y: from.y,
                z: from.z + oz,
            },
            anchor_to: Anchor::Ground {
                x: to.x + ox,
                y: to.y,
                z: to.z + oz,
            },
            width: self.simple_width_wt,
            step_height: STAIR_STEP_HEIGHT,
            rise_over_run: STAIR_RISE_OVER_RUN,
            grounded: false,
            railings: false,
            style: StairStyle::Block,
        })
    }

    /// The simple-stair tool's yellow x-ray preview: a floating marker cube at each
    /// endpoint, plus a ghost of the flight itself once both are known.
    ///
    /// Drawn through the stair-ghost channel (yellow, depth-test off) rather than the
    /// face-highlight channel, so the markers and the flight read as hovering in the
    /// room instead of being clipped by the geometry they sit against.
    pub(crate) fn simple_stair_ghost_mesh(&mut self) -> Option<CpuMesh> {
        let phase = self.platform_phase?;
        if !matches!(phase, PlatformPhase::SimpleFrom | PlatformPhase::SimpleTo) {
            return None;
        }
        let cursor = self
            .pick_structure_hit()
            .map(|h| Vec3::new(h.hit_wt.x.round(), h.hit_wt.y.round(), h.hit_wt.z.round()));
        let mut boxes: Vec<[f32; 6]> = Vec::new();
        if phase == PlatformPhase::SimpleFrom {
            boxes.push(endpoint_marker_box(cursor?));
        } else {
            let from = self.simple_from?;
            match cursor.and_then(|to| self.resolve_simple_run(from, to)) {
                // A valid flight: mark the run's *resolved* ends, so the markers stay
                // on the ghost as the wheel slides it, and show the steps themselves.
                Some(run) => {
                    let brushes = self.all_region_brushes();
                    for a in [&run.anchor_from, &run.anchor_to] {
                        let p = structures::resolve_run_anchor(None, a);
                        boxes.push(endpoint_marker_box(Vec3::new(p[0], p[1], p[2])));
                    }
                    boxes.extend(structures::stair_run_boxes(&run, None, None, &brushes));
                }
                // Degenerate (or nothing under the crosshair): the locked first
                // endpoint still shows, so the tool never looks dead.
                None => {
                    boxes.push(endpoint_marker_box(from));
                    if let Some(to) = cursor {
                        boxes.push(endpoint_marker_box(to));
                    }
                }
            }
        }
        Some(boxes_mesh(&boxes))
    }

    pub(crate) fn simple_stair_first_click(&mut self) {
        if let Some(hit) = self.pick_structure_hit() {
            self.simple_from = Some(Vec3::new(
                hit.hit_wt.x.round(),
                hit.hit_wt.y.round(),
                hit.hit_wt.z.round(),
            ));
            self.platform_phase = Some(PlatformPhase::SimpleTo);
            log::info!("simple stair: click the other endpoint (Esc cancels)");
        }
    }

    pub(crate) fn simple_stair_second_click(&mut self) -> Option<RegionMesh> {
        let from = self.simple_from?;
        let hit = self.pick_structure_hit()?;
        let to = Vec3::new(hit.hit_wt.x.round(), hit.hit_wt.y.round(), hit.hit_wt.z.round());
        self.simple_from = None;
        self.simple_ghost = None;
        self.platform_phase = Some(PlatformPhase::Idle);

        let Some(mut run) = self.resolve_simple_run(from, to) else {
            if (to.y - from.y).abs() == 0.0 {
                log::info!("simple stair: endpoints at the same height");
            } else {
                log::info!("simple stair: need horizontal distance");
            }
            return None;
        };
        run.id = self.next_run_id;
        self.next_run_id += 1;
        let id = run.id;
        self.stair_runs.push(run);
        // Leave the new flight *selected*, the way placing a platform does — so `F`
        // (ground it into one block), `V` and `X` act on what was just built instead
        // of needing it clicked again first.
        self.selected_run = Some(id);
        self.selected_platform = None;
        self.platform_phase = Some(PlatformPhase::Selected);
        log::info!(
            "simple stair-run {id} created — block style, {} WT wide, selected (F grounds it)",
            run.width
        );
        Some(self.rebuild_structures())
    }

    /// Grounded key (`F`): toggle `grounded` on the selected platform (and its
    /// connected stair-runs) or the selected stair-run (JS `toggle_grounded`).
    pub fn toggle_grounded_key(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform {
            let g = {
                let p = self.platforms.iter_mut().find(|p| p.id == pid)?;
                p.grounded = !p.grounded;
                p.grounded
            };
            for r in self
                .stair_runs
                .iter_mut()
                .filter(|r| r.from_platform == Some(pid) || r.to_platform == Some(pid))
            {
                r.grounded = g;
            }
            log::info!("platform {pid} grounded={g}");
            return Some(self.rebuild_structures());
        }
        if let Some(rid) = self.selected_run {
            let r = self.stair_runs.iter_mut().find(|r| r.id == rid)?;
            r.grounded = !r.grounded;
            log::info!("stair-run {rid} grounded={}", r.grounded);
            return Some(self.rebuild_structures());
        }
        None
    }

    /// Railings key (`V`): toggle `railings` on the selected platform (and its
    /// connected stair-runs) or the selected stair-run (JS `toggle_railings`).
    pub fn toggle_railings_key(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform {
            let on = {
                let p = self.platforms.iter_mut().find(|p| p.id == pid)?;
                p.railings = !p.railings;
                p.railings
            };
            for r in self
                .stair_runs
                .iter_mut()
                .filter(|r| r.from_platform == Some(pid) || r.to_platform == Some(pid))
            {
                r.railings = on;
            }
            log::info!("platform {pid} railings={on}");
            return Some(self.rebuild_structures());
        }
        if let Some(rid) = self.selected_run {
            let r = self.stair_runs.iter_mut().find(|r| r.id == rid)?;
            r.railings = !r.railings;
            log::info!("stair-run {rid} railings={}", r.railings);
            return Some(self.rebuild_structures());
        }
        None
    }

    /// Delete key (`X`/Delete): remove the selected platform (and every stair-run
    /// attached to it) or the selected stair-run (JS `delete`).
    pub fn delete_selected(&mut self) -> Option<RegionMesh> {
        if self.platform_phase != Some(PlatformPhase::Selected) {
            return None;
        }
        if let Some(pid) = self.selected_platform.take() {
            self.stair_runs
                .retain(|r| r.from_platform != Some(pid) && r.to_platform != Some(pid));
            self.platforms.retain(|p| p.id != pid);
            log::info!("platform {pid} deleted");
        } else if let Some(rid) = self.selected_run.take() {
            self.stair_runs.retain(|r| r.id != rid);
            log::info!("stair-run {rid} deleted");
        } else {
            return None;
        }
        self.platform_phase = Some(PlatformPhase::Idle);
        Some(self.rebuild_structures())
    }

    // ─── Structures geometry / nav (shared by the tool + the bake) ───────

    /// Every region brush (all ops), for grounded floor-lookup + railing wall
    /// probes (the helpers filter to subtracts themselves).
    pub(crate) fn all_region_brushes(&self) -> Vec<Brush> {
        self.regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied())
            .collect()
    }

    pub(crate) fn platform_by_id(&self, id: u32) -> Option<Platform> {
        self.platforms.iter().find(|p| p.id == id).copied()
    }

    /// The two platforms a stair-run connects (each `None` for a ground end).
    pub(crate) fn run_platforms(&self, run: &StairRun) -> (Option<Platform>, Option<Platform>) {
        (
            run.from_platform.and_then(|id| self.platform_by_id(id)),
            run.to_platform.and_then(|id| self.platform_by_id(id)),
        )
    }

    /// The solid WT boxes of every platform + stair-run — the single source that
    /// drives render, collision, and nav (so they can't drift). Grounded elements
    /// resolve their underside via `findFloorYAt` over the region brushes.
    pub(crate) fn structure_solid_boxes(&self) -> Vec<[f32; 6]> {
        let brushes = self.all_region_brushes();
        let mut boxes = Vec::new();
        for p in &self.platforms {
            if let Some(b) = p.solid_box(&brushes) {
                boxes.push(b);
            }
        }
        boxes.extend(self.stair_run_solid_boxes());
        boxes
    }

    /// Just the free-standing stair-runs' step blocks (WT).
    ///
    /// Split out from [`Self::structure_solid_boxes`] because the nav bake needs to know
    /// which of those boxes are **stairs**: inside stair geometry the grid allows a taller
    /// step, which is what lets hunters use a flight whose treads came out shallower than
    /// a nav cell (see `nav::STAIR_STEP`). A platform slab must not get that relaxation,
    /// so "all the structure solids" is not a usable answer.
    pub(crate) fn stair_run_solid_boxes(&self) -> Vec<[f32; 6]> {
        let brushes = self.all_region_brushes();
        let mut boxes = Vec::new();
        for r in &self.stair_runs {
            let (fp, tp) = self.run_platforms(r);
            boxes.extend(structures::stair_run_boxes(r, fp.as_ref(), tp.as_ref(), &brushes));
        }
        boxes
    }

    /// Re-derive the structures mesh + collider from the current platforms /
    /// stair-runs and return it for GPU upload (under [`STRUCT_ID`]). **Collider +
    /// nav use the solid boxes** (`structure_solid_boxes`, matching JS nav
    /// semantics: grounded = solid to floor). **Render uses the simple floating
    /// shell** (top + skirt + grounded pillar legs; stair treads + stringers) plus
    /// the cosmetic railings — thin planes that never enter the collider.
    pub(crate) fn rebuild_structures(&mut self) -> RegionMesh {
        let brushes = self.all_region_brushes();

        // ── Render mesh.
        // Platform-style structures always wear the "simple" (blue) scheme regardless
        // of the room's scheme — matching JS `PLATFORM_STYLES.simple.schemeName`. The
        // slabs/treads emit with the JS per-face zones + UVs (top → floor, verticals →
        // wall); railings carry their own railing zone. **Block-style** stair-runs are
        // the exception: they wear the scheme of the room they stand in.
        let mut b = ZonedBuilder::new();
        for p in &self.platforms {
            structures::append_platform_mesh(p, &brushes, &mut b, simple_scheme());
        }
        // A block-style run also hands back its lateral walls, which go into the
        // collider below so the block is solid to walk into from the side.
        let mut block_solid = ZonedBuilder::new();
        for r in &self.stair_runs {
            let (fp, tp) = self.run_platforms(r);
            match r.style {
                StairStyle::Platform => structures::append_stair_mesh(
                    r,
                    fp.as_ref(),
                    tp.as_ref(),
                    &brushes,
                    &mut b,
                    simple_scheme(),
                ),
                StairStyle::Block => structures::append_stair_block_mesh(
                    r,
                    fp.as_ref(),
                    tp.as_ref(),
                    &brushes,
                    &mut b,
                    &mut block_solid,
                    self.run_room_scheme(r, &brushes),
                ),
            }
        }
        self.append_railings(&brushes, &mut b);

        // ── Collider = platform slabs as solid boxes, each stair-run as a smooth
        // sloped RAMP (so the player walks the true slope, no per-riser auto-step
        // pop), PLUS the railings and the block-stair side walls. Railings are thin
        // cosmetic planes (never solid boxes), so they'd otherwise let the player walk
        // straight off a platform/stair edge; folding the exact same railing geometry
        // into the collision trimesh gives them real collision. Nav still uses the
        // stepped boxes from `structure_solid_boxes` (enemies don't touch this collider
        // — they're kept on-surface by the grid-nav edge check in `move_toward`).
        let mut platform_boxes = Vec::new();
        for p in &self.platforms {
            if let Some(bx) = p.solid_box(&brushes) {
                platform_boxes.push(bx);
            }
        }
        let mut ramps = Vec::new();
        for r in &self.stair_runs {
            let (fp, tp) = self.run_platforms(r);
            if let Some(q) = structures::stair_run_ramp(r, fp.as_ref(), tp.as_ref(), &brushes) {
                ramps.push(q);
            }
        }
        let mut collider = structure_collider_mesh(&platform_boxes, &ramps);
        let mut rail = ZonedBuilder::new();
        self.append_railings(&brushes, &mut rail);
        append_textured_collision(&mut collider, &rail.finish());
        append_textured_collision(&mut collider, &block_solid.finish());
        self.physics.set_region_collider(STRUCT_ID, &collider);

        RegionMesh {
            id: STRUCT_ID,
            mesh: b.finish(),
        }
    }

    /// The texture scheme a block-style stair-run wears: the theme of the room its
    /// **bottom** end stands in, so the flight continues that room's floor and wall
    /// textures. Falls back to the level default when the flight's foot is not inside
    /// any room (a run out over open ground).
    fn run_room_scheme(&self, run: &StairRun, brushes: &[Brush]) -> usize {
        let (fp, tp) = self.run_platforms(run);
        let a = structures::resolve_run_anchor(fp.as_ref(), &run.anchor_from);
        let c = structures::resolve_run_anchor(tp.as_ref(), &run.anchor_to);
        let low = if a[1] <= c[1] { a } else { c };
        structures::scheme_at(low[0], low[2], low[1], brushes).unwrap_or_else(default_scheme)
    }

    /// Emit every enabled railing (platforms + connected stair-runs) into `b`,
    /// tagged with the railing zone (→ the transparent `railing` texture) and its
    /// tile-fitted UVs. Shared by the render mesh and the collider so the railing
    /// the player sees is the exact surface they collide with.
    fn append_railings(&self, brushes: &[Brush], b: &mut ZonedBuilder) {
        for p in &self.platforms {
            if p.railings {
                structures::append_platform_railings(
                    p,
                    &self.stair_runs,
                    &self.platforms,
                    brushes,
                    b,
                    simple_scheme(),
                    RAILING_ZONE,
                );
            }
        }
        for r in &self.stair_runs {
            if r.railings {
                let (fp, tp) = self.run_platforms(r);
                structures::append_stair_railings(
                    r,
                    fp.as_ref(),
                    tp.as_ref(),
                    brushes,
                    b,
                    simple_scheme(),
                    RAILING_ZONE,
                );
            }
        }
    }

    /// Raycast the crosshair against the collision world (regions + structures)
    /// and classify: WT hit point, dominant surface axis, and which platform /
    /// stair-run (if any) that point lies inside.
    pub(crate) fn pick_structure_hit(&mut self) -> Option<StructureHit> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let hit = self.physics.raycast(origin, dir, 100.0)?;
        let axis = Axis::dominant(hit.normal);
        let hit_wt = hit.point / WORLD_SCALE;

        let brushes = self.all_region_brushes();
        const EPS: f32 = 0.25;
        let platform = self
            .platforms
            .iter()
            .find(|p| {
                p.solid_box(&brushes)
                    .map(|b| engine::geometry::geom::point_in_box_eps(&b, hit_wt, EPS))
                    .unwrap_or(false)
            })
            .map(|p| p.id);
        let run = if platform.is_some() {
            None
        } else {
            self.stair_runs
                .iter()
                .find(|r| {
                    let (fp, tp) = self.run_platforms(r);
                    structures::stair_run_boxes(r, fp.as_ref(), tp.as_ref(), &brushes)
                        .iter()
                        .any(|b| engine::geometry::geom::point_in_box_eps(b, hit_wt, EPS))
                })
                .map(|r| r.id)
        };
        Some(StructureHit {
            hit_wt,
            axis,
            platform,
            run,
        })
    }
}

/// A floating 1-WT marker cube for a simple-stair endpoint (WT `[x,y,z,w,h,d]`).
///
/// Lifted half a tile clear of the point it marks: a cube *centred* on a floor point
/// half-sinks into the floor, and one resting exactly on it shares a plane with the
/// floor — either way the marker stops reading as a marker. Drawn through the yellow
/// x-ray channel, so it stays visible even where the room geometry is in front of it.
fn endpoint_marker_box(p: Vec3) -> [f32; 6] {
    [p.x - 0.5, p.y + 0.5, p.z - 0.5, 1.0, 1.0, 1.0]
}
