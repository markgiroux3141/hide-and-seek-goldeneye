//! Headless checks on the menu port: the generated tables are PD's, the
//! dialogs open and navigate, and random input never panics.

use super::generated as gd;
use super::mp::Profile;
use super::types::*;
use super::Pd;

fn pd() -> Pd {
    Pd::new(Profile::Complete).expect("assets: run tools/pd-assets/pd_menu_gen.py")
}

fn tap(pd: &mut Pd, bit: u16) {
    pd.joy[0].buttons |= bit;
    pd.frame(1);
    pd.joy[0].buttons &= !bit;
    for _ in 0..8 {
        pd.frame(1);
    }
}

fn cur(pd: &Pd) -> &'static str {
    pd.menus[0].curdialog.map(|d| pd.menus[0].dialogs[d].def().name).unwrap_or("-")
}

#[test]
fn tables_match_the_decomp() {
    // setup.c:5813: four big-font selectables then END.
    assert_eq!(gd::G_COMBAT_SIMULATOR_MENU_ITEMS.len(), 5);
    assert!(gd::G_COMBAT_SIMULATOR_MENU_ITEMS[..4].iter().all(|i| i.ty == MENUITEMTYPE_SELECTABLE && i.flags & MENUITEMFLAG_BIGFONT != 0));
    assert_eq!(gd::MP_ARENAS.len(), 17);
    assert_eq!(gd::MP_BODIES.len(), 61);
    assert_eq!(gd::MP_CHALLENGES.len(), 30);
    assert_eq!(gd::BOT_PROFILES.len(), 18);
    // ROM mpconfigs: 44 configs, and challenge 1's description is text.
    let pd = pd();
    assert_eq!(pd.res.mpconfigs.len(), 44);
    let c1 = &pd.res.mpconfigs[gd::MP_CHALLENGES[0].confignum as usize];
    assert!(c1.description.len() > 20, "{:?}", c1.description);
}

#[test]
fn strings_resolve() {
    let pd = pd();
    assert_eq!(pd.lang(super::lang::tx(gd::B_MISC, 445)).trim(), "Combat Simulator");
    assert_eq!(pd.lang(super::lang::tx(gd::B_MPMENU, 17)).trim(), "Game Setup");
}

#[test]
fn perfect_menu_to_advanced_setup() {
    let mut pd = pd();
    pd.open_main_menu();
    for _ in 0..40 {
        pd.frame(1);
    }
    assert_eq!(cur(&pd), "g_CiMenuViaPcMenuDialog");
    // Carrington Institute is focused first; Combat Simulator is two down.
    tap(&mut pd, D_JPAD);
    tap(&mut pd, D_JPAD);
    tap(&mut pd, A_BUTTON);
    for _ in 0..40 {
        pd.frame(1);
    }
    assert_eq!(cur(&pd), "g_CombatSimulatorMenuDialog");
    assert_eq!(pd.menudata.root, MENUROOT_MPSETUP);
    // Advanced Setup is the fourth item.
    for _ in 0..3 {
        tap(&mut pd, D_JPAD);
    }
    tap(&mut pd, A_BUTTON);
    for _ in 0..40 {
        pd.frame(1);
    }
    assert_eq!(cur(&pd), "g_MpAdvancedSetupMenuDialog");
    assert_eq!(pd.vars.mpsetupmenu, gd::MPSETUPMENU_ADVSETUP);
    // Its sibling layer is Player Setup, Stuff, Challenges.
    let depth = pd.menus[0].depth;
    assert_eq!(pd.menus[0].layers[depth - 1].numsiblings, 4);
}

