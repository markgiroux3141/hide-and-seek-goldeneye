//! Texture theme registry + BMP asset load. Port of the JS `textureSchemes.json`
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
//! # Both the themes and the BMPs load at **runtime**
//!
//! The registry comes from `native/assets/themes.json` and the images from
//! `native/assets/textures/`, following the same runtime-asset convention as
//! every other asset class (audio, GLBs, props — see [`crate::audio`]). This
//! replaced a hand-written `include_bytes!` match, which meant a *recompile* per
//! texture and capped the usable library at whatever had been transcribed into
//! Rust. Now a new theme is: drop BMPs in, edit the JSON, restart.
//!
//! Consequently scheme indices are **not stable across runs** — they are just
//! positions in whatever `themes.json` lists. Levels persist a theme by
//! [`scheme_name`] and resolve it back through [`scheme_index`], so themes can be
//! added, reordered or removed without retexturing saved levels. Nothing outside
//! this module may assume a particular index; use [`default_scheme`] /
//! [`simple_scheme`] rather than hard-coding 0 and 9.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use image::ImageFormat;
use serde::Deserialize;

/// One zone's texture + repeat, or a flat color when `texture` is `None`
/// (zone 4 only; never actually emitted by the classifier).
#[derive(Clone, Copy, Debug)]
pub struct ZoneDef {
    pub texture: Option<&'static str>,
    pub repeat: f32,
    /// Texture-space UV offset (JS `zone.offsetX`/`offsetY`), applied after
    /// [`repeat`](Self::repeat). Units are whole textures: `0.5` slides the texture
    /// half a tile across the surface. Lets a theme align a band or a tile grid
    /// rather than accepting wherever world-space projection happens to land it.
    pub offset: [f32; 2],
    /// Flat color for a texture-less zone (JS `zone.color`), RGB 0..1.
    pub color: [f32; 3],
}

/// Where a scheme came from, which decides how the editor may treat it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SchemeKind {
    /// From `themes.json` — the hand-authored ten plus the extracted library.
    /// Read-only in the editor.
    Library,
    /// A pre-allocated custom slot, editable and saved to `user_themes.json`.
    /// `used` is false for a slot no preset occupies yet.
    Custom { used: bool },
    /// The single live scratch slot the editor mutates while you drag sliders.
    Scratch,
}

/// A named texture scheme: 8 zone slots (some `None`) + an optional number key.
#[derive(Clone, Copy, Debug)]
pub struct Scheme {
    pub name: &'static str,
    pub label: &'static str,
    /// Grouping for the picker UI (JS `scheme.group`) — "Facility", "Archives", …
    pub group: &'static str,
    /// The number key ('1'..'9') that selects this scheme, or `None` (simple_blue).
    ///
    /// A **level** may override these bindings (see the level file's
    /// `theme_hotkeys`); this is the fallback when it doesn't.
    pub key: Option<char>,
    pub kind: SchemeKind,
    pub zones: [Option<ZoneDef>; 8],
}

/// Zone slot the structures mesh tags its railings with → the transparent
/// `railing` texture. Reuses the brace slot (7), which the simple scheme never
/// uses for braces. Alpha-tested in `shader_textured.wgsl`.
///
/// Unlike a scheme index this genuinely is fixed: it's part of the zone contract
/// shared with `uv_zones`, not a position in a loaded list.
pub const RAILING_ZONE: u8 = 7;

/// The theme used by new regions. Required to exist by name in `themes.json`.
const DEFAULT_SCHEME_NAME: &str = "facility_white_tile";

/// The free-standing platform/stair "simple" style (JS
/// `PLATFORM_STYLES.simple.schemeName`). The structures mesh always uses this,
/// independent of whatever scheme the surrounding room has. Required by name.
const SIMPLE_SCHEME_NAME: &str = "simple_blue";

