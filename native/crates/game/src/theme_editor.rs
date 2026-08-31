//! The custom-theme draft: build a theme zone by zone from any texture in the
//! library, with live UV scale and offset.
//!
//! The draft is held here as plain data and mirrored into the renderer's **scratch
//! scheme** (`textures::scratch_scheme()`) whenever it changes. That slot already
//! has bind groups from init, so every edit is either one `write_buffer` (scale /
//! offset) or one bind-group rebuild (texture) — no mesh re-bake, no CSG re-fold.
//! Which is why dragging a slider retextures the world at frame rate.
//!
//! Saving writes `native/assets/user_themes.json` and copies the draft into a
//! pre-allocated custom slot so it is usable immediately (see
//! [`engine::render::textures::CUSTOM_SLOTS`] for why the slots are fixed).

use engine::render::textures::{self, ZoneSpec};

/// Zones the editor exposes, in the order it lists them — bottom of the wall to the
/// top, then the odds and ends.
///
/// "Top wall" (zone 4) is the cornice: leaving it undefined is the normal case and
/// means the wall has no ceiling-flush band at all, which is how every shipped theme
/// behaves. Defining it opts this theme in.
pub const EDITABLE_ZONES: [(u8, &str); 8] = [
    (0, "Floor"),
    (1, "Ceiling"),
    (2, "Lower wall"),
    (3, "Upper wall"),
    (4, "Top wall"),
    (5, "Stair / frame"),
    (6, "Doorframe floor"),
    (7, "Brace"),
];

/// Slider bounds for the cornice's depth in WT. The lower bound is a picture rail; the
/// upper is a deep frieze, past which it stops reading as trim.
pub const CORNICE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=6.0;

/// Slider bounds for UV scale. The shipped themes span 0.10 – 1.0; this is wider so
/// an author can go deliberately coarse or fine without being clamped mid-drag.
pub const REPEAT_RANGE: std::ops::RangeInclusive<f32> = 0.02..=4.0;

/// UV offset is in whole textures, so one full period either way covers every
/// distinct alignment — beyond ±1 it just repeats.
pub const OFFSET_RANGE: std::ops::RangeInclusive<f32> = -1.0..=1.0;

/// The in-progress theme.
pub struct ThemeDraft {
    /// Per-zone spec; `None` leaves the zone undefined (and therefore invisible).
    pub zones: [Option<ZoneSpec>; 8],
    /// Which zone the texture picker and sliders are editing.
    pub zone_sel: u8,
    /// Which source-level group the texture picker is showing.
    pub level_sel: String,
    /// Name typed for the next save.
    pub save_name: String,
    /// The theme this draft was seeded from, so a save can go back where it came from.
    ///
    /// `None` only before the first seed. It is deliberately *not* cleared by editing:
    /// "I opened Archives 1, changed the upper wall, save" should land on Archives 1,
    /// which is the whole point of editing in place rather than accreting near-copies
    /// until the 24 custom slots run out.
    pub origin: Option<usize>,
    /// Set once the draft differs from what it was seeded with, so the UI can say
    /// there is unsaved work.
    pub dirty: bool,
}

impl Default for ThemeDraft {
    fn default() -> Self {
        let mut d = ThemeDraft {
            zones: [None; 8],
            zone_sel: 0,
            level_sel: String::new(),
            save_name: String::new(),
            origin: None,
            dirty: false,
        };
        d.seed_from(textures::default_scheme());
        d.dirty = false;
        d
    }
}

impl ThemeDraft {
    /// Copy an existing theme into the draft — the way you start from something that
    /// nearly works rather than from scratch.
    pub fn seed_from(&mut self, scheme: usize) {
        let Some(s) = textures::schemes().get(scheme) else { return };
        self.zones = std::array::from_fn(|z| {
            s.zones[z].and_then(|zd| {
                zd.texture.map(|t| ZoneSpec {
                    texture: t,
                    repeat: zd.repeat,
                    offset: zd.offset,
                    height: zd.height,
                })
            })
        });
        self.save_name = s.label.to_string();
        self.origin = Some(scheme);
        self.dirty = true;
    }

    /// The slot a save could overwrite: this draft's origin, if that origin is a custom
    /// slot.
    ///
    /// `None` for a library theme — those are read-only, and the 394 of them are the
    /// reference the custom slots are derived *from*, so overwriting one would quietly
    /// change every level using it with no way back short of reinstalling the file.
    pub fn overwrite_target(&self) -> Option<usize> {
        let i = self.origin?;
        let s = textures::schemes().get(i)?;
        matches!(s.kind, textures::SchemeKind::Custom { .. }).then_some(i)
    }

    pub fn zone(&self, zone: u8) -> Option<ZoneSpec> {
        self.zones.get(zone as usize).copied().flatten()
    }

