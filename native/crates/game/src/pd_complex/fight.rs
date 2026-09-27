//! The headless match: one player (the guns spike's `Sim`) among the bots (the
//! simulant spike's `Sim`), on Complex.
//!
//! Frame order follows `lv_tick`: the player first (`bmove_tick`, the shots in
//! `hands_tick_attack`), then `props_tick`, which ticks the chrs — here
//! [`crate::pd_spike::sim::Sim::frame`]. Between them, each side's results are
//! handed to the other:
//!
//! 1. bots → guns: their perimeters (what the player collides with) and their hit
//!    volumes (what the player's weapons hit);
//! 2. the guns' frame;
//! 3. guns → bots: the player's hits on chrs (`chr_damage`, bot branch) and the
//!    player's chr (position, eye, facing, room);
//! 4. the bots' frame;
//! 5. bots → player: their hits (`chr_damage`, player branch), then death and
//!    respawn.

use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::pd_guns::player::PdInput;
use crate::pd_guns::range::{PartBox, Range, Target};
use crate::pd_guns::sim::{Sim as GunSim, SoundReq, HAND_MODELS};
use crate::pd_spike::chr::Act;
use crate::pd_spike::chraction;
use crate::pd_spike::pdmath::{baddtor2, turn, M_BADTAU};
use crate::pd_spike::gunpos::{assets_dir, BodyRig};
use crate::pd_spike::sim::{LevelChoice, Sim as BotSim, SimConfig, BODIES};

use super::health::HealthShow;

/// PD's multiplayer respawns on a button press once the red fade, the death
/// animation and the fade to black are done (`player.c:4574`). Fire is the
/// button; after this long (60 Hz ticks) it happens by itself.
pub const RESPAWN_AUTO_TICKS: i32 = 360;
/// The earliest fire can respawn: the death fades' length
/// ([`super::health::DEATH_ANIM_TICKS`] + the 60-tick fade to black).
pub const RESPAWN_MIN_TICKS: i32 = 150;

/// `SFXNUM_02AA_JO_ARGH`..`02B3` (`chr_grunt`, `chraction.c:3963`): Joanna's
/// hurt sounds. Only the ones in the sound pack play.
const JO_ARGH: [u16; 10] = [0x02aa, 0x02ab, 0x02ac, 0x02ad, 0x02ae, 0x02af, 0x02b0, 0x02b1, 0x02b2, 0x02b3];

/// One `BBOX` node of a body (`tools/pd-assets/pd_hitbox.py`): the rig joint that
/// poses it, the box in that joint's space (GLB units), its `HITPART_*`, and the
/// box it sits under.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct HitBoxDef {
    pub joint: usize,
    pub hitpart: i32,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub bbox_parent: Option<usize>,
}

#[derive(serde::Deserialize)]
struct HitBoxFile {
    boxes: Vec<HitBoxDef>,
}

/// Each spike body's hit boxes, in [`BODIES`] order (empty where not exported).
pub fn load_hitboxes() -> Vec<Vec<HitBoxDef>> {
    BODIES
        .iter()
        .map(|name| {
            let path = format!("{}/enemies/pd/characters/{name}.hitboxes.json", assets_dir());
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<HitBoxFile>(&t).map_err(|e| e.to_string())) {
                Ok(f) => f.boxes,
                Err(e) => {
                    log::warn!("pd_complex: no hit boxes for {name} ({path}: {e}); shots hit its whole perimeter");
                    Vec::new()
                }
            }
        })
        .collect()
}

pub struct Fight {
    /// The player: movement, guns, effects, HUD.
    pub guns: GunSim,
    /// The bots, with the player as one of their chrs ([`Fight::me`]).
    pub bots: BotSim,
    /// The player's index in `bots.chrs`.
    pub me: usize,
    /// `bondhealth`: 1 = full, dead at 0.
    pub health: f32,
    /// Ticks since the player died; `None` while alive.
    pub dead_for: Option<i32>,
    /// `player_display_damage`'s red flash, 1 → 0 (the panel's indicator).
    pub damage_flash: f32,
    /// The damage flash, the health bar and the death fades.
    pub hp: HealthShow,
    /// `guns.player_damage` already applied (explosion damage the guns report).
    player_damage_seen: f32,
    /// Times the player spawned (for tests and the scoreboard).
    pub spawns: u32,
    /// Each chr's fireslots' `endlvframe` (`chr_update_fireslot`): a gun's shot
    /// sound plays again only once its `duration60` has run out.
    fireslot_end: Vec<[i32; 2]>,
    /// `chr_grunt`'s rotating indexes: male, female, dataDyne Shock.
    grunt_next: [usize; 3],
    /// The bodies' rigs (skeleton + clips), to pose their hit boxes (and, in the
    /// window, to draw them). Empty when the body assets are missing.
    pub rigs: Vec<BodyRig>,
    pub hitboxes: Vec<Vec<HitBoxDef>>,
}

