//! Radial-menu unit tests.
//!
//! The whole state machine is deliberately free of window, world and egui, so the
//! gestures that are awkward to check by hand — descend, back out, the tap that
//! becomes a sticky menu — are ordinary tests rather than playtest notes.

use super::*;

/// A context with a couple of texture bindings and slots 1 + 3 occupied.
fn ctx() -> RadialCtx {
    RadialCtx {
        wave: 6,
        hunters: true,
        real_lighting: true,
        schemes: vec![
            ('1', "Facility tile".into(), 4),
            ('2', "Bunker steel".into(), 9),
        ],
        slots: [true, false, true, false, false, false, false, false],
        ..RadialCtx::default()
    }
}

/// Point `r` points along slot `i` of `n`.
fn at(i: usize, n: usize, r: f32) -> (f32, f32) {
    let (dx, dy) = slot_dir(i, n);
    (dx * r, dy * r)
}

#[test]
fn slot_zero_is_north_and_runs_clockwise() {
    // Straight up.
    assert_eq!(slot_at((0.0, -100.0), 8), Some(0));
    // Screen +x is right, so a quarter turn clockwise from north is slot 2 of 8.
    assert_eq!(slot_at((100.0, 0.0), 8), Some(2));
    // Straight down.
    assert_eq!(slot_at((0.0, 100.0), 8), Some(4));
    // And a quarter turn anticlockwise wraps to 6, not to -2.
    assert_eq!(slot_at((-100.0, 0.0), 8), Some(6));
}

#[test]
fn dead_zone_selects_nothing() {
    assert_eq!(slot_at((0.0, 0.0), 8), None);
    assert_eq!(slot_at((0.0, -(INNER_R - 1.0)), 8), None);
    assert_eq!(slot_at((0.0, -(INNER_R + 1.0)), 8), Some(0));
}

#[test]
fn every_slot_round_trips_through_its_own_direction() {
    for n in 2..=10 {
        for i in 0..n {
            assert_eq!(slot_at(at(i, n, RING_R), n), Some(i), "n={n} i={i}");
        }
    }
}

#[test]
fn flick_and_release_commits_the_hovered_action() {
    let mut r = Radial::default();
    r.press((640.0, 360.0), true);
    assert!(r.is_held());
    // South on the root ring is "Enter HUNT".
    r.ptr = at(4, 8, RING_R);
    let (action, lock) = r.release(&ctx());
    assert_eq!(action, Some(EditorAction::EnterHunt));
    assert_eq!(lock, LockRequest::Restore);
    assert!(!r.is_open());
}

#[test]
fn releasing_in_the_dead_zone_cancels() {
    let mut r = Radial::default();
    r.press((640.0, 360.0), true);
    // Held long enough that this is a deliberate cancel, not a tap.
    r.update(TAP_SECS * 2.0, &ctx());
    let (action, _) = r.release(&ctx());
    assert_eq!(action, None);
    assert!(!r.is_open());
}

#[test]
fn a_quick_tap_leaves_a_sticky_menu_and_frees_the_cursor() {
    let mut r = Radial::default();
    r.press((640.0, 360.0), true);
    r.update(TAP_SECS * 0.5, &ctx());
    let (action, lock) = r.release(&ctx());
    assert_eq!(action, None);
    assert_eq!(lock, LockRequest::Free);
    assert!(r.is_open() && r.is_sticky() && !r.is_held());
}

#[test]
fn opening_with_a_free_cursor_starts_sticky() {
    // No pointer lock means no raw motion to integrate, so the real cursor has to
    // drive it from the start.
    let mut r = Radial::default();
    r.press((640.0, 360.0), false);
    assert!(r.is_sticky() && !r.is_held());
}

#[test]
fn pushing_past_the_expand_radius_descends_into_a_submenu() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    assert_eq!(r.menu_id(), MenuId::Root);
    // North is Tools. Nudging as far as the chips is not enough...
    r.ptr = at(0, 8, RING_R);
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Root);
    // ...pushing past the expand radius is.
    r.ptr = at(0, 8, EXPAND_R + 5.0);
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Tools);
    // The pointer re-centres so the next flick starts from the middle.
    assert_eq!(r.ptr, (0.0, 0.0));
}

#[test]
fn descending_then_flicking_commits_the_child() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    r.ptr = at(0, 8, EXPAND_R + 5.0);
    r.update(0.016, &c);
    // Slot 1 of the Tools ring is the door tool.
    r.ptr = at(1, 8, RING_R);
    let (action, _) = r.release(&c);
    assert_eq!(action, Some(EditorAction::ArmTool(Tool::Door)));
}

