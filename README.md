# VERA20k

[![Discord](https://img.shields.io/badge/Discord-Join%20Server-5865F2?logo=discord&logoColor=white)](https://discord.gg/4jTp4VcdY)![PRs Welcome](https://img.shields.io/badge/PRs-welcome-blue.svg)[![Docs](https://img.shields.io/badge/Docs-GitHub%20Pages-blue)](https://hrusten.github.io/vera20k/)

**Early development**

Red Alert 2: Yuri's Revenge — rebuilt from scratch in Rust for large multiplayer battles.

Those who contribute the most get to decide the most. As long as it aligns with projects vision described below.

Main repository for the **vera20k Engine**

This project stands on the shoulders of giants. Thanks to OpenRA, XCC mixer, ModEnc website, PPM website, EA for open source RA1, World Altering Editor, Final Alert,YRpp, Ares, Phobos and much more.

---

## Project Goals

<small>

**1. Faithful Engine Replacement**
A drop-in replacement for `gamemd.exe` that stays 100% true to the original Westwood RA2 visual fidelity, atmosphere, and gameplay.

**2. Built for Scale**
Constructed from the ground up for large multiplayer — targeting support for up to **30 players** and **20,000 units** on significantly bigger maps.


## Current Status
 
**Early development** — Work is focused on the core engine. Approximately 20% complete.


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