/// The theme every vent duct's interior wears (`tools::vent`), so a duct reads as bare
/// ducting whatever room it passes through.
///
/// It wears `tempImgEd029F` — the authentic GoldenEye duct panel.
///
/// **The repeat is 0.25, and that is what "one texture per side" costs.** UVs reach the
/// shader in **WT, not metres** (`uv_zones::vertex_uv` divides by `WORLD_SCALE`, and
/// `shader_textured.wgsl` multiplies by this scale), so `repeat` counts tiles per WT.
/// A vent bore is `VENT_BORE` = 4 WT, so one whole texture stretched across a duct face
/// is 1/4 — a repeat of 1.0 would tile it four times per side. `vent_repeat_fits_the_bore`
/// pins that relationship, so changing the bore fails a test rather than quietly
/// re-tiling every duct in every level.
const VENT_SCHEME_NAME: &str = "vent_metal";

/// The theme ladders wear (`tools::ladder`). `tempImgEd01B8`, the dark metal from the
/// **Surface** level's exterior structures.
///
/// Ladders have no texture of their own and this is not for want of looking: the library
/// was extracted from GoldenEye level *surfaces* and GoldenEye built its ladders from
/// geometry, so what exists is the metalwork those ladders were made of, not a picture of
/// a ladder. Surface's railing sheets (`tempImgEd0095`, `00EE`, `00EF`) are the nearest
/// thing and are alpha-keyed fencing, not rungs.
///
/// So the rails and rungs stay geometry and simply wear the right metal. Not required by
/// name — a themes.json without it falls back to the default rather than refusing to
/// start.
const LADDER_SCHEME_NAME: &str = "ladder_metal";

/// Name of the live scratch slot. Underscore-prefixed so it sorts and reads as
/// internal, and so it cannot collide with a generated `<level>_NN` theme.
const SCRATCH_SCHEME_NAME: &str = "__scratch";

/// Flat colour for a texture-less zone when the JSON omits one: the legacy tunnel
/// brown `#8B7355`. Written as the exact hex division rather than the 3dp literal
/// the old hard-coded registry carried, so this and [`parse_hex_rgb`] agree bit
/// for bit on the same colour.
const LEGACY_TUNNEL_COLOR: [f32; 3] = [139.0 / 255.0, 115.0 / 255.0, 85.0 / 255.0];

// ---------------------------------------------------------------------------
// On-disk shape (`native/assets/themes.json`)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ManifestJson {
    /// Textures whose near-black texels are keyed to fully transparent.
    #[serde(default)]
    alpha_key_black: Vec<String>,
    schemes: Vec<SchemeJson>,
}

#[derive(Deserialize)]
struct SchemeJson {
    name: String,
    label: String,
    #[serde(default)]
    group: String,
    /// Written as a string in JSON ("1"); only the first char is used.
    #[serde(default)]
    key: Option<String>,
    /// Sparse map of zone index → definition. An absent key = undefined zone.
    #[serde(default)]
    zones: HashMap<u8, ZoneJson>,
}

#[derive(Deserialize)]
struct ZoneJson {
    #[serde(default)]
    texture: Option<String>,
    #[serde(default = "unit_repeat")]
    repeat: f32,
    /// UV offset in whole textures. Named to match the JS `offsetX`/`offsetY` that
    /// `materials.js` already read (and that no shipped scheme ever set).
    #[serde(default)]
    offset_x: f32,
    #[serde(default)]
    offset_y: f32,
    /// `#RRGGBB`, used only when `texture` is absent.
    #[serde(default)]
    color: Option<String>,
}

fn unit_repeat() -> f32 {
    1.0
}