impl Fight {
    /// A match on Complex with the bots `config` describes, plus one player.
    pub fn new(mut config: SimConfig) -> Result<Self, String> {
        config.level = LevelChoice::Complex;
        config.humans = 1;
        let bots = BotSim::try_new(config)?;
        let me = bots.player_index().ok_or("no player chr")?;
        let mut guns = GunSim::new(HAND_MODELS[0])?;
        guns.walk_level = bots.level.clone();
        guns.range = Range::for_stage(bots.level.clone());
        let mut f = Fight {
            guns,
            bots,
            me,
            health: 1.0,
            dead_for: None,
            damage_flash: 0.0,
            hp: HealthShow::default(),
            player_damage_seen: 0.0,
            spawns: 0,
            fireslot_end: Vec::new(),
            grunt_next: [0; 3],
            rigs: BodyRig::load_all().unwrap_or_else(|e| {
                log::warn!("pd_complex: no body rigs ({e}); shots hit whole perimeters");
                Vec::new()
            }),
            hitboxes: load_hitboxes(),
        };
        f.spawn_player();
        Ok(f)
    }

    /// The environment's match (`PD_BOTS`, `PD_DIFF`, `PD_WEAPON`, `PD_SEED`,
    /// `PD_NAV`, `PD_RATE`; see the simulant spike), on Complex.
    pub fn from_env() -> Result<Self, String> {
        Self::new(SimConfig::from_env())
    }

    /// Restart the match: the same bots, everyone respawned, scores cleared.
    pub fn reset(&mut self) {
        let n = self.bots.config.bots.len();
        self.bots.reset(n);
        self.me = self.bots.player_index().unwrap_or(n);
        self.guns.objs.clear();
        self.spawn_player();
    }

    /// `player_spawn` (`player.c:486`) for a multiplayer player:
    /// `player_choose_spawn_location` with the player as the chr, the pad's look
    /// angle turned into `vv_theta` (`angle = 360° − pad angle`), health full,
    /// standing on the floor there.
    pub fn spawn_player(&mut self) {
        let (pos, angle) = self.bots.choose_spawn_location(self.me);
        let mut a = turn() - angle;
        if a >= turn() {
            a -= turn();
        }
        let theta = a * 360.0 / M_BADTAU;
        self.guns.player.place(pos, theta);
        self.guns.restock();
        self.guns.bgun.p.crouchpos = crate::pd_guns::bgun::CROUCHPOS_STAND;
        self.health = 1.0;
        self.dead_for = None;
        self.damage_flash = 0.0;
        self.hp.reset();
        self.guns.host_fade = None;
        self.spawns += 1;
        let c = &mut self.bots.chrs[self.me];
        c.actiontype = Act::Stand;
        c.damage = 0.0;
        c.fadealpha = -1.0;
        self.sync_player_chr();
    }

    /// One N64 frame.
    pub fn frame(&mut self, input: &PdInput) {
        let lvupdate240 = self.bots.config.rate.lvupdate240() as i32;
        self.sync_bots_to_guns();
        let dead = self.dead_for.is_some();
        let respawn = dead && input.fire && self.dead_for.unwrap_or(0) >= RESPAWN_MIN_TICKS;
        // A dead player's controls do nothing (the guns keep ticking).
        let input = if dead { PdInput::default() } else { input.clone() };
        self.guns.frame(&input, lvupdate240);
        self.apply_player_weapons();
        self.apply_player_self_damage();
        self.sync_player_chr();
        self.bots.frame();
        self.apply_bot_hits();
        self.bot_shot_effects();
        self.bot_grunts();
        self.bot_footsteps();
        let lv = self.guns.lv();
        self.damage_flash = (self.damage_flash - lv.lvupdate60 as f32 / 30.0).max(0.0);
        self.hp.tick(self.health, dead, lv.lvupdate60freal);
        let mut faded = false;
        if self.dead_for.is_some() {
            faded = self.hp.tick_dead(lv.lvupdate60freal);
        }
        self.guns.host_fade = Some(self.hp.fade);
        if let Some(t) = self.dead_for.as_mut() {
            *t += lv.lvupdate60;
            if (respawn && faded) || *t >= RESPAWN_AUTO_TICKS {
                self.spawn_player();
            }
        }
    }

