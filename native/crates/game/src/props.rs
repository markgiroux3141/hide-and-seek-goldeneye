//! The placeable-prop catalog — the single source of truth the object-placement
//! panel, the GLB loader, the renderer keys, and (Milestone 3) the destructible
//! blast params all read from.
//!
//! Each [`PropDef`] pairs a stable [`MeshId`] (what persists in the level file) with
//! everything else about that prop as authored data. Adding a prop = one row here +
//! a `MeshId` variant + dropping its GLB in `native/assets/props/`.

use crate::combat::config::Explosion;
use crate::ecs::MeshId;

/// Authoring scale (GLB units → world metres). The 3DS-FPS prop GLBs are already
/// modelled in metres and origin-centred (verified: a crate is ~1.06 m), so this is
/// 1.0; kept as a knob in case a future prop needs rescaling.
pub const PROP_SCALE: f32 = 1.0;

/// Target standing height for a door panel, in metres — matched to the CSG doorway
/// carve (`DOOR_HEIGHT` = 7 WT = 1.75 m).
///
/// The door GLBs are ripped at real-world scale (2.18–2.92 m tall) but this world is
/// at GoldenEye N64 proportions: a room is 8 WT = **2.0 m** tall and the player capsule
/// is **1.5 m**. Left at `PROP_SCALE` every door would stand taller than the room it is
/// in and punch through the ceiling. Each door row therefore divides this by its own
/// measured GLB height — the divisor doubles as documentation of the source asset.
const DOOR_FIT: f32 = 1.75;

/// Shutters (`blast_door`, `big_metal_door`) are vehicle-scale panels; fitting them to
/// a person-sized doorway would defeat the point. They get the full room height less a
/// little headroom instead, so they read as a wide opening in the same 2.0 m room.
const SHUTTER_FIT: f32 = 1.9;

/// The destroy behaviour of a shootable prop: how much damage it soaks before it
/// blows, and the blast it throws when it does. Only meaningful together, so they
/// travel as one — furniture carries `None`.
#[derive(Clone, Copy, Debug)]
pub struct DestructibleDef {
    /// Hit points the prop absorbs before detonating (player-weapon damage per shot).
    pub health: f32,
    /// The blast thrown at the moment of destruction.
    pub blast: Explosion,
}

/// A wooden crate: a bit tanky, then a modest frag blast — a light crate, not a bomb.
pub const CRATE_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 60.0, blast: Explosion { radius: 3.0, max_damage: 80.0 } };
/// An explosive barrel: low health (a couple of hits pops it) into a big, deadly
/// blast — the reason it exists.
pub const BARREL_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 30.0, blast: Explosion { radius: 4.5, max_damage: 160.0 } };
/// A metal crate: sturdier than wood, modest blast.
pub const METAL_CRATE_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 90.0, blast: Explosion { radius: 3.0, max_damage: 70.0 } };
/// An ammo crate: cooks off into a bigger, hotter blast than a plain crate.
pub const AMMO_CRATE_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 45.0, blast: Explosion { radius: 3.5, max_damage: 120.0 } };
/// A cardboard crate: flimsy — a light pop.
pub const CARDBOARD_CRATE_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 25.0, blast: Explosion { radius: 2.0, max_damage: 40.0 } };
/// A gas can: low health, a sharp fuel blast — smaller than a barrel but nasty.
pub const GAS_CAN_DESTRUCTIBLE: DestructibleDef =
    DestructibleDef { health: 20.0, blast: Explosion { radius: 3.5, max_damage: 110.0 } };

/// Panel grouping for the object palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PropCategory {
    Pickups,
    Destructible,
    Furniture,
    Electronics,
    Clutter,
    Doors,
}

