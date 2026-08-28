//! **How a hunt starts** — the authored, per-level match setup that `G` reads.
//!
//! Before this existed, pressing `G` dropped you into a hunt built from whatever the
//! session happened to be carrying: the wave size the app pinned at boot, the
//! difficulty the `=`/`-` keys had last landed on, a fallback PP7 if the level authored
//! no pickups, and any env var (`AI=`, `BODIES=`, `SCORE_LIMIT=`) that had been set
//! before launch. That is a debug affordance, not a match setup — none of it was
//! visible, none of it was per-level, and none of it survived a restart.
//!
//! A [`PlayConfig`] is that setup made explicit and **saved in the level file**, so a
//! level ships with the fight it was designed for. It is authored in the PLAY tab of the
//! `O` panel and applied by `World::apply_play_config` at the top of the BUILD→HUNT
//! transition. `G` still works, and still toggles both ways — it just reads this now.
//!
//! ## Why the level file and not a settings file
//!
//! "Six hunters, no starting weapon, loot the floor" is a statement about *this level*
//! in the same way its pickup placement is: a cramped bunker and an open compound want
//! different waves. Storing it globally would mean every level inherited the last one's
//! setup, which is the problem being fixed rather than a fix for it. It rides
//! `LevelFile` as a `#[serde(default)]` block, so v1-v4 files load with
//! [`PlayConfig::default`] — which reproduces the old `G` behaviour exactly — and no
//! format bump is needed.
//!
//! ## What is deliberately *not* in here
//!
//! Anything already authored per-object stays there: a pickup's own respawn timer, a
//! door's access level, a spawn pad's facing. This block is for what the *match* needs
//! and no single object owns.

use serde::{Deserialize, Serialize};

use crate::enemy::AiMode;

use super::{BodySet, Mode, RoundOutcome, World, PLAYER_MAX_ARMOR, PLAYER_MAX_HEALTH};

/// Where the player's body appears at `G`.
///
/// [`EntryMode::Pads`] is the game: the authored spawn pads, drawn through Perfect
/// Dark's shortlist rule (`world::spawn`), falling back to the fly-cam when the level
/// authors none. [`EntryMode::Camera`] pins that fallback on — it is what you want when
/// you are testing one corner of a big level and do not care to walk there, and it is
/// the honest answer for a level with no pads yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum EntryMode {
    /// Authored pads if there are any; under the camera if there are not.
    #[default]
    Pads,
    /// Always under the fly-cam, even on a level with pads. Debug entry.
    Camera,
}

impl EntryMode {
    pub fn label(self) -> &'static str {
        match self {
            EntryMode::Pads => "Spawn pads",
            EntryMode::Camera => "Here (camera)",
        }
    }
}

/// Where the player's guns come from at `G`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum LoadoutMode {
    /// **The level decides.** Weapon pickups on the floor are the armoury; you start
    /// empty-handed and go and find one. A level that authors no weapon pickups hands
    /// over the starting sidearm instead, so it is never unplayable
    /// (`grant_fallback_sidearm`). This is what the game did before this tab existed.
    #[default]
    Level,
    /// **The authored loadout below.** You start carrying exactly [`PlayConfig::weapons`]
    /// with exactly the ammo listed, whatever is on the floor. Pickups still work.
    ///
    /// "Exactly" is literal: the inventory is stripped first, so a gun bought in the shop
    /// or carried out of a previous hunt does not quietly add itself to the list. Leave a
    /// level on `Level` if you want purchases to carry into it.
    Custom,
    /// **Nothing, ever.** Empty-handed with no fallback sidearm, even on a level with no
    /// weapon pickups — a fists-only run, or a level whose armoury is the point. Strips
    /// the inventory, for the same reason `Custom` does.
    Empty,
}

impl LoadoutMode {
    pub fn label(self) -> &'static str {
        match self {
            LoadoutMode::Level => "From the level",
            LoadoutMode::Custom => "Authored loadout",
            LoadoutMode::Empty => "Empty-handed",
        }
    }
}

/// One gun in an authored loadout.
///
/// `weapon` is an arsenal **display name** ("PP7", "Falcon 2") rather than an index,
/// for the same reason texture themes moved to names in level format v4: an index is a
/// position in whatever the live arsenal happens to list, and `ARSENAL=pd` relists it.
/// A name that no longer resolves is dropped with a warning at apply time rather than
/// silently handing over a different gun.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LoadoutSlot {
    pub weapon: String,
    /// Spare magazines in reserve on top of the full one in the gun — the same unit
    /// [`crate::combat::Weapon::stock`] takes, and the same unit a weapon pickup
    /// authors. `0` means one magazine and no refills.
    pub spare_mags: u32,
    /// Whether this is the gun in hand at `G`. The first flagged slot wins; if none is
    /// flagged, the first slot is.
    pub equipped: bool,
}

impl LoadoutSlot {
    pub fn new(weapon: impl Into<String>) -> Self {
        Self { weapon: weapon.into(), spare_mags: DEFAULT_SPARE_MAGS, equipped: false }
    }
}

