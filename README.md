# PeaceEmu-GB

**PeaceEmu's experimental Game Boy Advance emulation core.**

PeaceEmu-GB is a community-driven, open-source Rust project. It is an early hardware-emulation core, not yet a complete, cycle-accurate emulator or a ready-to-use game launcher.

## Current implementation

The CPU exposes `set_execution_state` and `set_condition_flags` for test harnesses, external loaders, and debugger setup. These helpers align the program counter for ARM/Thumb state and let tools seed N/Z/C/V without executing setup instructions. Guest code should use architectural instructions such as `BX` to change execution state.


- GBA cartridge header parsing and checksum validation, title/game-code/maker-code and ROM-size access, fixed-header-byte and software-revision diagnostics, and save-type detection; metadata is exposed through both the cartridge and high-level machine APIs.
- An ARM7TDMI execution core with implemented ARM-state and Thumb instruction subsets.
- A memory bus with GBA address decoding, work RAM, palette RAM, VRAM, OAM, keypad input, and I/O registers.
- Timer progression, scanline/VBlank/HBlank timing, interrupt flags, and several DMA paths.
- WAITCNT decoding for ROM0/ROM1/ROM2 and SRAM, including transfer-width-aware bus-duration estimates for 8-, 16-, and 32-bit accesses. ROM estimates align halfword/word transfers to the physical bus address; SRAM estimates preserve byte-wide timing. Instruction-fetch timing, supported scalar ARM/Thumb data accesses (including byte and word transfers), SWP/SWPB read-modify-write operations, supported ARM/Thumb multiple-transfer bursts, and ARM register-offset scalar transfers with immediate or register-specified shifts and ARM halfword/signed transfers with immediate or unshifted-register offsets now add modeled excess WAITCNT latency; regression tests cover ARM/Thumb loads and stores, immediate LDRSB/LDRSH/STRH, post-indexed addressing, register-shifted offsets, swap operations, WAITCNT sensitivity, and word bursts crossing ROM wait-state regions. DMA stalls remain unintegrated.
- SRAM, Flash, and EEPROM save-device handling and save-data import/export APIs; the machine facade exposes the detected backup-device type and current save-image size without copying the image. Diagnostic snapshots also include copied cartridge title, game/maker codes, header checksum, ROM size, and save metadata.
- Initial rendering for tiled backgrounds and bitmap display modes, plus regular and affine OBJ rendering. Frame export supports RGB/RGBA/BGRA/BGR888, RGB555, and RGB565 byte layouts.
- A high-level machine facade for bounded execution, program-counter breakpoints/revisit waits, VBlank-synchronized frame capture, multi-frame capture, and callback-based frame streaming.
- Initial Direct Sound FIFO queueing, timer-driven FIFO consumption, raw integer stereo mixing with per-FIFO routing, correct half/full-volume attenuation (including signed negative samples), and master-sound gating, and FIFO status snapshots (queue length/capacity/refill-threshold hint).

These features are partial implementations. Passing unit tests does not imply compatibility with every commercial game or hardware edge case.

## Requirements

- Rust toolchain **1.78 or newer** (stable recommended).
- Cargo, included with Rust.

Hardware regression coverage now also checks DISPSTAT/VCOUNT read-only status semantics and VCOUNT IRQ edge-latching, timer prescaler/cascade overflow sequencing, and VRAM byte-write replication after the 96 KiB mirror mapping. These tests target MMIO edge cases; CPU fetch wait-state integration remains partial and full commercial-game compatibility is not claimed. The suite additionally checks exact HBlank/VBlank IRQ boundaries, VBlank-triggered DMA completion and IRQ delivery, and Direct Sound FIFO capacity/underflow latch behavior.

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

The machine facade now forwards `gamepak_wait_control()` and `gamepak_access_cycles(address, sequential)` so frontends can inspect WAITCNT timing without taking ownership of the bus. CPU instruction fetches, supported scalar data accesses, and supported ARM/Thumb multiple-transfer bursts and ARM register-offset scalar transfers with immediate and register-specified shifts add modeled excess WAITCNT latency to instruction-cycle totals; DMA stalls remain outside this partial integration. The memory bus exposes `gamepak_access_cycles(address, sequential)` and `gamepak_access_cycles_for_width(address, sequential, width_bytes)` to decode WAITCNT durations for ROM0/ROM1/ROM2 and SRAM. Width-aware estimates account for the ROM's 16-bit bus, SRAM's 8-bit bus, and reject unsupported widths or accesses that cross a timing-region boundary. These helpers also feed partial CPU instruction-fetch, scalar data-access, and supported multiple-transfer timing, including ARM halfword/signed transfers with immediate or unshifted-register offsets; DMA stalls are not yet integrated. Regression tests now cover WAITCNT field changes across WS0/WS1/WS2, sequential and non-sequential ROM sequences, and the machine facade's timing snapshot. Additional coverage exercises every non-sequential WAITCNT encoding for ROM0/ROM1/ROM2 and SRAM, verifies independent sequential flags and reserved-bit masking, and checks SRAM-width timing through the machine facade. These tests validate the timing-query model, including bank-boundary restarts, empty/unsupported sequence inputs, and read-only machine-facade queries; CPU instruction fetches, selected scalar data accesses (including ARM halfword/signed transfers with immediate or unshifted-register offsets), and supported ARM/Thumb multiple-transfer bursts apply excess WAITCNT latency; ARM single-data-transfer register-specified shifts are modeled, while DMA stalls remain unmodeled.

