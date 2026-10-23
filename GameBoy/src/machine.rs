//! High-level machine facade that wires the CPU, memory bus, input, and video together.

use crate::{render_bitmap_frame, Cpu, CpuError, GbaButton, GbaCartridge, MemoryBus, VideoFrame};

/// Initial entry point for a GBA cartridge in the Game Pak ROM window.
pub const CARTRIDGE_ENTRY_POINT: u32 = 0x0800_0000;

/// Immutable debugger snapshot of CPU, execution counters, video timing, and IRQ state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineSnapshot {
    /// Snapshot of modeled ARM7TDMI registers and flags.
    pub cpu: crate::CpuSnapshot,
    /// Number of successfully executed instructions since construction or CPU reset.
    pub instructions_executed: u64,
    /// Approximate cycles elapsed since construction or CPU reset.
    pub cycles_elapsed: u64,
    /// Current WAITCNT register value used to estimate Game Pak access durations.
    pub gamepak_wait_control: u16,
    /// Size of the loaded cartridge ROM image in bytes.
    pub cartridge_rom_size_bytes: usize,
    /// Cartridge title copied into the detached diagnostic snapshot.
    pub cartridge_title: String,
    /// Four-byte cartridge game code copied into the snapshot.
    pub cartridge_game_code: String,
    /// Two-byte cartridge maker code copied into the snapshot.
    pub cartridge_maker_code: String,
    /// Validated complement checksum byte stored at header offset 0xBD.
    pub cartridge_header_checksum: u8,
    /// Backup-device type inferred from the loaded cartridge ROM, if recognized.
    pub cartridge_save_type: Option<crate::SaveType>,
    /// Currently allocated save-image size in bytes, if a save image is available.
    pub cartridge_save_size_bytes: Option<usize>,
    /// Fixed byte from cartridge header offset 0xB2 (normally 0x96 in retail ROMs).
    pub cartridge_header_fixed_value: u8,
    /// Software revision byte from cartridge header offset 0xBC.
    pub cartridge_software_version: u8,
    /// Current video scanline (0–227).
    pub video_scanline: u16,
    /// Cycles elapsed within the current scanline.
    pub video_cycles_into_scanline: u16,
    /// Whether the PPU is currently in VBlank.
    pub video_in_vblank: bool,
    /// Whether the PPU is currently in HBlank.
    pub video_in_hblank: bool,
    /// Whether DISPSTAT's VCOUNT comparison currently matches.
    pub video_vcount_match: bool,
    /// Estimated cycles until the next HBlank boundary.
    pub video_cycles_until_hblank: u32,
    /// Estimated cycles until the next VBlank entry.
    pub video_cycles_until_vblank: u32,
    /// Master interrupt enable (IME) bit.
    pub interrupt_master_enabled: bool,
    /// Interrupt enable (IE) mask.
    pub interrupt_enable: u16,
    /// Latched interrupt request (IF) flags.
    pub interrupt_requests: u16,
    /// Enabled pending interrupt sources (IE & IF), independent of IME.
    pub pending_interrupts: u16,
    /// Lowest-numbered pending source index, if any.
    pub pending_interrupt_source: Option<u8>,
    /// Whether IME, IE, and IF jointly indicate an IRQ ready for service.
    pub interrupt_pending: bool,
    /// Estimated cycles until overflow for timers 0 through 3; `None` means disabled or invalid.
    pub timer_cycles_until_overflow: [Option<u64>; 4],
    /// Current counter values for timers 0 through 3.
    pub timer_counters: [Option<u16>; 4],
    /// Enabled state for timers 0 through 3.
    pub timer_enabled: [Option<bool>; 4],
    /// Whether timers 0 through 3 request an IRQ on overflow.
    pub timer_irq_enabled: [Option<bool>; 4],
    /// Configured reload values for timers 0 through 3.
    pub timer_reload_values: [Option<u16>; 4],
    /// Current prescaler phase for timers 0 through 3.
    pub timer_prescaler_phases: [Option<u16>; 4],
    /// Selected prescaler divisors for timers 0 through 3.
    pub timer_prescaler_divisors: [Option<u16>; 4],
    /// Cascade-mode state for timers 0 through 3.
    pub timer_cascade_enabled: [Option<bool>; 4],
    /// Masked control register values for DMA channels 0 through 3.
    pub dma_controls: [Option<u16>; 4],
    /// Enabled state of DMA channels 0 through 3.
    pub dma_enabled: [Option<bool>; 4],
    /// Source address control modes for DMA channels 0 through 3.
    pub dma_source_controls: [Option<u8>; 4],
    /// Destination address control modes for DMA channels 0 through 3.
    pub dma_destination_controls: [Option<u8>; 4],
    /// Start timing selectors for DMA channels 0 through 3.
    pub dma_start_timing: [Option<u8>; 4],
    /// Repeat configuration for DMA channels 0 through 3.
    pub dma_repeat: [Option<bool>; 4],
    /// Word-transfer configuration for DMA channels 0 through 3.
    pub dma_word_transfer: [Option<bool>; 4],
    /// Completion-IRQ configuration for DMA channels 0 through 3.
    pub dma_irq_enabled: [Option<bool>; 4],
    /// Programmed source addresses for DMA channels 0 through 3.
    pub dma_source_addresses: [Option<u32>; 4],
    /// Programmed destination addresses for DMA channels 0 through 3.
    pub dma_destination_addresses: [Option<u32>; 4],
    /// Raw programmed transfer counts for DMA channels 0 through 3.
    pub dma_count_registers: [Option<u16>; 4],
    /// Effective transfer element counts, including channel-specific zero-count semantics.
    pub dma_transfer_counts: [Option<u32>; 4],
    /// Queued byte counts for Direct Sound FIFOs A and B.
    pub audio_fifo_lengths: [Option<usize>; 2],
    /// Last signed sample values for Direct Sound FIFOs A and B.
    pub audio_fifo_last_samples: [Option<i8>; 2],
    /// Bytes remaining until Direct Sound FIFOs A and B reach the 16-byte refill threshold.
    pub audio_fifo_bytes_until_refill: [Option<usize>; 2],
    /// KEYCNT selection and IRQ mode bits.
    pub keypad_control: u16,
    /// Active-high pressed-button mask (bits 0–9 map to A, B, Select, Start, directions, R, L).
    pub keypad_pressed: u16,
    /// Whether the configured KEYCNT selected-button condition is currently satisfied.
    pub keypad_irq_requested: bool,
}

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

/// Opaque, lossless snapshot of the modeled machine state for save states and rollback.
///
/// Unlike MachineSnapshot, this stores the CPU, complete memory bus (including RAM, I/O
/// registers, timers, video position, DMA and audio FIFO queues), and execution counters.
/// It does not serialize host-side frontend state or claim cycle accuracy beyond the emulator.
#[derive(Clone, Debug)]
pub struct GbaSaveState {
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

    /// Captures a complete, restorable snapshot of the emulated machine.
    ///
    /// This is separate from diagnostic_snapshot, which is intended for inspection
    /// and does not contain mutable hardware state.
    pub fn save_state(&self) -> GbaSaveState {
        GbaSaveState {
            cpu: self.cpu.clone(),
            bus: self.bus.clone(),
            instructions_executed: self.instructions_executed,
            cycles_elapsed: self.cycles_elapsed,
        }
    }

    /// Restores a state previously captured by save_state.
    ///
    /// CPU, cartridge/bus state, peripherals, and counters are restored together. The state
    /// object is borrowed and remains reusable, so the same checkpoint can be loaded repeatedly.
    pub fn load_state(&mut self, state: &GbaSaveState) {
        self.cpu = state.cpu.clone();
        self.bus = state.bus.clone();
        self.instructions_executed = state.instructions_executed;
        self.cycles_elapsed = state.cycles_elapsed;
    }

    /// Executes one instruction and advances modeled peripherals by estimated cycles.
    ///
    /// On an unsupported instruction, the CPU returns an error and this call
    /// does not increment the successful-instruction counter.
    pub fn step(&mut self) -> Result<u32, CpuError> {
        let (outcome, cycles) = self.cpu.step_with_cycles(&mut self.bus)?;
        if !matches!(outcome, crate::StepOutcome::Halted) {
            self.instructions_executed = self.instructions_executed.saturating_add(1);
        }
        self.cycles_elapsed = self.cycles_elapsed.saturating_add(u64::from(cycles));
        Ok(cycles)
    }

    /// Executes at most the requested number of instructions and returns their estimated cycles.
    ///
    /// A zero instruction budget performs no work. Execution stops immediately
    /// on the first CPU error; counters and peripherals reflect only the
    /// instructions that completed successfully.
    pub fn run_for_instructions(&mut self, instruction_budget: usize) -> Result<u64, CpuError> {
        let mut elapsed = 0_u64;
        for _ in 0..instruction_budget {
            elapsed = elapsed.saturating_add(u64::from(self.step()?));
        }
        Ok(elapsed)
    }

    /// Executes instructions until at least `cycle_budget` estimated cycles have elapsed.
    ///
    /// Since instructions are indivisible at this API level, the returned cycle
    /// count can exceed the budget by the final instruction's estimated duration.
    /// A zero budget performs no work. Peripheral timing advances through
    /// `step` for every executed instruction.
    /// Executes whole instructions until at least `cycle_budget` cycles have elapsed.
    ///
    /// Since instructions cannot be split, the returned count can exceed the requested budget
    /// by the remaining cycles of the final instruction. A zero budget executes nothing.
    pub fn run_for_cycles(&mut self, cycle_budget: u64) -> Result<u64, CpuError> {
        let mut elapsed = 0_u64;
        while elapsed < cycle_budget {
            elapsed = elapsed.saturating_add(u64::from(self.step()?));
        }
        Ok(elapsed)
    }

