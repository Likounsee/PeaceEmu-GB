//! High-level machine facade that wires the CPU, memory bus, input, and video together.

use crate::{render_bitmap_frame, Cpu, CpuError, GbaButton, GbaCartridge, MemoryBus, VideoFrame};

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

    /// Executes until the PPU reaches the next occurrence of a scanline.
    ///
    /// Unlike [`run_until_scanline`](Self::run_until_scanline), this waits for the
    /// next occurrence even when `target_scanline` is the current line. Returns
    /// `None` for an invalid line or when the step limit is reached.
    pub fn run_until_next_scanline(
        &mut self,
        target_scanline: u16,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if target_scanline >= 228 {
            return Ok(None);
        }
        let mut left_target = self.bus.video_scanline() != target_scanline;
        for executed in 0..max_steps {
            self.step()?;
            let scanline = self.bus.video_scanline();
            if scanline != target_scanline {
                left_target = true;
            } else if left_target {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }
    /// Executes until the next fresh VCOUNT-match assertion.
    ///
    /// If the comparison currently matches, the runner waits until it stops
    /// matching and then matches again. Returns the number of executed
    /// instructions, or `max_steps` when no fresh match occurs within the limit.
    pub fn run_until_next_vcount_match(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        let mut saw_match_end = !self.bus.video_vcount_match();
        for executed in 0..max_steps {
            self.step()?;
            let matches = self.bus.video_vcount_match();
            if !matches {
                saw_match_end = true;
            } else if saw_match_end {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes until a fresh enabled interrupt becomes pending.
    ///
    /// If an interrupt is already pending, this waits for the pending state to
    /// clear before accepting a later assertion. It does not enter an IRQ
    /// exception; it only observes the IE/IF/IME status exposed by the machine.
    pub fn run_until_next_interrupt(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        let mut saw_clear = !self.interrupt_pending();
        for executed in 0..max_steps {
            self.step()?;
            let pending = self.interrupt_pending();
            if !pending {
                saw_clear = true;
            } else if saw_clear {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }
    /// Executes until the PPU enters the next VBlank period, even if one is active now.
    ///
    /// Unlike [`run_until_vblank`](Self::run_until_vblank), this always waits for a
    /// fresh entry: if VBlank is already active, it first waits for VBlank to end.
    /// Returns the number of instructions executed, or `max_steps` if the next
    /// entry was not reached. CPU timing remains instruction-granular.
    pub fn run_until_next_vblank(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        let mut saw_vblank_end = !self.bus.video_in_vblank();
        for executed in 0..max_steps {
            self.step()?;
            let in_vblank = self.bus.video_in_vblank();
            if !in_vblank {
                saw_vblank_end = true;
            } else if saw_vblank_end {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes until the PPU enters the next HBlank period, even if one is active now.
    ///
    /// Unlike [`run_until_hblank`](Self::run_until_hblank), this waits for a fresh
    /// entry into HBlank. Returns the number of instructions executed, or
    /// `max_steps` if the next entry was not reached.
    pub fn run_until_next_hblank(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        let mut saw_hblank_end = !self.bus.video_in_hblank();
        for executed in 0..max_steps {
            self.step()?;
            let in_hblank = self.bus.video_in_hblank();
            if !in_hblank {
                saw_hblank_end = true;
            } else if saw_hblank_end {
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

    /// Returns the active-high mask of physically pressed keypad buttons.
    pub fn keys_pressed(&self) -> u16 {
        self.bus.keys_pressed()
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

    /// Updates one physical keypad button while preserving the other button states.
    ///
    /// This is a convenience wrapper around the memory bus input API. Keypad
    /// interrupt requests are latched when the configured KEYCNT condition becomes true.
    pub fn set_button_pressed(&mut self, button: GbaButton, pressed: bool) {
        self.bus.set_button_pressed(button, pressed);
    }

    /// Returns whether a physical keypad button is currently pressed.
    pub fn is_button_pressed(&self, button: GbaButton) -> bool {
        self.bus.is_button_pressed(button)
    }

    /// Returns frontend-relevant state for Direct Sound FIFO A (0) or B (1).
    pub fn audio_fifo_status(&self, fifo: usize) -> Option<crate::AudioFifoStatus> {
        self.bus.audio_fifo_status(fifo)
    }

    /// Returns the currently mixed Direct Sound stereo sample as raw signed integer levels.
    ///
    /// The mix reflects SOUNDCNT_H volume and left/right routing. It is a sample
    /// snapshot, not a resampled audio stream; callers should sample it at their
    /// chosen output rate and perform any required filtering or scaling.
    pub fn direct_sound_stereo_sample(&self) -> (i16, i16) {
        self.bus.direct_sound_stereo_sample()
    }

    /// Runs until the next fresh VBlank entry and returns the rendered frame.
    ///
    /// If VBlank is already active, this waits for it to end before waiting for
    /// the next entry. Returns `None` if the instruction budget is exhausted
    /// first or the current display mode cannot be rendered. CPU errors are
    /// propagated; instructions successfully executed before an error remain
    /// reflected in the machine state and counters.
    pub fn run_frame(&mut self, max_steps: usize) -> Result<Option<VideoFrame>, CpuError> {
        let mut saw_vblank_end = !self.bus.video_in_vblank();
        for _ in 0..max_steps {
            self.step()?;
            let in_vblank = self.bus.video_in_vblank();
            if !in_vblank {
                saw_vblank_end = true;
            } else if saw_vblank_end {
                return Ok(self.render_frame());
            }
        }
        Ok(None)
    }

    /// Captures a sequence of frames, giving each frame its own instruction budget.
    ///
    /// Returns an empty vector immediately when `count` is zero. Returns
    /// `None` if any frame misses its budget or the display mode cannot be
    /// rendered; CPU errors are propagated. Machine progress is not rolled back
    /// if a later frame cannot be captured.
    pub fn run_frames(
        &mut self,
        count: usize,
        max_steps_per_frame: usize,
    ) -> Result<Option<Vec<VideoFrame>>, CpuError> {
        // Do not reserve the caller-provided count up front: an untrusted or
        // accidental enormous count must not trigger an immediate allocation
        // panic when the first frame would fail its budget anyway.
        let mut frames = Vec::new();
        let completed = self.for_each_frame(count, max_steps_per_frame, |_, frame| {
            frames.push(frame);
            true
        })?;
        Ok((completed == Some(count)).then_some(frames))
    }

    /// Captures frames one at a time without retaining the whole sequence.
    ///
    /// The callback receives each completed frame and its zero-based index. Return
    /// true to continue or false to stop after that frame. Returns the number of
    /// frames delivered, or None if the instruction budget prevents the next
    /// frame from completing. A zero count returns Some(0) without invoking the
    /// callback. This is useful for streaming frames to a frontend or encoder.
    pub fn for_each_frame<F>(
        &mut self,
        count: usize,
        max_steps_per_frame: usize,
        mut on_frame: F,
    ) -> Result<Option<usize>, CpuError>
    where
        F: FnMut(usize, VideoFrame) -> bool,
    {
        for index in 0..count {
            let Some(frame) = self.run_frame(max_steps_per_frame)? else {
                return Ok(None);
            };
            if !on_frame(index, frame) {
                return Ok(Some(index + 1));
            }
        }
        Ok(Some(count))
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
    fn next_vcount_match_runner_waits_for_a_fresh_match() {
        let mut machine = GbaMachine::new(cartridge());
        // The comparator initially matches line zero. Advance to line one,
        // then verify the runner waits for the next occurrence of line zero
        // instead of treating the stale match as a new event.
        machine.bus_mut().write16(0x0400_0004, 0);
        // Use zero-filled EWRAM as a safe instruction stream; the tiny test
        // cartridge intentionally has no executable bytes beyond its header.
        machine.cpu.set_register(15, 0x0200_0000);
        assert!(machine.bus().video_vcount_match());
        machine.bus_mut().advance_cycles(1232);
        assert!(!machine.bus().video_vcount_match());

        let before = machine.instructions_executed();
        let executed = machine.run_until_next_vcount_match(280_000).unwrap();
        assert!(executed > 2, "runner returned for a stale VCOUNT match");
        assert_eq!(machine.instructions_executed(), before + executed as u64);
        assert_eq!(machine.bus().video_scanline(), 0);
        assert!(machine.bus().video_vcount_match());
    }

    #[test]
    fn next_interrupt_runner_does_not_claim_an_event_without_pending_irq() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_next_interrupt(2).unwrap(), 2);
        assert!(!machine.interrupt_pending());
        assert_eq!(machine.instructions_executed(), 2);
    }
    #[test]
    fn next_scanline_runner_does_not_return_for_the_current_line() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_next_scanline(0, 2).unwrap(), None);
        assert_eq!(machine.bus().video_scanline(), 0);
        assert_eq!(machine.run_until_next_scanline(228, 2).unwrap(), None);
        assert_eq!(machine.run_until_next_scanline(1, 2).unwrap(), None);
    }
    #[test]
    fn next_vblank_runner_waits_for_a_fresh_transition() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_next_vblank(2).unwrap(), 2);
        assert!(!machine.bus().video_in_vblank());

        // Being inside VBlank is not enough: the runner waits for it to end
        // and enter again rather than returning immediately.
        machine.bus_mut().advance_cycles(160 * 1232);
        assert!(machine.bus().video_in_vblank());
        let before = machine.instructions_executed();
        assert_eq!(machine.run_until_next_vblank(2).unwrap(), 2);
        assert_eq!(machine.instructions_executed(), before + 2);
        assert!(machine.bus().video_in_vblank());
    }

    #[test]
    fn next_hblank_runner_waits_for_a_fresh_transition() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().advance_cycles(1000);
        assert!(machine.bus().video_in_hblank());
        let before = machine.instructions_executed();
        assert_eq!(machine.run_until_next_hblank(2).unwrap(), 2);
        assert_eq!(machine.instructions_executed(), before + 2);
        assert!(machine.bus().video_in_hblank());
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
    fn run_frames_with_zero_count_does_not_advance_the_machine() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_frames(0, 0).unwrap().unwrap().len(), 0);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
    }

    #[test]
    fn run_frames_does_not_preallocate_an_unbounded_requested_count() {
        let mut machine = GbaMachine::new(cartridge());
        assert!(machine.run_frames(usize::MAX, 0).unwrap().is_none());
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
    }

    #[test]
    fn run_frame_respects_a_zero_instruction_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert!(machine.run_frame(0).unwrap().is_none());
        assert_eq!(machine.instructions_executed(), 0);
    }

    #[test]
    fn run_frame_returns_a_rendered_frame_at_the_next_vblank_entry() {
        let mut machine = GbaMachine::new(cartridge());
        // Position the PPU one cycle before VBlank without spending thousands
        // of CPU instructions to reach the boundary.
        machine.bus_mut().advance_cycles(160 * 1232 - 1);
        assert!(!machine.bus().video_in_vblank());

        let frame = machine.run_frame(4).unwrap().unwrap();

        assert!(machine.bus().video_in_vblank());
        assert_eq!(frame.width, crate::SCREEN_WIDTH);
        assert_eq!(frame.height, crate::SCREEN_HEIGHT);
        assert!(frame.is_well_formed());
    }

    #[test]
    fn streaming_frame_capture_stops_without_buffering_remaining_frames() {
        let mut machine = GbaMachine::new(cartridge());
        // Reach the next VBlank boundary with a single instruction remaining.
        machine.bus_mut().advance_cycles(160 * 1232 - 1);
        let mut received = Vec::new();
        let delivered = machine
            .for_each_frame(usize::MAX, 4, |index, frame| {
                received.push((index, frame.width, frame.height));
                false
            })
            .unwrap();

        assert_eq!(delivered, Some(1));
        assert_eq!(
            received,
            vec![(0, crate::SCREEN_WIDTH, crate::SCREEN_HEIGHT)]
        );
    }

    #[test]
    fn streaming_frame_capture_zero_count_does_not_call_callback() {
        let mut machine = GbaMachine::new(cartridge());
        let mut called = false;
        assert_eq!(
            machine
                .for_each_frame(0, 0, |_, _| {
                    called = true;
                    true
                })
                .unwrap(),
            Some(0)
        );
        assert!(!called);
        assert_eq!(machine.instructions_executed(), 0);
    }

    #[test]
    fn machine_exposes_cycle_counter_and_bulk_key_state() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.keys_pressed(), 0);

        machine.set_keys_pressed(GbaButton::A.mask() | GbaButton::Down.mask() | (1 << 15));
        assert_eq!(
            machine.keys_pressed(),
            GbaButton::A.mask() | GbaButton::Down.mask()
        );
        assert_eq!(
            machine.bus().read16(0x0400_0130) & 0x03FF,
            !(GbaButton::A.mask() | GbaButton::Down.mask()) & 0x03FF
        );

        machine.run_steps(1).unwrap();
        assert!(machine.cycles_elapsed() > 0);
    }

    #[test]
    fn machine_button_api_preserves_independent_button_states() {
        let mut machine = GbaMachine::new(cartridge());
        machine.set_button_pressed(GbaButton::A, true);
        machine.set_button_pressed(GbaButton::Left, true);
        assert!(machine.is_button_pressed(GbaButton::A));
        assert!(machine.is_button_pressed(GbaButton::Left));
        machine.set_button_pressed(GbaButton::A, false);
        assert!(!machine.is_button_pressed(GbaButton::A));
        assert!(machine.is_button_pressed(GbaButton::Left));
        assert_eq!(
            machine.bus().read16(0x0400_0130) & 0x03FF,
            (!(GbaButton::Left.mask())) & 0x03FF
        );
    }

    #[test]
    fn machine_exposes_the_current_direct_sound_stereo_mix() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0084, 0x0080);
        machine.bus_mut().write16(0x0400_0082, (1 << 8) | (1 << 9));
        assert_eq!(machine.direct_sound_stereo_sample(), (0, 0));
        machine.bus_mut().write32(0x0400_00A0, 0x0000_4040);
        machine.bus_mut().write16(0x0400_0100, 0xFFFF);
        machine.bus_mut().write16(0x0400_0102, 0x0080);
        machine.bus_mut().advance_cycles(1);
        assert_eq!(machine.direct_sound_stereo_sample(), (64, 64));
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
