# PeaceEmu GBA

PeaceEmu GBA is an experimental Game Boy Advance emulator core with a small cross-platform desktop runner.

## Run a game

Install a recent stable Rust toolchain, then run:

```powershell
cargo run --release --manifest-path GameBoy/Cargo.toml --bin peaceemu-gba -- "C:\path\to\game.gba"
```

The ROM must be a valid GBA image with a valid Nintendo header checksum. The runner opens a 4× resizable window, reads keyboard input, and writes battery-backed cartridge data to a sibling `.sav` file when you close it.

## Keyboard

| GBA button | Keyboard |
| --- | --- |
| A | Z |
| B | X |
| L | A |
| R | S |
| Start | Enter |
| Select | Backspace |
| D-pad | Arrow keys |
| Quit | Escape |

## Current limitations

- This is an early, incomplete GBA emulator, not a compatibility-certified emulator. Many commercial games may fail or render incorrectly.
- Unsupported ARM/Thumb instructions stop emulation and are reported in the terminal.
- Video output supports the currently implemented GBA display modes; timing and bus wait states are not yet cycle-accurate.
- The core can generate queued PCM samples, but the desktop runner does not yet connect them to a host audio device.
- Game Boy and Game Boy Color emulation are not implemented in this repository yet.

Please use ROM images you are legally entitled to use.