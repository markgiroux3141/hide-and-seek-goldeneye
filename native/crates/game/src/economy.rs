//! Player economy — the unified credit wallet (Game A "Siege Survival").
//!
//! Credits are the single currency (DESIGN.md §3 — one unified budget): earned by
//! defeating hunters now, and spent in the BUILD-phase shop on weapons + ammo. The
//! *same* wallet is intended to fund building / gadgets when the build-cost economy
//! lands, so nothing here is weapon-specific — it's just a balance with earn/spend.
//!
//! **Session-only for now.** There is no persistence across death/respawn yet: the
//! wallet resets when a fresh `World` is built. That waits on the wave/run model
//! decision (DESIGN_IDEAS.md §12 open question — is there death→respawn, and does
//! money carry?). Keeping it a plain owned value here means adding persistence later
//! is a serialize of one struct, not a refactor.

/// Credits granted for defeating one hunter. Flat for now; a later pass will scale
/// it by enemy archetype / difficulty. The award funnels through a single call site
/// (`World::award_kill`), so that change stays local to one function.
pub const KILL_BOUNTY: u32 = 100;

/// The player's unified credit wallet: a running balance with earn/spend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Economy {
    credits: u32,
}

impl Economy {
    /// A fresh wallet with a starting balance (0 in normal play; non-zero is handy
    /// for tests and dev shortcuts).
    pub fn new(starting_credits: u32) -> Self {
        Economy { credits: starting_credits }
    }

    /// Current balance — read by the HUD readout and the shop's affordability checks.
    pub fn credits(&self) -> u32 {
        self.credits
    }

    /// Add `amount` credits (a kill bounty, or a future passive income drip).
    /// Saturates at `u32::MAX` rather than wrapping.
    pub fn earn(&mut self, amount: u32) {
        self.credits = self.credits.saturating_add(amount);
    }

    /// Spend `cost` credits **iff** affordable: on success the balance is reduced and
    /// this returns `true`; when the player can't afford it the balance is left
    /// untouched and this returns `false`. Every shop purchase gates on this so a
    /// buy can never overdraw the wallet.
    #[must_use]
    pub fn try_spend(&mut self, cost: u32) -> bool {
        if self.credits >= cost {
            self.credits -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh wallet starts at its seeded balance and earning accumulates.
    #[test]
    fn earn_accumulates() {
        let mut e = Economy::new(0);
        assert_eq!(e.credits(), 0);
        e.earn(KILL_BOUNTY);
        e.earn(KILL_BOUNTY);
        assert_eq!(e.credits(), 2 * KILL_BOUNTY);
    }

    /// An affordable spend deducts and reports success; the exact balance is spendable.
    #[test]
    fn spend_when_affordable_deducts() {
        let mut e = Economy::new(300);
        assert!(e.try_spend(120), "120 ≤ 300 → affordable");
        assert_eq!(e.credits(), 180);
        assert!(e.try_spend(180), "spending the exact balance succeeds");
        assert_eq!(e.credits(), 0);
    }

    /// An unaffordable spend is a no-op: balance untouched, returns false.
    #[test]
    fn spend_when_broke_is_noop() {
        let mut e = Economy::new(50);
        assert!(!e.try_spend(51), "51 > 50 → declined");
        assert_eq!(e.credits(), 50, "declined purchase leaves the balance intact");
    }

    /// Earning saturates instead of wrapping past u32::MAX.
    #[test]
    fn earn_saturates() {
        let mut e = Economy::new(u32::MAX - 10);
        e.earn(1000);
        assert_eq!(e.credits(), u32::MAX);
    }
}
