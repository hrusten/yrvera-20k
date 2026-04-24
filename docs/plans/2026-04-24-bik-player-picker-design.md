# bik-player Picker: Archive-Centric Enumeration Design

## Goal

Make every physical `.bik` entry in every loaded MIX archive pickable from the bik-player dropdown, eliminating the RA2-over-YR shadowing caused by hash-based name collisions and exposing unknown-hash entries under a synthetic name.

## Architecture Context

### Current picker flow

`src/bin/bik-player.rs::discover_bik_assets` works filename-first:

1. Load `XccDatabase` from disk.
2. Take every XCC entry with `.bik` extension (101 names).
3. Filter to those resolvable via `mgr.get_ref(name)` (98 unique).
4. Return `Vec<String>` sorted alphabetically.

The dropdown in `src/bin/bik_player_ui.rs::draw_top_panel` renders these strings; selection calls `BikPlayerApp::load_asset(name)` which does `mgr.get_ref(name)` again — first-match-wins across the archive search order defined by `AssetManager`.

### Why this under-reports YR content

MIX archives index by a hash of the filename, not the name itself. When `movies01.mix` and `movmd03.mix` both contain `allimax.bik` under the same hash, `AssetManager::get_ref` returns the RA2 version and the YR version is unreachable by name. Diagnostic (`bik-survey --archives`) confirms:

- 143 physical `.bik` entries (by magic-byte scan) across 5 archives
- 98 unique names resolvable via the current picker
- 55 XCC-known names in `movmd03.mix` but only 15 served from there (the rest are shadowed)
- 1 unknown-hash `.bik` in `movmd03.mix` (`0x2E440187`) — invisible to any name-based picker

### Infrastructure already in place

- `AssetManager::visit_archives(|name, archive|)` — walks all loaded archives in search order
- `AssetManager::archive_entry_data(archive_name, entry_id)` — reads a specific archive's copy by hash, bypassing `get_ref`'s first-match rule
- `MixArchive::entries()` — exposes each entry's `id` (hash), `offset`, `size`
- `bik-survey::report_archives` — already implements magic-byte scanning (`BIK` / `KB2`) across all archives with reverse XCC lookup via both `mix_hash` and `westwood_hash`

No new AssetManager or MixArchive API surface is required.

## Impact Analysis

**Modified:**
- `src/bin/bik-player.rs` — new `PickerEntry` struct; `available_assets: Vec<String>` → `available_entries: Vec<PickerEntry>`; `discover_bik_assets` → `discover_bik_entries`; `load_asset` deleted; new `load_picker_entry`; status-line wording updated; `asset_name_input` field deleted.
- `src/bin/bik_player_ui.rs` — ComboBox iterates `PickerEntry`; selection dispatches through `load_picker_entry`; text-entry "MIX asset" box removed along with its label and adjacent separator.

**Unchanged:**
- `AssetManager`, `MixArchive`, `XccDatabase` — consumed only, no new methods.
- Decoder (`bink_decode`, `bink_audio`, `bink_file`), playback, UI timeline, audio sink — untouched.
- `bik-survey` — unchanged; continues as the reference implementation.
- Sim / render / engine — zero impact (change is binary-scoped).

**Risk:** low. The enumeration pattern is lifted from an already-working diagnostic. Fetch path (`archive_entry_data`) is an existing public API tested elsewhere.

## Chosen Approach

Enumerate `.bik` entries by walking each loaded archive and magic-byte-sniffing its contents. Resolve each entry's display name via a precomputed reverse XCC lookup (`hash → filename`, keyed by both `mix_hash` and `westwood_hash`), falling back to `0x{hash:08X}.bik` when the hash isn't in the XCC database. Each `PickerEntry` carries `(archive_name, entry_id, display)` so playback fetches via `archive_entry_data` and cannot be shadowed by a same-named entry in an earlier archive.

**Why this approach:**
- Reuses proven enumeration logic from `bik-survey`
- Requires no new AssetManager API
- Produces ground-truth one-to-one mapping: every physical `.bik` → exactly one picker row
- Fetch path cannot be ambiguous — the `(archive_name, entry_id)` tuple is a direct coordinate

## Design

### Components

Single file: `src/bin/bik-player.rs`. No new modules.

### Data structure

```rust
struct PickerEntry {
    archive_name: String,   // e.g. "movmd03.mix"
    entry_id: i32,          // hash id within that archive
    display: String,        // e.g. "movmd03.mix / allimax.bik"
}
```

`BikPlayerApp` field: `available_entries: Vec<PickerEntry>` replaces `available_assets: Vec<String>`. Field `asset_name_input: String` is removed.

### Interfaces / Contracts

- **`discover_bik_entries(mgr: &AssetManager, xcc: &XccDatabase) -> Vec<PickerEntry>`** — pure function, deterministic output given the same inputs. Called once at `BikPlayerApp::new`.
- **`BikPlayerApp::load_picker_entry(&mut self, entry: &PickerEntry)`** — replaces the dropdown's call path to `load_asset`. Fetches via `mgr.archive_entry_data(&entry.archive_name, entry.entry_id)`.
- **`BikPlayerApp::load_asset`** — deleted. No callers remain after the text box is removed.

