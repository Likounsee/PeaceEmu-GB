//! High-level machine facade that wires the CPU, memory bus, input, and video together.

use crate::{render_bitmap_frame, Cpu, CpuError, GbaCartridge, MemoryBus, VideoFrame};

/// Initial entry point for a GBA cartridge in the Game Pak ROM window.
pub const CARTRIDGE_ENTRY_POINT: u32 = 0x0800_0000;

/// A runnable, partially implemented Game Boy Advance machine.
///
/// This facade centralizes CPU stepping and approximate peripheral timing. It
/// does not provide BIOS boot, audio output, interrupts/exception entry, or
/// cycle-accurate hardware behavior.
#[derive(Clone, Debug)]
pub struct GbaMachine {
    cpu: Cpu,
    bus: MemoryBus,
    instructions_executed: u64,
    cycles_elapsed: u64,
}

impl GbaMachine {
    /// Creates a machine around a validated cartridge and starts at its ROM entry point.
    pub fn new(cartridge: GbaCartridge) -> Self {
        let mut cpu = Cpu::new();
        cpu.reset(CARTRIDGE_ENTRY_POINT);
        Self {
            cpu,
            bus: MemoryBus::new(cartridge),
            instructions_executed: 0,
            cycles_elapsed: 0,
        }
    }

    /// Executes one instruction and advances modeled peripherals by estimated cycles.
    ///
    /// On an unsupported instruction, the CPU returns an error and this call
    /// does not increment the successful-instruction counter.
    pub fn step(&mut self) -> Result<u32, CpuError> {
        let (_, cycles) = self.cpu.step_with_cycles(&mut self.bus)?;
        self.instructions_executed = self.instructions_executed.saturating_add(1);
        self.cycles_elapsed = self.cycles_elapsed.saturating_add(u64::from(cycles));
        Ok(cycles)
    }

    /// Returns the current CPU state for debugging and inspection.
    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Returns read-only access to the current memory bus.
    pub fn bus(&self) -> &MemoryBus {
        &self.bus
    }

    /// Returns mutable access to the memory bus for input, save data, and test setup.
    pub fn bus_mut(&mut self) -> &mut MemoryBus {
        &mut self.bus
    }

    /// Renders a frame from the current video-memory and register snapshot.
    ///
    /// Returns no frame for unsupported display modes unless forced blank is active.
    pub fn render_frame(&self) -> Option<VideoFrame> {
        render_bitmap_frame(&self.bus)
    }

    /// Returns the number of successfully executed instructions since construction.
    pub fn instructions_executed(&self) -> u64 {
        self.instructions_executed
    }

    /// Returns estimated CPU cycles elapsed since construction.
    ///
    /// This counter is diagnostic only; cycle estimates are not hardware exact.
    pub fn cycles_elapsed(&self) -> u64 {
        self.cycles_elapsed
    }

    /// Resets the CPU to the cartridge entry point and clears diagnostic counters.
    ///
    /// RAM and peripheral state are deliberately preserved; this is a CPU reset,
    /// not a full power-cycle reset.
    pub fn reset_cpu(&mut self) {
        self.cpu.reset(CARTRIDGE_ENTRY_POINT);
        self.instructions_executed = 0;
        self.cycles_elapsed = 0;
    }

    /// Splits the machine into its CPU and memory bus for advanced integrations.
    pub fn into_parts(self) -> (Cpu, MemoryBus) {
        (self.cpu, self.bus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::GBA_HEADER_SIZE;

    fn cartridge() -> GbaCartridge {
        let mut rom = vec![0; GBA_HEADER_SIZE];
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"TGME");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        GbaCartridge::parse(&rom).unwrap()
    }

    #[test]
    fn machine_starts_at_rom_and_exposes_diagnostic_state() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.bus().cartridge_save_type(), None);
    }

    #[test]
    fn machine_exposes_input_and_renders_a_frame() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().set_keys_pressed(1);
        assert_eq!(machine.bus().read16(0x0400_0130) & 1, 0);
        let frame = machine.render_frame().unwrap();
        assert_eq!(frame.width, crate::SCREEN_WIDTH);
        assert_eq!(frame.height, crate::SCREEN_HEIGHT);
        assert!(frame.is_well_formed());
    }

    #[test]
    fn cpu_reset_preserves_bus_state_and_clears_counters() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write32(0x0200_0000, 0x1234_5678);
        machine.reset_cpu();
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.bus().read32(0x0200_0000), 0x1234_5678);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
    }
}
