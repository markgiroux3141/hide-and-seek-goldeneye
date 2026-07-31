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

/// Blast an exploding wooden crate throws when destroyed (Milestone 3). Modest — a
/// crate is a light frag, not a bomb.
pub const CRATE_BLAST: Explosion = Explosion { radius: 3.0, max_damage: 80.0 };
/// Blast an explosive barrel throws — the reason it exists; bigger + deadlier.
pub const BARREL_BLAST: Explosion = Explosion { radius: 4.5, max_damage: 160.0 };

/// Panel grouping for the object palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PropCategory {
    Destructible,
    Furniture,
}

impl PropCategory {
    /// Display label for the panel header.
    pub fn label(self) -> &'static str {
        match self {
            PropCategory::Destructible => "Destructible",
            PropCategory::Furniture => "Furniture",
        }
    }

    /// Every category, in panel order.
    pub const ALL: &'static [PropCategory] =
        &[PropCategory::Destructible, PropCategory::Furniture];
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
    /// The blast thrown when destroyed, or `None` for indestructible furniture
    /// (consumed in Milestone 3).
    pub destructible: Option<Explosion>,
}

/// The full object palette. Panel iterates this grouped by [`PropCategory`]; the app
/// loads each `glb` at startup and uploads it to the renderer under `key`.
pub const CATALOG: &[PropDef] = &[
    PropDef {
        mesh: MeshId::WoodenCrate,
        key: "wooden_crate",
        name: "Wooden Crate",
        category: PropCategory::Destructible,
        glb: "wooden_crate.glb",
        scale: PROP_SCALE,
        destructible: Some(CRATE_BLAST),
    },
    PropDef {
        mesh: MeshId::ExplosiveBarrel,
        key: "explosive_barrel",
        name: "Explosive Barrel",
        category: PropCategory::Destructible,
        glb: "explosive_barrel.glb",
        scale: PROP_SCALE,
        destructible: Some(BARREL_BLAST),
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
];

/// The catalog entry for a placed prop's [`MeshId`] (linear scan — the catalog is
/// tiny). `None` only if a level references a prop id this build dropped.
pub fn def(mesh: MeshId) -> Option<&'static PropDef> {
    CATALOG.iter().find(|d| d.mesh == mesh)
}