### Data flow

```
Startup:
  GameConfig::load
    → AssetManager::new
    → XccDatabase::load_from_disk
    → discover_bik_entries(mgr, xcc) → Vec<PickerEntry>

Picker click (UI):
  entry: &PickerEntry
    → BikPlayerApp::load_picker_entry(entry)
    → mgr.archive_entry_data(&entry.archive_name, entry.entry_id) → Vec<u8>
    → BikPlayerApp::load_bytes(Arc<[u8]>, entry.display.clone())
```

### Enumeration algorithm (`discover_bik_entries`)

1. Build `HashMap<i32, String>` from XCC `.bik` entries, inserting both `mix_hash(filename)` and `westwood_hash(filename)` as keys. If XCC load fails, log a warning and proceed with an empty reverse map (unknown-hash fallback handles everything).
2. `mgr.visit_archives(|archive_name, archive|)`: for each `entry` in `archive.entries()`, call `archive.get_by_id(entry.id)`. If `data.len() >= 3 && (&data[..3] == b"BIK" || &data[..3] == b"KB2")`, it's a video.
3. Resolve filename: `reverse_xcc.get(&entry.id).cloned().unwrap_or_else(|| format!("0x{:08X}.bik", entry.id as u32))`.
4. Build display: `format!("{} / {}", archive_name, filename)`.
5. Push `PickerEntry { archive_name: archive_name.to_string(), entry_id: entry.id, display }`.
6. Sort by `(archive_name, display)` lexicographic.

Expected output: 143 entries (142 with XCC-resolved names, 1 with hex-hash synthetic name).

### UI changes (`draw_top_panel`)

- Change the ComboBox loop to iterate `&app.available_entries`, showing `entry.display` in each row.
- Selection stores the clicked entry's index (or clones the `PickerEntry`) into a local `Option<PickerEntry>`; after the closure returns, call `app.load_picker_entry(&picked)`.
- `selected_text` shows `app.source_name` (already set by `load_bytes` to the entry's `display`).
- Combo label changes from `"({n} .bik)"` to `"({n} .bik entries)"` for clarity (optional nicety).
- Delete: the `ui.label("MIX asset:")`, the `ui.text_edit_singleline(&mut app.asset_name_input)` block, its `resp.lost_focus() && Enter` handler, and the preceding `ui.separator()`.

### Status line (`BikPlayerApp::new`)

Replace the three-branch match:

```rust
let status = match (&asset_manager, available_entries.len()) {
    (None, _) => "No AssetManager (config.toml missing?). Use Open .bik… to load from disk.".to_string(),
    (Some(_), 0) => "No .bik entries found in loaded archives. Use Open .bik… to load from disk.".to_string(),
    (Some(_), n) => format!("{} .bik entries across loaded archives. Pick one or Open .bik… from disk.", n),
};
```

### Error handling

- XCC load failure: log warning, proceed with empty reverse map. Entries still enumerable by hex hash.
- Archive read failure during magic-byte peek: skip the entry silently (consistent with `bik-survey`).
- `archive_entry_data` returns `None` at load time (should not happen — the hash came from that archive's own entry list): set `status = "missing: {display}"`; do not panic.
- Parse failure after load: existing `load_bytes` error handling is reused.

### Testing strategy

- **Manual acceptance**: `cargo run --release --bin bik-player` → dropdown shows 143 rows sorted by archive. Pick `movmd03.mix / allimax.bik` (or any colliding name) and confirm it plays from `movmd03.mix`, not the RA2 version. Pick `movmd03.mix / 0x2E440187.bik` and confirm it loads (or fails with a decoder-level error, not "asset not found").
- **Cross-check**: the entry count from `discover_bik_entries` should equal the `total .bik in archives` figure reported by `bik-survey --archives` (143 in the current install). If they diverge, the enumeration is wrong.
- **Unit tests**: none added. A synthetic-MIX unit test would exercise scaffolding already covered by the live diagnostic and would drift independently.

## Architectural Decisions

**Patterns followed:**
- Magic-byte entry scan: mirrors `bik-survey::report_archives`.
- Reverse XCC lookup (`mix_hash` + `westwood_hash`): same dual-hash approach used throughout the asset layer.
- `archive_entry_data` for precise fetch: already the idiomatic escape hatch when `get_ref` shadowing is wrong.
- Picker logic kept inside the binary: this is a tool, not an engine feature — no `lib.rs` surface needed.

**Patterns deviated from:** none.

**Tech debt introduced:** none. The `load_asset(&str)` method is removed cleanly; no dead-code fallback or compatibility shim.

## Alternatives Considered

- **Dedupe rows by name with an archive badge** — hides the ground truth from users and introduces two display modes for the same data. Rejected.
- **Add a `prefer_md` toggle to AssetManager** — affects every consumer across the engine, far beyond the scope of a player dropdown. Separate question.
- **Grouped / tree dropdown** — egui's ComboBox doesn't support nesting cleanly; 143 flat rows are manageable.
- **`archive/name` micro-syntax in the text-entry box** — adds user-facing syntax nobody asked for; the dropdown already covers the use case.
- **Validate each entry's Bink header during enumeration** — adds startup cost; decode errors at play time already communicate the result.
