//! Parser for XCC Mixer's "global mix database.dat" file.
//!
//! The XCC database maps ~24,000 known C&C filenames to human-readable
//! descriptions. We use it as a **developer convenience** to reverse-lookup
//! MIX entry hash IDs into filenames for the MIX browser tool.
//!
//! Format: 4-byte LE count, then `filename\0description\0` pairs repeated.
//!
//! ## Dependency rules
//! - Part of assets/ — no dependencies on game modules.

use crate::assets::mix_hash::mix_hash;

/// A single entry from the XCC global mix database.
#[derive(Debug, Clone)]
pub struct XccEntry {
    /// Filename (e.g., "rules.ini", "game.fnt").
    pub filename: String,
    /// Human-readable description (e.g., "ts main settings", "game font").
    pub description: String,
}

/// Parsed XCC global mix database — a catalog of known C&C filenames.
pub struct XccDatabase {
    entries: Vec<XccEntry>,
}

/// Default path where XCC Mixer installs the database on Windows.
pub const DEFAULT_XCC_DATABASE_PATH: &str =
    r"C:\Program Files (x86)\XCC\Utilities\global mix database.dat";

/// Environment variable to override the XCC database path.
pub const XCC_DATABASE_PATH_ENV: &str = "XCC_DATABASE_PATH";

impl XccDatabase {
    /// Parse from the raw bytes of `global mix database.dat`.
    ///
    /// Format: `[u32 count] [filename\0 description\0]*count`
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 4 {
            return Err("XCC database too small (< 4 bytes)".to_string());
        }

        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let mut entries = Vec::with_capacity(count);
        let mut pos: usize = 4;

        for i in 0..count {
            let filename = read_null_terminated(data, &mut pos).ok_or_else(|| {
                format!("XCC database: unexpected end reading filename at entry {i}")
            })?;
            let description = read_null_terminated(data, &mut pos).ok_or_else(|| {
                format!("XCC database: unexpected end reading description at entry {i}")
            })?;
            entries.push(XccEntry {
                filename,
                description,
            });
        }

        // The file often contains more strings beyond the declared count
        // (multiple game generations). Keep parsing until EOF.
        while pos < data.len() {
            let Some(filename) = read_null_terminated(data, &mut pos) else {
                break;
            };
            let Some(description) = read_null_terminated(data, &mut pos) else {
                break;
            };
            entries.push(XccEntry {
                filename,
                description,
            });
        }

        log::info!(
            "XCC database loaded: {} entries ({} declared + {} extra)",
            entries.len(),
            count,
            entries.len().saturating_sub(count),
        );

        Ok(Self { entries })
    }

    /// Load from a file on disk. Tries the env var override first, then the
    /// default XCC Mixer install path.
    pub fn load_from_disk() -> Result<Self, String> {
        let path = std::env::var(XCC_DATABASE_PATH_ENV)
            .unwrap_or_else(|_| DEFAULT_XCC_DATABASE_PATH.to_string());

        let data = std::fs::read(&path)
            .map_err(|e| format!("Cannot read XCC database at '{}': {}", path, e))?;

        Self::from_bytes(&data)
    }

    /// Number of entries in the database.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the database is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries.
    pub fn entries(&self) -> &[XccEntry] {
        &self.entries
    }

    /// Build a hash-to-filename lookup table using our `mix_hash()`.
    ///
    /// Returns `(filename, hash)` pairs sorted by hash, deduplicated.
    /// This can be merged directly into the MIX browser dictionary.
    pub fn build_hash_dictionary(&self) -> Vec<(String, i32)> {
        let mut dict: Vec<(String, i32)> = self
            .entries
            .iter()
            .map(|entry| {
                let hash = mix_hash(&entry.filename);
                (entry.filename.clone(), hash)
            })
            .collect();
        dict.sort_by_key(|(_, hash)| *hash);
        dict.dedup_by_key(|(_, hash)| *hash);
        dict
    }

    /// Find an entry by filename (case-insensitive).
    pub fn find(&self, filename: &str) -> Option<&XccEntry> {
        let upper = filename.to_ascii_uppercase();
        self.entries
            .iter()
            .find(|e| e.filename.to_ascii_uppercase() == upper)
    }

    /// List all entries matching an extension (e.g., ".fnt", ".pal").
    pub fn by_extension(&self, ext: &str) -> Vec<&XccEntry> {
        let ext_lower = ext.to_ascii_lowercase();
        // Normalize: accept both ".fnt" and "fnt".
        let dot_ext = if ext_lower.starts_with('.') {
            ext_lower
        } else {
            format!(".{ext_lower}")
        };

        self.entries
            .iter()
            .filter(|e| e.filename.to_ascii_lowercase().ends_with(&dot_ext))
            .collect()
    }
}