The exported `MachineSnapshot` provides a detached, immutable debugger view of CPU registers, estimated execution counters, WAITCNT configuration, video phase and time-to-boundary estimates, VCOUNT comparison state, IME enable state, interrupt enable/request masks, pending IRQ source, KEYCNT configuration and match state, timer counters, reload values, prescaler/cascade configuration, IRQ state and overflow estimates, DMA masked control registers, enable/start/repeat/width/IRQ configuration, source and destination address-control modes, programmed source/destination addresses, raw count registers and effective transfer counts, audio FIFO levels/samples, and keypad pressed/KEYCNT condition state. Capturing it does not advance emulation.

The WAITCNT timing inspection helpers are regression-tested for valid transfer widths, unmapped addresses, accesses that cross ROM wait-state regions, zero-length sequences, and their non-mutating contract: querying timing must not advance CPU counters or video phase. CPU instruction fetches and selected scalar/multiple-transfer data accesses apply partial excess WAITCNT latency; DMA stalls remain unmodeled. The machine facade also has regression tests for invalid video-position targets, interrupt-flag acknowledgement that preserves unrelated requests, and invalid audio FIFO indices. DMA diagnostics are also checked against out-of-range channel indices, and an invalid audio-refill wait is verified not to execute instructions or advance machine timing. Timer diagnostics are covered for out-of-range indices, invalid timer-overflow waits must leave execution and timing unchanged, and the keypad IRQ waiter is checked against its step budget when the configured key condition is not met. Game Pak timing tests also check the inclusive start/end of each ROM/SRAM timing window, reject addresses outside the Game Pak range, and reject transfers that cross a window boundary. Additional runner regressions verify zero cycle budgets, absolute instruction/cycle targets, step-limit timeouts, selected-versus-unselected interrupt masks, and the requirement that a next-interrupt wait observe a fresh request. CPU timing tests cover signed versus unsigned multiply termination boundaries, while DMA diagnostics verify the distinct zero-count expansion and count-register masks for DMA0–DMA3.

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
    let _snapshot = machine.diagnostic_snapshot(); // detached CPU/peripheral/cartridge diagnostics
    let _save_type = machine.cartridge_save_type();
    let _save_size = machine.save_data_size_bytes();
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

The machine facade also exposes `run_until_instruction_count(target, max_steps)` for waiting on an absolute successful-instruction counter and `run_until_cycle_count(target, max_steps)` for an absolute estimated-cycle target. Both return the number of steps consumed or `None` on timeout; cycle targets can be overshot by the final instruction. `run_for_cycles(budget)` for approximate cycle-budget
execution and `run_for_instructions(count)` for a fixed instruction batch (returning estimated cycles consumed), `run_until_pc(address, max_steps)` for a bounded PC breakpoint and
`run_until_pc_revisit(address, max_steps)` for waiting until an address is revisited, `run_until_audio_fifo_refill(fifo, max_steps)` to run until Direct Sound FIFO A (0)
or B (1) reaches its 16-byte refill threshold, `run_until_next_video_position(line, cycle, max_steps)` for the next fresh raster-position occurrence, `run_until_video_position(line, cycle, max_steps)` for bounded raster-position synchronization, `run_until_next_frame(max_steps)` for bounded synchronization with the next frame wrap, `run_until_scanline(line, max_steps)` for bounded scanline synchronization,
`run_until_vblank(max_steps)` for VBlank synchronization, `run_until_hblank(max_steps)`
for HBlank synchronization, `run_until_vcount_match(max_steps)` for the programmed DISPSTAT compare event,
and `run_until_interrupt(max_steps)` for observing enabled pending IRQs. `run_until_keypad_interrupt(max_steps)` specifically waits for the keypad IRQ source (IF bit 12), while `keypad_irq_requested()` reports whether the KEYCNT button condition is currently satisfied independently of IE/IME. `video_cycles_until_hblank()` and `video_cycles_until_vblank()` expose cycle distances to the next blanking events, including wraparound to the next frame. `VideoFrame::crop(x, y, width, height)` extracts a checked rectangle, while `to_luma8()` exports one grayscale byte per pixel. The masked variants `run_until_interrupt_mask(mask, max_steps)` and `run_until_next_interrupt_mask(mask, max_steps)` wait for selected IF/IE sources without stopping for unrelated IRQs; a zero mask never matches. Instruction-level
cycle budgets may overshoot by the last instruction, and these helpers stop at their
event or limit; IRQ exception entry is not implemented yet. The initial program counter is 0x08000000. The facade is a
development API, not a promise of complete commercial-game compatibility or accurate
frame pacing.