impl PropCategory {
    /// Display label for the panel header.
    pub fn label(self) -> &'static str {
        match self {
            PropCategory::Pickups => "Pickups",
            PropCategory::Destructible => "Destructible",
            PropCategory::Furniture => "Furniture",
            PropCategory::Electronics => "Electronics",
            PropCategory::Clutter => "Clutter",
            PropCategory::Doors => "Doors",
        }
    }

    /// Every category, in panel order. Pickups lead: in a deathmatch they are what
    /// you are actually placing.
    pub const ALL: &'static [PropCategory] = &[
        PropCategory::Pickups,
        PropCategory::Destructible,
        PropCategory::Furniture,
        PropCategory::Electronics,
        PropCategory::Clutter,
        PropCategory::Doors,
    ];
}

/// One placeable object's authored definition.
pub struct PropDef {
    /// Stable id persisted on the placed entity's `Renderable`.
    pub mesh: MeshId,
    /// Renderer + preview lookup key (also the GLB basename).
    pub key: &'static str,
    /// Human-facing name shown in the panel.
    pub name: &'static str,
    /// Which palette group it appears under.
    pub category: PropCategory,
    /// GLB filename under `native/assets/props/`.
    pub glb: &'static str,
    /// Authoring scale applied to the placed transform.
    pub scale: f32,
    /// Health + blast if the prop can be shot apart, or `None` for indestructible
    /// furniture. Drives the HUNT prop collider bake + the shoot→darken→explode path.
    pub destructible: Option<DestructibleDef>,
}

/// Authoring scale for the green ammo crate. The Setup-Editor export is 1.21 m
/// across — a big floor crate in a world whose rooms are 2.0 m tall — so it is
/// brought down to roughly the tan box's footprint, which is what makes the two read
/// as two flavours of the same thing rather than two different objects.
const GREEN_CRATE_FIT: f32 = 0.6;