    /// Step 1: the living bots' perimeters and hit volumes into the guns' world.
    /// A hit volume is the perimeter's bounding box holding the body's `BBOX`
    /// nodes, posed as the body is drawn this frame (the same CPU pose the window
    /// renders), for `chr_test_hit`.
    fn sync_bots_to_guns(&mut self) {
        self.guns.walk_cyls = chraction::perims_except(&self.bots, self.me);
        self.guns.range.targets.retain(|t| t.chr.is_none());
        for (k, c) in self.bots.chrs.iter().enumerate() {
            if k == self.me || c.is_dead() {
                continue;
            }
            let mut t = Target::chr(k, c.pos.x, c.pos.z, c.radius, c.pos.y, c.pos.y + c.height);
            if let (Some(rig), Some(boxes)) = (self.rigs.get(c.body), self.hitboxes.get(c.body)) {
                if !boxes.is_empty() {
                    let (model_mtx, globals) = rig.pose(c);
                    let to_cm = Mat4::from_scale(Vec3::splat(100.0)) * model_mtx;
                    let mut lo = t.bbox.min;
                    let mut hi = t.bbox.max;
                    for b in boxes {
                        let Some(g) = globals.get(b.joint) else { continue };
                        let m = to_cm * *g;
                        // Grow the coarse box to hold every part (an arm out past
                        // the perimeter still takes the hit).
                        for corner in 0..8 {
                            let p = Vec3::new(
                                if corner & 1 == 0 { b.min[0] } else { b.max[0] },
                                if corner & 2 == 0 { b.min[1] } else { b.max[1] },
                                if corner & 4 == 0 { b.min[2] } else { b.max[2] },
                            );
                            let w = m.transform_point3(p);
                            lo = lo.min(w);
                            hi = hi.max(w);
                        }
                        t.parts.push(PartBox {
                            to_local: m.inverse(),
                            min: Vec3::from(b.min),
                            max: Vec3::from(b.max),
                            hitpart: b.hitpart,
                            parent: b.bbox_parent,
                        });
                    }
                    t.bbox = crate::pd_guns::range::Aabb::new(lo, hi);
                }
            }
            self.guns.range.targets.push(t);
        }
    }

    /// Step 3a: the player's hits on bots — `chr_damage_by_impact` /
    /// `chr_damage_by_explosion` → `chr_damage`, bot branch (the shove, the flinch,
    /// death at `maxdamage`).
    fn apply_player_weapons(&mut self) {
        let hits = std::mem::take(&mut self.guns.chr_hits);
        for h in hits {
            if h.chr >= self.bots.chrs.len() || h.chr == self.me || self.bots.chrs[h.chr].is_dead() {
                continue;
            }
            chraction::chr_damage_hitpart(&mut self.bots, h.chr, h.damage, h.dir, Some(self.me), false, h.hitpart);
        }
    }

    /// The player's own explosions and falls: explosion damage the guns recorded
    /// (`chr_damage_by_explosion` on the player), and `player_die(true)` from
    /// `bwalk_update_vertical`.
    fn apply_player_self_damage(&mut self) {
        let d = self.guns.player_damage - self.player_damage_seen;
        self.player_damage_seen = self.guns.player_damage;
        if d > 0.0 && self.dead_for.is_none() {
            self.player_damage(d, Vec3::ZERO, None);
        }
        if self.guns.player.die_request && self.dead_for.is_none() {
            self.player_die(None);
        }
    }

    /// Step 3b: the player's chr, where the bots see it: feet at `manground`,
    /// `prop->pos` at the eye, `chr_get_theta` = `BADDTOR2(360 − vv_theta)`, the
    /// perimeter's height, the floor's room.
    fn sync_player_chr(&mut self) {
        let p = &self.guns.player;
        let perim = p.perim();
        let c = &mut self.bots.chrs[self.me];
        c.prevpos = c.pos;
        c.pos = Vec3::new(p.pos.x, p.manground, p.pos.z);
        c.ground = p.ground;
        c.player_eye_y = p.pos.y;
        let mut theta = baddtor2(360.0 - p.theta);
        if theta >= turn() {
            theta -= turn();
        } else if theta < 0.0 {
            theta += turn();
        }
        c.player_theta = theta;
        c.height = perim.ymax - perim.ymin;
        c.radius = perim.radius;
        c.floorroom = p.floorroom;
        // CHRHFLAG_CLOAKED (`chr_cloak`): the RC-P120's cloak.
        c.cloaked = self.guns.cloak.cloaked;
    }