#[test]
fn falling_back_through_the_middle_goes_up_a_level() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    r.ptr = at(0, 8, EXPAND_R + 5.0);
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Tools);
    // The re-centred pointer must NOT immediately read as "go back": the back
    // gesture only arms once the pointer has left the dead zone.
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Tools);
    r.ptr = at(3, 8, RING_R);
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Tools);
    r.ptr = (0.0, 0.0);
    r.update(0.016, &c);
    assert_eq!(r.menu_id(), MenuId::Root);
}

#[test]
fn releasing_on_a_submenu_opens_it_sticky_rather_than_stranding_the_ring() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    r.update(TAP_SECS * 2.0, &c);
    r.ptr = at(0, 8, RING_R); // on Tools, but never past EXPAND_R
    let (action, lock) = r.release(&c);
    assert_eq!(action, None);
    assert_eq!(r.menu_id(), MenuId::Tools);
    // Held is over, so it must hand the cursor back or nothing can drive it.
    assert!(r.is_sticky() && !r.is_held());
    assert_eq!(lock, LockRequest::Free);
}

#[test]
fn sticky_clicks_navigate_and_commit() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), false); // sticky from the start
    r.cursor(640.0, 360.0 - RING_R); // north = Tools
    let (action, _) = r.click(&c);
    assert_eq!(action, None);
    assert_eq!(r.menu_id(), MenuId::Tools);
    // Now click the north entry of the Tools ring: Draw.
    r.cursor(640.0, 360.0 - RING_R);
    let (action, lock) = r.click(&c);
    assert_eq!(action, Some(EditorAction::ArmTool(Tool::Draw)));
    assert_eq!(lock, LockRequest::Restore);
    assert!(!r.is_open());
}

#[test]
fn sticky_click_in_the_middle_goes_back_then_closes() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), false);
    r.cursor(640.0, 360.0 - RING_R);
    r.click(&c);
    assert_eq!(r.menu_id(), MenuId::Tools);
    r.cursor(640.0, 360.0);
    r.click(&c);
    assert_eq!(r.menu_id(), MenuId::Root);
    assert!(r.is_open());
    r.cursor(640.0, 360.0);
    r.click(&c);
    assert!(!r.is_open());
}

#[test]
fn a_disabled_slot_commits_nothing_and_leaves_the_ring_up() {
    let mut r = Radial::default();
    // Nothing selected → the Selection ring is dead.
    let c = RadialCtx::default();
    r.press((640.0, 360.0), true);
    r.ptr = at(1, 8, RING_R);
    let (action, _) = r.release(&c);
    assert_eq!(action, None);
    assert!(r.is_open(), "a dud click should not dismiss the menu");
}

#[test]
fn a_repeating_slot_keeps_the_ring_open() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), false);
    // Root NW = Debug.
    r.cursor(640.0 - RING_R * 0.7071, 360.0 - RING_R * 0.7071);
    r.click(&c);
    assert_eq!(r.menu_id(), MenuId::Debug);
    // Debug slot 1 of 6 is the wave-size stepper.
    let (dx, dy) = slot_dir(1, 6);
    r.cursor(640.0 + dx * RING_R, 360.0 + dy * RING_R);
    let (action, lock) = r.click(&c);
    assert_eq!(action, Some(EditorAction::WaveSize(1)));
    assert!(r.is_open(), "steppers stay up so they can be clicked again");
    assert_eq!(lock, LockRequest::None);
}

#[test]
fn the_pointer_is_clamped_so_a_hard_flick_stays_returnable() {
    let mut r = Radial::default();
    r.press((640.0, 360.0), true);
    for _ in 0..200 {
        r.motion(40.0, 0.0);
    }
    let (x, y) = r.ptr;
    assert!((x * x + y * y).sqrt() <= MAX_R + 0.01);
}

#[test]
fn motion_is_ignored_once_the_button_is_up() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    r.update(TAP_SECS * 0.5, &c);
    r.release(&c); // → sticky
    r.motion(500.0, 0.0);
    assert_eq!(r.ptr, (0.0, 0.0));
}

/// How many entries sit ahead of the eight quick slots on the LEVEL ring: the named
/// level's Save, and the way into the LEVELS panel.
const LEVEL_RING_HEAD: usize = 2;