/// Spare magazines a freshly-added loadout slot carries. Matches
/// [`crate::combat::Weapon::stock_bought`]'s generosity, which is what a starting
/// weapon used to arrive with.
pub const DEFAULT_SPARE_MAGS: u32 = 10;

/// What the hunters bring.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum HunterWeapon {
    /// The tuned roster mix (`super::ENEMY_ROSTER`), cycling: rifle, pistol, dual
    /// rifle, dual pistols, rifle, shotgun — every hunter armed from the moment it
    /// enters. One hunt exercises every animation class.
    Roster,
    /// Every hunter carries this one gun (arsenal display name), single-wielded. For
    /// judging one weapon's behaviour without the roster's variety in the way.
    Fixed(String),
    /// Empty-handed — they loot the floor like you do. Only takes effect on a level
    /// that authors weapon pickups; with nothing to find they keep the roster weapon
    /// rather than standing around unarmed (`hunters_start_unarmed`).
    ///
    /// **The default, because it is what the game already did**: `unarmed_hunters` is
    /// on unless `ARMED_HUNTERS=1` says otherwise, so the player and the hunters have
    /// been playing by the same rule since the pickups pass.
    #[default]
    Loot,
}

impl HunterWeapon {
    pub fn label(&self) -> &str {
        match self {
            HunterWeapon::Roster => "Roster mix",
            HunterWeapon::Fixed(name) => name,
            HunterWeapon::Loot => "Loot the floor",
        }
    }
}

/// **Which fields an explicit override has claimed.**
///
/// The rule this exists to keep, stated once: *an explicit instruction outranks the
/// level's authored config*. `AI=pd` on the command line, `world.set_wave_size(3)` in a
/// test, a press of `I` in a live hunt — each is someone saying what they want right
/// now, and none of them should be silently undone by whatever the open level happens to
/// have saved. So the setters that carry those instructions raise a flag here, and
/// [`World::apply_play_config`] skips every flagged field (and logs that it did).
///
/// It is the same trap the AI-mode port was bitten by, generalised: `enable_pd_lab`
/// once pinned a body set and ate an explicit `BODIES=ge` for a whole playtest. Without
/// these flags, adding a level config would recreate that bug for six fields at once —
/// and, more immediately, would spawn four hunters into every headless test that had
/// carefully asked for none.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct PlayPins {
    /// `set_wave_size` / `set_spawn_enemies` — how many hunters.
    pub wave: bool,
    /// `set_difficulty` — the dial.
    pub difficulty: bool,
    /// `set_ai_mode`, raised only for an explicit `AI=` (the app calls the setter
    /// unconditionally, so pinning on the call alone would mean the config never won).
    pub ai: bool,
    /// `set_body_set` — `BODIES=` / `GE_CLIPS=1`.
    pub bodies: bool,
    /// `set_score_limit` — `SCORE_LIMIT=n`.
    pub score_limit: bool,
    /// A live press of `I` / `N`. Once you have toggled a cheat by hand, the panel's
    /// checkbox stops overriding you at the next `G`.
    pub cheats: bool,
}

/// The authored match setup for one level. Every field has a default that reproduces
/// the pre-PLAY-tab `G` behaviour, so an old level plays identically.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayConfig {
    // ── Entry ────────────────────────────────────────────────────────────────
    pub entry: EntryMode,

    // ── The player ───────────────────────────────────────────────────────────
    pub loadout: LoadoutMode,
    /// The authored guns, used when `loadout == Custom`. Kept across a switch to
    /// another mode so toggling back does not lose the list.
    pub weapons: Vec<LoadoutSlot>,
    /// Starting health (of [`PLAYER_MAX_HEALTH`]). Below max is a handicap run.
    pub health: f32,
    /// Starting armour (of [`PLAYER_MAX_ARMOR`]). 0 is the game's own default.
    pub armor: f32,

    // ── The opposition ───────────────────────────────────────────────────────
    /// How many hunters flood in. **0 spawns none** — an empty level to walk, which is
    /// what the `J` hunters toggle was for.
    pub enemy_count: usize,
    /// The difficulty dial (`0..=DIFFICULTY_MAX`) this level is tuned for.
    pub difficulty: u32,

    // ── AI + bodies ──────────────────────────────────────────────────────────
    /// Which engagement model the hunters use. Was `AI=` at launch only.
    pub ai: AiMode,
    /// Which body family the wave draws from. Was `BODIES=` at launch only.
    pub bodies: BodySet,
    pub hunter_weapon: HunterWeapon,

    // ── Match rules ──────────────────────────────────────────────────────────
    /// Whether either side comes back after dying. Off makes it one life each — the
    /// original hide-and-seek shape rather than a deathmatch.
    pub respawn: bool,
    /// Seconds between dying and respawning (both sides).
    pub respawn_delay: f32,
    /// Kills needed to win. `0` is endless — play until you leave.
    pub score_limit: u32,
    /// Round length in **minutes**. `0` is no limit. On expiry the side that is ahead
    /// wins; a tie goes to the player, who has held out for the full round.
    pub time_limit_min: f32,

    // ── Debug ────────────────────────────────────────────────────────────────
    /// Start invincible (the `I` toggle, pre-set). Still toggleable in-hunt.
    pub invincible: bool,
    /// Start unperceivable (the `N` toggle, pre-set) — the way to watch the AI work.
    pub invisible: bool,
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            entry: EntryMode::default(),
            loadout: LoadoutMode::default(),
            weapons: Vec::new(),
            health: PLAYER_MAX_HEALTH,
            armor: 0.0,
            // The *code* default (duel mode), not the app's boot pack: a headless
            // `World::new` has always spawned one hunter, and a great many tests read
            // that. The app raises it to `PLAYTEST_WAVE_SIZE` by writing this config at
            // boot, which is exactly the boot pin it replaces.
            enemy_count: super::ENEMY_COUNT,
            difficulty: 0,
            ai: AiMode::Ours,
            bodies: BodySet::All,
            hunter_weapon: HunterWeapon::Loot,
            respawn: true,
            respawn_delay: super::RESPAWN_DELAY,
            score_limit: super::SCORE_LIMIT,
            time_limit_min: 0.0,
            invincible: false,
            invisible: false,
        }
    }
}

