# Upstream reuse strategy

PeaceEmu-GBA should reuse proven emulator work where it saves time, while keeping the existing PeaceEmu-facing API, project-specific behavior, and regression tests. The goal is **selective adaptation**, not a wholesale replacement or a blind copy.

## First code-level reference: RustBoyAdvance-NG

- Repository: https://github.com/michelhe/rustboyadvance-ng
- Language/license: Rust, MIT (verified from its repository metadata and LICENSE).
- Useful areas: ARM7TDMI, cartridge backups (SRAM/Flash/EEPROM), DMA, timers, interrupts, GPU rendering, sound, BIOS helpers.
- Compatibility note: its current `master` uses Rust edition 2024 and declares Rust 1.88, while PeaceEmu currently supports Rust 1.78. Do not add the entire upstream workspace as a dependency blindly; review older revisions or port small, isolated behavior into PeaceEmu's existing interfaces.
- Initial adapted behavior: Flash 128-KiB identification now reports the Macronix-compatible ID bytes `C2 09` (low byte first), consistent with the 128-KiB device ID represented by the upstream Flash implementation. PeaceEmu keeps its own `FlashDevice`, public API, state machine, and regression tests.

## Other reference projects

| Project | License / stack | Intended use |
| --- | --- | --- |
| [Iron Boy Advance](https://github.com/areklaw2/iron-boy-advance) | GPL-3.0-or-later, Rust | Hardware behavior and test-coverage reference. Do not transplant its code into this MIT project without a deliberate licensing decision or compatible permission. |
| [NanoBoyAdvance](https://github.com/nba-emu/NanoBoyAdvance) | GPL-3.0, C++ | Cycle-accuracy reference and differential behavior checks; not a direct code source for the current MIT core without a licensing decision. |
| [GBATEK](https://problemkaputt.de/gbatek.htm) | Technical documentation | Hardware register and timing reference. |
| [SingleStepTests/ARM7TDMI](https://github.com/SingleStepTests/ARM7TDMI) and [jsmolka/gba-tests](https://github.com/jsmolka/gba-tests) | External test suites | Validation inputs when their distribution terms permit; do not add copyrighted commercial ROMs. |

## Adaptation rules

1. Keep `GbaMachine`, `MemoryBus`, cartridge/save, and frame APIs stable unless a deliberate API change is needed.
2. Inspect a concrete gap first, then adapt the smallest useful implementation instead of replacing CPU, bus, and renderer wholesale.
3. Preserve required copyright/license notices for any code actually derived from an upstream implementation.
4. Keep PeaceEmu regression tests; upstream passing tests are evidence, not a substitute.
5. Never claim a subsystem is complete merely because a behavior was borrowed from a mature emulator.

## Adapted code ledger

| Upstream | Upstream area | PeaceEmu area | Adaptation |
| --- | --- | --- | --- |
| [RustBoyAdvance-NG](https://github.com/michelhe/rustboyadvance-ng) (MIT; revision reviewed: current `master`) | `core/src/cartridge/backup/flash.rs` | `GameBoy/src/save.rs` | Adapted the 128-KiB Flash identification behavior and added a PeaceEmu regression test. This is a small behavior adaptation, not a copied module. |