#[test]
fn ctrl_turns_the_level_ring_from_load_into_save() {
    let mut c = ctx();
    let load = menu(MenuId::Level, &c);
    let slot = |v: &[Slot], n: usize| v[LEVEL_RING_HEAD + n - 1].target;
    assert_eq!(slot(&load, 1), Target::Act(EditorAction::LoadSlot(1)));
    // Slot 2 is empty, so there is nothing to load.
    assert!(!load[LEVEL_RING_HEAD + 1].enabled);
    c.ctrl = true;
    let save = menu(MenuId::Level, &c);
    assert_eq!(slot(&save, 1), Target::Act(EditorAction::SaveSlot(1)));
    assert!(
        save[LEVEL_RING_HEAD + 1].enabled,
        "an empty slot is a fine place to save"
    );
    assert!(save[LEVEL_RING_HEAD + 1].label.contains("SAVE"));
}

/// The named level leads the LEVEL ring, and its Save is dead until the level has a
/// file to save to — the ring must not offer a save that can only fail.
#[test]
fn the_level_ring_leads_with_the_named_level() {
    let mut c = ctx();
    let unnamed = menu(MenuId::Level, &c);
    assert_eq!(
        unnamed[0].target,
        Target::Act(EditorAction::SaveCurrentLevel)
    );
    assert!(!unnamed[0].enabled, "nothing to save back to yet");
    assert!(unnamed[0].label.contains("unnamed"));
    assert_eq!(
        unnamed[1].target,
        Target::Act(EditorAction::OpenPanel(PanelTab::Levels))
    );

    c.level = Some("Bunker Base".into());
    c.level_dirty = true;
    let named = menu(MenuId::Level, &c);
    assert!(named[0].enabled);
    assert!(named[0].label.contains("Bunker Base"));
    assert!(named[0].label.ends_with('*'), "unsaved edits are marked");
    assert!(named[0].on);

    c.level_dirty = false;
    let saved = menu(MenuId::Level, &c);
    assert!(!saved[0].label.ends_with('*'));
    assert!(!saved[0].on);
}

/// Every panel tab is reachable from the Objects ring, so a new tab can't be added
/// without the radial learning about it.
#[test]
fn the_objects_ring_covers_every_panel_tab() {
    let slots = menu(MenuId::Objects, &ctx());
    assert_eq!(slots.len(), PanelTab::ALL.len());
    for tab in PanelTab::ALL {
        assert!(
            slots
                .iter()
                .any(|s| s.target == Target::Act(EditorAction::OpenPanel(tab))),
            "{tab:?} is not on the Objects ring"
        );
    }
}

#[test]
fn the_texture_ring_is_built_from_this_level_bindings() {
    let c = ctx();
    let slots = menu(MenuId::Textures, &c);
    // Two bound digits plus the escape hatch into the panel.
    assert_eq!(slots.len(), 3);
    assert_eq!(slots[0].label, "Facility tile");
    assert_eq!(slots[0].hint, "1");
    assert_eq!(slots[0].target, Target::Act(EditorAction::SetScheme(4)));
    assert!(slots[2].is_menu() || matches!(slots[2].target, Target::Act(EditorAction::OpenPanel(_))));
}

#[test]
fn toggle_labels_carry_their_state() {
    let mut c = ctx();
    c.nav_overlay = false;
    let off = menu(MenuId::View, &c);
    assert!(off[2].label.ends_with("off"));
    assert!(!off[2].on);
    c.nav_overlay = true;
    let on = menu(MenuId::View, &c);
    assert!(on[2].label.ends_with("ON"));
    assert!(on[2].on);
}

#[test]
fn the_armed_tool_is_accented_on_every_ring_above_it() {
    let mut c = ctx();
    c.armed = Some(Tool::Draw);
    assert!(menu(MenuId::Root, &c)[0].on, "root Tools shows something is armed");
    let tools = menu(MenuId::Tools, &c);
    // Found by target, not by index: the ring's order is a presentation choice and
    // adding a tool to it should not break this assertion (it did when Vent landed).
    let slot = |t: Tool| {
        tools
            .iter()
            .find(|s| s.target == Target::Act(EditorAction::ArmTool(t)))
            .unwrap_or_else(|| panic!("{t:?} is on the Tools ring"))
    };
    assert!(slot(Tool::Draw).on, "the armed tool is accented");
    assert!(!slot(Tool::Door).on, "and the others are not");
    assert!(tools.iter().filter(|s| s.on).count() == 1, "exactly one accent");

    // A tool one ring deeper accents the whole path down to it, so a submenu never
    // hides the fact that something inside it is live.
    c.armed = Some(Tool::Platform);
    let tools = menu(MenuId::Tools, &c);
    let gate = tools
        .iter()
        .find(|s| s.target == Target::Menu(MenuId::Platform))
        .expect("the Platform ring is reachable from Tools");
    assert!(gate.on, "Tools shows the platform tool is armed");
    assert!(menu(MenuId::Root, &c)[0].on, "and so does the root");
    let inner = menu(MenuId::Platform, &c);
    assert_eq!(
        inner
            .iter()
            .filter(|s| s.on && matches!(s.target, Target::Act(EditorAction::ArmTool(_))))
            .count(),
        1,
        "exactly one armed tool on the ring it actually lives on"
    );
}