Timer inspection now includes the masked TM control register, selected prescaler divisor, live prescaler phase, overflow-IRQ enable, and cascade-mode bit through `timer_control()`, `timer_prescaler_divisor()`, `timer_prescaler_phase()`, `timer_irq_enabled()`, and `timer_cascade_enabled()`. Timer 0's reserved cascade bit is masked on both halfword and byte writes; timers 1–3 retain cascade selection. `gamepak_wait_control()` exposes the current WAITCNT configuration for frontends and timing diagnostics. These accessors inspect configuration only; CPU instruction fetches and selected scalar/multiple-transfer data accesses use WAITCNT to add modeled excess wait-state latency. DMA stalls and full bus contention remain outside this partial timing model. Game Pak bus-duration estimates align unaligned ROM halfword/word accesses to the physical ARM7TDMI bus address; SRAM retains its byte-lane behavior.

DMA inspection is available through `dma_control(channel)`, `dma_source_address(channel)`, `dma_destination_address(channel)`, `dma_count_register(channel)`, `dma_transfer_count(channel)`, `dma_enabled(channel)`, `dma_start_timing(channel)`, `dma_destination_control(channel)`, `dma_source_control(channel)`, `dma_repeat(channel)`, `dma_word_transfer(channel)`, and `dma_irq_enabled(channel)` on both `MemoryBus` and `GbaMachine`. Address/count accessors report programmed register values (with hardware-reserved count bits masked); `dma_transfer_count(channel)` separately expands a zero count to 0x4000 halfwords/words on DMA0–2 or 0x10000 on DMA3. These helpers expose configuration for debugging and do not imply that every DMA timing mode or transfer path is cycle-accurate. WAITCNT regression tests also cover byte-write preservation, the reserved high bit, and live timing reconfiguration. Machine snapshot tests verify repeated polling is read-only and that a WAITCNT update appears in the next snapshot without advancing CPU or video counters. CPU condition-code coverage verifies that a failed ARM condition skips a store's memory side effect. ARM7TDMI transfer regressions also cover odd-address LDRH rotation, odd-address LDRSH sign-extension behavior, byte-wide timing for odd-address Thumb LDSH on SRAM, SWP/SWPB memory exchange and four-cycle estimates, plus halfword load/store cycle estimates. Game Pak WAITCNT timing helpers cover ROM0/ROM1/ROM2 and SRAM wait-state decoding, access widths, sequential bursts, and restarting non-sequential timing when a burst crosses ROM timing banks; the width-aware helpers are used for instruction-fetch, selected scalar load/store (including ARM halfword/signed transfers with immediate or unshifted-register offsets), supported multiple-transfer timing, and ARM register-offset scalar transfers with immediate or register-specified shifts; DMA stalls remain to be integrated. Keypad regressions cover active-low KEYINPUT, KEYCNT byte writes, OR/AND selection, and edge-triggered keypad IRQ latching. Machine-facade coverage also verifies that acknowledging a keypad IRQ while a key remains held does not relatch it, while releasing and pressing the key again creates a fresh IRQ edge. Direct Sound FIFO tests check the 32-byte capacity, discard-on-full behavior, refill low-watermark reporting, and machine-facade status forwarding without advancing emulation time. Stereo-mix regressions cover independent left/right routing, both FIFOs' half/full volume controls, arithmetic attenuation of negative odd samples, the unclipped signed sum range (-256..254), and master-sound silence. Additional Direct Sound tests verify that disabling master sound clears controls, queues, and DAC latches, while a timer overflow on an empty FIFO preserves the last DAC sample until master reset. Keypad tests cover both OR-mode matching (any selected key) and AND-mode matching (all selected keys), plus edge-triggered IRQ latching. Independent FIFO reset coverage also verifies that FIFO A/B resets clear only the selected queue and its held sample, with reset command bits reading back as zero. Frame-streaming coverage verifies that a zero-frame request returns immediately without invoking the callback or advancing machine state. Regression coverage also verifies that repeated VBlank DMA advances the source address, reloads the destination, preserves the programmed count, and that these register changes are reflected in successive immutable machine snapshots. A dedicated HBlank DMA regression now checks the same increment/reload behavior at consecutive visible scanline boundaries, verifies that the channel remains enabled in repeat mode, and confirms its completion IRQ is latched. Additional machine-snapshot tests verify that all four DMA channels remain independently observable and that reprogramming a channel is reflected in the next snapshot, including changes to IRQ and transfer-width flags.