impl PlayConfig {
    /// Clamp every field into its legal range. Called on load (a hand-edited or
    /// older file can hold anything) and after every panel edit, so nothing
    /// downstream has to re-check.
    pub fn sanitize(&mut self) {
        self.health = self.health.clamp(1.0, PLAYER_MAX_HEALTH);
        self.armor = self.armor.clamp(0.0, PLAYER_MAX_ARMOR);
        self.enemy_count = self.enemy_count.min(super::WAVE_SIZE_MAX);
        self.difficulty = self.difficulty.min(super::DIFFICULTY_MAX);
        self.respawn_delay = self.respawn_delay.clamp(0.0, 30.0);
        self.time_limit_min = self.time_limit_min.clamp(0.0, 60.0);
        // An authored loadout has exactly one gun in hand.
        let first = self.weapons.iter().position(|w| w.equipped).unwrap_or(0);
        for (i, w) in self.weapons.iter_mut().enumerate() {
            w.equipped = i == first;
            w.spare_mags = w.spare_mags.min(99);
        }
    }

    /// The round length in seconds, or `None` when unlimited.
    pub fn time_limit_secs(&self) -> Option<f32> {
        (self.time_limit_min > 0.0).then(|| self.time_limit_min * 60.0)
    }

    /// One line describing the hunt this config starts, for the panel's START button
    /// and the `G` log — so what you are about to get is stated before you get it.
    pub fn summary(&self) -> String {
        let who = match self.enemy_count {
            0 => "no hunters".to_string(),
            1 => "1 hunter".to_string(),
            n => format!("{n} hunters"),
        };
        let guns = match self.loadout {
            LoadoutMode::Level => "level armoury".to_string(),
            LoadoutMode::Empty => "empty-handed".to_string(),
            LoadoutMode::Custom => match self.weapons.len() {
                0 => "empty loadout".to_string(),
                1 => self.weapons[0].weapon.clone(),
                n => format!("{n} guns"),
            },
        };
        let mut s = format!(
            "{who} · difficulty {}/{} · {guns} · {}",
            self.difficulty,
            super::DIFFICULTY_MAX,
            self.entry.label().to_ascii_lowercase(),
        );
        if !self.respawn {
            s.push_str(" · one life");
        }
        if self.invincible || self.invisible {
            s.push_str(" · cheats");
        }
        s
    }
}

// ─── Applying it ──────────────────────────────────────────────────────────────

impl World {
    /// The authored match setup for the open level.
    pub fn play_config(&self) -> &PlayConfig {
        &self.play
    }

    /// Replace the match setup — an **edit**, so it raises the level's unsaved marker
    /// (this is saved data, like the ambient light and the theme hotkeys). Sanitized on
    /// the way in, so nothing downstream re-checks ranges.
    pub fn set_play_config(&mut self, mut cfg: PlayConfig) {
        cfg.sanitize();
        if cfg != self.play {
            self.play = cfg;
            self.bump_revision();
        }
    }

    /// Install a starting config **without** counting as an edit.
    ///
    /// For the app's boot defaults (the wave size and difficulty it used to pin by
    /// calling the setters directly) and for the level loader. Both are establishing
    /// what the level *is*, not changing it, and neither should make a freshly-opened
    /// level look unsaved.
    pub fn set_play_config_default(&mut self, mut cfg: PlayConfig) {
        cfg.sanitize();
        self.play = cfg;
    }

    /// Whether an explicit override has claimed a field (`AI=`, a `set_wave_size` call,
    /// a press of `I`). The panel greys those controls out and says why, rather than
    /// showing a checkbox that silently does nothing.
    pub(crate) fn play_pins(&self) -> PlayPins {
        self.pins
    }

