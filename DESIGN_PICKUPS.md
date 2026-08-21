# Pickups — guns and ammo on the ground

The pivot this feature encodes: the game stops handing you an arsenal and starts
making you find one. You spawn **empty-handed**, the level is stocked with weapons
and ammo crates the author placed, and dying costs you everything you were carrying.
That is deathmatch, and it is a different game from the shop-and-siege economy the
credits wallet was built for (which stays in the codebase, unbroken, for when we go
back to it).

## What already existed, and what didn't

There was **no pickup concept at all** — `enemy.rs:2078` documents the AI's weapon
options with the line "7. pick up a weapon ..... no pickups in this game."

But every seam a pickup needs was already there, which is why this lands as an
addition rather than a rewrite:

| What a pickup needs | What the codebase already had |
| --- | --- |
| A per-weapon "you have this" flag | `World::owned: Vec<bool>` + `buy_weapon`, from the shop |
| A way to hand over rounds | `Weapon::add_reserve`, from the shop's ammo purchase |
| An authored, saveable, editable object | The ECS prop pipeline (palette → ghost → gizmo → undo → level file) |
| Runtime state that must not persist | The turret's `spawn_turrets`/`clear_turrets` BUILD↔HUNT bake |
| A world-space gun mesh | `upload_enemy_weapon` — the whole arsenal, already uploaded at startup, keyed by name |

The last row is the one that shaped the design. A gun lying on the floor is **not**
a catalog prop: the prop channel draws GLBs from `assets/props/`, while the guns live
in a separate render library keyed by weapon name that the hunters already draw
through. So weapon pickups ride that library and never enter `props::CATALOG`. Ammo
crates *are* ordinary props and do.

## The unarmed slot

You now start holding nothing. Rather than making "no weapon" a new state on the
`World` — which would mean `weapon()` returns an `Option` and every HUD, viewmodel
and fire path grows a branch — **Unarmed is a weapon**: a `WeaponStats` at index 0 of
every arsenal with no mesh, no sounds and a **zero-size magazine**.

`magazine_size == 0` is the firing gate, and it is checked in exactly one place
(`Weapon::update` returns early), so an unarmed player neither fires nor dry-clicks.
There is no punch yet; the slot is deliberately inert and is where a melee function
lands later.