    /// Step 5: the bots' hits on the player.
    fn apply_bot_hits(&mut self) {
        let hits = std::mem::take(&mut self.bots.player_hits);
        for h in hits {
            if h.chr != self.me || self.dead_for.is_some() {
                continue;
            }
            self.player_damage(h.damage, h.vector, h.attacker);
        }
    }

    /// What the bots' shots did this frame, heard and seen from the player: the
    /// half of `chr_shoot` (`chraction.c:9916`) the bot sim leaves out.
    /// * `chr_update_fireslot` (`:8803`): the gun's `shootsound` at the shooter,
    ///   at most once per chr per frame and not again until the fireslot's
    ///   `duration60` has passed; and the tracer (`beam_create`) when `makebeam`.
    /// * a chr hit: `bgun_play_prop_hit_sound` (`SFXMAP_8076_HIT_CHR`) and, on a
    ///   bot, `chr_emit_sparks`' blood (the player's own aren't drawn on their
    ///   screen: PD renders a spark group on a prop only while it is on screen).
    /// * a wall hit: `bgun_play_bg_hit_sound` and `SPARKTYPE_DEFAULT` sparks.
    ///   Bots leave no bullet holes (`chr_shoot` makes none).
    fn bot_shot_effects(&mut self) {
        use crate::pd_guns::fx;
        let n = self.bots.chrs.len();
        if self.guns.chr_beams.len() < n * 2 {
            self.guns.chr_beams.resize(n * 2, fx::Beam::default());
        }
        if self.fireslot_end.len() < n {
            self.fireslot_end.resize(n, [0; 2]);
        }
        let lvframe60 = self.bots.g.lvframe60;
        let mut sounddone = vec![false; n];
        let shots: Vec<crate::pd_spike::sim::Shot> = self.bots.shots.iter().filter(|s| s.age == 0).cloned().collect();
        for s in shots {
            let wnum = s.weapon.0 as i32;
            let (duration, sound) = self
                .guns
                .gset
                .func(wnum, 0)
                .and_then(|f| f.shoot.as_ref())
                .map_or((0, 0), |sh| (sh.duration60, sh.shootsound));
            let hand = s.hand.min(1);
            let playsound = if duration > 0 { !sounddone[s.shooter] && lvframe60 > self.fireslot_end[s.shooter][hand] } else { true };
            if playsound && sound != 0 {
                let (pan, volume) = (self.guns.pan_of(s.from), self.guns.ps_vol(sound, s.from));
                self.guns.sounds.push(SoundReq { id: sound, speed: 1.0, pan, volume, loop_hand: None });
                self.fireslot_end[s.shooter][hand] = lvframe60 + duration;
                sounddone[s.shooter] = true;
            }
            if s.beam {
                let b = &mut self.guns.chr_beams[s.shooter * 2 + hand];
                b.create(&mut self.guns.bgun.rng, wnum, s.from, s.to);
            }
            let dir = (s.to - s.from).normalize_or_zero();
            match s.hit_chr {
                Some(k) => {
                    let (pan, volume) = (self.guns.pan_of(s.to), self.guns.ps_vol(0x8076, s.to));
                    self.guns.sounds.push(SoundReq { id: 0x8076, speed: 1.0, pan, volume, loop_hand: None });
                    if k != self.me {
                        let rng = &mut self.guns.bgun.rng;
                        if rng.random() & 4 == 0 {
                            self.guns.sparks.create(rng, s.to + dir * 42.0, dir, Vec3::ZERO, fx::SPARKTYPE_FLESH_LARGE);
                        }
                        self.guns.sparks.create(rng, s.to, dir, Vec3::ZERO, fx::SPARKTYPE_BLOOD);
                        self.guns.sparks.create(rng, s.to, dir, Vec3::ZERO, fx::SPARKTYPE_FLESH);
                    }
                }
                None if s.hit_wall => {
                    self.guns.play_bg_hit_sound(wnum, s.to);
                    let rng = &mut self.guns.bgun.rng;
                    self.guns.sparks.create(rng, s.to, Vec3::ZERO, Vec3::ZERO, fx::SPARKTYPE_DEFAULT);
                }
                None => {}
            }
        }
    }