    /// **Apply the authored config to the world, at the top of BUILD→HUNT.**
    ///
    /// Called from `World::toggle_mode` before the nav bake, because the wave size and
    /// the body set have to be right *before* `spawn_wave` reads them. Skips any field an
    /// explicit override has pinned (see [`PlayPins`]) and says which, so a config that
    /// appears to be ignored is never a mystery.
    ///
    /// The player loadout is **not** here: it is applied after the level's pickups have
    /// been stocked, since [`LoadoutMode::Level`] is a question about what the level put
    /// on the floor. See [`World::apply_start_loadout`].
    pub(crate) fn apply_play_config(&mut self) {
        let cfg = self.play.clone();
        let pins = self.pins;

        // ── The opposition. `enemy_count == 0` is the empty level: the wave gate goes
        // off, and `wave_size` is left alone (it is clamped to >= 1 and nothing reads it
        // while the gate is shut).
        if !pins.wave {
            self.spawn_enemies = cfg.enemy_count > 0;
            self.hunters_enabled = cfg.enemy_count > 0;
            if cfg.enemy_count > 0 {
                self.wave_size = cfg.enemy_count.min(super::WAVE_SIZE_MAX);
            }
        }
        if !pins.difficulty {
            // The field, not `set_difficulty`: we are in BUILD, so its duel-restart is a
            // no-op, and its log line would be noise on every G.
            self.difficulty = cfg.difficulty.min(super::DIFFICULTY_MAX);
        }
        if !pins.ai {
            self.set_ai_mode(cfg.ai);
        }
        if !pins.bodies {
            self.set_body_set(cfg.bodies);
        }
        // What the hunters carry. `Fixed` is read by `spawn_wave` off the config itself;
        // only the loot flag is a world field.
        self.unarmed_hunters = matches!(cfg.hunter_weapon, HunterWeapon::Loot);

        // ── The player. Health/armour are set here rather than in `toggle_mode`'s own
        // reset because they are the authored *starting* values, and that reset runs on
        // the way out of HUNT too.
        self.player_health = cfg.health;
        self.player_armor = cfg.armor;
        if !pins.cheats {
            self.player_invulnerable = cfg.invincible;
            self.player_invisible = cfg.invisible;
        }

        // ── Rules.
        if !pins.score_limit {
            self.score_limit = cfg.score_limit;
        }
        self.round_clock = 0.0;

        log::info!("HUNT setup: {}", cfg.summary());
        let overridden: Vec<&str> = [
            (pins.wave, "hunter count"),
            (pins.difficulty, "difficulty"),
            (pins.ai, "AI model"),
            (pins.bodies, "bodies"),
            (pins.score_limit, "score limit"),
            (pins.cheats, "cheats"),
        ]
        .iter()
        .filter(|(p, _)| *p)
        .map(|(_, n)| *n)
        .collect();
        if !overridden.is_empty() {
            log::info!(
                "  (level config overridden for: {} — an explicit override outranks it)",
                overridden.join(", ")
            );
        }
    }

    /// Put the player's starting guns in their hands.
    ///
    /// Runs **after** the level's pickups have been stocked, at BUILD→HUNT and again on
    /// every respawn, so what you come back with matches what you started with. That
    /// second call is new: before this, a respawn stripped you to empty hands and never
    /// re-granted even the fallback sidearm, which on a level with no weapon pickups left
    /// you permanently unarmed.
    /// **Both authored modes start from a clean slate**, and that is a real decision
    /// rather than an implementation detail: `Custom` promises "you start with exactly
    /// these guns" and `Empty` promises "you start with nothing", and neither is true if
    /// whatever the last hunt (or the shop) left you holding is still in your hands. So
    /// they strip first. `LoadoutMode::Level` does not touch the inventory at all, which
    /// is the pre-existing behaviour and the mode to leave a level on if you want shop
    /// purchases to carry into it. (`reset_loadout` exempts `OWN_ALL=1` either way.)
    pub(crate) fn apply_start_loadout(&mut self) {
        match self.play.loadout {
            // What the level authored decides — the pre-existing behaviour.
            LoadoutMode::Level => self.grant_fallback_sidearm(),
            // Deliberately nothing, not even the safety-net sidearm.
            LoadoutMode::Empty => self.reset_loadout(),
            LoadoutMode::Custom => {
                self.reset_loadout();
                self.grant_authored_loadout();
            }
        }
    }

    /// Hand over exactly the authored loadout: each named gun, loaded, with its spare
    /// magazines in reserve, and the flagged one in hand.
    ///
    /// A name the **live** arsenal does not list is skipped with a warning rather than
    /// substituted — `ARSENAL=pd` relists every weapon, and quietly handing over a
    /// different gun than the file names is worse than handing over none. If nothing at
    /// all resolves, the level's own fallback runs so the hunt is still playable;
    /// [`LoadoutMode::Empty`] is how you ask for empty hands on purpose.
    fn grant_authored_loadout(&mut self) {
        let arsenal = self.arsenal.weapons();
        let mut equip: Option<usize> = None;
        let mut granted = 0usize;
        for slot in self.play.weapons.clone() {
            let Some(idx) = arsenal.iter().position(|w| w.name == slot.weapon) else {
                log::warn!(
                    "loadout names {:?}, which this session's arsenal ({}) does not \
                     carry — skipped",
                    slot.weapon,
                    self.arsenal.summary()
                );
                continue;
            };
            self.owned[idx] = true;
            self.weapons[idx].stock(slot.spare_mags);
            granted += 1;
            if slot.equipped || equip.is_none() {
                equip = Some(idx);
            }
        }
        if granted == 0 {
            log::warn!("authored loadout granted nothing — falling back to the level's own");
            self.grant_fallback_sidearm();
            return;
        }
        if let Some(idx) = equip {
            self.equip_weapon(idx);
        }
        log::info!("authored loadout: {granted} gun(s)");
    }