/// The platform system lives on its own ring, reached from Tools. Its three tools plus
/// a style pair is half a ring on its own, and the Tools ring has every other tool to
/// hold.
#[test]
fn the_platform_tools_live_on_their_own_ring() {
    let c = ctx();
    let tools = menu(MenuId::Tools, &c);
    assert!(
        tools
            .iter()
            .any(|s| s.target == Target::Menu(MenuId::Platform)),
        "Tools opens the Platform ring"
    );
    for t in [Tool::Platform, Tool::BlockStairs, Tool::Connect] {
        assert!(
            !tools
                .iter()
                .any(|s| s.target == Target::Act(EditorAction::ArmTool(t))),
            "{t:?} moved off the Tools ring"
        );
        assert!(
            menu(MenuId::Platform, &c)
                .iter()
                .any(|s| s.target == Target::Act(EditorAction::ArmTool(t))),
            "{t:?} is on the Platform ring"
        );
    }
}

/// The ring **reports** the live build style and opens the tab that owns it. Three
/// independent settings with a handful of values each is panel-shaped, not ring-shaped —
/// the same call the 390-theme texture library forced. But the ring still has to answer
/// "which am I building?" without you placing one to find out, so the label carries the
/// state even though the controls don't.
#[test]
fn the_platform_ring_reports_the_build_style_and_opens_the_tab_that_owns_it() {
    use engine::geometry::csg_runtime::StairShell;
    use engine::geometry::structures::PlatformStyle;

    let style = |c: &RadialCtx| {
        menu(MenuId::Platform, c)
            .into_iter()
            .find(|s| s.target == Target::Act(EditorAction::OpenPanel(PanelTab::Tools)))
            .expect("the style slot opens the TOOLS tab")
            .label
    };
    let mut c = ctx();
    assert_eq!(style(&c), "Style: slab + stairs");

    c.platform_style = PlatformStyle::Plane;
    c.stair_shell = StairShell::Ramp;
    assert_eq!(style(&c), "Style: plane + ramp");

    // Both halves move independently — they are two settings, not one switch.
    c.stair_shell = StairShell::Steps;
    assert_eq!(style(&c), "Style: plane + stairs");
}

/// Nesting is allowed but rationed. `Root → Tools → Platform` is the one path three
/// deep; everything else is a leaf ring, so the deepest thing you ever have to back out
/// of is two levels.
#[test]
fn nesting_stops_at_the_platform_ring() {
    let c = ctx();
    for id in [
        MenuId::Platform,
        MenuId::Selection,
        MenuId::Objects,
        MenuId::Textures,
        MenuId::Level,
        MenuId::View,
        MenuId::Debug,
    ] {
        for slot in menu(id, &c) {
            assert!(!slot.is_menu(), "{id:?} is a leaf ring — depth is capped at 3");
        }
    }
    assert_eq!(
        menu(MenuId::Tools, &c).iter().filter(|s| s.is_menu()).count(),
        1,
        "Tools nests exactly once, into Platform"
    );
}

#[test]
fn confirm_stairs_lights_up_only_with_a_pending_op() {
    let mut c = ctx();
    c.has_selection = true;
    // Found by label, not by index: the ring grows from the end, and a hard-coded
    // slot number turns every future addition into a spurious test failure.
    let confirm = |c: &RadialCtx| {
        menu(MenuId::Selection, c)
            .into_iter()
            .find(|s| s.label.starts_with("Confirm stairs"))
            .expect("the Selection ring has a confirm-stairs slot")
            .enabled
    };
    let slots = menu(MenuId::Selection, &c);
    assert!(slots[0].enabled, "push needs only a selection");
    assert!(!confirm(&c), "nothing pending to confirm");
    c.pending_stair = true;
    assert!(confirm(&c));
}

#[test]
fn closing_forgets_everything() {
    let mut r = Radial::default();
    let c = ctx();
    r.press((640.0, 360.0), true);
    r.ptr = at(0, 8, EXPAND_R + 5.0);
    r.update(0.016, &c);
    r.close();
    assert!(!r.is_open() && !r.is_held() && !r.is_sticky());
    assert_eq!(r.menu_id(), MenuId::Root);
    assert_eq!(r.ptr, (0.0, 0.0));
    assert!(r.view(&c).is_none());
}
