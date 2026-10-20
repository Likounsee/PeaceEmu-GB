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

    /// Executes instructions until at least `cycle_budget` estimated cycles have elapsed.
    ///
    /// Since instructions are indivisible at this API level, the returned cycle
    /// count can exceed the budget by the final instruction's estimated duration.
    /// A zero budget performs no work. Peripheral timing advances through
    /// `step` for every executed instruction.
    pub fn run_for_cycles(&mut self, cycle_budget: u64) -> Result<u64, CpuError> {
        let mut elapsed = 0_u64;
        while elapsed < cycle_budget {
            elapsed = elapsed.saturating_add(u64::from(self.step()?));
        }
        Ok(elapsed)
    }

    /// Executes until the PPU reaches a requested scanline or the instruction limit.
    ///
    /// The target must be in the GBA scanline range 0..228. Returns `None` for
    /// an invalid target; otherwise returns the number of executed instructions.
    /// If the target is the current scanline, returns zero. When the target is
    /// behind the current scanline, the next occurrence after wraparound is used.
    pub fn run_until_scanline(
        &mut self,
        target_scanline: u16,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if target_scanline >= 228 {
            return Ok(None);
        }
        if self.bus.video_scanline() == target_scanline {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.video_scanline() == target_scanline {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Executes up to the requested number of instructions, returning estimated total cycles.
    ///
    /// If an unsupported instruction is encountered, execution stops with that
    /// error; counters retain the progress made before the error.
    pub fn run_steps(&mut self, count: usize) -> Result<u64, CpuError> {
        let mut cycles = 0_u64;
        for _ in 0..count {
            cycles = cycles.saturating_add(u64::from(self.step()?));
        }
        Ok(cycles)
    }

    /// Returns the enabled interrupt sources currently requested by peripherals.
    ///
    /// This reports the IE/IF intersection even when IME is disabled.
    pub fn pending_interrupts(&self) -> u16 {
        self.bus.pending_interrupts()
    }

    /// Returns whether an enabled interrupt is ready for CPU service (IME, IE, and IF).
    ///
    /// This is a status query only; IRQ exception entry is not implemented yet.
    pub fn interrupt_pending(&self) -> bool {
        self.bus.interrupt_pending()
    }

    /// Executes up to `max_steps` instructions, stopping when a CPU-visible IRQ is pending.
    ///
    /// Returns the number of instructions executed. A return value equal to
    /// `max_steps` does not by itself mean an interrupt occurred; check
    /// [`interrupt_pending`](Self::interrupt_pending). If an unsupported
    /// instruction is encountered, the error is returned and prior progress remains.
    pub fn run_until_interrupt(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        if self.bus.interrupt_pending() {
            return Ok(0);
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.interrupt_pending() {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes up to `max_steps` instructions, stopping when the PPU enters VBlank.
    ///
    /// Returns the number of instructions executed. If VBlank is already active,
    /// returns zero; if the limit is reached first, inspect
    /// [`bus`](Self::bus) with [`MemoryBus::video_in_vblank`](crate::MemoryBus::video_in_vblank).
    /// CPU timing remains approximate, so this is a synchronization helper rather
    /// than a guarantee of hardware-accurate frame pacing.
    pub fn run_until_vblank(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        if self.bus.video_in_vblank() {
            return Ok(0);
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.video_in_vblank() {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes up to `max_steps` instructions, stopping when the PPU enters HBlank.
    ///
    /// Returns the number of instructions executed. If HBlank is already active,
    /// returns zero; if the limit is reached first, inspect
    /// `bus().video_in_hblank()`. Timing is instruction-granular and approximate,
    /// so a transition can be observed only after the instruction that crosses it.
    pub fn run_until_hblank(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        if self.bus.video_in_hblank() {
            return Ok(0);
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.video_in_hblank() {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes up to `max_steps` instructions until DISPSTAT's VCOUNT-match flag is set.
    ///
    /// Unlike `run_until_scanline`, this follows the compare value programmed in
    /// DISPSTAT rather than a caller-supplied scanline. Returns zero if the match
    /// is already active, or `max_steps` if the event was not reached in time.
    pub fn run_until_vcount_match(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        if self.bus.video_vcount_match() {
            return Ok(0);
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.video_vcount_match() {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Sets the active-low keypad input through a pressed-key bitmask.
    ///
    /// Bits 0..9 correspond to A, B, Select, Start, Right, Left, Up, Down, R, L.
    pub fn set_keys_pressed(&mut self, pressed: u16) {
        self.bus.set_keys_pressed(pressed);
    }

    /// Copies the active cartridge's persistent save image, if its type is known.
    ///
    /// The returned bytes are suitable for writing to a frontend-managed save file.
    pub fn save_data(&self) -> Option<Vec<u8>> {
        match self.bus.cartridge_save_type()? {
            crate::SaveType::Sram => Some(self.bus.sram_data().to_vec()),
            crate::SaveType::Flash64K | crate::SaveType::Flash128K => {
                self.bus.flash_data().map(<[u8]>::to_vec)
            }
            crate::SaveType::Eeprom => self.bus.eeprom_data().map(<[u8]>::to_vec),
        }
    }

    /// Loads a complete save image for this cartridge.
    ///
    /// Returns false if the ROM has no recognized save type. A size error leaves
    /// the existing save image unchanged.
    pub fn load_save_data(&mut self, data: &[u8]) -> Result<bool, crate::SramSizeError> {
        match self.bus.cartridge_save_type() {
            Some(crate::SaveType::Sram) => {
                self.bus.load_sram_data(data)?;
                Ok(true)
            }
            Some(crate::SaveType::Flash64K | crate::SaveType::Flash128K) => {
                self.bus.load_flash_data(data)
            }
            Some(crate::SaveType::Eeprom) => self.bus.load_eeprom_data(data),
            None => Ok(false),
        }
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
    fn batch_execution_matches_machine_diagnostic_counters() {
        let mut machine = GbaMachine::new(cartridge());
        let cycles = machine.run_steps(8).unwrap();
        assert!(cycles >= 8);
        assert_eq!(machine.instructions_executed(), 8);
        assert_eq!(machine.cycles_elapsed(), cycles);
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT + 32));
    }

    #[test]
    fn vblank_runner_respects_step_limit_without_claiming_a_frame() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_vblank(2).unwrap(), 2);
        assert!(!machine.bus().video_in_vblank());
        assert_eq!(machine.instructions_executed(), 2);
    }

    #[test]
    fn vcount_match_runner_uses_dispstat_compare_flag() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0004, 0); // Compare against scanline zero.
        assert!(machine.bus().video_vcount_match());
        let before = machine.instructions_executed();
        assert_eq!(machine.run_until_vcount_match(4).unwrap(), 0);
        assert_eq!(machine.instructions_executed(), before);

        machine.bus_mut().advance_cycles(1232); // Scanline one no longer matches zero.
        assert!(!machine.bus().video_vcount_match());
        assert_eq!(machine.run_until_vcount_match(2).unwrap(), 2);
        assert!(!machine.bus().video_vcount_match());
    }

    #[test]
    fn hblank_runner_respects_existing_state_and_step_limit() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_hblank(2).unwrap(), 2);
        assert!(!machine.bus().video_in_hblank());

        machine.bus_mut().advance_cycles(1000);
        assert!(machine.bus().video_in_hblank());
        let before = machine.instructions_executed();
        assert_eq!(machine.run_until_hblank(2).unwrap(), 0);
        assert_eq!(machine.instructions_executed(), before);
    }

    #[test]
    fn cycle_budget_runs_no_work_for_zero_and_reaches_positive_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_for_cycles(0).unwrap(), 0);
        assert_eq!(machine.instructions_executed(), 0);
        let elapsed = machine.run_for_cycles(7).unwrap();
        assert!(elapsed >= 7);
        assert_eq!(machine.cycles_elapsed(), elapsed);
    }

    #[test]
    fn scanline_runner_handles_current_invalid_and_bounded_targets() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_scanline(0, 10).unwrap(), Some(0));
        assert_eq!(machine.run_until_scanline(228, 10).unwrap(), None);
        machine.bus_mut().advance_cycles(1232);
        assert_eq!(machine.bus().video_scanline(), 1);
        assert_eq!(machine.run_until_scanline(1, 10).unwrap(), Some(0));
        assert_eq!(machine.run_until_scanline(2, 1).unwrap(), None);
        assert_eq!(machine.bus().video_scanline(), 1);
    }

    #[test]
    fn machine_exposes_interrupt_status_and_can_run_until_an_irq_is_pending() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0100, 0xFFFF);
        machine.bus_mut().write16(0x0400_0102, 0x00C0);
        machine.bus_mut().write16(0x0400_0200, 1 << 3);
        machine.bus_mut().write16(0x0400_0208, 1);

        assert_eq!(machine.pending_interrupts(), 0);
        assert!(!machine.interrupt_pending());
        let executed = machine.run_until_interrupt(8).unwrap();
        assert_eq!(executed, 1);
        assert_eq!(machine.pending_interrupts(), 1 << 3);
        assert!(machine.interrupt_pending());
        assert_eq!(machine.run_until_interrupt(8).unwrap(), 0);
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
    fn step_advances_cpu_and_diagnostic_counters_together() {
        let mut machine = GbaMachine::new(cartridge());
        let cycles = machine.step().unwrap();
        assert!(cycles > 0);
        assert_eq!(machine.instructions_executed(), 1);
        assert_eq!(machine.cycles_elapsed(), u64::from(cycles));
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT + 4));
    }

    #[test]
    fn machine_save_api_round_trips_sram_and_rejects_bad_sizes_atomically() {
        let mut rom = vec![0; GBA_HEADER_SIZE + 16];
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"TGME");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        rom[GBA_HEADER_SIZE..GBA_HEADER_SIZE + 7].copy_from_slice(b"SRAM_V1");
        let mut machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());

        assert_eq!(machine.save_data().unwrap().len(), crate::memory::SRAM_SIZE);
        assert!(machine
            .load_save_data(&vec![0x5A; crate::memory::SRAM_SIZE])
            .unwrap());
        assert!(machine
            .save_data()
            .unwrap()
            .iter()
            .all(|byte| *byte == 0x5A));

        let before = machine.save_data().unwrap();
        assert!(machine.load_save_data(&[0xFF; 4]).is_err());
        assert_eq!(machine.save_data().unwrap(), before);
    }

    #[test]
    fn machine_save_api_round_trips_flash_and_eeprom_images() {
        for (marker, save_type, size) in [
            (b"FLASH_V".as_slice(), crate::SaveType::Flash64K, 64 * 1024),
            (
                b"FLASH1M_V".as_slice(),
                crate::SaveType::Flash128K,
                128 * 1024,
            ),
            (b"EEPROM_V".as_slice(), crate::SaveType::Eeprom, 512),
        ] {
            let mut rom = vec![0; GBA_HEADER_SIZE + marker.len()];
            rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
            rom[0xAC..0xB0].copy_from_slice(b"TGME");
            rom[0xB0..0xB2].copy_from_slice(b"01");
            rom[0xBD] = rom[0xA0..=0xBC]
                .iter()
                .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
                .wrapping_neg();
            rom[GBA_HEADER_SIZE..].copy_from_slice(marker);
            let mut machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());
            assert_eq!(machine.bus().cartridge_save_type(), Some(save_type));
            let image = vec![0xA5; size];
            assert!(machine.load_save_data(&image).unwrap());
            assert_eq!(machine.save_data().unwrap(), image);
            assert!(machine.load_save_data(&[0x11; 7]).is_err());
            assert_eq!(machine.save_data().unwrap(), image);
        }
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