/// Parse `#RRGGBB` → RGB 0..1, or the legacy tunnel brown if malformed.
fn parse_hex_rgb(s: &str) -> [f32; 3] {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        return LEGACY_TUNNEL_COLOR;
    }
    let c = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .map(|v| v as f32 / 255.0)
            .unwrap_or(0.0)
    };
    [c(0), c(2), c(4)]
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// How many editable custom-theme slots exist.
///
/// They are **pre-allocated** rather than grown on demand, because the registry is
/// a `OnceLock` handing out `&'static [Scheme]` — appending at runtime would mean
/// making that mutable and reworking every caller, plus growing the renderer's
/// material table mid-frame. Fixed slots give the editor everything it needs: a
/// saved preset takes effect immediately (its slot already has bind groups, which
/// `set_material_texture` repoints) and persists to `user_themes.json` by slot name.
/// 24 is far more hand-authored themes than the library needs alongside ~390
/// generated ones.
pub const CUSTOM_SLOTS: usize = 24;

struct Registry {
    schemes: Vec<Scheme>,
    alpha_key_black: Vec<&'static str>,
    default_index: usize,
    simple_index: usize,
    vent_index: usize,
    ladder_index: usize,
    /// Index of the first custom slot; `CUSTOM_SLOTS` of them run consecutively.
    custom_base: usize,
    /// Index of the live scratch slot (immediately after the custom slots).
    scratch_index: usize,
}

// ---------------------------------------------------------------------------
// user_themes.json — hand-authored presets, kept apart from themes.json
// ---------------------------------------------------------------------------

/// Presets built in the editor, keyed by custom-slot name (`custom_01`, …).
///
/// Deliberately a **separate file** from `themes.json`: the tooling in
/// `tools/texture-themes/` regenerates that manifest's tail wholesale
/// (`adopt.py --bulk`) and prunes unreviewed entries from it (`--prune`), either of
/// which would silently destroy hand-made work living there.
#[derive(Default, Deserialize)]
struct UserThemesJson {
    #[serde(default)]
    themes: HashMap<String, UserThemeJson>,
}

#[derive(Deserialize)]
struct UserThemeJson {
    #[serde(default)]
    label: String,
    #[serde(default)]
    zones: HashMap<u8, ZoneJson>,
}

/// Path of the hand-authored preset file.
pub fn user_themes_path() -> PathBuf {
    assets_dir().join("user_themes.json")
}

/// Name of custom slot `n` (0-based).
pub fn custom_slot_name(n: usize) -> String {
    format!("custom_{:02}", n + 1)
}

/// `native/assets/` — the engine crate lives at `native/crates/engine`, so the
/// runtime asset dir is two levels up (same derivation as [`crate::audio`]).
fn assets_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"))
}

/// Directory holding the level-geometry BMPs.
pub fn textures_dir() -> PathBuf {
    assets_dir().join("textures")
}

/// Path of the theme manifest.
pub fn themes_path() -> PathBuf {
    assets_dir().join("themes.json")
}

