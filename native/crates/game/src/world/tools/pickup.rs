//! Weapons and ammo on the ground: authoring them in BUILD, collecting them in
//! HUNT. The component is [`crate::ecs::Pickup`]; the design is
//! `DESIGN_PICKUPS.md`.
//!
//! # Why this isn't an ECS system
//!
//! Same reason the turret isn't ([`super::turret`]): collecting a pickup writes the
//! player's weapon inventory and plays a sound, and `SystemCtx` deliberately carries
//! neither. So it ticks as a `World` method called straight from `fixed_step`, and
//! the component stays plain data.
//!
//! # A gun on the ground is not a prop
//!
//! Ammo crates are ordinary catalog props. A **weapon** pickup is not: its mesh
//! depends on which gun it is, and the whole arsenal is already uploaded to the
//! world-space weapon render library that the hunters draw their guns from. So
//! [`MeshId::WeaponPickup`] carries no [`crate::props`] catalog row and draws on that
//! channel instead — which is also what makes it free of a collider and absent from
//! the nav bake, since both of those are gated on a catalog lookup.

use super::super::*;
use crate::ecs::{MeshId, Pickup, PickupKind, Renderable, Transform};

/// How close the player's feet must get, horizontally, to collect. Generous enough
/// that you don't have to stand on the exact centre, tight enough that you don't
/// hoover up the gun you were walking past.
const GRAB_RADIUS: f32 = 1.0;

/// Vertical window for a collect, measured **pickup relative to the player's feet**.
/// Without it a gun on a balcony would be collected by walking underneath it.
///
/// `ABOVE` is roughly the player's own height, so you can take something off a waist-
/// or head-high surface you are standing next to; `BELOW` is the small slack for
/// standing on a step or a stair tread beside it.
const GRAB_ABOVE: f32 = 1.8;
const GRAB_BELOW: f32 = 0.6;

/// Half-extents of a gun lying on the floor, in metres. Every gun uses the same box:
/// the arsenal's meshes are all roughly a hand-held weapon, and giving each its own
/// footprint would make the placement ghost and the click target jump around as the
/// author flicks through the list, for no authoring benefit.
const WEAPON_PICKUP_HALF: Vec3 = Vec3::new(0.26, 0.09, 0.10);

/// How high a weapon pickup floats above the point it was authored at, and how far
/// it bobs either side of that.
const HOVER_HEIGHT: f32 = 0.30;
const BOB_AMPLITUDE: f32 = 0.06;

/// The model bounds registered for [`MeshId::WeaponPickup`] — the **column the gun
/// floats in**, from the floor up past the top of its bob, rather than a box around
/// the mesh itself.
///
/// This is the shape of the click target and the placement ghost, and the column is
/// the honest answer for both. The prop anchor puts a mesh's `min.y` on the floor, so
/// a box that started at the hovering gun would have that lift cancelled straight
/// back out and leave the pick box on the ground **under** a gun drawn 30 cm above
/// it — visible and unclickable. That is exactly the bug the sentry gun hit from the
/// other direction (`a_turrets_pick_box_covers_the_gun_you_can_see`), so it is worth
/// not re-earning: the volume spans everything the pickup occupies at any point in
/// its bob.
pub fn weapon_pickup_bounds() -> (Vec3, Vec3) {
    let h = WEAPON_PICKUP_HALF;
    (
        Vec3::new(-h.x, 0.0, -h.z),
        Vec3::new(h.x, HOVER_HEIGHT + BOB_AMPLITUDE + h.y, h.z),
    )
}
/// Bob cycles per second, and turns per second.
const BOB_RATE: f32 = 1.1;
const SPIN_RATE: f32 = 0.7;

/// World-space scale for a gun pickup's mesh. The weapon GLBs are in GoldenEye units
/// (the viewmodel draws them at ~0.0007); this is the same figure the hunters' held
/// guns end up at, so a gun on the floor is the size of the gun a hunter carries.
const WEAPON_PICKUP_SCALE: f32 = CHAR_SCALE;

/// Collecting anything plays the **reload** sound — the same clip a weapon swap uses.
/// It already reads as "a gun just got loaded", which is exactly what a pickup is, and
/// reusing it keeps the acquire moment sounding like the rest of the weapon handling
/// rather than like a menu confirmation. A gun is the louder of the two events.
const PICKUP_SOUND: &str = "sounds/weapons/reload.wav";
const GUN_VOL: f32 = 0.7;
const AMMO_VOL: f32 = 0.5;
/// How far a *hunter's* collect carries. The player's own pickups are unattenuated
/// (they happen at the listener); a hunter arming itself across the level is a warning
/// worth hearing, so it falls off like a door does rather than being silent.
const PICKUP_AUDIBLE_RANGE: f32 = 22.0;

/// A pickup that never returns parks its countdown here: `taken()` stays true and
/// subtracting a timestep leaves it unchanged, so "gone for the round" needs no
/// second flag alongside the clock.
const GONE: f32 = f32::INFINITY;

/// How long the HUD shows what you just picked up, in seconds.
pub(crate) const PICKUP_MESSAGE_TIME: f32 = 2.0;

/// Spare magazines a hunter carries when it acquires a gun — from a pickup, or at
/// spawn when `ARMED_HUNTERS=1` skips the shopping trip.
///
/// More generous than the player's authored default because a hunter cannot choose to
/// go and top up mid-fight: it only notices it is dry once it *is* dry, and a hunter
/// that spends the round walking to crates is not a hunter.
pub(crate) const HUNTER_SPAWN_MAGS: u32 = 4;

/// How close a hunter has to get to collect. Wider than the player's [`GRAB_RADIUS`]:
/// a hunter is steered by grid nav to the *cell* the pickup is in and stops on
/// arrival, so demanding the same precision as a mouse-steered player would leave one
/// standing next to a gun it could not pick up — which reads as a broken hunter, and
/// is the failure mode this radius exists to avoid.
const HUNTER_GRAB_RADIUS: f32 = 1.6;

/// What a hunter is short of, and therefore what it will cross the level for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HunterWant {
    /// Holding nothing — any weapon will do.
    Weapon,
    /// Has a gun, no rounds left for it — an ammo crate for that gun, or a whole
    /// different gun.
    Ammo,
}

impl World {
    // ── Authoring (BUILD) ───────────────────────────────────────────────────────

    /// The settings a newly-placed pickup gets — the panel's draft. Edited by the
    /// pickup settings block while a pickup tool is armed, then copied onto each
    /// entity at placement, which is how the door tool treats its catalog defaults.
    pub fn pickup_draft(&self) -> Pickup {
        self.pickup_draft
    }

    /// Overwrite the draft (panel edit). The `kind` is fixed by whichever pickup
    /// mesh is armed, so an edit can't turn the armed ammo crate into a gun.
    pub fn set_pickup_draft(&mut self, mut edited: Pickup) {
        edited.kind = self
            .prop_tool
            .and_then(crate::props::pickup_kind)
            .unwrap_or(edited.kind);
        edited.cooldown = 0.0;
        self.pickup_draft = edited;
    }

    /// Whether the level authors **any** weapon pickup.
    ///
    /// The gate on the whole empty-handed rule, for both sides, and it follows the
    /// precedent the spawn pads set: Perfect Dark only overrides the default player
    /// entry `if (g_NumSpawnPoints > 0)` (`playerreset.c:398`), and this is the same
    /// shape of guard. A level with no guns on the floor cannot be played by anyone who
    /// starts without one — the player would have nothing but the shop and the hunters
    /// would wander looking for something that does not exist — so on such a level
    /// everybody starts armed, exactly as they did before pickups.
    ///
    /// Counts **authored** pickups regardless of whether one is currently taken, so the
    /// answer doesn't depend on where in the BUILD→HUNT sequence it is asked.
    pub(crate) fn has_weapon_pickups(&self) -> bool {
        self.ecs
            .world()
            .query::<&Pickup>()
            .iter()
            .any(|p| p.kind == PickupKind::Weapon)
    }