`GbaMachine::diagnostic_snapshot()` returns a single immutable `MachineSnapshot` combining the CPU snapshot, instruction/cycle counters, video position and blanking flags, IE/IF masks, pending source priority, IRQ readiness, timer overflow estimates, DMA configuration plus source/destination/count diagnostics, Direct Sound FIFO queue lengths, last samples, and byte distances to each FIFO's 16-byte refill threshold. It also includes IME state, keypad KEYCNT configuration and current match state, cartridge ROM size, detected save-device type and image size, and selected header diagnostics so frontends can build status panels from one snapshot. It is intended for debugger/front-end polling and does not advance emulation. Machine-level regression coverage also verifies that one snapshot consistently combines timer, DMA, video, and IRQ state, and that `reset_cpu()` clears CPU execution counters/registers while preserving the advanced timer/video phase, queued Direct Sound bytes, and configured DMA transfer state. The machine facade also forwards diagnostic snapshots for interrupt IE/IF/IME state (`interrupt_enable_mask()`, `interrupt_request_flags()`, `pending_interrupts()`, `interrupt_master_enabled()`, and `interrupt_pending()`). `pending_interrupt_source()` returns the lowest-numbered pending source as a bit index (0–13), providing the GBA's fixed priority order even while IME is disabled. `request_interrupt(mask)` and `acknowledge_interrupts(mask)` let integrations latch or clear IF sources without crafting MMIO writes; both are masked to the 14 implemented GBA interrupt sources. `interrupt_pending()` additionally requires IME and at least one IE/IF match, but it only reports IRQ readiness and does not perform CPU exception entry. It also exposes live video status (`video_scanline()`, `video_in_vblank()`, `video_in_hblank()`, and `video_vcount_match()`), Game Pak access-duration estimates (`gamepak_access_cycles()`, `gamepak_access_cycles_for_width()`, and `gamepak_sequence_cycles()`). Sequence estimates use the requested access type for the first transfer and sequential timing for later transfers, reject unsupported widths and mapped-region crossings, and return zero cycles for an empty sequence, and Direct Sound FIFO queue/sample state (`audio_fifo_len()` and `audio_fifo_last_sample()`). These are inspection APIs; instruction-fetch and selected scalar/multiple-transfer data-access timing use WAITCNT partially, while DMA stalls and IRQ exception entry remain unimplemented.

`GbaMachine::run_until_timer_overflow(timer, max_steps)` can synchronize a bounded run to an enabled non-cascade timer's estimated overflow. It returns `None` for unsupported timer modes or when the instruction limit expires, and may overshoot by the last instruction. Timer inspection is available through `timer_counter(index)`, `timer_reload_value(index)`, and `timer_enabled(index)`. `timer_cycles_until_overflow(index)` estimates the remaining CPU cycles for enabled non-cascade timers and accounts for the active prescaler phase; it returns `None` when the timer is disabled or uses cascade mode. Direct Sound FIFO users can call `audio_fifo_bytes_until_refill(fifo)` on either `MemoryBus` or `GbaMachine` to check how many bytes remain before the 16-byte refill threshold.

## Project status

The main priorities are improving hardware accuracy and expanding integration between the CPU, memory bus, DMA, video, interrupts, and save devices. Timing is still approximate: instruction fetches, selected scalar data accesses, supported multiple-transfer bursts, and ARM single-data-transfer register offsets with immediate or register-specified shifts apply partial WAITCNT latency, including ARM halfword/signed transfers with immediate or unshifted-register offsets and Thumb LDSH odd-address byte timing; DMA stalls, complete exception handling, full audio output, and comprehensive game compatibility are not claimed.

Please report reproducible issues with the Rust version, operating system, command run, expected behavior, and actual behavior. Do not upload commercial ROMs or copyrighted game assets to issues.

## Contributing

Bug fixes and focused hardware-accuracy improvements are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Keep changes scoped, document observable behavior, and add regression coverage for fixes.

## License

This project is distributed under the MIT License; see [LICENSE](LICENSE). Copyright remains with each respective rights holder for their own contributions.
