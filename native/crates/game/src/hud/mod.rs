//! Player-Combat HUD (P3+): screen-space 2D text built from a code-defined 5×7
//! bitmap [`font`], drawn through the engine's textured-screen-quad HUD pipeline
//! (the same alpha-blended overlay path the crosshair uses). The first piece is
//! the ammo counter; the health HUD (P5) will reuse the atlas + quad layout.
//!
//! Split of duties: this module is pure CPU geometry — it builds the RGBA glyph
//! atlas (uploaded once at init) and lays a string out into [`HudVertex`] quads in
//! NDC. The renderer owns the GPU pipeline; `world::combat` feeds it the ammo
//! state each frame.

pub mod font;

use engine::render::mesh::HudVertex;
use font::{cell_width, CHARSET, GLYPH_H, GLYPH_W};

/// The atlas texel dimensions: all glyph cells laid out in one horizontal strip.
pub fn atlas_size() -> (u32, u32) {
    (CHARSET.chars().count() as u32 * cell_width(), GLYPH_H)
}

/// Build the glyph-atlas RGBA8 pixels: white where a glyph pixel is set, fully
/// transparent elsewhere (including the padding column after each glyph). Uploaded
/// once via `Renderer::upload_hud_atlas`.
pub fn atlas_rgba() -> (u32, u32, Vec<u8>) {
    let (w, h) = atlas_size();
    let mut px = vec![0u8; (w * h * 4) as usize];
    for (i, c) in CHARSET.chars().enumerate() {
        let Some(rows) = font::glyph(c) else { continue };
        let cell_x = i as u32 * cell_width();
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_W {
                // Bit (GLYPH_W-1 - col) is the pixel at column `col` (bit 4 = left).
                if bits & (1 << (GLYPH_W - 1 - col)) != 0 {
                    let x = cell_x + col;
                    let y = row as u32;
                    let o = ((y * w + x) * 4) as usize;
                    px[o] = 255;
                    px[o + 1] = 255;
                    px[o + 2] = 255;
                    px[o + 3] = 255;
                }
            }
        }
    }
    (w, h, px)
}

/// The atlas cell index of a character, or `None` if it isn't in the [`CHARSET`].
pub fn cell_index(c: char) -> Option<usize> {
    CHARSET.chars().position(|x| x == c)
}

/// Append `text`'s glyph quads to `out`. Text is drawn left-to-right starting at
/// NDC `(x_start, y_top)` (its top-left), each glyph `gw`×`gh` in NDC with `gap`
/// between cells. Space (and any unsupported char that maps to a blank cell)
/// advances without emitting a quad. Un-indexed: 6 verts per glyph.
pub fn layout_text(
    text: &str,
    x_start: f32,
    y_top: f32,
    gw: f32,
    gh: f32,
    gap: f32,
    out: &mut Vec<HudVertex>,
) {
    let (atlas_w, _) = atlas_size();
    let cw = cell_width() as f32;
    let mut x = x_start;
    for c in text.chars() {
        let Some(i) = cell_index(c) else { continue };
        if c != ' ' {
            // Atlas UVs: this cell's glyph columns (excluding the trailing pad).
            let u0 = (i as f32 * cw) / atlas_w as f32;
            let u1 = (i as f32 * cw + GLYPH_W as f32) / atlas_w as f32;
            let (x0, x1) = (x, x + gw);
            let (y_b, y_t) = (y_top - gh, y_top);
            // v=0 is the glyph top (atlas row 0), so map the higher-y verts to v=0.
            let tl = HudVertex { pos: [x0, y_t], uv: [u0, 0.0] };
            let tr = HudVertex { pos: [x1, y_t], uv: [u1, 0.0] };
            let br = HudVertex { pos: [x1, y_b], uv: [u1, 1.0] };
            let bl = HudVertex { pos: [x0, y_b], uv: [u0, 1.0] };
            out.extend_from_slice(&[tl, bl, br, tl, br, tr]);
        }
        x += gw + gap;
    }
}

/// The width in NDC that [`layout_text`] would occupy for `text` at glyph width
/// `gw` and gap `gap` (for right-alignment).
fn text_width(text: &str, gw: f32, gap: f32) -> f32 {
    let n = text.chars().count();
    if n == 0 {
        0.0
    } else {
        n as f32 * (gw + gap) - gap
    }
}

/// Build the ammo-counter HUD quads for the current weapon state, laid out bottom-
/// right and right-aligned: `MAG / RESERVE` (e.g. `7 / 70`). Reload feedback is the
/// viewmodel dip (the gun lowering), not on-screen text. `aspect` = framebuffer
/// w/h, used to keep glyphs proportioned (NDC x is `aspect`× wider in pixels than
/// NDC y).
pub fn ammo_quads(magazine: u32, reserve: u32, aspect: f32) -> Vec<HudVertex> {
    let text = format!("{magazine} / {reserve}");

    // Glyph height as a fraction of the NDC height; width keeps the 5:7 pixel
    // aspect after correcting for the (non-square) framebuffer.
    let gh = 0.075;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;

    // Right-align near the bottom-right corner.
    let right_edge = 0.94;
    let x_start = right_edge - text_width(&text, gw, gap);
    let y_top = -0.82;

    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The atlas covers every charset glyph and is RGBA8-sized.
    #[test]
    fn atlas_has_expected_dimensions() {
        let (w, h, px) = atlas_rgba();
        assert_eq!(h, GLYPH_H);
        assert_eq!(w, CHARSET.chars().count() as u32 * cell_width());
        assert_eq!(px.len(), (w * h * 4) as usize);
        // Some glyph pixels are opaque white (not an all-transparent atlas).
        assert!(px.chunks_exact(4).any(|p| p[3] == 255), "atlas has lit glyph texels");
    }

    /// Every charset character resolves to a cell and (except space) a bitmap.
    #[test]
    fn charset_is_fully_defined() {
        for c in CHARSET.chars() {
            assert!(cell_index(c).is_some(), "{c:?} has a cell");
            assert!(font::glyph(c).is_some(), "{c:?} has a bitmap");
        }
    }

    /// The counter right-aligns: a longer count string ("7 / 700") starts further
    /// left than a shorter one ("7 / 70").
    #[test]
    fn ammo_text_right_aligns() {
        let short = ammo_quads(7, 70, 1.6);
        let long = ammo_quads(7, 700, 1.6);
        assert!(!short.is_empty() && !long.is_empty());
        let short_left = short.iter().map(|v| v.pos[0]).fold(f32::INFINITY, f32::min);
        let long_left = long.iter().map(|v| v.pos[0]).fold(f32::INFINITY, f32::min);
        assert!(short_left > long_left, "the shorter count starts further right");
        // 6 verts per drawn glyph; "7 / 70" has 4 non-space glyphs (7 / 7 0).
        assert_eq!(short.len(), 4 * 6);
    }
}