#[test]
fn add_a_simulant() {
    let mut pd = pd();
    pd.open_combat_simulator();
    for _ in 0..30 {
        pd.frame(1);
    }
    for _ in 0..3 {
        tap(&mut pd, D_JPAD);
    }
    tap(&mut pd, A_BUTTON);
    for _ in 0..30 {
        pd.frame(1);
    }
    // Simulants is the 7th entry of Game Setup.
    for _ in 0..6 {
        tap(&mut pd, D_JPAD);
    }
    tap(&mut pd, A_BUTTON);
    assert_eq!(cur(&pd), "g_MpSimulantsMenuDialog");
    tap(&mut pd, A_BUTTON); // Add Simulant...
    assert_eq!(cur(&pd), "g_MpAddSimulantMenuDialog");
    tap(&mut pd, A_BUTTON); // MeatSim
    assert_eq!(cur(&pd), "g_MpSimulantsMenuDialog");
    assert!(pd.mp.setup.chrslots & 0x10 != 0);
    assert!(pd.mp.bots[0].base.name.starts_with("MeatSim"), "{:?}", pd.mp.bots[0].base.name);
}

#[test]
fn random_input_never_panics() {
    let mut pd = pd();
    pd.open_main_menu();
    let mut rng = crate::pd_spike::pdmath::Rng::new(99);
    let bits = [A_BUTTON, B_BUTTON, U_JPAD, D_JPAD, L_JPAD, R_JPAD, START_BUTTON, Z_TRIG, R_TRIG];
    for _ in 0..6000 {
        let b = bits[(rng.random() % bits.len() as u32) as usize];
        pd.joy[0].buttons = if rng.random() % 3 == 0 { b } else { 0 };
        pd.joy[0].stick_x = ((rng.random() % 161) as i32 - 80) as i8 * (rng.random() % 4 == 0) as i8;
        pd.frame(1);
        if pd.match_started.is_some() {
            pd.return_from_match();
        }
    }
}

/// Every MP body (with its default head), every MP head on its own, and the
/// hudpiece load and draw pixels through `menu_render_model`.
#[test]
fn every_menu_model_draws() {
    let mut pd = pd();
    let draw = |pd: &mut Pd, params: u32, modeltype: i32| -> usize {
        let mut mm = super::menu::MenuModel { newparams: params, loaddelay: 1, zoom: 30.0, curscale: 1.0, newscale: 1.0, newanimnum: gd::ANIM_01FC, headnum: -1, bodynum: -1, ..Default::default() };
        if modeltype == MENUMODELTYPE_HUDPIECE {
            mm = super::menu::MenuModel { curposx: -205.5, newposx: -205.5, curposy: 244.7, newposy: 244.7, curposz: 68.3, newposz: 68.3, curscale: 0.12209, newscale: 0.12209, newroty: -std::f32::consts::PI, zoom: -1.0, newanimnum: gd::ANIM_040D, ..mm };
        }
        pd.scissor_menu = [100, 70, 220, 150];
        pd.gfx.clear([0.0; 3]);
        for _ in 0..3 {
            pd.menu_render_model(&mut mm, modeltype);
        }
        assert!(pd.model_inst[if modeltype == MENUMODELTYPE_HUDPIECE { 4 } else { 0 }].is_some(), "params {params:#x} did not load: {:?}", pd.models.error);
        pd.gfx.fb.iter().filter(|p| p[0] + p[1] + p[2] > 0.0).count()
    };
    for b in 0..gd::MP_BODIES.len() {
        let head = gd::MP_BODIES[b].headnum;
        let mphead = gd::MP_HEADS.iter().position(|h| h.headnum == head).unwrap_or(0);
        let n = draw(&mut pd, 0xffff | (mphead as u32) << 16 | (b as u32) << 24, MENUMODELTYPE_DEFAULT);
        assert!(n > 50, "body {b} drew {n} pixels");
    }
    for h in 0..gd::MP_HEADS.len() {
        let filenum = gd::HEADS_AND_BODIES[gd::MP_HEADS[h].headnum as usize].filenum as u32;
        let n = draw(&mut pd, filenum, MENUMODELTYPE_DEFAULT);
        assert!(n > 50, "head {h} drew {n} pixels");
    }
    let n = draw(&mut pd, gd::FILE_GHUDPIECE as u32, MENUMODELTYPE_HUDPIECE);
    assert!(n > 50, "hudpiece drew {n} pixels");
    // The eye feeds the holoray origin (menu.c:2287) only on the menu roots.
    let _ = pd.text.holoray_fromx;
}
