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
pub mod health;

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

/// The dial readouts: `DANGER n / max   WAVE w`, centered along the top edge.
///
/// A live gauge for the two tuning dials — `=` / `-` for enemy hardness, `[` / `]` for
/// how many of them there are. The wave count sits here rather than in the scoreboard on
/// the right because it is a *setting* you are driving, not a score; and it has to be on
/// screen at all, because bisecting a defect by dropping to one hunter is worthless if
/// you cannot see how many you asked for. Plain spaces, no separator glyph — the HUD
/// atlas silently drops characters it does not carry.
///
/// `aspect` = framebuffer w/h (keeps glyphs proportioned).
pub fn danger_quads(level: u32, max: u32, wave: usize, aspect: f32) -> Vec<HudVertex> {
    let text = format!("DANGER {level} / {max}   WAVE {wave}");
    let gh = 0.05;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;
    let x_start = -text_width(&text, gw, gap) / 2.0; // top-center
    let y_top = 0.96;
    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

/// The credit-balance readout quads: `$N`, laid out along the top-left edge — the
/// player's money (earned from kills, spent in the BUILD-phase shop). Same glyph
/// size as [`danger_quads`]. `aspect` = framebuffer w/h (keeps glyphs proportioned).
pub fn credits_quads(credits: u32, aspect: f32) -> Vec<HudVertex> {
    let text = format!("${credits}");
    let gh = 0.05;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;
    let x_start = -0.94; // top-left corner
    let y_top = 0.96;
    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

/// The deathmatch scoreboard quads: `YOU n - n  SIMS n - n` (kills − deaths per side),
/// with `/ limit` appended when the round has a score limit. Laid out along the top edge,
/// right-aligned so it sits clear of the credits readout (top-left) and the difficulty
/// dial (top-centre). `aspect` = framebuffer w/h.
pub fn score_quads(
    you: (u32, u32),
    sims: (u32, u32),
    limit: u32,
    aspect: f32,
) -> Vec<HudVertex> {
    let text = if limit > 0 {
        format!("YOU {}-{} SIMS {}-{} / {limit}", you.0, you.1, sims.0, sims.1)
    } else {
        format!("YOU {}-{} SIMS {}-{}", you.0, you.1, sims.0, sims.1)
    };
    let gh = 0.05;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;
    let x_start = 0.94 - text_width(&text, gw, gap); // top-right
    let y_top = 0.96;
    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

/// The round clock: `TIME M:SS` remaining, on its own line directly under the
/// scoreboard and right-aligned with it.
///
/// Only drawn when the level authors a time limit (`World::round_time_left` is `None`
/// otherwise), so an unlimited round has no clock cluttering the corner. Seconds are
/// rounded **up**, so the last visible number is `0:01` and not a second of `0:00`.
pub fn time_quads(secs_left: f32, aspect: f32) -> Vec<HudVertex> {
    let total = secs_left.max(0.0).ceil() as u32;
    let text = format!("TIME {}:{:02}", total / 60, total % 60);
    let gh = 0.05;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;
    let x_start = 0.94 - text_width(&text, gw, gap);
    // One line below the scoreboard's 0.96, with a glyph's worth of air between them.
    let y_top = 0.96 - gh * 1.4;
    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

/// The just-collected banner: `PICKED UP <thing>`, centred a little below the middle
/// of the screen so it reads without covering the crosshair.
///
/// Deliberately a text line rather than a HUD widget — it is a momentary
/// confirmation of something the player already did, and the pickup sound carries
/// most of the feedback. `aspect` = framebuffer w/h.
pub fn pickup_quads(what: &str, aspect: f32) -> Vec<HudVertex> {
    let text = format!("PICKED UP {what}");
    let gh = 0.055;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.4;
    let x_start = -text_width(&text, gw, gap) / 2.0;
    // Below centre: clear of the crosshair, well above the ammo counter.
    let y_top = -0.30;
    let mut out = Vec::with_capacity(text.chars().count() * 6);
    layout_text(&text, x_start, y_top, gw, gh, gap, &mut out);
    out
}

/// The "YOU DIED" death-screen text quads (P5): a centered title + a smaller
/// prompt. Drawn white over the dark death overlay. `aspect` = w/h.
///
/// The prompt is no longer "PRESS R": with respawning, the player is coming back from the
/// spawn pool on its own after [`crate::world::RESPAWN_DELAY`] and `R` only cuts the wait
/// short — so the screen says what is about to happen rather than demanding an input.
pub fn death_quads(aspect: f32) -> Vec<HudVertex> {
    centered_message("YOU DIED", "RESPAWNING", aspect)
}

/// The round-over screen: which side took the score limit, and the prompt to start
/// another. Same layout as [`death_quads`], different words.
pub fn round_over_quads(player_won: bool, aspect: f32) -> Vec<HudVertex> {
    let title = if player_won { "YOU WIN" } else { "SIMS WIN" };
    centered_message(title, "PRESS R", aspect)
}

/// A centered big-title + small-prompt overlay pair, shared by the death and round-over
/// screens so they sit at identical positions.
fn centered_message(title: &str, prompt: &str, aspect: f32) -> Vec<HudVertex> {
    let mut out = Vec::new();
    let gh = 0.13;
    let gw = gh / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap = gw * 0.5;
    layout_text(title, -text_width(title, gw, gap) / 2.0, 0.16, gw, gh, gap, &mut out);
    let gh2 = 0.055;
    let gw2 = gh2 / aspect.max(1e-6) * (GLYPH_W as f32 / GLYPH_H as f32);
    let gap2 = gw2 * 0.5;
    layout_text(prompt, -text_width(prompt, gw2, gap2) / 2.0, -0.08, gw2, gh2, gap2, &mut out);
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

    /// The scoreboard lays out, stays right-aligned inside the NDC viewport, and drops
    /// the `/ limit` suffix in endless mode (so an endless round doesn't show `/ 0`).
    #[test]
    fn score_readout_right_aligns_and_hides_an_endless_limit() {
        let limited = score_quads((3, 1), (2, 4), 10, 1.6);
        let endless = score_quads((3, 1), (2, 4), 0, 1.6);
        assert!(!limited.is_empty() && !endless.is_empty());
        assert!(
            endless.len() < limited.len(),
            "endless mode omits the `/ limit` suffix"
        );
        // Right-aligned within the viewport: nothing runs off the right edge.
        let right = limited.iter().map(|v| v.pos[0]).fold(f32::NEG_INFINITY, f32::max);
        assert!(right <= 0.95, "the readout stays on screen (right edge {right})");
        // Two-digit scores push the text further left, never further right.
        let wide = score_quads((13, 11), (12, 14), 10, 1.6);
        let wide_left = wide.iter().map(|v| v.pos[0]).fold(f32::INFINITY, f32::min);
        let narrow_left = limited.iter().map(|v| v.pos[0]).fold(f32::INFINITY, f32::min);
        assert!(wide_left < narrow_left, "a longer score grows leftward");
    }

    /// Both end-screens share one layout, and each names its own outcome.
    #[test]
    fn round_over_screens_differ_from_the_death_screen() {
        let died = death_quads(1.6);
        let won = round_over_quads(true, 1.6);
        let lost = round_over_quads(false, 1.6);
        assert!(!died.is_empty() && !won.is_empty() && !lost.is_empty());
        // "YOU WIN" (6 drawn glyphs) and "SIMS WIN" (7) are different lengths, so the two
        // outcomes are genuinely distinct text rather than one shared string.
        assert_ne!(won.len(), lost.len(), "the two outcomes read differently");
    }

    /// **Every character the HUD prints must be in the [`CHARSET`].**
    ///
    /// A char without an atlas cell is silently *dropped* by [`layout_text`] — not drawn
    /// as a placeholder box — so a new HUD string containing an unatlased letter loses it
    /// on screen with nothing failing. That is not hypothetical: the scoreboard and the
    /// win screen needed `M`, `W` and `-`, none of which were in the charset, and without
    /// this test `SIMS WIN` would have shipped rendering as `SIS IN`.
    ///
    /// Each case below asserts the quad count equals 6 × (non-space chars), which only
    /// holds if every glyph in the string actually made it into the atlas.
    #[test]
    fn every_string_the_hud_prints_is_fully_atlased() {
        let drawn = |s: &str| s.chars().filter(|c| *c != ' ').count() * 6;

        assert_eq!(ammo_quads(7, 70, 1.6).len(), drawn("7 / 70"), "ammo counter");
        assert_eq!(
            danger_quads(4, 10, 3, 1.6).len(),
            drawn("DANGER 4 / 10   WAVE 3"),
            "danger + wave dials"
        );
        assert_eq!(credits_quads(250, 1.6).len(), drawn("$250"), "credit balance");
        assert_eq!(
            score_quads((3, 1), (2, 4), 10, 1.6).len(),
            drawn("YOU 3-1 SIMS 2-4 / 10"),
            "scoreboard"
        );
        assert_eq!(
            score_quads((3, 1), (2, 4), 0, 1.6).len(),
            drawn("YOU 3-1 SIMS 2-4"),
            "scoreboard (endless)"
        );
        assert_eq!(time_quads(161.0, 1.6).len(), drawn("TIME 2:41"), "round clock");
        assert_eq!(time_quads(0.0, 1.6).len(), drawn("TIME 0:00"), "round clock at zero");
        assert_eq!(death_quads(1.6).len(), drawn("YOU DIEDRESPAWNING"), "death screen");
        assert_eq!(round_over_quads(true, 1.6).len(), drawn("YOU WINPRESS R"), "win screen");
        assert_eq!(
            round_over_quads(false, 1.6).len(),
            drawn("SIMS WINPRESS R"),
            "loss screen"
        );
    }

    /// The clock reads as minutes and seconds, rounding **up** so the last visible
    /// number is `0:01` — a full second showing `0:00` before the round ends reads as a
    /// stopped clock.
    #[test]
    fn the_round_clock_counts_in_minutes_and_seconds() {
        let text = |secs: f32| {
            let t = secs.max(0.0).ceil() as u32;
            format!("TIME {}:{:02}", t / 60, t % 60)
        };
        assert_eq!(text(161.0), "TIME 2:41");
        assert_eq!(text(59.2), "TIME 1:00", "rounds up across the minute");
        assert_eq!(text(0.4), "TIME 0:01", "the last tick still shows a second");
        assert_eq!(text(-3.0), "TIME 0:00", "never negative");
    }

    /// **Every weapon name in every arsenal** survives the pickup banner.
    ///
    /// This is the one HUD string whose content is data rather than a literal, so it
    /// is the one that can silently lose a letter as the arsenal grows — a missing
    /// glyph is dropped, not boxed (`SIMS WIN` once printed as `SIS IN`). Checking the
    /// tables directly means a newly-added weapon fails here rather than in a
    /// playtest.
    #[test]
    fn every_weapon_name_survives_the_pickup_banner() {
        let drawn = |s: &str| s.chars().filter(|c| *c != ' ').count() * 6;
        for arsenal in [
            crate::combat::Arsenal::GoldenEye,
            crate::combat::Arsenal::PerfectDark,
            crate::combat::Arsenal::Both,
        ] {
            for w in arsenal.weapons() {
                for text in [w.name.to_ascii_uppercase(), format!("{} AMMO", w.name.to_ascii_uppercase())] {
                    let banner = format!("PICKED UP {text}");
                    assert_eq!(
                        pickup_quads(&text, 1.6).len(),
                        drawn(&banner),
                        "{:?} loses glyphs in the pickup banner — add them to \
                         `font::CHARSET` + `font::glyph`",
                        w.name
                    );
                }
            }
        }
    }
}