The cost of this choice is that `config::WEAPONS[0]` is no longer the PP7. Nothing in
the shipping code indexed that (the starting sidearm is found **by name**, which was
already the codebase's convention for weapon lookups), but several tests pinned
`weapon_index = 0` with a `// PP7` comment; they now resolve the index by name.

## The `Pickup` component

```rust
pub struct Pickup {
    pub kind: PickupKind,        // Weapon | Ammo
    pub weapon: &'static str,    // which gun — an arsenal display name
    pub mags: u32,               // magazines granted
    pub respawn: f32,            // seconds to return; 0 = gone for good
    pub cooldown: f32,           // runtime: >0 means taken, counting back
}
```

**The weapon is named, not indexed.** This follows the rule the shop and the enemy
weapon defs already set (`shop::listed_price`, `enemy_def_for` are both name-keyed on
purpose) so that reordering a weapon table cannot silently repoint an authored
pickup. The name resolves back to a `&'static str` through `arsenal::resolve_name`,
which scans **both** families rather than the live arsenal — so a level authored under
`ARSENAL=pd` still loads its pickups under `ARSENAL=ge`. Such a pickup is present and
selectable but cannot be granted (the weapon isn't in play); it says so once in the
log instead of vanishing from the file.

`cooldown` is runtime and is **not persisted**, for the same reason `DoorGeom` and
`Turret` aren't: an authored level always opens with every pickup on the floor.

## Authoring

The **O** panel's OBJECTS tab gains a `Pickups` section holding three entries:

- **Weapon Pickup** — the gun itself, whichever one the `Weapon` combo names.
- **Ammo Crate (Tan)** — `ammo_crate.glb`, the existing GoldenEye ammo box.
- **Ammo Crate (Green)** — a Setup-Editor OBJ the user supplied, installed at
  `assets/props/green_ammo_crate/`.

The two crates are **a visual choice only**. Which weapon a crate feeds is the
`Weapon` combo, exactly as for a gun pickup — a green crate can hold sniper rounds
and a tan one shotgun shells.

One settings block serves both authoring and editing, the same shape the door tool
uses: it edits the *draft* (what the next placed pickup gets) while the tool is
armed, and the *selected instance* when one is picked in the 3D view.

- **Weapon** — combo over the live arsenal (Unarmed excluded).
- **Magazines** — how many of that weapon's magazines the pickup hands over.
- **Returns after** — respawn seconds, `0` = never.

## What happens when you walk over one

`pickup_step` runs per fixed step in HUNT, before the hunter FSM.

A **weapon** pickup you don't own grants ownership plus a full magazine and `mags`
magazines of reserve, and **auto-equips** if you were unarmed — otherwise finding a
gun while holding nothing would leave you holding nothing. One you already own is
treated as ammo, which is what GoldenEye does with a second copy of a gun.

An **ammo** pickup adds `mags × magazine_size` to that weapon's reserve whether or
not you own the gun, so the rounds bank for when you find it.

Both then start the respawn clock, or are gone if `respawn == 0`. A taken pickup is
skipped by the draw list, so the shelf is visibly empty until it comes back.

Guns **hover and turn slowly**; crates sit still. Neither has a collider and neither
is baked into the nav grid — a pickup is something you walk *through*, and this falls
out of the existing design rather than needing a carve-out: both the collider bake
and the nav solids list are gated on a `props::def` lookup, and a weapon pickup has
no catalog row at all.

## The hunters play by the same rule

Hunters spawn empty-handed too, which makes the floor contested rather than a private
supply depot.

**A hunter with nothing to shoot goes shopping instead of fighting.** Two states drive
it, and they are behaviourally identical — holding nothing, and holding a gun with no
rounds anywhere. Both mean "cannot shoot", and both are answered by walking somewhere.

Two independent inputs say so, and it takes both:

- **Suppression** — a shopping hunter has its **knowledge inputs suppressed**
  (`set_detectable` *and* `set_omniscient`), so it may not act on the player. Suppressing
  both is the non-obvious part: omniscience is *knowledge*, not perception, so it bypasses
  visibility entirely, and with only `set_detectable(false)` an omniscient hunter walked
  straight past the gun to fight bare-handed. That took a test to find.
- **A destination** — `AiState::Fetch`, a first-class scored behaviour with its own
  channel (`Enemy::set_fetch_target`, written by the `World` every step). It **outscores
  everything**: a hunter that cannot shoot has no better option, which makes this one of
  the few honestly unconditional entries in the utility table, and it is zero the moment
  the hunter can fight. All three decision layers execute it through one function —
  the utility scorer, the legacy FSM (so the kill-switch keeps the behaviour rather than
  degrading) and Perfect Dark's ladder, where it lands as the pick-up-a-weapon rung
  (`bot_pick_up_weapon`) whose port note used to read *"no pickups in this game"*.

Arrival is nav-grid arrival, so the hunter can end up parked short of something it can
never collect — a gun on a ledge, a pocket A\* cannot path into. After three seconds
standing at a fetch point that hasn't granted, it writes that pickup off and goes back to
searching for five before re-asking. A statue is the one outcome worse than a wasted trip.

**Hunter ammo is finite now, in both AI models.** It wasn't: a hunter had unlimited
magazines (like a PD bot, whose `botact_reload` simply refills), and rounds were only
counted under `AI=pd` at all. A hunter that can never run out has no reason to want an
ammo crate, so "prioritise picking up ammo" would have had nothing to prioritise.
Reloads now draw from a real `reserve`, and running dry is both models' problem. What
stays PD-only is the *tactical* clause — topping up a partial magazine while unseen.

Two deliberate limits: a hunter keeps its **spawn-time animation class and arm rig**
when it picks up a different gun (those are resolved per body at spawn and rebuilding
the layered animator mid-fight is a bigger job than this), and it never picks up a
matched pair — it finds one gun, not dual-wield.

### The first playtest found two bugs in one symptom

Reported as *"they come right for me and hold their hands up and they think they're
shooting me but nothing is happening"*, with `hunter firing (Unarmed, primary)` in the
log. Two independent causes:

**The suppression gate keyed off the wrong thing.** It asked whether the hunter had
*found* a pickup, not whether it *wanted* one — so the moment the floor ran dry, every
empty-handed hunter fell straight back into ordinary engagement and charged. It keys off
the want now, and a hunter with nothing to fetch keeps its ordinary fan-out search until
something reappears.

**`start_enemy_fire` was never gated.** The per-shot pump stopped the *rounds*, but the
burst still started — so an unarmed hunter played the entire firing animation and logged
that it was firing, while dealing no damage. Gating the pump but not the trigger is worse
than not gating at all, because it looks like broken combat rather than like a hunter
with no gun.

And the reason the floor ran dry: **the player could hoover up guns it already owned.** A
duplicate weapon pickup was consumed as ammo unconditionally, so a player carrying the
whole arsenal (`OWN_ALL=1`, or just a good round) stripped the level of the only thing an
empty-handed hunter can use. A gun you already own now only converts to ammo if you
actually need the rounds; otherwise it stays where it is.

`spawn_pickups` logs what the level holds and which rule that puts both sides under, so
the next occurrence of this is one line in the log rather than a session of guessing.

### The second playtest found the mechanism itself was wrong

Reported as *"they still b-line right for me with no gun in their hand. If I happen to be
near a gun and they walk over it by accident they'll pick it up but they don't purposely
go look for one."* Suppression worked; the destination never arrived.

Fetching was routed through the **search-target channel** (`assign_search_target`), on
the reasoning above — a hunter that cannot shoot drops into the blind states and walks
where it is told. It does not. `Search` is scoreable only while `last_known` is `None`,
and `last_known` is written the first time the hunter hears a gunshot or gets a squad
call. So once anything at all had happened, `Search` scored zero, the scorer picked
`Investigate` instead, and `Investigate`'s executor walks to **the player's last-known
position**. That is the beeline. The fetch point was read by `Search` alone, so it steered
nothing.

**Two writers on one field** is the other half of it: the search point's owner is the
`World`'s fan-out coordinator, which hands out a fresh one on arrival, so the sweep and
the shopping trip overwrote each other. Fetching is a *behaviour*, and it belongs in the
behaviour scorer with a channel of its own.

And the reason it survived a playtest and a test suite: **`Enemy::is_engaged()` does not
cover `Investigate`.** The test asserted `!is_engaged()` over five seconds and passed
while the hunter walked straight at the player. "Walks at me" and "walks at the gun"
differ only in direction, so the assertions are geometric now, in the AI lab
(`an_unarmed_hunter_walks_to_the_gun_not_to_the_player` and its ammo and `AI=pd`
siblings): a room with the gun on one side of the hunter and a noisily firing player on
the other, asserting the distance to the gun falls and the distance to the player does
not. That test fails on the old build in the first two seconds.

## No pickups authored means everybody starts armed

The gate on the whole empty-handed rule, for both sides. A level with no guns on the
floor cannot be played by anyone who starts without one: the player would have only the
shop, and the hunters would wander after something that does not exist.

This is the same guard Perfect Dark puts on spawn pads — `if (g_NumSpawnPoints > 0)`
before overriding the default entry (`playerreset.c:398`) — and it is what keeps every
pre-pickups level, and every AI-lab arena, working exactly as it did. It also fixed 19
failing tests in one change, which is a fair sign it was the missing rule rather than a
convenience.

`ARMED_HUNTERS=1` is the separate kill-switch for a playtest that wants armed hunters
on a level that *does* have pickups.

## Death costs you everything

`respawn_player` calls `reset_loadout`: every `Weapon` is rebuilt fresh (so magazines
*and* reserves are gone), ownership drops back to Unarmed alone, and the viewmodel is
marked dirty so the gun leaves your hands.

`OWN_ALL=1` is exempt. It is the dev flag for judging 33 guns in a session, and
making it lose them on every death would defeat the only reason it exists.

## Four things the implementation overturned

Each of these was a plausible assumption that measurement contradicted, so they are
recorded rather than quietly fixed.

**1. "Rebuild the weapons and the ammo is gone."** `Weapon::new` seeds `magazine =
magazine_size` and `reserve = magazine_size × 10`. So the first version of
`reset_loadout` — which rebuilt every `Weapon` from its config — *restocked* the whole
arsenal on death instead of stripping it, and no ownership-based test could have seen
it. Hence `Weapon::empty` (zero pools, the unowned state) and `Weapon::stock` (what
actually puts rounds in a gun). Every weapon now **starts** empty too, which meant the
shop had to stock a purchase explicitly (`stock_bought`) or a bought gun would arrive
with nothing in it.

**2. "An ammo crate is a prop, so let it behave like one."** Ammo crates *are* catalog
props, which means they were being voxelized into the nav grid — so hunters would path
around a crate the player walks straight through (a pickup has no collider). Pickups
are now excluded from `prop_solid_boxes` explicitly, which is what makes the crate
agree with the weapon pickup rather than with scenery that happens to be lootable.

**3. "Bounds around the mesh are the click target."** The prop anchor puts a model's
`min.y` on the floor. Bounds taken around a gun *drawn* 30 cm up therefore have that
lift cancelled straight back out, leaving the pick box on the ground under a gun that
cannot be selected — the same class of bug the sentry gun hit from the other
direction. `weapon_pickup_bounds` is the **column the gun floats in**, floor to the
top of its bob.

**4. "The HUD can print a weapon name."** It could not. The HUD font is a
code-defined 28-glyph atlas covering digits, punctuation and the letters of the
existing readouts, and a missing glyph is **dropped silently** — `KLOBB` would have
printed as `LOB`, `D5K (SILENCED)` as `D5 SILENED`. The banner is the first HUD string
whose content is data rather than a literal, so the alphabet is now complete and
`every_weapon_name_survives_the_pickup_banner` checks all 56 names in both arsenals
directly.

**5. "The green crate needs tinting."** It did not — the colour was in the asset and
the *loader* was dropping it. The GoldenEye Setup Editor writes per-vertex colour as
`#vcolor` / `#fvcolorindex` lines, i.e. as **comments**, which any OBJ parser is
entitled to skip. The green crate states its colour *only* there (its MTL has no `Kd`
at all), so skipping them rendered a green crate grey: no `Kd` defaults to white, and
white × a grey N64 texture is grey. `obj_model` reads the palette now. It is the only
one of the five editor OBJs that carries one, so nothing else changed — and that is
pinned by `an_export_without_a_palette_still_uses_kd`.

Plus one authoring bug worth naming: every gun shares the single `WeaponPickup` mesh
id, so routing a gun switch through `arm_prop_placement` hit its *toggle* — picking a
second gun disarmed the tool while the panel still showed one selected.

## What this deliberately does not do

- **No dropped weapons on death.** A killed hunter's gun disappears as it always
  did; making corpses drop a live pickup is a small follow-up on this component, and
  the obvious next step for the floor economy.
- **A shopping hunter is not tactical about it.** It walks to the nearest useful
  pickup by straight-line distance and ignores the player completely on the way — it
  will not take cover, break off, or prefer a gun that is away from the fight. Nearest
  is re-asked every step, so it does adapt when something closer appears or another
  hunter beats it to one.
- **A hunter keeps its spawn-time grip.** Picking up a pistol does not change the
  two-handed rifle pose it was rigged with at spawn (see above).
- **No melee.** The unarmed slot is inert on both sides; a punch is where that lands.
- **The credits economy is untouched.** The shop still works and still prices every
  gun. A deathmatch level simply never opens it.