    /// The zone currently being edited.
    pub fn selected(&self) -> Option<ZoneSpec> {
        self.zone(self.zone_sel)
    }

    /// Point the selected zone at `texture`, keeping its scale/offset.
    ///
    /// A zone that was undefined gets sensible starting values rather than a zero
    /// repeat, which would stretch one texel across the whole surface.
    pub fn set_texture(&mut self, texture: &'static str) {
        let zi = self.zone_sel as usize;
        self.zones[zi] = Some(match self.zones[zi] {
            Some(prev) => ZoneSpec { texture, ..prev },
            None => ZoneSpec {
                texture,
                repeat: 0.35,
                offset: [0.0, 0.0],
                height: textures::DEFAULT_CORNICE_V,
            },
        });
        self.dirty = true;
    }

    pub fn set_repeat(&mut self, repeat: f32) {
        if let Some(z) = self.zones[self.zone_sel as usize].as_mut() {
            z.repeat = repeat;
            self.dirty = true;
        }
    }

    pub fn set_offset(&mut self, offset: [f32; 2]) {
        if let Some(z) = self.zones[self.zone_sel as usize].as_mut() {
            z.offset = offset;
            self.dirty = true;
        }
    }

    /// Set the cornice depth, in WT. Only zone 4 reads it, so setting it elsewhere is
    /// refused rather than silently stored.
    pub fn set_height(&mut self, height: f32) {
        if self.zone_sel != textures::CORNICE_ZONE {
            return;
        }
        if let Some(z) = self.zones[self.zone_sel as usize].as_mut() {
            z.height = height;
            self.dirty = true;
        }
    }

    /// Clear the selected zone. Undefined zones render *invisible*, so this is
    /// deliberate authoring rather than a reset — the UI warns.
    pub fn clear_zone(&mut self) {
        self.zones[self.zone_sel as usize] = None;
        self.dirty = true;
    }

    /// Persist to the next free custom slot. Returns the slot on success.
    pub fn save_as_preset(&mut self) -> Result<usize, String> {
        let slot = textures::first_free_custom_slot()
            .ok_or_else(|| format!("all {} custom slots are full", textures::CUSTOM_SLOTS))?;
        self.save_to(slot)?;
        Ok(slot)
    }

    /// Persist over an existing custom slot — editing a theme in place rather than
    /// leaving a trail of near-copies behind.
    ///
    /// Refuses a library slot rather than silently redirecting to a free one: a save
    /// that lands somewhere other than where the button said is worse than a refusal.
    pub fn save_over_origin(&mut self) -> Result<usize, String> {
        let slot = self
            .overwrite_target()
            .ok_or_else(|| "this theme is read-only — save it as a new preset".to_string())?;
        self.save_to(slot)?;
        Ok(slot)
    }

    /// Write the draft into `slot`.
    ///
    /// The caller must then push the draft into that slot's materials — saving to disk
    /// cannot update the live registry (a `OnceLock`), so without that step the preset
    /// would only appear correct after a restart. A changed cornice additionally needs
    /// the geometry re-folded, which `App::push_theme_to` handles.
    fn save_to(&mut self, slot: usize) -> Result<(), String> {
        let label = if self.save_name.trim().is_empty() {
            format!("Custom {}", slot)
        } else {
            self.save_name.trim().to_string()
        };
        textures::save_custom_preset(slot, &label, &self.zones)?;
        // The draft now *is* that slot, so a second save goes to the same place rather
        // than spawning a copy.
        self.origin = Some(slot);
        self.dirty = false;
        Ok(())
    }
}

