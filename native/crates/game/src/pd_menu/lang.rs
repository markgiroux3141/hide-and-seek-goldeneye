//! `lang_get` (`lang.c`): PD's text ids, `L_<BANK>_<n>`, resolved against the
//! English banks exported by `pd_menu_gen.py` (`native/assets/pd_menu/lang_en.json`).
//!
//! PD packs an id as `(bank << 9) | n`; the menus only ever add small offsets to
//! the index (`L_MISC_082 + i`, `L_OPTIONS_008 + team`), so a (bank, index) pair
//! is enough.

use std::collections::HashMap;

/// A text id: `L_<BANK>_<index>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tx {
    pub bank: u8,
    pub index: u16,
}

pub const fn tx(bank: u8, index: u16) -> Tx {
    Tx { bank, index }
}

impl Tx {
    /// `id + n`, as PD does with consecutive text ids.
    pub fn add(self, n: i32) -> Tx {
        Tx { bank: self.bank, index: (self.index as i32 + n).max(0) as u16 }
    }
}

pub struct Lang {
    banks: Vec<Vec<String>>,
}

impl Lang {
    pub fn load() -> Result<Lang, String> {
        let path = super::assets_dir().join("lang_en.json");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let map: HashMap<String, Vec<String>> = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let banks = super::generated::BANK_NAMES.iter().map(|b| map.get(*b).cloned().unwrap_or_default()).collect();
        Ok(Lang { banks })
    }

    pub fn get(&self, t: Tx) -> String {
        self.banks.get(t.bank as usize).and_then(|b| b.get(t.index as usize)).cloned().unwrap_or_default()
    }
}
