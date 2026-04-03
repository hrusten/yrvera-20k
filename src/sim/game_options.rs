//! Per-match game settings — the lobby options card.
//!
//! Parsed from `[MultiplayerDialogSettings]` in rulesmd.ini. Set once at game
//! start, read-only during gameplay. Included in the deterministic state
//! hash for lockstep correctness.

/// Per-match game settings from the lobby / `[MultiplayerDialogSettings]`.
///
/// Set once at game start, read-only during gameplay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GameOptions {
    // --- Runtime-checked by gameplay systems ---
    /// Defeat when all buildings lost (vs all objects lost). Rules+0x14B6.
    pub short_game: bool,
    /// Construction yards / base building enabled. Rules+0x14AF.
    pub bases: bool,
    /// Bridges can be destroyed. Rules+0x14AC.
    pub bridges_destroyable: bool,
    /// Superweapons can be built. Rules+0x14B9.
    pub super_weapons: bool,
    /// Build adjacent to allied buildings. Rules+0x14BA.
    pub build_off_ally: bool,
    /// Random crate spawning. Rules+0x14B1.
    pub crates: bool,
    /// MCV can repack into vehicle. Rules+0x14B8.
    pub mcv_redeploy: bool,
    /// TS-legacy semi-transparent fog. Default false in YR. Rules+0x14B7.
    pub fog_of_war: bool,
    /// Unexplored cells are black. Rules+0x14AE.
    pub shroud: bool,
    /// Ore/gems regenerate on the map. Rules+0x14B0.
    pub tiberium_grows: bool,
    /// Engineers capture at reduced HP only. Rules+0x14B4.
    pub multi_engineer: bool,
    /// Harvesters immune to enemy fire. Rules+0x14B3.
    pub harvester_truce: bool,
    /// Alliances can be changed mid-game. Rules+0x14BB (YR addition).
    pub ally_change_allowed: bool,

    // --- Used at init (Create_Houses, spawn units) ---
    /// Default starting credits per player. Rules+0x1484.
    pub starting_credits: i32,
    /// Number of starting units to spawn. Rules+0x1494.
    pub unit_count: i32,
    /// Maximum tech level for this match. Rules+0x149C.
    pub tech_level: i32,
    /// Game speed (0=fastest, 6=slowest). Rules+0x14A0.
    pub game_speed: i32,
    /// AI difficulty (0=Easy, 1=Normal, 2=Hard). Rules+0x14A4.
    pub ai_difficulty: i32,
    /// Number of AI opponents. Rules+0x14A8.
    pub ai_players: i32,
}

impl Default for GameOptions {
    /// Defaults from `[MultiplayerDialogSettings]` in rulesmd.ini (YR).
    fn default() -> Self {
        Self {
            short_game: true,
            bases: true,
            bridges_destroyable: true,
            super_weapons: true,
            build_off_ally: false,
            crates: true,
            mcv_redeploy: true,
            fog_of_war: false,
            shroud: true,
            tiberium_grows: true,
            multi_engineer: false,
            harvester_truce: false,
            ally_change_allowed: true,
            starting_credits: 10000,
            unit_count: 10,
            tech_level: 10,
            game_speed: 1,
            ai_difficulty: 0,
            ai_players: 0,
        }
    }
}