    /// Whether hunters should spawn empty-handed this hunt: the flag AND a level that
    /// actually has guns to find.
    pub(crate) fn hunters_start_unarmed(&self) -> bool {
        self.unarmed_hunters && self.has_weapon_pickups()
    }

    /// Make sure the player can fight on a level that authors no weapon pickups: hand
    /// over the starting sidearm, loaded.
    ///
    /// Called at BUILD→HUNT, which is the first moment the level's contents are known —
    /// the inventory is built in `World::new`, long before any level is loaded. A no-op
    /// on a level with pickups (you go and find one) and a no-op if the player already
    /// owns something (`OWN_ALL`, a shop purchase, a previous hunt).
    pub(crate) fn grant_fallback_sidearm(&mut self) {
        if self.has_weapon_pickups() {
            return;
        }
        let armed = self
            .owned
            .iter()
            .enumerate()
            .any(|(i, &o)| o && !self.arsenal.weapons()[i].is_unarmed());
        if armed {
            return;
        }
        // The same sidearm choice the game used before pickups: the PP7 in a GoldenEye
        // arsenal, PD's own Falcon 2 in a Perfect Dark one.
        let arsenal = self.arsenal.weapons();
        let Some(idx) = arsenal
            .iter()
            .position(|w| w.name == "PP7")
            .or_else(|| arsenal.iter().position(|w| w.name == "Falcon 2"))
            .or_else(|| arsenal.iter().position(|w| !w.is_unarmed()))
        else {
            return;
        };
        self.owned[idx] = true;
        self.weapons[idx].stock_bought();
        self.equip_weapon(idx);
        log::info!(
            "no weapon pickups authored — starting with the {} instead of empty-handed",
            arsenal[idx].name
        );
    }

    /// What kind of pickup the placement tool is armed for, or `None` when the armed
    /// prop is ordinary scenery (or nothing is armed). Drives whether the panel shows
    /// the pickup settings block, and whether it shows the magazine control.
    pub fn armed_pickup_kind(&self) -> Option<PickupKind> {
        self.prop_tool.and_then(crate::props::pickup_kind)
    }

