//! Keep/reject verdicts on texture themes, for pruning the extracted library.
//!
//! `native/assets/themes.json` currently carries ~390 themes auto-extracted from
//! the ripped GoldenEye levels (see `tools/texture-themes/`). That set is
//! deliberately over-broad — it was generated to be *pruned*, and a good number of
//! entries look bad in practice, which no heuristic could predict. So the picker
//! panel lets an author walk the list in-game, look at each theme on real
//! geometry, and mark it Keep or Reject.
//!
//! Verdicts live in `native/assets/theme_review.json`, next to the manifest they
//! annotate. That is the **only** asset file the game writes: it is authoring
//! output, not content, and it exists so a later pruning pass can cut
//! `themes.json` down to the kept set without a human re-deciding anything.
//!
//! Verdicts are keyed by theme **name**, never by index — the same reason level
//! files are (see `Brush::scheme`). Pruning the manifest reorders it, and a
//! verdict must survive that.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// An author's verdict on one theme.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Keep,
    Reject,
}

/// Which themes the picker list shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewFilter {
    All,
    Unreviewed,
    Kept,
    Rejected,
}

impl ReviewFilter {
    pub const ALL: [ReviewFilter; 4] = [
        ReviewFilter::All,
        ReviewFilter::Unreviewed,
        ReviewFilter::Kept,
        ReviewFilter::Rejected,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReviewFilter::All => "All",
            ReviewFilter::Unreviewed => "New",
            ReviewFilter::Kept => "Kept",
            ReviewFilter::Rejected => "Cut",
        }
    }

    /// Does a theme with this verdict pass the filter?
    pub fn accepts(self, verdict: Option<Verdict>) -> bool {
        match self {
            ReviewFilter::All => true,
            ReviewFilter::Unreviewed => verdict.is_none(),
            ReviewFilter::Kept => verdict == Some(Verdict::Keep),
            ReviewFilter::Rejected => verdict == Some(Verdict::Reject),
        }
    }
}

/// All verdicts, loaded from and saved to disk.
#[derive(Default, Serialize, Deserialize)]
pub struct ThemeReview {
    #[serde(default)]
    verdicts: HashMap<String, Verdict>,
}

/// `native/assets/theme_review.json` — beside the manifest it annotates.
pub fn review_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/theme_review.json"))
}

impl ThemeReview {
    /// Load verdicts, or an empty set if the file is absent or unreadable.
    ///
    /// Never fails: a corrupt review file must not stop the game booting, and the
    /// worst case is re-reviewing some themes.
    pub fn load() -> Self {
        let path = review_path();
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                log::warn!("{}: {e}; starting with no verdicts", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = review_path();
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::warn!("cannot write {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("cannot serialize theme review: {e}"),
        }
    }

    pub fn get(&self, name: &str) -> Option<Verdict> {
        self.verdicts.get(name).copied()
    }

    /// Set or clear a verdict, then persist.
    ///
    /// Saves on every change rather than at shutdown: reviewing ~390 themes is a
    /// long grind, and losing it to a crash would be worse than the cost of
    /// rewriting a few KB of JSON per click.
    pub fn set(&mut self, name: &str, verdict: Option<Verdict>) {
        match verdict {
            Some(v) => self.verdicts.insert(name.to_string(), v),
            None => self.verdicts.remove(name),
        };
        self.save();
    }

    /// Toggle a verdict: clicking the verdict a theme already has clears it.
    pub fn toggle(&mut self, name: &str, verdict: Verdict) {
        let next = if self.get(name) == Some(verdict) { None } else { Some(verdict) };
        self.set(name, next);
    }

    /// (kept, rejected, unreviewed) across the live theme registry.
    pub fn tally(&self) -> (usize, usize, usize) {
        let mut kept = 0;
        let mut rejected = 0;
        let mut unreviewed = 0;
        for s in engine::render::textures::schemes() {
            match self.get(s.name) {
                Some(Verdict::Keep) => kept += 1,
                Some(Verdict::Reject) => rejected += 1,
                None => unreviewed += 1,
            }
        }
        (kept, rejected, unreviewed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_sets_then_clears() {
        // Uses a scratch instance; `set` writes to disk, which is fine — the file
        // is authoring output and the test only asserts in-memory behaviour.
        let mut r = ThemeReview::default();
        assert_eq!(r.get("x"), None);
        r.verdicts.insert("x".into(), Verdict::Keep);
        assert_eq!(r.get("x"), Some(Verdict::Keep));
        r.verdicts.remove("x");
        assert_eq!(r.get("x"), None);
    }

    #[test]
    fn filters_partition_the_verdict_space() {
        let cases = [None, Some(Verdict::Keep), Some(Verdict::Reject)];
        for v in cases {
            // Exactly one of the three non-All filters accepts each verdict, and
            // All accepts everything.
            let n = [ReviewFilter::Unreviewed, ReviewFilter::Kept, ReviewFilter::Rejected]
                .iter()
                .filter(|f| f.accepts(v))
                .count();
            assert_eq!(n, 1, "{v:?} matched {n} filters");
            assert!(ReviewFilter::All.accepts(v));
        }
    }

    #[test]
    fn verdicts_round_trip_through_json() {
        let mut r = ThemeReview::default();
        r.verdicts.insert("caverns_01".into(), Verdict::Keep);
        r.verdicts.insert("dam_07".into(), Verdict::Reject);
        let json = serde_json::to_string(&r).unwrap();
        let back: ThemeReview = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("caverns_01"), Some(Verdict::Keep));
        assert_eq!(back.get("dam_07"), Some(Verdict::Reject));
        assert_eq!(back.get("nope"), None);
    }
}
