//! Shop pricing — the authored-fresh credit costs for buying weapons and ammo from
//! the menu shop (Game A economy). Kept SEPARATE from [`crate::combat::config`]
//! (which is the ported weapon *combat* data — the read-only oracle): a price is a
//! game-economy value, not a GoldenEye stat, so it lives here and is free to tune.
//!
//! Prices are keyed by weapon **name** (not index) so reordering `config::WEAPONS`
//! can't silently mis-price a gun. A test asserts every configured weapon is listed.

/// Rounds granted per ammo purchase, expressed as this many full magazines of the
/// weapon being restocked (so `magazine_size × this` rounds are added to reserve).
pub const AMMO_MAGS_PER_BUY: u32 = 2;

/// The explicit price table. `None` means the weapon isn't listed — [`weapon_price`]
/// then falls back to a nominal default, and the coverage test flags the omission.
fn listed_price(name: &str) -> Option<u32> {
    Some(match name {
        "PP7" => 0, // the free starter — owned from the first frame, never buyable
        // Pistols / PP7 variants
        "DD44 Dostovei" => 300,
        "PP7 (Silenced)" => 700,
        "Cougar Magnum" => 800,
        "Silver PP7" => 1200,
        "Gold PP7" => 1500,
        "Golden Gun" => 5000, // one-shot-kill premium
        // SMGs
        "Klobb" => 400,
        "D5K Deutsche" => 700,
        "D5K (Silenced)" => 900,
        "ZMG 9mm" => 1200,
        "Phantom" => 1500,
        // Rifles
        "KF7 Soviet" => 1500,
        "AR33" => 1800,
        "RC-P90" => 2500,
        // Shotguns
        "Shotgun" => 1000,
        "Auto Shotgun" => 2200,
        // Special
        "Sniper Rifle" => 3000,
        "Moonraker Laser" => 4000,
        // Explosives (projectile)
        "Grenade" => 500,
        "Grenade Launcher" => 3500,
        "Rocket Launcher" => 5000,
        // Explosives (mines)
        "Proximity Mine" => 600,
        "Timed Mine" => 600,
        "Remote Mine" => 800,
        _ => return None,
    })
}

/// Credit cost to buy the weapon named `name`. Unlisted weapons fall back to a
/// nominal 1000 (the coverage test keeps this from happening for real weapons).
pub fn weapon_price(name: &str) -> u32 {
    listed_price(name).unwrap_or(1000)
}

/// Coarse UI category for grouping weapons into sections in the shop list. Follows
/// the ordering of `config::WEAPONS`, so weapons in a category are contiguous and a
/// single header can be emitted whenever the category changes while iterating.
pub fn weapon_category(name: &str) -> &'static str {
    match name {
        "PP7" | "DD44 Dostovei" | "Cougar Magnum" | "Golden Gun" | "Gold PP7"
        | "Silver PP7" | "PP7 (Silenced)" => "PISTOLS",
        "Klobb" | "D5K Deutsche" | "D5K (Silenced)" | "Phantom" | "ZMG 9mm" => "SMGS",
        "RC-P90" | "AR33" | "KF7 Soviet" => "RIFLES",
        "Shotgun" | "Auto Shotgun" => "SHOTGUNS",
        "Sniper Rifle" | "Moonraker Laser" => "SPECIAL",
        "Rocket Launcher" | "Grenade Launcher" | "Grenade" => "EXPLOSIVES",
        "Proximity Mine" | "Timed Mine" | "Remote Mine" => "MINES",
        _ => "OTHER",
    }
}

/// Credit cost for one ammo purchase ([`AMMO_MAGS_PER_BUY`] magazines) for `name`.
/// Derived from the weapon price so pricier guns cost more to feed, floored so even
/// the free starter charges something. Deliberately simple for v1 — may split into
/// its own authored table once weapons are balanced.
pub fn ammo_price(name: &str) -> u32 {
    (weapon_price(name) / 10).clamp(25, 500)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::config;

    /// Every configured weapon has an explicit price entry (catches a name typo or a
    /// newly-added weapon that was never priced — it would silently hit the default).
    #[test]
    fn every_configured_weapon_is_priced() {
        for w in config::WEAPONS {
            assert!(
                listed_price(w.name).is_some(),
                "weapon {:?} has no shop price — add it to listed_price()",
                w.name
            );
        }
    }

    /// The PP7 is free (the starter) and every other weapon costs something.
    #[test]
    fn pp7_is_free_others_cost() {
        assert_eq!(weapon_price("PP7"), 0);
        for w in config::WEAPONS {
            if w.name != "PP7" {
                assert!(weapon_price(w.name) > 0, "{} should cost credits", w.name);
            }
        }
    }

    /// Ammo pricing stays within its authored floor/ceiling.
    #[test]
    fn ammo_price_is_bounded() {
        for w in config::WEAPONS {
            let p = ammo_price(w.name);
            assert!((25..=500).contains(&p), "{} ammo price {p} out of range", w.name);
        }
    }

    /// Every configured weapon lands in a real category (never the "OTHER" fallback),
    /// so the shop list groups them all under a proper header.
    #[test]
    fn every_configured_weapon_has_a_category() {
        for w in config::WEAPONS {
            assert_ne!(
                weapon_category(w.name),
                "OTHER",
                "weapon {:?} has no shop category — add it to weapon_category()",
                w.name
            );
        }
    }
}