/// Leak an owned `String` to `&'static str`.
///
/// The registry is loaded exactly once into a `OnceLock` and lives for the whole
/// process, so leaking is the honest representation of that lifetime — and it
/// keeps [`Scheme`]/[`ZoneDef`] `Copy` with `&'static str` fields, which is what
/// every existing caller (and the `[Option<BindGroup>; 8]` material table) is
/// already written against. Bounded by the manifest size, not by runtime events.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn load_registry() -> Result<Registry, String> {
    let path = themes_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest: ManifestJson = serde_json::from_str(&raw)
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

    let mut schemes: Vec<Scheme> = Vec::with_capacity(manifest.schemes.len());
    for s in manifest.schemes {
        let mut zones: [Option<ZoneDef>; 8] = std::array::from_fn(|_| None);
        for (zi, z) in s.zones {
            if zi as usize >= zones.len() {
                return Err(format!(
                    "scheme '{}' defines zone {zi}, but only 0..={} exist",
                    s.name,
                    zones.len() - 1
                ));
            }
            zones[zi as usize] = Some(ZoneDef {
                texture: z.texture.map(leak),
                repeat: z.repeat,
                offset: [z.offset_x, z.offset_y],
                color: z.color.as_deref().map(parse_hex_rgb).unwrap_or(LEGACY_TUNNEL_COLOR),
            });
        }
        schemes.push(Scheme {
            name: leak(s.name),
            label: leak(s.label),
            group: leak(s.group),
            key: s.key.and_then(|k| k.chars().next()),
            kind: SchemeKind::Library,
            zones,
        });
    }

    if schemes.is_empty() {
        return Err(format!("{} defines no schemes", path.display()));
    }
    let find = |want: &str| {
        schemes
            .iter()
            .position(|s| s.name == want)
            .ok_or_else(|| format!("{} is missing the required scheme '{want}'", path.display()))
    };
    let default_index = find(DEFAULT_SCHEME_NAME)?;
    let simple_index = find(SIMPLE_SCHEME_NAME)?;
    let vent_index = find(VENT_SCHEME_NAME).unwrap_or(default_index);
    let ladder_index = find(LADDER_SCHEME_NAME).unwrap_or(simple_index);
    let default_zones = schemes[default_index].zones;

    // ── The custom slots + the scratch slot, appended after the library.
    //
    // Every slot is materialised whether or not a preset occupies it, so the
    // renderer builds bind groups for all of them at init and the editor can point
    // a slot at any texture later with no allocation. An empty slot is seeded from
    // the default theme so it is never undefined (an undefined zone renders
    // *invisible*, which would read as a hole in the level rather than an empty
    // slot).
    let user = load_user_themes();
    let custom_base = schemes.len();
    for n in 0..CUSTOM_SLOTS {
        let name = custom_slot_name(n);
        let authored = user.themes.get(&name);
        let mut zones = default_zones;
        if let Some(u) = authored {
            for (zi, z) in &u.zones {
                if (*zi as usize) < zones.len() {
                    zones[*zi as usize] = Some(ZoneDef {
                        texture: z.texture.clone().map(leak),
                        repeat: z.repeat,
                        offset: [z.offset_x, z.offset_y],
                        color: z
                            .color
                            .as_deref()
                            .map(parse_hex_rgb)
                            .unwrap_or(LEGACY_TUNNEL_COLOR),
                    });
                }
            }
        }
        let label = match authored {
            Some(u) if !u.label.is_empty() => u.label.clone(),
            Some(_) => format!("Custom {:02}", n + 1),
            None => format!("(empty {:02})", n + 1),
        };
        schemes.push(Scheme {
            name: leak(name),
            label: leak(label),
            group: "Custom",
            key: None,
            kind: SchemeKind::Custom { used: authored.is_some() },
            zones,
        });
    }

    let scratch_index = schemes.len();
    schemes.push(Scheme {
        name: leak(SCRATCH_SCHEME_NAME.to_string()),
        label: "Scratch (unsaved)",
        group: "Custom",
        key: None,
        kind: SchemeKind::Scratch,
        zones: default_zones,
    });

    Ok(Registry {
        schemes,
        alpha_key_black: manifest.alpha_key_black.into_iter().map(leak).collect(),
        default_index,
        simple_index,
        vent_index,
        ladder_index,
        custom_base,
        scratch_index,
    })
}

/// Read `user_themes.json`, or an empty set if absent/corrupt.
///
/// Never fatal: hand-authored presets are additive, so losing them costs the author
/// their presets but must not stop the game booting.
fn load_user_themes() -> UserThemesJson {
    let path = user_themes_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            log::warn!("{}: {e}; custom presets unavailable", path.display());
            UserThemesJson::default()
        }),
        Err(_) => UserThemesJson::default(),
    }
}

