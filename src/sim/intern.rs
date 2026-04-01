//! String interning for zero-cost owner/type_ref clones in the sim layer.
//!
//! `InternedId` is a `Copy` newtype around `u32`. Cloning an entity's owner or
//! type_ref becomes a register copy instead of a heap allocation. The
//! `StringInterner` maps strings to IDs and back, using uppercase normalization
//! so case-insensitive owner comparisons become plain `==` on IDs.
//!
//! ## Determinism
//! The interner is part of `Simulation` — all peers intern the same strings
//! from the same rules.ini + map data in the same order, producing identical IDs.

use std::collections::BTreeMap;
use std::fmt;

/// Interned string handle — `Copy`, `Eq`, `Ord`, `Hash`. Zero-cost clones.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct InternedId(u32);

impl InternedId {
    /// Raw index, mainly for state hashing / debug.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for InternedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InternedId({})", self.0)
    }
}

impl fmt::Display for InternedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Bidirectional string interner with uppercase-normalized lookup.
///
/// `intern("Americans")` and `intern("americans")` return the same ID.
/// The display string preserves the casing of the first call that created the entry.
#[derive(Debug, Clone)]
pub struct StringInterner {
    /// Uppercase-normalized key → ID.
    to_id: BTreeMap<String, InternedId>,
    /// ID (index) → display string (first-seen casing).
    to_str: Vec<String>,
}

impl StringInterner {
    /// Create an empty interner.
    pub fn new() -> Self {
        Self {
            to_id: BTreeMap::new(),
            to_str: Vec::new(),
        }
    }

    /// Intern a string, returning its ID. If already interned (case-insensitive),
    /// returns the existing ID. Otherwise assigns the next sequential ID.
    pub fn intern(&mut self, s: &str) -> InternedId {
        let key = s.to_ascii_uppercase();
        if let Some(&id) = self.to_id.get(&key) {
            return id;
        }
        let id = InternedId(self.to_str.len() as u32);
        self.to_str.push(s.to_string());
        self.to_id.insert(key, id);
        id
    }

    /// Look up an ID without inserting. Returns `None` if not yet interned.
    pub fn get(&self, s: &str) -> Option<InternedId> {
        self.to_id.get(&s.to_ascii_uppercase()).copied()
    }

    /// Resolve an ID back to its display string (first-seen casing).
    ///
    /// # Panics
    /// Panics if `id` was not produced by this interner.
    #[inline]
    pub fn resolve(&self, id: InternedId) -> &str {
        &self.to_str[id.0 as usize]
    }

    /// Number of unique strings interned.
    pub fn len(&self) -> usize {
        self.to_str.len()
    }

    /// Returns `true` if no strings have been interned.
    pub fn is_empty(&self) -> bool {
        self.to_str.is_empty()
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl serde::Serialize for StringInterner {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_str.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for StringInterner {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let strings = Vec::<String>::deserialize(deserializer)?;
        let mut to_id = BTreeMap::new();
        for (i, s) in strings.iter().enumerate() {
            to_id.insert(s.to_ascii_uppercase(), InternedId(i as u32));
        }
        Ok(StringInterner {
            to_id,
            to_str: strings,
        })
    }
}

// ---------------------------------------------------------------------------
// Test / convenience helpers
// ---------------------------------------------------------------------------

use std::cell::RefCell;

thread_local! {
    /// Thread-local interner used by `test_intern()` so that test entities
    /// created independently still share consistent IDs.
    static TEST_INTERNER: RefCell<StringInterner> = RefCell::new(StringInterner::new());
}

/// Intern a string using the thread-local test interner.
///
/// This lets `GameEntity::test_default()` and other test helpers create
/// entities with consistent `InternedId` values without requiring callers
/// to manage an explicit interner.
pub fn test_intern(s: &str) -> InternedId {
    TEST_INTERNER.with(|cell| cell.borrow_mut().intern(s))
}

/// Get a copy of the thread-local test interner for use in test assertions
/// that need to resolve IDs back to strings.
pub fn test_interner() -> StringInterner {
    TEST_INTERNER.with(|cell| cell.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_case_insensitive() {
        let mut interner = StringInterner::new();
        let a = interner.intern("Americans");
        let b = interner.intern("americans");
        let c = interner.intern("AMERICANS");
        assert_eq!(a, b);
        assert_eq!(b, c);
        // Display string preserves first-seen casing
        assert_eq!(interner.resolve(a), "Americans");
    }

    #[test]
    fn different_strings_get_different_ids() {
        let mut interner = StringInterner::new();
        let a = interner.intern("Americans");
        let b = interner.intern("HTNK");
        assert_ne!(a, b);
    }

    #[test]
    fn get_returns_none_for_unknown() {
        let interner = StringInterner::new();
        assert_eq!(interner.get("unknown"), None);
    }

    #[test]
    fn serde_round_trip() {
        let mut interner = StringInterner::new();
        let a = interner.intern("Americans");
        let b = interner.intern("HTNK");
        let c = interner.intern("Soviet");

        let json = serde_json::to_string(&interner).unwrap();
        let restored: StringInterner = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.resolve(a), "Americans");
        assert_eq!(restored.resolve(b), "HTNK");
        assert_eq!(restored.resolve(c), "Soviet");
        // Case-insensitive lookup still works after round-trip
        assert_eq!(restored.get("americans"), Some(a));
        assert_eq!(restored.get("htnk"), Some(b));
    }

    #[test]
    fn interned_id_is_copy() {
        let mut interner = StringInterner::new();
        let id = interner.intern("test");
        let id2 = id; // Copy, not move
        assert_eq!(id, id2);
    }
}
