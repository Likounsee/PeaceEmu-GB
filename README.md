# PeaceEmu-GB

**PeaceEmu's experimental Game Boy Advance emulation core.**

PeaceEmu-GB is a community-driven, open-source Rust project. It is an early hardware-emulation core, not yet a complete, cycle-accurate emulator or a ready-to-use game launcher.

## Current implementation

- GBA cartridge header parsing, basic validation, title/game-code access, and save-type detection.
- An ARM7TDMI execution core with implemented ARM-state and Thumb instruction subsets.
- A memory bus with GBA address decoding, work RAM, palette RAM, VRAM, OAM, keypad input, and I/O registers.
- Timer progression, scanline/VBlank/HBlank timing, interrupt flags, and several DMA paths.
- WAITCNT decoding for ROM0/ROM1/ROM2 and SRAM, including transfer-width-aware bus-duration estimates for 8-, 16-, and 32-bit accesses (not yet integrated into CPU instruction timing).
- SRAM, Flash, and EEPROM save-device handling and save-data import/export APIs.
- Initial rendering for tiled backgrounds and bitmap display modes, plus regular and affine OBJ rendering. Frame export supports RGB/RGBA/BGRA/BGR888, RGB555, and RGB565 byte layouts.
- A high-level machine facade for bounded execution, program-counter breakpoints/revisit waits, VBlank-synchronized frame capture, multi-frame capture, and callback-based frame streaming.
- Initial Direct Sound FIFO queueing, timer-driven FIFO consumption, stereo sample inspection, and FIFO status snapshots (queue length/capacity/refill-threshold hint).

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

The memory bus exposes `gamepak_access_cycles(address, sequential)` to decode the configured WAITCNT access duration for ROM0/ROM1/ROM2 and SRAM. This reports the configured access duration for timing integrations; CPU execution does not yet automatically apply Game Pak wait states.

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

        // Export for common frontend/GPU upload formats (all row-major).
        let rgb = frame.to_rgb888().expect("valid framebuffer");
        let rgba = frame.to_rgba8888().expect("valid framebuffer");
        let bgra = frame.to_bgra8888().expect("valid framebuffer");
        let rgb565 = frame.to_rgb565_le().expect("valid framebuffer");
        let rgb565_be = frame.to_rgb565_be().expect("valid framebuffer");
        let rgb555 = frame.to_rgb555_le().expect("valid framebuffer");
        let bgr = frame.to_bgr888().expect("valid framebuffer");
        let grayscale = frame.to_luma8().expect("valid framebuffer");
        let center = frame.crop(40, 20, 160, 120).expect("valid crop");
        println!("RGB bytes: {}, RGBA bytes: {}", rgb.len(), rgba.len());
        println!("BGRA bytes: {}, BGR bytes: {}", bgra.len(), bgr.len());
        println!("RGB565 LE bytes: {}, BE bytes: {}", rgb565.len(), rgb565_be.len());
        println!("RGB555 LE bytes: {}", rgb555.len());
    }
    Ok(())
}
```

The machine facade also exposes `run_for_cycles(budget)` for approximate cycle-budget
execution, `run_until_pc(address, max_steps)` for a bounded PC breakpoint and
`run_until_pc_revisit(address, max_steps)` for waiting until an address is revisited, `run_until_audio_fifo_refill(fifo, max_steps)` to run until Direct Sound FIFO A (0)
or B (1) reaches its 16-byte refill threshold, `run_until_scanline(line, max_steps)` for bounded scanline synchronization,
`run_until_vblank(max_steps)` for VBlank synchronization, `run_until_hblank(max_steps)`
for HBlank synchronization, `run_until_vcount_match(max_steps)` for the programmed DISPSTAT compare event,
and `run_until_interrupt(max_steps)` for observing enabled pending IRQs. `video_cycles_until_hblank()` and `video_cycles_until_vblank()` expose cycle distances to the next blanking events, including wraparound to the next frame. `VideoFrame::crop(x, y, width, height)` extracts a checked rectangle, while `to_luma8()` exports one grayscale byte per pixel. The masked variants `run_until_interrupt_mask(mask, max_steps)` and `run_until_next_interrupt_mask(mask, max_steps)` wait for selected IF/IE sources without stopping for unrelated IRQs; a zero mask never matches. Instruction-level
cycle budgets may overshoot by the last instruction, and these helpers stop at their
event or limit; IRQ exception entry is not implemented yet. The initial program counter is 0x08000000. The facade is a
development API, not a promise of complete commercial-game compatibility or accurate
frame pacing.

Timer inspection now includes the masked TM control register, selected prescaler divisor, live prescaler phase, overflow-IRQ enable, and cascade-mode bit through `timer_control()`, `timer_prescaler_divisor()`, `timer_prescaler_phase()`, `timer_irq_enabled()`, and `timer_cascade_enabled()`. `gamepak_wait_control()` exposes the current WAITCNT configuration for frontends and timing diagnostics. These accessors inspect configuration only and do not add WAITCNT wait states to CPU instruction timing.

The machine facade also forwards diagnostic snapshots for interrupt IE/IF/IME state (`interrupt_enable_mask()`, `interrupt_request_flags()`, and `interrupt_master_enabled()`), live video status (`video_scanline()`, `video_in_vblank()`, `video_in_hblank()`, and `video_vcount_match()`), Game Pak access-duration estimates (`gamepak_access_cycles()`), and Direct Sound FIFO queue/sample state (`audio_fifo_len()` and `audio_fifo_last_sample()`). These are inspection APIs and do not add CPU wait-state integration or IRQ exception entry.

`GbaMachine::run_until_timer_overflow(timer, max_steps)` can synchronize a bounded run to an enabled non-cascade timer's estimated overflow. It returns `None` for unsupported timer modes or when the instruction limit expires, and may overshoot by the last instruction. Timer inspection is available through `timer_counter(index)`, `timer_reload_value(index)`, and `timer_enabled(index)`. `timer_cycles_until_overflow(index)` estimates the remaining CPU cycles for enabled non-cascade timers and accounts for the active prescaler phase; it returns `None` when the timer is disabled or uses cascade mode. Direct Sound FIFO users can call `audio_fifo_bytes_until_refill(fifo)` on either `MemoryBus` or `GbaMachine` to check how many bytes remain before the 16-byte refill threshold.

## Project status

The main priorities are improving hardware accuracy and expanding integration between the CPU, memory bus, DMA, video, interrupts, and save devices. Timing is still approximate; complete exception handling, wait-state accuracy, full audio output, and comprehensive game compatibility are not claimed.

Please report reproducible issues with the Rust version, operating system, command run, expected behavior, and actual behavior. Do not upload commercial ROMs or copyrighted game assets to issues.

## Contributing

Bug fixes and focused hardware-accuracy improvements are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Keep changes scoped, document observable behavior, and add regression coverage for fixes.

## License

This project is distributed under the MIT License; see [LICENSE](LICENSE). Copyright remains with each respective rights holder for their own contributions.