    /// Executes until the PPU reaches a scanline and cycle offset, with a step limit.
    ///
    /// `target_cycle` must be in 0..1232. The runner returns the number of
    /// instructions executed once the PPU is on `target_scanline` at or after
    /// that cycle offset. If the requested position has already passed on the
    /// current scanline, it waits for the next frame's occurrence. Returns
    /// `None` for an invalid position or when the step limit expires.
    pub fn run_until_video_position(
        &mut self,
        target_scanline: u16,
        target_cycle: u16,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if target_scanline >= 228 || target_cycle >= 1232 {
            return Ok(None);
        }
        let current_scanline = self.bus.video_scanline();
        let current_cycle = self.bus.video_cycles_into_scanline();
        if current_scanline == target_scanline && current_cycle == target_cycle {
            return Ok(Some(0));
        }
        let already_passed = current_scanline == target_scanline && current_cycle > target_cycle;
        let mut left_current_occurrence = !already_passed;
        for executed in 0..max_steps {
            self.step()?;
            let scanline = self.bus.video_scanline();
            let cycle = self.bus.video_cycles_into_scanline();
            if scanline != target_scanline {
                left_current_occurrence = true;
            } else if left_current_occurrence && cycle >= target_cycle {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Executes until the next occurrence of a raster position.
    ///
    /// Unlike `run_until_video_position`, this always waits for a fresh
    /// occurrence: if the machine is exactly at the requested position, it
    /// waits for the next frame. If the position is still ahead on the current
    /// scanline, it can be reached during that scanline. Invalid positions and
    /// exhausted step budgets return `None`.
    pub fn run_until_next_video_position(
        &mut self,
        target_scanline: u16,
        target_cycle: u16,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if target_scanline >= 228 || target_cycle >= 1232 {
            return Ok(None);
        }
        let current_scanline = self.bus.video_scanline();
        let current_cycle = self.bus.video_cycles_into_scanline();
        let mut left_current_occurrence =
            current_scanline != target_scanline || current_cycle < target_cycle;
        for executed in 0..max_steps {
            self.step()?;
            let scanline = self.bus.video_scanline();
            let cycle = self.bus.video_cycles_into_scanline();
            if scanline != target_scanline {
                left_current_occurrence = true;
            } else if left_current_occurrence && cycle >= target_cycle {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
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

    /// Executes until the CPU reaches an instruction address or the step limit.
    ///
    /// The target is aligned to the CPU's current instruction state (halfword in
    /// Thumb, word in ARM) on each check. Returns Some(0) when already at the
    /// target, Some(n) after reaching it in n steps, or None if the limit expires
    /// first. CPU errors are propagated and prior progress is retained.
    pub fn run_until_pc(
        &mut self,
        target_address: u32,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        let at_target = |machine: &Self| {
            let alignment_mask = if machine.cpu.is_thumb() { !1 } else { !3 };
            machine.cpu.register(15) == Some(target_address & alignment_mask)
        };
        if at_target(self) {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if at_target(self) {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Executes until the CPU leaves the given instruction address and later
    /// returns to it, or until the instruction limit expires.
    ///
    /// This is useful for waiting for a loop or subroutine address to be
    /// revisited. The target is aligned according to the active ARM/Thumb state.
    /// Returns Some(n) when the address is revisited and None on timeout.
    pub fn run_until_pc_revisit(
        &mut self,
        target_address: u32,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        let at_target = |machine: &Self| {
            let alignment_mask = if machine.cpu.is_thumb() { !1 } else { !3 };
            machine.cpu.register(15) == Some(target_address & alignment_mask)
        };
        let mut left_target = !at_target(self);
        for executed in 0..max_steps {
            self.step()?;
            if !at_target(self) {
                left_target = true;
            } else if left_target {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Executes until the lifetime estimated-cycle counter reaches `target_cycles`, or the step limit.
    ///
    /// Returns the number of instructions executed by this call. The target is an
    /// absolute counter value; because execution is instruction-granular, the final
    /// cycle count may exceed the target. Returns `Some(0)` if the target has already
    /// been reached and `None` if the step limit expires first. CPU errors propagate.
    pub fn run_until_cycle_count(
        &mut self,
        target_cycles: u64,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if self.cycles_elapsed >= target_cycles {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.cycles_elapsed >= target_cycles {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Executes until the lifetime instruction counter reaches `target_count`, or the step limit.
    ///
    /// Returns the number of instructions executed by this call. If the target has
    /// already been reached, returns `Some(0)`; if the step limit expires first,
    /// returns `None`. The target is an absolute machine counter, not a relative
    /// number of additional instructions. CPU errors are propagated and progress
    /// completed before the error is retained.
    pub fn run_until_instruction_count(
        &mut self,
        target_count: u64,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if self.instructions_executed >= target_count {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.instructions_executed >= target_count {
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

    /// Returns the lowest-numbered enabled interrupt source awaiting service.
    ///
    /// The result is a source bit index (0–13), and is available even when IME
    /// is disabled. This reports priority only; it does not enter an IRQ exception.
    pub fn pending_interrupt_source(&self) -> Option<u8> {
        self.bus.pending_interrupt_source()
    }

    /// Returns the masked DMA control register for channel 0–3.
    pub fn dma_control(&self, channel: usize) -> Option<u16> {
        self.bus.dma_control(channel)
    }

    /// Returns the currently programmed source address for DMA channel 0–3.
    pub fn dma_source_address(&self, channel: usize) -> Option<u32> {
        self.bus.dma_source_address(channel)
    }

    /// Returns the currently programmed destination address for DMA channel 0–3.
    pub fn dma_destination_address(&self, channel: usize) -> Option<u32> {
        self.bus.dma_destination_address(channel)
    }

    /// Returns the masked DMA count register as programmed.
    pub fn dma_count_register(&self, channel: usize) -> Option<u16> {
        self.bus.dma_count_register(channel)
    }

    /// Returns the effective DMA transfer length, expanding zero to the channel maximum.
    pub fn dma_transfer_count(&self, channel: usize) -> Option<u32> {
        self.bus.dma_transfer_count(channel)
    }

    /// Returns whether a DMA channel is enabled.
    pub fn dma_enabled(&self, channel: usize) -> Option<bool> {
        self.bus.dma_enabled(channel)
    }

    /// Returns the DMA start timing selector (0 immediate, 1 VBlank, 2 HBlank, 3 special).
    pub fn dma_start_timing(&self, channel: usize) -> Option<u8> {
        self.bus.dma_start_timing(channel)
    }

    /// Returns the destination address control mode for a DMA channel.
    pub fn dma_destination_control(&self, channel: usize) -> Option<u8> {
        self.bus.dma_destination_control(channel)
    }

    /// Returns the source address control mode for a DMA channel.
    pub fn dma_source_control(&self, channel: usize) -> Option<u8> {
        self.bus.dma_source_control(channel)
    }

    /// Returns whether a DMA channel repeats on a non-immediate trigger.
    pub fn dma_repeat(&self, channel: usize) -> Option<bool> {
        self.bus.dma_repeat(channel)
    }

    /// Returns whether a DMA channel uses 32-bit word transfers.
    pub fn dma_word_transfer(&self, channel: usize) -> Option<bool> {
        self.bus.dma_word_transfer(channel)
    }

    /// Returns whether a DMA channel is configured to request an IRQ on completion.
    pub fn dma_irq_enabled(&self, channel: usize) -> Option<bool> {
        self.bus.dma_irq_enabled(channel)
    }

    /// Services one external Game Pak DRQ request through DMA3.
    ///
    /// Returns `true` when DMA3 is enabled in Game Pak DRQ mode and accepts
    /// the request; video timing alone never services this external trigger.
    pub fn request_game_pak_dma(&mut self) -> bool {
        self.bus.request_game_pak_dma()
    }

    /// Returns the interrupt-enable (IE) bitmask, with reserved bits removed.
    pub fn interrupt_enable_mask(&self) -> u16 {
        self.bus.interrupt_enable_mask()
    }

    /// Returns the latched interrupt-request (IF) flags, with reserved bits removed.
    pub fn interrupt_request_flags(&self) -> u16 {
        self.bus.interrupt_request_flags()
    }

    /// Latches the selected interrupt request flags (only implemented GBA sources).
    pub fn request_interrupt(&mut self, sources: u16) {
        self.bus.request_interrupt(sources);
    }

    /// Acknowledges selected interrupt request flags using write-one-to-clear semantics.
    pub fn acknowledge_interrupts(&mut self, sources: u16) {
        self.bus.acknowledge_interrupts(sources);
    }

    /// Returns whether the interrupt master-enable (IME) bit is set.
    pub fn interrupt_master_enabled(&self) -> bool {
        self.bus.interrupt_master_enabled()
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
        self.run_until_interrupt_mask(u16::MAX, max_steps)
    }

    /// Executes until a configured keypad interrupt is CPU-visible or the step limit expires.
    ///
    /// This waits for the keypad IRQ source (IF bit 12) with IE and IME enabled;
    /// it does not synthesize button presses or enter an IRQ exception. Returns
    /// zero if the keypad IRQ is already pending, or the number of instructions
    /// executed before it becomes pending. Returns `max_steps` on timeout.
    pub fn run_until_keypad_interrupt(&mut self, max_steps: usize) -> Result<usize, CpuError> {
        self.run_until_interrupt_mask(1 << 12, max_steps)
    }

    /// Executes up to `max_steps` instructions until a selected enabled IRQ is pending.
    ///
    /// `interrupt_mask` uses the GBA IF bit layout (VBlank bit 0, HBlank bit 1,
    /// VCOUNT bit 2, timers bits 3–6, serial bit 7, DMA bits 8–11, keypad bit 12,
    /// and Game Pak bit 13). A zero mask never matches. Other pending IRQ sources
    /// do not stop this runner unless at least one selected source is also pending.
    /// Returns the number of executed instructions; reaching the limit does not
    /// imply a match. IRQ exception entry is not implemented.
    pub fn run_until_interrupt_mask(
        &mut self,
        interrupt_mask: u16,
        max_steps: usize,
    ) -> Result<usize, CpuError> {
        let selected_pending = |machine: &Self| {
            interrupt_mask != 0
                && machine.bus.interrupt_pending()
                && machine.bus.pending_interrupts() & interrupt_mask != 0
        };
        if selected_pending(self) {
            return Ok(0);
        }
        for executed in 0..max_steps {
            self.step()?;
            if selected_pending(self) {
                return Ok(executed + 1);
            }
        }
        Ok(max_steps)
    }

    /// Executes until the PPU wraps from the end of a frame to scanline zero.
    ///
    /// Unlike waiting for scanline zero, this always waits for the *next* frame
    /// boundary, even if called while already on scanline zero. Returns the number
    /// of instructions executed, or `None` if the step limit expires first.
    /// The instruction that crosses the boundary is counted in the result.
    pub fn run_until_next_frame(&mut self, max_steps: usize) -> Result<Option<usize>, CpuError> {
        let mut previous_scanline = self.bus.video_scanline();
        for executed in 0..max_steps {
            self.step()?;
            let scanline = self.bus.video_scanline();
            if scanline < previous_scanline {
                return Ok(Some(executed + 1));
            }
            previous_scanline = scanline;
        }
        Ok(None)
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
        self.run_until_next_interrupt_mask(u16::MAX, max_steps)
    }

    /// Waits for a fresh assertion of one of the selected enabled IRQ sources.
    ///
    /// If a selected IRQ is already pending, the runner waits for all selected
    /// sources to clear before accepting a later assertion. Unselected IRQs do
    /// not affect this state machine. Returns `max_steps` if no fresh assertion
    /// occurs within the instruction budget. IRQ exception entry is not modeled.
    pub fn run_until_next_interrupt_mask(
        &mut self,
        interrupt_mask: u16,
        max_steps: usize,
    ) -> Result<usize, CpuError> {
        let selected_pending = |machine: &Self| {
            interrupt_mask != 0
                && machine.bus.interrupt_pending()
                && machine.bus.pending_interrupts() & interrupt_mask != 0
        };
        let mut saw_clear = !selected_pending(self);
        for executed in 0..max_steps {
            self.step()?;
            let pending = selected_pending(self);
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

    /// Returns the backup-device type inferred from the loaded cartridge ROM.
    ///
    /// This reports the detected device even before a save image is loaded.
    pub fn cartridge_save_type(&self) -> Option<crate::SaveType> {
        self.bus.cartridge_save_type()
    }

    /// Returns the currently allocated save-image size without copying its data.
    ///
    /// Returns None when the cartridge has no recognized save device or when
    /// that device has no accessible image.
    pub fn save_data_size_bytes(&self) -> Option<usize> {
        match self.cartridge_save_type()? {
            crate::SaveType::Sram => Some(self.bus.sram_data().len()),
            crate::SaveType::Flash64K | crate::SaveType::Flash128K => {
                self.bus.flash_data().map(<[u8]>::len)
            }
            crate::SaveType::Eeprom => self.bus.eeprom_data().map(<[u8]>::len),
        }
    }

    /// Returns the current CPU state for debugging and inspection.
    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Returns a detached snapshot of the modeled CPU state.
    ///
    /// Debuggers can retain the snapshot while execution continues.
    pub fn cpu_snapshot(&self) -> crate::CpuSnapshot {
        self.cpu.snapshot()
    }

    /// Restores modeled CPU registers and flags from a debugger/save-state snapshot.
    ///
    /// Peripheral state and machine execution counters are intentionally left
    /// unchanged; use this for CPU-state editing, not a full machine rewind.
    pub fn restore_cpu_snapshot(&mut self, snapshot: crate::CpuSnapshot) {
        self.cpu.restore_snapshot(snapshot);
    }

    /// Returns the loaded cartridge ROM image size in bytes.
    pub fn cartridge_rom_size_bytes(&self) -> usize {
        self.bus.cartridge_rom_size_bytes()
    }

    /// Returns the title declared in the loaded cartridge header.
    pub fn cartridge_title(&self) -> &str {
        self.bus.cartridge_title()
    }

    /// Returns the four-byte game code declared in the loaded cartridge header.
    pub fn cartridge_game_code(&self) -> &str {
        self.bus.cartridge_game_code()
    }

    /// Returns the two-byte maker code declared in the loaded cartridge header.
    pub fn cartridge_maker_code(&self) -> &str {
        self.bus.cartridge_maker_code()
    }

    /// Returns the cartridge header's fixed byte at offset 0xB2.
    pub fn cartridge_header_fixed_value(&self) -> u8 {
        self.bus.cartridge_header_fixed_value()
    }

    /// Returns the software revision byte at cartridge header offset 0xBC.
    pub fn cartridge_software_version(&self) -> u8 {
        self.bus.cartridge_software_version()
    }

    /// Returns the validated complement checksum stored at header offset 0xBD.
    pub fn cartridge_header_checksum(&self) -> u8 {
        self.bus.cartridge_header_checksum()
    }

    /// Returns read-only access to the current memory bus.
    pub fn bus(&self) -> &MemoryBus {
        &self.bus
    }

    /// Returns mutable access to the memory bus for input, save data, and test setup.
    pub fn bus_mut(&mut self) -> &mut MemoryBus {
        &mut self.bus
    }

    /// Returns the current value of a hardware timer counter (timer 0–3).
    pub fn timer_counter(&self, timer: usize) -> Option<u16> {
        self.bus.timer_counter(timer)
    }

    /// Returns the reload value configured for a hardware timer (timer 0–3).
    pub fn timer_reload_value(&self, timer: usize) -> Option<u16> {
        self.bus.timer_reload_value(timer)
    }

    /// Returns whether a hardware timer (timer 0–3) is enabled.
    pub fn timer_enabled(&self, timer: usize) -> Option<bool> {
        self.bus.timer_enabled(timer)
    }

    /// Returns the timer's masked control register (TM0..TM3), if valid.
    pub fn timer_control(&self, timer: usize) -> Option<u16> {
        self.bus.timer_control(timer)
    }

    /// Returns the timer's selected prescaler divisor (1, 64, 256, or 1024).
    pub fn timer_prescaler_divisor(&self, timer: usize) -> Option<u16> {
        self.bus.timer_prescaler_divisor(timer)
    }

    /// Returns the current prescaler phase for a hardware timer (timer 0–3).
    ///
    /// The phase is the number of CPU cycles accumulated toward the next
    /// prescaled tick. It is not used to clock timers configured for cascade mode.
    pub fn timer_prescaler_phase(&self, timer: usize) -> Option<u16> {
        self.bus.timer_prescaler_phase(timer)
    }

    /// Returns whether the timer is configured to request an IRQ on overflow.
    pub fn timer_irq_enabled(&self, timer: usize) -> Option<bool> {
        self.bus.timer_irq_enabled(timer)
    }

    /// Returns whether the timer is configured for cascade clocking.
    pub fn timer_cascade_enabled(&self, timer: usize) -> Option<bool> {
        self.bus.timer_cascade_enabled(timer)
    }

    /// Returns the configured Game Pak WAITCNT register.
    ///
    /// This is a configuration snapshot. WAITCNT excess latency is partially
    /// integrated for instruction fetches and selected data transfers; DMA
    /// stalls remain outside actual CPU cycle accounting. Supported ARM/Thumb
    /// scalar loads/stores, SWP/SWPB read-modify-write operations, ARM register
    /// offsets with immediate or register-specified shifts, and multiple-transfer
    /// bursts add excess WAITCNT latency. DMA stalls and bus contention are not
    /// yet included.
    pub fn gamepak_wait_control(&self) -> u16 {
        self.bus.gamepak_wait_control()
    }

    /// Estimates CPU cycles until a timer overflows, if its current mode supports it.
    pub fn timer_cycles_until_overflow(&self, timer: usize) -> Option<u64> {
        self.bus.timer_cycles_until_overflow(timer)
    }

    /// Executes until an enabled non-cascade timer is estimated to overflow.
    ///
    /// Returns `Some(0)` if the timer is already due, `Some(n)` after the
    /// estimated overflow duration is covered by `n` instructions, or `None`
    /// for an invalid/unsupported timer mode or when `max_steps` is exhausted.
    /// Since instructions are indivisible, the final step may pass the overflow
    /// point by up to one instruction. This is based on the bus timing model,
    /// not a cycle-exact hardware event callback.
    pub fn run_until_timer_overflow(
        &mut self,
        timer: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        let Some(cycles_to_overflow) = self.bus.timer_cycles_until_overflow(timer) else {
            return Ok(None);
        };
        if cycles_to_overflow == 0 {
            return Ok(Some(0));
        }
        let starting_cycles = self.cycles_elapsed;
        for executed in 0..max_steps {
            self.step()?;
            if self.cycles_elapsed.saturating_sub(starting_cycles) >= cycles_to_overflow {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Returns the number of cycles elapsed in the current scanline.
    /// Returns the current scanline counter (0 through 227).
    pub fn video_scanline(&self) -> u16 {
        self.bus.video_scanline()
    }

    /// Returns whether the PPU is currently in VBlank.
    pub fn video_in_vblank(&self) -> bool {
        self.bus.video_in_vblank()
    }

    /// Returns whether the PPU is currently in HBlank.
    pub fn video_in_hblank(&self) -> bool {
        self.bus.video_in_hblank()
    }

    /// Returns whether DISPSTAT's VCOUNT comparison currently matches.
    pub fn video_vcount_match(&self) -> bool {
        self.bus.video_vcount_match()
    }

    /// Returns the number of cycles elapsed in the current scanline.
    pub fn video_cycles_into_scanline(&self) -> u16 {
        self.bus.video_cycles_into_scanline()
    }

    /// Returns cycles until the next HBlank boundary.
    pub fn video_cycles_until_hblank(&self) -> u32 {
        self.bus.video_cycles_until_hblank()
    }

    /// Returns cycles until the next VBlank entry, based on the current scanline phase.
    pub fn video_cycles_until_vblank(&self) -> u32 {
        self.bus.video_cycles_until_vblank()
    }

    /// Estimates total Game Pak bus cycles for a sequence of same-width accesses.
    ///
    /// This inspection helper does not advance emulation. The first access uses
    /// first_sequential; later accesses are modeled as sequential.
    /// Returns the configured duration for one Game Pak access, if the address is mapped.
    ///
    /// This is a timing query only and does not advance CPU or peripheral state.
    pub fn gamepak_access_cycles(&self, address: u32, sequential: bool) -> Option<u32> {
        self.bus.gamepak_access_cycles(address, sequential)
    }

    /// Estimates Game Pak bus cycles for one 8-, 16-, or 32-bit access.
    ///
    /// This reports bus duration only; it does not advance emulation or apply
    /// wait states to CPU instruction timing.
    pub fn gamepak_access_cycles_for_width(
        &self,
        address: u32,
        sequential: bool,
        width_bytes: u8,
    ) -> Option<u32> {
        self.bus
            .gamepak_access_cycles_for_width(address, sequential, width_bytes)
    }

    /// Estimates total Game Pak bus cycles for a sequence of same-width accesses.
    ///
    /// The first access uses `first_sequential`; subsequent accesses are sequential
    /// only while they remain in the same WAITCNT region. Crossing ROM0/ROM1/ROM2
    /// restarts with a non-sequential access. This query does not advance emulation.
    pub fn gamepak_sequence_cycles(
        &self,
        address: u32,
        first_sequential: bool,
        width_bytes: u8,
        access_count: usize,
    ) -> Option<u64> {
        self.bus
            .gamepak_sequence_cycles(address, first_sequential, width_bytes, access_count)
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

    /// Returns whether KEYCNT's selected-button condition is currently satisfied.
    ///
    /// This reports the keypad condition itself, independently of whether the
    /// keypad IRQ is enabled in IE/IME or has been acknowledged in IF.
    pub fn keypad_irq_requested(&self) -> bool {
        self.bus.keypad_irq_requested()
    }

    /// Returns frontend-relevant state for Direct Sound FIFO A (0) or B (1).
    pub fn audio_fifo_status(&self, fifo: usize) -> Option<crate::AudioFifoStatus> {
        self.bus.audio_fifo_status(fifo)
    }

    /// Returns the number of queued bytes in Direct Sound FIFO A (0) or B (1).
    pub fn audio_fifo_len(&self, fifo: usize) -> Option<usize> {
        self.bus.audio_fifo_len(fifo)
    }

    /// Returns the most recently latched signed PCM sample for FIFO A (0) or B (1).
    pub fn audio_fifo_last_sample(&self, fifo: usize) -> Option<i8> {
        self.bus.audio_fifo_last_sample(fifo)
    }

    /// Executes until a Direct Sound FIFO reaches its refill threshold or the
    /// instruction limit is exhausted.
    ///
    /// FIFO A is index 0 and FIFO B is index 1. The threshold is 16 queued bytes
    /// or fewer. Returns the number of executed instructions, `Some(0)` if
    /// the FIFO is already at the threshold, and `None` for an invalid FIFO
    /// index or when no threshold is reached within `max_steps`. This waits
    /// for the FIFO state; it does not guarantee that DMA is configured to refill it.
    pub fn run_until_audio_fifo_refill(
        &mut self,
        fifo: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        let Some(status) = self.bus.audio_fifo_status(fifo) else {
            return Ok(None);
        };
        if status.refill_threshold_reached {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            let Some(status) = self.bus.audio_fifo_status(fifo) else {
                return Ok(None);
            };
            if status.refill_threshold_reached {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Runs until a Direct Sound FIFO has been refilled above its low-water mark.
    ///
    /// Returns `Some(0)` if the FIFO already contains more than 16 bytes,
    /// `Some(n)` after a step reaches that level, or `None` for an invalid FIFO
    /// index or exhausted budget. This observes queue occupancy; it does not
    /// guarantee that the samples came from DMA rather than software writes.
    pub fn run_until_audio_fifo_refilled(
        &mut self,
        fifo: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        let Some(status) = self.bus.audio_fifo_status(fifo) else {
            return Ok(None);
        };
        if status.queued_bytes > 16 {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            let Some(status) = self.bus.audio_fifo_status(fifo) else {
                return Ok(None);
            };
            if status.queued_bytes > 16 {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Runs until a DMA channel's completion IRQ flag is latched.
    ///
    /// The channel index must be 0 through 3. This checks the DMA IF bit,
    /// independently of IE and IME, because the peripheral can complete while
    /// the CPU's interrupt delivery is masked. Returns `Some(0)` if the flag
    /// is already pending, `Some(n)` after a step latches it, or `None` for
    /// an invalid channel or an exhausted step budget.
    pub fn run_until_dma_interrupt(
        &mut self,
        channel: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if channel >= 4 {
            return Ok(None);
        }
        let source = 1_u16 << (8 + channel);
        if self.bus.interrupt_request_flags() & source != 0 {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.interrupt_request_flags() & source != 0 {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Runs until a DMA channel becomes enabled.
    ///
    /// Returns `Some(0)` if the channel is already enabled, `Some(n)` after
    /// a CPU step enables it, or `None` for an invalid channel or exhausted
    /// instruction budget. This observes the channel's enable bit; it does not
    /// imply that a transfer has completed.
    pub fn run_until_dma_enabled(
        &mut self,
        channel: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if channel >= 4 {
            return Ok(None);
        }
        if self.bus.dma_enabled(channel) == Some(true) {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.dma_enabled(channel) == Some(true) {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Runs until a DMA channel is disabled, usually after a one-shot transfer.
    ///
    /// Returns `Some(0)` if the channel is already disabled, `Some(n)` after
    /// a CPU step observes it disabled, or `None` for an invalid channel or an
    /// exhausted budget. Repeating DMA channels remain enabled after each burst,
    /// so this method is intended for one-shot transfers.
    pub fn run_until_dma_disabled(
        &mut self,
        channel: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if channel >= 4 {
            return Ok(None);
        }
        if self.bus.dma_enabled(channel) == Some(false) {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.dma_enabled(channel) == Some(false) {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Returns how many additional sample bytes a FIFO can queue before
    /// reaching its Direct Sound refill threshold.
    ///
    /// Returns zero if refill is already due and None for an invalid FIFO index.
    pub fn audio_fifo_bytes_until_refill(&self, fifo: usize) -> Option<usize> {
        self.bus.audio_fifo_bytes_until_refill(fifo)
    }

    /// Runs until a Direct Sound FIFO contains at least one queued sample byte.
    ///
    /// Returns `Some(0)` if the FIFO is already non-empty, `Some(n)` if it
    /// becomes non-empty after n instructions, or `None` for an invalid FIFO
    /// index or an exhausted step budget. Peripheral timers advance normally.
    pub fn run_until_audio_fifo_nonempty(
        &mut self,
        fifo: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if self.bus.audio_fifo_len(fifo).is_none() {
            return Ok(None);
        }
        if self.bus.audio_fifo_len(fifo) != Some(0) {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.audio_fifo_len(fifo).is_some_and(|len| len > 0) {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Runs until a Direct Sound FIFO has no queued sample bytes remaining.
    ///
    /// Returns `Some(0)` if the FIFO is already empty, `Some(n)` if it
    /// empties after n instructions, or `None` for an invalid FIFO index or
    /// an exhausted step budget. A FIFO only drains when its selected timer
    /// clocks Direct Sound; this helper does not synthesize timer events.
    pub fn run_until_audio_fifo_empty(
        &mut self,
        fifo: usize,
        max_steps: usize,
    ) -> Result<Option<usize>, CpuError> {
        if self.bus.audio_fifo_len(fifo).is_none() {
            return Ok(None);
        }
        if self.bus.audio_fifo_len(fifo) == Some(0) {
            return Ok(Some(0));
        }
        for executed in 0..max_steps {
            self.step()?;
            if self.bus.audio_fifo_len(fifo) == Some(0) {
                return Ok(Some(executed + 1));
            }
        }
        Ok(None)
    }

    /// Returns the currently mixed Direct Sound stereo sample as raw signed integer levels.
    ///
    /// The mix reflects SOUNDCNT_H volume and left/right routing. The result
    /// is an unclipped signed integer sum (each side can range from -256 to 254
    /// with the currently modeled two FIFOs), not a resampled audio stream.
    /// Callers should sample it at their chosen output rate and perform any
    /// required filtering, scaling, and final output clipping.
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

    /// Captures a consistent, read-only snapshot of CPU, timing, video, and IRQ state.
    ///
    /// This is intended for debuggers, frontends, tests, and telemetry. Timing
    /// counters remain approximate and the snapshot does not advance emulation.
    pub fn diagnostic_snapshot(&self) -> MachineSnapshot {
        MachineSnapshot {
            cpu: self.cpu.snapshot(),
            instructions_executed: self.instructions_executed,
            cycles_elapsed: self.cycles_elapsed,
            gamepak_wait_control: self.bus.gamepak_wait_control(),
            cartridge_rom_size_bytes: self.bus.cartridge_rom_size_bytes(),
            cartridge_title: self.bus.cartridge_title().to_owned(),
            cartridge_game_code: self.bus.cartridge_game_code().to_owned(),
            cartridge_maker_code: self.bus.cartridge_maker_code().to_owned(),
            cartridge_header_checksum: self.bus.cartridge_header_checksum(),
            cartridge_save_type: self.bus.cartridge_save_type(),
            cartridge_save_size_bytes: self.save_data_size_bytes(),
            cartridge_header_fixed_value: self.bus.cartridge_header_fixed_value(),
            cartridge_software_version: self.bus.cartridge_software_version(),
            video_scanline: self.bus.video_scanline(),
            video_cycles_into_scanline: self.bus.video_cycles_into_scanline(),
            video_in_vblank: self.bus.video_in_vblank(),
            video_in_hblank: self.bus.video_in_hblank(),
            video_vcount_match: self.bus.video_vcount_match(),
            video_cycles_until_hblank: self.bus.video_cycles_until_hblank(),
            video_cycles_until_vblank: self.bus.video_cycles_until_vblank(),
            interrupt_master_enabled: self.bus.interrupt_master_enabled(),
            interrupt_enable: self.bus.interrupt_enable_mask(),
            interrupt_requests: self.bus.interrupt_request_flags(),
            pending_interrupts: self.bus.pending_interrupts(),
            pending_interrupt_source: self.bus.pending_interrupt_source(),
            interrupt_pending: self.bus.interrupt_pending(),
            timer_cycles_until_overflow: [
                self.bus.timer_cycles_until_overflow(0),
                self.bus.timer_cycles_until_overflow(1),
                self.bus.timer_cycles_until_overflow(2),
                self.bus.timer_cycles_until_overflow(3),
            ],
            timer_counters: [
                self.bus.timer_counter(0),
                self.bus.timer_counter(1),
                self.bus.timer_counter(2),
                self.bus.timer_counter(3),
            ],
            timer_enabled: [
                self.bus.timer_enabled(0),
                self.bus.timer_enabled(1),
                self.bus.timer_enabled(2),
                self.bus.timer_enabled(3),
            ],
            timer_irq_enabled: [
                self.bus.timer_irq_enabled(0),
                self.bus.timer_irq_enabled(1),
                self.bus.timer_irq_enabled(2),
                self.bus.timer_irq_enabled(3),
            ],
            timer_reload_values: [
                self.bus.timer_reload_value(0),
                self.bus.timer_reload_value(1),
                self.bus.timer_reload_value(2),
                self.bus.timer_reload_value(3),
            ],
            timer_prescaler_phases: [
                self.bus.timer_prescaler_phase(0),
                self.bus.timer_prescaler_phase(1),
                self.bus.timer_prescaler_phase(2),
                self.bus.timer_prescaler_phase(3),
            ],
            timer_prescaler_divisors: [
                self.bus.timer_prescaler_divisor(0),
                self.bus.timer_prescaler_divisor(1),
                self.bus.timer_prescaler_divisor(2),
                self.bus.timer_prescaler_divisor(3),
            ],
            timer_cascade_enabled: [
                self.bus.timer_cascade_enabled(0),
                self.bus.timer_cascade_enabled(1),
                self.bus.timer_cascade_enabled(2),
                self.bus.timer_cascade_enabled(3),
            ],
            dma_controls: [
                self.bus.dma_control(0),
                self.bus.dma_control(1),
                self.bus.dma_control(2),
                self.bus.dma_control(3),
            ],
            dma_enabled: [
                self.bus.dma_enabled(0),
                self.bus.dma_enabled(1),
                self.bus.dma_enabled(2),
                self.bus.dma_enabled(3),
            ],
            dma_source_controls: [
                self.bus.dma_source_control(0),
                self.bus.dma_source_control(1),
                self.bus.dma_source_control(2),
                self.bus.dma_source_control(3),
            ],
            dma_destination_controls: [
                self.bus.dma_destination_control(0),
                self.bus.dma_destination_control(1),
                self.bus.dma_destination_control(2),
                self.bus.dma_destination_control(3),
            ],
            dma_start_timing: [
                self.bus.dma_start_timing(0),
                self.bus.dma_start_timing(1),
                self.bus.dma_start_timing(2),
                self.bus.dma_start_timing(3),
            ],
            dma_repeat: [
                self.bus.dma_repeat(0),
                self.bus.dma_repeat(1),
                self.bus.dma_repeat(2),
                self.bus.dma_repeat(3),
            ],
            dma_word_transfer: [
                self.bus.dma_word_transfer(0),
                self.bus.dma_word_transfer(1),
                self.bus.dma_word_transfer(2),
                self.bus.dma_word_transfer(3),
            ],
            dma_irq_enabled: [
                self.bus.dma_irq_enabled(0),
                self.bus.dma_irq_enabled(1),
                self.bus.dma_irq_enabled(2),
                self.bus.dma_irq_enabled(3),
            ],
            dma_source_addresses: [
                self.bus.dma_source_address(0),
                self.bus.dma_source_address(1),
                self.bus.dma_source_address(2),
                self.bus.dma_source_address(3),
            ],
            dma_destination_addresses: [
                self.bus.dma_destination_address(0),
                self.bus.dma_destination_address(1),
                self.bus.dma_destination_address(2),
                self.bus.dma_destination_address(3),
            ],
            dma_count_registers: [
                self.bus.dma_count_register(0),
                self.bus.dma_count_register(1),
                self.bus.dma_count_register(2),
                self.bus.dma_count_register(3),
            ],
            dma_transfer_counts: [
                self.bus.dma_transfer_count(0),
                self.bus.dma_transfer_count(1),
                self.bus.dma_transfer_count(2),
                self.bus.dma_transfer_count(3),
            ],
            audio_fifo_lengths: [self.bus.audio_fifo_len(0), self.bus.audio_fifo_len(1)],
            audio_fifo_last_samples: [
                self.bus.audio_fifo_last_sample(0),
                self.bus.audio_fifo_last_sample(1),
            ],
            audio_fifo_bytes_until_refill: [
                self.bus.audio_fifo_bytes_until_refill(0),
                self.bus.audio_fifo_bytes_until_refill(1),
            ],
            keypad_control: self.bus.read16(0x0400_0132),
            keypad_pressed: self.bus.keys_pressed(),
            keypad_irq_requested: self.bus.keypad_irq_requested(),
        }
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
    fn audio_fifo_refill_runner_distinguishes_low_water_from_refilled_state() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_audio_fifo_refilled(2, 10).unwrap(), None);
        assert_eq!(machine.run_until_audio_fifo_refilled(0, 0).unwrap(), None);

        // Five FIFO words queue 20 bytes, safely above the 16-byte refill point.
        for word in [
            0x0102_0304,
            0x0506_0708,
            0x090A_0B0C,
            0x0D0E_0F10,
            0x1112_1314,
        ] {
            machine.bus_mut().write32(0x0400_00A0, word);
        }
        assert_eq!(machine.audio_fifo_bytes_until_refill(0), Some(4));
        assert_eq!(
            machine.run_until_audio_fifo_refilled(0, 0).unwrap(),
            Some(0)
        );
        assert_eq!(machine.run_until_audio_fifo_refilled(1, 0).unwrap(), None);
    }

    #[test]
    fn halted_machine_advances_cycles_without_counting_idle_as_instructions() {
        let mut rom = vec![0; GBA_HEADER_SIZE];
        rom[0..4].copy_from_slice(&0xEF00_0002_u32.to_le_bytes()); // SWI Halt.
        rom[0xA0..0xAC].copy_from_slice(b"HALT TEST   ");
        rom[0xAC..0xB0].copy_from_slice(b"HLTE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let mut machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());

        machine.step().unwrap();
        assert_eq!(machine.instructions_executed(), 1);
        let cycles_before_idle = machine.cycles_elapsed();
        assert_eq!(machine.step().unwrap(), 1);
        assert_eq!(machine.instructions_executed(), 1);
        assert_eq!(machine.cycles_elapsed(), cycles_before_idle + 1);
    }

    #[test]
    fn machine_can_restore_cpu_snapshot_without_rewinding_peripherals_or_counters() {
        let mut machine = GbaMachine::new(cartridge());
        let mut snapshot = machine.cpu_snapshot();
        snapshot.registers[0] = 0xDEAD_BEEF;
        snapshot.pc = 0x0800_0123;
        snapshot.thumb = true;
        snapshot.flags = crate::ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: false,
        };
        machine.bus_mut().write16(0x0400_0200, 1 << 3);
        machine.bus_mut().request_interrupt(1 << 3);

        machine.restore_cpu_snapshot(snapshot);

        assert_eq!(machine.cpu().register(0), Some(0xDEAD_BEEF));
        assert_eq!(machine.cpu().register(15), Some(0x0800_0122));
        assert!(machine.cpu().is_thumb());
        assert_eq!(machine.cpu().flags(), snapshot.flags);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.bus().interrupt_enable_mask(), 1 << 3);
        assert_eq!(machine.bus().interrupt_request_flags() & (1 << 3), 1 << 3);
    }

    #[test]
    fn diagnostic_snapshot_separates_pending_irq_from_master_enable() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0200, 1 << 3);
        machine.bus_mut().request_interrupt(1 << 3);

        let disabled = machine.diagnostic_snapshot();
        assert_eq!(disabled.interrupt_enable, 1 << 3);
        assert_eq!(disabled.interrupt_requests & (1 << 3), 1 << 3);
        assert_eq!(disabled.pending_interrupts, 1 << 3);
        assert_eq!(disabled.pending_interrupt_source, Some(3));
        assert!(!disabled.interrupt_master_enabled);
        assert!(!disabled.interrupt_pending);

        machine.bus_mut().write16(0x0400_0208, 1);
        let enabled = machine.diagnostic_snapshot();
        assert!(enabled.interrupt_master_enabled);
        assert!(enabled.interrupt_pending);
        assert_eq!(enabled.pending_interrupt_source, Some(3));
        assert_eq!(enabled.instructions_executed, 0);
        assert_eq!(enabled.cycles_elapsed, 0);
    }

    #[test]
    fn machine_starts_at_rom_and_exposes_diagnostic_state() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.diagnostic_snapshot().gamepak_wait_control, 0);
        assert_eq!(machine.cartridge_rom_size_bytes(), GBA_HEADER_SIZE);
        assert_eq!(machine.cartridge_save_type(), None);
        assert_eq!(machine.save_data_size_bytes(), None);
        assert_eq!(machine.cartridge_title(), "TEST GAME");
        assert_eq!(machine.cartridge_game_code(), "TGME");
        assert_eq!(machine.cartridge_maker_code(), "01");
        assert_eq!(machine.cartridge_header_fixed_value(), 0);
        assert_eq!(machine.cartridge_software_version(), 0);
        assert_eq!(
            machine.cartridge_header_checksum(),
            machine.bus().cartridge_header_checksum()
        );
        assert_eq!(machine.bus().cartridge_save_type(), None);
    }

    #[test]
    fn machine_exposes_nondefault_cartridge_header_metadata() {
        let mut rom = vec![0; GBA_HEADER_SIZE];
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"TGME");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        rom[0xBC] = 7;
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());

        assert_eq!(machine.cartridge_title(), "TEST GAME");
        assert_eq!(machine.cartridge_game_code(), "TGME");
        assert_eq!(machine.cartridge_maker_code(), "01");
        assert_eq!(machine.cartridge_header_fixed_value(), 0x96);
        assert_eq!(machine.cartridge_software_version(), 7);
        let snapshot = machine.diagnostic_snapshot();
        assert_eq!(snapshot.cartridge_rom_size_bytes, GBA_HEADER_SIZE);
        assert_eq!(snapshot.cartridge_title, "TEST GAME");
        assert_eq!(snapshot.cartridge_game_code, "TGME");
        assert_eq!(snapshot.cartridge_maker_code, "01");
        assert_eq!(snapshot.cartridge_header_checksum, rom[0xBD]);
        assert_eq!(snapshot.cartridge_save_type, None);
        assert_eq!(snapshot.cartridge_save_size_bytes, None);
        assert_eq!(snapshot.cartridge_header_fixed_value, 0x96);
        assert_eq!(snapshot.cartridge_software_version, 7);
        assert_eq!(machine.cartridge_header_checksum(), rom[0xBD]);
    }

    #[test]
    fn instruction_batch_runner_matches_repeated_single_steps() {
        let mut batched = GbaMachine::new(cartridge());
        let mut stepped = GbaMachine::new(cartridge());

        assert_eq!(batched.run_for_instructions(0).unwrap(), 0);
        assert_eq!(batched.diagnostic_snapshot(), stepped.diagnostic_snapshot());

        let batched_cycles = batched.run_for_instructions(12).unwrap();
        let mut stepped_cycles = 0_u64;
        for _ in 0..12 {
            stepped_cycles += u64::from(stepped.step().unwrap());
        }

        assert_eq!(batched_cycles, stepped_cycles);
        assert_eq!(batched.diagnostic_snapshot(), stepped.diagnostic_snapshot());
        assert_eq!(batched.instructions_executed(), 12);
        assert_eq!(batched.cycles_elapsed(), batched_cycles);
    }

    #[test]
    fn instruction_batch_runner_stops_cleanly_on_cpu_error() {
        let mut rom = cartridge().rom().to_vec();
        rom[..4].copy_from_slice(&0xF000_0000_u32.to_le_bytes());
        let mut machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());
        let before = machine.diagnostic_snapshot();

        assert!(machine.run_for_instructions(4).is_err());
        assert_eq!(machine.diagnostic_snapshot(), before);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
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
    fn cpu_reset_restarts_instruction_count_runner_without_rebuilding_machine() {
        let mut machine = GbaMachine::new(cartridge());
        machine.run_steps(2).unwrap();
        assert_eq!(machine.instructions_executed(), 2);
        assert!(machine.cycles_elapsed() > 0);

        machine.reset_cpu();
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.run_until_instruction_count(1, 1).unwrap(), Some(1));
        assert_eq!(machine.instructions_executed(), 1);
    }

    #[test]
    fn cycle_count_runner_uses_absolute_counter_and_allows_instruction_overshoot() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_cycle_count(0, 0).unwrap(), Some(0));
        assert_eq!(machine.run_until_cycle_count(100, 2).unwrap(), None);
        assert_eq!(machine.instructions_executed(), 2);

        let target = machine.cycles_elapsed() + 1;
        assert_eq!(machine.run_until_cycle_count(target, 1).unwrap(), Some(1));
        assert!(machine.cycles_elapsed() >= target);
        assert_eq!(machine.run_until_cycle_count(1, 0).unwrap(), Some(0));
    }

    #[test]
    fn instruction_count_runner_uses_absolute_counter_and_honors_limits() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_instruction_count(0, 0).unwrap(), Some(0));
        assert_eq!(machine.run_until_instruction_count(3, 2).unwrap(), None);
        assert_eq!(machine.instructions_executed(), 2);
        assert_eq!(machine.run_until_instruction_count(3, 1).unwrap(), Some(1));
        assert_eq!(machine.instructions_executed(), 3);
        assert_eq!(machine.run_until_instruction_count(2, 0).unwrap(), Some(0));
    }

    #[test]
    fn machine_exposes_waitstate_queries_and_detached_cpu_snapshots() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.gamepak_wait_control(), 0);
        assert_eq!(machine.gamepak_access_cycles(0x0800_0000, false), Some(4));
        assert_eq!(machine.gamepak_access_cycles(0x0800_0000, true), Some(2));
        assert_eq!(machine.gamepak_access_cycles(0x0200_0000, false), None);
        machine.bus_mut().write16(0x0400_0204, 1 << 2);
        assert_eq!(machine.gamepak_wait_control(), 1 << 2);
        assert_eq!(machine.gamepak_access_cycles(0x0800_0000, false), Some(3));
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(5)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 3),
            Some(7)
        );

        // ROM1's configured wait must take effect after crossing from ROM0;
        // timing inspection must not advance CPU or peripheral counters.
        machine.bus_mut().write16(0x0400_0204, 2 << 5);
        let before_instructions = machine.instructions_executed();
        let before_cycles = machine.cycles_elapsed();
        assert_eq!(
            machine.gamepak_sequence_cycles(0x09FF_FFFE, false, 2, 2),
            Some(6)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0200_0000, false, 2, 0),
            Some(0)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 3, 1),
            None
        );
        assert_eq!(machine.instructions_executed(), before_instructions);
        assert_eq!(machine.cycles_elapsed(), before_cycles);

        let snapshot = machine.cpu_snapshot();
        assert_eq!(snapshot.pc, CARTRIDGE_ENTRY_POINT);
        machine.run_steps(1).unwrap();
        assert_eq!(snapshot.pc, CARTRIDGE_ENTRY_POINT);
        assert_eq!(machine.cpu_snapshot().pc, CARTRIDGE_ENTRY_POINT + 4);
    }

    #[test]
    fn program_counter_runners_handle_current_target_limits_and_revisits() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(
            machine.run_until_pc(CARTRIDGE_ENTRY_POINT, 0).unwrap(),
            Some(0)
        );
        assert_eq!(
            machine.run_until_pc(CARTRIDGE_ENTRY_POINT + 8, 1).unwrap(),
            None
        );
        assert_eq!(machine.instructions_executed(), 1);
        assert_eq!(
            machine.run_until_pc(CARTRIDGE_ENTRY_POINT + 8, 2).unwrap(),
            Some(1)
        );
        assert_eq!(
            machine
                .run_until_pc_revisit(CARTRIDGE_ENTRY_POINT + 8, 0)
                .unwrap(),
            None
        );
    }

    #[test]
    fn audio_fifo_refill_runner_handles_ready_invalid_and_step_limit_states() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_audio_fifo_refill(0, 0).unwrap(), Some(0));
        assert_eq!(machine.run_until_audio_fifo_refill(2, 10).unwrap(), None);

        // Populate FIFO A above its refill threshold, then verify a bounded
        // runner does not claim a refill if no timer consumes the queue.
        for _ in 0..5 {
            machine.bus_mut().write32(0x0400_00A0, 0x0102_0304);
        }
        assert_eq!(machine.bus().audio_fifo_status(0).unwrap().queued_bytes, 20);
        machine.cpu.set_register(15, 0x0200_0000);
        assert_eq!(machine.run_until_audio_fifo_refill(0, 2).unwrap(), None);
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
    fn pending_interrupt_mask_is_independent_of_master_enable() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0200, (1 << 0) | (1 << 3));
        machine
            .bus_mut()
            .request_interrupt((1 << 0) | (1 << 2) | (1 << 3));
        assert_eq!(machine.pending_interrupts(), (1 << 0) | (1 << 3));
        assert!(!machine.interrupt_pending());
        machine.bus_mut().write16(0x0400_0208, 1);
        assert!(machine.interrupt_pending());
        machine.bus_mut().acknowledge_interrupts(1 << 0);
        assert_eq!(machine.pending_interrupts(), 1 << 3);
    }

    #[test]
    fn machine_forwards_interrupt_register_diagnostics() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.interrupt_enable_mask(), 0);
        assert_eq!(machine.interrupt_request_flags(), 0);
        assert!(!machine.interrupt_master_enabled());

        machine.bus_mut().write16(0x0400_0200, 0xFFFF);
        machine.bus_mut().request_interrupt(0x3FFF);
        machine.bus_mut().write16(0x0400_0208, 1);
        assert_eq!(machine.interrupt_enable_mask(), 0x3FFF);
        assert_eq!(machine.interrupt_request_flags(), 0x3FFF);
        assert!(machine.interrupt_master_enabled());
    }

    #[test]
    fn machine_forwards_audio_fifo_sample_and_queue_diagnostics() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.audio_fifo_len(0), Some(0));
        assert_eq!(machine.audio_fifo_last_sample(0), Some(0));
        assert_eq!(machine.audio_fifo_len(2), None);
        assert_eq!(machine.audio_fifo_last_sample(2), None);

        machine.bus_mut().write32(0x0400_00A0, 0x0000_4040);
        assert_eq!(machine.audio_fifo_len(0), Some(4));
    }

    #[test]
    fn machine_snapshot_reports_audio_fifo_refill_distance_without_advancing_time() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        assert_eq!(before.audio_fifo_bytes_until_refill, [Some(0), Some(0)]);

        for word in 0..5_u32 {
            machine.bus_mut().write32(0x0400_00A0, word);
        }
        let queued = machine.diagnostic_snapshot();
        assert_eq!(queued.audio_fifo_lengths, [Some(20), Some(0)]);
        assert_eq!(queued.audio_fifo_bytes_until_refill, [Some(4), Some(0)]);
        assert_eq!(machine.diagnostic_snapshot(), queued);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
    }

    #[test]
    fn machine_snapshot_combines_timer_dma_video_and_irq_state() {
        let mut machine = GbaMachine::new(cartridge());

        // Timer 0: enabled, IRQ on, one cycle from overflow.
        machine.bus_mut().write16(0x0400_0100, 0xFFFF);
        machine.bus_mut().write16(0x0400_0102, 0x00C0);
        // DMA0: enabled for VBlank, with a one-halfword transfer configured.
        machine.bus_mut().write32(0x0400_00B0, 0x0200_0000);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_0000);
        machine.bus_mut().write16(0x0400_00B8, 1);
        machine.bus_mut().write16(0x0400_00BA, 0x9000);
        // Make the timer IRQ source observable in the combined snapshot.
        machine.bus_mut().write16(0x0400_0200, 1 << 3);

        let snapshot = machine.diagnostic_snapshot();
        assert_eq!(snapshot.timer_counters[0], Some(0xFFFF));
        assert_eq!(snapshot.timer_enabled[0], Some(true));
        assert_eq!(snapshot.timer_irq_enabled[0], Some(true));
        assert_eq!(snapshot.dma_enabled[0], Some(true));
        assert_eq!(snapshot.dma_start_timing[0], Some(1));
        assert_eq!(snapshot.dma_source_addresses[0], Some(0x0200_0000));
        assert_eq!(snapshot.dma_destination_addresses[0], Some(0x0300_0000));
        assert_eq!(snapshot.dma_transfer_counts[0], Some(1));
        assert_eq!(snapshot.video_scanline, 0);
        assert_eq!(snapshot.video_cycles_into_scanline, 0);
        assert_eq!(snapshot.interrupt_enable, 1 << 3);
        assert_eq!(snapshot.interrupt_requests & (1 << 3), 0);
        assert_eq!(machine.diagnostic_snapshot(), snapshot);
        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
    }

    #[test]
    fn cpu_reset_clears_execution_counters_but_preserves_peripheral_state() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0100, 0x1234);
        machine.bus_mut().write16(0x0400_0102, 0x0080);
        machine.bus_mut().write32(0x0400_00A0, 0x4433_2211);
        machine.bus_mut().write32(0x0400_00B0, 0x0200_0000);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_0000);
        machine.bus_mut().write16(0x0400_00B8, 1);
        machine.bus_mut().write16(0x0400_00BA, 0x9000);
        machine.bus_mut().advance_cycles(17);
        machine.cpu.set_register(0, 0xDEAD_BEEF);
        machine.instructions_executed = 7;
        machine.cycles_elapsed = 29;

        machine.reset_cpu();

        assert_eq!(machine.instructions_executed(), 0);
        assert_eq!(machine.cycles_elapsed(), 0);
        assert_eq!(machine.cpu.register(0), Some(0));
        assert_eq!(machine.cpu.register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.bus().timer_counter(0), Some(0x1245));
        assert_eq!(machine.bus().video_cycles_into_scanline(), 17);
        assert_eq!(machine.audio_fifo_len(0), Some(4));
        assert_eq!(machine.dma_enabled(0), Some(true));
        assert_eq!(machine.dma_source_address(0), Some(0x0200_0000));
        assert_eq!(machine.dma_destination_address(0), Some(0x0300_0000));
        assert_eq!(machine.dma_transfer_count(0), Some(1));
    }

    #[test]
    fn next_interrupt_runner_does_not_claim_an_event_without_pending_irq() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_next_interrupt(2).unwrap(), 2);
        assert!(!machine.interrupt_pending());
        assert_eq!(machine.instructions_executed(), 2);
    }

    #[test]
    fn interrupt_mask_runners_ignore_unselected_sources_and_match_selected_sources() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0200, 1 << 3); // Enable timer 0 IRQ only.
        machine.bus_mut().write16(0x0400_0208, 1); // IME on.
        machine.bus_mut().request_interrupt(1 << 0); // Unselected VBlank request.
        assert!(!machine.interrupt_pending());

        assert_eq!(machine.run_until_interrupt_mask(1 << 3, 2).unwrap(), 2);
        assert_eq!(machine.run_until_next_interrupt_mask(0, 2).unwrap(), 2);
        assert_eq!(machine.instructions_executed(), 4);

        machine.bus_mut().request_interrupt(1 << 3);
        assert!(machine.interrupt_pending());
        assert_eq!(machine.run_until_interrupt_mask(1 << 3, 2).unwrap(), 0);
        assert_eq!(machine.run_until_interrupt_mask(0, 2).unwrap(), 2);
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
    fn video_position_runner_validates_targets_and_waits_for_the_requested_phase() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_video_position(228, 0, 10).unwrap(), None);
        assert_eq!(machine.run_until_video_position(0, 1232, 10).unwrap(), None);
        assert_eq!(machine.run_until_video_position(0, 0, 10).unwrap(), Some(0));

        // Starting before the HBlank boundary on scanline zero should reach
        // the requested phase with a bounded number of CPU steps.
        machine.bus_mut().advance_cycles(950);
        assert_eq!(machine.video_scanline(), 0);
        let steps = machine.run_until_video_position(0, 960, 16).unwrap();
        assert!(steps.is_some_and(|count| count > 0 && count <= 16));
        assert_eq!(machine.video_scanline(), 0);
        assert!(machine.video_cycles_into_scanline() >= 960);

        // If the requested position has passed, it must wait for the next
        // occurrence rather than reporting an immediate success.
        let steps = machine.run_until_video_position(0, 900, 1).unwrap();
        assert_eq!(steps, None);
    }

    #[test]
    fn next_video_position_runner_requires_a_fresh_occurrence() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(
            machine.run_until_next_video_position(228, 0, 10).unwrap(),
            None
        );
        assert_eq!(
            machine.run_until_next_video_position(0, 1232, 10).unwrap(),
            None
        );
        assert_eq!(
            machine.run_until_next_video_position(0, 0, 0).unwrap(),
            None
        );

        // At the exact target, the next occurrence is on the next frame.
        assert_eq!(
            machine.run_until_next_video_position(0, 0, 4).unwrap(),
            None
        );

        // Use a fresh machine so this check is independent of the earlier
        // timeout run. The target phase is still ahead on scanline zero.
        let mut phase_machine = GbaMachine::new(cartridge());
        phase_machine.bus_mut().advance_cycles(950);
        let steps = phase_machine
            .run_until_next_video_position(0, 960, 16)
            .unwrap();
        assert!(steps.is_some_and(|count| count > 0 && count <= 16));
        assert_eq!(phase_machine.video_scanline(), 0);
        assert!(phase_machine.video_cycles_into_scanline() >= 960);
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
    fn machine_reports_pending_interrupt_priority() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0200, (1 << 8) | (1 << 1));
        machine.request_interrupt((1 << 8) | (1 << 1));
        assert_eq!(machine.pending_interrupt_source(), Some(1));
        machine.acknowledge_interrupts(1 << 1);
        assert_eq!(machine.pending_interrupt_source(), Some(8));
        machine.acknowledge_interrupts(1 << 8);
        assert_eq!(machine.pending_interrupt_source(), None);
    }

    #[test]
    fn machine_exposes_interrupt_status_and_can_run_until_an_irq_is_pending() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.pending_interrupts(), 0);
        assert!(!machine.interrupt_pending());

        machine.bus_mut().write16(0x0400_0200, 1 << 3);
        machine.request_interrupt(1 << 3);
        assert_eq!(machine.pending_interrupts(), 1 << 3);
        assert!(!machine.interrupt_pending());

        machine.bus_mut().write16(0x0400_0208, 1);
        assert!(machine.interrupt_pending());
        machine.acknowledge_interrupts(1 << 3);
        assert_eq!(machine.pending_interrupts(), 0);
        assert!(!machine.interrupt_pending());
    }

    #[test]
    fn dma_state_runners_wait_for_hblank_transfer_and_report_initial_state() {
        let mut rom = vec![0; 0xC0];
        rom[0..4].copy_from_slice(&0xEAFF_FFFE_u32.to_le_bytes()); // B to self.
        rom[0xB2] = 0x96;
        let sum = rom[0xA0..=0xBC]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        rom[0xBD] = 0_u8.wrapping_sub(sum).wrapping_sub(0x19);
        let cartridge = GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid");
        let mut machine = GbaMachine::new(cartridge);

        assert_eq!(machine.run_until_dma_enabled(4, 10).unwrap(), None);
        assert_eq!(machine.run_until_dma_disabled(4, 10).unwrap(), None);
        assert_eq!(machine.run_until_dma_disabled(0, 0).unwrap(), Some(0));

        machine.bus_mut().write16(0x0200_0000, 0xBEEF);
        machine.bus_mut().write32(0x0400_00B0, 0x0200_0000);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_0000);
        machine.bus_mut().write16(0x0400_00B8, 1);
        machine.bus_mut().write16(0x0400_00BA, 0xA000); // One-shot HBlank DMA.

        assert_eq!(machine.run_until_dma_enabled(0, 0).unwrap(), Some(0));
        let steps = machine.run_until_dma_disabled(0, 2_000).unwrap();
        assert!(matches!(steps, Some(count) if count > 0 && count <= 2_000));
        assert_eq!(machine.bus().read16(0x0300_0000), 0xBEEF);
        assert_eq!(machine.bus().dma_enabled(0), Some(false));
    }

