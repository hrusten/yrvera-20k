# Yrvera-20k 

**Early development**

Red Alert 2: Yuri's Revenge — rebuilt from scratch in Rust for large multiplayer battles.

[![Discord](https://img.shields.io/badge/Discord-Join%20Server-5865F2?logo=discord&logoColor=white)](https://discord.gg/4jTp4VcdY)

Main repository for the **Yrvera-20k Engine** and **Yrvera-20k: Allied Uprising**, a standalone game built on RA2:YR game assets.

This project stands on the shoulders of giants. Thanks to OpenRA team, ModEnc team, PPM team, EA for open source RA1, Modders, World Altering Editor, Final Alert,YRpp and much more.

---

## Project Goals

### 1. Faithful Engine Replacement

A drop-in replacement for `gamemd.exe` that stays 100% true to the original Westwood RA2 visual fidelity, atmosphere, and gameplay. Built entirely on original game files.

### 2. Built for Scale

Engineered from the ground up for large multiplayer — targeting support for up to **30 players** and **20,000 units** on significantly bigger maps.

### 3. Quality-of-Life Improvements

Modernized battle controls while preserving the RA2 feel — formation movement, area commands, and more.

### 4. Integrated Multiplayer Client

A multiplayer client embedded directly into the game, supporting both original RA2:YR and Allied Uprising. Built to handle 30-player lobbies and designed to minimize cheating, lag, and toxic behavior.

### 5. Allied Uprising

A full game built on the Yrvera-20k engine using RA2:YR game assets. Same Westwood RA2 atmosphere and core gameplay, but with added strategic depth tailored for large team games, co-op, and FFA.

All new art assets stay true to the original Westwood RA2 style.

Feature scope is still being defined:

- Enhanced economy and combat systems
- New domains of war
- Asymmetric game modes (General / Commander)
- Advanced fog of war
- New units not possible in the original engine
- Upscaled graphics faithful to the original look
- Improved battle controls
- Redesigned HUD and sidebar
- New map features and systems
- Camera zoom
- Advanced minimap


## Current Status
 
**Early development** — Work is focused on the core engine. Approximately 10% complete.


## Requirements

- [Rust](https://rustup.rs/) 1.85+ (edition 2024)
- A copy of **Red Alert 2: Yuri's Revenge** (the engine reads .mix files from your install)


## Setup

1. Clone the repo:
   ```
   git clone https://github.com/hrusten/yrvera-20k.git
   cd yrvera-20k
   ```

2. Copy the example config and set your RA2 install path:
   ```
   cp config.toml.example config.toml
   ```
   Edit `config.toml` and set `ra2_dir` to where your RA2/YR is installed.

3. Build and run:
   ```
   cargo run --bin yrvera
   ```

## Contributing

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes and commit
4. Push to your fork and open a Pull Request

## License

- [GNU General Public License v3.0](LICENSE-GPL)
- [European Union Public License v1.2](LICENSE-EUPL)