    /// Whether the player enters at an authored pad. `false` pins the fly-cam drop —
    /// either because the level has no pads, or because the author asked for camera
    /// entry to test a specific corner.
    pub(crate) fn entry_uses_pads(&self) -> bool {
        self.play.entry == EntryMode::Pads && self.spawn_pad_count() > 0
    }

    /// Whether either side comes back after dying.
    pub(crate) fn respawn_enabled(&self) -> bool {
        self.play.respawn
    }

    /// Seconds between dying and coming back.
    pub(crate) fn respawn_delay(&self) -> f32 {
        self.play.respawn_delay
    }

    /// The hunter weapon policy for this level, read by `spawn_wave`.
    pub(crate) fn hunter_weapon_policy(&self) -> &HunterWeapon {
        &self.play.hunter_weapon
    }

    /// Advance the round clock and end the round if the authored time limit is up.
    ///
    /// One fixed step, from `fixed_step`'s HUNT arm while the round is live. The clock
    /// runs through the death beat — a deathmatch timer does not stop because you are
    /// waiting to respawn — but not behind the result screen, which `fixed_step` has
    /// already returned from by then.
    ///
    /// On expiry the side that is **ahead on kills** wins; a tie goes to the player, who
    /// has survived the full round. There is no draw: the result screen is a two-state
    /// port of PD's, and inventing a third state for a tie-on-time is not worth a HUD
    /// change nobody asked for.
    pub(crate) fn round_clock_step(&mut self, dt: f32) {
        let Some(limit) = self.play.time_limit_secs() else { return };
        if self.round_over.is_some() {
            return;
        }
        self.round_clock += dt;
        if self.round_clock < limit {
            return;
        }
        let mine = self.player_score.kills;
        let theirs = self.hunter_side_score().kills;
        self.round_over = Some(if mine >= theirs {
            RoundOutcome::PlayerWins
        } else {
            RoundOutcome::HuntersWin
        });
        log::info!(
            "TIME — {limit:.0}s up, you {mine} : hunters {theirs} → {:?}",
            self.round_over.unwrap()
        );
    }

    /// Seconds left in the round, or `None` when no limit is authored (or outside HUNT).
    /// For the HUD clock and the panel readout.
    pub fn round_time_left(&self) -> Option<f32> {
        if self.mode != Mode::Hunt {
            return None;
        }
        self.play.time_limit_secs().map(|l| (l - self.round_clock).max(0.0))
    }