/// Read a null-terminated ASCII string from `data` starting at `pos`.
/// Advances `pos` past the null terminator. Returns `None` if no null found.
fn read_null_terminated(data: &[u8], pos: &mut usize) -> Option<String> {
    if *pos >= data.len() {
        return None;
    }
    let start = *pos;
    let end = data[start..].iter().position(|&b| b == 0)?;
    let s = String::from_utf8_lossy(&data[start..start + end]).into_owned();
    *pos = start + end + 1;
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_database() {
        // 2 entries: "foo\0bar\0" and "baz.shp\0quux\0"
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes()); // count = 2
        data.extend_from_slice(b"foo\0bar\0");
        data.extend_from_slice(b"baz.shp\0quux\0");

        let db = XccDatabase::from_bytes(&data).expect("parse should succeed");
        assert_eq!(db.len(), 2);
        assert_eq!(db.entries()[0].filename, "foo");
        assert_eq!(db.entries()[0].description, "bar");
        assert_eq!(db.entries()[1].filename, "baz.shp");
        assert_eq!(db.entries()[1].description, "quux");
    }

    #[test]
    fn test_find_case_insensitive() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(b"Game.FNT\0game font\0");

        let db = XccDatabase::from_bytes(&data).expect("parse");
        assert!(db.find("game.fnt").is_some());
        assert!(db.find("GAME.FNT").is_some());
        assert!(db.find("nonexistent").is_none());
    }

    #[test]
    fn test_by_extension() {
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(b"game.fnt\0font\0");
        data.extend_from_slice(b"rules.ini\0config\0");
        data.extend_from_slice(b"vcr.fnt\0vcr font\0");

        let db = XccDatabase::from_bytes(&data).expect("parse");
        let fonts = db.by_extension("fnt");
        assert_eq!(fonts.len(), 2);
        assert_eq!(fonts[0].filename, "game.fnt");
        assert_eq!(fonts[1].filename, "vcr.fnt");
    }

    #[test]
    fn test_build_hash_dictionary() {
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(b"rules.ini\0config\0");
        data.extend_from_slice(b"art.ini\0graphics\0");

        let db = XccDatabase::from_bytes(&data).expect("parse");
        let dict = db.build_hash_dictionary();
        assert_eq!(dict.len(), 2);
        // Verify hashes match our mix_hash function.
        assert!(
            dict.iter()
                .any(|(name, hash)| name == "rules.ini" && *hash == mix_hash("rules.ini"))
        );
        assert!(
            dict.iter()
                .any(|(name, hash)| name == "art.ini" && *hash == mix_hash("art.ini"))
        );
    }

    #[test]
    fn test_extra_entries_beyond_count() {
        // Count says 1, but file has 2 pairs — second should still be parsed.
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes()); // declares 1
        data.extend_from_slice(b"first.shp\0first\0");
        data.extend_from_slice(b"second.shp\0second\0");

        let db = XccDatabase::from_bytes(&data).expect("parse");
        assert_eq!(db.len(), 2); // both parsed
    }

    #[test]
    fn test_empty_database() {
        let data = 0u32.to_le_bytes().to_vec();
        let db = XccDatabase::from_bytes(&data).expect("parse");
        assert!(db.is_empty());
    }
}
