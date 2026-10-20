# PeaceEmu-GB

**PeaceEmu's experimental Game Boy Advance emulation core.**

PeaceEmu-GB is a community-driven, open-source Rust project. It is an early hardware-emulation core, not yet a complete, cycle-accurate emulator or a ready-to-use game launcher.

## Current implementation

- GBA cartridge header parsing, basic validation, title/game-code access, and save-type detection.
- An ARM7TDMI execution core with implemented ARM-state and Thumb instruction subsets.
- A memory bus with GBA address decoding, work RAM, palette RAM, VRAM, OAM, keypad input, and I/O registers.
- Timer progression, scanline/VBlank/HBlank timing, interrupt flags, and several DMA paths.
- SRAM, Flash, and EEPROM save-device handling and save-data import/export APIs.
- Initial rendering for tiled backgrounds and bitmap display modes, plus regular and affine OBJ rendering.

These features are partial implementations. Passing unit tests does not imply compatibility with every commercial game or hardware edge case.

## Requirements

- Rust toolchain **1.78 or newer** (stable recommended).
- Cargo, included with Rust.

## Build and test

From the repository root:

```sh
cargo test --manifest-path GameBoy/Cargo.toml
cargo check --manifest-path GameBoy/Cargo.toml
cargo fmt --manifest-path GameBoy/Cargo.toml -- --check
```

To build the library:

```sh
cargo build --manifest-path GameBoy/Cargo.toml
```

## Project status

The main priorities are improving hardware accuracy and expanding integration between the CPU, memory bus, DMA, video, interrupts, and save devices. Timing is still approximate; complete exception handling, wait-state accuracy, full audio output, and comprehensive game compatibility are not claimed.

Please report reproducible issues with the Rust version, operating system, command run, expected behavior, and actual behavior. Do not upload commercial ROMs or copyrighted game assets to issues.

## Contributing

Bug fixes and focused hardware-accuracy improvements are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Keep changes scoped, document observable behavior, and add regression coverage for fixes.

## License

This project is distributed under the MIT License; see [LICENSE](LICENSE). Copyright remains with each respective rights holder for their own contributions.
