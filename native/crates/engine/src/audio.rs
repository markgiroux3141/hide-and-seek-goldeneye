//! Audio subsystem — one-shot SFX (weapon fire/reload/empty, and later doors,
//! enemy pain/fire, the player hit) plus a looping background music track.
//!
//! A near-literal port of the JS `src/audio/AudioManager.ts` (the read-only
//! oracle): `load(name)` decodes + caches a sound, `play(name, volume)` fires a
//! fresh voice each call (so overlapping shots each get their own instance), and
//! background music is a separate looping track. Backed by [`kira`], which gives
//! us per-instance volume, looping, and mixing for free and decodes both the WAV
//! SFX and the MP3 music via symphonia.
//!
//! Assets live under `native/assets/audio/` (the runtime-dir convention shared
//! with the enemy GLBs / weapon meshes), addressed by asset-relative name, e.g.
//! `"sounds/weapons/pp7-fire.wav"` or `"music/102 Facility.mp3"`.
//!
//! Everything here is best-effort: a failed device init or a missing/undecodable
//! file logs a warning and no-ops rather than panicking, so the game still runs
//! silently on a box with no audio output. Headless tests never construct this —
//! the *decisions* (which cue a weapon requests) are tested on the caller side.

use std::collections::HashMap;
use std::path::PathBuf;

use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::{AudioManager as KiraManager, AudioManagerSettings, DefaultBackend, Decibels, Tween};

/// Convert a linear amplitude gain (the JS `GainNode` model: 1.0 = unchanged,
/// 0.5 = half) to the decibels kira wants: `dB = 20·log10(amplitude)`. Clamped
/// away from zero so amplitude 0 can't map to −∞ dB / NaN.
fn amplitude_to_db(amplitude: f32) -> Decibels {
    Decibels(20.0 * amplitude.max(1e-4).log10())
}

/// Owns the kira mixer, the cache of decoded one-shot sounds, and the handle to
/// the currently-playing background music. Mirrors the JS `AudioManager` shape.
pub struct AudioManager {
    /// The kira mixer/output. All playback goes through here.
    manager: KiraManager<DefaultBackend>,
    /// Decoded one-shot sounds by asset-relative name (JS `buffers` map). Cheap to
    /// clone per play — the sample data is `Arc`-shared inside `StaticSoundData`.
    sounds: HashMap<String, StaticSoundData>,
    /// Handle to the looping background track, kept alive so it keeps playing and
    /// can be stopped/replaced later (mode/level changes — not used this pass).
    music: Option<StaticSoundHandle>,
    /// `native/assets/audio/`, resolved once from the engine crate's manifest dir.
    root: PathBuf,
}

impl AudioManager {
    /// Initialize the audio device. Returns `None` (with a warning) if no output
    /// device is available — the game then runs silently. Call once at startup.
    pub fn new() -> Option<Self> {
        let manager = match KiraManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("audio: device init failed, running silent: {e}");
                return None;
            }
        };
        // engine crate lives at native/crates/engine → ../../assets/audio.
        let root = PathBuf::from(format!(
            "{}/../../assets/audio",
            env!("CARGO_MANIFEST_DIR")
        ));
        log::info!("audio: initialized (assets at {})", root.display());
        Some(Self {
            manager,
            sounds: HashMap::new(),
            music: None,
            root,
        })
    }

    /// Absolute path to an asset-relative sound name.
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Decode + cache a one-shot sound by asset-relative name. Idempotent (mirrors
    /// JS `loadSound` skipping already-cached URLs). A decode failure warns and
    /// leaves the name uncached so [`Self::play`] simply no-ops for it. Preload the
    /// sounds a scene needs here to avoid a first-play decode hitch.
    pub fn load(&mut self, name: &str) {
        if self.sounds.contains_key(name) {
            return;
        }
        match StaticSoundData::from_file(self.path(name)) {
            Ok(data) => {
                self.sounds.insert(name.to_string(), data);
            }
            Err(e) => log::warn!("audio: load '{name}' failed: {e}"),
        }
    }

    /// Play a one-shot at `volume` (a linear amplitude gain, matching the JS
    /// `GainNode.gain.value` — 1.0 = unchanged, 0.5 = half). Each call spawns an
    /// independent voice, so overlapping shots don't cut each other off (JS makes a
    /// fresh `BufferSource` per play). Lazy-loads the sound if it wasn't preloaded.
    pub fn play(&mut self, name: &str, volume: f32) {
        if !self.sounds.contains_key(name) {
            self.load(name);
        }
        let Some(data) = self.sounds.get(name) else {
            return; // load failed / missing — silent, already warned.
        };
        // kira volume is in decibels; convert the linear amplitude gain.
        let data = data.volume(amplitude_to_db(volume));
        if let Err(e) = self.manager.play(data) {
            log::warn!("audio: play '{name}' failed: {e}");
        }
    }

    /// Start the background music track, replacing any current one. `looping` loops
    /// the whole track end-to-end (JS `new Audio(path); audio.loop = true`). The
    /// handle is retained so the track keeps playing and can be stopped later.
    pub fn play_music(&mut self, name: &str, looping: bool) {
        self.stop_music();
        let mut data = match StaticSoundData::from_file(self.path(name)) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("audio: music '{name}' failed to load: {e}");
                return;
            }
        };
        if looping {
            data = data.loop_region(..);
        }
        match self.manager.play(data) {
            Ok(handle) => {
                self.music = Some(handle);
                log::info!("audio: background music '{name}' (looping={looping})");
            }
            Err(e) => log::warn!("audio: music '{name}' failed to play: {e}"),
        }
    }

    /// Stop the background music (immediately). No-op if none is playing. Retained
    /// for the future mode/level-change paths; unused while music plays throughout.
    pub fn stop_music(&mut self) {
        if let Some(handle) = self.music.as_mut() {
            handle.stop(Tween::default());
        }
        self.music = None;
    }
}

// NB: no unit tests here — playback is real hardware output that headless CI can't
// assert on. The audio *decisions* (which cue a weapon requests, when music
// starts) are unit-tested on the caller side (see `game`'s `combat` tests); the
// actual sound is confirmed live with the user.