    /// The bots' footsteps (`footstep_check_default`'s `ps_create`), at each bot.
    fn bot_footsteps(&mut self) {
        let steps = std::mem::take(&mut self.bots.footsteps);
        for (k, id) in steps {
            let Some(c) = self.bots.chrs.get(k) else { continue };
            let pos = c.pos;
            let (pan, volume) = (self.guns.pan_of(pos), self.guns.ps_vol(id, pos));
            self.guns.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
        }
    }

    /// `chr_grunt` (`chraction.c:3806`) for each bot hurt this frame: the voice by
    /// head (Maian, dataDyne Shock, male, Joanna, female), at the bot.
    fn bot_grunts(&mut self) {
        let grunts = std::mem::take(&mut self.bots.grunts);
        for k in grunts {
            let Some(c) = self.bots.chrs.get(k) else { continue };
            if c.player {
                continue;
            }
            let pos = c.prop_pos();
            // BODIES: Joanna, Guard, Cassandra, Mr Blonde, dataDyne Shock, Elvis.
            let id = match c.body {
                5 => [0x05df, 0x05e0, 0x05e1][(self.guns.bgun.rng.random() % 3) as usize],
                4 => {
                    const SHOCK: [u16; 14] =
                        [0x86, 0x88, 0x8a, 0x8c, 0x8e, 0x90, 0x92, 0x94, 0x96, 0x98, 0x9a, 0x9c, 0x9e, 0x87];
                    let i = self.grunt_next[2];
                    self.grunt_next[2] = (i + 1) % SHOCK.len();
                    SHOCK[i]
                }
                1 | 3 => {
                    let i = self.grunt_next[0];
                    self.grunt_next[0] = (i + 1) % 25;
                    0x86 + i as u16
                }
                0 => JO_ARGH[(self.guns.bgun.rng.random() % 10) as usize],
                _ => {
                    let i = self.grunt_next[1];
                    self.grunt_next[1] = (i + 1) % 3;
                    [0x0d, 0x0e, 0x0f][i]
                }
            };
            let (pan, volume) = (self.guns.pan_of(pos), self.guns.ps_vol(id, pos));
            self.guns.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
        }
    }

    /// `chr_damage`'s player branch in normal multiplayer (`chraction.c:4752`):
    /// handicap 1 and no shield, so `bondhealth -= damage × 0.125`; the shove
    /// (`bondshotspeed += vector × 0.75`), the grunt, the red flash, and death at 0.
    pub fn player_damage(&mut self, damage: f32, vector: Vec3, attacker: Option<usize>) {
        if damage <= 0.0 || self.dead_for.is_some() {
            return;
        }
        let amount = damage * 0.125;
        self.hp.display_health(self.health);
        self.health -= amount;
        self.hp.display_damage();
        self.guns.player.shotspeed.x += vector.x * 0.75;
        self.guns.player.shotspeed.z += vector.z * 0.75;
        self.damage_flash = 1.0;
        if self.health <= 0.0 {
            self.health = 0.0;
            self.player_die(attacker);
        } else {
            // chr_grunt: one of Jo's ten, `random() % 10`.
            let id = JO_ARGH[(self.guns.bgun.rng.random() % 10) as usize];
            self.guns.sounds.push(SoundReq { id, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
        }
    }

    /// `player_die_by_shooter` → the kill is scored like a bot's (`chr_die`), and
    /// the player's chr stops being a target.
    fn player_die(&mut self, attacker: Option<usize>) {
        chraction::chr_die(&mut self.bots, self.me, attacker);
        self.dead_for = Some(0);
        self.health = 0.0;
        self.guns.bgun.p.crouchpos = crate::pd_guns::bgun::CROUCHPOS_SQUAT;
    }

    /// The player's chr.
    pub fn my_chr(&self) -> &crate::pd_spike::chr::Chr {
        &self.bots.chrs[self.me]
    }

    /// Shared level (for the renderer and tests).
    pub fn level(&self) -> Arc<crate::pd_spike::tile_level::TileLevel> {
        self.bots.level.clone()
    }
}