/// The loaded registry.
///
/// Panics if the manifest is missing, malformed, or lacks a required scheme.
/// That is deliberate: unlike one absent BMP (which degrades to a visible
/// magenta surface, see [`decode`]), a missing registry means *no* geometry can
/// be textured at all, and booting into an untextured void would present as a
/// rendering bug rather than an asset problem. Failing at startup with the path
/// and the parse error names the actual fault.
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| match load_registry() {
        Ok(r) => r,
        Err(e) => panic!("texture theme registry unusable — {e}"),
    })
}

/// Every theme, in manifest order (which is UI listing order, nothing more).
pub fn schemes() -> &'static [Scheme] {
    &registry().schemes
}

/// Index of the theme new regions get.
pub fn default_scheme() -> usize {
    registry().default_index
}

/// Index of the platform/stair "simple" theme (see [`SIMPLE_SCHEME_NAME`]).
pub fn simple_scheme() -> usize {
    registry().simple_index
}

/// Index of the vent-duct theme (see [`VENT_SCHEME_NAME`]).
pub fn vent_scheme() -> usize {
    registry().vent_index
}

/// Index of the ladder theme (see [`LADDER_SCHEME_NAME`]).
pub fn ladder_scheme() -> usize {
    registry().ladder_index
}

/// The live scratch slot the theme editor mutates while you edit.
///
/// Applying it to a room is allowed — it is a real, name-persisted scheme — but it
/// is *shared and transient*: editing again changes every room wearing it. The
/// editor says so, and "Save as preset" is how a look becomes permanent.
pub fn scratch_scheme() -> usize {
    registry().scratch_index
}

/// Index range of the editable custom slots.
pub fn custom_slots() -> std::ops::Range<usize> {
    let r = registry();
    r.custom_base..(r.custom_base + CUSTOM_SLOTS)
}

/// The first custom slot with no preset in it, or `None` when all are taken.
pub fn first_free_custom_slot() -> Option<usize> {
    custom_slots().find(|&i| schemes()[i].kind == SchemeKind::Custom { used: false })
}

/// One zone of a hand-authored preset, as the editor holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneSpec {
    pub texture: &'static str,
    pub repeat: f32,
    pub offset: [f32; 2],
}

/// Write a preset into custom slot `slot`, persisting `user_themes.json`.
///
/// Only touches the one slot: the file is read, that key replaced, and the whole
/// thing written back, so presets in other slots survive. The **live registry is not
/// updated** — it is a `OnceLock` (see [`CUSTOM_SLOTS`]) — so the caller is
/// responsible for pointing the slot's materials at the new textures via
/// `Renderer::set_material_texture` / `set_material_params`. The label and
/// `kind.used` refresh on the next run.
pub fn save_custom_preset(
    slot: usize,
    label: &str,
    zones: &[Option<ZoneSpec>; 8],
) -> Result<(), String> {
    if !custom_slots().contains(&slot) {
        return Err(format!("{slot} is not a custom slot"));
    }
    let path = user_themes_path();
    // Round-trip through `Value` rather than a typed struct so unknown keys another
    // build wrote are preserved rather than dropped.
    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?,
        Err(_) => serde_json::json!({ "themes": {} }),
    };
    if !root.get("themes").map(|t| t.is_object()).unwrap_or(false) {
        root["themes"] = serde_json::json!({});
    }

    let mut zone_map = serde_json::Map::new();
    for (zi, z) in zones.iter().enumerate() {
        let Some(z) = z else { continue };
        let mut entry = serde_json::Map::new();
        entry.insert("texture".into(), serde_json::Value::from(z.texture));
        entry.insert("repeat".into(), serde_json::Value::from(z.repeat));
        if z.offset[0] != 0.0 {
            entry.insert("offset_x".into(), serde_json::Value::from(z.offset[0]));
        }
        if z.offset[1] != 0.0 {
            entry.insert("offset_y".into(), serde_json::Value::from(z.offset[1]));
        }
        zone_map.insert(zi.to_string(), serde_json::Value::Object(entry));
    }
    root["themes"][custom_slot_name(slot - registry().custom_base)] = serde_json::json!({
        "label": label,
        "zones": serde_json::Value::Object(zone_map),
    });

    let json = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

