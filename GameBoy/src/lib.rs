#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Game Boy Advance hardware primitives for PeaceEmu.
//!
//! The package keeps the internal development codename `gameboy`, but targets
//! the Game Boy Advance only. Cartridge validation, an instruction-functional
//! ARM7TDMI/Thumb CPU subset, and an initial memory bus are implemented; full
//! peripherals, exceptions, and cycle-accurate timing remain in development.

/// Game Boy Advance cartridge header parsing and validation.
pub mod cartridge;
/// Initial ARM7TDMI CPU implementation.
pub mod cpu;
/// Initial Game Boy Advance memory-bus implementation.
pub mod memory;
/// Cartridge backup-device detection from ROM signatures.
pub mod save;

pub use cartridge::{CartridgeError, GbaCartridge};
pub use cpu::{ConditionFlags, Cpu, CpuError, StepOutcome};
pub use memory::{MemoryBus, SramSizeError};
pub use save::{EepromDevice, FlashDevice, SaveType};

/// High-level machine facade connecting the CPU, bus, and video renderer.
pub mod machine;
/// Initial bitmap-mode renderer for the Game Boy Advance display.
pub mod video;

pub use machine::{GbaMachine, CARTRIDGE_ENTRY_POINT};
pub use video::{render_bitmap_frame, VideoFrame, SCREEN_HEIGHT, SCREEN_WIDTH};
