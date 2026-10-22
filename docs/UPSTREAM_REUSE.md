# Upstream reuse strategy

PeaceEmu-GBA should reuse proven emulator work where it saves time, but it must keep the existing PeaceEmu-facing API, its own test suite, and project-specific behavior. The goal is **selective adaptation**, not a wholesale replacement or a blind copy.

## Candidate evaluated: RustBoyAdvance-NG

- Repository: https://github.com/michelhe/rustboyadvance-ng
- Language: Rust
- License reported by GitHub: MIT
- Why it is the first code-level reference: it has separate ARM7TDMI and GBA-core crates, plus implementations for cartridge backups, DMA, timers, interrupts, GPU layers/rendering, sound, and BIOS helpers.
- Reuse approach: inspect one subsystem at a time, identify a concrete gap in PeaceEmu, then port/adapt the smallest useful implementation behind PeaceEmu's current interfaces. Preserve attribution and the upstream MIT notice for any code actually derived from it. Do not copy an entire crate or import its frontend/workspace wholesale.

## Other reference projects

| Project | License / stack | Intended use |
| --- | --- | --- |
| [Iron Boy Advance](https://github.com/areklaw2/iron-boy-advance) | GPL-3.0-or-later, Rust | Hardware behavior and test-coverage reference. Do not transplant its code into this MIT project unless the licensing strategy is deliberately changed or compatible permission is obtained. |
| [NanoBoyAdvance](https://github.com/nba-emu/NanoBoyAdvance) | GPL-3.0, C++ | Cycle-accuracy reference and differential behavior checks; not a direct code source for the current MIT core without a licensing decision. |
| [GBATEK](https://problemkaputt.de/gbatek.htm) | Technical documentation | Hardware register and timing reference; documentation is not code to copy. |
| [SingleStepTests/ARM7TDMI](https://github.com/SingleStepTests/ARM7TDMI) and [jsmolka/gba-tests](https://github.com/jsmolka/gba-tests) | External test suites | Use legally distributable tests/fixtures as validation inputs; do not add copyrighted commercial ROMs. |

## Adaptation rules

1. Keep the public `GbaMachine`, `MemoryBus`, cartridge/save, and frame APIs stable unless a deliberate API change is needed.
2. Prefer importing or adapting small, separable components (for example BIOS software-interrupt helpers or hardware edge-case logic) over replacing the existing CPU, bus, and renderer all at once.
3. Every adapted code unit must retain required copyright/license notices and be recorded below with its source path, upstream revision, local path, and modifications.
4. Keep project-specific regression tests. Upstream passing tests are evidence, not a substitute for PeaceEmu's own tests.
5. Never claim a subsystem is complete merely because it was borrowed from a more mature emulator.

## Adapted code ledger

No upstream implementation code has been copied into PeaceEmu-GBA yet. The first candidate has been identified; actual code reuse will be recorded here as each small component is reviewed and adapted.

| Upstream revision | Upstream path | PeaceEmu path | Adaptation / notice |
| --- | --- | --- | --- |
| — | — | — | No imported code yet. |
