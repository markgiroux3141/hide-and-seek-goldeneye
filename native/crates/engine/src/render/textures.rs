//! Texture scheme registry + BMP asset load. Port of the JS `textureSchemes.json`
//! config (`src/scene/textureSchemes.js`) and the BMP loading side of
//! `src/scene/materials.js`.
//!
//! Each scheme maps zone indices (0..7) to a texture name + a `repeat` scale.
//! The zone layout matches `uv_zones`:
//!   0 = floor, 1 = ceiling, 2 = lower wall, 3 = upper wall,
//!   4 = tunnel legacy (flat color, never emitted), 5 = stair/doorframe
//!   sides+ceiling, 6 = doorframe floor, 7 = brace.
//!
//! `repeat` is applied as a UV *scale* in the shader (not baked into the mesh),
//! so switching a region's scheme is a bind-group swap with no re-bake. This
//! mirrors the JS `texture.repeat` (a texture-matrix scale) rather than the
//! prompt's alternative of baking it in.
//!
//! Assets are **embedded** via `include_bytes!` so the binary is self-contained
//! (no CWD-relative `public/textures/` lookup — the JS build fetched over HTTP).

use image::ImageFormat;

/// One zone's texture + repeat, or a flat color when `texture` is `None`
/// (zone 4 only; never actually emitted by the classifier).
#[derive(Clone, Copy, Debug)]
pub struct ZoneDef {
    pub texture: Option<&'static str>,
    pub repeat: f32,
    /// Flat color for a texture-less zone (JS `zone.color`), linear RGB 0..1.
    pub color: [f32; 3],
}

impl ZoneDef {
    const fn tex(texture: &'static str, repeat: f32) -> Self {
        ZoneDef { texture: Some(texture), repeat, color: [0.545, 0.451, 0.333] }
    }
}

/// A named texture scheme: 8 zone slots (some `None`) + an optional number key.
#[derive(Clone, Copy, Debug)]
pub struct Scheme {
    pub name: &'static str,
    pub label: &'static str,
    /// The number key ('1'..'9') that selects this scheme, or `None` (simple_blue).
    pub key: Option<char>,
    pub zones: [Option<ZoneDef>; 8],
}

// Shorthand for a defined zone slot.
const fn z(texture: &'static str, repeat: f32) -> Option<ZoneDef> {
    Some(ZoneDef::tex(texture, repeat))
}
const N: Option<ZoneDef> = None;