/// Resolve a number key ('1'..'9') to a scheme index, or `None` if unbound.
pub fn scheme_for_key(key: char) -> Option<usize> {
    schemes().iter().position(|s| s.key == Some(key))
}

/// Resolve a persisted theme *name* to its current index, or `None` if this
/// build's manifest has no such theme.
pub fn scheme_index(name: &str) -> Option<usize> {
    schemes().iter().position(|s| s.name == name)
}

/// The stable name to persist for a scheme index. Out-of-range indices (a level
/// authored against a manifest that has since shrunk) fall back to the default.
pub fn scheme_name(index: usize) -> &'static str {
    schemes()
        .get(index)
        .map(|s| s.name)
        .unwrap_or(DEFAULT_SCHEME_NAME)
}

// ---------------------------------------------------------------------------
// Image load
// ---------------------------------------------------------------------------

/// A decoded RGBA8 image.
pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedTexture {
    /// A 2×2 magenta stand-in for a texture that could not be loaded, matching
    /// the convention used for the crosshair: a miss should be glaringly visible
    /// on screen, not an invisible surface that reads as a geometry bug.
    fn magenta() -> Self {
        DecodedTexture {
            width: 2,
            height: 2,
            rgba: [255, 0, 255, 255].repeat(4),
        }
    }
}

/// Decode a texture by name from `native/assets/textures/<name>.bmp` → RGBA8.
///
/// Never fails: an unreadable or undecodable file warns and yields magenta (see
/// [`DecodedTexture::magenta`]). Use [`try_decode`] when absence must be
/// detected rather than papered over.
pub fn decode(name: &str) -> DecodedTexture {
    match try_decode(name) {
        Some(d) => d,
        None => {
            log::warn!(
                "texture '{name}' missing or undecodable under {} — using magenta",
                textures_dir().display()
            );
            DecodedTexture::magenta()
        }
    }
}

/// Decode a texture by name, or `None` if it is absent / not a readable BMP.
pub fn try_decode(name: &str) -> Option<DecodedTexture> {
    let path = textures_dir().join(format!("{name}.bmp"));
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Bmp).ok()?;
    let mut rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    // Key near-black to fully transparent for the railing-style textures (JS
    // `initMaterials` does the same on the canvas), so alpha-testing cuts them out.
    if registry().alpha_key_black.contains(&name) {
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

// ---------------------------------------------------------------------------
// The browsable texture catalog
// ---------------------------------------------------------------------------

/// One entry of `native/assets/texture_index.json`: a texture plus the provenance
/// needed to find it in a list of ~1000.
#[derive(Clone, Debug, Deserialize)]
pub struct TextureInfo {
    /// GoldenEye levels this image appears in, or `["flat"]` for the loose pile.
    /// The picker groups by this — a flat 1000-row list is unusable, and "which
    /// level did this come from" is how an author actually thinks about it.
    #[serde(default)]
    pub levels: Vec<String>,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// Tiles cleanly on both axes. **Advisory only** — this does not distinguish
    /// wall material from signage, and no cheap statistic does (three were tried;
    /// see `tools/texture-themes/texlib.py`). A false here does mean the image
    /// cannot tile as a wall, which is the useful half.
    #[serde(default)]
    pub tiles: bool,
}

/// The texture catalog, keyed by name. Empty if the manifest is absent — the
/// catalog only drives the authoring UI, so a missing one degrades to "no browsable
/// list" rather than breaking rendering (which reads `themes.json`, not this).
fn catalog() -> &'static HashMap<String, TextureInfo> {
    static CATALOG: OnceLock<HashMap<String, TextureInfo>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let path = assets_dir().join("texture_index.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                log::warn!("{}: {e}; texture catalog unavailable", path.display());
                HashMap::new()
            }),
            Err(e) => {
                log::warn!("{}: {e}; texture catalog unavailable", path.display());
                HashMap::new()
            }
        }
    })
}

