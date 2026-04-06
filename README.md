# VERA20k

[![Discord](https://img.shields.io/badge/Discord-Join%20Server-5865F2?logo=discord&logoColor=white)](https://discord.gg/4jTp4VcdY)![PRs Welcome](https://img.shields.io/badge/PRs-welcome-blue.svg)[![Docs](https://img.shields.io/badge/Docs-GitHub%20Pages-blue)](https://hrusten.github.io/vera20k/)

**Early development**

Red Alert 2: Yuri's Revenge — rebuilt from scratch in Rust for large multiplayer battles.

Those who contribute the most get to decide the most. As long as it aligns with projects vision described below.

Main repository for the **vera20k Engine** and **vera20k: Allied Uprising**, a standalone game built on RA2:YR game assets.

This project stands on the shoulders of giants. Thanks to OpenRA, XCC mixer, ModEnc website, PPM website, EA for open source RA1, World Altering Editor, Final Alert,YRpp, Ares, Phobos and much more.

---

## Project Goals

<small>

**1. Faithful Engine Replacement**
A drop-in replacement for `gamemd.exe` that stays 100% true to the original Westwood RA2 visual fidelity, atmosphere, and gameplay.

**2. Built for Scale**
Constructed from the ground up for large multiplayer — targeting support for up to **30 players** and **20,000 units** on significantly bigger maps.

**3. Integrated Multiplayer Client**
A multiplayer client embedded directly into the game, supporting both original RA2:YR and Allied Uprising. Built to handle 30-player lobbies and designed to minimize cheating, lag, and toxic behavior.

**4. Allied Uprising**
A full game built on the Vera 20k engine using RA2:YR game assets. Same Westwood RA2 atmosphere and core gameplay, but with added strategic depth tailored for large team games, co-op, and FFA.

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

</small>


## Current Status
 
**Early development** — Work is focused on the core engine. Approximately 15% complete.


## Requirements

- [Rust](https://rustup.rs/) 1.85+ (edition 2024)
- A copy of **Red Alert 2: Yuri's Revenge** (the engine reads .mix files from your install)


## Setup

1. Clone the repo:
   ```
   git clone https://github.com/hrusten/vera20k.git
   cd vera20k
   ```

2. Copy the example config and set your RA2 install path:
   ```
   cp config.toml.example config.toml
   ```
   Edit `config.toml` and set `ra2_dir` to where your RA2/YR is installed.

3. Build and run:
   ```
   cargo run --bin vera20k
   ```

## Contributing

Read the [architecture overview](https://hrusten.github.io/vera20k/) before diving in.

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes and commit
4. Push to your fork and open a Pull Request

## License

- [GNU General Public License v3.0](LICENSE-GPL)