/// The scheme registry, transcribed from `public/textureSchemes.json`. Order is
/// the canonical scheme-index order (index 0 = the default, facility_white_tile).
pub static SCHEMES: &[Scheme] = &[
    Scheme {
        name: "facility_white_tile",
        label: "Facility White Tile",
        key: Some('1'),
        zones: [
            z("grey_tile_floor", 0.35),
            z("brown_wall", 0.10),
            z("white_tile", 1.0),
            z("brown_wall", 0.5),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "facility_blue_brick",
        label: "Facility Blue Brick",
        key: Some('2'),
        zones: [
            z("grey_tile_floor", 0.35),
            z("foam_ceiling", 0.25),
            z("blue_brick_wall", 0.45),
            z("blue_brick_wall", 0.45),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            z("white_brace", 0.25),
        ],
    },
    Scheme {
        name: "facility_industrial_room",
        label: "Facility Industrial",
        key: Some('3'),
        zones: [
            z("yellow_floor", 0.1),
            z("dark_ceiling", 0.8),
            z("grey_line_wall", 0.18),
            z("grey_line_wall", 0.18),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            z("white_brace", 0.25),
        ],
    },
    Scheme {
        name: "facility_split_wall_a",
        label: "Facility Split Wall A",
        key: Some('4'),
        zones: [
            z("tempImgEd02B7", 0.35),
            z("tempImgEd0102", 0.35),
            z("tempImgEd02CE", 1.0),
            z("tempImgEd0123", 0.1667),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "facility_split_wall_b",
        label: "Facility Split Wall B",
        key: Some('5'),
        zones: [
            z("tempImgEd02B7", 0.35),
            z("tempImgEd0102", 0.35),
            z("tempImgEd02CE", 1.0),
            z("tempImgEd0125", 0.1667),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "facility_solid_wall_a",
        label: "Facility Solid Wall A",
        key: Some('6'),
        zones: [
            z("tempImgEd02B7", 0.35),
            z("tempImgEd0102", 0.35),
            z("tempImgEd02D4", 1.0),
            z("tempImgEd02D4", 1.0),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "facility_solid_wall_b",
        label: "Facility Solid Wall B",
        key: Some('7'),
        zones: [
            z("tempImgEd02B7", 0.35),
            z("tempImgEd0102", 0.35),
            z("tempImgEd02D5", 1.0),
            z("tempImgEd02D5", 1.0),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "archives_1",
        label: "Archives 1",
        key: Some('8'),
        zones: [
            z("tempImgEd00EB", 0.35),
            z("tempImgEd05F4", 0.35),
            z("tempImgEd05C2", 0.1667),
            z("tempImgEd05BC", 0.1667),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "bunker_1",
        label: "Bunker 1",
        key: Some('9'),
        zones: [
            z("tempImgEd0385", 0.35),
            z("tempImgEd0101", 0.35),
            z("tempImgEd013B", 0.25),
            z("tempImgEd013B", 0.25),
            N,
            z("stair_gradient", 1.0),
            z("floor_doorframe", 0.35),
            N,
        ],
    },
    Scheme {
        name: "simple_blue",
        label: "Simple Blue",
        key: None,
        zones: [
            z("floor_doorframe", 1.0),
            z("floor_doorframe", 1.0),
            z("blue_stairs", 1.0),
            z("blue_stairs", 1.0),
            N,
            z("blue_stairs", 1.0),
            N,
            // Zone 7 is the "brace" slot for room schemes, but the simple scheme
            // never textures braces — so structures reuse it for their railings,
            // pointing at the alpha-keyed `railing` texture (see [`RAILING_ZONE`]).
            z("railing", 1.0),
        ],
    },
];

/// The default scheme index (facility_white_tile) — used for new regions.
pub const DEFAULT_SCHEME: usize = 0;

/// Scheme index of the free-standing platform/stair "simple" style (JS
/// `PLATFORM_STYLES.simple.schemeName = 'simple_blue'`). The structures mesh
/// always uses this, independent of whatever scheme the surrounding room has.
pub const SIMPLE_SCHEME: usize = 9;

/// Zone slot the structures mesh tags its railings with → the transparent
/// `railing` texture. Reuses the brace slot (7), which the simple scheme never
/// uses for braces. Alpha-tested in `shader_textured.wgsl`.
pub const RAILING_ZONE: u8 = 7;

/// Textures embedded with an alpha channel keyed from near-black (JS converts
/// `public/transparent_textures/*.bmp` black pixels → fully transparent). The
/// shader alpha-tests these so the transparent parts are cut out.
const TRANSPARENT_BLACK: &[&str] = &["railing"];

/// Resolve a number key ('1'..'9') to a scheme index, or `None` if unbound.
pub fn scheme_for_key(key: char) -> Option<usize> {
    SCHEMES.iter().position(|s| s.key == Some(key))
}

/// A decoded RGBA8 image.
pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode an embedded BMP by texture name → RGBA8, or `None` if the name isn't
/// one we embed / fails to decode.
pub fn decode(name: &str) -> Option<DecodedTexture> {
    let bytes = texture_bytes(name)?;
    let img = image::load_from_memory_with_format(bytes, ImageFormat::Bmp).ok()?;
    let mut rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    // Key near-black to fully transparent for the railing-style textures (JS
    // `initMaterials` does the same on the canvas), so alpha-testing cuts them out.
    if TRANSPARENT_BLACK.contains(&name) {
        for px in rgba.pixels_mut() {
            if px[0] < 10 && px[1] < 10 && px[2] < 10 {
                px[3] = 0;
            }
        }
    }
    Some(DecodedTexture {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Every distinct texture name referenced by any scheme (deduplicated).
pub fn all_texture_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for s in SCHEMES {
        for zone in s.zones.iter().flatten() {
            if let Some(t) = zone.texture {
                if !names.contains(&t) {
                    names.push(t);
                }
            }
        }
    }
    names
}

/// Map a texture name to its embedded BMP bytes. Paths are relative to this
/// source file: `native/crates/engine/src/` → repo root is four levels up.
fn texture_bytes(name: &str) -> Option<&'static [u8]> {
    macro_rules! tex {
        ($n:literal) => {
            include_bytes!(concat!("../../../../../public/textures/", $n, ".bmp")) as &[u8]
        };
    }
    macro_rules! tex_transparent {
        ($n:literal) => {
            include_bytes!(concat!("../../../../../public/transparent_textures/", $n, ".bmp")) as &[u8]
        };
    }
    Some(match name {
        "railing" => tex_transparent!("railing"),
        "grey_tile_floor" => tex!("grey_tile_floor"),
        "brown_wall" => tex!("brown_wall"),
        "white_tile" => tex!("white_tile"),
        "stair_gradient" => tex!("stair_gradient"),
        "floor_doorframe" => tex!("floor_doorframe"),
        "foam_ceiling" => tex!("foam_ceiling"),
        "blue_brick_wall" => tex!("blue_brick_wall"),
        "white_brace" => tex!("white_brace"),
        "yellow_floor" => tex!("yellow_floor"),
        "dark_ceiling" => tex!("dark_ceiling"),
        "grey_line_wall" => tex!("grey_line_wall"),
        "blue_stairs" => tex!("blue_stairs"),
        "tempImgEd02B7" => tex!("tempImgEd02B7"),
        "tempImgEd0102" => tex!("tempImgEd0102"),
        "tempImgEd02CE" => tex!("tempImgEd02CE"),
        "tempImgEd0123" => tex!("tempImgEd0123"),
        "tempImgEd0125" => tex!("tempImgEd0125"),
        "tempImgEd02D4" => tex!("tempImgEd02D4"),
        "tempImgEd02D5" => tex!("tempImgEd02D5"),
        "tempImgEd00EB" => tex!("tempImgEd00EB"),
        "tempImgEd05F4" => tex!("tempImgEd05F4"),
        "tempImgEd05C2" => tex!("tempImgEd05C2"),
        "tempImgEd05BC" => tex!("tempImgEd05BC"),
        "tempImgEd0385" => tex!("tempImgEd0385"),
        "tempImgEd0101" => tex!("tempImgEd0101"),
        "tempImgEd013B" => tex!("tempImgEd013B"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_referenced_texture_decodes() {
        for name in all_texture_names() {
            let d = decode(name).unwrap_or_else(|| panic!("texture {name} failed to decode"));
            assert!(d.width > 0 && d.height > 0, "{name} has zero dimensions");
            assert_eq!(
                d.rgba.len() as u32,
                d.width * d.height * 4,
                "{name} RGBA buffer size mismatch"
            );
        }
    }

    #[test]
    fn number_keys_map_to_schemes() {
        assert_eq!(scheme_for_key('1'), Some(0));
        assert_eq!(scheme_for_key('9'), Some(8));
        assert_eq!(SCHEMES[scheme_for_key('1').unwrap()].name, "facility_white_tile");
        // simple_blue has no key.
        assert!(SCHEMES.iter().any(|s| s.name == "simple_blue" && s.key.is_none()));
    }

    #[test]
    fn schemes_have_expected_shape() {
        assert_eq!(SCHEMES.len(), 10);
        // Zone 4 is never defined (flat-color legacy tunnel).
        for s in SCHEMES {
            assert!(s.zones[4].is_none(), "{} unexpectedly defines zone 4", s.name);
        }
    }

    #[test]
    fn simple_scheme_is_blue_with_a_railing_zone() {
        assert_eq!(SCHEMES[SIMPLE_SCHEME].name, "simple_blue");
        let rail = SCHEMES[SIMPLE_SCHEME].zones[RAILING_ZONE as usize]
            .expect("simple scheme defines the railing zone");
        assert_eq!(rail.texture, Some("railing"));
    }

    #[test]
    fn railing_texture_has_transparent_black() {
        let d = decode("railing").expect("railing decodes");
        // The key-to-transparent pass must have zeroed at least some alpha
        // (railing BMPs are black-background line art).
        assert!(
            d.rgba.chunks_exact(4).any(|px| px[3] == 0),
            "railing texture should have transparent (keyed-black) texels"
        );
    }
}