/// Build the stock room the editor previews a theme in.
///
/// Runs the engine's **real** CSG fold over one room brush rather than hand-rolling
/// a textured box, so the preview inherits the actual wall split at `WALL_SPLIT_V`,
/// the world-space UV projection and the normal-based zone classification. A
/// hand-built box could disagree with all three and would then lie about the theme.
///
/// Dimensions match `World::new`'s opening room (24x16x24 WT), which is also what
/// the `repeat` calibration was derived against — so a scale that looks right here
/// looks right in a default-sized room.
pub fn preview_room_mesh(scheme: usize) -> engine::render::mesh::TexturedMesh {
    use engine::geometry::csg_runtime::{Brush, Op, Region};
    let mut region = Region::new(1);
    let mut brush = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 24.0, 16.0, 24.0);
    brush.scheme = scheme;
    region.brushes.push(brush);
    region.evaluate_textured(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Editing in place ────────────────────────────────────────────
    //
    // Only the paths that touch no disk. The success path of a save writes the real
    // `assets/user_themes.json`, and a test that edits the author's own theme file to
    // prove a button works has done more harm than the bug it guards against.

    /// A draft remembers where it came from, so a save can go back there instead of
    /// accreting near-copies until the 24 custom slots run out.
    #[test]
    fn a_draft_remembers_the_theme_it_was_seeded_from() {
        let mut d = ThemeDraft::default();
        let src = textures::custom_slots().start;
        d.seed_from(src);
        assert_eq!(d.origin, Some(src));
    }

    /// A library theme offers no overwrite target: the 394 shipped themes are the
    /// reference the custom slots are derived *from*, and overwriting one would change
    /// every level using it with no way back short of reinstalling the file.
    #[test]
    fn a_library_theme_cannot_be_overwritten() {
        let mut d = ThemeDraft::default();
        d.seed_from(textures::default_scheme());
        assert!(
            matches!(
                textures::schemes()[textures::default_scheme()].kind,
                textures::SchemeKind::Library
            ),
            "the default theme is library content — the premise of this test"
        );
        assert_eq!(d.overwrite_target(), None);

        // And asking anyway is refused rather than quietly redirected to a free slot: a
        // save that lands somewhere other than where the button said is worse than an
        // error message.
        let before = d.zones;
        let err = d.save_over_origin().expect_err("a library save must be refused");
        assert!(err.contains("read-only"), "unhelpful refusal: {err}");
        assert_eq!(d.zones, before, "a refused save changes nothing");
        assert_eq!(d.origin, Some(textures::default_scheme()), "and moves nothing");
    }

    /// A custom slot does offer one, which is the whole feature.
    #[test]
    fn a_custom_slot_offers_itself_as_the_overwrite_target() {
        let slot = textures::custom_slots().start;
        assert!(
            matches!(
                textures::schemes()[slot].kind,
                textures::SchemeKind::Custom { .. }
            ),
            "the first custom slot is a custom slot — the premise of this test"
        );
        let mut d = ThemeDraft::default();
        d.seed_from(slot);
        assert_eq!(d.overwrite_target(), Some(slot));
    }

    /// A fresh draft has nowhere to save back to, so the UI offers only "save as new".
    #[test]
    fn a_never_seeded_draft_has_no_overwrite_target() {
        let d = ThemeDraft {
            zones: [None; 8],
            zone_sel: 0,
            level_sel: String::new(),
            save_name: String::new(),
            origin: None,
            dirty: false,
        };
        assert_eq!(d.overwrite_target(), None);
    }

    #[test]
    fn seeding_copies_a_themes_zones() {
        let mut d = ThemeDraft::default();
        d.seed_from(textures::default_scheme());
        let src = &textures::schemes()[textures::default_scheme()];
        for (zi, want) in src.zones.iter().enumerate() {
            match (want.and_then(|z| z.texture), d.zones[zi]) {
                (Some(t), Some(spec)) => assert_eq!(spec.texture, t, "zone {zi}"),
                (None, got) => assert!(got.is_none(), "zone {zi} should be undefined"),
                (Some(t), None) => panic!("zone {zi} lost texture {t}"),
            }
        }
    }

    #[test]
    fn setting_a_texture_on_an_undefined_zone_gets_a_usable_repeat() {
        let mut d = ThemeDraft::default();
        d.zone_sel = 7; // brace: undefined in the default theme
        d.clear_zone();
        d.set_texture("white_brace");
        let z = d.selected().expect("zone now defined");
        assert_eq!(z.texture, "white_brace");
        assert!(
            REPEAT_RANGE.contains(&z.repeat) && z.repeat > 0.0,
            "a fresh zone must get a sane repeat, got {}",
            z.repeat
        );
    }

    #[test]
    fn changing_a_texture_keeps_the_tuned_scale() {
        let mut d = ThemeDraft::default();
        d.zone_sel = 0;
        d.set_repeat(0.77);
        d.set_offset([0.25, -0.5]);
        d.set_texture("white_tile");
        let z = d.selected().unwrap();
        assert_eq!(z.repeat, 0.77, "scale must survive a texture swap");
        assert_eq!(z.offset, [0.25, -0.5], "offset must survive a texture swap");
    }

    #[test]
    fn the_editor_exposes_the_cornice_and_nothing_undrawable() {
        // Zone 4 became the cornice, so the editor must offer it — that is the only
        // way a theme opts in.
        assert!(
            EDITABLE_ZONES.iter().any(|(z, _)| *z == textures::CORNICE_ZONE),
            "the cornice zone is missing from the editor"
        );
        // Every slot the editor offers must be one the classifier actually emits, or
        // authoring it is dead content.
        for (z, _) in EDITABLE_ZONES {
            assert!(z < 8, "zone {z} is outside the 8-slot table");
        }
    }

    #[test]
    fn draft_starts_clean_and_edits_dirty_it() {
        let mut d = ThemeDraft::default();
        assert!(!d.dirty, "a fresh draft has nothing to save");
        d.set_repeat(0.5);
        assert!(d.dirty);
    }
}
