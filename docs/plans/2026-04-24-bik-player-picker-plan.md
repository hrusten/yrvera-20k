# bik-player Picker: Archive-Centric Enumeration Implementation Plan

> **For Claude:** Execute this plan task-by-task. Each task is self-contained.

**Goal:** Enumerate every physical `.bik` entry in every loaded MIX archive as its own picker row so YR-only and RA2/YR-duplicate videos are all reachable, and expose the one unknown-hash entry under a hex-hash synthetic name.

**Architecture:** Binary-local change inside the `bik-player` tool. Reuses existing `AssetManager::visit_archives` and `AssetManager::archive_entry_data` (no engine API surface changes). Mirrors the magic-byte enumeration pattern already in use by `bik-survey --archives`.

**Design Doc:** [docs/plans/2026-04-24-bik-player-picker-design.md](2026-04-24-bik-player-picker-design.md)

---

## Grounding Summary

- **ra2-rust-game-docs/:** N/A. This is a developer/diagnostic tool UI change, not gamemd.exe behavior replication. No Ghidra reports apply.
- **Ghidra / gamemd.exe:** N/A. No sim behavior, no retail engine parity to verify.
- **Repo pattern mirrored:** [src/bin/bik-survey.rs:178-232](../../src/bin/bik-survey.rs#L178-L232) (`report_archives`) — already does magic-byte scanning (`BIK` / `KB2`), dual-hash reverse XCC membership via `mix_hash` + `westwood_hash`, and `visit_archives`-based iteration. The new enumeration is a data-producing variant of the same logic.
- **AssetManager API in use:** [`visit_archives`](../../src/assets/asset_manager.rs#L413), [`archive_entry_data`](../../src/assets/asset_manager.rs#L407), [`archive`](../../src/assets/asset_manager.rs#L401). All already public and tested.
- **MixArchive API in use:** [`entries()`](../../src/assets/mix_archive.rs#L316), [`get_by_id`](../../src/assets/mix_archive.rs#L282). Unchanged.
- **XccDatabase API in use:** [`by_extension(".bik")`](../../src/assets/xcc_database.rs#L140). Unchanged.
- **INI keys:** N/A — this feature does not consume `rules(md).ini` / `art(md).ini`.
- **Unknowns after grounding:** none. All APIs exist, all data sources are understood, the live diagnostic (`bik-survey --archives`) has already produced the expected row counts (143 entries, 1 hex-hash fallback).

## Key Technical Decisions

- **Precompute reverse XCC as `HashMap<i32, String>` with both hash functions as keys.** — `AssetManager` resolves names via both `mix_hash` and `westwood_hash`; inserting both ensures every physical entry can be named regardless of which hash family the archive uses. **Confidence:** high. **Source:** repo pattern `src/bin/bik-survey.rs:181-190` (dual-hash set build).
- **Fetch bytes via `archive_entry_data(archive_name, entry_id)` rather than `get_ref(name)`.** — bypasses first-match-wins shadowing; coordinate is unambiguous. **Confidence:** high. **Source:** [src/assets/asset_manager.rs:407-410](../../src/assets/asset_manager.rs#L407-L410) + design doc Chosen Approach.
- **Unknown-hash filename format `0x{hash:08X}.bik`.** — sortable, visually distinct, reads as a clearly synthetic name. **Confidence:** high. **Source:** design doc Q4 (user-approved).
- **Sort by `(archive_name, display)` lexicographic.** — matches the archive-first label format. Deterministic. **Confidence:** high. **Source:** design doc Q3 (user-approved).
- **Delete `load_asset(&str)` and the text-entry MIX asset box.** — After the new picker covers all 143 entries and `Open .bik…` handles arbitrary disk files, the text box has no unique role. **Confidence:** high. **Source:** design doc Q5 (user-approved).

## Open Questions

### Resolved During Planning

- Granularity (1 row per physical entry vs dedup) → **1 row per physical entry**. Source: design doc Q1.
- Row label format → **`archive.mix / name.bik`**. Source: design doc Q2.
- Sort order → **alphabetical by archive, then name**. Source: design doc Q3.
- Unknown-hash format → **`0x{hash:08X}.bik`**. Source: design doc Q4.
- Keep/remove text-entry box → **remove**. Source: design doc Q5.

### Deferred to Implementation

- None.

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| Modify | `src/bin/bik-player.rs` | Add `PickerEntry`, `discover_bik_entries`, `load_picker_entry`; remove `discover_bik_assets`, `load_asset`, `asset_name_input`; rename `available_assets` → `available_entries`; update startup status wording |
| Modify | `src/bin/bik_player_ui.rs` | ComboBox iterates `PickerEntry`; selection dispatches through `load_picker_entry`; text-entry "MIX asset:" label + input + separator removed |

## Interface Changes

- `BikPlayerApp` struct (binary-internal only, no external consumers):
  - `available_assets: Vec<String>` → `available_entries: Vec<PickerEntry>`
  - `asset_name_input: String` removed
  - `load_asset(&mut self, name: &str)` removed
  - `load_picker_entry(&mut self, entry: &PickerEntry)` added
- No public API (in `src/lib.rs` or any engine module) changes.

## Sim Checklist

N/A. No sim/ files touched.

## Risk Areas

- **Shadowing fetch regression** — if `load_picker_entry` accidentally falls back to `get_ref(name)`, the fix is invisibly broken (picker shows YR row but still plays RA2 copy). Verification step in Task 2 picks a known-colliding name and confirms the source archive.
- **Orphan field / dead code** — deleting `asset_name_input` from the struct requires deleting all its UI usages in the same commit or `cargo check` breaks. Atomic commit in Task 1.
- **Count divergence** — if `discover_bik_entries` reports a different number than `bik-survey --archives`, the enumeration logic has drifted. Task 2 cross-checks the number explicitly.

## Parity-Critical Items

None. This is a developer/diagnostic tool (`bik-player` is not part of the shipping game client); there is no gamemd.exe player-facing behavior to match. The "ground truth" being matched here is `bik-survey --archives`'s own output — verified in Task 2.

---

## Tasks

### Task 1: Replace filename-based picker with archive-centric enumeration

**Why:** Single atomic refactor across both binary files. The struct field rename and method removal span both files, so split commits would leave the tree uncompilable in between.

**Files:**
- Modify: `src/bin/bik-player.rs`
- Modify: `src/bin/bik_player_ui.rs`

**Pattern:** Mirrors `src/bin/bik-survey.rs::report_archives` (magic-byte scan + dual-hash XCC reverse lookup via `visit_archives`). Fetch via `AssetManager::archive_entry_data` follows the existing pattern where callers need a specific archive's copy rather than first-match.

**Step 1: Update imports in `src/bin/bik-player.rs`**

At the top of the file (after the existing `use` lines), add:

```rust
use std::collections::HashMap;
use vera20k::assets::mix_hash::{mix_hash, westwood_hash};
```

**Step 2: Add `PickerEntry` struct**

Insert after the existing `use` block and before `fn main()`:

```rust
/// One physical `.bik` entry in a loaded MIX archive.
///
/// Carries `(archive_name, entry_id)` as the fetch coordinate so
/// `AssetManager::archive_entry_data` can return that specific copy,
/// bypassing first-match-wins shadowing between archives that share
/// the same filename hash.
#[derive(Clone)]
pub struct PickerEntry {
    pub archive_name: String,
    pub entry_id: i32,
    pub display: String,
}
```

**Step 3: Replace `BikPlayerApp` field declarations**

In the `pub struct BikPlayerApp` block, remove:

```rust
    pub asset_name_input: String,
    pub available_assets: Vec<String>,
```

Replace with:

```rust
    pub available_entries: Vec<PickerEntry>,
```

The two removed doc comments go with their fields.

**Step 4: Rewrite `discover_bik_assets` as `discover_bik_entries`**

Replace the entire existing function:

```rust
fn discover_bik_assets(mgr: &AssetManager) -> Vec<String> { ... }
```

with:

```rust
/// Enumerate every physical `.bik` entry across all loaded MIX archives.
///
/// Walks each archive, magic-byte-sniffs every entry (`BIK` or `KB2` header),
/// and resolves filenames through a reverse XCC lookup keyed by both
/// `mix_hash` and `westwood_hash`. Unknown hashes get a synthetic
/// `0x{hash:08X}.bik` label. Returned entries are sorted by
/// `(archive_name, display)` lexicographic.
fn discover_bik_entries(mgr: &AssetManager) -> Vec<PickerEntry> {
    let reverse_xcc: HashMap<i32, String> = match XccDatabase::load_from_disk() {
        Ok(xcc) => {
            let mut map = HashMap::new();
            for entry in xcc.by_extension(".bik") {
                map.insert(mix_hash(&entry.filename), entry.filename.clone());
                map.insert(westwood_hash(&entry.filename), entry.filename.clone());
            }
            map
        }
        Err(e) => {
            log::warn!(
                "XCC database not available ({}); unknown-hash fallback used for all entries",
                e
            );
            HashMap::new()
        }
    };

    let mut entries: Vec<PickerEntry> = Vec::new();
    mgr.visit_archives(|archive_name, archive| {
        for mix_entry in archive.entries() {
            let Some(data) = archive.get_by_id(mix_entry.id) else {
                continue;
            };
            if data.len() < 3 {
                continue;
            }
            if &data[..3] != b"BIK" && &data[..3] != b"KB2" {
                continue;
            }
            let filename = reverse_xcc
                .get(&mix_entry.id)
                .cloned()
                .unwrap_or_else(|| format!("0x{:08X}.bik", mix_entry.id as u32));
            let display = format!("{} / {}", archive_name, filename);
            entries.push(PickerEntry {
                archive_name: archive_name.to_string(),
                entry_id: mix_entry.id,
                display,
            });
        }
    });
    entries.sort_by(|a, b| {
        a.archive_name
            .to_ascii_lowercase()
            .cmp(&b.archive_name.to_ascii_lowercase())
            .then_with(|| a.display.cmp(&b.display))
    });
    log::info!(
        "bik-player: {} .bik entries discovered across {} archives",
        entries.len(),
        mgr.loaded_archive_names().len()
    );
    entries
}
```

Signature change: now takes only `&AssetManager` (XCC is loaded internally, since failure is non-fatal and confined to this function).

**Step 5: Update `BikPlayerApp::new`**

Inside `impl BikPlayerApp { fn new(...) }`, replace:

```rust
        let available_assets = asset_manager
            .as_ref()
            .map(discover_bik_assets)
            .unwrap_or_default();
        let status = match (&asset_manager, available_assets.len()) {
            (None, _) => "No AssetManager (config.toml missing?). Use Open .bik… to load from disk.".to_string(),
            (Some(_), 0) => "No .bik assets found in loaded archives. Use Open .bik… to load from disk.".to_string(),
            (Some(_), n) => format!("{} .bik assets available. Pick one or Open .bik… from disk.", n),
        };
```

with:

```rust
        let available_entries = asset_manager
            .as_ref()
            .map(discover_bik_entries)
            .unwrap_or_default();
        let status = match (&asset_manager, available_entries.len()) {
            (None, _) => "No AssetManager (config.toml missing?). Use Open .bik… to load from disk.".to_string(),
            (Some(_), 0) => "No .bik entries found in loaded archives. Use Open .bik… to load from disk.".to_string(),
            (Some(_), n) => format!("{} .bik entries across loaded archives. Pick one or Open .bik… from disk.", n),
        };
```

Then inside the `Self { ... }` struct initializer, remove the `asset_name_input: String::new(),` line and replace `available_assets,` with `available_entries,`.

**Step 6: Delete `BikPlayerApp::load_asset` and add `load_picker_entry`**

Remove the entire existing method:

```rust
    pub fn load_asset(&mut self, name: &str) {
        let Some(mgr) = self.asset_manager.as_ref() else {
            self.status = "No AssetManager available (config.toml missing?)".to_string();
            return;
        };
        let Some(bytes) = mgr.get_ref(name) else {
            self.status = format!("asset not found: {}", name);
            return;
        };
        self.load_bytes(Arc::<[u8]>::from(bytes), name.to_string());
    }
```

Add in its place:

```rust
    /// Load a picker entry via its precise (archive, entry_id) coordinate.
    /// Bypasses `get_ref`'s first-match-wins rule so a shadowed copy in a
    /// later archive is still reachable.
    pub fn load_picker_entry(&mut self, entry: &PickerEntry) {
        let Some(mgr) = self.asset_manager.as_ref() else {
            self.status = "No AssetManager available (config.toml missing?)".to_string();
            return;
        };
        match mgr.archive_entry_data(&entry.archive_name, entry.entry_id) {
            Some(bytes) => self.load_bytes(Arc::<[u8]>::from(bytes), entry.display.clone()),
            None => self.status = format!("missing: {}", entry.display),
        }
    }
```

**Step 7: Update `draw_top_panel` in `src/bin/bik_player_ui.rs`**

Replace the ComboBox block (`if !app.available_assets.is_empty() { ... }` and everything inside through its trailing `ui.separator();`) plus the text-entry block (`ui.label("MIX asset:"); ... if resp.lost_focus() ...`) with a single block:

```rust
            // Dropdown of every physical .bik entry in every loaded MIX archive.
            if !app.available_entries.is_empty() {
                let current = if app.source_name.is_empty() {
                    "— pick a .bik —".to_string()
                } else {
                    app.source_name.clone()
                };
                let mut picked: Option<PickerEntry> = None;
                egui::ComboBox::from_label(format!("({} .bik entries)", app.available_entries.len()))
                    .selected_text(current)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for entry in &app.available_entries {
                            if ui
                                .selectable_label(app.source_name == entry.display, &entry.display)
                                .clicked()
                            {
                                picked = Some(entry.clone());
                            }
                        }
                    });
                if let Some(entry) = picked {
                    app.load_picker_entry(&entry);
                }
                ui.separator();
            }
```

Also add `use crate::PickerEntry;` at the top of `src/bin/bik_player_ui.rs` alongside the existing `use crate::BikPlayerApp;`.

Verify the text-entry block and its `ui.label("MIX asset:")` / `text_edit_singleline` / `resp.lost_focus()` / trailing `ui.separator()` are fully gone.

**Step 8: Build and check**

Run: `cargo check --bin bik-player`
Expected: succeeds with no warnings about dead code in bik-player.rs or bik_player_ui.rs related to our changes. Other unrelated warnings elsewhere in the tree are fine.

**Step 9: Commit**

```
bik-player: enumerate physical .bik entries per archive

Replace the XCC-filename picker with an archive-centric enumeration so
RA2/YR duplicate-name .bik files are both reachable and the one
unknown-hash entry in movmd03.mix is exposed under 0x{hash:08X}.bik.
Fetch via AssetManager::archive_entry_data bypasses first-match
shadowing. Removes the now-redundant MIX asset text-entry box.
```

---

### Task 2: Manual verification

**Why:** No unit test covers this (it exercises real MIX content). Cross-check the new picker's count and behavior against `bik-survey --archives`, which has the same enumeration logic and a separate reporting path.

**Files:** none (verification only).

**Step 1: Get baseline count from the diagnostic**

Run: `cargo run --release --bin bik-survey -- --archives`
Expected: the `== total .bik in archives: N (...) ==` line at the bottom. Record `N` (currently 143 in the dev install — may differ on other installs).

**Step 2: Launch the player**

Run: `cargo run --release --bin bik-player`
Expected: a 960×720 window titled "vera20k bik-player". Status line reads `{N} .bik entries across loaded archives. Pick one or Open .bik… from disk.` matching the `N` from Step 1.

**Step 3: Verify dropdown row count**

Click the ComboBox label `({N} .bik entries)`. Scroll through the list.
Expected: `N` rows total. Rows are grouped by archive in ASCII lowercase order. On the dev install that resolves to `langmd.mix / ...` first (at char 4, `'m' < 'u'`), then `language.mix / ...`, then `movies01.mix / ...`, `movies02.mix / ...`, `movmd03.mix / ...`. On other installs the set of archives may differ; the guarantee is alphabetical by archive name, not a fixed sequence.

**Step 4: Verify shadowed-name picks resolve to the selected archive**

Pick a row of the form `movmd03.mix / X.bik` where `X` is known to exist in both `movies01.mix` (or `movies02.mix`) and `movmd03.mix` — e.g., `allimax.bik` or any YR-duplicated name visible in the dropdown.
Expected: the status line reads `loaded movmd03.mix / X.bik: ...` — specifically starting with `movmd03.mix / `, not `movies01.mix / ` or a bare filename. Video begins playing.

**Step 5: Verify unknown-hash entry is pickable**

Pick `movmd03.mix / 0x2E440187.bik` (or whatever hex-hash entry the current install produces — Step 1's diagnostic output lists unresolved hashes).
Expected: either a successful `loaded movmd03.mix / 0x{hash:08X}.bik: {width}x{height}...` message, or a decoder error like `parse error: ...`. Must NOT be a `missing:` or `asset not found:` message — those indicate the fetch path is wrong.

**Step 6: Verify text-entry box is gone**

Inspect the top panel.
Expected: no `MIX asset:` label, no text input field. Only `Open .bik…`, the dropdown, `Vol`, slider, and Mute/Unmute button remain.

**Step 7: Verify `Open .bik…` still works**

Click `Open .bik…`, select any `.bik` from disk.
Expected: loads and plays as before. Status line shows the filesystem path, not an archive-prefixed display.

**Step 8: If everything passes**

No further commit needed — Task 1's commit covers the change. If any step fails, stop and report the failure mode; do not commit a regression.

---

## Sources & References

- **Design doc:** [docs/plans/2026-04-24-bik-player-picker-design.md](2026-04-24-bik-player-picker-design.md)
- **Repo pattern mirrored:** [src/bin/bik-survey.rs:178-232](../../src/bin/bik-survey.rs#L178-L232) (`report_archives`)
- **AssetManager APIs consumed:** [`visit_archives`](../../src/assets/asset_manager.rs#L413), [`archive_entry_data`](../../src/assets/asset_manager.rs#L407), [`loaded_archive_names`](../../src/assets/asset_manager.rs#L423)
- **MixArchive APIs consumed:** [`entries()`](../../src/assets/mix_archive.rs#L316), [`get_by_id`](../../src/assets/mix_archive.rs#L282)
- **XccDatabase API consumed:** [`by_extension`](../../src/assets/xcc_database.rs#L140)
- **Hash functions:** `vera20k::assets::mix_hash::{mix_hash, westwood_hash}` (same pair used in `bik-survey`)
- **Ghidra / ra2-rust-game-docs / INI:** N/A for this change (tool UI, no gamemd.exe behavior)
