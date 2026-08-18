//! A tiny code-defined 5×7 bitmap font for the HUD (the ammo counter and later
//! HUD text). Each glyph is 7 rows; in a row the low 5 bits are the pixels, bit 4
//! (`0b10000`) leftmost. Only the glyphs the HUD needs so far are defined —
//! digits, a slash, space, and the letters in "RELOADING". Extend [`glyph`] +
//! [`CHARSET`] to add more (e.g. for future HUD strings).

/// Glyph cell dimensions in texels.
pub const GLYPH_W: u32 = 5;
pub const GLYPH_H: u32 = 7;
/// Transparent padding column appended to each glyph cell in the atlas, so a
/// nearest-sampled quad that bleeds a texel past the glyph lands on transparency
/// rather than the neighbouring glyph.
pub const PAD: u32 = 1;

/// The atlas glyph order. A character's cell index in the atlas is its position
/// here; [`crate::hud::cell_index`] maps a `char` to it.
///
/// **A glyph missing from here is silently dropped**, not drawn as a box —
/// [`crate::hud::layout_text`] skips any char without a cell. So this must cover every
/// character the HUD actually prints, or a string quietly loses letters (`SIMS WIN`
/// rendered as `SIS IN` before `M` and `W` were added for the scoreboard).
pub const CHARSET: &str = "0123456789/- YOUDIEPRSANGWM$";

/// Full atlas cell width = glyph + the transparent [`PAD`] column(s).
pub const fn cell_width() -> u32 {
    GLYPH_W + PAD
}

/// The 7-row bitmap for a supported glyph, or `None` if unsupported (callers skip
/// it — space is supported but blank so text still advances).
pub fn glyph(c: char) -> Option<[u8; 7]> {
    let rows = match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        // Score separator, as in `3-1`.
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        // `M` / `W` — the scoreboard's "SIMS" and the win/respawn screens' "WIN".
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'N' => [0b10001, 0b11001, 0b11001, 0b10101, 0b10011, 0b10011, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        // `$` — an S with a vertical bar struck through it (credits readout).
        '$' => [0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100],
        _ => return None,
    };
    Some(rows)
}