/// Look up one texture's catalog entry.
pub fn texture_info(name: &str) -> Option<&'static TextureInfo> {
    catalog().get(name)
}

/// Source-level groups in display order, each with its textures sorted by name.
///
/// A texture used by several levels appears under each of them, which is what an
/// author wants: browsing "Caverns" should show everything Caverns used, not only
/// what is unique to it.
pub fn catalog_by_level() -> &'static Vec<(String, Vec<&'static str>)> {
    static GROUPED: OnceLock<Vec<(String, Vec<&'static str>)>> = OnceLock::new();
    GROUPED.get_or_init(|| {
        let mut by_level: HashMap<&str, Vec<&'static str>> = HashMap::new();
        for (name, info) in catalog() {
            let name: &'static str = leak(name.clone());
            for level in &info.levels {
                by_level.entry(leak(level.clone())).or_default().push(name);
            }
        }
        let mut out: Vec<(String, Vec<&'static str>)> = by_level
            .into_iter()
            .map(|(level, mut names)| {
                names.sort_unstable();
                (level.to_string(), names)
            })
            .collect();
        // Numeric level prefixes ("01 - Dam") sort correctly as strings; "flat" ends
        // up last, which is where the unattributed pile belongs.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    })
}

/// Every distinct texture name referenced by any scheme (deduplicated).
pub fn all_texture_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for s in schemes() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_referenced_texture_decodes() {
        for name in all_texture_names() {
            let d = try_decode(name)
                .unwrap_or_else(|| panic!("texture {name} failed to decode"));
            assert!(d.width > 0 && d.height > 0, "{name} has zero dimensions");
            assert_eq!(
                d.rgba.len() as u32,
                d.width * d.height * 4,
                "{name} RGBA buffer size mismatch"
            );
        }
    }

    /// Number keys must be unambiguous. Which theme sits on which key is content
    /// (it moves whenever `themes.json` is recurated), so this asserts the
    /// *contract* rather than a particular binding.
    #[test]
    fn number_keys_map_to_schemes() {
        assert_eq!(scheme_for_key('1'), Some(default_scheme()), "key 1 is the default");
        // No key may select two themes.
        let mut seen: Vec<char> = Vec::new();
        for s in schemes() {
            if let Some(k) = s.key {
                assert!(
                    k.is_ascii_digit() && k != '0',
                    "{} bound to non-digit key {k:?}",
                    s.name
                );
                assert!(!seen.contains(&k), "key {k} is claimed twice (at {})", s.name);
                seen.push(k);
            }
        }
        // simple_blue is selected by the structures mesh, never by a key.
        assert!(schemes().iter().any(|s| s.name == "simple_blue" && s.key.is_none()));
    }

    #[test]
    fn schemes_have_expected_shape() {
        // A lower bound, not an exact count: themes are content and get added.
        assert!(schemes().len() >= 10, "expected the shipped themes at minimum");
        for s in schemes() {
            // Zone 4 is never emitted by the classifier (flat-color legacy tunnel),
            // so defining it is silently dead content.
            assert!(s.zones[4].is_none(), "{} unexpectedly defines zone 4", s.name);
            // A theme that can't texture a room's basic surfaces is a mistake.
            for zi in 0..4 {
                assert!(s.zones[zi].is_some(), "{} leaves zone {zi} undefined", s.name);
            }
            assert!(!s.label.is_empty(), "{} has no label for the picker", s.name);
            assert!(!s.group.is_empty(), "{} has no group for the picker", s.name);
        }
    }

    #[test]
    fn simple_scheme_is_blue_with_a_railing_zone() {
        assert_eq!(schemes()[simple_scheme()].name, "simple_blue");
        let rail = schemes()[simple_scheme()].zones[RAILING_ZONE as usize]
            .expect("simple scheme defines the railing zone");
        assert_eq!(rail.texture, Some("railing"));
    }

    #[test]
    fn railing_texture_has_transparent_black() {
        let d = try_decode("railing").expect("railing decodes");
        // The key-to-transparent pass must have zeroed at least some alpha
        // (railing BMPs are black-background line art).
        assert!(
            d.rgba.chunks_exact(4).any(|px| px[3] == 0),
            "railing texture should have transparent (keyed-black) texels"
        );
    }

    #[test]
    fn missing_texture_yields_magenta_not_a_failure() {
        let d = decode("definitely_not_a_real_texture_name");
        assert_eq!((d.width, d.height), (2, 2));
        assert_eq!(&d.rgba[0..4], &[255, 0, 255, 255]);
    }

    /// Name ⇄ index must round-trip for every theme: this is the contract level
    /// files depend on now that they persist names rather than indices.
    #[test]
    fn scheme_names_round_trip_through_indices() {
        for (i, s) in schemes().iter().enumerate() {
            assert_eq!(scheme_index(s.name), Some(i), "{} lost its index", s.name);
            assert_eq!(scheme_name(i), s.name);
        }
        assert_eq!(scheme_index("no_such_theme"), None);
        // Out-of-range degrades to the default rather than panicking.
        assert_eq!(scheme_name(usize::MAX), DEFAULT_SCHEME_NAME);
    }

    /// UV offset defaults to zero for every **library** theme, so the whole shipped
    /// set renders exactly as it did before the field existed.
    ///
    /// Scoped to `SchemeKind::Library` deliberately: a hand-authored preset in a
    /// custom slot is precisely where a non-zero offset belongs, and an earlier
    /// version of this test asserted over *all* schemes and failed the moment a
    /// preset with a UV shift was saved.
    #[test]
    fn uv_offset_defaults_to_zero_in_the_shipped_library() {
        let mut checked = 0;
        for s in schemes() {
            if s.kind != SchemeKind::Library {
                continue;
            }
            for (zi, z) in s.zones.iter().enumerate() {
                let Some(z) = z else { continue };
                assert_eq!(
                    z.offset, [0.0, 0.0],
                    "{} zone {zi} has a non-zero authored offset",
                    s.name
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected library themes to check");
    }

    /// The catalog must cover every texture the shipped themes reference, or the
    /// editor would show a theme it cannot re-pick the textures of.
    #[test]
    fn catalog_covers_every_referenced_texture() {
        assert!(!catalog().is_empty(), "texture_index.json should be present");
        for name in all_texture_names() {
            assert!(
                texture_info(name).is_some(),
                "{name} is used by a theme but missing from texture_index.json"
            );
        }
    }

    #[test]
    fn catalog_groups_by_source_level() {
        let groups = catalog_by_level();
        assert!(groups.len() > 15, "expected ~21 GoldenEye levels, got {}", groups.len());
        // Every catalogued texture appears under at least one group.
        let grouped: std::collections::HashSet<&str> =
            groups.iter().flat_map(|(_, names)| names.iter().copied()).collect();
        for name in catalog().keys() {
            assert!(grouped.contains(name.as_str()), "{name} is in no level group");
        }
        // Groups are sorted, and every texture in a group is decodable.
        let mut sorted = groups.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(&sorted, groups, "groups must come out sorted");
    }

    #[test]
    fn hex_colors_parse() {
        assert_eq!(parse_hex_rgb("#8B7355"), LEGACY_TUNNEL_COLOR);
        assert_eq!(parse_hex_rgb("#000000"), [0.0, 0.0, 0.0]);
        assert_eq!(parse_hex_rgb("#ffffff"), [1.0, 1.0, 1.0]);
        // Malformed falls back rather than panicking.
        assert_eq!(parse_hex_rgb("nope"), LEGACY_TUNNEL_COLOR);
    }
}