    /// End the round because a side has been wiped out and nothing is coming back.
    ///
    /// Only reachable with respawning off, which is the one-life shape: the player dying
    /// hands it to the hunters, and the last hunter falling hands it to the player.
    /// Without this a one-life round would simply freeze — a dead player with no respawn
    /// clock and no result screen.
    pub(crate) fn check_wipeout(&mut self) {
        if self.play.respawn || self.round_over.is_some() {
            return;
        }
        if self.player_dead {
            self.round_over = Some(RoundOutcome::HuntersWin);
        } else if !self.enemies.is_empty() && self.enemies.iter().all(|e| e.enemy.is_dead()) {
            self.round_over = Some(RoundOutcome::PlayerWins);
        }
        if let Some(o) = self.round_over {
            log::info!("ROUND OVER — {o:?} (one life each)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default has to reproduce the old `G`: the boot wave size, difficulty 0, the
    /// level's own armoury, respawning on. A drift here silently re-tunes every level
    /// that never opened the PLAY tab.
    #[test]
    fn the_default_is_the_old_g_behaviour() {
        let c = PlayConfig::default();
        assert_eq!(c.enemy_count, crate::world::ENEMY_COUNT);
        assert_eq!(c.difficulty, 0);
        assert_eq!(c.hunter_weapon, HunterWeapon::Loot);
        assert_eq!(c.score_limit, crate::world::SCORE_LIMIT);
        assert_eq!(c.loadout, LoadoutMode::Level);
        assert_eq!(c.entry, EntryMode::Pads);
        assert!(c.respawn);
        assert!(!c.invincible && !c.invisible);
        assert_eq!(c.time_limit_secs(), None, "no time limit by default");
    }

    #[test]
    fn sanitize_clamps_out_of_range_fields() {
        let mut c = PlayConfig {
            health: 9_999.0,
            armor: -3.0,
            enemy_count: 500,
            difficulty: 99,
            respawn_delay: -1.0,
            time_limit_min: 1_000.0,
            ..PlayConfig::default()
        };
        c.sanitize();
        assert_eq!(c.health, PLAYER_MAX_HEALTH);
        assert_eq!(c.armor, 0.0);
        assert_eq!(c.enemy_count, crate::world::WAVE_SIZE_MAX);
        assert_eq!(c.difficulty, crate::world::DIFFICULTY_MAX);
        assert_eq!(c.respawn_delay, 0.0);
        assert_eq!(c.time_limit_min, 60.0);
    }

    /// Exactly one gun is in hand, whatever the panel wrote — two flagged slots is the
    /// state a checkbox list drifts into.
    #[test]
    fn sanitize_leaves_exactly_one_equipped_gun() {
        let mut c = PlayConfig {
            loadout: LoadoutMode::Custom,
            weapons: vec![
                LoadoutSlot { weapon: "PP7".into(), spare_mags: 2, equipped: false },
                LoadoutSlot { weapon: "KF7 Soviet".into(), spare_mags: 4, equipped: true },
                LoadoutSlot { weapon: "Shotgun".into(), spare_mags: 200, equipped: true },
            ],
            ..PlayConfig::default()
        };
        c.sanitize();
        let armed: Vec<usize> =
            c.weapons.iter().enumerate().filter(|(_, w)| w.equipped).map(|(i, _)| i).collect();
        assert_eq!(armed, vec![1], "the first flagged slot keeps the hands");
        assert_eq!(c.weapons[2].spare_mags, 99, "reserve is capped");
    }

    /// An empty list must not panic, and must not invent an equipped index.
    #[test]
    fn sanitize_survives_an_empty_loadout() {
        let mut c = PlayConfig { loadout: LoadoutMode::Custom, ..PlayConfig::default() };
        c.sanitize();
        assert!(c.weapons.is_empty());
    }

    #[test]
    fn a_time_limit_converts_to_seconds() {
        let c = PlayConfig { time_limit_min: 2.5, ..PlayConfig::default() };
        assert_eq!(c.time_limit_secs(), Some(150.0));
    }

    /// The whole point of the file-backed config: it round-trips, and an old file with
    /// no `play` block at all deserializes to the default.
    #[test]
    fn it_round_trips_through_json() {
        let mut c = PlayConfig {
            entry: EntryMode::Camera,
            loadout: LoadoutMode::Custom,
            weapons: vec![LoadoutSlot::new("PP7")],
            enemy_count: 6,
            difficulty: 7,
            ai: AiMode::Pd,
            bodies: BodySet::PerfectDark,
            hunter_weapon: HunterWeapon::Fixed("Shotgun".into()),
            respawn: false,
            time_limit_min: 5.0,
            invincible: true,
            ..PlayConfig::default()
        };
        c.sanitize();
        let json = serde_json::to_string(&c).unwrap();
        let back: PlayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        let empty: PlayConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, PlayConfig::default(), "a file with no block gets the default");
    }

    // ─── What the config actually does at G ───────────────────────────────────
    //
    // Everything above tests the struct; these test the transition. They go through
    // `toggle_mode` on a real room rather than calling `apply_play_config` directly,
    // because the interesting failures are all about *ordering* — a field read before it
    // was set — and a direct call cannot see those.

    use crate::world::tools::spawn_point::tests::{big_room, place_pad};
    use crate::world::scoreboard::Score;
    use crate::world::Killer;
    use engine::platform::input::InputState;
    use glam::Vec3;

    /// A 40 m room with three pads and the fly-cam parked away from all of them, so
    /// "entered at a pad" and "entered under the camera" are distinguishable.
    const CAM: Vec3 = Vec3::new(20.0, 2.0, 20.0);
    fn room_with_pads() -> World {
        let mut world = big_room(40.0);
        for p in [
            Vec3::new(6.0, 0.0, 6.0),
            Vec3::new(34.0, 0.0, 6.0),
            Vec3::new(20.0, 0.0, 34.0),
        ] {
            place_pad(&mut world, p, 0.0);
        }
        world.camera.pos = CAM;
        world
    }

    /// Author a config without going through `set_play_config`'s revision bump, so the
    /// tests below read as "this level ships with X".
    fn author(world: &mut World, f: impl FnOnce(&mut PlayConfig)) {
        let mut cfg = world.play_config().clone();
        f(&mut cfg);
        world.set_play_config_default(cfg);
    }

    fn step(world: &mut World, secs: f32) {
        let dt = 1.0 / 60.0;
        let input = InputState::default();
        for _ in 0..(secs / dt).ceil() as usize {
            world.fixed_step(dt, &input);
        }
    }

    /// **The debug entry the tab was asked for.** With pads authored, `EntryMode::Camera`
    /// still drops the player under the fly-cam — which is how you test one corner of a
    /// big level without walking there.
    #[test]
    fn camera_entry_ignores_authored_pads() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.entry = EntryMode::Camera;
            c.enemy_count = 0;
        });
        world.toggle_mode();
        let at = world.player_pos().expect("in HUNT");
        assert!(
            (at.x - CAM.x).abs() < 0.5 && (at.z - CAM.z).abs() < 0.5,
            "camera entry put the player at {at:?}, not under the camera at {CAM:?}",
        );