/// The full object palette. Panel iterates this grouped by [`PropCategory`]; the app
/// loads each `glb` at startup and uploads it to the renderer under `key`.
///
/// **Weapon pickups are not in here.** A gun on the ground draws from the world-space
/// weapon render library (keyed by weapon name, uploaded for the whole arsenal at
/// startup) rather than the prop channel, so [`crate::ecs::MeshId::WeaponPickup`] has
/// no catalog row — see [`crate::world::tools::pickup`]. Everything downstream that
/// keys off `def()` therefore skips it correctly by construction: no collider, no nav
/// footprint, nothing to walk into.
pub const CATALOG: &[PropDef] = &[
    // ── Pickups (collectable; no `destructible` — you walk through them) ──────
    PropDef {
        mesh: MeshId::AmmoPickupTan,
        key: "ammo_pickup_tan",
        name: "Ammo Crate (Tan)",
        category: PropCategory::Pickups,
        // Shares the destructible ammo crate's mesh: same GoldenEye box, placed as a
        // collectable instead of as something to shoot. The catalog keys on `mesh`
        // and `key`, not on the asset, so one GLB can serve both roles.
        glb: "ammo_crate.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::AmmoPickupGreen,
        key: "ammo_pickup_green",
        name: "Ammo Crate (Green)",
        category: PropCategory::Pickups,
        glb: "green_ammo_crate/green_ammo_crate.obj",
        scale: GREEN_CRATE_FIT,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::WoodenCrate,
        key: "wooden_crate",
        name: "Wooden Crate",
        category: PropCategory::Destructible,
        glb: "wooden_crate.glb",
        scale: PROP_SCALE,
        destructible: Some(CRATE_DESTRUCTIBLE),
    },
    PropDef {
        mesh: MeshId::ExplosiveBarrel,
        key: "explosive_barrel",
        name: "Explosive Barrel",
        category: PropCategory::Destructible,
        glb: "explosive_barrel.glb",
        scale: PROP_SCALE,
        destructible: Some(BARREL_DESTRUCTIBLE),
    },
    PropDef {
        mesh: MeshId::FilingCabinet,
        key: "filing_cabinet",
        name: "Filing Cabinet",
        category: PropCategory::Furniture,
        glb: "filing_cabinet.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Bookshelf,
        key: "bookshelf",
        name: "Bookshelf",
        category: PropCategory::Furniture,
        glb: "bookshelf.glb",
        // The source bookshelf is oversized for our rooms — 60% reads right.
        scale: PROP_SCALE * 0.6,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::HeavyWoodenTable,
        key: "heavy_wooden_table",
        name: "Heavy Wooden Table",
        category: PropCategory::Furniture,
        glb: "heavy_wooden_table.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    // ── Destructible ──────────────────────────────────────────────────────────
    PropDef {
        mesh: MeshId::MetalCrate,
        key: "metal_crate",
        name: "Metal Crate",
        category: PropCategory::Destructible,
        glb: "metal_crate.glb",
        scale: PROP_SCALE,
        destructible: Some(METAL_CRATE_DESTRUCTIBLE),
    },
    PropDef {
        mesh: MeshId::AmmoCrate,
        key: "ammo_crate",
        name: "Ammo Crate",
        category: PropCategory::Destructible,
        glb: "ammo_crate.glb",
        scale: PROP_SCALE,
        destructible: Some(AMMO_CRATE_DESTRUCTIBLE),
    },
    PropDef {
        mesh: MeshId::CardboardCrate,
        key: "cardboard_crate",
        name: "Cardboard Crate",
        category: PropCategory::Destructible,
        glb: "cardboard_crate.glb",
        scale: PROP_SCALE,
        destructible: Some(CARDBOARD_CRATE_DESTRUCTIBLE),
    },
    // ── Furniture ─────────────────────────────────────────────────────────────
    PropDef {
        mesh: MeshId::WoodenTable,
        key: "wooden_table",
        name: "Wooden Table",
        category: PropCategory::Furniture,
        glb: "wooden_table.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BlackTable,
        key: "black_table",
        name: "Black Table",
        category: PropCategory::Furniture,
        glb: "black_table.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BlackChair,
        key: "black_chair",
        name: "Black Chair",
        category: PropCategory::Furniture,
        glb: "black_chair.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BlueChair,
        key: "blue_chair",
        name: "Blue Chair",
        category: PropCategory::Furniture,
        glb: "blue_chair.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Locker,
        key: "locker",
        name: "Locker",
        category: PropCategory::Furniture,
        glb: "locker.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::MetalSafe,
        key: "metal_safe",
        name: "Metal Safe",
        category: PropCategory::Furniture,
        glb: "metal_safe.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::MetalCrateStack,
        key: "metal_crate_stack",
        name: "Metal Crate Stack",
        category: PropCategory::Furniture,
        glb: "metal_crate_stack.glb",
        // ~4 m stack — 60% keeps it a big-but-placeable obstacle.
        scale: PROP_SCALE * 0.6,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Divider,
        key: "divider",
        name: "Divider",
        category: PropCategory::Furniture,
        glb: "divider.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    // ── Electronics ───────────────────────────────────────────────────────────
    PropDef {
        mesh: MeshId::Console,
        key: "console",
        name: "Console",
        category: PropCategory::Electronics,
        glb: "console.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Console1,
        key: "console_1",
        name: "Console (Bank)",
        category: PropCategory::Electronics,
        glb: "console_1.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Mainframe,
        key: "mainframe",
        name: "Mainframe",
        category: PropCategory::Electronics,
        glb: "mainframe.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Tv,
        key: "tv",
        name: "Monitor",
        category: PropCategory::Electronics,
        glb: "tv.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Tv1,
        key: "tv_1",
        name: "Monitor (Wide)",
        category: PropCategory::Electronics,
        glb: "tv_1.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Keyboard,
        key: "keyboard",
        name: "Keyboard",
        category: PropCategory::Electronics,
        glb: "keyboard.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Radio,
        key: "radio",
        name: "Radio",
        category: PropCategory::Electronics,
        glb: "radio.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::RackDevice,
        key: "rack_device",
        name: "Rack Device",
        category: PropCategory::Electronics,
        glb: "rack_device.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::WallDisplay,
        key: "wall_display",
        name: "Wall Display",
        category: PropCategory::Electronics,
        glb: "wall_display.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    // ── Clutter ───────────────────────────────────────────────────────────────
    PropDef {
        mesh: MeshId::TrashCans,
        key: "trash_cans",
        name: "Trash Cans",
        category: PropCategory::Clutter,
        glb: "trash_cans.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Barricade,
        key: "barricade",
        name: "Barricade",
        category: PropCategory::Clutter,
        glb: "barricade.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Beaker,
        key: "beaker",
        name: "Beaker",
        category: PropCategory::Clutter,
        glb: "beaker.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Book,
        key: "book",
        name: "Book",
        category: PropCategory::Clutter,
        glb: "book.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Calculator,
        key: "calculator",
        name: "Calculator",
        category: PropCategory::Clutter,
        glb: "calculator.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Alarm,
        key: "alarm",
        name: "Alarm",
        category: PropCategory::Clutter,
        glb: "alarm.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    // ── Doors (static props; mechanics later) ─────────────────────────────────
    PropDef {
        mesh: MeshId::MetalDoor,
        key: "metal_door",
        name: "Metal Door",
        category: PropCategory::Doors,
        glb: "metal_door.glb",
        // GLB is 2.433 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.433,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::MetalDoor2,
        key: "metal_door_2",
        name: "Metal Door (Tall)",
        category: PropCategory::Doors,
        glb: "metal_door_2.glb",
        // GLB is 2.924 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.924,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BrownSlidingDoor,
        key: "brown_sliding_door",
        name: "Sliding Door",
        category: PropCategory::Doors,
        glb: "brown_sliding_door.glb",
        // GLB is 2.181 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.181,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BlastDoor,
        key: "blast_door",
        name: "Blast Door",
        category: PropCategory::Doors,
        glb: "blast_door.glb",
        // GLB is 3.836 m tall — fit to the doorway carve.
        scale: SHUTTER_FIT / 3.836,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::WoodenDoor,
        key: "wooden_door",
        name: "Wooden Door",
        category: PropCategory::Doors,
        glb: "wooden_door.glb",
        // GLB is 2.711 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.711,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::ElevatorDoor,
        key: "elevator_door",
        name: "Elevator Door",
        category: PropCategory::Doors,
        glb: "elevator_door.glb",
        // GLB is 2.446 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.446,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::GreyDoor,
        key: "grey_door",
        name: "Grey Door",
        category: PropCategory::Doors,
        glb: "grey_door.glb",
        // GLB is 2.7 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.7,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BigMetalDoor,
        key: "big_metal_door",
        name: "Big Metal Door",
        category: PropCategory::Doors,
        glb: "big_metal_door.glb",
        // GLB is 4.647 m tall — fit to the doorway carve.
        scale: SHUTTER_FIT / 4.647,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BathroomDoor,
        key: "bathroom_door",
        name: "Bathroom Door",
        category: PropCategory::Doors,
        glb: "bathroom_door.glb",
        // GLB is 2.522 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.522,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::JailDoor,
        key: "jail_door",
        name: "Jail Door",
        category: PropCategory::Doors,
        glb: "jail_door.glb",
        // GLB is 2.747 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.747,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::MetalSafeDoor,
        key: "metal_safe_door",
        name: "Safe Door",
        category: PropCategory::Doors,
        glb: "metal_safe_door.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    // ── Consolidated primary+secondary props (opaque + alpha-cutout, merged at
    //    load; the `glb` here is the primary, the cutout half via `secondary_glb`) ──
    PropDef {
        mesh: MeshId::GasCan,
        key: "gas_can",
        name: "Gas Can",
        category: PropCategory::Destructible,
        glb: "gas_can_primary.glb",
        scale: PROP_SCALE,
        destructible: Some(GAS_CAN_DESTRUCTIBLE),
    },
    PropDef {
        mesh: MeshId::ChainlinkFence,
        key: "chainlink_fence",
        name: "Chain-link Fence",
        category: PropCategory::Furniture,
        glb: "chainlink_fence_primary.glb",
        // ~3.6 m tall panel — 80% keeps it a tall barrier without dwarfing the room.
        scale: PROP_SCALE * 0.8,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::MetalGrate,
        key: "metal_grate",
        name: "Metal Grate",
        category: PropCategory::Furniture,
        glb: "metal_grate_primary.glb",
        // ~2.4×3.1 m floor grate — 70% fits a room bay.
        scale: PROP_SCALE * 0.7,
        destructible: None,
    },
    // Raw GoldenEye-editor OBJs (not GLBs): opaque + cutout geometry in one file,
    // colour in the material Kd, loaded via the OBJ path (see `load_prop_model`).
    PropDef {
        mesh: MeshId::SecurityCamera,
        key: "security_camera",
        name: "Security Camera",
        category: PropCategory::Electronics,
        glb: "security_camera/security_camera.obj",
        scale: PROP_SCALE,
        destructible: None,
    },
    // The sentry gun is the one *articulated* prop: its export is a parts sheet, so it
    // loads split into six pieces and draws as six matrices driven by
    // [`crate::turret`]. `key` names the whole prop (panel, placement, persistence);
    // the pieces have their own renderer keys in `turret::PARTS`.
    PropDef {
        mesh: MeshId::SentryGun,
        key: "sentry_gun",
        name: "Sentry Gun",
        category: PropCategory::Electronics,
        glb: "sentry_gun/sentry_gun.obj",
        scale: crate::turret::RIG_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::GreyTable,
        key: "grey_table",
        name: "Grey Table",
        category: PropCategory::Furniture,
        glb: "grey_table/grey_table.obj",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::BodyArmour,
        key: "body_armour",
        name: "Body Armour",
        category: PropCategory::Clutter,
        glb: "body_armour/body_armour.obj",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::Lamp,
        key: "lamp",
        name: "Lamp",
        category: PropCategory::Clutter,
        glb: "lamp_primary.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::TestTubes,
        key: "test_tubes",
        name: "Test Tubes",
        category: PropCategory::Clutter,
        glb: "test_tubes_primary.glb",
        scale: PROP_SCALE,
        destructible: None,
    },
    PropDef {
        mesh: MeshId::GlassDoor,
        key: "glass_door",
        name: "Glass Door",
        category: PropCategory::Doors,
        glb: "glass_door_primary.glb",
        // GLB is 2.58 m tall — fit to the doorway carve.
        scale: DOOR_FIT / 2.58,
        destructible: None,
    },
];

/// The catalog entry for a placed prop's [`MeshId`] (linear scan — the catalog is
/// tiny). `None` only if a level references a prop id this build dropped.
pub fn def(mesh: MeshId) -> Option<&'static PropDef> {
    CATALOG.iter().find(|d| d.mesh == mesh)
}

/// Load a prop's model from its catalog `glb` path, dispatching on file type: a raw
/// GoldenEye-editor **`.obj`** (OBJ + MTL + BMP, colour in `Kd`) goes through the OBJ
/// loader; anything else is a **`.glb`** through the weapon/prop GLB loader. Both yield
/// the same [`engine::assets::textured_model::TexturedModel`], so the caller (app
/// startup / the catalog tests) treats every prop identically. The `path` is the full
/// on-disk path, not the bare catalog name.
pub fn load_prop_model(path: &str) -> Result<engine::assets::textured_model::TexturedModel, String> {
    if path.to_ascii_lowercase().ends_with(".obj") {
        engine::assets::obj_model::load_obj(path)
    } else {
        crate::combat::load_gun(path)
    }
}

/// The optional **secondary** GLB for a prop — the alpha-cutout material half of a
/// GoldenEye "primary + secondary" pair (glass, chain-link, grates). When present, the
/// app loads it and [`engine::assets::textured_model::TexturedModel::append`]s it onto
/// the primary at startup, so the prop is a single consolidated object everywhere
/// (catalog / panel / placement / persistence). Kept as a side table rather than a
/// [`PropDef`] field so the ~50 single-GLB props don't each carry a `secondary: None`.
/// The `PropDef.glb` is the primary (`*_primary.glb`) for these.
pub fn secondary_glb(mesh: MeshId) -> Option<&'static str> {
    Some(match mesh {
        MeshId::GlassDoor => "glass_door_secondary.glb",
        MeshId::Lamp => "lamp_secondary.glb",
        MeshId::TestTubes => "test_tubes_secondary.glb",
        MeshId::ChainlinkFence => "chainlink_fence_secondary.glb",
        MeshId::MetalGrate => "metal_grate_secondary.glb",
        MeshId::GasCan => "gas_can_secondary.glb",
        // NB: SecurityCamera is now a self-contained OBJ (no secondary GLB).
        _ => return None,
    })
}

/// Whether placing this mesh authors a **pickup** — something the player collects
/// rather than scenery. Drives the panel's pickup settings block and the
/// [`crate::ecs::Pickup`] component attached at placement.
///
/// [`MeshId::WeaponPickup`] is included even though it has no [`CATALOG`] row: it is
/// the one pickup mesh that is not a prop (see the `CATALOG` docs).
///
/// A side table rather than a [`PropDef`] field for the same reason as
/// [`secondary_glb`]: the ~50 scenery rows shouldn't each carry a `pickup: None`.
pub fn pickup_kind(mesh: MeshId) -> Option<crate::ecs::PickupKind> {
    Some(match mesh {
        MeshId::WeaponPickup => crate::ecs::PickupKind::Weapon,
        MeshId::AmmoPickupTan | MeshId::AmmoPickupGreen => crate::ecs::PickupKind::Ammo,
        _ => return None,
    })
}

/// Whether a prop bolts to the **ceiling** rather than standing on the floor.
///
/// A ceiling prop differs in both directions of the placement flow: it is picked
/// against a down-facing face instead of an up-facing one, and its authored position
/// is its *mount* point rather than the centre of its base — so its model anchor is
/// the model origin, which such a prop's rig authors at the point it hangs from.
///
/// A side table rather than a [`PropDef`] field for the same reason as
/// [`secondary_glb`]: the ~50 floor-standing rows shouldn't each carry a `false`.
pub fn ceiling_mounted(mesh: MeshId) -> bool {
    matches!(mesh, MeshId::SentryGun)
}

/// How a door panel moves when it opens. The catalog fixes this per model (a sliding
/// door can't swing), while the *direction* of the motion — hinge side, which way it
/// swings, which way it slides — is authored per placed door on the
/// [`crate::ecs::Door`] component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoorMotion {
    /// Hinged swing about one vertical edge of the panel.
    Swing,
    /// Slides sideways along the panel's own width axis, into a wall pocket.
    SlideSideways,
    /// Rises vertically — a shutter / vehicle blast door.
    SlideUp,
}

/// Everything the catalog fixes about a door: how it moves and how it sounds.
/// Sounds are grouped into four voices (wood / metal / slide / shutter) rather than
/// one pair per model, so a new door row only picks a voice.
#[derive(Clone, Copy, Debug)]
pub struct DoorDef {
    pub motion: DoorMotion,
    /// Played the moment the panel starts opening.
    pub open_sound: &'static str,
    /// Played the moment it starts closing.
    pub close_sound: &'static str,
}

const WOOD: (&str, &str) = ("sounds/doors/wood-open.wav", "sounds/doors/wood-close.wav");
const METAL: (&str, &str) = ("sounds/doors/metal-open.wav", "sounds/doors/metal-close.wav");
// A sliding panel makes the same noise in both directions — it is the sound of the
// door travelling in its track, not of a latch — so open and close share one clip.
const SLIDE: (&str, &str) = ("sounds/doors/slide-open.wav", "sounds/doors/slide-open.wav");
const SHUTTER: (&str, &str) =
    ("sounds/doors/shutter-open.wav", "sounds/doors/shutter-close.wav");

/// Played when a door refuses to open (locked / not yours to open).
pub const DOOR_LOCKED_SOUND: &str = "sounds/doors/locked.wav";

const fn door(motion: DoorMotion, sfx: (&'static str, &'static str)) -> DoorDef {
    DoorDef { motion, open_sound: sfx.0, close_sound: sfx.1 }
}

/// The door behaviour for a prop, or `None` if it is ordinary scenery. A side table
/// rather than a [`PropDef`] field for the same reason as [`secondary_glb`]: the ~50
/// non-door rows shouldn't each carry a `door: None`.
///
/// Motion is the honest reading of each GoldenEye asset: the two room-sized panels
/// (`blast_door` 4.55 m, `big_metal_door` 7.02 m) are vehicle/blast doors that rise
/// rather than swing, and `metal_safe_door` is a 1.16 m vault hatch — a swing with a
/// short arc, which the authored `open_angle` supplies.
pub fn door_def(mesh: MeshId) -> Option<DoorDef> {
    Some(match mesh {
        // ── Swing, light voice ──
        MeshId::WoodenDoor | MeshId::BathroomDoor => door(DoorMotion::Swing, WOOD),
        MeshId::GreyDoor | MeshId::GlassDoor => door(DoorMotion::Swing, WOOD),
        // ── Swing, metal voice ──
        MeshId::MetalDoor | MeshId::MetalDoor2 => door(DoorMotion::Swing, METAL),
        MeshId::JailDoor | MeshId::MetalSafeDoor => door(DoorMotion::Swing, METAL),
        // ── Slide sideways ──
        MeshId::BrownSlidingDoor | MeshId::ElevatorDoor => {
            door(DoorMotion::SlideSideways, SLIDE)
        }
        // ── Shutters ──
        MeshId::BlastDoor | MeshId::BigMetalDoor => door(DoorMotion::SlideUp, SHUTTER),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn asset_path(glb: &str) -> String {
        format!("{}/../../assets/props/{}", env!("CARGO_MANIFEST_DIR"), glb)
    }

    /// Every catalog row is internally consistent (unique `key`/`mesh`) and its GLB —
    /// plus its `secondary_glb` cutout half, when it has one — is present under
    /// `assets/props/`, so a hand-added row can't silently reference a missing asset.
    #[test]
    fn catalog_rows_are_unique_and_their_assets_exist() {
        let mut keys = HashSet::new();
        let mut meshes = HashSet::new();
        for d in CATALOG {
            assert!(keys.insert(d.key), "duplicate catalog key: {}", d.key);
            assert!(meshes.insert(d.mesh), "duplicate catalog mesh: {:?}", d.mesh);
            assert!(
                std::path::Path::new(&asset_path(d.glb)).exists(),
                "catalog GLB missing on disk: {}",
                d.glb
            );
            if let Some(sec) = secondary_glb(d.mesh) {
                assert!(
                    std::path::Path::new(&asset_path(sec)).exists(),
                    "secondary GLB missing on disk: {sec}"
                );
            }
        }
    }

    /// Every catalog GLB actually parses through the prop loader into a non-empty mesh
    /// — the same CPU load path the app runs at startup — and a consolidated prop's
    /// secondary half both loads AND merges (grows the vertex count) via `append`, so
    /// an unsupported/corrupt/mis-paired import is caught here, not in-game.
    #[test]
    fn every_catalog_glb_loads_and_secondaries_merge() {
        for d in CATALOG {
            let mut model = load_prop_model(&asset_path(d.glb))
                .unwrap_or_else(|e| panic!("prop {} ({}) failed to load: {e}", d.name, d.glb));
            let before = model.vertices.len();
            assert!(before > 0, "prop {} loaded but has no geometry", d.name);
            if let Some(sec) = secondary_glb(d.mesh) {
                let s = load_prop_model(&asset_path(sec))
                    .unwrap_or_else(|e| panic!("prop {} secondary {sec} failed to load: {e}", d.name));
                model.append(s);
                assert!(
                    model.vertices.len() > before,
                    "prop {} secondary {sec} merged no geometry",
                    d.name
                );
            }
        }
    }
}