    #[test]
    fn dma_interrupt_runner_observes_hblank_completion_without_ie_or_ime() {
        let mut rom = vec![0; 0xC0];
        rom[0..4].copy_from_slice(&0xEAFF_FFFE_u32.to_le_bytes()); // B to self.
        rom[0xB2] = 0x96;
        let sum = rom[0xA0..=0xBC]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
        rom[0xBD] = 0_u8.wrapping_sub(sum).wrapping_sub(0x19);
        let cartridge = GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid");
        let mut machine = GbaMachine::new(cartridge);
        machine.bus_mut().write16(0x0200_0000, 0xCAFE);
        machine.bus_mut().write32(0x0400_00B0, 0x0200_0000);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_0000);
        machine.bus_mut().write16(0x0400_00B8, 1);
        machine.bus_mut().write16(0x0400_00BA, 0xE000); // HBlank + IRQ + enable.

        let steps = machine.run_until_dma_interrupt(0, 2_000).unwrap();
        assert!(matches!(steps, Some(count) if count > 0 && count <= 2_000));
        assert_eq!(machine.bus().read16(0x0300_0000), 0xCAFE);
        assert_eq!(machine.bus().interrupt_request_flags() & (1 << 8), 1 << 8);
        assert!(!machine.interrupt_pending()); // IE/IME do not gate the latched IF bit.
    }

    #[test]
    fn dma_interrupt_runner_handles_existing_flags_invalid_channels_and_zero_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_dma_interrupt(4, 10).unwrap(), None);
        assert_eq!(machine.run_until_dma_interrupt(0, 0).unwrap(), None);
        machine.request_interrupt(1 << 10);
        assert_eq!(machine.run_until_dma_interrupt(2, 0).unwrap(), Some(0));
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
    fn next_frame_runner_waits_for_wraparound_and_respects_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_next_frame(0).unwrap(), None);
        assert_eq!(machine.video_scanline(), 0);

        // Start immediately before the frame boundary. One valid instruction
        // crosses into scanline zero, without requiring a full-frame run.
        machine.bus_mut().advance_cycles(228 * 1232 - 1);
        assert_eq!(machine.video_scanline(), 227);
        let steps = machine.run_until_next_frame(4).unwrap();
        assert!(steps.is_some_and(|count| count > 0 && count <= 4));
        assert_eq!(machine.video_scanline(), 0);

        // Starting on scanline zero waits for the *next* wrap, not an immediate hit.
        assert_eq!(machine.run_until_next_frame(0).unwrap(), None);
        assert_eq!(machine.video_scanline(), 0);
    }

    #[test]
    fn zero_frame_stream_does_not_invoke_callback_or_advance_machine() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        let mut callbacks = 0;
        assert_eq!(
            machine
                .for_each_frame(0, 0, |_, _| {
                    callbacks += 1;
                    true
                })
                .unwrap(),
            Some(0)
        );
        assert_eq!(callbacks, 0);
        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
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
    fn machine_can_wait_for_timer_overflow_with_a_step_limit() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_timer_overflow(4, 10).unwrap(), None);
        assert_eq!(machine.run_until_timer_overflow(0, 0).unwrap(), None);

        machine.bus_mut().write16(0x0400_0100, 0xFFFF);
        machine.bus_mut().write16(0x0400_0102, 0x0080);
        assert_eq!(machine.timer_cycles_until_overflow(0), Some(1));
        assert_eq!(machine.run_until_timer_overflow(0, 1).unwrap(), Some(1));
        assert!(machine.cycles_elapsed() >= 1);
    }

    #[test]
    fn machine_gamepak_width_timing_tracks_waitcnt_changes() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0204, 0);
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 1),
            Some(4)
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 2),
            Some(4)
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(6)
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0E00_0000, false, 4),
            Some(16)
        );

        machine.bus_mut().write16(0x0400_0204, 0x0417);
        assert_eq!(machine.gamepak_wait_control(), 0x0417);
        assert_eq!(machine.diagnostic_snapshot().gamepak_wait_control, 0x0417);
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(4)
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 3),
            None
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0200_0000, false, 4),
            None
        );
    }

    #[test]
    fn machine_gamepak_sequence_timing_tracks_waitcnt_updates() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0204, 0);
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 4),
            Some(10)
        );

        machine.bus_mut().write16(0x0400_0204, (1 << 2) | (1 << 4));
        assert_eq!(machine.gamepak_wait_control(), 0x0014);
        assert_eq!(machine.diagnostic_snapshot().gamepak_wait_control, 0x0014);
        assert_eq!(machine.gamepak_access_cycles(0x0800_0000, false), Some(3));
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, true, 4),
            Some(2)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 4),
            Some(6)
        );

        machine
            .bus_mut()
            .write16(0x0400_0204, (1 << 2) | (1 << 4) | (2 << 5) | (1 << 7));
        assert_eq!(machine.gamepak_access_cycles(0x0A00_0000, false), Some(2));
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0A00_0000, false, 2, 4),
            Some(5)
        );
    }

    #[test]
    fn machine_gamepak_sram_timing_tracks_waitcnt_and_snapshot() {
        let mut machine = GbaMachine::new(cartridge());
        for (encoding, cycles) in [4, 3, 2, 8].into_iter().enumerate() {
            machine.bus_mut().write16(0x0400_0204, encoding as u16);
            assert_eq!(machine.gamepak_wait_control(), encoding as u16);
            assert_eq!(
                machine.gamepak_access_cycles_for_width(0x0E00_0000, false, 4),
                Some(cycles * 4)
            );
            assert_eq!(
                machine.diagnostic_snapshot().gamepak_wait_control,
                encoding as u16
            );
        }
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0E00_0000, false, 3),
            None
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0FFF_FFFE, false, 4),
            None
        );
    }

    #[test]
    fn machine_forwards_gamepak_sequence_boundary_behavior() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0204, 0);
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 0),
            Some(0)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 3),
            Some(8)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x09FF_FFFE, false, 4, 1),
            Some(6)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 3, 1),
            None
        );
    }

    #[test]
    fn machine_forwards_timer_video_and_gamepak_inspection() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.timer_counter(4), None);
        assert_eq!(machine.timer_enabled(4), None);
        assert_eq!(machine.timer_control(4), None);
        assert_eq!(machine.timer_irq_enabled(4), None);
        assert_eq!(machine.timer_cascade_enabled(4), None);
        assert_eq!(machine.timer_cycles_until_overflow(4), None);
        assert_eq!(machine.timer_counter(0), machine.bus().timer_counter(0));
        assert_eq!(machine.timer_control(0), machine.bus().timer_control(0));
        assert_eq!(machine.timer_irq_enabled(0), Some(false));
        assert_eq!(machine.timer_cascade_enabled(0), Some(false));
        assert_eq!(machine.timer_prescaler_divisor(0), Some(1));
        assert_eq!(machine.timer_prescaler_divisor(4), None);
        assert_eq!(machine.timer_prescaler_phase(0), Some(0));
        assert_eq!(machine.timer_prescaler_phase(4), None);
        assert_eq!(
            machine.timer_reload_value(0),
            machine.bus().timer_reload_value(0)
        );
        assert_eq!(machine.timer_enabled(0), machine.bus().timer_enabled(0));
        assert_eq!(
            machine.timer_cycles_until_overflow(0),
            machine.bus().timer_cycles_until_overflow(0)
        );
        assert_eq!(machine.video_scanline(), machine.bus().video_scanline());
        assert_eq!(machine.video_in_vblank(), machine.bus().video_in_vblank());
        assert_eq!(machine.video_in_hblank(), machine.bus().video_in_hblank());
        assert_eq!(
            machine.video_vcount_match(),
            machine.bus().video_vcount_match()
        );
        assert_eq!(
            machine.video_cycles_into_scanline(),
            machine.bus().video_cycles_into_scanline()
        );
        assert_eq!(
            machine.video_cycles_until_hblank(),
            machine.bus().video_cycles_until_hblank()
        );
        assert_eq!(
            machine.video_cycles_until_vblank(),
            machine.bus().video_cycles_until_vblank()
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 3),
            machine
                .bus()
                .gamepak_sequence_cycles(0x0800_0000, false, 2, 3)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 3, 2),
            None
        );
        assert_eq!(
            machine.gamepak_access_cycles(0x0800_0000, false),
            machine.bus().gamepak_access_cycles(0x0800_0000, false)
        );
        for width in [1, 2, 4, 3] {
            assert_eq!(
                machine.gamepak_access_cycles_for_width(0x0800_0000, false, width),
                machine
                    .bus()
                    .gamepak_access_cycles_for_width(0x0800_0000, false, width)
            );
        }
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0200_0000, false, 4),
            None
        );
        assert_eq!(machine.gamepak_access_cycles(0x0200_0000, false), None);
    }

    #[test]
    fn machine_diagnostic_snapshot_combines_cpu_video_counters_and_irq_state() {
        let mut machine = GbaMachine::new(cartridge());
        let initial = machine.diagnostic_snapshot();
        assert_eq!(initial.cpu.pc, CARTRIDGE_ENTRY_POINT);
        assert_eq!(initial.instructions_executed, 0);
        assert_eq!(initial.cycles_elapsed, 0);
        assert_eq!(initial.video_scanline, machine.video_scanline());
        assert_eq!(initial.video_vcount_match, machine.video_vcount_match());
        assert_eq!(
            initial.video_cycles_until_hblank,
            machine.video_cycles_until_hblank()
        );
        assert_eq!(
            initial.video_cycles_until_vblank,
            machine.video_cycles_until_vblank()
        );
        assert_eq!(initial.video_cycles_until_hblank, 960);
        assert_eq!(initial.video_cycles_until_vblank, 160 * 1232);
        assert_eq!(machine.cycles_elapsed(), initial.cycles_elapsed);

        machine.bus_mut().advance_cycles(960);
        let hblank = machine.diagnostic_snapshot();
        assert!(hblank.video_in_hblank);
        assert_eq!(hblank.video_cycles_until_hblank, 1232);
        assert_eq!(hblank.cycles_elapsed, initial.cycles_elapsed);
        assert!(!initial.interrupt_master_enabled);
        assert_eq!(initial.interrupt_enable, 0);
        assert_eq!(initial.interrupt_requests, 0);
        assert_eq!(initial.keypad_control, 0);
        assert_eq!(initial.pending_interrupts, 0);
        assert_eq!(initial.pending_interrupt_source, None);
        assert!(!initial.interrupt_pending);
        assert_eq!(initial.timer_cycles_until_overflow[0], None);
        assert_eq!(initial.timer_counters, [Some(0); 4]);
        assert_eq!(initial.timer_enabled, [Some(false); 4]);
        assert_eq!(initial.timer_irq_enabled, [Some(false); 4]);
        assert_eq!(initial.timer_reload_values, [Some(0); 4]);
        assert_eq!(initial.timer_prescaler_phases, [Some(0); 4]);
        assert_eq!(initial.timer_prescaler_divisors, [Some(1); 4]);
        assert_eq!(initial.timer_cascade_enabled, [Some(false); 4]);

        machine.bus_mut().write16(0x0400_0100, 0xFFFE);
        machine.bus_mut().write16(0x0400_0102, 0x00C0);
        let timer = machine.diagnostic_snapshot();
        assert_eq!(timer.timer_counters[0], Some(0xFFFE));
        assert_eq!(timer.timer_enabled[0], Some(true));
        assert_eq!(timer.timer_irq_enabled[0], Some(true));
        assert_eq!(timer.timer_cycles_until_overflow[0], Some(2));
        assert_eq!(initial.dma_enabled, [Some(false); 4]);
        assert_eq!(initial.dma_start_timing, [Some(0); 4]);
        assert_eq!(initial.dma_repeat, [Some(false); 4]);
        assert_eq!(initial.dma_word_transfer, [Some(false); 4]);
        assert_eq!(initial.dma_irq_enabled, [Some(false); 4]);
        assert_eq!(initial.dma_source_addresses, [Some(0); 4]);
        assert_eq!(initial.dma_destination_addresses, [Some(0); 4]);
        assert_eq!(initial.dma_count_registers, [Some(0); 4]);
        assert_eq!(
            initial.dma_transfer_counts,
            [Some(0x4000), Some(0x4000), Some(0x4000), Some(0x10000)]
        );
        assert_eq!(initial.audio_fifo_lengths, [Some(0); 2]);
        assert_eq!(initial.audio_fifo_last_samples, [Some(0); 2]);
        assert_eq!(initial.keypad_pressed, 0);
        assert!(!initial.keypad_irq_requested);

        machine.bus_mut().write16(0x0400_0200, 1 << 2);
        machine.request_interrupt(1 << 2);
        let pending = machine.diagnostic_snapshot();
        assert!(!pending.interrupt_master_enabled);
        assert_eq!(pending.interrupt_enable, 1 << 2);
        assert_eq!(pending.interrupt_requests, 1 << 2);
        assert_eq!(pending.pending_interrupts, 1 << 2);
        assert_eq!(pending.pending_interrupt_source, Some(2));
        // IF & IE indicate a pending source, but IME is still disabled.
        assert!(!pending.interrupt_pending);
        assert_eq!(pending.cpu.pc, machine.cpu().register(15).unwrap());

        machine.bus_mut().write16(0x0400_0208, 1);
        let master_enabled = machine.diagnostic_snapshot();
        assert!(master_enabled.interrupt_master_enabled);
        assert!(master_enabled.interrupt_pending);
        machine.bus_mut().write32(0x0400_00B0, 0x0200_1234);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_5678);
        machine.bus_mut().write16(0x0400_00B8, 7);
        let dma = machine.diagnostic_snapshot();
        assert_eq!(dma.dma_source_addresses[0], Some(0x0200_1234));
        assert_eq!(dma.dma_destination_addresses[0], Some(0x0300_5678));
        assert_eq!(dma.dma_count_registers[0], Some(7));
        assert_eq!(dma.dma_transfer_counts[0], Some(7));
        machine.bus_mut().write16(
            0x0400_00BA,
            (1 << 15) | (2 << 12) | (2 << 5) | (1 << 7) | (1 << 9) | (1 << 10),
        );
        let configured_dma = machine.diagnostic_snapshot();
        assert_eq!(configured_dma.dma_controls[0], Some(0xA6C0));
        assert_eq!(configured_dma.dma_source_controls[0], Some(1));
        assert_eq!(configured_dma.dma_destination_controls[0], Some(2));

        machine
            .bus_mut()
            .write16(0x0400_0132, (1 << 14) | GbaButton::A.mask());
        machine.set_button_pressed(GbaButton::A, true);
        let keypad = machine.diagnostic_snapshot();
        assert_eq!(keypad.keypad_control, (1 << 14) | GbaButton::A.mask());
        assert_eq!(keypad.keypad_pressed, GbaButton::A.mask());
        assert!(keypad.keypad_irq_requested);
    }

    #[test]
    fn machine_snapshot_tracks_repeated_vblank_dma_registers() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0200_0000, 0xCAFE);
        machine.bus_mut().write16(0x0200_0002, 0xFACE);
        machine.bus_mut().write32(0x0400_00B0, 0x0200_0000);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_0000);
        machine.bus_mut().write16(0x0400_00B8, 1);
        machine.bus_mut().write16(0x0400_00BA, 0x9260);

        let armed = machine.diagnostic_snapshot();
        assert_eq!(armed.dma_enabled[0], Some(true));
        assert_eq!(armed.dma_source_addresses[0], Some(0x0200_0000));
        assert_eq!(armed.dma_destination_addresses[0], Some(0x0300_0000));

        machine.bus_mut().advance_cycles(1232 * 160);
        let first = machine.diagnostic_snapshot();
        assert_eq!(machine.bus().read16(0x0300_0000), 0xCAFE);
        assert_eq!(first.dma_source_addresses[0], Some(0x0200_0002));
        assert_eq!(first.dma_destination_addresses[0], Some(0x0300_0000));
        assert_eq!(first.dma_count_registers[0], Some(1));
        assert_eq!(first.dma_enabled[0], Some(true));

        machine.bus_mut().advance_cycles(1232 * 228);
        let repeated = machine.diagnostic_snapshot();
        assert_eq!(machine.bus().read16(0x0300_0000), 0xFACE);
        assert_eq!(repeated.dma_source_addresses[0], Some(0x0200_0004));
        assert_eq!(repeated.dma_destination_addresses[0], Some(0x0300_0000));
        assert_eq!(repeated.dma_enabled[0], Some(true));
    }

    #[test]
    fn machine_cpu_snapshot_tracks_execution_state() {
        let machine = GbaMachine::new(cartridge());
        let snapshot = machine.cpu_snapshot();
        assert_eq!(snapshot.pc, CARTRIDGE_ENTRY_POINT);
        assert_eq!(snapshot.registers[15], CARTRIDGE_ENTRY_POINT);
        assert!(!snapshot.thumb);
        assert_eq!(snapshot.cpsr, machine.cpu().cpsr_flags());
    }

    #[test]
    fn machine_forwards_external_game_pak_dma_requests() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0200_0100, 0xBEEF);
        machine.bus_mut().write32(0x0400_00D4, 0x0200_0100);
        machine.bus_mut().write32(0x0400_00D8, 0x0300_0100);
        machine.bus_mut().write16(0x0400_00DC, 1);
        machine.bus_mut().write16(0x0400_00DE, 0x9000);

        assert!(!machine.request_game_pak_dma());
        assert_eq!(machine.bus().read16(0x0300_0100), 0);
        machine.bus_mut().write16(0x0400_00DE, 0x8800);
        assert!(machine.request_game_pak_dma());
        assert_eq!(machine.bus().read16(0x0300_0100), 0xBEEF);
    }

    #[test]
    fn machine_forwards_dma_control_diagnostics() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.dma_control(4), None);
        assert_eq!(machine.dma_enabled(4), None);
        assert_eq!(machine.dma_start_timing(4), None);
        assert_eq!(machine.dma_word_transfer(4), None);
        assert_eq!(machine.dma_irq_enabled(4), None);

        machine.bus_mut().write16(0x0400_00BA, 0xD400);
        assert_eq!(machine.dma_control(0), Some(0xD400));
        assert_eq!(machine.dma_enabled(0), Some(true));
        assert_eq!(machine.dma_start_timing(0), Some(1));
        assert_eq!(machine.dma_word_transfer(0), Some(true));
        assert_eq!(machine.dma_irq_enabled(0), Some(true));
    }

    #[test]
    fn machine_forwards_dma_address_and_count_registers() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write32(0x0400_00B0, 0x0200_1234);
        machine.bus_mut().write32(0x0400_00B4, 0x0300_5678);
        machine.bus_mut().write16(0x0400_00B8, 0xFFFF);
        assert_eq!(machine.dma_source_address(0), Some(0x0200_1234));
        assert_eq!(machine.dma_destination_address(0), Some(0x0300_5678));
        assert_eq!(machine.dma_count_register(0), Some(0x3FFF));
        assert_eq!(machine.dma_transfer_count(0), Some(0x3FFF));
        machine.bus_mut().write16(0x0400_00B8, 0);
        assert_eq!(machine.dma_transfer_count(0), Some(0x4000));
        assert_eq!(machine.dma_transfer_count(3), Some(0x1_0000));
        assert_eq!(machine.dma_source_address(4), None);
        assert_eq!(machine.dma_destination_address(4), None);
        assert_eq!(machine.dma_count_register(4), None);
    }

    #[test]
    fn machine_forwards_dma_address_modes_and_repeat() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_00BA, 0xD7E0);
        assert_eq!(machine.dma_destination_control(0), Some(3));
        assert_eq!(machine.dma_source_control(0), Some(3));
        assert_eq!(machine.dma_repeat(0), Some(true));
        assert_eq!(machine.dma_destination_control(4), None);
        assert_eq!(machine.dma_source_control(4), None);
        assert_eq!(machine.dma_repeat(4), None);
    }

    #[test]
    fn machine_exposes_waitcnt_and_timer_control_bits() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0204, 0x4017);
        assert_eq!(machine.gamepak_wait_control(), 0x4017);
        assert_eq!(
            machine.gamepak_wait_control(),
            machine.bus().gamepak_wait_control()
        );

        machine.bus_mut().write16(0x0400_0102, 0x00C5);
        assert_eq!(machine.timer_control(0), Some(0x00C1));
        assert_eq!(machine.timer_enabled(0), Some(true));
        assert_eq!(machine.timer_irq_enabled(0), Some(true));
        assert_eq!(machine.timer_cascade_enabled(0), Some(false));
        assert_eq!(machine.timer_prescaler_divisor(0), Some(64));
        assert_eq!(machine.timer_prescaler_phase(0), Some(0));

        for (selector, divisor) in [(0_u16, 1_u16), (1, 64), (2, 256), (3, 1024)] {
            machine.bus_mut().write16(0x0400_0102, 0x0080 | selector);
            assert_eq!(machine.timer_prescaler_divisor(0), Some(divisor));
            assert_eq!(
                machine.timer_prescaler_phase(0),
                machine.bus().timer_prescaler_phase(0)
            );
        }
    }

    #[test]
    fn audio_fifo_refill_runner_handles_threshold_invalid_index_and_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_audio_fifo_refill(0, 0).unwrap(), Some(0));
        assert_eq!(machine.run_until_audio_fifo_refill(2, 10).unwrap(), None);

        for _ in 0..5 {
            machine.bus_mut().write32(0x0400_00A0, 0x0102_0304);
        }
        assert_eq!(machine.audio_fifo_bytes_until_refill(0), Some(4));
        assert_eq!(machine.run_until_audio_fifo_refill(0, 0).unwrap(), None);
    }

    #[test]
    fn keypad_irq_waiter_observes_keycnt_condition_and_interrupt_masks() {
        let mut machine = GbaMachine::new(cartridge());
        let keycnt = (1 << 14) | GbaButton::A.mask();
        machine.bus_mut().write16(0x0400_0132, keycnt);
        assert!(!machine.keypad_irq_requested());
        assert_eq!(machine.run_until_keypad_interrupt(0).unwrap(), 0);
        assert!(!machine.interrupt_pending());

        machine.set_button_pressed(GbaButton::A, true);
        assert!(machine.keypad_irq_requested());
        assert_eq!(machine.interrupt_request_flags() & (1 << 12), 1 << 12);
        // The request is latched, but it is not CPU-visible until IE and IME are set.
        assert!(!machine.interrupt_pending());
        machine.bus_mut().write16(0x0400_0200, 1 << 12);
        machine.bus_mut().write16(0x0400_0208, 1);
        assert!(machine.interrupt_pending());
        assert_eq!(machine.run_until_keypad_interrupt(0).unwrap(), 0);

        machine.acknowledge_interrupts(1 << 12);
        assert!(!machine.interrupt_pending());
        assert!(machine.keypad_irq_requested());
        machine.set_button_pressed(GbaButton::A, false);
        assert!(!machine.keypad_irq_requested());
    }

    #[test]
    fn machine_exposes_audio_fifo_refill_distance() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.audio_fifo_bytes_until_refill(0), Some(0));
        assert_eq!(machine.audio_fifo_bytes_until_refill(2), None);
        machine.bus_mut().write32(0x0400_00A0, 0x0102_0304);
        for _ in 0..7 {
            machine.bus_mut().write32(0x0400_00A0, 0x0102_0304);
        }
        assert_eq!(machine.audio_fifo_bytes_until_refill(0), Some(16));
    }

    #[test]
    fn machine_exposes_direct_sound_fifo_refill_status() {
        let mut machine = GbaMachine::new(cartridge());
        let status = machine.audio_fifo_status(0).unwrap();
        assert_eq!(status.queued_bytes, 0);
        assert_eq!(status.capacity, 32);
        assert!(status.refill_threshold_reached);
        assert_eq!(machine.audio_fifo_status(2), None);

        for _ in 0..5 {
            machine.bus_mut().write32(0x0400_00A0, 0x0102_0304);
        }
        let status = machine.audio_fifo_status(0).unwrap();
        assert_eq!(status.queued_bytes, 20);
        assert!(!status.refill_threshold_reached);
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
        // The mixer returns half-volume samples unless SOUNDCNT_H selects full volume.
        assert_eq!(machine.direct_sound_stereo_sample(), (32, 32));
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

        assert_eq!(machine.cartridge_save_type(), Some(crate::SaveType::Sram));
        assert_eq!(
            machine.diagnostic_snapshot().cartridge_save_type,
            Some(crate::SaveType::Sram)
        );
        assert_eq!(
            machine.diagnostic_snapshot().cartridge_save_size_bytes,
            Some(crate::memory::SRAM_SIZE)
        );
        assert_eq!(
            machine.save_data_size_bytes(),
            Some(crate::memory::SRAM_SIZE)
        );
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
            assert_eq!(machine.cartridge_save_type(), Some(save_type));
            let snapshot = machine.diagnostic_snapshot();
            assert_eq!(snapshot.cartridge_save_type, Some(save_type));
            assert_eq!(snapshot.cartridge_save_size_bytes, Some(size));
            assert_eq!(
                snapshot.cartridge_rom_size_bytes,
                GBA_HEADER_SIZE + marker.len()
            );
            assert_eq!(machine.save_data_size_bytes(), Some(size));
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

    #[test]
    fn machine_snapshot_reports_all_dma_channels_independently() {
        let mut machine = GbaMachine::new(cartridge());

        for channel in 0..4_u32 {
            let base = 0x0400_00B0 + channel * 12;
            let source = 0x0200_1000 + channel * 0x100;
            let destination = 0x0300_2000 + channel * 0x100;
            let count = (channel + 1) as u16;

            machine.bus_mut().write32(base, source);
            machine.bus_mut().write32(base + 4, destination);
            machine.bus_mut().write16(base + 8, count);
            machine.bus_mut().write16(base + 10, 0xD400);
        }

        let snapshot = machine.diagnostic_snapshot();
        for channel in 0..4 {
            assert_eq!(
                snapshot.dma_source_addresses[channel],
                Some(0x0200_1000 + channel as u32 * 0x100)
            );
            assert_eq!(
                snapshot.dma_destination_addresses[channel],
                Some(0x0300_2000 + channel as u32 * 0x100)
            );
            assert_eq!(
                snapshot.dma_count_registers[channel],
                Some(channel as u16 + 1)
            );
            assert_eq!(snapshot.dma_enabled[channel], Some(true));
            assert_eq!(snapshot.dma_irq_enabled[channel], Some(true));
            assert_eq!(snapshot.dma_word_transfer[channel], Some(true));
            assert_eq!(snapshot.dma_start_timing[channel], Some(1));
        }

        assert_eq!(machine.dma_source_address(4), None);
        assert_eq!(machine.dma_destination_address(4), None);
        assert_eq!(machine.dma_count_register(4), None);
        assert_eq!(machine.dma_enabled(4), None);
    }

    #[test]
    fn machine_snapshot_reflects_dma_channel_reconfiguration() {
        let mut machine = GbaMachine::new(cartridge());
        let base = 0x0400_00B0;

        machine.bus_mut().write32(base, 0x0200_0100);
        machine.bus_mut().write32(base + 4, 0x0300_0100);
        machine.bus_mut().write16(base + 8, 12);
        machine.bus_mut().write16(base + 10, 0xA400);

        let first = machine.diagnostic_snapshot();
        assert_eq!(first.dma_enabled[0], Some(true));
        assert_eq!(first.dma_source_addresses[0], Some(0x0200_0100));
        assert_eq!(first.dma_destination_addresses[0], Some(0x0300_0100));
        assert_eq!(first.dma_count_registers[0], Some(12));
        assert_eq!(first.dma_irq_enabled[0], Some(false));

        machine.bus_mut().write32(base, 0x0200_0300);
        machine.bus_mut().write32(base + 4, 0x0300_0500);
        machine.bus_mut().write16(base + 8, 3);
        machine.bus_mut().write16(base + 10, 0xD000);

        let second = machine.diagnostic_snapshot();
        assert_eq!(second.dma_enabled[0], Some(true));
        assert_eq!(second.dma_source_addresses[0], Some(0x0200_0300));
        assert_eq!(second.dma_destination_addresses[0], Some(0x0300_0500));
        assert_eq!(second.dma_count_registers[0], Some(3));
        assert_eq!(second.dma_irq_enabled[0], Some(true));
        assert_eq!(second.dma_word_transfer[0], Some(false));
    }

    #[test]
    fn gamepak_timing_queries_are_side_effect_free_and_validate_inputs() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0204, 0x0417);
        let before = machine.diagnostic_snapshot();

        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(4)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x0800_0000, false, 2, 0),
            Some(0)
        );
        assert_eq!(
            machine.gamepak_sequence_cycles(0x09FF_FFFF, false, 4, 2),
            Some(12)
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0200_0000, false, 2),
            None
        );
        assert_eq!(
            machine.gamepak_access_cycles_for_width(0x0800_0000, false, 0),
            None
        );

        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
        assert_eq!(after.gamepak_wait_control, before.gamepak_wait_control);
    }

    #[test]
    fn machine_video_position_helpers_reject_invalid_targets_without_running() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();

        assert_eq!(machine.run_until_video_position(228, 0, 10).unwrap(), None);
        assert_eq!(machine.run_until_video_position(0, 1232, 10).unwrap(), None);
        assert_eq!(
            machine.run_until_next_video_position(228, 0, 10).unwrap(),
            None
        );
        assert_eq!(
            machine.run_until_next_video_position(0, 1232, 10).unwrap(),
            None
        );
        assert_eq!(machine.run_until_video_position(0, 0, 0).unwrap(), Some(0));

        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }

    #[test]
    fn machine_interrupt_acknowledgement_preserves_unrelated_requests() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0200, 0b0000_0101);
        machine.request_interrupt(0b0000_0101);
        assert_eq!(machine.interrupt_request_flags() & 0b0101, 0b0101);

        machine.acknowledge_interrupts(0b0000_0001);
        assert_eq!(machine.interrupt_request_flags() & 0b0001, 0);
        assert_eq!(machine.interrupt_request_flags() & 0b0100, 0b0100);
        assert_eq!(machine.pending_interrupts() & 0b0101, 0b0100);
    }

    #[test]
    fn audio_fifo_queue_runners_handle_empty_nonempty_invalid_and_zero_budget() {
        let mut machine = GbaMachine::new(cartridge());
        assert_eq!(machine.run_until_audio_fifo_nonempty(0, 0).unwrap(), None);
        assert_eq!(machine.run_until_audio_fifo_empty(0, 0).unwrap(), Some(0));
        assert_eq!(machine.run_until_audio_fifo_nonempty(2, 10).unwrap(), None);
        assert_eq!(machine.run_until_audio_fifo_empty(2, 10).unwrap(), None);

        machine.bus_mut().write32(0x0400_00A0, 0x1234_5678);
        assert_eq!(machine.audio_fifo_len(0), Some(4));
        assert_eq!(
            machine.run_until_audio_fifo_nonempty(0, 0).unwrap(),
            Some(0)
        );
        assert_eq!(machine.run_until_audio_fifo_empty(0, 0).unwrap(), None);
        assert_eq!(machine.audio_fifo_len(0), Some(4));
    }

    #[test]
    fn machine_audio_fifo_helpers_validate_fifo_indices() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.audio_fifo_status(2), None);
        assert_eq!(machine.audio_fifo_len(2), None);
        assert_eq!(machine.audio_fifo_last_sample(2), None);
        assert_eq!(machine.audio_fifo_bytes_until_refill(2), None);
    }

    #[test]
    fn machine_dma_diagnostics_reject_out_of_range_channels() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.dma_control(4), None);
        assert_eq!(machine.dma_source_address(4), None);
        assert_eq!(machine.dma_destination_address(4), None);
        assert_eq!(machine.dma_count_register(4), None);
        assert_eq!(machine.dma_transfer_count(4), None);
        assert_eq!(machine.dma_enabled(4), None);
        assert_eq!(machine.dma_start_timing(4), None);
        assert_eq!(machine.dma_destination_control(4), None);
        assert_eq!(machine.dma_source_control(4), None);
        assert_eq!(machine.dma_repeat(4), None);
        assert_eq!(machine.dma_word_transfer(4), None);
        assert_eq!(machine.dma_irq_enabled(4), None);
    }

    #[test]
    fn invalid_audio_refill_wait_does_not_execute_instructions() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        assert_eq!(machine.run_until_audio_fifo_refill(2, 32).unwrap(), None);
        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }

    #[test]
    fn machine_timer_diagnostics_reject_out_of_range_indices() {
        let machine = GbaMachine::new(cartridge());
        assert_eq!(machine.timer_counter(4), None);
        assert_eq!(machine.timer_reload_value(4), None);
        assert_eq!(machine.timer_enabled(4), None);
        assert_eq!(machine.timer_control(4), None);
        assert_eq!(machine.timer_prescaler_divisor(4), None);
        assert_eq!(machine.timer_prescaler_phase(4), None);
        assert_eq!(machine.timer_irq_enabled(4), None);
        assert_eq!(machine.timer_cascade_enabled(4), None);
        assert_eq!(machine.timer_cycles_until_overflow(4), None);
    }

    #[test]
    fn invalid_timer_overflow_wait_does_not_execute_instructions() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        assert_eq!(machine.run_until_timer_overflow(4, 32).unwrap(), None);
        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }

    #[test]
    fn keypad_irq_waiter_respects_step_budget_without_matching_keys() {
        let mut machine = GbaMachine::new(cartridge());
        machine
            .bus_mut()
            .write16(0x0400_0132, (1 << 14) | GbaButton::A.mask());
        machine.bus_mut().write16(0x0400_0200, 1 << 12);
        machine.bus_mut().write16(0x0400_0208, 1);
        assert_eq!(machine.run_until_keypad_interrupt(3).unwrap(), 3);
        assert!(!machine.keypad_irq_requested());
        assert!(!machine.interrupt_pending());
        assert_eq!(machine.instructions_executed(), 3);
    }

    #[test]
    fn cycle_budget_and_absolute_runners_respect_zero_and_step_limits() {
        let mut machine = GbaMachine::new(cartridge());
        let initial = machine.diagnostic_snapshot();

        assert_eq!(machine.run_for_cycles(0).unwrap(), 0);
        assert_eq!(machine.run_until_instruction_count(1, 0).unwrap(), None);
        assert_eq!(machine.run_until_cycle_count(1, 0).unwrap(), None);
        let untouched = machine.diagnostic_snapshot();
        assert_eq!(
            untouched.instructions_executed,
            initial.instructions_executed
        );
        assert_eq!(untouched.cycles_elapsed, initial.cycles_elapsed);
        assert_eq!(untouched.video_scanline, initial.video_scanline);
        assert_eq!(
            untouched.video_cycles_into_scanline,
            initial.video_cycles_into_scanline
        );

        assert_eq!(machine.run_until_instruction_count(3, 3).unwrap(), Some(3));
        assert_eq!(machine.instructions_executed(), 3);
        assert_eq!(machine.run_until_instruction_count(2, 0).unwrap(), Some(0));

        let before_cycle_wait = machine.cycles_elapsed();
        assert_eq!(
            machine
                .run_until_cycle_count(before_cycle_wait + 1, 0)
                .unwrap(),
            None
        );
        assert_eq!(machine.cycles_elapsed(), before_cycle_wait);
        assert_eq!(
            machine.run_until_cycle_count(before_cycle_wait, 0).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn interrupt_mask_runner_ignores_unselected_pending_sources() {
        let mut machine = GbaMachine::new(cartridge());
        let selected = 1_u16 << 4;
        let unrelated = 1_u16 << 2;

        machine.bus_mut().write16(0x0400_0200, selected | unrelated);
        machine.bus_mut().write16(0x0400_0208, 1);
        machine.request_interrupt(unrelated);
        assert_eq!(machine.pending_interrupts() & unrelated, unrelated);
        assert_eq!(machine.pending_interrupts() & selected, 0);

        let before = machine.instructions_executed();
        assert_eq!(machine.run_until_interrupt_mask(selected, 2).unwrap(), 2);
        assert_eq!(machine.instructions_executed(), before + 2);

        machine.request_interrupt(selected);
        assert_eq!(machine.run_until_interrupt_mask(selected, 2).unwrap(), 0);
        assert_eq!(
            machine.run_until_interrupt_mask(0, 2).unwrap(),
            2,
            "a zero mask must never match, even when interrupts are pending"
        );
    }

    #[test]
    fn next_interrupt_mask_wait_requires_a_fresh_selected_request() {
        let mut machine = GbaMachine::new(cartridge());
        let selected = 1_u16 << 4;
        machine.bus_mut().write16(0x0400_0200, selected);
        machine.bus_mut().write16(0x0400_0208, 1);
        machine.request_interrupt(selected);

        assert_eq!(
            machine.run_until_next_interrupt_mask(selected, 2).unwrap(),
            2,
            "an already-pending source is not a fresh assertion"
        );
        assert_eq!(machine.instructions_executed(), 2);
    }

    #[test]
    fn diagnostic_snapshot_is_read_only_and_tracks_waitcnt_updates() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        let repeated = machine.diagnostic_snapshot();
        assert_eq!(before.instructions_executed, repeated.instructions_executed);
        assert_eq!(before.cycles_elapsed, repeated.cycles_elapsed);
        assert_eq!(before.video_scanline, repeated.video_scanline);
        assert_eq!(
            before.video_cycles_into_scanline,
            repeated.video_cycles_into_scanline
        );

        machine.bus_mut().write16(0x0400_0204, 0x1234);
        let after = machine.diagnostic_snapshot();
        assert_eq!(after.gamepak_wait_control, 0x1234);
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }

    #[test]
    fn machine_keypad_irq_is_edge_triggered_across_release_and_repress() {
        let mut machine = GbaMachine::new(cartridge());
        let irq = 1_u16 << 12;
        machine
            .bus_mut()
            .write16(0x0400_0132, (1 << 14) | crate::GbaButton::A.mask());

        machine.set_button_pressed(crate::GbaButton::A, true);
        assert!(machine.keypad_irq_requested());
        assert_eq!(machine.interrupt_request_flags() & irq, irq);

        machine.acknowledge_interrupts(irq);
        machine.set_button_pressed(crate::GbaButton::A, true);
        assert_eq!(
            machine.interrupt_request_flags() & irq,
            0,
            "holding a matching key must not relatch the edge-triggered IRQ"
        );

        machine.set_button_pressed(crate::GbaButton::A, false);
        assert!(!machine.keypad_irq_requested());
        machine.set_button_pressed(crate::GbaButton::A, true);
        assert_eq!(machine.interrupt_request_flags() & irq, irq);
    }

    #[test]
    fn machine_forwards_keypad_and_fifo_status_without_advancing_time() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();

        machine
            .bus_mut()
            .write16(0x0400_0132, (1 << 14) | crate::GbaButton::Start.mask());
        machine.set_button_pressed(crate::GbaButton::Start, true);
        assert!(machine.is_button_pressed(crate::GbaButton::Start));
        assert!(machine.keypad_irq_requested());
        assert_eq!(machine.interrupt_request_flags() & (1 << 12), 1 << 12);

        machine.bus_mut().write32(0x0400_00A4, 0x807F_01FF);
        assert_eq!(machine.audio_fifo_len(1), Some(4));
        assert_eq!(machine.audio_fifo_bytes_until_refill(1), Some(0));
        assert_eq!(machine.audio_fifo_status(1).unwrap().queued_bytes, 4);
        assert_eq!(machine.audio_fifo_len(2), None);

        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }

    #[test]
    fn save_state_restores_cpu_ram_interrupts_timers_video_and_counters() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write32(0x0200_0100, 0xCAFE_BABE);
        machine.bus_mut().write16(0x0400_0100, 0xFFFE);
        machine.bus_mut().write16(0x0400_0102, 0x0080);
        machine.bus_mut().advance_cycles(1);
        machine.request_interrupt(1 << 4);
        machine.set_button_pressed(crate::GbaButton::A, true);
        machine.bus_mut().write32(0x0400_00A0, 0x807F_01FF);
        let mut cpu_snapshot = machine.cpu().snapshot();
        cpu_snapshot.registers[0] = 0x1234_5678;
        cpu_snapshot.pc = CARTRIDGE_ENTRY_POINT;
        machine.restore_cpu_snapshot(cpu_snapshot);
        let checkpoint = machine.save_state();
        let checkpoint_diagnostics = machine.diagnostic_snapshot();

        machine.bus_mut().write32(0x0200_0100, 0xDEAD_BEEF);
        machine.bus_mut().write16(0x0400_0100, 0x4321);
        machine.acknowledge_interrupts(1 << 4);
        machine.set_button_pressed(crate::GbaButton::A, false);
        machine.bus_mut().write32(0x0400_00A0, 0);
        let mut changed_cpu = machine.cpu().snapshot();
        changed_cpu.registers[0] = 0;
        machine.restore_cpu_snapshot(changed_cpu);
        machine.run_for_instructions(3).unwrap();
        machine.bus_mut().advance_cycles(2000);

        machine.load_state(&checkpoint);
        assert_eq!(machine.bus().read32(0x0200_0100), 0xCAFE_BABE);
        assert_eq!(machine.cpu().register(0), Some(0x1234_5678));
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
        assert_eq!(machine.interrupt_request_flags() & (1 << 4), 1 << 4);
        assert_eq!(machine.timer_counter(0), Some(0xFFFF));
        assert!(machine.is_button_pressed(crate::GbaButton::A));
        assert_eq!(machine.audio_fifo_len(0), Some(4));
        assert_eq!(
            machine.instructions_executed(),
            checkpoint_diagnostics.instructions_executed
        );
        assert_eq!(
            machine.cycles_elapsed(),
            checkpoint_diagnostics.cycles_elapsed
        );
        let restored_diagnostics = machine.diagnostic_snapshot();
        assert_eq!(
            restored_diagnostics.video_scanline,
            checkpoint_diagnostics.video_scanline
        );
        assert_eq!(
            restored_diagnostics.video_cycles_into_scanline,
            checkpoint_diagnostics.video_cycles_into_scanline
        );

        // Checkpoints are reusable rather than consumed by a restore.
        machine.bus_mut().write32(0x0200_0100, 0);
        machine.load_state(&checkpoint);
        assert_eq!(machine.bus().read32(0x0200_0100), 0xCAFE_BABE);
    }

    #[test]
    fn unsupported_rom_instruction_does_not_advance_machine_counters() {
        let mut rom = vec![0; GBA_HEADER_SIZE];
        // Undefined ARM instruction at the entry point; keep the cartridge
        // header valid so construction succeeds and execution reaches it.
        rom[0..4].copy_from_slice(&0xE7F0_00F0_u32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"TGME");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let mut machine = GbaMachine::new(GbaCartridge::parse(&rom).unwrap());
        let before = machine.diagnostic_snapshot();

        let error = machine.step().unwrap_err();
        assert!(matches!(error, CpuError::UnsupportedInstruction { .. }));

        let after = machine.diagnostic_snapshot();
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(machine.cpu().register(15), Some(CARTRIDGE_ENTRY_POINT));
    }

    #[test]
    fn instruction_runner_reports_the_same_cycles_as_machine_counters() {
        let mut machine = GbaMachine::new(cartridge());
        let cycles = machine.run_for_instructions(8).unwrap();

        assert_eq!(machine.instructions_executed(), 8);
        assert_eq!(machine.cycles_elapsed(), cycles);
        assert!(cycles > 0);
    }

    #[test]
    fn machine_timer_reconfiguration_is_visible_without_advancing_cycles() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write16(0x0400_0100, 0xFFFE);
        machine.bus_mut().write16(0x0400_0102, 0x0081);
        machine.bus_mut().advance_cycles(17);
        let before = machine.diagnostic_snapshot();
        assert_eq!(machine.timer_prescaler_phase(0), Some(17));

        machine.bus_mut().write16(0x0400_0102, 0x0001);
        let after = machine.diagnostic_snapshot();
        assert_eq!(machine.timer_prescaler_phase(0), Some(0));
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.instructions_executed, before.instructions_executed);
    }

    #[test]
    fn loading_the_same_checkpoint_twice_replays_identical_machine_state() {
        let mut machine = GbaMachine::new(cartridge());
        machine.bus_mut().write32(0x0200_0000, 0xE3A0_0001);
        let checkpoint = machine.save_state();

        machine.bus_mut().write32(0x0200_0000, 0xDEAD_BEEF);
        machine.load_state(&checkpoint);
        let first = machine.diagnostic_snapshot();
        machine.bus_mut().write32(0x0200_0000, 0x1234_5678);
        machine.load_state(&checkpoint);
        let second = machine.diagnostic_snapshot();

        assert_eq!(first, second);
        assert_eq!(machine.bus().read32(0x0200_0000), 0xE3A0_0001);
    }

    #[test]
    fn zero_budget_interrupt_and_frame_waiters_do_not_advance_state() {
        let mut machine = GbaMachine::new(cartridge());
        let before = machine.diagnostic_snapshot();
        assert_eq!(machine.run_until_interrupt(0), Ok(0));
        assert_eq!(machine.run_until_next_frame(0), Ok(None));
        assert_eq!(machine.run_until_next_scanline(1, 0), Ok(None));
        let after = machine.diagnostic_snapshot();
        assert_eq!(after.cycles_elapsed, before.cycles_elapsed);
        assert_eq!(after.instructions_executed, before.instructions_executed);
        assert_eq!(after.video_scanline, before.video_scanline);
        assert_eq!(
            after.video_cycles_into_scanline,
            before.video_cycles_into_scanline
        );
    }
}