        // …and pad entry on the same level does the opposite, which is what makes the
        // assertion above about the mode rather than about this room.
        world.toggle_mode();
        author(&mut world, |c| c.entry = EntryMode::Pads);
        world.toggle_mode();
        let at = world.player_pos().expect("in HUNT");
        assert!(
            (at.x - CAM.x).abs() > 1.0 || (at.z - CAM.z).abs() > 1.0,
            "pad entry should not put the player under the camera",
        );
    }

    /// An authored loadout arrives **loaded and in hand**: the flagged gun equipped, its
    /// magazine full, and the authored spare magazines in reserve.
    #[test]
    fn an_authored_loadout_arrives_loaded_and_in_hand() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.loadout = LoadoutMode::Custom;
            c.weapons = vec![
                LoadoutSlot { weapon: "PP7".into(), spare_mags: 1, equipped: false },
                LoadoutSlot { weapon: "KF7 Soviet".into(), spare_mags: 3, equipped: true },
            ];
        });
        world.toggle_mode();

        assert_eq!(world.weapon().config().name, "KF7 Soviet", "the flagged gun is in hand");
        let kf7 = crate::world::tools::pickup::tests::weapon_idx(&world, "KF7 Soviet");
        let pp7 = crate::world::tools::pickup::tests::weapon_idx(&world, "PP7");
        assert!(world.owned[kf7] && world.owned[pp7], "both authored guns are carried");
        let cap = world.weapons[kf7].config().magazine_size;
        assert_eq!(world.weapons[kf7].magazine(), cap, "the gun in hand is loaded");
        assert_eq!(
            world.weapons[kf7].reserve(),
            cap * 3,
            "three spare magazines, as authored"
        );
    }

    /// A gun this session's arsenal does not carry is **skipped, not substituted** — and
    /// a loadout where nothing resolves falls back to the level's own armoury rather than
    /// leaving the hunt unplayable. `LoadoutMode::Empty` is how you ask for empty hands.
    #[test]
    fn an_unknown_gun_is_skipped_and_a_dead_loadout_falls_back() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.loadout = LoadoutMode::Custom;
            c.weapons = vec![
                LoadoutSlot { weapon: "Laser Bazooka".into(), spare_mags: 1, equipped: true },
                LoadoutSlot { weapon: "PP7".into(), spare_mags: 0, equipped: false },
            ];
        });
        world.toggle_mode();
        assert_eq!(world.weapon().config().name, "PP7", "the one gun that resolved");

        // Nothing resolves → the level's own fallback, so the hunt is still playable.
        world.toggle_mode();
        author(&mut world, |c| {
            c.weapons = vec![LoadoutSlot { weapon: "Laser Bazooka".into(), spare_mags: 1, equipped: true }];
        });
        world.toggle_mode();
        assert!(
            !world.weapon().config().is_unarmed(),
            "a loadout that granted nothing should fall back, not leave empty hands"
        );
    }

    /// `LoadoutMode::Empty` means empty **even on a level with no guns to find**, which is
    /// the one case the safety-net sidearm exists for. Asserted against `Level` mode on the
    /// same room, so it is about the mode and not about the room being bare.
    #[test]
    fn empty_handed_mode_overrides_the_fallback_sidearm() {
        let mut world = room_with_pads();
        author(&mut world, |c| c.enemy_count = 0);
        world.toggle_mode();
        assert!(
            !world.weapon().config().is_unarmed(),
            "a bare level hands over the fallback sidearm"
        );
        world.toggle_mode();

        author(&mut world, |c| c.loadout = LoadoutMode::Empty);
        world.toggle_mode();
        assert!(
            world.weapon().config().is_unarmed(),
            "empty-handed mode means empty, fallback included"
        );
    }

    /// The authored loadout is what you come back with. Before this, a respawn stripped
    /// you and re-granted nothing, so "start with a KF7" meant "for one life".
    #[test]
    fn the_authored_loadout_comes_back_on_respawn() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.score_limit = 0; // endless, so dying can't end the round instead
            c.loadout = LoadoutMode::Custom;
            c.weapons = vec![LoadoutSlot { weapon: "KF7 Soviet".into(), spare_mags: 2, equipped: true }];
        });
        world.toggle_mode();
        assert_eq!(world.weapon().config().name, "KF7 Soviet");

        let delay = world.play_config().respawn_delay;
        world.kill_player(Killer::Unattributed);
        step(&mut world, delay + 0.2);
        assert!(!world.is_player_dead(), "back on their feet");
        assert_eq!(
            world.weapon().config().name, "KF7 Soviet",
            "the authored loadout is the starting condition, every life"
        );
    }

    /// `enemy_count = 0` is the empty level — the `J` toggle, authored.
    #[test]
    fn zero_hunters_spawns_an_empty_level() {
        let mut world = room_with_pads();
        author(&mut world, |c| c.enemy_count = 0);
        world.toggle_mode();
        assert!(world.enemies.is_empty(), "nobody hunting");
    }

    /// One life each: the player dying is the round, with no respawn clock left running
    /// behind a frozen sim. This is the failure mode the wipeout check exists for.
    #[test]
    fn one_life_each_ends_the_round_when_the_player_dies() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.respawn = false;
        });
        world.toggle_mode();
        world.kill_player(Killer::Unattributed);
        assert_eq!(
            world.round_outcome(),
            Some(RoundOutcome::HuntersWin),
            "a one-life death ends the round"
        );
        step(&mut world, 5.0);
        assert!(world.is_player_dead(), "and nothing brings the player back");
    }

    /// The authored time limit ends the round, and the side ahead on kills wins. A tie
    /// goes to the player — stated in `round_clock_step`, asserted here.
    #[test]
    fn the_time_limit_ends_the_round_and_the_lead_wins() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.time_limit_min = 2.0 / 60.0; // two seconds
        });
        world.toggle_mode();
        world.hunter_scores = vec![Score { kills: 3, deaths: 0 }];

        step(&mut world, 1.0);
        assert!(world.round_outcome().is_none(), "still inside the limit");
        step(&mut world, 1.5);
        assert_eq!(
            world.round_outcome(),
            Some(RoundOutcome::HuntersWin),
            "time up with the hunters ahead"
        );

        // Level on kills → the player, for having lasted the round.
        world.toggle_mode();
        world.toggle_mode();
        world.hunter_scores = vec![Score { kills: 2, deaths: 0 }];
        world.player_score = Score { kills: 2, deaths: 0 };
        step(&mut world, 2.5);
        assert_eq!(
            world.round_outcome(),
            Some(RoundOutcome::PlayerWins),
            "a tie on time goes to the player"
        );
    }

    /// No limit authored → the clock never ends anything, however long the round runs.
    #[test]
    fn no_time_limit_never_ends_the_round() {
        let mut world = room_with_pads();
        author(&mut world, |c| c.enemy_count = 0);
        world.toggle_mode();
        step(&mut world, 20.0);
        assert!(world.round_outcome().is_none(), "an unlimited round keeps going");
    }

    /// **The rule the pins exist for.** An explicit override — a `set_score_limit` call
    /// here, an `AI=pd` launch or a press of `I` in the real game — outranks whatever the
    /// open level authored, and applying the config does not quietly undo it.
    #[test]
    fn an_explicit_override_outranks_the_level_config() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 0;
            c.score_limit = 7;
            c.difficulty = 9;
            c.invincible = true;
        });
        // Three explicit overrides, of the three kinds: a launch/test setter, a live key,
        // and a dial nudge.
        world.set_score_limit(0);
        world.toggle_invulnerable();
        world.set_difficulty(2);

        world.toggle_mode();
        assert_eq!(world.score_limit(), 0, "the explicit score limit stands");
        assert_eq!(world.difficulty(), 2, "the explicit difficulty stands");
        assert!(
            world.is_invulnerable(),
            "a cheat toggled by hand is not un-toggled by the config"
        );

        // …and an untouched field still comes from the config, so the pins are per-field
        // rather than an all-or-nothing switch.
        assert!(world.enemies.is_empty(), "the authored wave size still applies");
    }

    /// `HunterWeapon::Fixed` arms the whole pack with one gun, replacing the roster mix.
    #[test]
    fn a_fixed_hunter_weapon_arms_the_whole_pack_with_one_gun() {
        let mut world = room_with_pads();
        author(&mut world, |c| {
            c.enemy_count = 3;
            c.hunter_weapon = HunterWeapon::Fixed("Shotgun".into());
        });
        world.toggle_mode();
        assert_eq!(world.enemies.len(), 3, "the authored wave spawned");
        for (i, inst) in world.enemies.iter().enumerate() {
            assert_eq!(
                inst.weapon.name, "Shotgun",
                "hunter {i} carries {} rather than the fixed gun",
                inst.weapon.name
            );
        }
    }

    /// The config rides the level file, and — the load-bearing half — a file that predates
    /// the PLAY tab leaves the session's config **alone** rather than resetting it. That is
    /// why the field is an `Option`: defaulting it would silently re-tune every level that
    /// has ever been saved.
    #[test]
    fn the_config_survives_a_save_load_and_an_old_file_does_not_reset_it() {
        let mut world = big_room(40.0);
        author(&mut world, |c| {
            c.enemy_count = 5;
            c.entry = EntryMode::Camera;
            c.loadout = LoadoutMode::Custom;
            c.weapons = vec![LoadoutSlot::new("PP7")];
            c.time_limit_min = 4.0;
            c.respawn = false;
        });
        let saved = world.play_config().clone();
        let path = std::env::temp_dir().join("bah_play_config_roundtrip.json");
        world.save_level(&path).expect("save");

        let mut fresh = big_room(40.0);
        fresh.load_level(&path).expect("load");
        assert_eq!(fresh.play_config(), &saved, "the authored setup came back");

        // Now strip the block, the way every pre-existing level file looks.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
        json.as_object_mut().unwrap().remove("play");
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();

        let mut old = big_room(40.0);
        author(&mut old, |c| c.enemy_count = 6); // the session's own setup
        old.load_level(&path).expect("load");
        assert_eq!(
            old.play_config().enemy_count, 6,
            "a file that says nothing about the match must not reset the session's setup"
        );
        let _ = std::fs::remove_file(&path);
    }
}
