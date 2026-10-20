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

## Minimal integration example

The library exposes a high-level machine facade so callers do not have to wire
the CPU and bus manually:

```rust
use gameboy::{GbaCartridge, GbaMachine};

fn start(rom_bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let cartridge = GbaCartridge::parse(rom_bytes)?;
    let mut machine = GbaMachine::new(cartridge);

    // Run a small batch; modeled peripherals advance by estimated cycles.
    let cycles = machine.run_steps(100)?;
    machine.set_keys_pressed(0); // all buttons released
    let _save_snapshot = machine.save_data(); // None if the ROM save type is unknown
    println!("step took {cycles} estimated cycles");

    if let Some(frame) = machine.render_frame() {
        // frame.pixels contains opaque 0xAARRGGBB pixels.
        println!("rendered {}x{} pixels", frame.width, frame.height);
    }
    Ok(())
}
```

The machine facade also exposes `run_for_cycles(budget)` for approximate cycle-budget
execution, `run_until_scanline(line, max_steps)` for bounded scanline synchronization,
`run_until_vblank(max_steps)` for VBlank synchronization, `run_until_hblank(max_steps)`
for HBlank synchronization, and `run_until_interrupt(max_steps)` for observing enabled pending IRQs. Instruction-level
cycle budgets may overshoot by the last instruction, and these helpers stop at their
event or limit; IRQ exception entry is not implemented yet. The initial program counter is 0x08000000. The facade is a
development API, not a promise of complete commercial-game compatibility or accurate
frame pacing.

## Project status

The main priorities are improving hardware accuracy and expanding integration between the CPU, memory bus, DMA, video, interrupts, and save devices. Timing is still approximate; complete exception handling, wait-state accuracy, full audio output, and comprehensive game compatibility are not claimed.

Please report reproducible issues with the Rust version, operating system, command run, expected behavior, and actual behavior. Do not upload commercial ROMs or copyrighted game assets to issues.

## Contributing

Bug fixes and focused hardware-accuracy improvements are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Keep changes scoped, document observable behavior, and add regression coverage for fixes.

## License

This project is distributed under the MIT License; see [LICENSE](LICENSE). Copyright remains with each respective rights holder for their own contributions.