    /// The weapon the armed **weapon** pickup names, so the panel can mark the row
    /// and preview the right gun. `None` unless a weapon pickup is armed.
    pub fn armed_pickup_weapon(&self) -> Option<&'static str> {
        (self.prop_tool == Some(MeshId::WeaponPickup)).then_some(self.pickup_draft.weapon)
    }

    /// Arm placement of a **weapon** pickup for the gun named `weapon`, BUILD only.
    /// Re-arming the same gun disarms, matching [`World::arm_prop_placement`].
    pub fn arm_weapon_pickup(&mut self, weapon: &'static str) {
        let armed = self.prop_tool == Some(MeshId::WeaponPickup);
        // Clicking the gun that is already armed disarms, like every other palette row.
        if armed && self.pickup_draft.weapon == weapon {
            self.cancel_prop_placement();
            return;
        }
        // Keep the reserve/respawn the author has been tuning; only the gun changes
        // when they pick a different one out of the list.
        let (mags, respawn) = (self.pickup_draft.mags, self.pickup_draft.respawn);
        // Only arm when not already armed. Every gun shares one `MeshId`, so calling
        // `arm_prop_placement` again would hit its *toggle* and disarm the tool —
        // switching from one gun to another would silently stop placement while the
        // panel still showed a gun selected.
        if !armed {
            self.arm_prop_placement(MeshId::WeaponPickup);
        }
        self.pickup_draft = Pickup { mags, respawn, ..Pickup::weapon(weapon) };
    }

    /// Arm placement of an **ammo crate**, BUILD only. The crate is a visual choice;
    /// which weapon it feeds stays whatever the draft says.
    pub fn arm_ammo_pickup(&mut self, mesh: MeshId) {
        let weapon = self.pickup_draft.weapon;
        let (mags, respawn) = (self.pickup_draft.mags, self.pickup_draft.respawn);
        self.arm_prop_placement(mesh);
        if self.prop_tool == Some(mesh) {
            self.pickup_draft = Pickup { mags, respawn, ..Pickup::ammo(weapon) };
        }
    }

    /// The selected prop's pickup settings, for the panel inspector. `None` when the
    /// selection isn't a pickup.
    pub fn selected_pickup(&self) -> Option<Pickup> {
        let e = self.selected_prop?;
        self.ecs.world().entity(e).ok()?.get::<&Pickup>().map(|p| *p)
    }

    /// Write edited pickup settings back onto the selected pickup. Ignores `kind`
    /// and `cooldown` — the first is fixed by the mesh, the second is runtime.
    pub fn set_selected_pickup(&mut self, edited: Pickup) {
        let Some(e) = self.selected_prop else { return };
        if let Ok(p) = self.ecs.world_mut().query_one_mut::<&mut Pickup>(e) {
            p.weapon = edited.weapon;
            p.mags = edited.mags;
            p.respawn = edited.respawn;
        }
    }

    /// The [`Pickup`] a placement of `mesh` should attach, or `None` for scenery.
    /// Called by `confirm_prop_placement`, so every pickup — however it was placed —
    /// gets its component from one place.
    pub(crate) fn pickup_for_placement(&self, mesh: MeshId) -> Option<Pickup> {
        let kind = crate::props::pickup_kind(mesh)?;
        Some(Pickup { kind, ..self.pickup_draft })
    }

    // ── Drawing ─────────────────────────────────────────────────────────────────

    /// The weapon-pickup draws this frame: `(weapon name, model→world)` per gun lying
    /// on the ground, hovering and slowly turning. Fed into the same render library
    /// the hunters' held guns use, so no new channel and no new asset load.
    ///
    /// A taken pickup is omitted, which is what makes an empty shelf read as empty.
    /// Live in **both** modes: the editor has to show what it is placing.
    pub(crate) fn weapon_pickup_draws(&self) -> Vec<(&'static str, Mat4)> {
        let t = self.pickup_clock;
        let mut out = Vec::new();
        for (tr, r, p) in self
            .ecs
            .world()
            .query::<(&Transform, &Renderable, &Pickup)>()
            .iter()
        {
            if r.mesh != MeshId::WeaponPickup || p.taken() {
                continue;
            }
            // Phase the bob and spin by position so a rack of pickups doesn't pulse
            // in lockstep — a detail, but a row of guns beating as one reads as a
            // glitch rather than as several objects.
            let phase = tr.pos.x * 1.7 + tr.pos.z * 2.3;
            let bob = (t * std::f32::consts::TAU * BOB_RATE + phase).sin() * BOB_AMPLITUDE;
            let spin = t * std::f32::consts::TAU * SPIN_RATE + phase;
            let centre = tr.pos + Vec3::Y * (HOVER_HEIGHT + bob);
            out.push((
                p.weapon,
                Mat4::from_translation(centre)
                    * Mat4::from_rotation_y(spin)
                    * Mat4::from_quat(tr.rot)
                    * Mat4::from_scale(Vec3::splat(WEAPON_PICKUP_SCALE)),
            ));
        }
        out
    }

    /// Advance the shared hover/spin clock. Driven per **render** frame (like the
    /// character animation mixer) rather than per fixed step, so the motion is smooth
    /// at any framerate and costs nothing when nothing is placed.
    pub fn advance_pickups(&mut self, dt: f32) {
        self.pickup_clock += dt;
    }

    /// What the player just collected, for the HUD banner: `(text, seconds left)`.
    pub fn pickup_message(&self) -> Option<(&str, f32)> {
        (self.pickup_message_timer > 0.0)
            .then(|| (self.pickup_message.as_str(), self.pickup_message_timer))
    }

    // ── The hunt ────────────────────────────────────────────────────────────────

    /// Put every pickup back on the floor at BUILD→HUNT. Mirrors
    /// [`World::spawn_turrets`]: the runtime countdown is derived here, never loaded,
    /// so a hunt always starts with a fully stocked level.
    pub(crate) fn spawn_pickups(&mut self) {
        let live: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Pickup)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        let n = live.len();
        for e in live {
            if let Ok(p) = self.ecs.world_mut().query_one_mut::<&mut Pickup>(e) {
                p.cooldown = 0.0;
            }
        }
        self.pickup_message.clear();
        self.pickup_message_timer = 0.0;
        // One line naming what the level actually holds and which rule that puts both
        // sides under. `World::roster_summary` is the pattern: a resolved choice nobody
        // can see is a choice that will be argued about later — and this one has three
        // inputs (the authored pickups, `ARMED_HUNTERS`, `OWN_ALL`) whose combination
        // decides whether anybody starts with a gun, which is exactly the sort of thing
        // that is impossible to diagnose from watching.
        let guns = self
            .ecs
            .world()
            .query::<&Pickup>()
            .iter()
            .filter(|p| p.kind == PickupKind::Weapon)
            .count();
        log::info!(
            "pickups: {n} placed ({guns} weapon, {} ammo) — hunters start {}{}",
            n - guns,
            if self.hunters_start_unarmed() { "EMPTY-HANDED" } else { "armed" },
            if self.own_all {
                "; OWN_ALL=1 so the player already owns every gun"
            } else {
                ""
            }
        );
    }

    /// Reset the pickups at HUNT→BUILD, so one collected this hunt is back on the
    /// floor in the editor rather than invisible.
    pub(crate) fn clear_pickups(&mut self) {
        self.spawn_pickups();
        self.pickup_message.clear();
        self.pickup_message_timer = 0.0;
    }

    /// One pickup tick: run the respawn clocks, then hand the player anything they
    /// are standing on. Called per fixed step in HUNT.
    pub(crate) fn pickup_step(&mut self, dt: f32) {
        self.pickup_message_timer = (self.pickup_message_timer - dt).max(0.0);
        self.pickup_respawn_step(dt);
        self.player_pickup_step();
        // The hunters collect after the player, so a dead heat goes to the player —
        // the one arbitrary tie-break here, and it favours the side that can see what
        // it was going for.
        self.hunter_pickup_step();
    }

    /// Run every taken pickup's return clock. Unconditional: it must not depend on
    /// there being a player to collect things, or a headless run (and the beat after a
    /// death) would freeze the level's restocking.
    fn pickup_respawn_step(&mut self, dt: f32) {
        let ticked: Vec<(hecs::Entity, f32)> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Pickup)>()
            .iter()
            // `GONE` is infinite, so a never-returning pickup stays taken.
            .filter(|(_, p)| p.taken())
            .map(|(e, p)| (e, (p.cooldown - dt).max(0.0)))
            .collect();
        for (e, left) in ticked {
            if let Ok(p) = self.ecs.world_mut().query_one_mut::<&mut Pickup>(e) {
                p.cooldown = left;
            }
        }
    }

    /// Hand the player whatever they are standing on.
    fn player_pickup_step(&mut self) {
        let Some(feet) = self.player_pos() else { return };
        // Snapshot first: granting a pickup writes the weapon inventory and plays a
        // sound, neither of which can happen while the ECS query borrow is live.
        let collected: Vec<(hecs::Entity, Pickup)> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &Pickup)>()
            .iter()
            .filter(|(_, tr, p)| {
                if p.taken() {
                    return false;
                }
                let flat = Vec2::new(tr.pos.x - feet.x, tr.pos.z - feet.z).length();
                // How far the pickup sits above the player's feet: within arm's reach
                // upward, and only slightly below.
                let rise = tr.pos.y - feet.y;
                flat <= GRAB_RADIUS && rise <= GRAB_ABOVE && rise >= -GRAB_BELOW
            })
            .map(|(e, _, p)| (e, *p))
            .collect();
        for (e, p) in collected {
            if !self.grant_pickup(&p) {
                continue; // nothing to give (an absent gun)
            }
            let cooldown = if p.respawn > 0.0 { p.respawn } else { GONE };
            if let Ok(live) = self.ecs.world_mut().query_one_mut::<&mut Pickup>(e) {
                live.cooldown = cooldown;
            }
        }
    }

    // ── The hunters go shopping too ──────────────────────────────────────────────

    /// What hunter `inst` is short of, or `None` if it can fight.
    ///
    /// Two states, and they are behaviourally the same thing: a hunter holding nothing
    /// and a hunter holding a gun with no rounds anywhere can both only walk around
    /// pointing it. Both therefore stop engaging and go shopping — which is also what
    /// stops the "aims forever, never fires" stall the AI testbed exists to catch.
    pub(crate) fn hunter_want(inst: &EnemyInstance) -> Option<HunterWant> {
        if inst.enemy.is_dead() {
            return None;
        }
        if inst.weapon.is_unarmed() {
            return Some(HunterWant::Weapon);
        }
        // A clip of 0 is an *unclipped* weapon (a thrown grenade), which never runs
        // out in the reload sense — not a dry one.
        if inst.weapon.clip > 0 && inst.loaded == 0 && inst.reserve == 0 {
            return Some(HunterWant::Ammo);
        }
        None
    }

    /// Where hunter `inst` should walk to fetch what it needs, or `None` if it needs
    /// nothing (or nothing on the floor would help).
    ///
    /// Nearest-first by straight-line distance, not by path length: a hunter re-asks
    /// every step and A\* already does the walking, so paying for a real path
    /// comparison across every pickup, for every dry hunter, every step would buy
    /// almost nothing. A gun behind a wall is still the right gun to want.
    pub(crate) fn hunter_fetch_target(&self, inst: &EnemyInstance) -> Option<Vec3> {
        let want = Self::hunter_want(inst)?;
        self.best_pickup_for(want, inst.weapon.name, inst.enemy.pos)
            .map(|(_, pos)| pos)
    }

    /// The nearest pickup that would satisfy `want` for a holder of `holding`:
    /// `(entity, position)`.
    ///
    /// A **weapon** pickup satisfies both wants — a fresh gun arrives loaded, so it is
    /// as good an answer to "I am out of ammo" as a crate is, and often a better one.
    /// An **ammo** crate only satisfies a hunter that is holding the gun it feeds,
    /// which is what stops a dry hunter marching across the level for rounds it cannot
    /// chamber.
    fn best_pickup_for(
        &self,
        want: HunterWant,
        holding: &str,
        from: Vec3,
    ) -> Option<(hecs::Entity, Vec3)> {
        let mut best: Option<(hecs::Entity, Vec3, f32)> = None;
        for (e, tr, p) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &Pickup)>()
            .iter()
        {
            if p.taken() {
                continue;
            }
            let useful = match (want, p.kind) {
                (_, PickupKind::Weapon) => true,
                (HunterWant::Ammo, PickupKind::Ammo) => p.weapon == holding,
                (HunterWant::Weapon, PickupKind::Ammo) => false,
            };
            if !useful {
                continue;
            }
            let d = tr.pos.distance(from);
            if best.is_none_or(|(_, _, bd)| d < bd) {
                best = Some((e, tr.pos, d));
            }
        }
        best.map(|(e, pos, _)| (e, pos))
    }

    /// Let the hunters collect whatever they are standing on. Called from
    /// [`Self::pickup_step`] after the player's pass, so where both arrive on the same
    /// step the player wins the race — the one arbitrary tie-break in here, and it
    /// favours the side that can see what it is going for.
    fn hunter_pickup_step(&mut self) {
        // (hunter index, pickup entity, what it holds) resolved before any mutation:
        // granting writes the roster, and taking writes the ECS.
        let mut grants: Vec<(usize, hecs::Entity, Pickup)> = Vec::new();
        let mut claimed: Vec<hecs::Entity> = Vec::new();
        for (i, inst) in self.enemies.iter().enumerate() {
            if Self::hunter_want(inst).is_none() {
                continue;
            }
            let want = Self::hunter_want(inst).unwrap();
            let Some((e, pos)) = self.best_pickup_for(want, inst.weapon.name, inst.enemy.pos)
            else {
                continue;
            };
            // Two hunters converging on the same gun: only the first gets it, and the
            // other re-picks next step (its `want` has not changed).
            if claimed.contains(&e) {
                continue;
            }
            let flat = Vec2::new(pos.x - inst.enemy.pos.x, pos.z - inst.enemy.pos.z).length();
            let rise = pos.y - inst.enemy.pos.y;
            if flat > HUNTER_GRAB_RADIUS || rise > GRAB_ABOVE || rise < -GRAB_BELOW {
                continue;
            }
            let Some(p) = self
                .ecs
                .world()
                .entity(e)
                .ok()
                .and_then(|r| r.get::<&Pickup>().map(|p| *p))
            else {
                continue;
            };
            claimed.push(e);
            grants.push((i, e, p));
        }
        for (i, e, p) in grants {
            if !self.grant_hunter_pickup(i, &p) {
                continue;
            }
            let cooldown = if p.respawn > 0.0 { p.respawn } else { GONE };
            if let Ok(live) = self.ecs.world_mut().query_one_mut::<&mut Pickup>(e) {
                live.cooldown = cooldown;
            }
        }
    }

    /// Give hunter `idx` what `p` holds. `false` if there was nothing to give.
    ///
    /// Picking up a **weapon** re-equips the hunter: a new [`EnemyWeaponDef`], a full
    /// magazine and spare mags. What deliberately does *not* change is its animation
    /// class or its arm rig — those are resolved per body at spawn and are not cheap to
    /// redo, so a hunter that finds a pistol still holds it with its spawn-time grip.
    /// That is a visible compromise and the honest place to note it: the alternative is
    /// rebuilding the layered animator mid-fight.
    fn grant_hunter_pickup(&mut self, idx: usize, p: &Pickup) -> bool {
        let Some(cfg) = self
            .arsenal
            .weapons()
            .iter()
            .find(|w| w.name == p.weapon)
            .copied()
        else {
            return false; // not in this session's arsenal — see `grant_pickup`
        };
        let Some(inst) = self.enemies.get_mut(idx) else {
            return false;
        };
        let mags = p.mags.max(1);
        match p.kind {
            PickupKind::Weapon => {
                let def = crate::combat::enemy_def_for(&cfg);
                inst.weapon = def;
                inst.dual = false; // a hunter finds one gun, not a matched pair
                inst.loaded = def.clip;
                inst.reserve = def.clip * mags;
                inst.reload_timer = 0.0;
                inst.use_secondary = false;
                log::info!("hunter {idx} picked up {}", def.name);
            }
            PickupKind::Ammo => {
                let rounds = inst.weapon.clip * mags;
                inst.reserve = inst.reserve.saturating_add(rounds);
                log::info!("hunter {idx} picked up {rounds} rounds for {}", inst.weapon.name);
            }
        }
        // Audible where it happens — a gun being racked across the room is information
        // the player should get, at the same falloff a door gets.
        let (at, listener) = (
            self.enemies[idx].enemy.pos,
            self.player_pos().unwrap_or(Vec3::ZERO),
        );
        if let Some(audio) = self.audio.as_mut() {
            let vol = super::door::falloff_volume(GUN_VOL, at, listener, PICKUP_AUDIBLE_RANGE);
            if vol > 0.0 {
                audio.play(PICKUP_SOUND, vol);
            }
        }
        true
    }

    /// Hand `p` to the player. Returns `false` when there was nothing to give, in
    /// which case the pickup stays on the floor rather than being consumed for
    /// nothing.
    ///
    /// A **weapon** you don't own grants ownership, a full magazine and `mags` spare
    /// magazines — and equips itself if you were empty-handed, since otherwise
    /// finding a gun while holding nothing would leave you holding nothing. One you
    /// already own is ammo instead, which is what GoldenEye does with a second copy
    /// of a gun.
    ///
    /// **Ammo** is granted whether or not you own the gun, so rounds bank for the
    /// weapon you haven't found yet.
    fn grant_pickup(&mut self, p: &Pickup) -> bool {
        // The authored weapon may not be in this session's arsenal (a level authored
        // under a different `ARSENAL=`). Say so once, on collection, rather than
        // leaving a pickup that silently does nothing.
        let Some(idx) = self
            .arsenal
            .weapons()
            .iter()
            .position(|w| w.name == p.weapon)
        else {
            log::warn!(
                "pickup for {:?} can't be collected — that weapon is not in the live \
                 arsenal ({})",
                p.weapon,
                self.arsenal.summary()
            );
            return false;
        };
        let cfg = self.arsenal.weapons()[idx];
        let rounds = cfg.magazine_size * p.mags.max(1);

        let fresh = matches!(p.kind, PickupKind::Weapon) && !self.owned[idx];
        // A gun you ALREADY own only converts to ammo if you actually need the rounds.
        //
        // Not a nicety — it is what stops the player starving the hunters. A duplicate
        // gun used to be taken off the floor unconditionally, so a player carrying the
        // whole arsenal (`OWN_ALL=1`, or just a good round) could hoover up every weapon
        // pickup in the level as ammo it did not need, leaving the empty-handed hunters
        // with nothing to find. "Need" is measured against what this pickup carries: if
        // you already hold that many spare rounds, it has nothing to offer you and stays
        // where it is for someone who does.
        if !fresh && matches!(p.kind, PickupKind::Weapon) {
            let (_, reserve) = (self.weapons[idx].magazine(), self.weapons[idx].reserve());
            if reserve >= rounds {
                return false;
            }
        }
        if fresh {
            self.owned[idx] = true;
            // Arrives loaded plus the authored spare magazines. `stock`, not
            // `add_reserve`: reserve rounds can't fill a magazine without a reload the
            // player never asked for, and a gun off the floor should be ready to fire.
            self.weapons[idx].stock(p.mags.max(1));
            // Empty-handed → the gun you just found is the gun you are holding. Goes
            // through the same instant equip a fresh hunt uses rather than the
            // lower/raise cycle, because there is no outgoing weapon to lower.
            if self.weapon().config().is_unarmed() {
                self.equip_weapon(idx);
            }
            log::info!("picked up {} (+{rounds} rounds)", cfg.name);
        } else {
            self.weapons[idx].add_reserve(rounds);
            log::info!("picked up {rounds} rounds for {}", cfg.name);
        }

        let vol = if fresh { GUN_VOL } else { AMMO_VOL };
        if let Some(audio) = self.audio.as_mut() {
            audio.play(PICKUP_SOUND, vol);
        }
        self.pickup_message = if fresh {
            cfg.name.to_ascii_uppercase()
        } else {
            format!("{} AMMO", cfg.name.to_ascii_uppercase())
        };
        self.pickup_message_timer = PICKUP_MESSAGE_TIME;
        true
    }

    /// Equip weapon `idx` immediately — no lower/raise dip, meshes swapped on the
    /// spot. For picking a gun up off the floor while empty-handed, and for the
    /// loadout reset on death; the `Q` cycle keeps its animated switch.
    pub(crate) fn equip_weapon(&mut self, idx: usize) {
        if idx >= self.weapons.len() {
            return;
        }
        self.switching = false;
        self.switch_target = idx;
        self.weapon_mut().cancel_reload();
        self.weapon_index = idx;
        let cfg = *self.weapon().config();
        let (gun, muzzle) = load_weapon_models(&cfg);
        self.gun_model = gun;
        self.muzzle_model = muzzle;
        self.models_dirty = true;
    }

    /// Strip the player back to empty hands: every weapon rebuilt from its config
    /// (so magazines **and** reserves are gone), ownership back to the unarmed slot
    /// alone. Called on respawn — dying costs you what you were carrying, which is
    /// what makes the pickups on the floor worth crossing the level for.
    ///
    /// `OWN_ALL=1` is exempt: it exists so a whole arsenal can be judged in one
    /// session, and taking the guns away on every death would defeat it.
    pub(crate) fn reset_loadout(&mut self) {
        if self.own_all {
            return;
        }
        let arsenal = self.arsenal.weapons();
        for (i, cfg) in arsenal.iter().enumerate() {
            // `empty`, not `new`: a fresh `Weapon` comes with a full magazine and ten
            // more in reserve, so rebuilding with `new` would have *restocked* the
            // whole arsenal on death instead of stripping it — the bug this line is
            // the fix for, and one no test would have caught by looking at ownership
            // alone.
            self.weapons[i] = Weapon::empty(*cfg);
        }
        let unarmed = arsenal
            .iter()
            .position(|w| w.is_unarmed())
            .unwrap_or(0);
        self.owned.iter_mut().for_each(|o| *o = false);
        self.owned[unarmed] = true;
        self.equip_weapon(unarmed);
        log::info!("loadout reset — empty-handed");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ecs::{ComponentData, EntityData};
    use crate::world::tools::spawn_point::tests::{big_room, place_pad};

    /// The live index of the weapon named `name`.
    pub(crate) fn weapon_idx(world: &World, name: &str) -> usize {
        world
            .arsenal
            .weapons()
            .iter()
            .position(|w| w.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the live arsenal"))
    }

    /// Author a pickup entity at `pos` through the same component list a placement
    /// writes, with model bounds registered so it is drawable/selectable.
    pub(crate) fn place_pickup(world: &mut World, mesh: MeshId, pos: Vec3, p: Pickup) {
        world.register_prop_bounds(
            mesh,
            Vec3::new(-0.3, 0.0, -0.3),
            Vec3::new(0.3, 0.4, 0.3),
        );
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::Renderable { mesh },
                ComponentData::Pickup {
                    kind: p.kind,
                    weapon: p.weapon.to_string(),
                    mags: p.mags,
                    respawn: p.respawn,
                },
            ],
        });
    }

    /// A room with a pad, entered in HUNT with no hunters — the pickup harness.
    fn arena() -> World {
        let mut world = big_room(30.0);
        world.set_spawn_enemies(false);
        world.set_score_limit(0);
        place_pad(&mut world, Vec3::new(15.0, 0.0, 15.0), 0.0);
        world
    }

    /// Walk the player onto `pos` and tick one step, so the grab test runs against
    /// the real per-step path rather than a hand-called grant.
    fn stand_on(world: &mut World, pos: Vec3) {
        if let Some(c) = world.character.as_mut() {
            c.pos = pos;
        }
        world.pickup_step(1.0 / 60.0);
    }

    /// You start a deathmatch empty-handed: the unarmed slot is equipped, it is the
    /// only thing owned, and pulling the trigger does nothing at all.
    #[test]
    fn the_player_starts_unarmed_and_cannot_fire() {
        let world = World::new();
        assert!(
            world.weapon().config().is_unarmed(),
            "started holding {:?}",
            world.weapon().config().name
        );
        assert_eq!(
            world.owned.iter().filter(|&&o| o).count(),
            1,
            "exactly one slot owned at the start"
        );

        // The fire gate, at the `Weapon` level: no shot, and no dry-click either.
        let mut w = Weapon::new(crate::combat::config::UNARMED);
        for _ in 0..10 {
            assert!(!w.update(1.0 / 60.0, true), "unarmed fired a shot");
            w.update(1.0 / 60.0, false);
        }
        assert!(w.take_cues().is_empty(), "unarmed queued a sound");
    }

    /// Walking over a gun grants it, loads it, and — because you were empty-handed —
    /// puts it in your hands. The last part is the one that would otherwise leave a
    /// player standing on a rifle holding nothing.
    #[test]
    fn collecting_a_gun_owns_it_and_equips_it_when_empty_handed() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        place_pickup(&mut world, MeshId::WeaponPickup, at, Pickup::weapon("AR33"));
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let ar33 = weapon_idx(&world, "AR33");
        assert!(!world.owns_weapon(ar33), "not owned before collection");

        stand_on(&mut world, at);
        assert!(world.owns_weapon(ar33), "collecting the gun grants it");
        assert_eq!(
            world.weapon().config().name,
            "AR33",
            "an empty-handed player equips what they pick up"
        );
        let (mag, reserve) = world.weapon_ammo(ar33).unwrap();
        assert!(mag > 0 && reserve > 0, "arrived loaded: {mag}/{reserve}");
    }

    /// An ammo crate feeds the weapon it was authored for and **only** that one, and
    /// banks the rounds even for a gun the player hasn't found yet.
    #[test]
    fn an_ammo_crate_feeds_only_its_linked_weapon() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        place_pickup(&mut world, MeshId::AmmoPickupGreen, at, Pickup::ammo("Sniper Rifle"));
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let sniper = weapon_idx(&world, "Sniper Rifle");
        let shotgun = weapon_idx(&world, "Shotgun");
        let before = world.weapon_ammo(sniper).unwrap().1;
        let other_before = world.weapon_ammo(shotgun).unwrap().1;

        stand_on(&mut world, at);
        let after = world.weapon_ammo(sniper).unwrap().1;
        assert!(after > before, "sniper reserve grew: {before} → {after}");
        assert_eq!(
            world.weapon_ammo(shotgun).unwrap().1,
            other_before,
            "an unrelated weapon's reserve moved"
        );
        // Ammo alone never hands you the gun.
        assert!(
            !world.owns_weapon(sniper),
            "an ammo crate must not grant the weapon"
        );
    }

    /// A collected pickup goes away, comes back on its authored clock, and can be
    /// collected again. `respawn == 0` is gone for the round instead.
    #[test]
    fn a_taken_pickup_returns_on_its_clock_or_never() {
        let mut world = arena();
        let repeat = Vec3::new(15.0, 0.0, 12.0);
        let once = Vec3::new(15.0, 0.0, 18.0);
        place_pickup(
            &mut world,
            MeshId::AmmoPickupTan,
            repeat,
            Pickup { respawn: 3.0, ..Pickup::ammo("Klobb") },
        );
        place_pickup(
            &mut world,
            MeshId::AmmoPickupTan,
            once,
            Pickup { respawn: 0.0, ..Pickup::ammo("Klobb") },
        );
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let klobb = weapon_idx(&world, "Klobb");
        let taken_count = |w: &World| {
            w.ecs.world().query::<&Pickup>().iter().filter(|p| p.taken()).count()
        };

        stand_on(&mut world, repeat);
        stand_on(&mut world, once);
        assert_eq!(taken_count(&world), 2, "both collected");
        let after_two = world.weapon_ammo(klobb).unwrap().1;

        // Step past the 3 s clock, standing clear so nothing is re-collected.
        if let Some(c) = world.character.as_mut() {
            c.pos = Vec3::new(15.0, 0.0, 15.0);
        }
        for _ in 0..(3.5 * 60.0) as usize {
            world.pickup_step(1.0 / 60.0);
        }
        assert_eq!(taken_count(&world), 1, "the timed one came back, the other did not");

        // And the returned one is collectable again.
        stand_on(&mut world, repeat);
        assert!(
            world.weapon_ammo(klobb).unwrap().1 > after_two,
            "the respawned crate gave rounds a second time"
        );
        // The never-returning one stays gone however long we wait.
        for _ in 0..(60.0 * 60.0) as usize {
            world.pickup_step(1.0 / 60.0);
        }
        let gone = world
            .ecs
            .world()
            .query::<(&Transform, &Pickup)>()
            .iter()
            .any(|(t, p)| t.pos == once && p.taken());
        assert!(gone, "a respawn of 0 must never come back");
    }

    /// Dying costs you everything: the guns, the loaded magazines and the reserves.
    /// This is the rule the whole feature hangs on — without it the level is a
    /// one-lap shopping trip.
    #[test]
    fn death_strips_the_loadout_back_to_empty_hands() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        place_pickup(&mut world, MeshId::WeaponPickup, at, Pickup::weapon("RC-P90"));
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let rcp90 = weapon_idx(&world, "RC-P90");
        stand_on(&mut world, at);
        assert!(world.owns_weapon(rcp90), "armed before dying");

        world.take_player_damage(1e6);
        let dt = 1.0 / 60.0;
        let input = InputState::default();
        for _ in 0..((RESPAWN_DELAY + 0.5) / dt) as usize {
            world.fixed_step(dt, &input);
        }
        assert!(!world.is_player_dead(), "respawned");
        assert!(!world.owns_weapon(rcp90), "kept a gun through death");
        assert!(
            world.weapon().config().is_unarmed(),
            "respawned holding {:?}",
            world.weapon().config().name
        );
        assert_eq!(
            world.weapon_ammo(rcp90).unwrap(),
            (0, 0),
            "the magazine and reserve must go with the gun"
        );
    }

    /// A pickup is something you walk **through**: no collider for the player's
    /// capsule, no footprint in the nav grid for the hunters. Both fall out of the
    /// catalog gating rather than a special case, which is exactly the sort of
    /// implicit behaviour worth pinning.
    #[test]
    fn pickups_are_walk_through_for_both_sides() {
        let mut world = World::new();
        world.initial_meshes();
        // Both flavours: the gun (no catalog row at all) and an ammo crate, which IS
        // a catalog prop and would otherwise be voxelized solid like any crate.
        place_pickup(
            &mut world,
            MeshId::WeaponPickup,
            Vec3::new(3.0, 0.0, 3.0),
            Pickup::weapon("AR33"),
        );
        place_pickup(
            &mut world,
            MeshId::AmmoPickupTan,
            Vec3::new(3.0, 0.0, 4.0),
            Pickup::ammo("AR33"),
        );
        world.spawn_prop_colliders();
        assert!(
            world.prop_colliders.is_empty(),
            "a pickup must not be solid to the player"
        );
        assert!(
            world.prop_solid_boxes().is_empty(),
            "a pickup must not block the hunters' nav grid"
        );
        // A destructible crate right next to them still does both, so the exclusion is
        // specific to pickups rather than something that quietly turned props off.
        let id = world.ecs.alloc_id();
        world.register_prop_bounds(
            MeshId::WoodenCrate,
            Vec3::new(-0.5, 0.0, -0.5),
            Vec3::new(0.5, 1.0, 0.5),
        );
        world.ecs.spawn_authored(&crate::ecs::EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: [5.0, 0.0, 5.0],
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::Renderable { mesh: MeshId::WoodenCrate },
            ],
        });
        world.spawn_prop_colliders();
        assert_eq!(world.prop_colliders.len(), 1, "the crate is still solid");
        assert_eq!(world.prop_solid_boxes().len(), 1, "the crate still blocks nav");
    }

    /// You can click the gun you can see.
    ///
    /// A weapon pickup is drawn floating 30 cm up, but the prop anchor puts a mesh's
    /// `min.y` on the floor — so bounds taken around the *hovering* mesh would have
    /// that lift cancelled straight back out, leaving the click box on the ground
    /// under a gun nobody can select. The sentry gun hit the same class of bug from
    /// the other direction, which is why this is pinned rather than eyeballed.
    #[test]
    fn a_weapon_pickups_click_box_covers_the_gun_you_can_see() {
        let mut world = World::new();
        let at = Vec3::new(4.0, 0.0, 4.0);
        let (min, max) = weapon_pickup_bounds();
        world.register_prop_bounds(MeshId::WeaponPickup, min, max);
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&crate::ecs::EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: at.to_array(),
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::Renderable { mesh: MeshId::WeaponPickup },
                ComponentData::Pickup {
                    kind: PickupKind::Weapon,
                    weapon: "AR33".to_string(),
                    mags: 2,
                    respawn: 0.0,
                },
            ],
        });
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        let (lo, hi) = world.prop_world_aabb(e).expect("a pickup has a click box");

        // Sample the drawn gun's centre across a full bob cycle: every one of them has
        // to fall inside the box, at rest and at both extremes.
        for step in 0..12 {
            world.advance_pickups(1.0 / (BOB_RATE * 12.0));
            let centre = world.weapon_pickup_draws()[0].1.transform_point3(Vec3::ZERO);
            assert!(
                centre.y >= lo.y && centre.y <= hi.y,
                "step {step}: the gun is drawn at y={} but the click box is {}..{}",
                centre.y,
                lo.y,
                hi.y
            );
        }
        // And the box stands on the floor it was authored on, not floating with it —
        // you should be able to click the base as well as the gun.
        assert!((lo.y - at.y).abs() < 1e-3, "click box starts at y={}", lo.y);
    }

    /// The authored weapon link survives a save/load round trip — the reason the
    /// component stores a name rather than an index.
    #[test]
    fn the_weapon_link_survives_a_save_and_load() {
        let mut world = World::new();
        place_pickup(
            &mut world,
            MeshId::AmmoPickupGreen,
            Vec3::new(2.0, 0.0, 2.0),
            Pickup { mags: 5, respawn: 7.5, ..Pickup::ammo("Cougar Magnum") },
        );
        let saved = world.ecs.save_authored();

        let mut loaded = World::new();
        loaded.ecs.load_authored(&saved);
        let p = loaded
            .ecs
            .world()
            .query::<&Pickup>()
            .iter()
            .map(|p| *p)
            .next()
            .expect("the pickup came back");
        assert_eq!(p.weapon, "Cougar Magnum");
        assert_eq!(p.kind, PickupKind::Ammo);
        assert_eq!(p.mags, 5);
        assert_eq!(p.respawn, 7.5);
        assert_eq!(p.cooldown, 0.0, "runtime state must not persist");
    }

    /// A gun on the ground draws on the world-space weapon channel, hovering above
    /// where it was authored — and vanishes while taken. It draws in BUILD too,
    /// because the editor has to show what it is placing.
    #[test]
    fn a_weapon_pickup_hovers_and_disappears_when_taken() {
        let mut world = World::new();
        let at = Vec3::new(4.0, 0.0, 4.0);
        place_pickup(&mut world, MeshId::WeaponPickup, at, Pickup::weapon("Klobb"));

        let draws = world.weapon_pickup_draws();
        assert_eq!(draws.len(), 1, "one gun drawn in BUILD");
        assert_eq!(draws[0].0, "Klobb", "keyed by weapon name for the render library");
        let centre = draws[0].1.transform_point3(Vec3::ZERO);
        assert!(
            centre.y > at.y + HOVER_HEIGHT - BOB_AMPLITUDE - 1e-3,
            "the gun should float above the floor, got y={}",
            centre.y
        );

        // It turns: the same model point lands somewhere else a moment later.
        let before = draws[0].1.transform_point3(Vec3::X);
        world.advance_pickups(0.4);
        let after = world.weapon_pickup_draws()[0].1.transform_point3(Vec3::X);
        assert!(before.distance(after) > 1e-3, "the gun is not turning");

        // And a collected one is not drawn at all.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Pickup)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.ecs.world_mut().query_one_mut::<&mut Pickup>(e).unwrap().cooldown = 5.0;
        assert!(
            world.weapon_pickup_draws().is_empty(),
            "a taken pickup is still on screen"
        );
    }

    /// The grab is a column, not a sphere: you cannot collect the gun on the balcony
    /// by standing underneath it, and you cannot collect one across the room.
    #[test]
    fn a_pickup_out_of_reach_is_not_collected() {
        let mut world = arena();
        let feet = Vec3::new(15.0, 0.0, 15.0);
        // Directly overhead but a storey up, level but well across the floor, and one
        // just inside reach as the control.
        place_pickup(&mut world, MeshId::AmmoPickupTan, feet + Vec3::Y * 3.0, Pickup::ammo("Klobb"));
        place_pickup(&mut world, MeshId::AmmoPickupTan, feet + Vec3::Z * 4.0, Pickup::ammo("Klobb"));
        place_pickup(
            &mut world,
            MeshId::AmmoPickupTan,
            feet + Vec3::new(0.5, 1.2, 0.0),
            Pickup::ammo("Klobb"),
        );
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        stand_on(&mut world, feet);
        let taken = world
            .ecs
            .world()
            .query::<&Pickup>()
            .iter()
            .filter(|p| p.taken())
            .count();
        assert_eq!(
            taken, 1,
            "only the one within reach should be collected (a storey up and a room \
             away must both be out of reach)"
        );
    }

    /// Switching from one gun to another in the palette keeps the tool armed, and
    /// re-clicking the armed gun disarms it.
    ///
    /// Every gun shares the one `WeaponPickup` mesh id, so routing a gun switch
    /// through `arm_prop_placement` hits its *toggle* and disarms — the panel would
    /// still show a gun selected while clicking the floor did nothing.
    #[test]
    fn switching_gun_keeps_the_tool_armed_and_reclicking_disarms() {
        let mut world = World::new();
        world.arm_weapon_pickup("AR33");
        assert!(world.is_placing_prop(), "arming a gun arms the tool");
        assert_eq!(world.armed_pickup_weapon(), Some("AR33"));

        world.arm_weapon_pickup("Shotgun");
        assert!(world.is_placing_prop(), "switching guns must not disarm");
        assert_eq!(world.armed_pickup_weapon(), Some("Shotgun"));

        world.arm_weapon_pickup("Shotgun");
        assert!(!world.is_placing_prop(), "re-clicking the armed gun disarms");

        // The ammo/respawn the author has been tuning survives a gun switch — only
        // the weapon changes.
        world.arm_weapon_pickup("Klobb");
        world.set_pickup_draft(Pickup { mags: 9, respawn: 42.0, ..world.pickup_draft() });
        world.arm_weapon_pickup("Sniper Rifle");
        let d = world.pickup_draft();
        assert_eq!((d.mags, d.respawn), (9, 42.0), "settings reset on a gun switch");
        assert_eq!(d.weapon, "Sniper Rifle");
    }

    /// A new round restocks the level. Without this the second round of a session
    /// starts stripped of everything the first one collected — a pickup with no
    /// respawn time never comes back on its own.
    #[test]
    fn a_new_round_puts_every_pickup_back() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        place_pickup(
            &mut world,
            MeshId::WeaponPickup,
            at,
            Pickup { respawn: 0.0, ..Pickup::weapon("AR33") },
        );
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        stand_on(&mut world, at);
        assert!(
            world.ecs.world().query::<&Pickup>().iter().all(|p| p.taken()),
            "collected"
        );
        world.restart_round();
        assert!(
            world.ecs.world().query::<&Pickup>().iter().all(|p| !p.taken()),
            "a new round must restock the level"
        );
        // And the player comes into it empty-handed, having lost the gun they found.
        assert!(world.weapon().config().is_unarmed(), "a new round starts unarmed");
    }

    // ── The hunters ─────────────────────────────────────────────────────────────

    /// A room with pads and a weapon on the floor, entered in HUNT with `n` hunters.
    fn hunter_arena(n: usize, gun_at: Vec3) -> World {
        let mut world = big_room(40.0);
        world.set_wave_size(n);
        world.set_score_limit(0);
        world.set_spawn_enemies(true);
        // Pads far from the gun, so a hunter has to actually travel to it.
        place_pad(&mut world, Vec3::new(6.0, 0.0, 6.0), 0.0);
        place_pad(&mut world, Vec3::new(34.0, 0.0, 6.0), 0.0);
        place_pickup(&mut world, MeshId::WeaponPickup, gun_at, Pickup::weapon("AR33"));
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();
        world
    }

    /// Hunters spawn **empty-handed** too, and an empty-handed hunter cannot shoot —
    /// so it has to go and find something, which is the whole reason the fetch
    /// behaviour exists.
    #[test]
    fn hunters_spawn_empty_handed_and_hold_no_gun() {
        let world = hunter_arena(2, Vec3::new(20.0, 0.0, 20.0));
        assert!(!world.enemies.is_empty(), "hunters spawned");
        for (i, inst) in world.enemies.iter().enumerate() {
            assert!(inst.weapon.is_unarmed(), "hunter {i} spawned holding a gun");
            assert_eq!(inst.loaded, 0, "hunter {i} has rounds with no gun");
            assert_eq!(inst.reserve, 0, "hunter {i} has a reserve with no gun");
        }
        // Nothing is drawn in their hands. Stated against the pickup draws rather than
        // as "the list is empty", because the gun lying on the floor rides that same
        // world-space channel — so the invariant is that *everything* being drawn is a
        // pickup and none of it is being held.
        assert_eq!(
            world.enemy_weapon_draws(1.6).len(),
            world.weapon_pickup_draws().len(),
            "an empty-handed hunter is drawing a weapon"
        );
    }

    /// An unarmed hunter **walks to the gun and arms itself** — the behaviour, end to
    /// end, through the real fixed step.
    #[test]
    fn an_unarmed_hunter_fetches_a_weapon_and_arms_itself() {
        let gun = Vec3::new(20.0, 0.0, 20.0);
        let mut world = hunter_arena(1, gun);
        let start = world.enemies[0].enemy.pos;
        assert!(
            world.hunter_fetch_target(&world.enemies[0]).is_some(),
            "an unarmed hunter should want the gun on the floor"
        );

        let dt = 1.0 / 60.0;
        let input = InputState::default();
        let mut armed_after = None;
        for f in 0..(30.0 / dt) as usize {
            world.fixed_step(dt, &input);
            if !world.enemies[0].weapon.is_unarmed() {
                armed_after = Some(f as f32 * dt);
                break;
            }
        }
        let t = armed_after.unwrap_or_else(|| {
            panic!(
                "the hunter never armed itself in 30 s — it got from {start:?} to {:?}, \
                 gun at {gun:?}",
                world.enemies[0].enemy.pos
            )
        });
        println!("hunter armed itself {t:.1}s after spawning");
        let inst = &world.enemies[0];
        assert_eq!(inst.weapon.name, "AR33", "it picked up the gun that was there");
        assert!(inst.loaded > 0, "the gun it found is loaded");
        assert!(inst.reserve > 0, "and it has spare magazines");
        // The pickup is gone from the floor — the player cannot also have it.
        assert!(
            world.ecs.world().query::<&Pickup>().iter().all(|p| p.taken()),
            "the hunter armed itself but the gun is still lying there"
        );
    }

    /// A hunter that has run **completely dry** stops fighting and goes for ammo, and
    /// a crate for its own gun refills it.
    ///
    /// The dry state is behaviourally the same as being unarmed, which is the point:
    /// both are "cannot shoot", and both are answered by walking somewhere.
    #[test]
    fn a_dry_hunter_wants_ammo_and_a_crate_refills_it() {
        let mut world = hunter_arena(1, Vec3::new(20.0, 0.0, 20.0));
        // Arm it by hand with a gun, then empty it completely.
        let ar33 = *world
            .arsenal
            .weapons()
            .iter()
            .find(|w| w.name == "AR33")
            .unwrap();
        let def = crate::combat::enemy_def_for(&ar33);
        {
            let inst = &mut world.enemies[0];
            inst.weapon = def;
            inst.loaded = def.clip;
            inst.reserve = def.clip;
            assert!(World::hunter_want(inst).is_none(), "a loaded hunter wants nothing");
            inst.loaded = 0;
            inst.reserve = 0;
        }
        assert_eq!(
            World::hunter_want(&world.enemies[0]),
            Some(HunterWant::Ammo),
            "a hunter with a gun and no rounds anywhere wants ammo"
        );

        // A crate for a DIFFERENT gun is no use to it…
        let at = world.enemies[0].enemy.pos + Vec3::new(1.0, 0.0, 0.0);
        place_pickup(&mut world, MeshId::AmmoPickupTan, at, Pickup::ammo("Klobb"));
        world.pickup_step(1.0 / 60.0);
        assert_eq!(world.enemies[0].reserve, 0, "it took rounds it cannot chamber");

        // …and one for its own gun is.
        place_pickup(&mut world, MeshId::AmmoPickupGreen, at, Pickup::ammo("AR33"));
        world.pickup_step(1.0 / 60.0);
        assert!(
            world.enemies[0].reserve > 0,
            "the crate for its own weapon did not refill it"
        );
        assert!(World::hunter_want(&world.enemies[0]).is_none(), "it is armed again");
    }

    /// An empty-handed hunter does **not** engage: it commits to [`AiState::Fetch`] and
    /// is steered by the gun's position rather than the player's.
    ///
    /// **`!is_engaged()` is not enough on its own**, and this test is the record of why.
    /// It used to assert only that, over 5 s, plus that the hunter's *search* target was
    /// the gun — and it passed for the whole life of the bug, because
    /// [`crate::enemy::Enemy::is_engaged`] covers `Alert…Peek` and **not** `Investigate`.
    /// The hunter was in `Investigate` walking straight at the player the entire time,
    /// while the search target it was not reading happened to hold the right point. The
    /// state and the destination both have to be asserted, and the geometry is pinned
    /// separately in the AI lab (`an_unarmed_hunter_walks_to_the_gun_not_to_the_player`),
    /// which is the only place direction can actually be measured.
    #[test]
    fn an_unarmed_hunter_ignores_the_player_and_goes_shopping() {
        let gun = Vec3::new(34.0, 0.0, 34.0);
        // Gun on the far side, player right next to the hunter's pad.
        let mut world = hunter_arena(1, gun);
        let dt = 1.0 / 60.0;
        let input = InputState::default();
        for _ in 0..(4.0 / dt) as usize {
            // Park the player on top of the hunter every step: impossible to miss.
            let at = world.enemies[0].enemy.pos + Vec3::new(1.5, 0.0, 0.0);
            if let Some(c) = world.character.as_mut() {
                c.pos = at;
            }
            // …and shout, so the hunter holds a fresh last-known position. That is what
            // used to hand the decision to `Investigate`.
            world.enemies[0].enemy.hear_noise(at);
            world.fixed_step(dt, &input);
            assert!(
                !world.enemies[0].enemy.is_engaged(),
                "an empty-handed hunter engaged the player instead of finding a gun"
            );
            assert_eq!(
                world.enemies[0].enemy.state(),
                crate::enemy::AiState::Fetch,
                "an empty-handed hunter is doing something other than shopping"
            );
        }
        // It is steered by the gun, not by the player.
        let target = world.enemies[0].enemy.fetch_target().expect("a fetch target");
        assert!(target.distance(gun) < 2.0, "its fetch target is {target:?}, not the gun");
    }

    /// An empty-handed hunter with **nothing to go and get** still refuses to fight —
    /// it does not charge, and above all it does not "fire".
    ///
    /// The playtest defect, exactly: suppression keyed off having *found* a pickup, so
    /// once the floor was empty (the player can take guns it already owns) every
    /// unarmed hunter fell back into ordinary engagement — walking up to the player,
    /// raising its arms and logging `hunter firing (Unarmed, primary)` while dealing no
    /// damage. Two separate bugs in one symptom: the suppression gate and an ungated
    /// `start_enemy_fire`.
    #[test]
    fn an_unarmed_hunter_with_no_gun_available_neither_engages_nor_fires() {
        // A level with a weapon pickup (so hunters spawn unarmed) that is then taken,
        // leaving them nothing to fetch.
        let mut world = hunter_arena(1, Vec3::new(20.0, 0.0, 20.0));
        let all: Vec<hecs::Entity> = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Pickup)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for e in all {
            world.ecs.world_mut().query_one_mut::<&mut Pickup>(e).unwrap().cooldown = GONE;
        }
        assert!(world.enemies[0].weapon.is_unarmed(), "the hunter has no gun");
        assert!(
            world.hunter_fetch_target(&world.enemies[0]).is_none(),
            "nothing on the floor to fetch — the precondition for this test"
        );

        let dt = 1.0 / 60.0;
        let input = InputState::default();
        for _ in 0..(5.0 / dt) as usize {
            // Stand right on top of it: impossible to miss, impossible to excuse.
            let at = world.enemies[0].enemy.pos + Vec3::new(1.5, 0.0, 0.0);
            if let Some(c) = world.character.as_mut() {
                c.pos = at;
            }
            world.fixed_step(dt, &input);
            assert!(
                !world.enemies[0].enemy.is_engaged(),
                "an empty-handed hunter engaged the player with no gun to be had"
            );
            assert!(
                world.enemies[0].fire_elapsed.is_none(),
                "an empty-handed hunter started a fire burst"
            );
        }
        // And the player is untouched by it.
        assert_eq!(
            world.player_health(),
            PLAYER_MAX_HEALTH,
            "an unarmed hunter dealt damage"
        );
    }

    /// The player does **not** hoover a gun it already owns off the floor unless it
    /// actually needs the rounds — which is what leaves anything for the hunters.
    ///
    /// With `OWN_ALL=1` (or simply a good round) the player owns everything, and a
    /// duplicate gun used to be consumed as ammo unconditionally. That starved the
    /// empty-handed hunters of the only thing they can use.
    #[test]
    fn a_gun_you_already_own_and_dont_need_stays_on_the_floor() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        place_pickup(&mut world, MeshId::WeaponPickup, at, Pickup::weapon("AR33"));
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let ar33 = weapon_idx(&world, "AR33");
        // Own it, with plenty of spare rounds — the shop's stock is far more than a
        // pickup carries.
        world.owned[ar33] = true;
        world.weapons[ar33].stock_bought();
        stand_on(&mut world, at);
        assert!(
            world.ecs.world().query::<&Pickup>().iter().all(|p| !p.taken()),
            "a gun the player has no use for was taken off the floor anyway"
        );

        // Run the reserve down and it becomes worth taking again.
        world.weapons[ar33] = crate::combat::Weapon::empty(crate::combat::config::AR33);
        stand_on(&mut world, at);
        assert!(
            world.ecs.world().query::<&Pickup>().iter().all(|p| p.taken()),
            "a dry player should still be able to top up from a duplicate gun"
        );
    }

    /// On a level that authors **no** weapon pickups, everybody starts armed.
    ///
    /// Graceful degradation, and the same guard Perfect Dark puts on spawn pads
    /// (`if (g_NumSpawnPoints > 0)`): an empty-handed start is only playable where
    /// there is something to find. Without this every pre-pickups level — and every
    /// AI-lab arena — becomes a room full of hunters wandering after guns that do not
    /// exist.
    #[test]
    fn a_level_with_no_pickups_arms_everyone_as_before() {
        let mut world = big_room(40.0);
        world.set_wave_size(2);
        world.set_score_limit(0);
        place_pad(&mut world, Vec3::new(6.0, 0.0, 6.0), 0.0);
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        assert!(!world.has_weapon_pickups());
        world.toggle_mode();

        for (i, inst) in world.enemies.iter().enumerate() {
            assert!(!inst.weapon.is_unarmed(), "hunter {i} has nothing to fight with");
            assert!(inst.reserve > 0, "hunter {i} has no spare ammo");
        }
        assert!(
            !world.weapon().config().is_unarmed(),
            "the player was left empty-handed on a level with no guns"
        );
        assert!(world.weapon().magazine() > 0, "the fallback sidearm is loaded");
    }

    /// A pickup for a weapon this session's arsenal doesn't have is inert rather
    /// than crashing or handing out someone else's gun — the `ARSENAL=` mismatch
    /// case the name-keyed link is designed to survive.
    #[test]
    fn a_pickup_for_an_absent_weapon_is_inert() {
        let mut world = arena();
        let at = Vec3::new(15.0, 0.0, 12.0);
        // A real Perfect Dark weapon, authored into a level running the GoldenEye
        // arsenal (the default).
        place_pickup(&mut world, MeshId::WeaponPickup, at, Pickup::weapon("FarSight XR-20"));
        world.camera.pos = Vec3::new(15.0, 2.0, 15.0);
        world.toggle_mode();

        let owned_before = world.owned.iter().filter(|&&o| o).count();
        stand_on(&mut world, at);
        assert_eq!(
            world.owned.iter().filter(|&&o| o).count(),
            owned_before,
            "an absent weapon must not grant anything"
        );
        // And it stays on the floor rather than being silently consumed.
        let still_there = world
            .ecs
            .world()
            .query::<&Pickup>()
            .iter()
            .all(|p| !p.taken());
        assert!(still_there, "an ungrantable pickup was consumed for nothing");
    }
}
