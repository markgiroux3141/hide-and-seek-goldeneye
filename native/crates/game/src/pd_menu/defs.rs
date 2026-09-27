//! Dialogs the Combat Simulator reaches that live outside this spike: the
//! Controller Pak file manager (`filemgr.c`) and the solo-mission menus. Each
//! is a one-screen danger dialog in PD's own dialog machinery, so the flow
//! around it (open → B / "OK" → back) behaves like a real dialog.

use super::types::*;
use super::Pd;

fn pak_text(_pd: &mut Pd, _item: &'static MenuItem) -> String {
    "No Controller Pak in this spike.\n".into()
}

fn not_in_spike_text(_pd: &mut Pd, _item: &'static MenuItem) -> String {
    "Not part of this spike.\n".into()
}

fn ok_text(_pd: &mut Pd, _item: &'static MenuItem) -> String {
    "OK\n".into()
}

fn pak_title(_pd: &mut Pd, _def: &'static MenuDialogDef) -> String {
    "Controller Pak\n".into()
}

fn spike_title(_pd: &mut Pd, _def: &'static MenuDialogDef) -> String {
    "Perfect Dark Spike\n".into()
}

pub static STUB_PAK_ITEMS: [MenuItem; 3] = [
    MenuItem { ty: MENUITEMTYPE_LABEL, param: 0, flags: MENUITEMFLAG_LESSLEFTPADDING, param2: P::Fn(pak_text), param3: P::Num(0), handler: H::None },
    MenuItem { ty: MENUITEMTYPE_SELECTABLE, param: 0, flags: MENUITEMFLAG_SELECTABLE_CLOSESDIALOG, param2: P::Fn(ok_text), param3: P::Num(0), handler: H::None },
    MenuItem::END,
];

pub static STUB_PAK_DIALOG: MenuDialogDef = MenuDialogDef {
    name: "stub_pak",
    ty: MENUDIALOGTYPE_DANGER,
    title: P::DFn(pak_title),
    items: &STUB_PAK_ITEMS,
    handler: None,
    flags: 0,
    nextsibling: None,
};

pub static STUB_NOT_IN_SPIKE_ITEMS: [MenuItem; 3] = [
    MenuItem { ty: MENUITEMTYPE_LABEL, param: 0, flags: MENUITEMFLAG_LESSLEFTPADDING, param2: P::Fn(not_in_spike_text), param3: P::Num(0), handler: H::None },
    MenuItem { ty: MENUITEMTYPE_SELECTABLE, param: 0, flags: MENUITEMFLAG_SELECTABLE_CLOSESDIALOG, param2: P::Fn(ok_text), param3: P::Num(0), handler: H::None },
    MenuItem::END,
];

pub static STUB_NOT_IN_SPIKE_DIALOG: MenuDialogDef = MenuDialogDef {
    name: "stub_not_in_spike",
    ty: MENUDIALOGTYPE_DANGER,
    title: P::DFn(spike_title),
    items: &STUB_NOT_IN_SPIKE_ITEMS,
    handler: None,
    flags: 0,
    nextsibling: None,
};
