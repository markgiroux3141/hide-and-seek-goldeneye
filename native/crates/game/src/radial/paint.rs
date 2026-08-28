//! Drawing the ring.
//!
//! egui rather than the in-world bitmap HUD: the HUD is a font-atlas quad builder
//! that silently drops unatlased glyphs, and this needs proportional text at two
//! sizes plus dimmed fills. egui is already in the render path and already themed
//! gold-on-black for the shop, so the ring reads as the same product.
//!
//! We paint into a raw foreground layer and do our own hit-testing off the virtual
//! pointer — no egui widgets, no `Response`. That is what keeps the menu behaving
//! identically whether the OS cursor is locked (held, flick-driven) or free
//! (sticky, cursor-driven).
//!
//! Chips on a ring rather than pie wedges: an annular sector is not convex, so
//! egui's convex-polygon fill would render it wrong, and horizontal text in a chip
//! is far more readable than text rotated around a circle.

use super::{RadialView, INNER_R, RING_R};
use crate::app::{SHOP_DIM, SHOP_GOLD, SHOP_GOLD_DIM, SHOP_TEXT};

/// Backdrop dimming disc, reaching a little past the chips.
const BACKDROP_R: f32 = RING_R + 78.0;

pub fn draw(ctx: &egui::Context, view: &RadialView) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("radial_menu"),
    ));
    let o = egui::pos2(view.origin.0, view.origin.1);

    // Backdrop: enough to lift the ring off a bright textured wall, not enough to
    // hide what you are aiming at (the menu is short-lived and the world behind it
    // is the thing you are about to edit).
    painter.circle_filled(o, BACKDROP_R, egui::Color32::from_black_alpha(150));
    painter.circle_stroke(
        o,
        RING_R,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 122, 60, 40)),
    );

    // The flick indicator: a line out to the virtual pointer. In held mode this is
    // the only feedback that the mouse is doing anything at all, since the real
    // cursor is hidden and the camera is frozen.
    let p = egui::pos2(o.x + view.ptr.0, o.y + view.ptr.1);
    let r = (view.ptr.0 * view.ptr.0 + view.ptr.1 * view.ptr.1).sqrt();
    if r > 4.0 {
        painter.line_segment([o, p], egui::Stroke::new(1.5, SHOP_GOLD_DIM));
        painter.circle_filled(p, 3.5, SHOP_GOLD);
    }

    // Dead zone: cancel at the root, back-up inside a submenu.
    painter.circle_filled(o, INNER_R, egui::Color32::from_rgba_unmultiplied(8, 8, 10, 230));
    let hub_hot = r < INNER_R;
    painter.circle_stroke(
        o,
        INNER_R,
        egui::Stroke::new(
            1.0,
            if hub_hot && view.can_back {
                SHOP_GOLD
            } else {
                SHOP_GOLD_DIM
            },
        ),
    );
    painter.text(
        o - egui::vec2(0.0, 6.0),
        egui::Align2::CENTER_CENTER,
        view.title,
        egui::FontId::proportional(13.0),
        SHOP_GOLD,
    );
    painter.text(
        o + egui::vec2(0.0, 9.0),
        egui::Align2::CENTER_CENTER,
        if view.can_back { "back" } else { "cancel" },
        egui::FontId::proportional(9.0),
        SHOP_DIM,
    );

    // Chips.
    let n = view.slots.len();
    for (i, slot) in view.slots.iter().enumerate() {
        let (dx, dy) = super::slot_dir(i, n);
        let c = egui::pos2(o.x + dx * RING_R, o.y + dy * RING_R);
        let hot = view.hovered == Some(i);

        let (fill, border, text_col) = match (slot.enabled, hot, slot.on) {
            (false, _, _) => (
                egui::Color32::from_rgba_unmultiplied(14, 14, 16, 200),
                egui::Color32::from_rgb(48, 48, 52),
                SHOP_DIM,
            ),
            (true, true, _) => (SHOP_GOLD, SHOP_GOLD, egui::Color32::BLACK),
            (true, false, true) => (
                egui::Color32::from_rgba_unmultiplied(40, 32, 12, 235),
                SHOP_GOLD,
                SHOP_GOLD,
            ),
            (true, false, false) => (
                egui::Color32::from_rgba_unmultiplied(12, 12, 14, 235),
                SHOP_GOLD_DIM,
                SHOP_TEXT,
            ),
        };

        // A submenu says so, so "this opens another ring" is never a surprise.
        let label = if slot.is_menu() {
            format!("{} \u{25b8}", slot.label)
        } else {
            slot.label.clone()
        };
        let main = painter.layout_no_wrap(label, egui::FontId::proportional(14.0), text_col);
        // The hotkey, printed under every entry. This is the teaching half of the
        // whole feature: you reach for the menu, and it hands you back the key.
        let hint = (!slot.hint.is_empty()).then(|| {
            let col = if hot {
                egui::Color32::from_rgb(60, 45, 10)
            } else {
                SHOP_DIM
            };
            painter.layout_no_wrap(slot.hint.to_string(), egui::FontId::monospace(10.0), col)
        });

        let hint_h = hint.as_ref().map(|g| g.size().y + 1.0).unwrap_or(0.0);
        let w = main.size().x.max(hint.as_ref().map(|g| g.size().x).unwrap_or(0.0)) + 18.0;
        let h = main.size().y + hint_h + 10.0;
        let rect = egui::Rect::from_center_size(c, egui::vec2(w, h));
        painter.rect_filled(rect, 5.0, fill);
        painter.rect_stroke(
            rect,
            5.0,
            egui::Stroke::new(if hot { 1.5 } else { 1.0 }, border),
            egui::StrokeKind::Inside,
        );

        let top = rect.top() + 5.0;
        painter.galley(
            egui::pos2(c.x - main.size().x * 0.5, top),
            main.clone(),
            text_col,
        );
        if let Some(g) = hint {
            painter.galley(
                egui::pos2(c.x - g.size().x * 0.5, top + main.size().y + 1.0),
                g,
                SHOP_DIM,
            );
        }
    }

    // Only worth saying while sticky — in held mode the gesture explains itself.
    if view.sticky {
        painter.text(
            egui::pos2(o.x, o.y + BACKDROP_R - 14.0),
            egui::Align2::CENTER_CENTER,
            "click to pick   \u{2022}   Esc to close",
            egui::FontId::proportional(11.0),
            SHOP_DIM,
        );
    }
}
