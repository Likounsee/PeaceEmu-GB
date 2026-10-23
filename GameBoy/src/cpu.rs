//! Partial ARM7TDMI ARM/Thumb execution core with estimated instruction and Game Pak wait-state timing.

use crate::MemoryBus;

/// ARM condition flags in the CPSR.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConditionFlags {
    /// Negative flag.
    pub negative: bool,
    /// Zero flag.
    pub zero: bool,
    /// Carry / not-borrow flag.
    pub carry: bool,
    /// Signed overflow flag.
    pub overflow: bool,
}

/// Result of executing one instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// An instruction was executed.
    Executed {
        /// Address of the executed instruction.
        address: u32,
        /// Raw 32-bit instruction word.
        instruction: u32,
    },
    /// The CPU is halted and is waiting for an enabled interrupt request.
    Halted,
    /// An instruction was skipped because its condition failed.
    ConditionFailed {
        /// Address of the skipped instruction.
        address: u32,
        /// Raw 32-bit instruction word.
        instruction: u32,
    },
}

/// Errors for CPU instructions that are not implemented yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuError {
    /// The instruction encoding is outside the currently implemented subset.
    UnsupportedInstruction {
        /// Address of the unsupported instruction.
        address: u32,
        /// Raw 32-bit instruction word.
        instruction: u32,
    },
}

impl core::fmt::Display for CpuError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedInstruction {
                address,
                instruction,
            } => write!(
                formatter,
                "unsupported CPU instruction 0x{instruction:08X} at 0x{address:08X}"
            ),
        }
    }
}

impl CpuError {
    /// Returns the address of the instruction that could not be executed.
    pub fn address(self) -> u32 {
        match self {
            Self::UnsupportedInstruction { address, .. } => address,
        }
    }

    /// Returns the raw instruction word (Thumb instructions occupy its low 16 bits).
    pub fn instruction(self) -> u32 {
        match self {
            Self::UnsupportedInstruction { instruction, .. } => instruction,
        }
    }
}

impl std::error::Error for CpuError {}

/// Immutable, debugger-friendly snapshot of the modeled ARM7TDMI state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuSnapshot {
    /// General-purpose registers r0 through r15.
    pub registers: [u32; 16],
    /// Modeled condition flags.
    pub flags: ConditionFlags,
    /// Modeled CPSR bits (N, Z, C, V and T).
    pub cpsr: u32,
    /// Whether the processor is in Thumb state.
    pub thumb: bool,
    /// Whether BIOS Halt is active and waiting for an enabled interrupt.
    pub halted: bool,
    /// Interrupt sources selected by BIOS IntrWait/VBlankIntrWait, if any.
    pub bios_wait_mask: Option<u16>,
    /// Stored program-counter value.
    pub pc: u32,
}

/// ARM7TDMI CPU state with partial ARM and Thumb instruction support.
///
/// Banked registers, SPSRs, exception entry/return, and cycle-accurate
/// execution are not modeled yet.
#[derive(Clone, Debug)]
pub struct Cpu {
    registers: [u32; 16],
    flags: ConditionFlags,
    thumb: bool,
    /// True after BIOS Halt (SWI 0x02) until an enabled interrupt is pending.
    halted: bool,
    /// Interrupt mask awaited by BIOS IntrWait/VBlankIntrWait, if any.
    bios_wait_mask: Option<u16>,
    /// Expected address of the next sequential instruction fetch and its state.
    last_fetch_next: Option<(u32, bool)>,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Creates a CPU with zeroed registers and flags.
    ///
    /// Set the program counter to the desired entry point before stepping.
    pub fn new() -> Self {
        Self {
            registers: [0; 16],
            flags: ConditionFlags::default(),
            thumb: false,
            halted: false,
            bios_wait_mask: None,
            last_fetch_next: None,
        }
    }

    /// Resets all registers and flags and sets the initial program counter.
    pub fn reset(&mut self, entry_point: u32) {
        self.registers = [0; 16];
        self.registers[15] = entry_point & !3;
        self.flags = ConditionFlags::default();
        self.thumb = false;
        self.halted = false;
        self.bios_wait_mask = None;
        self.last_fetch_next = None;
    }

    /// Returns a general-purpose register, including r15 as the current PC.
    pub fn register(&self, index: usize) -> Option<u32> {
        self.registers.get(index).copied()
    }

    /// Returns a snapshot of all sixteen registers (r0–r15).
    ///
    /// This is intended for debuggers and state inspection; modifying the returned
    /// array cannot mutate the live CPU.
    pub fn registers(&self) -> [u32; 16] {
        self.registers
    }

    /// Returns the modeled CPSR bits: N, Z, C, V and T.
    ///
    /// Unmodeled mode, interrupt-mask, and other CPSR fields read as zero.
    pub fn cpsr_flags(&self) -> u32 {
        (u32::from(self.flags.negative) << 31)
            | (u32::from(self.flags.zero) << 30)
            | (u32::from(self.flags.carry) << 29)
            | (u32::from(self.flags.overflow) << 28)
            | (u32::from(self.thumb) << 5)
    }

    /// Sets a general-purpose register. Writes to r15 follow the current
    /// instruction state: halfword alignment in Thumb state, word alignment in ARM.
    pub fn set_register(&mut self, index: usize, value: u32) -> bool {
        if index >= self.registers.len() {
            return false;
        }
        if index == 15 {
            // A debugger/loader PC edit breaks the previous fetch sequence and
            // explicitly resumes execution from the new address.
            self.last_fetch_next = None;
            self.halted = false;
            self.bios_wait_mask = None;
        }
        self.registers[index] = if index == 15 {
            if self.thumb {
                value & !1
            } else {
                value & !3
            }
        } else {
            value
        };
        true
    }

    /// Returns whether BIOS Halt has stopped instruction execution.
    ///
    /// A debugger CPU snapshot does not preserve this wait state; full machine
    /// save states do, because they clone the complete CPU.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Returns whether the CPU is currently executing Thumb instructions.
    pub fn is_thumb(&self) -> bool {
        self.thumb
    }

    /// Sets the execution state for an external loader or debugger.
    ///
    /// Switching state aligns the stored PC to a halfword in Thumb state or
    /// a word in ARM state. Normal guest software should use BX to switch state.
    pub fn set_execution_state(&mut self, thumb: bool) {
        // This API is for an external loader/debugger, not an instruction.
        // A state change therefore also releases a BIOS wait at the old PC.
        self.halted = false;
        self.bios_wait_mask = None;
        self.thumb = thumb;
        self.last_fetch_next = None;
        self.registers[15] = if thumb {
            self.registers[15] & !1
        } else {
            self.registers[15] & !3
        };
    }

    /// Returns the modeled CPSR condition flags.
    pub fn flags(&self) -> ConditionFlags {
        self.flags
    }

    /// Replaces the modeled condition flags for a debugger or test harness.
    ///
    /// This changes N, Z, C, and V only; it does not change execution state or PC.
    pub fn set_condition_flags(&mut self, flags: ConditionFlags) {
        self.flags = flags;
    }

    /// Captures the current modeled state for debugging or save-state tooling.
    pub fn snapshot(&self) -> CpuSnapshot {
        CpuSnapshot {
            registers: self.registers,
            flags: self.flags,
            cpsr: self.cpsr_flags(),
            thumb: self.thumb,
            halted: self.halted,
            bios_wait_mask: self.bios_wait_mask,
            pc: self.registers[15],
        }
    }

    /// Restores a modeled CPU snapshot for a debugger or save-state loader.
    ///
    /// The program counter is aligned to the restored instruction state and the
    /// sequential-fetch hint is discarded, so the next instruction starts a
    /// fresh bus fetch sequence. BIOS Halt and its optional interrupt wait mask
    /// are preserved. Banked registers, SPSRs, and unmodeled CPSR fields are not
    /// part of a snapshot.
    pub fn restore_snapshot(&mut self, snapshot: CpuSnapshot) {
        self.registers = snapshot.registers;
        self.thumb = snapshot.thumb;
        self.registers[15] = if self.thumb {
            snapshot.pc & !1
        } else {
            snapshot.pc & !3
        };
        self.flags = snapshot.flags;
        self.halted = snapshot.halted;
        self.bios_wait_mask = snapshot.bios_wait_mask;
        self.last_fetch_next = None;
    }

    /// Evaluates an ARM condition code against the current flags.
    /// Codes 0x0–0xE follow ARM semantics; reserved 0xF and larger values return false.
    pub fn condition_passed_for(&self, condition: u8) -> bool {
        condition <= 0xF && self.condition_passed(u32::from(condition))
    }

    /// Executes one instruction and advances bus timers by an approximate cycle count.
    ///
    /// This is a transitional timing API: it accounts for common instruction
    /// classes and adds excess WAITCNT latency for Game Pak instruction fetches
    /// and supported scalar/multiple data accesses. Uncommon transfer encodings,
    /// exact pipeline refill timing, and DMA stalls remain incomplete. Use `step`
    /// when callers need instruction execution without advancing peripheral time.
    pub fn step_with_cycles(
        &mut self,
        bus: &mut MemoryBus,
    ) -> Result<(StepOutcome, u32), CpuError> {
        if self.halted {
            let pending = bus.pending_interrupts();
            if let Some(mask) = self.bios_wait_mask {
                let matched = pending & mask;
                if matched == 0 {
                    bus.advance_cycles(1);
                    return Ok((StepOutcome::Halted, 1));
                }
                // IntrWait consumes the interrupt flags it waited for. This
                // models the BIOS wait contract, not IRQ exception entry.
                bus.acknowledge_interrupts(matched);
                self.bios_wait_mask = None;
            } else if pending == 0 {
                bus.advance_cycles(1);
                return Ok((StepOutcome::Halted, 1));
            }
            // HALT wakes on an enabled IE/IF source even if IME is clear.
            // Full IRQ exception entry and vectoring are separate work.
            self.halted = false;
            self.last_fetch_next = None;
        }
        let address = self.registers[15];
        let was_thumb = self.thumb;
        let fetch_width = if was_thumb { 2 } else { 4 };
        let fetch_sequential = self.last_fetch_next == Some((address, was_thumb));
        let instruction = if was_thumb {
            u32::from(bus.read16(address & !1))
        } else {
            bus.read32(address & !3)
        };
        let estimated_cycles = self.estimate_cycles(instruction);
        let data_access = self.gamepak_data_access(instruction, was_thumb, address);
        // Capture the condition before execution. Comparing the resulting PC
        // with address+2 is insufficient: a taken backward branch can land
        // exactly on the sequential address.
        let thumb_branch_taken = was_thumb
            && instruction & 0xF000 == 0xD000
            && ((instruction >> 8) & 0xF) < 0xE
            && self.condition_passed((instruction >> 8) & 0xF);
        // A taken control-flow instruction breaks the bus's sequential fetch
        // stream even when its destination happens to equal address + width.
        let redirects_pc = if was_thumb {
            let raw = instruction as u16;
            (raw & 0xF000 == 0xD000 && ((raw >> 8) & 0xF) < 0xE && thumb_branch_taken)
                || raw & 0xF800 == 0xE000
                || raw & 0xF800 == 0xF800
                || (raw & 0xFC00 == 0x4400
                    && (((raw >> 8) & 3) == 3
                        || (((raw >> 8) & 3) != 1 && ((raw & 7) | ((raw >> 4) & 8)) == 15)))
                || (raw & 0xFE00 == 0xBC00 && raw & (1 << 8) != 0)
        } else {
            let condition_passed =
                (instruction >> 28) != 0xF && self.condition_passed(instruction >> 28);
            condition_passed
                && ((instruction & 0x0E00_0000 == 0x0A00_0000)
                    || instruction & 0x0FFF_FFF0 == 0x012F_FF10
                    || (instruction & 0x0C00_0000 == 0 && (instruction >> 12) & 0xF == 15)
                    || (instruction & 0x0C00_0000 == 0x0400_0000
                        && instruction & (1 << 20) != 0
                        && (instruction >> 12) & 0xF == 15)
                    || (instruction & 0x0E00_0000 == 0x0800_0000
                        && instruction & (1 << 20) != 0
                        && instruction & (1 << 15) != 0))
        };
        let outcome = self.step(bus)?;
        // A condition-failed ARM instruction is skipped without a branch or
        // data access, so it consumes only the sequential instruction cycle.
        let cycles = if matches!(outcome, StepOutcome::ConditionFailed { .. })
            || (was_thumb
                && instruction & 0xF000 == 0xD000
                && ((instruction >> 8) & 0xF) < 0xE
                && !thumb_branch_taken)
        {
            1
        } else {
            estimated_cycles
        };
        // The base instruction estimate assumes the normal low-wait-state
        // instruction fetch. Add only the excess duration for Game Pak fetches;
        // RAM and I/O execution retain the existing instruction timing model.
        let fetch_cycles = bus
            .gamepak_access_cycles_for_width(address, fetch_sequential, fetch_width)
            .unwrap_or(fetch_width as u32 / 2);
        let base_fetch_cycles = u32::from(fetch_width / 2);
        let wait_penalty = fetch_cycles.saturating_sub(base_fetch_cycles);
        // Scalar loads/stores and supported multiple transfers also pay excess
        // Game Pak latency. The instruction estimate already budgets nominal
        // data cycles, so only excess latency is added. DMA contention remains
        // separate follow-up work.
        let data_wait_penalty = if matches!(outcome, StepOutcome::ConditionFailed { .. }) {
            0
        } else {
            data_access
                .and_then(|(data_address, width, access_count)| {
                    bus.gamepak_sequence_cycles(data_address, false, width, access_count)
                        .map(|access_cycles| access_cycles.saturating_sub(access_count as u64))
                })
                .and_then(|penalty| u32::try_from(penalty).ok())
                .unwrap_or(0)
        };
        let total_cycles = cycles
            .saturating_add(wait_penalty)
            .saturating_add(data_wait_penalty);
        self.last_fetch_next = if !redirects_pc
            && self.thumb == was_thumb
            && self.registers[15] == address.wrapping_add(u32::from(fetch_width))
        {
            Some((address.wrapping_add(u32::from(fetch_width)), was_thumb))
        } else {
            None
        };
        bus.advance_cycles(total_cycles);
        Ok((outcome, total_cycles))
    }

    /// Predicts the effective address and width for supported scalar memory
    /// instructions so WAITCNT can contribute excess data-access latency.
    /// Register-offset ARM transfers use ARM7TDMI address-generation rules.
    /// Uncommon/unpredictable encodings remain outside this estimate.
    fn gamepak_data_access(
        &self,
        instruction: u32,
        was_thumb: bool,
        address: u32,
    ) -> Option<(u32, u8, usize)> {
        if was_thumb {
            let raw = instruction as u16;
            // Thumb PUSH/POP and LDM/STM transfer registers in ascending
            // address order. Their first bus access is non-sequential.
            if raw & 0xF600 == 0xB400 {
                let count = (raw & 0x00FF).count_ones() as usize + usize::from(raw & 0x0100 != 0);
                if count == 0 {
                    return None;
                }
                let push = raw & 0xFE00 == 0xB400;
                let start = if push {
                    self.registers[13].wrapping_sub((count as u32) * 4)
                } else {
                    self.registers[13]
                };
                return Some((start, 4, count));
            }
            if raw & 0xF000 == 0xC000 {
                let count = (raw & 0x00FF).count_ones() as usize;
                if count == 0 {
                    return None;
                }
                let base = self.registers[usize::from((raw >> 8) & 7)];
                return Some((base, 4, count));
            }
            // Thumb literal load: LDR Rd, [PC, #imm].
            if raw & 0xF800 == 0x4800 {
                let base = address.wrapping_add(4) & !3;
                return Some((base.wrapping_add(u32::from(raw & 0x00FF) * 4), 4, 1));
            }
            // Register-offset transfer family (STR/STRH/STRB/LDSB/LDR/LDRH/LDRB/LDSH).
            if raw & 0xF000 == 0x5000 {
                let operation = (raw >> 9) & 7;
                let base = self.registers[usize::from((raw >> 3) & 7)];
                let offset = self.registers[usize::from((raw >> 6) & 7)];
                let effective = base.wrapping_add(offset);
                // LDSH at an odd address is executed as a signed byte load
                // on ARM7TDMI, so its external bus access is byte-wide too.
                let width = match operation {
                    0 | 4 => 4,
                    1 | 5 => 2,
                    7 if effective & 1 == 0 => 2,
                    _ => 1,
                };
                return Some((effective, width, 1));
            }
            // Immediate-offset word/byte transfer.
            if raw & 0xE000 == 0x6000 {
                let byte = raw & 0x1000 != 0;
                let scale = if byte { 1 } else { 4 };
                let base = self.registers[usize::from((raw >> 3) & 7)];
                let offset = u32::from((raw >> 6) & 0x1F) * scale;
                return Some((base.wrapping_add(offset), if byte { 1 } else { 4 }, 1));
            }
            // Immediate-offset halfword transfer.
            if raw & 0xF000 == 0x8000 {
                let base = self.registers[usize::from((raw >> 3) & 7)];
                let offset = u32::from((raw >> 6) & 0x1F) * 2;
                return Some((base.wrapping_add(offset), 2, 1));
            }
            // SP-relative word transfer.
            if raw & 0xF000 == 0x9000 {
                let base = self.registers[13];
                let offset = u32::from(raw & 0x00FF) * 4;
                return Some((base.wrapping_add(offset), 4, 1));
            }
            return None;
        }

        // SWP/SWPB issue a read and a write against the same address.
        // Count both bus phases so external Game Pak wait states are included
        // even though writes to the ROM window do not modify cartridge data.
        if instruction & 0x0FB0_0FF0 == 0x0100_0090 {
            let rn = ((instruction >> 16) & 0xF) as usize;
            let data_address = self.read_operand_register(rn, address);
            let width = if instruction & (1 << 22) != 0 { 1 } else { 4 };
            return Some((data_address, width, 2));
        }

        // ARM LDM/STM burst: the core transfers the selected registers from
        // the lowest address upward, regardless of increment/decrement mode.
        if instruction & 0x0E00_0000 == 0x0800_0000 {
            let register_list = instruction & 0xFFFF;
            let count = register_list.count_ones() as usize;
            if count == 0 {
                // Empty lists transfer PC at the sixteenth word slot; they do
                // not issue sixteen independent external data reads.
                let rn = ((instruction >> 16) & 0xF) as usize;
                let base = self.read_operand_register(rn, address);
                let increment = instruction & (1 << 23) != 0;
                let pre_index = instruction & (1 << 24) != 0;
                let start = match (increment, pre_index) {
                    (true, false) => base.wrapping_add(60),
                    (true, true) => base.wrapping_add(64),
                    (false, false) => base.wrapping_sub(60),
                    (false, true) => base.wrapping_sub(64),
                };
                return Some((start, 4, 1));
            }
            let rn = ((instruction >> 16) & 0xF) as usize;
            let base = self.read_operand_register(rn, address);
            let increment = instruction & (1 << 23) != 0;
            let pre_index = instruction & (1 << 24) != 0;
            let start = match (increment, pre_index) {
                (true, false) => base,
                (true, true) => base.wrapping_add(4),
                (false, false) => base.wrapping_sub((count as u32) * 4).wrapping_add(4),
                (false, true) => base.wrapping_sub((count as u32) * 4),
            };
            return Some((start, 4, count));
        }

        // ARM single data transfer, with either an immediate offset or a
        // register offset using an immediate barrel-shifter amount.
        if instruction & 0x0C00_0000 == 0x0400_0000 {
            let rn = ((instruction >> 16) & 0xF) as usize;
            let base = self.read_operand_register(rn, address);
            let offset = if instruction & (1 << 25) == 0 {
                instruction & 0xFFF
            } else {
                let rm_index = (instruction & 0xF) as usize;
                let rm = if rm_index == 15 {
                    address.wrapping_add(12)
                } else {
                    self.read_operand_register(rm_index, address)
                };
                let shift_type = (instruction >> 5) & 3;
                if instruction & (1 << 4) != 0 {
                    // Rs=PC is unpredictable for register-controlled shifts.
                    if (instruction >> 8) & 0xF == 15 {
                        return None;
                    }
                    // Register-specified shifts use Rs[7:0]. A zero shift
                    // leaves Rm unchanged for all four shift types.
                    let rs = self.registers[((instruction >> 8) & 0xF) as usize];
                    let amount = rs & 0xFF;
                    match shift_type {
                        0 => match amount {
                            0 => rm,
                            1..=31 => rm << amount,
                            _ => 0,
                        },
                        1 => match amount {
                            0 => rm,
                            1..=31 => rm >> amount,
                            _ => 0,
                        },
                        2 => match amount {
                            0 => rm,
                            1..=31 => ((rm as i32) >> amount) as u32,
                            _ if rm & 0x8000_0000 != 0 => u32::MAX,
                            _ => 0,
                        },
                        _ => {
                            if amount == 0 {
                                rm
                            } else {
                                rm.rotate_right(amount & 31)
                            }
                        }
                    }
                } else {
                    let amount = (instruction >> 7) & 0x1F;
                    match shift_type {
                        0 => rm.wrapping_shl(amount),
                        1 => {
                            if amount == 0 {
                                0
                            } else {
                                rm >> amount
                            }
                        }
                        2 => {
                            if amount == 0 {
                                if rm & 0x8000_0000 != 0 {
                                    u32::MAX
                                } else {
                                    0
                                }
                            } else {
                                ((rm as i32) >> amount) as u32
                            }
                        }
                        _ => {
                            if amount == 0 {
                                (u32::from(self.flags.carry) << 31) | (rm >> 1)
                            } else {
                                rm.rotate_right(amount)
                            }
                        }
                    }
                }
            };
            let adjusted = if instruction & (1 << 23) != 0 {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
            let effective = if instruction & (1 << 24) != 0 {
                adjusted
            } else {
                base
            };
            return Some((
                effective,
                if instruction & (1 << 22) != 0 { 1 } else { 4 },
                1,
            ));
        }

        // ARM halfword and signed transfers support immediate or unshifted
        // register offsets. The operation field determines byte/halfword width.
        if instruction & 0x0E00_0090 == 0x0000_0090 && (instruction >> 5) & 3 != 0 {
            let operation = (instruction >> 5) & 3;
            let rn = ((instruction >> 16) & 0xF) as usize;
            let base = self.read_operand_register(rn, address);
            let offset = if instruction & (1 << 22) != 0 {
                ((instruction >> 4) & 0xF0) | (instruction & 0xF)
            } else {
                // Register-offset halfword/signed transfers use Rm directly;
                // the encoding's unused bits must be zero.
                if instruction & 0xF00 != 0 {
                    return None;
                }
                self.read_operand_register((instruction & 0xF) as usize, address)
            };
            let adjusted = if instruction & (1 << 23) != 0 {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
            let effective = if instruction & (1 << 24) != 0 {
                adjusted
            } else {
                base
            };
            // Signed halfword loads at odd addresses execute as signed
            // byte loads on ARM7TDMI, so charge the byte-wide bus duration.
            let width = if operation == 2 || (operation == 3 && effective & 1 != 0) {
                1
            } else {
                2
            };
            return Some((effective, width, 1));
        }
        None
    }

    /// Returns ARM7TDMI's approximate internal multiply cycles (1–4),
    /// based on whether the multiplier is sign/zero-extended from 8, 16,
    /// 24, or 32 bits.
    fn multiply_internal_cycles(value: u32) -> u32 {
        Self::multiply_internal_cycles_for_extension(value, true)
    }

    /// Returns the multiplier cycles for long multiply instructions.
    /// Signed forms may terminate when the operand is sign-extended from a
    /// shorter width; unsigned forms may terminate only when it is zero-extended.
    fn multiply_internal_cycles_for_extension(value: u32, signed: bool) -> u32 {
        if value & 0xFFFF_FF00 == 0 || (signed && value & 0xFFFF_FF00 == 0xFFFF_FF00) {
            1
        } else if value & 0xFFFF_0000 == 0 || (signed && value & 0xFFFF_0000 == 0xFFFF_0000) {
            2
        } else if value & 0xFF00_0000 == 0 || (signed && value & 0xFF00_0000 == 0xFF00_0000) {
            3
        } else {
            4
        }
    }

    fn estimate_cycles(&self, instruction: u32) -> u32 {
        if self.thumb {
            let raw = instruction as u16;
            // Conditional/unconditional branches and BL may refill the pipeline.
            if raw & 0xF000 == 0xD000 && ((raw >> 8) & 0xF) < 0xE {
                return 3;
            }
            if raw & 0xF800 == 0xE000 || raw & 0xF800 == 0xF800 {
                return 3;
            }
            // The first half of Thumb BL only builds the high displacement
            // in LR; it does not redirect the PC or refill the pipeline.
            // The second half performs the branch and pays the refill cost.
            if raw & 0xF800 == 0xF000 {
                return 1;
            }
            // High-register ADD/MOV to PC and BX redirect execution and
            // refill the Thumb pipeline; do not estimate them as one-cycle ALU ops.
            if raw & 0xFC00 == 0x4400 {
                let operation = (raw >> 8) & 3;
                let destination = (raw & 7) | ((raw >> 4) & 8);
                if operation == 3 || (operation != 1 && destination == 15) {
                    return 3;
                }
            }
            // PUSH/POP and Thumb multiple transfers take one transfer
            // cycle per register, plus the instruction-fetch cycle. POP {pc}
            // also refills the pipeline.
            if raw & 0xF600 == 0xB400 {
                let registers = (raw & 0x00FF).count_ones() + u32::from(raw & 0x0100 != 0);
                let loads_pc = raw & 0xFE00 == 0xBC00 && raw & 0x0100 != 0;
                return 1 + registers + if loads_pc { 2 } else { 0 };
            }
            if raw & 0xF000 == 0xC000 {
                let registers = (raw & 0x00FF).count_ones();
                // An empty Thumb LDM/STM list transfers PC while spanning
                // sixteen words. LDM also refills the pipeline after loading PC.
                if registers == 0 {
                    return if raw & (1 << 11) != 0 { 19 } else { 17 };
                }
                return 1 + registers;
            }
            // Thumb MUL has operand-dependent internal timing on ARM7TDMI.
            // The multiplier is Rs (bits 3..5); include the instruction cycle
            // in addition to the value-dependent multiply cycles.
            if raw & 0xFC00 == 0x4000 && (raw >> 6) & 0xF == 13 {
                let rs = usize::from((raw >> 3) & 7);
                return 1 + Self::multiply_internal_cycles(self.registers[rs]);
            }
            // Ordinary single transfers include register-offset, immediate,
            // halfword, SP-relative, and literal loads.
            if raw & 0xF000 == 0x5000 {
                // Register-offset operations 0..2 store; 3..7 load.
                return if (raw >> 9) & 7 >= 3 { 3 } else { 2 };
            }
            if raw & 0xE000 == 0x6000 || raw & 0xF000 == 0x8000 || raw & 0xF000 == 0x9000 {
                let load = raw & (1 << 11) != 0;
                return if load { 3 } else { 2 };
            }
            if raw & 0xF800 == 0x4800 {
                return 3;
            }
            return 1;
        }

        // BIOS HLE routines still occupy CPU time. Estimate the cost of the
        // memory-heavy services from their arguments instead of charging every
        // SWI a single ALU cycle; peripheral timers must advance during large
        // CpuSet/CpuFastSet/RegisterRamReset operations.
        if instruction & 0x0F00_0000 == 0x0F00_0000 {
            match instruction & 0x00FF_FFFF {
                0x01 => {
                    let flags = self.registers[0];
                    let bytes = [
                        (1 << 0, 256_u32 * 1024),
                        (1 << 1, 32 * 1024),
                        (1 << 2, 1024),
                        (1 << 3, 96 * 1024),
                        (1 << 4, 1024),
                        (1 << 8, 32 * 1024),
                    ]
                    .iter()
                    .filter(|(flag, _)| flags & flag != 0)
                    .fold(0_u32, |total, (_, length)| total.saturating_add(*length));
                    if bytes != 0 {
                        return 1_u32.saturating_add(bytes.saturating_mul(2));
                    }
                }
                0x0B | 0x0C => {
                    let control = self.registers[2];
                    let fill = control & (1 << 24) != 0;
                    let count = if instruction & 0x00FF_FFFF == 0x0C {
                        (control & 0x001F_FFFF).saturating_add(7) & !7
                    } else {
                        control & 0x001F_FFFF
                    };
                    // A fill reads its source once, then writes each output
                    // element. A copy reads and writes every element. The base
                    // cycle is instruction/HLE overhead, not the first source read.
                    let per_element = if fill { 1 } else { 2 };
                    let source_read = u32::from(fill && count != 0);
                    return 1_u32.saturating_add(
                        count
                            .saturating_mul(per_element)
                            .saturating_add(source_read),
                    );
                }
                _ => {}
            }
        }

        // ARM branch and branch-with-link require a pipeline refill.
        if instruction & 0x0E00_0000 == 0x0A00_0000 {
            return 3;
        }
        // SWP/SWPB perform a read-modify-write sequence and take four
        // ARM7TDMI cycles. Match them before the halfword-transfer pattern,
        // which shares the low 0x90 encoding bits and would undercount them.
        if instruction & 0x0FB0_0FF0 == 0x0100_0090 {
            return 4;
        }
        // Single and halfword transfers use at least one data access cycle.
        if instruction & 0x0E00_0000 == 0x0800_0000 {
            let register_list = instruction & 0x0000_FFFF;
            let empty_list = register_list == 0;
            // ARM7TDMI treats an empty list as a PC transfer whose address
            // span still covers 16 registers. A load therefore also refills
            // the pipeline, just like an explicit LDM including PC.
            let registers = if empty_list {
                16
            } else {
                register_list.count_ones()
            };
            let load = instruction & (1 << 20) != 0;
            let loads_pc = load && (empty_list || register_list & (1 << 15) != 0);
            return 1 + registers + if loads_pc { 2 } else { 0 };
        }
        // Match multiply encodings before the broad halfword-transfer
        // pattern below: that pattern also matches the low opcode bits of
        // MUL/MLA and long multiply, making operand-dependent timing unreachable.
        let long_multiply = instruction & 0x0F80_00F0 == 0x0080_0090;
        if instruction & 0x0FC0_00F0 == 0x0000_0090 || long_multiply {
            let rs = ((instruction >> 8) & 0xF) as usize;
            let multiplier = self.registers[rs];
            // In long multiply encodings, bit 22 selects signed (SMULL/SMLAL)
            // versus unsigned (UMULL/UMLAL) early termination rules.
            let signed_long_multiply = !long_multiply || instruction & (1 << 22) != 0;
            let multiplier_cycles =
                Self::multiply_internal_cycles_for_extension(multiplier, signed_long_multiply);
            let accumulate = instruction & (1 << 21) != 0;
            // Long multiply writes a 64-bit result and needs one additional
            // internal cycle beyond MUL/MLA for the high result word.
            return 1 + multiplier_cycles + u32::from(accumulate) + u32::from(long_multiply);
        }
        // Data-processing instructions that write PC redirect execution
        // and refill the ARM pipeline. Exclude status-register operations,
        // multiply and SWP encodings, which share parts of this broad class
        // but have their own timing/decoding paths above.
        if instruction & 0x0C00_0000 == 0
            && (instruction >> 12) & 0xF == 15
            && instruction & 0x0FBF_0FFF != 0x010F_0000
            && instruction & 0x0FB0_FFF0 != 0x0120_F000
            && instruction & 0x0FB0_F000 != 0x0320_F000
            && instruction & 0x0FC0_00F0 != 0x0000_0090
            && instruction & 0x0F80_00F0 != 0x0080_0090
            && instruction & 0x0FB0_0FF0 != 0x0100_0090
        {
            return 3;
        }
        // BX uses the same broad data-processing encoding space and has
        // bit 4 set with bit 7 clear, but changes instruction state and
        // refills the pipeline rather than performing a register shift.
        if instruction & 0x0FFF_FFF0 == 0x012F_FF10 {
            return 3;
        }
        // Register-controlled operand shifts need an extra internal cycle
        // on ARM7TDMI. Restrict this estimate to encodings that the executor
        // recognizes as ordinary data-processing operations; special
        // encodings such as BX must retain their own timing above.
        if instruction & 0x0C00_0090 == 0x0000_0010 && (instruction >> 12) & 0xF != 15 {
            return 2;
        }
        // Halfword/signed transfers share the single-transfer major opcode
        // but must be classified first: otherwise LDRH/LDRSH are counted as
        // ordinary word/byte accesses by the broader mask below.
        if instruction & 0x0E00_0090 == 0x0000_0090 {
            let load = instruction & (1 << 20) != 0;
            let destination = (instruction >> 12) & 0xF;
            return if load && destination == 15 {
                5
            } else if load {
                3
            } else {
                2
            };
        }
        if instruction & 0x0C00_0000 == 0x0400_0000 {
            let load = instruction & (1 << 20) != 0;
            let destination = (instruction >> 12) & 0xF;
            // Loads require a data read cycle in addition to the instruction
            // cycle; a load to PC also flushes/refills the pipeline.
            return if load && destination == 15 {
                5
            } else if load {
                3
            } else {
                2
            };
        }
        1
    }

    /// Executes one ARM-state instruction from the current program counter.
    ///
    /// The CPU advances the PC by four bytes before executing the operation.
    /// This implementation is instruction-functional, not cycle-accurate.
    /// Thumb execution is supported, but exceptions, banked registers, and SPSRs
    /// are not modeled yet.
    pub fn step(&mut self, bus: &mut MemoryBus) -> Result<StepOutcome, CpuError> {
        if self.halted {
            if bus.pending_interrupts() == 0 {
                return Ok(StepOutcome::Halted);
            }
            self.halted = false;
            self.last_fetch_next = None;
        }
        // Unsupported instructions are reported to the caller rather than
        // vectored through an emulated exception. Restore the small CPU state
        // snapshot on failure so debuggers can inspect or retry the faulting
        // instruction without partially modified registers or condition flags.
        let state_before = self.clone();
        let result = self.step_inner(bus);
        if result.is_err() {
            *self = state_before;
        }
        result
    }

    fn step_inner(&mut self, bus: &mut MemoryBus) -> Result<StepOutcome, CpuError> {
        if self.thumb {
            return self.step_thumb(bus);
        }
        let address = self.registers[15] & !3;
        let instruction = bus.read32(address);
        self.registers[15] = address.wrapping_add(4);

        let condition = instruction >> 28;
        if condition == 0xF {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }
        if !self.condition_passed(condition) {
            return Ok(StepOutcome::ConditionFailed {
                address,
                instruction,
            });
        }

        if instruction & 0x0F00_0000 == 0x0F00_0000 {
            self.execute_swi(address, instruction, instruction & 0x00FF_FFFF, false, bus)?;
        } else if instruction & 0x0FBF_0FFF == 0x010F_0000 {
            self.execute_mrs(address, instruction)?;
        } else if instruction & 0x0FB0_FFF0 == 0x0120_F000
            || instruction & 0x0FB0_F000 == 0x0320_F000
        {
            self.execute_msr(address, instruction)?;
        } else if instruction & 0x0FB0_0FF0 == 0x0100_0090 {
            self.execute_swap(address, instruction, bus)?;
        } else if instruction & 0x0F80_00F0 == 0x0080_0090 {
            self.execute_long_multiply(address, instruction)?;
        } else if instruction & 0x0FFF_FFF0 == 0x012F_FF10 {
            let rm = (instruction & 0xF) as usize;
            // BX with Rm=PC is architecturally unpredictable on ARM7TDMI.
            if rm == 15 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            let target = self.registers[rm];
            self.thumb = target & 1 != 0;
            self.registers[15] = if self.thumb { target & !1 } else { target & !3 };
        } else if instruction & 0x0FC0_00F0 == 0x0000_0090 {
            self.execute_multiply(address, instruction)?;
        } else if instruction & 0x0E00_0000 == 0x0A00_0000 {
            self.execute_branch(address, instruction);
        } else if instruction & 0x0E00_0000 == 0x0800_0000 {
            self.execute_block_transfer(address, instruction, bus)?;
        } else if instruction & 0x0C00_0000 == 0x0400_0000 {
            self.execute_single_transfer(address, instruction, bus)?;
        } else if instruction & 0x0E00_0090 == 0x0000_0090 {
            self.execute_halfword_transfer(address, instruction, bus)?;
        } else if instruction & 0x0C00_0000 == 0 {
            self.execute_data_processing(address, instruction)?;
        } else {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        Ok(StepOutcome::Executed {
            address,
            instruction,
        })
    }

    fn step_thumb(&mut self, bus: &mut MemoryBus) -> Result<StepOutcome, CpuError> {
        let address = self.registers[15] & !1;
        let instruction = u32::from(bus.read16(address));
        self.registers[15] = address.wrapping_add(2);
        let raw = instruction as u16;

        if raw & 0xE000 == 0x0000 && raw & 0x1800 != 0x1800 {
            // Thumb immediate LSL/LSR/ASR (format 1).
            let kind = u32::from((raw >> 11) & 3);
            let amount = u32::from((raw >> 6) & 0x1F);
            let rs = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let (result, carry) =
                Self::shift_thumb_immediate(self.registers[rs], kind, amount, self.flags.carry);
            self.registers[rd] = result;
            self.flags.negative = result >> 31 != 0;
            self.flags.zero = result == 0;
            if let Some(carry) = carry {
                self.flags.carry = carry;
            }
        } else if raw & 0xF800 == 0x1800 {
            // Thumb ADD/SUB with a register or a three-bit immediate.
            let immediate = raw & (1 << 10) != 0;
            let subtract = raw & (1 << 9) != 0;
            let operand = usize::from((raw >> 6) & 7);
            let rs = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let right = if immediate {
                operand as u32
            } else {
                self.registers[operand]
            };
            let left = self.registers[rs];
            let (result, carry, overflow) = if subtract {
                Self::subtract(left, right, false)
            } else {
                Self::add(left, right, false)
            };
            self.registers[rd] = result;
            self.set_arithmetic_flags(result, carry, overflow);
        } else if raw & 0xE000 == 0x2000 {
            // Thumb MOV/CMP/ADD/SUB with an eight-bit immediate.
            let operation = (raw >> 11) & 3;
            let rd = usize::from((raw >> 8) & 7);
            let immediate = u32::from(raw & 0xFF);
            let left = self.registers[rd];
            match operation {
                0 => {
                    self.registers[rd] = immediate;
                    self.flags.negative = false;
                    self.flags.zero = immediate == 0;
                }
                1 => {
                    let (result, carry, overflow) = Self::subtract(left, immediate, false);
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                2 | 3 => {
                    let (result, carry, overflow) = if operation == 2 {
                        Self::add(left, immediate, false)
                    } else {
                        Self::subtract(left, immediate, false)
                    };
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                _ => unreachable!(),
            }
        } else if raw & 0xFC00 == 0x4000 {
            // The complete Thumb low-register ALU group.
            let operation = (raw >> 6) & 0xF;
            let rs = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let left = self.registers[rd];
            let right = self.registers[rs];
            let mut write_result = true;
            let (result, carry, overflow) = match operation {
                0 => (left & right, None, None), // AND
                1 => (left ^ right, None, None), // EOR
                2..=4 => {
                    let (value, carry) =
                        Self::shift_register(left, u32::from(operation - 2), right & 0xFF);
                    (value, carry, None)
                }
                5 => {
                    let (value, carry, overflow) = Self::add(left, right, self.flags.carry);
                    (value, Some(carry), Some(overflow))
                }
                6 => {
                    let (value, carry, overflow) = Self::subtract(left, right, !self.flags.carry);
                    (value, Some(carry), Some(overflow))
                }
                7 => {
                    let (value, carry) = Self::shift_register(left, 3, right & 0xFF);
                    (value, carry, None)
                }
                8 => {
                    write_result = false;
                    (left & right, None, None)
                } // TST
                9 => {
                    let (value, carry, overflow) = Self::subtract(0, right, false);
                    (value, Some(carry), Some(overflow)) // NEG
                }
                10 => {
                    write_result = false;
                    let (value, carry, overflow) = Self::subtract(left, right, false);
                    (value, Some(carry), Some(overflow))
                }
                11 => {
                    write_result = false;
                    let (value, carry, overflow) = Self::add(left, right, false);
                    (value, Some(carry), Some(overflow))
                }
                12 => (left | right, None, None),             // ORR
                13 => (left.wrapping_mul(right), None, None), // MUL
                14 => (left & !right, None, None),            // BIC
                15 => (!right, None, None),                   // MVN
                _ => unreachable!(),
            };
            if write_result {
                self.registers[rd] = result;
            }
            self.flags.negative = result >> 31 != 0;
            self.flags.zero = result == 0;
            if let Some(carry) = carry {
                self.flags.carry = carry;
            }
            if let Some(overflow) = overflow {
                self.flags.overflow = overflow;
            }
        } else if raw & 0xF800 == 0xF000 {
            // First half of Thumb BL: form the signed high displacement in LR.
            let high_displacement = ((u32::from(raw & 0x07FF) << 21) as i32 >> 9) as u32;
            self.registers[14] = address.wrapping_add(4).wrapping_add(high_displacement);
        } else if raw & 0xF800 == 0xF800 {
            // Second half of Thumb BL: branch relative to the saved high part.
            let low_displacement = u32::from(raw & 0x07FF) << 1;
            let target = self.registers[14].wrapping_add(low_displacement);
            self.registers[14] = address.wrapping_add(2) | 1;
            self.registers[15] = target & !1;
        } else if raw & 0xF800 == 0xE000 {
            let displacement = (((u32::from(raw & 0x07FF)) << 21) as i32 >> 20) as u32;
            self.registers[15] = address.wrapping_add(4).wrapping_add(displacement) & !1;
        } else if raw & 0xFF00 == 0xDF00 {
            self.execute_swi(address, instruction, u32::from(raw & 0x00FF), true, bus)?;
        } else if raw & 0xF000 == 0xD000 {
            let condition = u32::from((raw >> 8) & 0xF);
            if condition >= 0xE {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            if self.condition_passed(condition) {
                let displacement = ((u32::from(raw & 0xFF) << 24) as i32 >> 23) as u32;
                self.registers[15] = address.wrapping_add(4).wrapping_add(displacement) & !1;
            }
        } else if raw & 0xFC00 == 0x4400 {
            let operation = (raw >> 8) & 3;
            let rd = usize::from((raw & 7) | ((raw >> 4) & 8));
            let rs = usize::from((raw >> 3) & 0xF);
            // BX with PC as Rm is architecturally unpredictable on ARM7TDMI.
            // Other high-register operations may legitimately read PC.
            if operation == 3 && rs == 15 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            let right = self.read_thumb_register(rs, address);
            let left = self.read_thumb_register(rd, address);
            match operation {
                0 => {
                    let result = left.wrapping_add(right);
                    if rd == 15 {
                        self.registers[15] = result & !1;
                    } else {
                        self.registers[rd] = result;
                    }
                }
                1 => {
                    let (result, carry, overflow) = Self::subtract(left, right, false);
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                2 => {
                    if rd == 15 {
                        self.registers[15] = right & !1;
                    } else {
                        self.registers[rd] = right;
                    }
                }
                3 => {
                    self.thumb = right & 1 != 0;
                    self.registers[15] = if self.thumb { right & !1 } else { right & !3 };
                }
                _ => unreachable!(),
            }
        } else if raw & 0xF800 == 0x4800 {
            let rd = usize::from((raw >> 8) & 7);
            let offset = u32::from(raw & 0xFF) * 4;
            let base = address.wrapping_add(4) & !3;
            self.registers[rd] = bus.read32(base.wrapping_add(offset));
        } else if raw & 0xF000 == 0x5000 {
            let operation = (raw >> 9) & 7;
            let ro = usize::from((raw >> 6) & 7);
            let rb = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let effective = self.registers[rb].wrapping_add(self.registers[ro]);
            match operation {
                0 => bus.write32(effective, self.registers[rd]),
                1 => bus.write16(effective, self.registers[rd] as u16),
                2 => bus.write8(effective, self.registers[rd] as u8),
                3 => {
                    self.registers[rd] = bus.read8(effective) as i8 as i32 as u32;
                }
                4 => {
                    // Thumb LDR uses the same ARM7TDMI unaligned-word
                    // rotation behavior as ARM-state LDR.
                    self.registers[rd] = bus.read32(effective).rotate_right((effective & 3) * 8);
                }
                5 => {
                    // ARM7TDMI LDRH rotates the aligned halfword by one byte
                    // when the effective address is odd.
                    let value = bus.read16(effective);
                    self.registers[rd] = u32::from(if effective & 1 != 0 {
                        value.rotate_right(8)
                    } else {
                        value
                    });
                }
                6 => self.registers[rd] = u32::from(bus.read8(effective)),
                7 => {
                    self.registers[rd] = if effective & 1 != 0 {
                        bus.read8(effective) as i8 as i32 as u32
                    } else {
                        bus.read16(effective) as i16 as i32 as u32
                    };
                }
                _ => unreachable!(),
            }
        } else if raw & 0xE000 == 0x6000 {
            let byte_transfer = raw & (1 << 12) != 0;
            let load = raw & (1 << 11) != 0;
            let offset = u32::from((raw >> 6) & 0x1F);
            let rb = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let scaled_offset = if byte_transfer { offset } else { offset * 4 };
            let effective = self.registers[rb].wrapping_add(scaled_offset);
            if load {
                self.registers[rd] = if byte_transfer {
                    u32::from(bus.read8(effective))
                } else {
                    // The bus returns an aligned word; rotate it to model the
                    // ARM7TDMI result for an unaligned Thumb LDR address.
                    bus.read32(effective).rotate_right((effective & 3) * 8)
                };
            } else if byte_transfer {
                bus.write8(effective, self.registers[rd] as u8);
            } else {
                bus.write32(effective, self.registers[rd]);
            }
        } else if raw & 0xF000 == 0x8000 {
            let load = raw & (1 << 11) != 0;
            let offset = u32::from((raw >> 6) & 0x1F) * 2;
            let rb = usize::from((raw >> 3) & 7);
            let rd = usize::from(raw & 7);
            let effective = self.registers[rb].wrapping_add(offset);
            if load {
                // Unsigned Thumb LDRH rotates the aligned halfword by 8 bits
                // when the effective address is odd, just like ARM-state LDRH.
                let value = bus.read16(effective);
                self.registers[rd] = u32::from(if effective & 1 != 0 {
                    value.rotate_right(8)
                } else {
                    value
                });
            } else {
                bus.write16(effective, self.registers[rd] as u16);
            }
        } else if raw & 0xF000 == 0xA000 {
            // ADD Rd, PC/SP, #imm: the PC base is word-aligned address+4.
            let rd = usize::from((raw >> 8) & 7);
            let use_sp = raw & (1 << 11) != 0;
            let offset = u32::from(raw & 0xFF) * 4;
            let base = if use_sp {
                self.registers[13]
            } else {
                address.wrapping_add(4) & !3
            };
            self.registers[rd] = base.wrapping_add(offset);
        } else if raw & 0xF000 == 0x9000 {
            let load = raw & (1 << 11) != 0;
            let rd = usize::from((raw >> 8) & 7);
            let offset = u32::from(raw & 0xFF) * 4;
            let effective = self.registers[13].wrapping_add(offset);
            if load {
                // This is an ordinary Thumb word load: preserve ARM7TDMI's
                // rotate-right behavior even if software has made SP unaligned.
                self.registers[rd] = bus.read32(effective).rotate_right((effective & 3) * 8);
            } else {
                bus.write32(effective, self.registers[rd]);
            }
        } else if raw & 0xFE00 == 0xB400 || raw & 0xFE00 == 0xBC00 {
            let pop = raw & 0x0800 != 0;
            let extra_register = raw & (1 << 8) != 0;
            let register_list = u32::from(raw & 0xFF);
            let count = register_list.count_ones() + u32::from(extra_register);
            if count == 0 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }

            let old_sp = self.registers[13];
            if pop {
                let mut transfer_address = old_sp;
                for register in 0..8 {
                    if register_list & (1 << register) != 0 {
                        self.registers[register] = bus.read32(transfer_address);
                        transfer_address = transfer_address.wrapping_add(4);
                    }
                }
                if extra_register {
                    let value = bus.read32(transfer_address);
                    self.registers[15] = value & !1;
                }
                self.registers[13] = old_sp.wrapping_add(count * 4);
            } else {
                let new_sp = old_sp.wrapping_sub(count * 4);
                let mut transfer_address = new_sp;
                for register in 0..8 {
                    if register_list & (1 << register) != 0 {
                        bus.write32(transfer_address, self.registers[register]);
                        transfer_address = transfer_address.wrapping_add(4);
                    }
                }
                if extra_register {
                    bus.write32(transfer_address, self.registers[14]);
                }
                self.registers[13] = new_sp;
            }
        } else if raw & 0xFF00 == 0xB000 {
            let subtract = raw & (1 << 7) != 0;
            let offset = u32::from(raw & 0x7F) * 4;
            self.registers[13] = if subtract {
                self.registers[13].wrapping_sub(offset)
            } else {
                self.registers[13].wrapping_add(offset)
            };
        } else if raw & 0xF000 == 0xC000 {
            let load = raw & (1 << 11) != 0;
            let rb = usize::from((raw >> 8) & 7);
            let register_list = u32::from(raw & 0xFF);
            // ARM7TDMI's empty Thumb LDMIA/STMIA list is a special case:
            // it transfers PC while advancing the base by sixteen words.
            let empty_list = register_list == 0;
            let effective_list = if empty_list { 1 << 15 } else { register_list };
            let count = if empty_list {
                16
            } else {
                register_list.count_ones()
            };

            let base = self.registers[rb];
            let mut transfer_address = base;
            for register in 0..16 {
                if effective_list & (1 << register) == 0 {
                    continue;
                }
                if load {
                    let value = bus.read32(transfer_address);
                    if register == 15 {
                        // LDM does not switch instruction set; the loaded PC
                        // is halfword-aligned because this transfer is Thumb.
                        self.registers[15] = value & !1;
                    } else {
                        self.registers[register] = value;
                    }
                } else {
                    // Thumb STMIA has a defined special case when the base is
                    // included in the register list: if it is the lowest
                    // listed register, the stored value is the updated base;
                    // otherwise the original base is stored.
                    let value = if empty_list {
                        // PC reads as the Thumb pipeline-visible address + 4.
                        address.wrapping_add(4)
                    } else if register == rb {
                        let lowest_register = register_list.trailing_zeros();
                        if lowest_register == rb as u32 {
                            base.wrapping_add(count * 4)
                        } else {
                            base
                        }
                    } else {
                        self.registers[register]
                    };
                    bus.write32(transfer_address, value);
                }
                transfer_address = transfer_address.wrapping_add(4);
            }
            // For LDMIA with write-back and the base in the list, the
            // ARM7TDMI keeps the loaded base only when it is the lowest
            // register transferred. Otherwise write-back takes precedence.
            // STMIA always writes back, and an empty list has its special
            // sixteen-word span regardless of the synthetic PC transfer.
            let loaded_base_wins = load
                && !empty_list
                && register_list & (1 << rb) != 0
                && register_list.trailing_zeros() == rb as u32;
            if empty_list || !load || register_list & (1 << rb) == 0 || !loaded_base_wins {
                self.registers[rb] = base.wrapping_add(count * 4);
            }
        } else if raw & 0xE000 == 0x2000 {
            let operation = (raw >> 11) & 3;
            let rd = usize::from((raw >> 8) & 7);
            let immediate = u32::from(raw & 0xFF);
            let left = self.registers[rd];
            match operation {
                0 => {
                    self.registers[rd] = immediate;
                    self.set_logic_flags(immediate, None);
                }
                1 => {
                    let (result, carry, overflow) = Self::subtract(left, immediate, false);
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                2 => {
                    let (result, carry, overflow) = Self::add(left, immediate, false);
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                3 => {
                    let (result, carry, overflow) = Self::subtract(left, immediate, false);
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                _ => unreachable!(),
            }
        } else if raw & 0xE000 == 0x0000 {
            let operation = (raw >> 11) & 3;
            if operation != 3 {
                let immediate = u32::from((raw >> 6) & 0x1F);
                let rs = usize::from((raw >> 3) & 7);
                let rd = usize::from(raw & 7);
                let value = self.registers[rs];
                let carry_in = self.flags.carry;
                let (result, carry) =
                    Self::shift_thumb_immediate(value, u32::from(operation), immediate, carry_in);
                self.registers[rd] = result;
                self.set_logic_flags(result, carry);
            } else {
                let immediate_operand = raw & (1 << 10) != 0;
                let subtract = raw & (1 << 9) != 0;
                let operand = if immediate_operand {
                    u32::from((raw >> 6) & 7)
                } else {
                    self.registers[usize::from((raw >> 6) & 7)]
                };
                let rs = usize::from((raw >> 3) & 7);
                let rd = usize::from(raw & 7);
                let left = self.registers[rs];
                let (result, carry, overflow) = if subtract {
                    Self::subtract(left, operand, false)
                } else {
                    Self::add(left, operand, false)
                };
                self.registers[rd] = result;
                self.set_arithmetic_flags(result, carry, overflow);
            }
        } else if raw & 0xFC00 == 0x4000 {
            let operation = (raw >> 6) & 0xF;
            let rd = usize::from(raw & 7);
            let rs = usize::from((raw >> 3) & 7);
            let left = self.registers[rd];
            let right = self.registers[rs];
            match operation {
                0 => {
                    let result = left & right;
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                1 => {
                    let result = left ^ right;
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                2 | 3 | 4 | 7 => {
                    // Register-controlled Thumb shifts use the low byte of Rs.
                    let amount = right & 0xFF;
                    let (result, carry) = if amount == 0 {
                        // A zero register-specified shift preserves the value
                        // and C flag, but the instruction still updates N and Z.
                        (left, None)
                    } else {
                        match operation {
                            2 => match amount {
                                1..=31 => (left << amount, Some((left >> (32 - amount)) & 1 != 0)),
                                32 => (0, Some(left & 1 != 0)),
                                _ => (0, Some(false)),
                            },
                            3 => match amount {
                                1..=31 => (left >> amount, Some((left >> (amount - 1)) & 1 != 0)),
                                32 => (0, Some(left >> 31 != 0)),
                                _ => (0, Some(false)),
                            },
                            4 => {
                                let sign = left >> 31 != 0;
                                if amount >= 32 {
                                    (if sign { u32::MAX } else { 0 }, Some(sign))
                                } else {
                                    (
                                        ((left as i32) >> amount) as u32,
                                        Some((left >> (amount - 1)) & 1 != 0),
                                    )
                                }
                            }
                            7 => {
                                let rotation = amount & 31;
                                if rotation == 0 {
                                    (left, Some(left >> 31 != 0))
                                } else {
                                    let result = left.rotate_right(rotation);
                                    (result, Some(result >> 31 != 0))
                                }
                            }
                            _ => unreachable!(),
                        }
                    };
                    self.registers[rd] = result;
                    self.set_logic_flags(result, carry);
                }
                5 => {
                    let (result, carry, overflow) = Self::add(left, right, self.flags.carry);
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                6 => {
                    let borrow_in = !self.flags.carry;
                    let (result, carry, overflow) = Self::subtract(left, right, borrow_in);
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                8 => {
                    self.set_logic_flags(left & right, None);
                }
                9 => {
                    let (result, carry, overflow) = Self::subtract(0, right, false);
                    self.registers[rd] = result;
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                10 => {
                    let (result, carry, overflow) = Self::subtract(left, right, false);
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                11 => {
                    let (result, carry, overflow) = Self::add(left, right, false);
                    self.set_arithmetic_flags(result, carry, overflow);
                }
                12 => {
                    let result = left | right;
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                13 => {
                    let result = left.wrapping_mul(right);
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                14 => {
                    let result = left & !right;
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                15 => {
                    let result = !right;
                    self.registers[rd] = result;
                    self.set_logic_flags(result, None);
                }
                _ => {
                    return Err(CpuError::UnsupportedInstruction {
                        address,
                        instruction,
                    });
                }
            }
        } else {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        Ok(StepOutcome::Executed {
            address,
            instruction,
        })
    }

    fn read_thumb_register(&self, index: usize, address: u32) -> u32 {
        if index == 15 {
            // High-register ALU operations and BX observe PC+4, including bit 1.
            // Literal loads align their own PC-relative base separately.
            address.wrapping_add(4)
        } else {
            self.registers[index]
        }
    }

    fn set_logic_flags(&mut self, result: u32, carry: Option<bool>) {
        self.flags.negative = result >> 31 != 0;
        self.flags.zero = result == 0;
        if let Some(carry) = carry {
            self.flags.carry = carry;
        }
    }

    fn set_arithmetic_flags(&mut self, result: u32, carry: bool, overflow: bool) {
        self.flags.negative = result >> 31 != 0;
        self.flags.zero = result == 0;
        self.flags.carry = carry;
        self.flags.overflow = overflow;
    }

    fn condition_passed(&self, condition: u32) -> bool {
        let f = self.flags;
        match condition {
            0x0 => f.zero,
            0x1 => !f.zero,
            0x2 => f.carry,
            0x3 => !f.carry,
            0x4 => f.negative,
            0x5 => !f.negative,
            0x6 => f.overflow,
            0x7 => !f.overflow,
            0x8 => f.carry && !f.zero,
            0x9 => !f.carry || f.zero,
            0xA => f.negative == f.overflow,
            0xB => f.negative != f.overflow,
            0xC => !f.zero && f.negative == f.overflow,
            0xD => f.zero || f.negative != f.overflow,
            0xE => true,
            _ => false,
        }
    }

    fn read_operand_register(&self, index: usize, instruction_address: u32) -> u32 {
        if index == 15 {
            instruction_address.wrapping_add(8)
        } else {
            self.registers[index]
        }
    }

    fn execute_branch(&mut self, address: u32, instruction: u32) {
        if instruction & (1 << 24) != 0 {
            self.registers[14] = address.wrapping_add(4);
        }
        let displacement = (((instruction & 0x00FF_FFFF) << 8) as i32 >> 6) as u32;
        self.registers[15] = address.wrapping_add(8).wrapping_add(displacement) & !3;
    }

    /// Writes the modeled CPSR condition flags with MSR CPSR_f.
    /// Control fields and SPSR writes require exception-mode state and remain unsupported.
    fn execute_msr(&mut self, address: u32, instruction: u32) -> Result<(), CpuError> {
        let saved_status = instruction & (1 << 22) != 0;
        let field_mask = (instruction >> 16) & 0xF;
        let immediate = instruction & (1 << 25) != 0;

        if saved_status || field_mask & !0x8 != 0 {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }
        if field_mask == 0 {
            return Ok(());
        }

        let value = if immediate {
            (instruction & 0xFF).rotate_right(((instruction >> 8) & 0xF) * 2)
        } else {
            let rm = (instruction & 0xF) as usize;
            if rm == 15 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            self.registers[rm]
        };

        self.flags.negative = value >> 31 != 0;
        self.flags.zero = value & (1 << 30) != 0;
        self.flags.carry = value & (1 << 29) != 0;
        self.flags.overflow = value & (1 << 28) != 0;
        Ok(())
    }

    /// Reads the modeled CPSR fields with ARM-state MRS.
    /// Mode, interrupt masks and other unmodeled CPSR bits read as zero.
    fn execute_mrs(&mut self, address: u32, instruction: u32) -> Result<(), CpuError> {
        let rd = ((instruction >> 12) & 0xF) as usize;
        let saved_status = instruction & (1 << 22) != 0;
        if saved_status || rd == 15 {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let mut cpsr = 0_u32;
        cpsr |= u32::from(self.flags.negative) << 31;
        cpsr |= u32::from(self.flags.zero) << 30;
        cpsr |= u32::from(self.flags.carry) << 29;
        cpsr |= u32::from(self.flags.overflow) << 28;
        cpsr |= u32::from(self.thumb) << 5;
        self.registers[rd] = cpsr;
        Ok(())
    }

    /// Handles a small high-value subset of GBA BIOS software interrupts in HLE.
    ///
    /// This deliberately implements common arithmetic and memory-copy services
    /// without pretending to provide BIOS exception banking or a full BIOS ROM.
    fn execute_swi(
        &mut self,
        address: u32,
        instruction: u32,
        swi: u32,
        thumb: bool,
        bus: &mut MemoryBus,
    ) -> Result<(), CpuError> {
        match swi {
            // Halt: stop fetching instructions until an enabled interrupt is requested.
            0x02 => {
                self.bios_wait_mask = None;
                self.halted = true;
            }
            // IntrWait: R0 requests clearing old IF flags; R1 selects sources.
            0x04 | 0x05 => {
                let (discard_old, mask) = if swi == 0x05 {
                    (true, 1_u16)
                } else {
                    (self.registers[0] != 0, self.registers[1] as u16 & 0x3FFF)
                };
                if discard_old {
                    bus.acknowledge_interrupts(mask);
                }
                let matched = bus.pending_interrupts() & mask;
                if matched != 0 {
                    bus.acknowledge_interrupts(matched);
                    self.bios_wait_mask = None;
                    self.halted = false;
                } else {
                    self.bios_wait_mask = Some(mask);
                    self.halted = true;
                }
            }
            // RegisterRamReset: clear the selected memory regions by BIOS flag.
            0x01 => {
                let flags = self.registers[0];
                let regions: &[(u32, u32, u32)] = &[
                    (1 << 0, 0x0200_0000, 256 * 1024),
                    (1 << 1, 0x0300_0000, 32 * 1024),
                    (1 << 2, 0x0500_0000, 1024),
                    (1 << 3, 0x0600_0000, 96 * 1024),
                    (1 << 4, 0x0700_0000, 1024),
                    (1 << 8, 0x0E00_0000, 32 * 1024),
                ];
                for &(flag, base, length) in regions {
                    if flags & flag != 0 {
                        for offset in 0..length {
                            bus.write8(base.wrapping_add(offset), 0);
                        }
                    }
                }
            }
            // Div: signed R0 / R1 -> quotient R0, remainder R1, |quotient| R3.
            0x06 | 0x07 => {
                if swi == 0x07 {
                    self.registers.swap(0, 1);
                }
                let numerator = self.registers[0] as i32;
                let denominator = self.registers[1] as i32;
                if denominator == 0 {
                    // BIOS division by zero yields a sign-dependent sentinel,
                    // preserves the numerator as remainder, and reports |quotient|.
                    let quotient = if numerator < 0 { 1 } else { -1 };
                    self.registers[0] = quotient as u32;
                    self.registers[1] = numerator as u32;
                    self.registers[3] = 1;
                } else {
                    let quotient = numerator.checked_div(denominator).unwrap_or(i32::MIN);
                    let remainder = numerator.checked_rem(denominator).unwrap_or(0);
                    self.registers[0] = quotient as u32;
                    self.registers[1] = remainder as u32;
                    self.registers[3] = quotient.wrapping_abs() as u32;
                }
            }
            // BitUnPack: unpack packed source fields and add the configured data offset.
            0x10 => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let info = self.registers[2];
                let source_len = usize::from(bus.read16(info));
                let source_width = bus.read8(info.wrapping_add(2));
                let destination_width = bus.read8(info.wrapping_add(3));
                let offset_word = bus.read32(info.wrapping_add(4));
                if source_width == 0
                    || source_width > 8
                    || destination_width == 0
                    || destination_width > 32
                    || destination_width < source_width
                {
                    return Err(CpuError::UnsupportedInstruction {
                        address,
                        instruction,
                    });
                }
                let source_bits = source_len.saturating_mul(8);
                let element_count = source_bits / usize::from(source_width);
                let output_bits = element_count.saturating_mul(usize::from(destination_width));
                let word_count = output_bits.saturating_add(31) / 32;
                let mut words = vec![0_u32; word_count];
                let source_mask = (1_u32 << source_width) - 1;
                let destination_mask = if destination_width == 32 {
                    u32::MAX
                } else {
                    (1_u32 << destination_width) - 1
                };
                let data_offset = offset_word & 0x7FFF_FFFF;
                for index in 0..element_count {
                    let bit = index * usize::from(source_width);
                    let mut packed = 0_u32;
                    for source_bit in 0..usize::from(source_width) {
                        let input_bit = bit + source_bit;
                        let byte = bus.read8(source.wrapping_add((input_bit / 8) as u32));
                        packed |= u32::from((byte >> (input_bit % 8)) & 1) << source_bit;
                    }
                    let mut value = packed & source_mask;
                    // Bit 31 of data_offset requests that zero-valued inputs remain zero.
                    if value != 0 || offset_word & 0x8000_0000 == 0 {
                        value = value.wrapping_add(data_offset);
                    }
                    value &= destination_mask;
                    let output_bit = index * usize::from(destination_width);
                    let word_index = output_bit / 32;
                    let shift = output_bit % 32;
                    let shifted = u64::from(value) << shift;
                    words[word_index] |= shifted as u32;
                    if shift + usize::from(destination_width) > 32 {
                        words[word_index + 1] |= (shifted >> 32) as u32;
                    }
                }
                for (index, word) in words.into_iter().enumerate() {
                    bus.write32(destination.wrapping_add((index as u32) * 4), word);
                }
            }
            // GetBiosChecksum reports the fixed checksum of the retail GBA BIOS.
            // This is a compatibility value; it does not claim to emulate BIOS ROM.
            0x0D => {
                self.registers[0] = 0xBAAE_187F;
            }
            // ArcTan: interpret R0 as signed 1.14 tangent and return a 3.12 angle.
            0x09 => {
                let tangent = (self.registers[0] as i16) as f64 / 16_384.0;
                self.registers[0] = (tangent.atan() * 4_096.0).round() as i32 as u32;
            }
            // ArcTan2: signed 1.14 Y (R1) and X (R0), result in signed 3.12 radians.
            0x0A => {
                let x = (self.registers[0] as i16) as f64;
                let y = (self.registers[1] as i16) as f64;
                let angle = if x == 0.0 && y == 0.0 {
                    0.0
                } else {
                    y.atan2(x)
                };
                self.registers[0] = (angle * 4_096.0).round() as i32 as u32;
            }
            // BgAffineSet: build affine BG matrices and fixed-point reference points.
            0x0E => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let count = self.registers[2].min(0x1000);
                for index in 0..count {
                    let src = source.wrapping_add(index * 20);
                    let dst = destination.wrapping_add(index * 16);
                    let tex_x = bus.read32(src) as i32;
                    let tex_y = bus.read32(src.wrapping_add(4)) as i32;
                    let screen_x = bus.read16(src.wrapping_add(8)) as i16 as i32;
                    let screen_y = bus.read16(src.wrapping_add(10)) as i16 as i32;
                    let scale_x = bus.read16(src.wrapping_add(12)) as i16 as f64 / 256.0;
                    let scale_y = bus.read16(src.wrapping_add(14)) as i16 as f64 / 256.0;
                    let theta = bus.read16(src.wrapping_add(16));
                    let angle = f64::from(theta) * (std::f64::consts::TAU / 65_536.0);
                    let sin = angle.sin();
                    let cos = angle.cos();
                    let pa = (cos * scale_x * 256.0).round() as i16;
                    let pb = (-sin * scale_x * 256.0).round() as i16;
                    let pc = (sin * scale_y * 256.0).round() as i16;
                    let pd = (cos * scale_y * 256.0).round() as i16;
                    bus.write16(dst, pa as u16);
                    bus.write16(dst.wrapping_add(2), pb as u16);
                    bus.write16(dst.wrapping_add(4), pc as u16);
                    bus.write16(dst.wrapping_add(6), pd as u16);
                    let ref_x = tex_x.wrapping_sub(pa as i32 * screen_x + pb as i32 * screen_y);
                    let ref_y = tex_y.wrapping_sub(pc as i32 * screen_x + pd as i32 * screen_y);
                    bus.write32(dst.wrapping_add(8), ref_x as u32);
                    bus.write32(dst.wrapping_add(12), ref_y as u32);
                }
            }
            // ObjAffineSet: write each 2x2 OBJ matrix using the caller's byte stride.
            0x0F => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let count = self.registers[2].min(0x1000);
                let stride = self.registers[3].max(2);
                for index in 0..count {
                    let src = source.wrapping_add(index * 8);
                    let dst = destination.wrapping_add(index * 2);
                    let scale_x = bus.read16(src) as i16 as f64 / 256.0;
                    let scale_y = bus.read16(src.wrapping_add(2)) as i16 as f64 / 256.0;
                    let theta = bus.read16(src.wrapping_add(4));
                    let angle = f64::from(theta) * (std::f64::consts::TAU / 65_536.0);
                    let sin = angle.sin();
                    let cos = angle.cos();
                    let matrix = [
                        (cos * scale_x * 256.0).round() as i16,
                        (-sin * scale_x * 256.0).round() as i16,
                        (sin * scale_y * 256.0).round() as i16,
                        (cos * scale_y * 256.0).round() as i16,
                    ];
                    for (slot, coefficient) in matrix.into_iter().enumerate() {
                        bus.write16(
                            dst.wrapping_add((slot as u32).wrapping_mul(stride)),
                            coefficient as u16,
                        );
                    }
                }
            }
            // Sqrt: unsigned integer square root.
            0x08 => {
                let value = u64::from(self.registers[0]);
                let mut root = value;
                let mut next = (root + 1) >> 1;
                while next < root {
                    root = next;
                    next = (root + value / root) >> 1;
                }
                self.registers[0] = root as u32;
            }
            // CpuSet: 16-bit or 32-bit copy/fill. Source bit 24 selects fill.
            0x0B => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let control = self.registers[2];
                let count = control & 0x001F_FFFF;
                let fill = control & (1 << 24) != 0;
                let word = control & (1 << 26) != 0;
                if word {
                    let mut value = 0;
                    for index in 0..count {
                        if !fill || index == 0 {
                            value = bus.read32(source.wrapping_add(index * 4) & !3);
                        }
                        bus.write32(destination.wrapping_add(index * 4) & !3, value);
                    }
                } else {
                    let mut value = 0;
                    for index in 0..count {
                        if !fill || index == 0 {
                            value = bus.read16(source.wrapping_add(index * 2) & !1);
                        }
                        bus.write16(destination.wrapping_add(index * 2) & !1, value);
                    }
                }
            }
            // Differential filters restore byte/halfword delta-compressed assets.
            0x16 | 0x17 => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let wide = swi == 0x17;
                let header = bus.read32(source);
                let expected_tag = if wide { 0x81 } else { 0x80 };
                if header as u8 != expected_tag {
                    return Err(CpuError::UnsupportedInstruction {
                        address,
                        instruction,
                    });
                }
                let output_size = ((header >> 8) & 0x00FF_FFFF) as usize;
                let mut cursor = source.wrapping_add(4);
                if wide {
                    let output_size = output_size & !1;
                    if output_size != 0 {
                        let mut accumulated = bus.read16(cursor);
                        cursor = cursor.wrapping_add(2);
                        let mut written = 0_usize;
                        while written < output_size {
                            bus.write16(destination.wrapping_add(written as u32), accumulated);
                            written += 2;
                            if written < output_size {
                                accumulated = accumulated.wrapping_add(bus.read16(cursor));
                                cursor = cursor.wrapping_add(2);
                            }
                        }
                    }
                } else if output_size != 0 {
                    let mut accumulated = bus.read8(cursor);
                    cursor = cursor.wrapping_add(1);
                    for index in 0..output_size {
                        bus.write8(destination.wrapping_add(index as u32), accumulated);
                        if index + 1 < output_size {
                            accumulated = accumulated.wrapping_add(bus.read8(cursor));
                            cursor = cursor.wrapping_add(1);
                        }
                    }
                }
            }
            // LZ77, Huffman, and run-length BIOS decompressors share the same
            // bounded output path; VRAM packing remains specific to the caller.
            0x11 | 0x12 | 0x13 | 0x14 | 0x15 => {
                let source = self.registers[0];
                let destination = self.registers[1];
                let output = if swi == 0x13 {
                    Self::huffman_decompress_bios_data(address, instruction, source, bus)?
                } else {
                    Self::decompress_bios_data(address, instruction, swi, source, bus)?
                };
                let vram = matches!(swi, 0x12 | 0x15);
                if vram {
                    for (index, chunk) in output.chunks(2).enumerate() {
                        let low = chunk[0];
                        let high = chunk.get(1).copied().unwrap_or(0);
                        bus.write16(
                            destination.wrapping_add((index as u32) * 2) & !1,
                            u16::from_le_bytes([low, high]),
                        );
                    }
                } else {
                    for (index, byte) in output.into_iter().enumerate() {
                        bus.write8(destination.wrapping_add(index as u32), byte);
                    }
                }
            }
            // CpuFastSet copies/fills words in groups of eight; count rounds up.
            0x0C => {
                let source = self.registers[0] & !3;
                let destination = self.registers[1] & !3;
                let control = self.registers[2];
                let count = (control & 0x001F_FFFF).saturating_add(7) & !7;
                let fill = control & (1 << 24) != 0;
                let mut value = 0;
                for index in 0..count {
                    if !fill || index == 0 {
                        value = bus.read32(source.wrapping_add(index * 4));
                    }
                    bus.write32(destination.wrapping_add(index * 4), value);
                }
            }
            _ => {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                })
            }
        }
        // HLE returns directly to the instruction following SWI. The real BIOS
        // uses SVC mode and SPSR banking; that exception machinery is not modeled.
        if thumb {
            self.registers[15] = address.wrapping_add(2) & !1;
        } else {
            self.registers[15] = address.wrapping_add(4) & !3;
        }
        Ok(())
    }

    /// Decodes the BIOS Huffman stream (SWI 0x13), supporting 4-bit and 8-bit symbols.
    ///
    /// The tree stores leaf symbols in child slots; payload bits are consumed most-significant
    /// bit first from little-endian 32-bit words, as required by the GBA BIOS format.
    fn huffman_decompress_bios_data(
        address: u32,
        instruction: u32,
        source: u32,
        bus: &MemoryBus,
    ) -> Result<Vec<u8>, CpuError> {
        let header = bus.read32(source);
        let format = (header & 0xFF) as u8;
        let symbol_bits = match format {
            0x20 => 8_usize,
            0x24 => 4_usize,
            _ => {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                })
            }
        };
        let output_size = ((header >> 8) & 0x00FF_FFFF) as usize;
        if output_size == 0 {
            return Ok(Vec::new());
        }
        let symbol_count = output_size.saturating_mul(8 / symbol_bits);
        let tree_size = usize::from(bus.read8(source.wrapping_add(4))).saturating_add(1) * 2;
        let tree_start = source.wrapping_add(5);
        let payload_start = source
            .wrapping_add(5)
            .wrapping_add(tree_size as u32)
            .wrapping_add(3)
            & !3;
        let mut output = Vec::with_capacity(output_size);
        let mut payload_word_address = payload_start;
        let mut payload_word = bus.read32(payload_word_address);
        let mut bits_remaining = 32_u8;

        for symbol_index in 0..symbol_count {
            let mut node_address = tree_start;
            let mut decoded = false;
            for _ in 0..=tree_size / 2 {
                let node = bus.read8(node_address);
                if bits_remaining == 0 {
                    payload_word_address = payload_word_address.wrapping_add(4);
                    payload_word = bus.read32(payload_word_address);
                    bits_remaining = 32;
                }
                let bit = (payload_word >> 31) as u8;
                payload_word <<= 1;
                bits_remaining -= 1;
                let child_offset = u32::from(node & 0x3F) * 2 + 2 + u32::from(bit);
                let child_address = node_address.wrapping_add(child_offset);
                let is_leaf = if bit == 0 {
                    node & 0x80 != 0
                } else {
                    node & 0x40 != 0
                };
                if is_leaf {
                    let symbol = bus.read8(child_address);
                    if symbol_bits == 8 {
                        output.push(symbol);
                    } else if symbol_index & 1 == 0 {
                        // GBA stores the first 4-bit symbol in the low nibble.
                        output.push(symbol & 0x0F);
                    } else if let Some(byte) = output.last_mut() {
                        *byte |= (symbol & 0x0F) << 4;
                    }
                    decoded = true;
                    break;
                }
                node_address = child_address;
            }
            if !decoded {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
        }
        output.truncate(output_size);
        Ok(output)
    }

    /// Decodes the BIOS LZ77 (0x10 header) and RLE (0x30 header) streams.
    ///
    /// VRAM-specific packing is handled by the caller: the decoder always
    /// returns the uncompressed byte stream, making the format logic testable
    /// independently of destination bus restrictions.
    fn decompress_bios_data(
        address: u32,
        instruction: u32,
        swi: u32,
        source: u32,
        bus: &MemoryBus,
    ) -> Result<Vec<u8>, CpuError> {
        let header = bus.read32(source);
        let expected_tag = if matches!(swi, 0x11 | 0x12) {
            0x10
        } else {
            0x30
        };
        if header as u8 != expected_tag {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }
        let output_size = ((header >> 8) & 0x00FF_FFFF) as usize;
        let mut output = Vec::with_capacity(output_size);
        let mut cursor = source.wrapping_add(4);

        if matches!(swi, 0x14 | 0x15) {
            while output.len() < output_size {
                let control = bus.read8(cursor);
                cursor = cursor.wrapping_add(1);
                if control & 0x80 != 0 {
                    let count = usize::from(control & 0x7F) + 3;
                    let value = bus.read8(cursor);
                    cursor = cursor.wrapping_add(1);
                    let remaining = output_size - output.len();
                    output.extend(core::iter::repeat(value).take(count.min(remaining)));
                } else {
                    let count = usize::from(control) + 1;
                    let remaining = (output_size - output.len()).min(count);
                    for _ in 0..remaining {
                        output.push(bus.read8(cursor));
                        cursor = cursor.wrapping_add(1);
                    }
                }
            }
        } else {
            while output.len() < output_size {
                let flags = bus.read8(cursor);
                cursor = cursor.wrapping_add(1);
                for bit in (0..8).rev() {
                    if output.len() >= output_size {
                        break;
                    }
                    if flags & (1 << bit) == 0 {
                        output.push(bus.read8(cursor));
                        cursor = cursor.wrapping_add(1);
                    } else {
                        let first = bus.read8(cursor);
                        let second = bus.read8(cursor.wrapping_add(1));
                        cursor = cursor.wrapping_add(2);
                        let count = usize::from(first >> 4) + 3;
                        let distance = (usize::from(first & 0x0F) << 8 | usize::from(second)) + 1;
                        if distance > output.len() {
                            return Err(CpuError::UnsupportedInstruction {
                                address,
                                instruction,
                            });
                        }
                        let remaining = (output_size - output.len()).min(count);
                        for _ in 0..remaining {
                            let value = output[output.len() - distance];
                            output.push(value);
                        }
                    }
                }
            }
        }
        Ok(output)
    }

    /// Executes ARM7TDMI SWP/SWPB. The read and write are performed as
    /// one instruction-level operation; bus-cycle atomicity is not modeled.
    fn execute_swap(
        &mut self,
        address: u32,
        instruction: u32,
        bus: &mut MemoryBus,
    ) -> Result<(), CpuError> {
        let byte = instruction & (1 << 22) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;
        let rm = (instruction & 0xF) as usize;

        if rn == 15 || rd == 15 || rm == 15 {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let effective = self.registers[rn];
        if !byte && effective & 3 != 0 {
            // Word SWP requires a word-aligned address on ARM7TDMI.
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let source = self.registers[rm];
        let previous = if byte {
            let previous = u32::from(bus.read8(effective));
            bus.write8(effective, source as u8);
            previous
        } else {
            let previous = bus.read32(effective);
            bus.write32(effective, source);
            previous
        };
        self.registers[rd] = previous;
        Ok(())
    }

    fn execute_long_multiply(&mut self, address: u32, instruction: u32) -> Result<(), CpuError> {
        let signed = instruction & (1 << 22) != 0;
        let accumulate = instruction & (1 << 21) != 0;
        let set_flags = instruction & (1 << 20) != 0;
        let rd_hi = ((instruction >> 16) & 0xF) as usize;
        let rd_lo = ((instruction >> 12) & 0xF) as usize;
        let rs = ((instruction >> 8) & 0xF) as usize;
        let rm = (instruction & 0xF) as usize;

        // ARM7TDMI forbids PC operands, identical high/low destinations,
        // and using Rm as either destination. Rs may alias a destination.
        if rd_hi == 15
            || rd_lo == 15
            || rd_hi == rd_lo
            || rs == 15
            || rm == 15
            || rd_hi == rm
            || rd_lo == rm
        {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let mut result = if signed {
            let left = self.registers[rm] as i32 as i64;
            let right = self.registers[rs] as i32 as i64;
            (left.wrapping_mul(right)) as u64
        } else {
            u64::from(self.registers[rm]) * u64::from(self.registers[rs])
        };

        if accumulate {
            let accumulator =
                (u64::from(self.registers[rd_hi]) << 32) | u64::from(self.registers[rd_lo]);
            result = result.wrapping_add(accumulator);
        }

        self.registers[rd_lo] = result as u32;
        self.registers[rd_hi] = (result >> 32) as u32;
        if set_flags {
            self.flags.negative = result >> 63 != 0;
            self.flags.zero = result == 0;
            // Long multiply updates N and Z only; C and V are unchanged.
        }
        Ok(())
    }

    fn execute_multiply(&mut self, address: u32, instruction: u32) -> Result<(), CpuError> {
        let accumulate = instruction & (1 << 21) != 0;
        let set_flags = instruction & (1 << 20) != 0;
        let rd = ((instruction >> 16) & 0xF) as usize;
        let rn = ((instruction >> 12) & 0xF) as usize;
        let rs = ((instruction >> 8) & 0xF) as usize;
        let rm = (instruction & 0xF) as usize;

        // ARM7TDMI multiply operands and destination must not use r15.
        // Rd must also differ from Rm; that overlap is architecturally
        // unpredictable on this core, for both MUL and MLA.
        if rd == 15
            || rs == 15
            || rm == 15
            || rd == rm
            || (accumulate && rn == 15)
            || (!accumulate && rn != 0)
        {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let mut result = self.registers[rm].wrapping_mul(self.registers[rs]);
        if accumulate {
            result = result.wrapping_add(self.registers[rn]);
        }
        self.registers[rd] = result;

        if set_flags {
            self.flags.negative = result >> 31 != 0;
            self.flags.zero = result == 0;
            // MUL/MLA update N and Z only; C and V are unchanged.
        }
        Ok(())
    }

    fn execute_data_processing(&mut self, address: u32, instruction: u32) -> Result<(), CpuError> {
        let immediate = instruction & (1 << 25) != 0;
        let opcode = (instruction >> 21) & 0xF;
        let set_flags = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;

        // TST, TEQ, CMP and CMN are defined only with S=1. Treat their
        // reserved S=0 encodings as unsupported instead of silently changing
        // CPSR flags for an instruction that is not architecturally valid.
        if (0x8..=0xB).contains(&opcode) && !set_flags {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // Data-processing instructions with S=1 and Rd=PC perform an
        // exception return: the CPSR is restored from the current SPSR.
        // Since this core does not model SPSRs or exception modes yet, reject
        // these instructions rather than silently branch with incorrect flags.
        if set_flags && rd == 15 {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let left = self.read_operand_register(rn, address);
        let (right, shifter_carry) = if immediate {
            let value = instruction & 0xFF;
            let rotate = ((instruction >> 8) & 0xF) * 2;
            let result = value.rotate_right(rotate);
            (
                result,
                if rotate == 0 {
                    None
                } else {
                    Some(result >> 31 != 0)
                },
            )
        } else if instruction & (1 << 4) != 0 {
            // Register-controlled shifts use Rs[7:0] as the shift amount.
            // Bit 7 must be clear for the ARM7TDMI data-processing encoding.
            if instruction & (1 << 7) != 0 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            let rm = (instruction & 0xF) as usize;
            let rs = ((instruction >> 8) & 0xF) as usize;
            // ARM7TDMI register-controlled shifts cannot use PC as Rs.
            // Rm=PC observes the extra register-shift pipeline cycle (PC+12).
            if rs == 15 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            let value = if rm == 15 {
                address.wrapping_add(12)
            } else {
                self.registers[rm]
            };
            let amount = self.registers[rs] & 0xFF;
            let shift_type = (instruction >> 5) & 3;
            Self::shift_register(value, shift_type, amount)
        } else {
            let rm = (instruction & 0xF) as usize;
            let value = self.read_operand_register(rm, address);
            let shift_type = (instruction >> 5) & 3;
            let shift_amount = (instruction >> 7) & 0x1F;
            let (result, carry) =
                Self::shift_immediate(value, shift_type, shift_amount, self.flags.carry);
            (result, carry)
        };

        let (result, write_result, arithmetic_flags) = match opcode {
            0x0 => (left & right, true, None),
            0x1 => (left ^ right, true, None),
            0x2 => {
                let (result, carry, overflow) = Self::subtract(left, right, false);
                (result, true, Some((carry, overflow)))
            }
            0x3 => {
                let (result, carry, overflow) = Self::subtract(right, left, false);
                (result, true, Some((carry, overflow)))
            }
            0x4 => {
                let (result, carry, overflow) = Self::add(left, right, false);
                (result, true, Some((carry, overflow)))
            }
            0x5 => {
                let (result, carry, overflow) = Self::add(left, right, self.flags.carry);
                (result, true, Some((carry, overflow)))
            }
            0x6 => {
                let (result, carry, overflow) = Self::subtract(left, right, !self.flags.carry);
                (result, true, Some((carry, overflow)))
            }
            0x7 => {
                let (result, carry, overflow) = Self::subtract(right, left, !self.flags.carry);
                (result, true, Some((carry, overflow)))
            }
            0x8 => (left & right, false, None),
            0x9 => (left ^ right, false, None),
            0xA => {
                let (result, carry, overflow) = Self::subtract(left, right, false);
                (result, false, Some((carry, overflow)))
            }
            0xB => {
                let (result, carry, overflow) = Self::add(left, right, false);
                (result, false, Some((carry, overflow)))
            }
            0xC => (left | right, true, None),
            0xD => (right, true, None),
            0xE => (left & !right, true, None),
            0xF => (!right, true, None),
            _ => unreachable!(),
        };

        if write_result {
            self.registers[rd] = if rd == 15 { result & !3 } else { result };
        }

        if set_flags || !write_result {
            self.flags.negative = result >> 31 != 0;
            self.flags.zero = result == 0;
            if let Some((carry, overflow)) = arithmetic_flags {
                self.flags.carry = carry;
                self.flags.overflow = overflow;
            } else if let Some(carry) = shifter_carry {
                self.flags.carry = carry;
            }
        }
        Ok(())
    }

    fn execute_block_transfer(
        &mut self,
        address: u32,
        instruction: u32,
        bus: &mut MemoryBus,
    ) -> Result<(), CpuError> {
        let pre_index = instruction & (1 << 24) != 0;
        let increment = instruction & (1 << 23) != 0;
        let user_registers = instruction & (1 << 22) != 0;
        let write_back = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let register_list = instruction & 0xFFFF;

        // User-bank transfers require exception/banked-register support.
        if user_registers {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // Writing back through r15 makes the block-transfer base depend on
        // the pipeline-visible PC and has unpredictable control-flow effects.
        if rn == 15 && write_back {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // ARM7TDMI treats an empty list as a PC transfer, but adjusts the
        // address/write-back as though all 16 registers had been listed.
        let empty_list = register_list == 0;
        let effective_list = if empty_list { 1 << 15 } else { register_list };
        let count = if empty_list {
            16
        } else {
            register_list.count_ones()
        };
        let base = self.read_operand_register(rn, address);
        let bytes = count * 4;
        let first_address = match (increment, pre_index) {
            (true, false) => base,
            (true, true) => base.wrapping_add(4),
            (false, false) => base.wrapping_sub(bytes).wrapping_add(4),
            (false, true) => base.wrapping_sub(bytes),
        };
        // With an empty list, PC is the only transferred register but its
        // slot is still the sixteenth word in the 16-register address span.
        let mut transfer_address = if empty_list {
            first_address.wrapping_add(15 * 4)
        } else {
            first_address
        };

        for register in 0..16 {
            if effective_list & (1 << register) == 0 {
                continue;
            }

            if load {
                let value = bus.read32(transfer_address);
                self.registers[register] = if register == 15 { value & !3 } else { value };
            } else {
                let value = if register == 15 {
                    address.wrapping_add(12)
                } else if register == rn && write_back {
                    // ARM7TDMI STM with write-back and the base in the list
                    // stores the updated base only when it is the lowest
                    // numbered register transferred. Otherwise it stores
                    // the original base value.
                    let lowest_register = register_list.trailing_zeros();
                    if lowest_register == rn as u32 {
                        if increment {
                            base.wrapping_add(bytes)
                        } else {
                            base.wrapping_sub(bytes)
                        }
                    } else {
                        base
                    }
                } else {
                    self.registers[register]
                };
                bus.write32(transfer_address, value);
            }
            transfer_address = transfer_address.wrapping_add(4);
        }

        if write_back {
            // For LDM with the base register in the list, ARM7TDMI's result
            // depends on register ordering: if the base is the first register
            // loaded, the loaded memory value wins; otherwise write-back wins.
            // The loop above has already loaded the base in the first case.
            let loaded_base_wins = load
                && !empty_list
                && register_list & (1 << rn) != 0
                && register_list.trailing_zeros() == rn as u32;
            if !loaded_base_wins {
                self.registers[rn] = if increment {
                    base.wrapping_add(bytes)
                } else {
                    base.wrapping_sub(bytes)
                };
            }
        }
        Ok(())
    }

    fn execute_halfword_transfer(
        &mut self,
        address: u32,
        instruction: u32,
        bus: &mut MemoryBus,
    ) -> Result<(), CpuError> {
        let pre_index = instruction & (1 << 24) != 0;
        let add_offset = instruction & (1 << 23) != 0;
        let immediate_offset = instruction & (1 << 22) != 0;
        let write_back = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;
        let operation = (instruction >> 5) & 3;

        // ARM7TDMI halfword/signed-byte transfers do not permit PC as Rd.
        // Loads that write back into their own base register are also
        // unpredictable for post-indexed or explicitly write-back forms.
        if rd == 15 || (load && rd == rn && (!pre_index || write_back)) {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // A PC-relative halfword literal load is only valid without write-back.
        // Post-indexing or explicitly writing back through r15 would turn the
        // instruction fetch address into a data base and unpredictably redirect
        // execution on ARM7TDMI.
        if rn == 15 && (!pre_index || write_back) {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // Operation 00 is multiply/other encoding space, not a halfword
        // transfer. STR supports only the unsigned halfword operation.
        if operation == 0 || (!load && operation != 1) {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let offset = if immediate_offset {
            ((instruction >> 4) & 0xF0) | (instruction & 0xF)
        } else {
            if instruction & 0xF00 != 0 {
                return Err(CpuError::UnsupportedInstruction {
                    address,
                    instruction,
                });
            }
            self.read_operand_register((instruction & 0xF) as usize, address)
        };
        let base = self.read_operand_register(rn, address);
        let adjusted = if add_offset {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let effective = if pre_index { adjusted } else { base };

        if load {
            let value = match operation {
                // ARM7TDMI rotates an unaligned unsigned halfword load
                // by 8 bits; signed halfword loads use byte-sign-extension
                // semantics at odd addresses (handled below).
                1 => {
                    let value = bus.read16(effective);
                    u32::from(if effective & 1 != 0 {
                        value.rotate_right(8)
                    } else {
                        value
                    })
                }
                2 => bus.read8(effective) as i8 as i32 as u32,
                // LDRSH at an odd address behaves like LDRSB on ARM7TDMI.
                3 => {
                    if effective & 1 != 0 {
                        bus.read8(effective) as i8 as i32 as u32
                    } else {
                        bus.read16(effective) as i16 as i32 as u32
                    }
                }
                _ => unreachable!(),
            };
            self.registers[rd] = if rd == 15 { value & !3 } else { value };
        } else {
            bus.write16(effective, self.registers[rd] as u16);
        }

        if !pre_index || write_back {
            self.registers[rn] = adjusted;
        }
        Ok(())
    }

    fn execute_single_transfer(
        &mut self,
        address: u32,
        instruction: u32,
        bus: &mut MemoryBus,
    ) -> Result<(), CpuError> {
        let register_offset = instruction & (1 << 25) != 0;
        let pre_index = instruction & (1 << 24) != 0;
        let add_offset = instruction & (1 << 23) != 0;
        let byte_transfer = instruction & (1 << 22) != 0;
        let write_back = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;

        // ARM7TDMI defines a load with write-back and Rd == Rn as
        // unpredictable. Post-indexed loads always write back, so reject
        // both forms instead of silently choosing an arbitrary register value.
        if load && rd == rn && (!pre_index || write_back) {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // PC-relative literal transfers are valid only without write-back.
        // Updating r15 as the address base has unpredictable control-flow
        // effects on ARM7TDMI, including for post-indexed encodings.
        if rn == 15 && (!pre_index || write_back) {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        // With P=0 and W=1, ARM single-transfer encodings select LDRT/STRT
        // (unprivileged access semantics), not an ordinary post-indexed load
        // or store. This core has no CPU privilege-mode model, so reject these
        // forms instead of silently executing them with privileged semantics.
        if !pre_index && write_back {
            return Err(CpuError::UnsupportedInstruction {
                address,
                instruction,
            });
        }

        let base = self.read_operand_register(rn, address);
        let offset = if register_offset {
            let rm = (instruction & 0xF) as usize;
            let value = if rm == 15 {
                // ARM7TDMI exposes PC+12 when PC supplies a single-transfer
                // register offset, including the shifted-register form.
                address.wrapping_add(12)
            } else {
                self.read_operand_register(rm, address)
            };
            let shift_type = (instruction >> 5) & 3;
            if instruction & (1 << 4) != 0 {
                let rs = ((instruction >> 8) & 0xF) as usize;
                if rs == 15 {
                    return Err(CpuError::UnsupportedInstruction {
                        address,
                        instruction,
                    });
                }
                let amount = self.registers[rs] & 0xFF;
                Self::shift_register(value, shift_type, amount).0
            } else {
                let shift_amount = (instruction >> 7) & 0x1F;
                Self::shift_immediate(value, shift_type, shift_amount, self.flags.carry).0
            }
        } else {
            instruction & 0xFFF
        };
        let adjusted = if add_offset {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let effective = if pre_index { adjusted } else { base };

        if load {
            let value = if byte_transfer {
                u32::from(bus.read8(effective))
            } else {
                // ARM7TDMI word loads align the bus access and rotate the
                // fetched word right when the effective address is unaligned.
                bus.read32(effective).rotate_right((effective & 3) * 8)
            };
            self.registers[rd] = if rd == 15 { value & !3 } else { value };
        } else {
            // ARM7TDMI exposes PC as instruction address + 12 when it is
            // used as the source of an ARM-state single data transfer.
            let value = if rd == 15 {
                address.wrapping_add(12)
            } else {
                self.registers[rd]
            };
            if byte_transfer {
                bus.write8(effective, value as u8);
            } else {
                bus.write32(effective, value);
            }
        }

        if !pre_index || write_back {
            self.registers[rn] = adjusted;
        }
        Ok(())
    }

    fn shift_immediate(value: u32, kind: u32, amount: u32, old_carry: bool) -> (u32, Option<bool>) {
        match (kind, amount) {
            (0, 0) => (value, None),
            (0, n) => (value << n, Some(value & (1 << (32 - n)) != 0)),
            (1, 0) => (0, Some(value >> 31 != 0)),
            (1, n) => (value >> n, Some(value & (1 << (n - 1)) != 0)),
            (2, 0) => ((value as i32 >> 31) as u32, Some(value >> 31 != 0)),
            (2, n) => (
                ((value as i32) >> n) as u32,
                Some(value & (1 << (n - 1)) != 0),
            ),
            (3, 0) => {
                let result = (u32::from(old_carry) << 31) | (value >> 1);
                (result, Some(value & 1 != 0))
            }
            (3, n) => {
                let result = value.rotate_right(n);
                (result, Some(result >> 31 != 0))
            }
            _ => unreachable!(),
        }
    }

    /// Applies a Thumb immediate shift. Unlike ARM's operand shifter, an
    /// encoded LSR/ASR amount of zero means a shift by 32.
    fn shift_thumb_immediate(
        value: u32,
        kind: u32,
        amount: u32,
        _old_carry: bool,
    ) -> (u32, Option<bool>) {
        match (kind, amount) {
            (0, 0) => (value, None),
            (0, n) => (value << n, Some((value >> (32 - n)) & 1 != 0)),
            (1, 0) => (0, Some(value >> 31 != 0)),
            (1, n) => (value >> n, Some((value >> (n - 1)) & 1 != 0)),
            (2, 0) => {
                let sign = value >> 31 != 0;
                (if sign { u32::MAX } else { 0 }, Some(sign))
            }
            (2, n) => (
                ((value as i32) >> n) as u32,
                Some((value >> (n - 1)) & 1 != 0),
            ),
            (3, _) => unreachable!("Thumb immediate shifts do not encode ROR"),
            _ => unreachable!("invalid Thumb immediate shift"),
        }
    }

    /// Applies an ARM register-controlled shift (ARM7TDMI register operand form).
    fn shift_register(value: u32, kind: u32, amount: u32) -> (u32, Option<bool>) {
        if amount == 0 {
            return (value, None);
        }

        match kind {
            0 => match amount {
                1..=31 => (value << amount, Some((value >> (32 - amount)) & 1 != 0)),
                32 => (0, Some(value & 1 != 0)),
                _ => (0, Some(false)),
            },
            1 => match amount {
                1..=31 => (value >> amount, Some((value >> (amount - 1)) & 1 != 0)),
                32 => (0, Some(value >> 31 != 0)),
                _ => (0, Some(false)),
            },
            2 => {
                if amount >= 32 {
                    let carry = value >> 31 != 0;
                    (if carry { u32::MAX } else { 0 }, Some(carry))
                } else {
                    (
                        ((value as i32) >> amount) as u32,
                        Some((value >> (amount - 1)) & 1 != 0),
                    )
                }
            }
            3 => {
                let rotation = amount & 31;
                if rotation == 0 {
                    (value, Some(value >> 31 != 0))
                } else {
                    let result = value.rotate_right(rotation);
                    (result, Some(result >> 31 != 0))
                }
            }
            _ => unreachable!(),
        }
    }

    fn add(left: u32, right: u32, carry_in: bool) -> (u32, bool, bool) {
        let sum = u64::from(left) + u64::from(right) + u64::from(carry_in);
        let result = sum as u32;
        let overflow = ((!(left ^ right) & (left ^ result)) >> 31) != 0;
        (result, sum >> 32 != 0, overflow)
    }

    fn subtract(left: u32, right: u32, borrow_in: bool) -> (u32, bool, bool) {
        let subtrahend = u64::from(right) + u64::from(borrow_in);
        let result = left.wrapping_sub(right).wrapping_sub(u32::from(borrow_in));
        let carry = u64::from(left) >= subtrahend;
        let overflow = (((left ^ right) & (left ^ result)) >> 31) != 0;
        (result, carry, overflow)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bios_bg_affine_set_writes_identity_matrix_and_reference_point() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 100 << 8);
        bus.write32(source + 4, 50 << 8);
        bus.write16(source + 8, 2);
        bus.write16(source + 10, 3);
        bus.write16(source + 12, 0x0100);
        bus.write16(source + 14, 0x0100);
        bus.write16(source + 16, 0);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.set_register(2, 1);
        cpu.execute_swi(0x0200_0200, 0xEF00_000E, 0x0E, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read16(destination) as i16, 256);
        assert_eq!(bus.read16(destination + 2) as i16, 0);
        assert_eq!(bus.read16(destination + 4) as i16, 0);
        assert_eq!(bus.read16(destination + 6) as i16, 256);
        assert_eq!(bus.read32(destination + 8), (98 << 8) as u32);
        assert_eq!(bus.read32(destination + 12), (47 << 8) as u32);
    }

    #[test]
    fn bios_obj_affine_set_respects_destination_stride() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write16(source, 0x0100);
        bus.write16(source + 2, 0x0100);
        bus.write16(source + 4, 0);
        bus.write16(source + 8, 0x0080);
        bus.write16(source + 10, 0x0200);
        bus.write16(source + 12, 0x4000);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.set_register(2, 2);
        cpu.set_register(3, 8);
        cpu.execute_swi(0x0200_0200, 0xEF00_000F, 0x0F, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read16(destination) as i16, 256);
        assert_eq!(bus.read16(destination + 8) as i16, 0);
        assert_eq!(bus.read16(destination + 16) as i16, 0);
        assert_eq!(bus.read16(destination + 24) as i16, 256);
        assert_eq!(bus.read16(destination + 2) as i16, 0);
        assert_eq!(bus.read16(destination + 10) as i16, -128);
        assert_eq!(bus.read16(destination + 18) as i16, 512);
        assert_eq!(bus.read16(destination + 26) as i16, 0);
    }

    #[test]
    fn bios_affine_services_leave_destination_untouched_for_zero_count() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        for offset in (0..32).step_by(2) {
            bus.write16(destination + offset, 0xA55A);
        }
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.set_register(2, 0);
        cpu.execute_swi(0x0200_0200, 0xEF00_000E, 0x0E, false, &mut bus)
            .unwrap();
        cpu.set_register(3, 8);
        cpu.execute_swi(0x0200_0204, 0xEF00_000F, 0x0F, false, &mut bus)
            .unwrap();
        for offset in (0..32).step_by(2) {
            assert_eq!(bus.read16(destination + offset), 0xA55A);
        }
    }

    #[test]
    fn bios_arctan_returns_zero_for_zero_and_expected_axis_angles() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);

        cpu.set_register(0, 0);
        cpu.execute_swi(0x0200_0200, 0xEF00_0009, 0x09, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0).unwrap(), 0);

        cpu.set_register(0, 0);
        cpu.set_register(1, 0x4000);
        cpu.execute_swi(0x0200_0204, 0xEF00_000A, 0x0A, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0).unwrap() as i32, 6434);

        cpu.set_register(0, 0xC000);
        cpu.set_register(1, 0);
        cpu.execute_swi(0x0200_0208, 0xEF00_000A, 0x0A, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0).unwrap() as i32, 12868);
    }

    #[test]
    fn bios_halt_ignores_requests_without_an_enabled_interrupt_and_reset_wakes_cpu() {
        let mut bus = bus_with_program(&[0xEF00_0002, 0xE3A0_0007]); // Halt; MOV r0, #7.
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.step_with_cycles(&mut bus).unwrap();
        assert!(cpu.is_halted());

        bus.request_interrupt(1 << 0); // IF set, but IE remains clear.
        let (idle, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(idle, StepOutcome::Halted);
        assert_eq!(cycles, 1);
        assert!(cpu.is_halted());

        cpu.reset(0x0200_0004);
        assert!(!cpu.is_halted());
        cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(7));
    }

    #[test]
    fn bios_halt_stops_instruction_fetch_until_enabled_interrupt_is_pending() {
        let mut bus = bus_with_program(&[0xEF00_0002, 0xE3A0_0007]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);

        let (halt, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(matches!(halt, StepOutcome::Executed { .. }));
        assert!(cpu.is_halted());
        assert_eq!(cpu.register(15), Some(0x0200_0004));

        let pc = cpu.register(15).unwrap();
        let (idle, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(idle, StepOutcome::Halted);
        assert_eq!(cycles, 1);
        assert_eq!(cpu.register(15), Some(pc));

        bus.write16(0x0400_0200, 1 << 0); // Enable VBlank IRQ.
        bus.request_interrupt(1 << 0);
        let (resumed, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(matches!(resumed, StepOutcome::Executed { .. }));
        assert!(!cpu.is_halted());
        assert_eq!(cpu.register(0), Some(7));
    }

    #[test]
    fn bios_register_ram_reset_clears_only_selected_regions() {
        let mut bus = bus_with_program(&[]);
        bus.write8(0x0200_0000, 0xA5);
        bus.write8(0x0300_0000, 0x5A);
        bus.write8(0x0500_0000, 0x3C);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, (1 << 0) | (1 << 1));
        cpu.execute_swi(0x0200_0100, 0xEF00_0001, 0x01, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read8(0x0200_0000), 0);
        assert_eq!(bus.read8(0x0300_0000), 0);
        assert_eq!(bus.read8(0x0500_0000), 0x3C);
    }

    #[test]
    fn bios_arctan_returns_zero_and_positive_quarter_turn_approximation() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0);
        cpu.execute_swi(0x0200_0100, 0xEF00_0009, 0x09, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(0));

        cpu.set_register(0, 16_384);
        cpu.execute_swi(0x0200_0100, 0xEF00_0009, 0x09, false, &mut bus)
            .unwrap();
        let angle = cpu.register(0).unwrap() as i32;
        assert!((angle - 3_217).abs() <= 1);
    }

    #[test]
    fn bios_arctan2_handles_origin_and_quadrants() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0);
        cpu.set_register(1, 0);
        cpu.execute_swi(0x0200_0100, 0xEF00_000A, 0x0A, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(0));

        cpu.set_register(0, 0);
        cpu.set_register(1, 16_384);
        cpu.execute_swi(0x0200_0100, 0xEF00_000A, 0x0A, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(6_434));

        cpu.set_register(0, (-16_384_i32) as u32);
        cpu.set_register(1, 0);
        cpu.execute_swi(0x0200_0100, 0xEF00_000A, 0x0A, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(12_868));
    }

    #[test]
    fn bios_division_handles_signed_overflow_and_div_arm_operand_order() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);

        // ARM's signed division edge case wraps INT_MIN / -1 to INT_MIN.
        cpu.set_register(0, i32::MIN as u32);
        cpu.set_register(1, u32::MAX);
        cpu.execute_swi(0x0200_0100, 0xEF00_0006, 0x06, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(i32::MIN as u32));
        assert_eq!(cpu.register(1), Some(0));
        assert_eq!(cpu.register(3), Some(0x8000_0000));

        // DivArm swaps R0/R1 before dividing.
        cpu.set_register(0, 3);
        cpu.set_register(1, 10);
        cpu.execute_swi(0x0200_0104, 0xEF00_0007, 0x07, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(3));
        assert_eq!(cpu.register(1), Some(1));
        assert_eq!(cpu.register(3), Some(3));
    }

    #[test]
    fn bios_division_by_zero_returns_sign_sentinel_and_preserves_numerator() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, (-7_i32) as u32);
        cpu.set_register(1, 0);
        cpu.execute_swi(0x0200_0100, 0xEF00_0006, 0x06, false, &mut bus)
            .unwrap();
        assert_eq!(cpu.register(0), Some(1));
        assert_eq!(cpu.register(1), Some((-7_i32) as u32));
        assert_eq!(cpu.register(3), Some(1));
    }

    #[test]
    fn diff8_filter_reconstructs_wrapping_delta_stream() {
        let mut bus = bus_with_program(&[]);
        bus.write32(0x0200_0010, 0x0000_0380); // type 0x80, 3 output bytes
        bus.write8(0x0200_0014, 250);
        bus.write8(0x0200_0015, 10);
        bus.write8(0x0200_0016, 5);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0010);
        cpu.set_register(1, 0x0300_0000);
        cpu.execute_swi(0x0200_0100, 0xEF00_0016, 0x16, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read8(0x0300_0000), 250);
        assert_eq!(bus.read8(0x0300_0001), 4);
        assert_eq!(bus.read8(0x0300_0002), 9);
    }

    #[test]
    fn diff16_filter_reconstructs_halfword_deltas() {
        let mut bus = bus_with_program(&[]);
        bus.write32(0x0200_0010, 0x0000_0481); // type 0x81, 4 output bytes
        bus.write16(0x0200_0014, 0xFFFE);
        bus.write16(0x0200_0016, 5);
        bus.write16(0x0200_0018, 4);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0010);
        cpu.set_register(1, 0x0300_0000);
        cpu.execute_swi(0x0200_0100, 0xEF00_0017, 0x17, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read16(0x0300_0000), 0xFFFE);
        assert_eq!(bus.read16(0x0300_0002), 3);
    }

    #[test]
    fn bit_unpack_handles_packed_nibbles_and_offset_zero_flag() {
        let mut bus = bus_with_program(&[]);
        // Two source bytes, 4-bit source fields, 8-bit output fields, offset 0x10.
        bus.write8(0x0200_0000, 0x21);
        bus.write8(0x0200_0001, 0x03);
        bus.write16(0x0200_0010, 2);
        bus.write8(0x0200_0012, 4);
        bus.write8(0x0200_0013, 8);
        bus.write32(0x0200_0014, 0x8000_0010);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0000);
        cpu.set_register(1, 0x0300_0000);
        cpu.set_register(2, 0x0200_0010);
        cpu.execute_swi(0x0200_0100, 0xEF00_0010, 0x10, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read32(0x0300_0000), 0x0013_1211);
        assert_eq!(cpu.register(15), Some(0x0200_0104));
    }

    #[test]
    fn bit_unpack_rejects_invalid_field_widths_without_writing_output() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0010, 1);
        bus.write8(0x0200_0012, 0);
        bus.write8(0x0200_0013, 8);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0000);
        cpu.set_register(1, 0x0300_0000);
        cpu.set_register(2, 0x0200_0010);
        assert!(cpu
            .execute_swi(0x0200_0100, 0xEF00_0010, 0x10, false, &mut bus)
            .is_err());
        assert_eq!(bus.read32(0x0300_0000), 0);
    }

    #[test]
    fn snapshot_is_detached_and_consistent() {
        let mut cpu = Cpu::new();
        cpu.set_register(0, 0x1234_5678);
        cpu.set_register(15, 0x0800_0000);
        let snapshot = cpu.snapshot();
        assert_eq!(snapshot.registers[0], 0x1234_5678);
        assert_eq!(snapshot.pc, 0x0800_0000);
        assert_eq!(snapshot.cpsr, cpu.cpsr_flags());
        assert!(!snapshot.thumb);
        cpu.set_register(0, 9);
        assert_eq!(snapshot.registers[0], 0x1234_5678);
    }

    #[test]
    fn public_condition_evaluator_handles_reserved_codes() {
        let cpu = Cpu::new();
        assert!(cpu.condition_passed_for(0xE));
        assert!(!cpu.condition_passed_for(0xF));
        assert!(!cpu.condition_passed_for(0x10));
    }

    #[test]
    fn debugger_pc_edit_resumes_halt_and_clears_bios_wait_mask() {
        let mut bus = bus_with_program(&[0xEF00_0002, 0xE3A0_0007]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.step_with_cycles(&mut bus).unwrap();
        assert!(cpu.is_halted());

        assert!(cpu.set_register(15, 0x0200_0004));
        assert!(!cpu.is_halted());
        let (outcome, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(matches!(outcome, StepOutcome::Executed { .. }));
        assert_eq!(cpu.register(0), Some(7));

        cpu.reset(0x0200_0000);
        cpu.set_register(0, 1);
        cpu.set_register(1, 1 << 0);
        cpu.execute_swi(0x0200_0000, 0xEF00_0004, 0x04, false, &mut bus)
            .unwrap();
        assert!(cpu.is_halted());
        cpu.set_execution_state(true);
        assert!(!cpu.is_halted());
    }

    #[test]
    fn external_state_setup_updates_flags_and_aligns_pc_by_instruction_state() {
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_condition_flags(ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: true,
        });
        cpu.set_execution_state(true);
        cpu.set_register(15, 0x0200_0003);
        assert!(cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0002));
        assert!(cpu.flags().negative);
        assert!(cpu.flags().carry);
        assert!(cpu.flags().overflow);
        assert!(!cpu.flags().zero);

        cpu.set_execution_state(false);
        assert!(!cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0000));
        assert!(cpu.flags().negative);
    }

    use super::*;
    use crate::GbaCartridge;

    fn bus_with_program(program: &[u32]) -> MemoryBus {
        let mut rom = vec![0; 0xC0 + program.len() * 4];
        for (index, instruction) in program.iter().enumerate() {
            let offset = 0xC0 + index * 4;
            rom[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        let checksum = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        rom[0xBD] = checksum;
        // Place the test program in the cartridge mapping at ROM offset zero.
        let mut bus = MemoryBus::new(GbaCartridge::parse(&rom).unwrap());
        for (index, instruction) in program.iter().enumerate() {
            bus.write32(0x0200_0000 + index as u32 * 4, *instruction);
        }
        bus
    }

    #[test]
    fn unsupported_arm_and_thumb_instructions_preserve_faulting_pc() {
        let mut bus = bus_with_program(&[0xF000_0000]);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        let error = cpu.step(&mut bus).unwrap_err();
        assert_eq!(error.address(), 0x0200_0000);
        assert_eq!(error.instruction(), 0xF000_0000);
        assert_eq!(cpu.register(15), Some(0x0200_0000));
        assert_eq!(cpu.register(0), Some(0));
        assert_eq!(cpu.flags(), ConditionFlags::default());

        bus.write16(0x0200_0000, 0xDE00); // Undefined Thumb conditional branch.
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);
        let error = cpu.step(&mut bus).unwrap_err();
        assert_eq!(error.address(), 0x0200_0000);
        assert_eq!(error.instruction(), 0xDE00);
        assert_eq!(cpu.register(15), Some(0x0200_0000));
    }

    #[test]
    fn bios_hle_div_and_sqrt_cover_signed_and_unsigned_edges() {
        let mut bus = bus_with_program(&[0xEF00_0006, 0xEF00_0008]);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, (-17_i32) as u32);
        cpu.set_register(1, 5);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some((-3_i32) as u32));
        assert_eq!(cpu.register(1), Some((-2_i32) as u32));
        assert_eq!(cpu.register(3), Some(3));
        cpu.set_register(0, 0xFFFF_FFFF);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(65_535));
    }

    #[test]
    fn bios_hle_checksum_and_halfword_cpuset_are_supported() {
        let mut bus = bus_with_program(&[0xEF00_000D, 0xEF00_000B]);
        bus.write16(0x0200_0100, 0x1357);
        bus.write16(0x0200_0102, 0x2468);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0xBAAE_187F));

        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, 2);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read16(0x0200_0200), 0x1357);
        assert_eq!(bus.read16(0x0200_0202), 0x2468);
    }

    #[test]
    fn bios_hle_cpuset_copies_and_fills_memory() {
        let mut bus = bus_with_program(&[0xEF00_000B, 0xEF00_000B]);
        bus.write32(0x0200_0100, 0x1234_5678);
        bus.write32(0x0200_0104, 0x9ABC_DEF0);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, (1 << 26) | 2);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0200), 0x1234_5678);
        assert_eq!(bus.read32(0x0200_0204), 0x9ABC_DEF0);

        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0300);
        cpu.set_register(2, (1 << 26) | (1 << 24) | 3);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0300), 0x1234_5678);
        assert_eq!(bus.read32(0x0200_0304), 0x1234_5678);
        assert_eq!(bus.read32(0x0200_0308), 0x1234_5678);
    }

    #[test]
    fn bios_cpufastset_rounds_copy_count_up_to_eight_words() {
        let mut bus = bus_with_program(&[0xEF00_000C]);
        for index in 0..8_u32 {
            bus.write32(0x0200_0100 + index * 4, 0xA500_0000 | index);
        }
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, 3); // Three requested words round up to eight.
        cpu.step(&mut bus).unwrap();
        for index in 0..8_u32 {
            assert_eq!(bus.read32(0x0200_0200 + index * 4), 0xA500_0000 | index);
        }
    }

    #[test]
    fn bios_cpufastset_fill_reuses_first_word_for_rounded_count() {
        let mut bus = bus_with_program(&[0xEF00_000C]);
        bus.write32(0x0200_0100, 0xDEAD_BEEF);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, (1 << 24) | 1); // Fill, rounded to eight words.
        cpu.step(&mut bus).unwrap();
        for index in 0..8_u32 {
            assert_eq!(bus.read32(0x0200_0200 + index * 4), 0xDEAD_BEEF);
        }
    }

    #[test]
    fn bios_cpuset_zero_count_leaves_destination_untouched() {
        let mut bus = bus_with_program(&[0xEF00_000B]);
        bus.write16(0x0200_0200, 0xCAFE);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, 0);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read16(0x0200_0200), 0xCAFE);
    }

    #[test]
    fn bios_huffman_decompression_decodes_eight_bit_symbols_msb_first() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 0x0000_0420); // 4 output bytes, 8-bit symbols.
        bus.write8(source + 4, 1); // Four-byte tree.
        bus.write8(source + 5, 0xC0); // Both root children are leaves.
        bus.write8(source + 7, b'A');
        bus.write8(source + 8, b'B');
        bus.write32(source + 12, 0x5000_0000); // Bits 0,1,0,1.

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.execute_swi(0x0200_0200, 0xEF00_0013, 0x13, false, &mut bus)
            .unwrap();

        assert_eq!(
            (0..4)
                .map(|i| bus.read8(destination + i))
                .collect::<Vec<_>>(),
            b"ABAB"
        );
        assert_eq!(cpu.register(15), Some(0x0200_0204));
    }

    #[test]
    fn bios_huffman_decompression_packs_four_bit_symbols_high_nibble_first() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 0x0000_0124); // One output byte, 4-bit symbols.
        bus.write8(source + 4, 1);
        bus.write8(source + 5, 0xC0);
        bus.write8(source + 7, 0x03);
        bus.write8(source + 8, 0x0C);
        bus.write32(source + 12, 0x5000_0000); // Symbols 3, C, then padding.

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.execute_swi(0x0200_0200, 0xEF00_0013, 0x13, false, &mut bus)
            .unwrap();

        assert_eq!(bus.read8(destination), 0xC3);
    }

    #[test]
    fn bios_huffman_decompression_rejects_tree_without_a_reachable_leaf() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 0x0000_0120);
        bus.write8(source + 4, 0); // A two-byte tree.
        bus.write8(source + 5, 0); // Neither child is a leaf.
        bus.write32(source + 8, 0); // Always choose the non-leaf left child.

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        assert!(cpu
            .execute_swi(0x0200_0200, 0xEF00_0013, 0x13, false, &mut bus)
            .is_err());
        assert_eq!(cpu.register(15), Some(0x0200_0200));
    }

    #[test]
    fn bios_huffman_decompression_zero_length_does_not_touch_destination() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 0x0000_0020);
        bus.write8(destination, 0xA5);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.execute_swi(0x0200_0200, 0xEF00_0013, 0x13, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read8(destination), 0xA5);
    }

    #[test]
    fn bios_huffman_decompression_rejects_unknown_format_without_advancing_pc() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        bus.write32(source, 0x0000_0128);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);

        assert!(cpu
            .execute_swi(0x0200_0200, 0xEF00_0013, 0x13, false, &mut bus)
            .is_err());
        assert_eq!(cpu.register(15), Some(0x0200_0200));
    }

    #[test]
    fn bios_hle_rle_decompression_handles_runs_and_literals() {
        let mut bus = bus_with_program(&[0xEF00_0014]);
        // RLE header: five output bytes, then a 3-byte run and two literals.
        for (offset, byte) in [
            (0, 0x30),
            (1, 5),
            (2, 0),
            (3, 0),
            (4, 0x80),
            (5, b'A'),
            (6, 1),
            (7, b'B'),
            (8, b'C'),
        ] {
            bus.write8(0x0200_0100 + offset, byte);
        }
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.step(&mut bus).unwrap();
        assert_eq!(
            (0..5)
                .map(|i| bus.read8(0x0200_0200 + i))
                .collect::<Vec<_>>(),
            b"AAABC"
        );
    }

    #[test]
    fn bios_hle_lz77_decompression_handles_overlapping_backreferences() {
        let mut bus = bus_with_program(&[0xEF00_0011]);
        // LZ77 header: six bytes. One literal ('A'), then a 5-byte back-reference.
        for (offset, byte) in [
            (0, 0x10),
            (1, 6),
            (2, 0),
            (3, 0),
            (4, 0x40),
            (5, b'A'),
            (6, 0x20),
            (7, 0x00),
        ] {
            bus.write8(0x0200_0100 + offset, byte);
        }
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.step(&mut bus).unwrap();
        assert_eq!(
            (0..6)
                .map(|i| bus.read8(0x0200_0200 + i))
                .collect::<Vec<_>>(),
            b"AAAAAA"
        );
    }

    #[test]
    fn bios_hle_rle_vram_decompression_writes_packed_halfwords() {
        let mut bus = bus_with_program(&[0xEF00_0015]);
        for (offset, byte) in [(0, 0x30), (1, 3), (2, 0), (3, 0), (4, 0x80), (5, 0x5A)] {
            bus.write8(0x0200_0100 + offset, byte);
        }
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0600_0000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read16(0x0600_0000), 0x5A5A);
        assert_eq!(bus.read16(0x0600_0002) & 0x00FF, 0x005A);
    }

    #[test]
    fn thumb_bios_hle_swi_returns_to_next_halfword() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xDF08);
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 144);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(12));
        assert_eq!(cpu.register(15), Some(0x0200_0002));
        assert!(cpu.thumb);
    }

    #[test]
    fn thumb_shift_immediate_and_arithmetic_update_registers_and_flags() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x2005); // MOVS r0, #5
        bus.write16(0x0200_0002, 0x3003); // ADDS r0, #3
        bus.write16(0x0200_0004, 0x0041); // LSLS r1, r0, #1
        bus.write16(0x0200_0006, 0x2808); // CMP r0, #8
        bus.write16(0x0200_0008, 0x4008); // ANDS r0, r1
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);

        for _ in 0..5 {
            cpu.step(&mut bus).unwrap();
        }

        assert_eq!(cpu.register(0), Some(0));
        assert_eq!(cpu.register(1), Some(16));
        assert!(cpu.flags().zero);
        assert!(!cpu.flags().negative);
        assert_eq!(cpu.register(15), Some(0x0200_000A));
    }

    #[test]
    fn cpu_debug_snapshots_report_registers_and_modeled_cpsr_flags() {
        let mut cpu = Cpu::new();
        cpu.set_register(0, 0x1234_5678);
        cpu.set_register(15, 0x0800_0000);
        assert_eq!(cpu.registers()[0], 0x1234_5678);
        assert_eq!(cpu.registers()[15], 0x0800_0000);
        assert_eq!(cpu.cpsr_flags(), 0);

        // A failed conditional instruction leaves flags untouched, while reset
        // clears the flags and returns the CPU to ARM state.
        cpu.reset(0x0800_0100);
        assert_eq!(cpu.registers()[0], 0);
        assert_eq!(cpu.registers()[15], 0x0800_0100);
        assert_eq!(cpu.cpsr_flags(), 0);
    }

    #[test]
    fn arm_multiply_cycle_estimates_follow_multiplier_sign_extension() {
        let mut cpu = Cpu::new();
        // MUL r0, r1, r2; Rs is r2. Include one fetch cycle plus
        // the ARM7TDMI internal multiply cycles.
        let mul_r2 = 0xE000_0291;
        for (operand, expected) in [
            (0x0000_007F, 2),
            (0x0000_7FFF, 3),
            (0x007F_FFFF, 4),
            (0x1234_5678, 5),
            (0xFFFF_FF80, 2),
            (0xFFFF_8000, 3),
            (0xFF80_0000, 4),
            (0x8000_0000, 5),
        ] {
            cpu.set_register(2, operand);
            assert_eq!(cpu.estimate_cycles(mul_r2), expected, "Rs={operand:#010X}");
        }

        // MLA adds one internal accumulate cycle.
        cpu.set_register(2, 7);
        assert_eq!(cpu.estimate_cycles(0xE020_0291), 3);
        // UMULL includes an extra result cycle beyond the internal multiply.
        assert_eq!(cpu.estimate_cycles(0xE081_0392), 3);
        cpu.set_register(3, 0x1234_5678);
        assert_eq!(cpu.estimate_cycles(0xE081_0392), 6);
        // UMLAL adds the accumulate cycle to the long-multiply estimate.
        assert_eq!(cpu.estimate_cycles(0xE0A1_0392), 7);
    }

    #[test]
    fn arm_long_multiply_cycle_estimates_include_extra_result_cycle() {
        let mut cpu = Cpu::new();
        // RS=0 uses one internal multiply cycle; long multiply adds one result cycle.
        assert_eq!(cpu.estimate_cycles(0xE081_0392), 3); // UMULL r0,r1,r2,r3
        assert_eq!(cpu.estimate_cycles(0xE091_0392), 3); // UMULLS: setting flags does not affect timing
        assert_eq!(cpu.estimate_cycles(0xE0A1_0392), 4); // UMLAL r0,r1,r2,r3
        assert_eq!(cpu.estimate_cycles(0xE0C1_0392), 3); // SMULL r0,r1,r2,r3
        assert_eq!(cpu.estimate_cycles(0xE0E1_0392), 4); // SMLAL r0,r1,r2,r3

        // Negative-looking values terminate early for signed long multiply,
        // but unsigned long multiply must consume all four multiplier cycles.
        cpu.set_register(3, 0xFFFF_FF80);
        assert_eq!(cpu.estimate_cycles(0xE081_0392), 6); // UMULL
        assert_eq!(cpu.estimate_cycles(0xE0C1_0392), 3); // SMULL
    }

    #[test]
    fn arm_empty_block_transfer_estimate_accounts_for_pc_load_refill() {
        let cpu = Cpu::new();
        // Empty STM behaves as a long-span PC store: 16-register span.
        assert_eq!(cpu.estimate_cycles(0xE8A0_0000), 17);
        // Empty LDM transfers PC and must also account for pipeline refill.
        assert_eq!(cpu.estimate_cycles(0xE8B0_0000), 19);
        // An explicit one-register LDM without PC remains a short transfer.
        assert_eq!(cpu.estimate_cycles(0xE8B0_0001), 2);
    }

    #[test]
    fn arm_single_load_to_pc_includes_pipeline_refill_estimate() {
        let cpu = Cpu::new();
        // LDR r0, [r1] includes the instruction fetch and data read cycles.
        assert_eq!(cpu.estimate_cycles(0xE591_0000), 3);
        // LDR pc, [r1] also pays the pipeline refill after loading the target.
        assert_eq!(cpu.estimate_cycles(0xE591_F000), 5);
        // STR pc, [r1] does not redirect execution.
        assert_eq!(cpu.estimate_cycles(0xE581_F000), 2);
    }

    #[test]
    fn arm_register_controlled_shifts_include_internal_cycle() {
        let cpu = Cpu::new();

        // MOV r0, r1, LSL r2 uses a register-controlled shift and takes
        // one additional internal cycle compared with an immediate shift.
        assert_eq!(cpu.estimate_cycles(0xE1A0_0001), 1);
        assert_eq!(cpu.estimate_cycles(0xE1A0_0211), 2);

        // PC writes still use their pipeline-refill estimate, not the
        // ordinary non-branch ALU estimate.
        assert_eq!(cpu.estimate_cycles(0xE1A0_F211), 3);

        // Special instruction classes must not be mistaken for register shifts.
        assert_eq!(cpu.estimate_cycles(0xE10F_0000), 1); // MRS r0, CPSR
        assert_eq!(cpu.estimate_cycles(0xE128_F000), 1); // MSR CPSR_f, r0
        assert_eq!(cpu.estimate_cycles(0xE000_0291), 2); // MUL r0, r1, r2
        assert_eq!(cpu.estimate_cycles(0xE102_1093), 4); // SWP r1, r3, [r2]
    }

    #[test]
    fn arm_data_processing_pc_writes_include_pipeline_refill() {
        let cpu = Cpu::new();
        assert_eq!(cpu.estimate_cycles(0xE1A0_F000), 3); // MOV pc, r0
        assert_eq!(cpu.estimate_cycles(0xE080_F001), 3); // ADD pc, r0, r1
        assert_eq!(cpu.estimate_cycles(0xE1A0_0001), 1); // MOV r0, r1
    }

    #[test]
    fn arm_swap_cycle_estimates_include_read_modify_write_cost() {
        let cpu = Cpu::new();

        // SWP and SWPB must not fall through to the shared halfword pattern.
        assert_eq!(cpu.estimate_cycles(0xE102_1093), 4);
        assert_eq!(cpu.estimate_cycles(0xE142_4095), 4);
        // A real LDRH encoding retains the existing halfword estimate.
        assert_eq!(cpu.estimate_cycles(0xE1D2_10B0), 3);
    }

    #[test]
    fn thumb_high_register_control_flow_estimates_pipeline_refill() {
        let mut cpu = Cpu::new();
        cpu.thumb = true;

        assert_eq!(cpu.estimate_cycles(0x4700), 3); // BX r0
        assert_eq!(cpu.estimate_cycles(0x4487), 3); // ADD pc, r0
        assert_eq!(cpu.estimate_cycles(0x4687), 3); // MOV pc, r0
        assert_eq!(cpu.estimate_cycles(0x4607), 1); // MOV r7, r0
        assert_eq!(cpu.estimate_cycles(0x4507), 1); // CMP r0, r0
    }

    #[test]
    fn cycle_estimates_scale_with_arm_and_thumb_block_transfers() {
        let mut cpu = Cpu::new();

        // ARM STMIA r0!, {r1-r3}: fetch plus three word transfers.
        assert_eq!(cpu.estimate_cycles(0xE8A0_000E), 4);
        // ARM LDMIA r0!, {r1-r3,pc}: transfers plus pipeline refill.
        assert_eq!(cpu.estimate_cycles(0xE8B0_800E), 7);

        cpu.thumb = true;
        // Thumb STMIA r0!, {r1-r3}: fetch plus three transfers.
        assert_eq!(cpu.estimate_cycles(0xC00E), 4);
        // Thumb POP {r0-r2,pc}: fetch, four transfers and pipeline refill.
        assert_eq!(cpu.estimate_cycles(0xBD07), 7);
    }

    #[test]
    fn failed_arm_condition_advances_peripherals_by_one_cycle() {
        // Keep this test focused on condition handling, not Game Pak wait states.
        let mut bus = bus_with_program(&[0x03A0_002A]); // MOVEQ r0, #42 (EQ fails after reset).
        bus.write32(0x0300_0000, 0x03A0_002A);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0300_0000);

        // The timer is one tick from overflow. A condition-failed instruction
        // is skipped, but the CPU still spends one sequential fetch cycle.
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x00C0);
        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert!(matches!(outcome, StepOutcome::ConditionFailed { .. }));
        assert_eq!(cycles, 1);
        assert_eq!(cpu.register(0), Some(0));
        assert_eq!(cpu.register(15), Some(0x0300_0004));
        // Overflow reloads the configured value, which is FFFF here;
        // the timer IRQ flag is the observable proof that the tick occurred.
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 3), 1 << 3);
    }

    #[test]
    fn thumb_bx_pc_is_rejected_as_unpredictable() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);
        // BX PC has Rm=15, which ARM7TDMI does not define predictably.
        bus.write16(0x0200_0000, 0x4778);

        let result = cpu.step(&mut bus);

        assert_eq!(
            result,
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction: 0x4778,
            })
        );
        assert!(cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0000));
    }

    #[test]
    fn thumb_high_register_add_reads_unaligned_pipeline_pc() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0002);
        // ADD r8, pc. High-register ALU operations read PC+4 without word alignment.
        bus.write16(0x0200_0002, 0x44F8);

        cpu.step(&mut bus).unwrap();

        assert_eq!(cpu.register(8), Some(0x0200_0006));
        assert_eq!(cpu.register(15), Some(0x0200_0004));
    }

    #[test]
    fn thumb_register_shift_by_zero_updates_nz_and_preserves_carry() {
        // LSL r0, r1 with r1=0 leaves r0/C unchanged but still updates N/Z.
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);
        cpu.set_register(0, 0);
        cpu.set_register(1, 0);
        cpu.flags = ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: true,
        };
        bus.write16(0x0200_0000, 0x4088);

        cpu.step(&mut bus).unwrap();

        assert_eq!(cpu.register(0), Some(0));
        assert_eq!(
            cpu.flags(),
            ConditionFlags {
                negative: false,
                zero: true,
                carry: true,
                overflow: true,
            }
        );
    }

    #[test]
    fn timed_thumb_conditional_branch_uses_taken_or_sequential_cycle_count() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.thumb = true;
        cpu.set_register(15, 0x0200_0000);

        // EQ is false with reset flags: continue sequentially in one cycle.
        bus.write16(0x0200_0000, 0xD001);
        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cycles, 1);
        assert_eq!(cpu.register(15), Some(0x0200_0002));

        // BNE +2 is taken because Z is clear.
        cpu.set_register(15, 0x0200_0000);
        bus.write16(0x0200_0000, 0xD101);
        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cycles, 3);
        assert_eq!(cpu.register(15), Some(0x0200_0006));

        // A taken BNE with displacement -2 lands at address+2, the same PC
        // as sequential execution. It must still pay the pipeline-refill cost.
        cpu.set_register(15, 0x0200_0000);
        bus.write16(0x0200_0000, 0xD1FF);
        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cycles, 3);
        assert_eq!(cpu.register(15), Some(0x0200_0002));
    }

    #[test]
    fn timed_step_advances_memory_timers_after_instruction_execution() {
        let mut bus = bus_with_program(&[0xE3A0_002A]); // MOV r0, #42
        bus.write16(0x0400_0100, 0xFFFE);
        bus.write16(0x0400_0102, 0x0080); // Enable timer 0, divisor 1.

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert!(matches!(outcome, StepOutcome::Executed { .. }));
        assert_eq!(cycles, 1);
        assert_eq!(cpu.register(0), Some(42));
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
    }

    #[test]
    fn executes_data_processing_and_updates_flags() {
        let mut bus = bus_with_program(&[
            0xE3A0_002A, // MOV r0, #42
            0xE280_1008, // ADD r1, r0, #8
            0xE351_0032, // CMP r1, #50
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.step(&mut bus).unwrap();
        cpu.step(&mut bus).unwrap();
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(42));
        assert_eq!(cpu.register(1), Some(50));
        assert!(cpu.flags().zero);
        assert!(cpu.flags().carry);
    }

    #[test]
    fn arm_mrs_reads_modeled_cpsr_flags_and_thumb_state() {
        let mut bus = bus_with_program(&[0xE10F_2000]); // MRS r2, CPSR
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.flags = ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: true,
        };

        cpu.step(&mut bus).unwrap();

        // MRS executes in ARM state, so the modeled T bit is clear.
        assert_eq!(cpu.register(2), Some(0xB000_0000));
        assert!(!cpu.is_thumb());
        assert!(cpu.flags().negative);
        assert!(cpu.flags().carry);
    }

    #[test]
    fn arm_mrs_rejects_spsr_and_msr_control_fields_without_mutating_flags() {
        let mut bus = bus_with_program(&[
            0xE14F_2000, // MRS r2, SPSR (no SPSR model)
            0xE121_F000, // MSR CPSR_c, r0 (control field not modeled)
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0xF000_0000);
        cpu.flags = ConditionFlags {
            negative: false,
            zero: true,
            carry: false,
            overflow: false,
        };

        assert!(matches!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction { .. })
        ));
        assert_eq!(cpu.register(2), Some(0));
        assert!(cpu.flags().zero);

        assert!(matches!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction { .. })
        ));
        assert!(!cpu.flags().negative);
        assert!(cpu.flags().zero);
        assert!(!cpu.flags().carry);
        assert!(!cpu.flags().overflow);
    }

    #[test]
    fn arm_msr_updates_cpsr_condition_flags_from_register_and_immediate() {
        let mut bus = bus_with_program(&[
            0xE128_F000, // MSR CPSR_f, r0
            0xE328_F4F0, // MSR CPSR_f, #0xF0000000 (rotated immediate)
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0xA000_0000);

        cpu.step(&mut bus).unwrap();
        assert_eq!(
            cpu.flags(),
            ConditionFlags {
                negative: true,
                zero: false,
                carry: true,
                overflow: false,
            }
        );

        cpu.step(&mut bus).unwrap();
        assert_eq!(
            cpu.flags(),
            ConditionFlags {
                negative: true,
                zero: true,
                carry: true,
                overflow: true,
            }
        );
    }

    #[test]
    fn arm_data_processing_exception_return_is_rejected_without_spsr() {
        // MOVS pc, lr restores CPSR from the current SPSR on ARM7TDMI.
        // The core has no SPSR/exception-mode model yet, so it must reject
        // this instruction rather than silently changing flags and branching.
        let mut bus = bus_with_program(&[0xE1B0_F00E]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction: 0xE1B0_F00E,
            })
        );
    }

    #[test]
    fn failed_condition_skips_instruction() {
        let mut bus = bus_with_program(&[0x03A0_0001]); // MOVEQ r0, #1
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        assert!(matches!(
            cpu.step(&mut bus),
            Ok(StepOutcome::ConditionFailed { .. })
        ));
        assert_eq!(cpu.register(0), Some(0));
        assert_eq!(cpu.register(15), Some(0x0200_0004));
    }

    #[test]
    fn branch_link_sets_return_address() {
        let mut bus = bus_with_program(&[0xEB00_0000]); // BL to address + 8
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(14), Some(0x0200_0004));
        assert_eq!(cpu.register(15), Some(0x0200_0008));
    }

    #[test]
    fn block_transfers_store_and_load_register_lists_with_writeback() {
        let mut bus = bus_with_program(&[
            0xE8AD_0003, // STMIA sp!, {r0, r1}
            0xE8BD_000C, // LDMIA sp!, {r2, r3}
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(13, 0x0200_0010);
        cpu.set_register(0, 0x1122_3344);
        cpu.set_register(1, 0xAABB_CCDD);

        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0010), 0x1122_3344);
        assert_eq!(bus.read32(0x0200_0014), 0xAABB_CCDD);
        assert_eq!(cpu.register(13), Some(0x0200_0018));

        // Reload the same memory range to exercise ascending register ordering.
        cpu.set_register(13, 0x0200_0010);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x1122_3344));
        assert_eq!(cpu.register(3), Some(0xAABB_CCDD));
        assert_eq!(cpu.register(13), Some(0x0200_0018));
    }

    #[test]
    fn thumb_stmia_handles_base_register_in_the_register_list() {
        let mut bus = bus_with_program(&[]);
        // STMIA r1!, {r0, r1}: because r1 is not the lowest listed
        // register, the original base value is stored for r1.
        bus.write16(0x0200_0000, 0xC103);
        // STMIA r0!, {r0, r1}: because r0 is the lowest listed register,
        // the updated base value is stored for r0.
        bus.write16(0x0200_0002, 0xC003);

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 0x1122_3344);
        cpu.set_register(1, 0x0200_0100);

        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0100), 0x1122_3344);
        assert_eq!(bus.read32(0x0200_0104), 0x0200_0100);
        assert_eq!(cpu.register(1), Some(0x0200_0108));

        // Use a fresh base to exercise the lowest-register case.
        cpu.set_register(0, 0x0200_0120);
        cpu.set_register(1, 0xAABB_CCDD);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0120), 0x0200_0128);
        assert_eq!(bus.read32(0x0200_0124), 0xAABB_CCDD);
        assert_eq!(cpu.register(0), Some(0x0200_0128));
    }

    #[test]
    fn arm_stm_writeback_stores_base_according_to_register_order() {
        // STMIA r1!, {r0, r1}: base is not the lowest register, so memory
        // receives the original base for r1 while write-back updates r1.
        let mut bus = bus_with_program(&[0xE8A1_0003]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x1122_3344);
        cpu.set_register(1, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0100), 0x1122_3344);
        assert_eq!(bus.read32(0x0200_0104), 0x0200_0100);
        assert_eq!(cpu.register(1), Some(0x0200_0108));

        // STMIA r0!, {r0, r1}: base is the lowest register, so the updated
        // base is stored in its slot.
        let mut bus = bus_with_program(&[0xE8A0_0003]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0120);
        cpu.set_register(1, 0xAABB_CCDD);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0120), 0x0200_0128);
        assert_eq!(bus.read32(0x0200_0124), 0xAABB_CCDD);
        assert_eq!(cpu.register(0), Some(0x0200_0128));
    }

    #[test]
    fn arm_stmdb_writeback_stores_base_according_to_register_order() {
        // STMDB r0!, {r0, r1}: the base is the lowest listed register, so
        // its memory slot receives the decremented base value.
        let mut bus = bus_with_program(&[0xE920_0003]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0120);
        cpu.set_register(1, 0xAABB_CCDD);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0118), 0x0200_0118);
        assert_eq!(bus.read32(0x0200_011C), 0xAABB_CCDD);
        assert_eq!(cpu.register(0), Some(0x0200_0118));

        // STMDB r1!, {r0, r1}: because r0 is listed first, r1's slot
        // contains the original base even though write-back decrements r1.
        let mut bus = bus_with_program(&[0xE921_0003]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x1122_3344);
        cpu.set_register(1, 0x0200_0120);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0118), 0x1122_3344);
        assert_eq!(bus.read32(0x0200_011C), 0x0200_0120);
        assert_eq!(cpu.register(1), Some(0x0200_0118));
    }

    #[test]
    fn arm_ldm_writeback_respects_base_register_order() {
        // If Rn is the lowest register in the list, the loaded value wins.
        let mut bus = bus_with_program(&[
            0xE8B1_0006, // LDMIA r1!, {r1, r2}
            0xE8B1_000C, // LDMIA r1!, {r2, r3}
        ]);
        bus.write32(0x0200_0100, 0xAABB_CCDD);
        bus.write32(0x0200_0104, 0x1122_3344);
        bus.write32(0x0200_0200, 0x5566_7788);
        bus.write32(0x0200_0204, 0x99AA_BBCC);

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(1), Some(0xAABB_CCDD));
        assert_eq!(cpu.register(2), Some(0x1122_3344));

        // When Rn is not the lowest register in the list, write-back wins.
        cpu.set_register(1, 0x0200_0200);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x5566_7788));
        assert_eq!(cpu.register(3), Some(0x99AA_BBCC));
        assert_eq!(cpu.register(1), Some(0x0200_0208));
    }

    #[test]
    fn arm_empty_block_transfer_uses_pc_and_full_writeback_span() {
        // STMIA r0!, {} stores the ARM-visible PC at base + 0x3C and advances
        // the base as though all 16 registers had been transferred.
        let mut bus = bus_with_program(&[0xE8A0_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_013C), 0x0200_000C);
        assert_eq!(cpu.register(0), Some(0x0200_0140));

        // LDMDB r1!, {} loads PC from base - 4 while decrementing by 0x40.
        let mut bus = bus_with_program(&[0xE931_0000]);
        bus.write32(0x0200_011C, 0x0200_0041);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0120);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(15), Some(0x0200_0040));
        assert_eq!(cpu.register(1), Some(0x0200_00E0));
    }

    #[test]
    fn unaligned_halfword_loads_follow_arm7tdmi_rotation_and_signed_byte_rules() {
        let mut bus = bus_with_program(&[
            0xE1D1_20B0, // LDRH r2, [r1]
            0xE1D1_30F0, // LDRSH r3, [r1]
        ]);
        bus.write16(0x0200_0100, 0x80FF);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0101);

        // An odd LDRH rotates the aligned halfword by eight bits.
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x0000_FF80));

        // An odd LDRSH is treated as LDRSB and sign-extends the addressed byte.
        cpu.reset(0x0200_0004);
        cpu.set_register(1, 0x0200_0101);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(3), Some(0xFFFF_FF80));
    }

    #[test]
    fn halfword_transfers_report_load_and_store_cycle_estimates() {
        let mut bus = bus_with_program(&[
            0xE1D1_20B0, // LDRH r2, [r1]
            0xE1C1_20B2, // STRH r2, [r1, #2]
            0xE1D1_30F0, // LDRSH r3, [r1]
        ]);
        bus.write16(0x0200_0100, 0x80FF);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0100);

        let (_, load_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(load_cycles, 3);
        assert_eq!(cpu.register(2), Some(0x0000_80FF));

        let (_, store_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(store_cycles, 2);
        assert_eq!(bus.read16(0x0200_0102), 0x80FF);

        // This encoding carries a +0xF immediate halfword offset.
        // Keep the cycle assertion independent from the dedicated odd-address test.
        bus.write16(0x0200_0110, 0xFF80);
        cpu.set_register(1, 0x0200_0101);
        let (_, signed_load_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(signed_load_cycles, 3);
        // Odd-address LDRSH value semantics are covered independently above;
        // this test focuses on the cycle estimate for this encoded transfer.
    }

    #[test]
    fn arm_swap_and_swap_byte_report_expected_instruction_cycles() {
        let mut bus = bus_with_program(&[
            0xE102_1093, // SWP r1, r3, [r2]
            0xE142_4095, // SWPB r4, r5, [r2]
        ]);
        bus.write32(0x0200_0100, 0x1234_5678);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(2, 0x0200_0100);
        cpu.set_register(3, 0xAABB_CCDD);
        cpu.set_register(5, 0x42);

        assert_eq!(cpu.step_with_cycles(&mut bus).unwrap().1, 4);
        assert_eq!(cpu.register(1), Some(0x1234_5678));
        assert_eq!(bus.read32(0x0200_0100), 0xAABB_CCDD);
        assert_eq!(cpu.step_with_cycles(&mut bus).unwrap().1, 4);
        assert_eq!(cpu.register(4), Some(0xDD));
        assert_eq!(bus.read8(0x0200_0100), 0x42);
    }

    #[test]
    fn halfword_and_signed_loads_use_correct_width_and_sign_extension() {
        let mut bus = bus_with_program(&[
            0xE1C1_00B0, // STRH r0, [r1]
            0xE1D1_20B0, // LDRH r2, [r1]
            0xE1D1_20D0, // LDRSB r2, [r1]
            0xE1D1_30F0, // LDRSH r3, [r1]
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0000_80FF);
        cpu.set_register(1, 0x0200_0100);

        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read16(0x0200_0100), 0x80FF);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x0000_80FF));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0xFFFF_FFFF));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(3), Some(0xFFFF_80FF));
    }

    #[test]
    fn arm_swap_and_swap_byte_exchange_memory_and_register_values() {
        let mut bus = bus_with_program(&[
            0xE102_1093, // SWP r1, r3, [r2]
            0xE142_4095, // SWPB r4, r5, [r2]
        ]);
        bus.write32(0x0200_0100, 0x1234_5678);
        bus.write8(0x0200_0100, 0x78);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(2, 0x0200_0100);
        cpu.set_register(3, 0xAABB_CCDD);
        cpu.set_register(5, 0x42);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(1), Some(0x1234_5678));
        assert_eq!(bus.read32(0x0200_0100), 0xAABB_CCDD);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(4), Some(0xDD));
        assert_eq!(bus.read8(0x0200_0100), 0x42);
        assert_eq!(cpu.register(2), Some(0x0200_0100));
    }

    #[test]
    fn arm_word_swap_rejects_unaligned_address_without_modifying_state() {
        let instruction = 0xE102_1093; // SWP r1, r3, [r2]
        let mut bus = bus_with_program(&[instruction]);
        bus.write32(0x0200_0100, 0x1234_5678);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0xDEAD_BEEF);
        cpu.set_register(2, 0x0200_0101);
        cpu.set_register(3, 0xAABB_CCDD);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
        assert_eq!(cpu.register(1), Some(0xDEAD_BEEF));
        assert_eq!(bus.read32(0x0200_0100), 0x1234_5678);
    }

    #[test]
    fn long_multiply_supports_unsigned_signed_and_accumulating_forms() {
        // UMULL r1, r0, r2, r3 => r0:r1 = 0xFFFF_FFFF * 2.
        let mut bus = bus_with_program(&[
            0xE081_0392, // UMULL r0, r1, r2, r3 (encoding checked below)
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(2, 0xFFFF_FFFF);
        cpu.set_register(3, 2);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0xFFFF_FFFE));
        assert_eq!(cpu.register(1), Some(1));
    }

    #[test]
    fn long_multiply_accumulate_and_signed_flags_are_correct() {
        // SMLALS r0, r1, r2, r3: accumulator 5 + (-2 * 3) = -1.
        let mut bus = bus_with_program(&[
            0xE0F1_0392, // SMLALS r0, r1, r2, r3
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 5);
        cpu.set_register(1, 0);
        cpu.set_register(2, (-2_i32) as u32);
        cpu.set_register(3, 3);
        cpu.flags.carry = true;
        cpu.flags.overflow = true;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0xFFFF_FFFF));
        assert_eq!(cpu.register(1), Some(0xFFFF_FFFF));
        assert!(cpu.flags().negative);
        assert!(!cpu.flags().zero);
        // Long multiply with S set updates N/Z only.
        assert!(cpu.flags().carry);
        assert!(cpu.flags().overflow);
    }

    #[test]
    fn long_multiply_signed_and_unsigned_accumulate_forms_are_correct() {
        let mut bus = bus_with_program(&[
            0xE0C1_0392, // SMULL r0, r1, r2, r3
            0xE0A1_0392, // UMLAL r0, r1, r2, r3
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(2, 0xFFFF_FFFE); // -2 signed, large unsigned.
        cpu.set_register(3, 3);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0xFFFF_FFFA));
        assert_eq!(cpu.register(1), Some(0xFFFF_FFFF));

        // UMLAL adds unsigned (0xFFFF_FFFE * 3) to the previous 64-bit result.
        cpu.set_register(0, 0xFFFF_FFFA);
        cpu.set_register(1, 0xFFFF_FFFF);
        cpu.step(&mut bus).unwrap();
        let expected = (u64::from(0xFFFF_FFFF_u32) << 32) | u64::from(0xFFFF_FFFA_u32);
        let expected = expected.wrapping_add(u64::from(0xFFFF_FFFE_u32) * 3);
        assert_eq!(cpu.register(0), Some(expected as u32));
        assert_eq!(cpu.register(1), Some((expected >> 32) as u32));
    }

    #[test]
    fn long_multiply_rejects_r_m_aliasing_a_destination() {
        // UMULL r0, r1, r0, r3 is unpredictable on ARM7TDMI because Rm
        // aliases one of the destination registers.
        let instruction = 0xE081_0390;
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 7);
        cpu.set_register(3, 9);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
        assert_eq!(cpu.register(0), Some(7));
        assert_eq!(cpu.register(1), Some(0));
    }

    #[test]
    fn multiply_and_multiply_accumulate_update_results_and_flags() {
        let mut bus = bus_with_program(&[
            0xE000_0291, // MUL r0, r1, r2
            0xE023_5491, // MLA r3, r1, r4, r5
            0xE010_0291, // MULS r0, r1, r2
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 6);
        cpu.set_register(2, 7);
        cpu.set_register(4, 8);
        cpu.set_register(5, 9);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(42));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(3), Some(57));
        cpu.set_register(1, 0);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0));
        assert!(cpu.flags().zero);
        assert!(!cpu.flags().negative);
    }

    #[test]
    fn thumb_state_executes_immediate_arithmetic_and_branches() {
        let mut bus = bus_with_program(&[]);
        // Thumb code is written directly to EWRAM as halfwords.
        bus.write16(0x0200_0000, 0x2007); // MOVS r0, #7
        bus.write16(0x0200_0002, 0x3005); // ADDS r0, #5
        bus.write16(0x0200_0004, 0x280C); // CMP r0, #12
        bus.write16(0x0200_0006, 0xD000); // BEQ +0 (to address +10)
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(7));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(12));
        cpu.step(&mut bus).unwrap();
        assert!(cpu.flags().zero);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(15), Some(0x0200_000A));
    }

    #[test]
    fn thumb_long_branch_with_link_sets_target_and_thumb_return_address() {
        let mut bus = bus_with_program(&[]);
        // BL +0: high part at 0, low part at 2, target at 4.
        bus.write16(0x0200_0000, 0xF000);
        bus.write16(0x0200_0002, 0xF800);
        bus.write16(0x0200_0004, 0x2007); // MOVS r0, #7 at the target.
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(14), Some(0x0200_0004));
        assert_eq!(cpu.register(15), Some(0x0200_0002));

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(15), Some(0x0200_0004));
        assert_eq!(cpu.register(14), Some(0x0200_0005));

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(7));
    }

    #[test]
    fn thumb_long_branch_with_link_sign_extends_negative_high_displacement() {
        let mut bus = bus_with_program(&[]);
        // The first BL halfword encodes -4096; the second half branches to
        // 0x02000004 instead of wrapping forward from the call site.
        bus.write16(0x0200_1000, 0xF7FF);
        bus.write16(0x0200_1002, 0xF800);
        bus.write16(0x0200_0004, 0x2009);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_1000);
        cpu.thumb = true;

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(14), Some(0x0200_0004));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(15), Some(0x0200_0004));
        assert_eq!(cpu.register(14), Some(0x0200_1005));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(9));
    }

    #[test]
    fn arm_register_controlled_shifts_cover_boundaries_and_carry() {
        assert_eq!(
            Cpu::shift_register(0x8000_0001, 0, 1),
            (0x0000_0002, Some(true))
        );
        assert_eq!(Cpu::shift_register(0x8000_0001, 0, 32), (0, Some(true)));
        assert_eq!(Cpu::shift_register(0x8000_0001, 0, 33), (0, Some(false)));
        assert_eq!(Cpu::shift_register(0x8000_0001, 1, 32), (0, Some(true)));
        assert_eq!(
            Cpu::shift_register(0x8000_0001, 2, 32),
            (u32::MAX, Some(true))
        );
        assert_eq!(
            Cpu::shift_register(0x8000_0001, 2, 40),
            (u32::MAX, Some(true))
        );
        assert_eq!(
            Cpu::shift_register(0x8000_0001, 3, 32),
            (0x8000_0001, Some(true))
        );
        assert_eq!(Cpu::shift_register(0x8000_0001, 0, 0), (0x8000_0001, None));
    }

    #[test]
    fn arm_register_shift_reads_pc_as_address_plus_twelve() {
        let mut bus = bus_with_program(&[0xE1A0_211F]); // MOV r2, pc, LSL r1
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0);

        cpu.step(&mut bus).unwrap();

        assert_eq!(cpu.register(2), Some(0x0200_000C));
    }

    #[test]
    fn arm_register_shift_rejects_pc_as_shift_amount_register() {
        let instruction = 0xE1A0_0F12; // MOV r0, r2, LSL pc (unpredictable)
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(2, 7);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
        assert_eq!(cpu.register(0), Some(0));
    }

    #[test]
    fn arm_data_processing_uses_register_controlled_shift_amount() {
        let mut bus = bus_with_program(&[0xE1A0_0312]); // MOV r0, r2, LSL r3
        let mut cpu = Cpu::new();
        cpu.reset(0x0800_00C0);
        cpu.set_register(2, 0x0000_0003);
        cpu.set_register(3, 4);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x0000_0030));
    }

    #[test]
    fn thumb_add_pc_and_sp_relative_address_calculations() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xA001); // ADD r0, PC, #4
        bus.write16(0x0200_0002, 0xA902); // ADD r1, SP, #8
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(13, 0x0200_0100);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x0200_0008));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(1), Some(0x0200_0108));
    }

    #[test]
    fn thumb_load_store_transfers_words_bytes_and_halfwords() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x6008); // STR r0, [r1]
        bus.write16(0x0200_0002, 0x680A); // LDR r2, [r1]
        bus.write16(0x0200_0004, 0x704B); // STRB r3, [r1, #1]
        bus.write16(0x0200_0006, 0x784C); // LDRB r4, [r1, #1]
        bus.write16(0x0200_0008, 0x800D); // STRH r5, [r1]
        bus.write16(0x0200_000A, 0x880E); // LDRH r6, [r1]
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 0x1234_5678);
        cpu.set_register(1, 0x0200_0100);
        cpu.set_register(3, 0xAA);
        cpu.set_register(5, 0xBEEF);

        for _ in 0..6 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.register(2), Some(0x1234_5678));
        assert_eq!(cpu.register(4), Some(0x0000_00AA));
        assert_eq!(cpu.register(6), Some(0x0000_BEEF));
    }

    #[test]
    fn thumb_push_pop_and_stack_adjustment_preserve_registers() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xB403); // PUSH {r0, r1}
        bus.write16(0x0200_0002, 0xB081); // SUB sp, #4
        bus.write16(0x0200_0004, 0xB001); // ADD sp, #4
        bus.write16(0x0200_0006, 0xBC0C); // POP {r2, r3}
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(13, 0x0200_0120);
        cpu.set_register(0, 0x1122_3344);
        cpu.set_register(1, 0xAABB_CCDD);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(13), Some(0x0200_0118));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(13), Some(0x0200_0114));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(13), Some(0x0200_0118));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x1122_3344));
        assert_eq!(cpu.register(3), Some(0xAABB_CCDD));
        assert_eq!(cpu.register(13), Some(0x0200_0120));
    }

    #[test]
    fn thumb_multiple_transfers_store_load_and_write_back() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xC006); // STMIA r0!, {r1, r2}
        bus.write16(0x0200_0002, 0xC818); // LDMIA r0!, {r3, r4}
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x1122_3344);
        cpu.set_register(2, 0xAABB_CCDD);

        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0100), 0x1122_3344);
        assert_eq!(bus.read32(0x0200_0104), 0xAABB_CCDD);
        assert_eq!(cpu.register(0), Some(0x0200_0108));

        cpu.set_register(0, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(3), Some(0x1122_3344));
        assert_eq!(cpu.register(4), Some(0xAABB_CCDD));
        assert_eq!(cpu.register(0), Some(0x0200_0108));
    }

    #[test]
    fn bx_rejects_pc_as_target_register() {
        let instruction = 0xE12F_FF1F; // BX pc (unpredictable on ARM7TDMI)
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
        assert!(!cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0000));
    }

    #[test]
    fn bx_switches_between_arm_and_thumb_instruction_sets() {
        let mut bus = bus_with_program(&[0xE12F_FF11]); // BX r1
        bus.write16(0x0200_0008, 0x2201); // MOVS r2, #1
        bus.write16(0x0200_000A, 0x4700); // BX r0
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0010);
        cpu.set_register(1, 0x0200_0009);

        cpu.step(&mut bus).unwrap();
        assert!(cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0008));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(1));
        cpu.step(&mut bus).unwrap();
        assert!(!cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0010));
    }

    #[test]
    fn thumb_immediate_lsr_and_asr_zero_encode_shift_by_32() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x0808); // LSRS r0, r1, #0 (shift by 32)
        bus.write16(0x0200_0002, 0x100A); // ASRS r2, r1, #0 (shift by 32)
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(1, 0x8000_0001);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0));
        assert!(!cpu.flags().negative);
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(u32::MAX));
        assert!(cpu.flags().negative);
        assert!(cpu.flags().carry);
    }

    #[test]
    fn thumb_register_shifts_and_negate_update_results_and_flags() {
        let mut bus = bus_with_program(&[0]);
        // LSL r0, r1; LSR r2, r3; ASR r4, r5; ROR r6, r7; NEG r0, r1.
        for (index, instruction) in [
            0x4088_u16, // LSL r0, r1
            0x40DA,     // LSR r2, r3
            0x412C,     // ASR r4, r5
            0x41FE,     // ROR r6, r7
            0x4248,     // NEG r0, r1
        ]
        .into_iter()
        .enumerate()
        {
            bus.write16(0x0200_0000 + index as u32 * 2, instruction);
        }

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 0x8000_0001);
        cpu.set_register(1, 1);
        cpu.set_register(2, 0x8000_0001);
        cpu.set_register(3, 1);
        cpu.set_register(4, 0x8000_0001);
        cpu.set_register(5, 1);
        cpu.set_register(6, 0x8000_0001);
        cpu.set_register(7, 1);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(2));
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x4000_0000));
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(4), Some(0xC000_0000));
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(6), Some(0xC000_0000));
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(u32::MAX));
        assert!(cpu.flags().negative);
    }

    #[test]
    fn thumb_adc_and_sbc_consume_and_update_carry_correctly() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x4148); // ADCS r0, r1
        bus.write16(0x0200_0002, 0x419A); // SBCS r2, r3
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.flags.carry = true;
        cpu.set_register(0, u32::MAX);
        cpu.set_register(1, 0);
        cpu.set_register(2, 0);
        cpu.set_register(3, 1);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0));
        assert!(cpu.flags().zero);
        assert!(cpu.flags().carry);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(u32::MAX));
        assert!(cpu.flags().negative);
        assert!(!cpu.flags().zero);
        assert!(!cpu.flags().carry);
    }

    #[test]
    fn setting_pc_in_thumb_state_preserves_halfword_alignment() {
        let mut bus = bus_with_program(&[]);
        // A Thumb MOV r0, #7 placed at an address ending in 2.
        bus.write16(0x0200_0002, 0x2007);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;

        assert!(cpu.set_register(15, 0x0200_0003));
        assert_eq!(cpu.register(15), Some(0x0200_0002));
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(7));
        assert_eq!(cpu.register(15), Some(0x0200_0004));
    }

    #[test]
    fn thumb_register_shifts_handle_32_and_over_32_bit_amounts() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x4088); // LSL r0, r1
        bus.write16(0x0200_0002, 0x40DA); // LSR r2, r3
        bus.write16(0x0200_0004, 0x412C); // ASR r4, r5
        bus.write16(0x0200_0006, 0x41FE); // ROR r6, r7

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 0x8000_0001);
        cpu.set_register(1, 32);
        cpu.set_register(2, 0x8000_0001);
        cpu.set_register(3, 33);
        cpu.set_register(4, 0x8000_0001);
        cpu.set_register(5, 40);
        cpu.set_register(6, 0x8000_0001);
        cpu.set_register(7, 32);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0));
        assert!(cpu.flags().carry); // LSL by 32 takes bit 0 into carry.

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0));
        assert!(!cpu.flags().carry); // LSR above 32 produces zero and clears carry.

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(4), Some(u32::MAX));
        assert!(cpu.flags().carry); // ASR above 32 sign-fills and preserves the sign in carry.

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(6), Some(0x8000_0001));
        assert!(cpu.flags().carry); // ROR by a nonzero multiple of 32 preserves bit 31.
    }

    #[test]
    fn arm_mul_rejects_nonzero_reserved_accumulator_register() {
        // MUL encodings require the Rn field to be zero (unlike MLA).
        let instruction = 0xE001_1090;
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
    }

    #[test]
    fn arm_multiply_rejects_destination_equal_to_first_operand() {
        // MUL r0, r0, r1 and MLA r0, r0, r1, r2 are unpredictable on ARM7TDMI.
        for instruction in [0xE000_0190_u32, 0xE020_2190] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(0, 3);
            cpu.set_register(1, 7);
            cpu.set_register(2, 11);

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
            assert_eq!(cpu.register(0), Some(3));
        }
    }

    #[test]
    fn arm_store_pc_uses_pipeline_visible_address_plus_twelve() {
        // STR pc, [r0] stores the ARM7TDMI pipeline-visible PC (instruction
        // address + 12), not the already-advanced internal PC (address + 4).
        let mut bus = bus_with_program(&[0xE580_F000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read32(0x0200_0100), 0x0200_000C);

        // STRB pc, [r0] uses the low byte of the same architectural value.
        let mut bus = bus_with_program(&[0xE5C0_F000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.read8(0x0200_0100), 0x0C);
    }

    #[test]
    fn arm_word_load_rotates_unaligned_bus_data_for_each_byte_offset() {
        for (offset, expected) in [(1, 0x7812_3456), (2, 0x5678_1234), (3, 0x3456_7812)] {
            let mut bus = bus_with_program(&[0xE591_2000]); // LDR r2, [r1]
            bus.write32(0x0200_0100, 0x1234_5678);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(1, 0x0200_0100 + offset);

            cpu.step(&mut bus).unwrap();

            assert_eq!(cpu.register(2), Some(expected), "offset {offset}");
        }
    }

    #[test]
    fn thumb_word_load_rotates_unaligned_bus_data_for_each_addressing_form() {
        // Immediate-offset LDR r2, [r1, #0].
        let mut immediate_bus = bus_with_program(&[]);
        immediate_bus.write16(0x0200_0000, 0x680A);
        immediate_bus.write32(0x0200_0100, 0x1234_5678);
        let mut immediate_cpu = Cpu::new();
        immediate_cpu.reset(0x0200_0000);
        immediate_cpu.thumb = true;
        immediate_cpu.set_register(1, 0x0200_0102);
        immediate_cpu.step(&mut immediate_bus).unwrap();
        assert_eq!(immediate_cpu.register(2), Some(0x5678_1234));

        // Register-offset LDR r2, [r1, r3].
        let mut register_bus = bus_with_program(&[]);
        register_bus.write16(0x0200_0000, 0x58CA);
        register_bus.write32(0x0200_0100, 0x1234_5678);
        let mut register_cpu = Cpu::new();
        register_cpu.reset(0x0200_0000);
        register_cpu.thumb = true;
        register_cpu.set_register(1, 0x0200_0100);
        register_cpu.set_register(3, 3);
        register_cpu.step(&mut register_bus).unwrap();
        assert_eq!(register_cpu.register(2), Some(0x3456_7812));
    }

    #[test]
    fn arm_test_and_compare_encodings_without_s_bit_are_rejected() {
        // TST, TEQ, CMP and CMN require S=1 on ARM7TDMI.
        for instruction in [0xE101_0002_u32, 0xE121_0002, 0xE141_0002, 0xE161_0002] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(1, 0xF0F0_F0F0);
            cpu.set_register(2, 0x0F0F_0F0F);
            let flags_before = cpu.flags;

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
            assert_eq!(cpu.flags, flags_before);
        }
    }

    #[test]
    fn arm_exception_return_is_rejected_until_spsr_is_modeled() {
        // SUBS pc, lr, #4 is an exception return on ARM7TDMI. It must restore
        // CPSR from SPSR, which this core does not model yet.
        let mut bus = bus_with_program(&[0xE25E_F004]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(14, 0x0200_0100);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction: 0xE25E_F004,
            })
        );
    }

    #[test]
    fn arm_and_thumb_unsigned_halfword_loads_rotate_odd_addresses() {
        // ARM LDRH r2, [r1] at an odd address rotates the aligned halfword.
        let instruction = 0xE1D1_20B0;
        let mut arm_bus = bus_with_program(&[instruction]);
        arm_bus.write16(0x0200_0100, 0x1234);
        let mut arm_cpu = Cpu::new();
        arm_cpu.reset(0x0200_0000);
        arm_cpu.set_register(1, 0x0200_0101);
        arm_cpu.step(&mut arm_bus).unwrap();
        assert_eq!(arm_cpu.register(2), Some(0x3412));

        // Thumb LDRH r2, [r1, r0] (register-offset form) uses same behavior.
        let mut thumb_bus = bus_with_program(&[]);
        thumb_bus.write16(0x0200_0100, 0x1234);
        thumb_bus.write16(0x0200_0000, 0x5A0A);
        let mut thumb_cpu = Cpu::new();
        thumb_cpu.reset(0x0200_0000);
        thumb_cpu.thumb = true;
        thumb_cpu.set_register(1, 0x0200_0101);
        thumb_cpu.set_register(0, 0);
        thumb_cpu.step(&mut thumb_bus).unwrap();
        assert_eq!(thumb_cpu.register(2), Some(0x3412));
    }

    #[test]
    fn thumb_empty_multiple_transfer_uses_pc_and_sixteen_word_writeback() {
        // Empty LDMIA behaves as loading PC and advancing the base by 0x40.
        let mut load_bus = bus_with_program(&[]);
        load_bus.write16(0x0200_0000, 0xC800); // LDMIA r0!, {}
        load_bus.write32(0x0200_0100, 0x0200_0021);
        let mut load_cpu = Cpu::new();
        load_cpu.reset(0x0200_0000);
        load_cpu.thumb = true;
        load_cpu.set_register(0, 0x0200_0100);
        load_cpu.step(&mut load_bus).unwrap();
        assert_eq!(load_cpu.register(15), Some(0x0200_0020));
        assert_eq!(load_cpu.register(0), Some(0x0200_0140));
        assert!(load_cpu.is_thumb());

        // Empty STMIA stores the Thumb-visible PC and advances by 0x40 too.
        let mut store_bus = bus_with_program(&[]);
        store_bus.write16(0x0200_0000, 0xC000); // STMIA r0!, {}
        let mut store_cpu = Cpu::new();
        store_cpu.reset(0x0200_0000);
        store_cpu.thumb = true;
        store_cpu.set_register(0, 0x0200_0100);
        store_cpu.step(&mut store_bus).unwrap();
        assert_eq!(store_bus.read32(0x0200_0100), 0x0200_0004);
        assert_eq!(store_cpu.register(0), Some(0x0200_0140));
    }

    #[test]
    fn arm_bx_cycle_estimate_includes_pipeline_refill() {
        // BX r0 enters Thumb state and refills the pipeline.
        let mut bus = bus_with_program(&[0xE12F_FF10]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0101);

        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert_eq!(cycles, 3);
        assert!(cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0200_0100));
    }

    #[test]
    fn ordinary_loads_cost_more_cycles_than_stores() {
        // ARM LDR/STR (immediate), with r1 pointing at writable EWRAM.
        for (instruction, expected) in [
            (0xE591_2000_u32, 3), // LDR r2, [r1]
            (0xE581_2000_u32, 2), // STR r2, [r1]
            (0xE1D1_20B0_u32, 3), // LDRH r2, [r1]
            (0xE1C1_20B0_u32, 2), // STRH r2, [r1]
        ] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(1, 0x0200_0100);
            let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
            assert_eq!(cycles, expected, "instruction 0x{instruction:08X}");
        }

        // Thumb LDR and STR immediate forms.
        for (instruction, expected) in [(0x680A_u16, 3), (0x600A_u16, 2)] {
            let mut bus = bus_with_program(&[]);
            bus.write16(0x0200_0000, instruction);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.thumb = true;
            cpu.set_register(1, 0x0200_0100);
            let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
            assert_eq!(cycles, expected, "instruction 0x{instruction:04X}");
        }
    }

    #[test]
    fn thumb_bl_first_half_does_not_pay_branch_refill_cycles() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xF000); // BL high displacement half
        bus.write16(0x0200_0002, 0xF800); // BL low displacement half
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;

        let (_, first_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(first_cycles, 1);
        assert_eq!(cpu.register(15), Some(0x0200_0002));

        let (_, second_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(second_cycles, 3);
        assert_eq!(cpu.register(15), Some(0x0200_0004));
    }

    #[test]
    fn thumb_multiply_cycle_estimates_follow_multiplier_width() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x4348); // MUL r0, r1
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 7);
        cpu.set_register(1, 3);
        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cycles, 2); // one-cycle multiplier plus instruction cycle

        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x4348); // MUL r0, r1
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(0, 7);
        cpu.set_register(1, 0x1234_5678);
        let (_, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cycles, 5);
    }

    #[test]
    fn thumb_empty_multiple_transfer_cycles_include_load_pc_refill() {
        let mut load_bus = bus_with_program(&[]);
        load_bus.write16(0x0200_0000, 0xC800); // LDMIA r0!, {}
        load_bus.write32(0x0200_0100, 0x0200_0020);
        let mut load_cpu = Cpu::new();
        load_cpu.reset(0x0200_0000);
        load_cpu.thumb = true;
        load_cpu.set_register(0, 0x0200_0100);
        let (_, load_cycles) = load_cpu.step_with_cycles(&mut load_bus).unwrap();
        assert_eq!(load_cycles, 19);

        let mut store_bus = bus_with_program(&[]);
        store_bus.write16(0x0200_0000, 0xC000); // STMIA r0!, {}
        let mut store_cpu = Cpu::new();
        store_cpu.reset(0x0200_0000);
        store_cpu.thumb = true;
        store_cpu.set_register(0, 0x0200_0100);
        let (_, store_cycles) = store_cpu.step_with_cycles(&mut store_bus).unwrap();
        assert_eq!(store_cycles, 17);
    }

    #[test]
    fn thumb_sp_relative_word_load_rotates_unaligned_address() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x9800); // LDR r0, [SP, #0]
        bus.write32(0x0200_0100, 0x1234_5678);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(13, 0x0200_0101);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x7812_3456));
    }

    #[test]
    fn thumb_immediate_halfword_load_rotates_odd_address() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x880A); // LDRH r2, [r1, #0]
        bus.write16(0x0200_0100, 0x1234);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(1, 0x0200_0101);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0x3412));
    }

    #[test]
    fn thumb_signed_halfword_load_uses_signed_byte_for_odd_address() {
        // Thumb LDRSH r2, [r1, r0] with an odd effective address behaves as LDRSB.
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0x5E0A);
        bus.write8(0x0200_0101, 0x80);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(1, 0x0200_0101);
        cpu.set_register(0, 0);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0xFFFF_FF80));
    }

    #[test]
    fn arm_signed_halfword_load_uses_signed_byte_for_odd_address() {
        // LDRSH r2, [r1]: odd addresses behave as signed-byte loads.
        let mut bus = bus_with_program(&[0xE1D1_20F0]);
        bus.write8(0x0200_0101, 0x80);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0101);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(2), Some(0xFFFF_FF80));
    }

    #[test]
    fn arm_halfword_and_signed_transfers_to_pc_are_rejected() {
        // ARM7TDMI does not define PC as Rd for halfword/signed-byte transfers,
        // including STRH (whose PC source value would otherwise be mis-modeled).
        for instruction in [
            0xE1D1_F0B0_u32, // LDRH
            0xE1D1_F0D0,     // LDRSB
            0xE1D1_F0F0,     // LDRSH
            0xE1C1_F0B0,     // STRH
        ] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(1, 0x0200_0100);
            bus.write16(0x0200_0100, 0x1234);
            bus.write8(0x0200_0100, 0x80);

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
        }
    }

    #[test]
    fn arm_halfword_load_with_writeback_and_same_base_is_rejected() {
        // LDRH r1, [r1], #2 is an unpredictable base/destination overlap.
        let instruction = 0xE0D1_10B2;
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0100);
        bus.write16(0x0200_0100, 0xCAFE);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
        assert_eq!(cpu.register(1), Some(0x0200_0100));
    }

    #[test]
    fn arm_halfword_pc_base_writeback_is_rejected() {
        // LDRH r0, [pc], #2 and LDRH r0, [pc, #2]! are unpredictable.
        for instruction in [0xE0DF_00B2_u32, 0xE1FF_00B2] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
            assert_eq!(cpu.register(15), Some(0x0200_0000));
        }
    }

    #[test]
    fn arm_block_transfer_rejects_pc_base_writeback() {
        // STMIA pc!, {r0} and LDMIA pc!, {r0} have unpredictable PC writeback.
        for instruction in [0xE8AF_0001_u32, 0xE8BF_0001] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(0, 0x1234_5678);

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
            assert_eq!(cpu.register(15), Some(0x0200_0000));
            assert_eq!(bus.read32(0x0200_0004), 0);
        }
    }

    #[test]
    fn arm_post_indexed_pc_base_transfer_is_rejected() {
        let mut bus = bus_with_program(&[0xE49F_0004]); // LDR r0, [pc], #4
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction: 0xE49F_0004,
            })
        );
        // The invalid write-back must not redirect execution through r15.
        assert_eq!(cpu.register(15), Some(0x0200_0000));
    }

    #[test]
    fn arm_unprivileged_single_transfers_are_rejected_without_mode_support() {
        // P=0/W=1 denotes LDRT/STRT, not ordinary post-indexed transfers.
        for instruction in [0xE4B1_0004_u32, 0xE4A1_0004] {
            let mut bus = bus_with_program(&[instruction]);
            let mut cpu = Cpu::new();
            cpu.reset(0x0200_0000);
            cpu.set_register(1, 0x0200_0100);
            cpu.set_register(0, 0x1234_5678);
            bus.write32(0x0200_0100, 0xAABB_CCDD);

            assert_eq!(
                cpu.step(&mut bus),
                Err(CpuError::UnsupportedInstruction {
                    address: 0x0200_0000,
                    instruction,
                }),
                "instruction 0x{instruction:08X}"
            );
            // Rejection must happen before a data access or base write-back.
            assert_eq!(cpu.register(1), Some(0x0200_0100));
            assert_eq!(bus.read32(0x0200_0100), 0xAABB_CCDD);
        }
    }

    #[test]
    fn arm_load_with_writeback_and_same_base_destination_is_rejected() {
        // LDR r1, [r1], #4: architecturally unpredictable on ARM7TDMI.
        let mut bus = bus_with_program(&[0xE491_1004]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0200_0100);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction: 0xE491_1004,
            })
        );
        assert_eq!(cpu.register(1), Some(0x0200_0100));
    }

    #[test]
    fn unsupported_instruction_restores_cpu_registers_flags_and_execution_state() {
        let mut bus = bus_with_program(&[0xF000_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x1234_5678);
        cpu.set_condition_flags(ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: false,
        });
        cpu.set_execution_state(false);
        cpu.set_register(15, 0x0200_0000);
        let before = cpu.snapshot();

        assert!(matches!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction { .. })
        ));
        assert_eq!(cpu.snapshot(), before);
    }

    #[test]
    fn unsupported_instruction_is_reported() {
        let mut bus = bus_with_program(&[0xF000_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        assert!(matches!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction { .. })
        ));
    }
    #[test]
    fn thumb_ldmia_base_in_list_obeys_register_order_for_writeback() {
        let mut bus = bus_with_program(&[0]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.thumb = true;

        // LDMIA r0!, {r0, r1}: because r0 is the lowest listed register,
        // the value loaded from memory wins over the updated base.
        cpu.set_register(0, 0x0200_0100);
        bus.write32(0x0200_0100, 0x1234_5678);
        bus.write32(0x0200_0104, 0x8765_4321);
        bus.write16(0x0200_0000, 0xC803);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x1234_5678));
        assert_eq!(cpu.register(1), Some(0x8765_4321));

        // LDMIA r1!, {r0, r1}: when the base is not the lowest listed
        // register, write-back wins over the loaded value for r1.
        cpu.reset(0x0200_0000);
        cpu.thumb = true;
        cpu.set_register(1, 0x0200_0120);
        bus.write32(0x0200_0120, 0xAABB_CCDD);
        bus.write32(0x0200_0124, 0x1122_3344);
        bus.write16(0x0200_0000, 0xC903);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0xAABB_CCDD));
        assert_eq!(cpu.register(1), Some(0x0200_0128));
    }

    #[test]
    fn long_multiply_cycle_estimates_distinguish_signed_and_unsigned_extension() {
        let cases = [
            (0x0000_007F, 1, 1),
            (0xFFFF_FF80, 1, 4),
            (0x0000_7FFF, 2, 2),
            (0xFFFF_8000, 2, 4),
            (0x007F_FFFF, 3, 3),
            (0xFF80_0000, 3, 4),
            (0x1234_5678, 4, 4),
        ];

        for (value, signed_cycles, unsigned_cycles) in cases {
            assert_eq!(
                Cpu::multiply_internal_cycles_for_extension(value, true),
                signed_cycles,
                "signed multiplier {value:#010X}"
            );
            assert_eq!(
                Cpu::multiply_internal_cycles_for_extension(value, false),
                unsigned_cycles,
                "unsigned multiplier {value:#010X}"
            );
        }
    }

    #[test]
    fn failed_arm_condition_skips_memory_side_effects() {
        // STRNE r0, [r1] is skipped while Z=1.
        let mut bus = bus_with_program(&[0x1581_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0xDEAD_BEEF);
        cpu.set_register(1, 0x0200_0100);
        bus.write32(0x0200_0100, 0x1234_5678);
        cpu.set_condition_flags(ConditionFlags {
            negative: false,
            zero: true,
            carry: false,
            overflow: false,
        });

        cpu.step(&mut bus).unwrap();

        assert_eq!(bus.read32(0x0200_0100), 0x1234_5678);
        assert_eq!(cpu.register(1), Some(0x0200_0100));
        assert_eq!(cpu.register(15), Some(0x0200_0004));
    }
    #[test]
    fn arm_scalar_gamepak_data_access_adds_waitcnt_latency() {
        let mut rom_bus = bus_with_program(&[0xE591_0000, 0xE1A0_0000]); // LDR r0, [r1]
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x0800_0100);
        rom_bus.write16(0x0400_0204, 0);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE591_0000, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(
            rom_cycles > ram_cycles,
            "ROM data reads must include WAITCNT latency"
        );
        assert_eq!(ram_cycles, 3, "IWRAM data reads keep the base estimate");
    }

    #[test]
    fn thumb_scalar_gamepak_data_access_adds_waitcnt_latency() {
        let mut rom_bus = bus_with_program(&[0x2001_6808]); // LDR r0, [r1]; MOV r0, #1
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_execution_state(true);
        rom_cpu.set_register(1, 0x0800_0100);
        rom_bus.write16(0x0400_0204, 0);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0x2001_6808]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_execution_state(true);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(
            rom_cycles > ram_cycles,
            "Thumb ROM data reads must include WAITCNT latency"
        );
        assert_eq!(
            ram_cycles, 3,
            "Thumb IWRAM data reads keep the base estimate"
        );
    }

    #[test]
    fn arm_gamepak_word_swap_adds_read_modify_write_wait_states() {
        let instruction = 0xE102_1093; // SWP r1, r3, [r2]
        let mut rom_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(2, 0x0800_0100);
        rom_cpu.set_register(3, 0x1234_5678);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(2, 0x0300_0100);
        ram_cpu.set_register(3, 0x1234_5678);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_gamepak_swap_byte_adds_read_modify_write_wait_states() {
        let instruction = 0xE142_4095; // SWPB r4, r5, [r2]
        let mut rom_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(2, 0x0800_0100);
        rom_cpu.set_register(5, 0x5A);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(2, 0x0300_0100);
        ram_cpu.set_register(5, 0x5A);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_gamepak_word_load_costs_more_than_byte_load() {
        let mut byte_bus = bus_with_program(&[0xE5D1_0000, 0xE1A0_0000]); // LDRB r0, [r1]
        let mut byte_cpu = Cpu::new();
        byte_cpu.reset(0x0200_0000);
        byte_cpu.set_register(1, 0x0800_0100);
        byte_bus.write16(0x0400_0204, 0);
        let (_, byte_cycles) = byte_cpu.step_with_cycles(&mut byte_bus).unwrap();

        let mut word_bus = bus_with_program(&[0xE591_0000, 0xE1A0_0000]); // LDR r0, [r1]
        let mut word_cpu = Cpu::new();
        word_cpu.reset(0x0200_0000);
        word_cpu.set_register(1, 0x0800_0100);
        word_bus.write16(0x0400_0204, 0);
        let (_, word_cycles) = word_cpu.step_with_cycles(&mut word_bus).unwrap();

        assert!(
            word_cycles > byte_cycles,
            "a Game Pak word load spans more 16-bit bus activity than a byte load"
        );
    }

    #[test]
    fn thumb_gamepak_byte_store_adds_waitcnt_latency() {
        // STRB r0, [r1, #0] (Thumb encoding 0x7008).
        let mut rom_bus = bus_with_program(&[0x7008]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_execution_state(true);
        rom_cpu.set_register(0, 0x5A);
        rom_cpu.set_register(1, 0x0800_0100);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0x7008]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_execution_state(true);
        ram_cpu.set_register(0, 0x5A);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_register_offset_gamepak_load_adds_waitcnt_latency() {
        // LDR r0, [r1, r2, LSL #2]
        let mut rom_bus = bus_with_program(&[0xE791_0102, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x0800_0100);
        rom_cpu.set_register(2, 1);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE791_0102, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        ram_cpu.set_register(2, 1);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
        assert_eq!(ram_cycles, 3);
    }

    #[test]
    fn arm_register_specified_shift_gamepak_load_adds_waitcnt_latency() {
        // LDR r0, [r1, r2, LSL r3], with r2=1 and r3=2.
        let mut rom_bus = bus_with_program(&[0xE791_0312, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x07FF_FFFC);
        rom_cpu.set_register(2, 1);
        rom_cpu.set_register(3, 2);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE791_0312, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        ram_cpu.set_register(2, 1);
        ram_cpu.set_register(3, 2);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_register_shifted_gamepak_store_adds_waitcnt_latency() {
        // STR r0, [r1, r2, LSL r3], where the shifted address reaches ROM.
        let instruction = 0xE781_0312;
        let mut rom_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(0, 0x1234_5678);
        rom_cpu.set_register(1, 0x07FF_FFFC);
        rom_cpu.set_register(2, 1);
        rom_cpu.set_register(3, 2);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(0, 0x1234_5678);
        ram_cpu.set_register(1, 0x0300_0100);
        ram_cpu.set_register(2, 1);
        ram_cpu.set_register(3, 2);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_post_indexed_gamepak_load_uses_base_address_for_wait_states() {
        // LDR r0, [r1], #4 accesses the original base, then updates r1.
        let instruction = 0xE491_0004;
        let mut rom_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x0800_0100);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();
        assert_eq!(rom_cpu.register(1), Some(0x0800_0104));

        let mut ram_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();
        assert_eq!(ram_cpu.register(1), Some(0x0300_0104));

        assert!(rom_cycles > ram_cycles);
    }

    #[test]
    fn arm_gamepak_data_wait_states_follow_waitcnt_configuration() {
        let instruction = 0xE591_0000; // LDR r0, [r1]
        let mut fast_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut fast_cpu = Cpu::new();
        fast_cpu.reset(0x0200_0000);
        fast_cpu.set_register(1, 0x0800_0100);
        fast_bus.write16(0x0400_0204, 0);
        let (_, fast_cycles) = fast_cpu.step_with_cycles(&mut fast_bus).unwrap();

        let mut slow_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
        let mut slow_cpu = Cpu::new();
        slow_cpu.reset(0x0200_0000);
        slow_cpu.set_register(1, 0x0800_0100);
        slow_bus.write16(0x0400_0204, 0x7FFF);
        let (_, slow_cycles) = slow_cpu.step_with_cycles(&mut slow_bus).unwrap();

        assert!(
            slow_cycles > fast_cycles,
            "larger WAITCNT wait states must increase CPU data-access cycles"
        );
    }

    #[test]
    fn arm_gamepak_byte_store_adds_waitcnt_latency_even_when_rom_ignores_write() {
        // STRB r0, [r1]. Game Pak ROM ignores writes, but the bus access
        // still consumes the configured external-bus wait states.
        let mut rom_bus = bus_with_program(&[0xE5C1_0000, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(0, 0x5A);
        rom_cpu.set_register(1, 0x0800_0100);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE5C1_0000, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(0, 0x5A);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
        assert_eq!(ram_cycles, 2);
    }

    #[test]
    fn arm_register_specified_shift_transfer_executes_and_uses_shifted_address() {
        // LDR r0, [r1, r2, LSL r3] reaches ROM at 0x08000000.
        let mut bus = bus_with_program(&[0xE791_0312, 0xE1A0_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x07FF_FFFC);
        cpu.set_register(2, 1);
        cpu.set_register(3, 2);

        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(matches!(outcome, StepOutcome::Executed { .. }));
        assert_eq!(cpu.register(0), Some(0));
        assert!(cycles > 3);
    }

    #[test]
    fn arm_register_specified_shift_transfer_rejects_pc_shift_amount() {
        // LDR r0, [r1, r2, LSL pc] is an unpredictable address form.
        let instruction = 0xE791_0F12;
        let mut bus = bus_with_program(&[instruction]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0300_0100);
        cpu.set_register(2, 1);

        assert_eq!(
            cpu.step(&mut bus),
            Err(CpuError::UnsupportedInstruction {
                address: 0x0200_0000,
                instruction,
            })
        );
    }

    #[test]
    fn failed_arm_condition_does_not_charge_data_wait_states() {
        // LDREQ r0, [r1, r2, LSL #2]; Z is clear after reset, so no data bus access.
        let mut bus = bus_with_program(&[0x0791_0102, 0xE1A0_0000]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(1, 0x0800_0100);
        cpu.set_register(2, 1);

        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(matches!(outcome, StepOutcome::ConditionFailed { .. }));
        assert_eq!(cycles, 1);
    }

    #[test]
    fn arm_register_offset_halfword_gamepak_load_adds_waitcnt_latency() {
        // LDRH r0, [r1, r2]
        let mut rom_bus = bus_with_program(&[0xE191_00B2, 0xE1A0_0000]);
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x0800_0100);
        rom_cpu.set_register(2, 2);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE191_00B2, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        ram_cpu.set_register(2, 2);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
        assert_eq!(ram_cycles, 3);
    }

    #[test]
    fn thumb_odd_address_signed_halfword_load_uses_byte_wide_gamepak_timing() {
        // LDSH r0, [r1, r2] uses a byte bus access for odd addresses and a
        // halfword access for even addresses on ARM7TDMI.
        let instruction = 0x0000_5E88_u32; // LDSH r0, [r1, r2]
        let mut odd_bus = bus_with_program(&[instruction]);
        let mut odd_cpu = Cpu::new();
        odd_cpu.reset(0x0200_0000);
        odd_cpu.set_execution_state(true);
        odd_cpu.set_register(1, 0x0E00_0101);
        odd_cpu.set_register(2, 0);
        let (_, odd_cycles) = odd_cpu.step_with_cycles(&mut odd_bus).unwrap();

        let mut even_bus = bus_with_program(&[instruction]);
        let mut even_cpu = Cpu::new();
        even_cpu.reset(0x0200_0000);
        even_cpu.set_execution_state(true);
        even_cpu.set_register(1, 0x0E00_0100);
        even_cpu.set_register(2, 0);
        let (_, even_cycles) = even_cpu.step_with_cycles(&mut even_bus).unwrap();

        assert!(
            odd_cycles < even_cycles,
            "odd LDSH should use byte-wide SRAM timing: odd={odd_cycles}, even={even_cycles}"
        );
    }

    #[test]
    fn arm_odd_address_signed_halfword_load_uses_byte_wide_sram_timing() {
        // LDRSH r0, [r1] uses a byte bus access when r1 is odd.
        for instruction in [0xE1D1_00F0, 0xE191_00F2] {
            let mut odd_bus = bus_with_program(&[instruction]);
            let mut odd_cpu = Cpu::new();
            odd_cpu.reset(0x0200_0000);
            odd_cpu.set_register(1, 0x0E00_0101);
            odd_cpu.set_register(2, 0);
            let (_, odd_cycles) = odd_cpu.step_with_cycles(&mut odd_bus).unwrap();

            let mut even_bus = bus_with_program(&[instruction]);
            let mut even_cpu = Cpu::new();
            even_cpu.reset(0x0200_0000);
            even_cpu.set_register(1, 0x0E00_0100);
            even_cpu.set_register(2, 0);
            let (_, even_cycles) = even_cpu.step_with_cycles(&mut even_bus).unwrap();

            assert!(
                odd_cycles < even_cycles,
                "odd LDRSH 0x{instruction:08X} should use byte-wide SRAM timing: odd={odd_cycles}, even={even_cycles}"
            );
        }
    }

    #[test]
    fn arm_immediate_signed_and_halfword_transfers_charge_gamepak_waitcnt() {
        // LDRSH r0, [r1, #4]; LDRSB r0, [r1, #4]; STRH r0, [r1, #4].
        for instruction in [0xE1D1_00F4, 0xE1D1_00D4, 0xE1C1_00B4] {
            let mut rom_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
            let mut rom_cpu = Cpu::new();
            rom_cpu.reset(0x0200_0000);
            rom_cpu.set_register(1, 0x0800_0100);
            rom_cpu.set_register(0, 0x1234_5678);
            let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

            let mut ram_bus = bus_with_program(&[instruction, 0xE1A0_0000]);
            let mut ram_cpu = Cpu::new();
            ram_cpu.reset(0x0200_0000);
            ram_cpu.set_register(1, 0x0300_0100);
            ram_cpu.set_register(0, 0x1234_5678);
            let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

            assert!(
                rom_cycles > ram_cycles,
                "instruction 0x{instruction:08X} should pay Game Pak wait states"
            );
        }
    }

    #[test]
    fn arm_multiple_gamepak_transfer_models_sequential_data_wait_states() {
        let mut rom_bus = bus_with_program(&[0xE8B1_0005, 0xE1A0_0000]); // LDMIA r1!, {r0, r2}
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_register(1, 0x0800_0100);
        rom_bus.write16(0x0400_0204, 1 << 4);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0xE8B1_0005, 0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_register(1, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
        assert_eq!(ram_cycles, 3);
    }

    #[test]
    fn thumb_multiple_gamepak_transfer_models_sequential_data_wait_states() {
        let mut rom_bus = bus_with_program(&[0x2001_C80A]); // LDMIA r0!, {r1, r3}
        let mut rom_cpu = Cpu::new();
        rom_cpu.reset(0x0200_0000);
        rom_cpu.set_execution_state(true);
        rom_cpu.set_register(0, 0x0800_0100);
        rom_bus.write16(0x0400_0204, 1 << 4);
        let (_, rom_cycles) = rom_cpu.step_with_cycles(&mut rom_bus).unwrap();

        let mut ram_bus = bus_with_program(&[0x2001_C80A]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.reset(0x0200_0000);
        ram_cpu.set_execution_state(true);
        ram_cpu.set_register(0, 0x0300_0100);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();

        assert!(rom_cycles > ram_cycles);
        assert_eq!(ram_cycles, 3);
    }

    #[test]
    fn gamepak_instruction_fetch_wait_states_affect_cycles_but_ram_does_not() {
        let mut bus = bus_with_program(&[0xE1A0_0000, 0xE1A0_0000]);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0800_00C0);
        bus.write16(0x0400_0204, 0);
        let (_, rom_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(rom_cycles >= 4, "ROM fetch should include WAITCNT latency");

        let mut ram_bus = bus_with_program(&[0xE1A0_0000]);
        let mut ram_cpu = Cpu::new();
        ram_cpu.set_register(15, 0x0300_0000);
        ram_bus.write32(0x0300_0000, 0xE1A0_0000);
        let (_, ram_cycles) = ram_cpu.step_with_cycles(&mut ram_bus).unwrap();
        assert_eq!(
            ram_cycles, 1,
            "IWRAM fetch must not receive Game Pak wait states"
        );
    }

    #[test]
    fn taken_thumb_branch_breaks_sequential_fetch_even_when_target_is_next_halfword() {
        // BNE with displacement -2 targets address+2, numerically the next
        // sequential PC, but hardware must restart the fetch sequence.
        let mut bus = bus_with_program(&[0x2001_D1FF, 0x2002_2003]);
        let mut cpu = Cpu::new();
        cpu.set_execution_state(true);
        cpu.set_register(15, 0x0800_00C0);
        cpu.set_condition_flags(ConditionFlags {
            negative: false,
            zero: false,
            carry: false,
            overflow: false,
        });
        bus.write16(0x0400_0204, 1 << 4);

        let (_, branch_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(cpu.register(15), Some(0x0800_00C2));
        let (_, after_branch_cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert!(branch_cycles >= 3);
        assert!(after_branch_cycles > 1);
    }

    #[test]
    fn not_taken_thumb_branch_keeps_sequential_gamepak_fetch_timing() {
        // BNE is not taken while Z is set, so the following halfword remains
        // on the sequential fetch path rather than paying a branch refill.
        let mut bus = bus_with_program(&[0x2001_D1FF]);
        let mut cpu = Cpu::new();
        cpu.set_execution_state(true);
        cpu.set_register(15, 0x0800_00C0);
        cpu.set_condition_flags(ConditionFlags {
            negative: false,
            zero: true,
            carry: false,
            overflow: false,
        });
        bus.write16(0x0400_0204, 1 << 4);

        let (_, branch_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        let (_, sequential_cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert_eq!(cpu.register(15), Some(0x0800_00C4));
        assert!(branch_cycles > sequential_cycles);
    }

    #[test]
    fn thumb_gamepak_fetch_uses_halfword_waitcnt_timing() {
        let mut bus = bus_with_program(&[0x2001_2002]);
        let mut cpu = Cpu::new();
        cpu.set_execution_state(true);
        cpu.set_register(15, 0x0800_00C0);
        bus.write16(0x0400_0204, 1 << 4); // WS0 sequential halfword: one cycle.

        let (_, first_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        let (_, second_cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert!(first_cycles > second_cycles);
        assert_eq!(cpu.register(0), Some(1));
    }

    #[test]
    fn gamepak_sequential_fetch_uses_waitcnt_sequential_duration() {
        let mut bus = bus_with_program(&[0xE1A0_0000, 0xE1A0_0000]);
        let mut cpu = Cpu::new();
        cpu.set_register(15, 0x0800_00C0);
        bus.write16(0x0400_0204, 1 << 4); // WS0 sequential halfword: one cycle.
        let (_, first_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        let (_, second_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(first_cycles > second_cycles);

        // An external PC edit breaks the sequential fetch stream even when
        // the new address is adjacent to the previous instruction.
        assert!(cpu.set_register(15, 0x0800_00C4));
        let (_, edited_pc_cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(edited_pc_cycles, first_cycles);
    }

    #[test]
    fn arm_adds_reports_signed_overflow_and_preserves_unaffected_flags() {
        let mut bus = bus_with_program(&[
            0xE290_0001, // ADDS r0, r0, #1
            0xE290_0001, // ADDS r0, r0, #1
        ]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x7FFF_FFFF);
        cpu.flags = ConditionFlags {
            negative: false,
            zero: false,
            carry: true,
            overflow: false,
        };

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x8000_0000));
        assert_eq!(
            cpu.flags(),
            ConditionFlags {
                negative: true,
                zero: false,
                carry: false,
                overflow: true,
            }
        );

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(0), Some(0x8000_0001));
        assert!(cpu.flags().negative);
        assert!(!cpu.flags().overflow);
    }

    #[test]
    fn thumb_bl_pair_updates_link_register_and_redirects_pc() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0200_0000, 0xF000); // BL high displacement = 0
        bus.write16(0x0200_0002, 0xF800); // BL low displacement = 0
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_execution_state(true);

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(14), Some(0x0200_0004));
        assert_eq!(cpu.register(15), Some(0x0200_0002));

        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.register(14), Some(0x0200_0005));
        assert_eq!(cpu.register(15), Some(0x0200_0004));
        assert!(cpu.is_thumb());
    }

    #[test]
    fn restoring_cpu_snapshot_restores_registers_flags_state_and_aligned_pc() {
        let mut original = Cpu::new();
        original.reset(0x0200_0000);
        original.set_register(0, 0x1234_5678);
        original.set_register(15, 0x0200_0012);
        original.set_condition_flags(ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: true,
        });
        original.set_execution_state(true);
        let snapshot = original.snapshot();

        let mut restored = Cpu::new();
        restored.reset(0x0800_0000);
        restored.restore_snapshot(snapshot);

        assert_eq!(restored.register(0), Some(0x1234_5678));
        assert_eq!(restored.register(15), Some(0x0200_0010));
        assert_eq!(restored.flags(), snapshot.flags);
        assert!(restored.is_thumb());
        assert_eq!(restored.snapshot().registers, snapshot.registers);
    }

    #[test]
    fn restoring_cpu_snapshot_preserves_bios_halt_state() {
        let mut bus = bus_with_program(&[0xEF00_0002]); // SWI Halt.
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.step_with_cycles(&mut bus).unwrap();
        assert!(cpu.is_halted());

        let snapshot = cpu.snapshot();
        assert!(snapshot.halted);

        let mut restored = Cpu::new();
        restored.restore_snapshot(snapshot);
        assert!(restored.is_halted());

        let (outcome, cycles) = restored.step_with_cycles(&mut bus).unwrap();
        assert_eq!(outcome, StepOutcome::Halted);
        assert_eq!(cycles, 1);
        assert_eq!(restored.register(15), Some(0x0200_0004));
    }

    #[test]
    fn cpu_snapshot_preserves_bios_interrupt_wait_mask() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0400_0200, 3);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0);
        cpu.set_register(1, 1);
        cpu.execute_swi(0x0200_0000, 0xEF00_0004, 0x04, false, &mut bus)
            .unwrap();
        assert!(cpu.is_halted());
        assert_eq!(cpu.snapshot().bios_wait_mask, Some(1));

        let snapshot = cpu.snapshot();
        let mut restored = Cpu::new();
        restored.restore_snapshot(snapshot);
        assert_eq!(restored.snapshot().bios_wait_mask, Some(1));

        bus.request_interrupt(2);
        assert_eq!(
            restored.step_with_cycles(&mut bus).unwrap().0,
            StepOutcome::Halted
        );
        bus.request_interrupt(1);
        assert!(!matches!(
            restored.step_with_cycles(&mut bus).unwrap().0,
            StepOutcome::Halted
        ));
        assert_eq!(bus.pending_interrupts(), 2);
    }

    #[test]
    fn restoring_arm_snapshot_aligns_pc_and_clears_thumb_state() {
        let mut cpu = Cpu::new();
        cpu.restore_snapshot(CpuSnapshot {
            registers: [0; 16],
            flags: ConditionFlags::default(),
            cpsr: 0,
            thumb: false,
            halted: false,
            bios_wait_mask: None,
            pc: 0x0800_0003,
        });

        assert!(!cpu.is_thumb());
        assert_eq!(cpu.register(15), Some(0x0800_0000));
    }

    #[test]
    fn failed_arm_condition_does_not_mutate_flags_or_skip_sequential_pc() {
        let mut bus = bus_with_program(&[0x0A00_0000]); // BEQ +0; Z is clear.
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_condition_flags(ConditionFlags {
            negative: true,
            zero: false,
            carry: true,
            overflow: true,
        });
        let before_flags = cpu.flags();

        let outcome = cpu.step(&mut bus).unwrap();

        assert_eq!(
            outcome,
            StepOutcome::ConditionFailed {
                address: 0x0200_0000,
                instruction: 0x0A00_0000,
            }
        );
        assert_eq!(cpu.register(15), Some(0x0200_0004));
        assert_eq!(cpu.flags(), before_flags);
    }

    #[test]
    fn arm_condition_codes_cover_all_flag_combinations_and_reserved_nv() {
        let cases = [
            (
                ConditionFlags {
                    negative: false,
                    zero: false,
                    carry: false,
                    overflow: false,
                },
                [
                    false, true, false, true, false, true, false, true, false, true, true, false,
                    true, false, true, false,
                ],
            ),
            (
                ConditionFlags {
                    negative: false,
                    zero: true,
                    carry: true,
                    overflow: false,
                },
                [
                    true, false, true, false, false, true, false, true, false, true, true, false,
                    false, true, true, false,
                ],
            ),
            (
                ConditionFlags {
                    negative: true,
                    zero: false,
                    carry: true,
                    overflow: true,
                },
                [
                    false, true, true, false, true, false, true, false, true, false, true, false,
                    true, false, true, false,
                ],
            ),
            (
                ConditionFlags {
                    negative: true,
                    zero: false,
                    carry: false,
                    overflow: false,
                },
                [
                    false, true, false, true, true, false, false, true, false, true, false, true,
                    false, true, true, false,
                ],
            ),
        ];
        let mut cpu = Cpu::new();
        for (flags, expected) in cases {
            cpu.set_condition_flags(flags);
            for (condition, expected_pass) in expected.into_iter().enumerate() {
                assert_eq!(
                    cpu.condition_passed_for(condition as u8),
                    expected_pass,
                    "condition code {condition:#x} with flags {flags:?}",
                );
            }
        }
        assert!(!cpu.condition_passed_for(0x10));
        assert!(!cpu.condition_passed_for(u8::MAX));
    }

    #[test]
    fn bios_register_ram_reset_clears_ewram_palette_and_sram_only() {
        let mut bus = bus_with_program(&[]);
        bus.write32(0x0200_0100, 0x1234_5678);
        bus.write32(0x0300_0100, 0x2345_6789);
        bus.write16(0x0500_0010, 0x3456);
        bus.write16(0x0600_0010, 0x4567);
        bus.write16(0x0700_0010, 0x5678);
        bus.write8(0x0E00_0010, 0x67);

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, (1 << 0) | (1 << 2) | (1 << 8));
        cpu.execute_swi(0x0200_0200, 0xEF00_0001, 0x01, false, &mut bus)
            .unwrap();

        assert_eq!(bus.read32(0x0200_0100), 0);
        assert_eq!(bus.read16(0x0500_0010), 0);
        assert_eq!(bus.read8(0x0E00_0010), 0);
        assert_eq!(bus.read32(0x0300_0100), 0x2345_6789);
        assert_eq!(bus.read16(0x0600_0010), 0x4567);
        assert_eq!(bus.read16(0x0700_0010), 0x5678);
    }

    #[test]
    fn bios_register_ram_reset_zero_flags_leave_memory_untouched() {
        let mut bus = bus_with_program(&[]);
        bus.write32(0x0200_0100, 0xCAFE_BABE);
        bus.write16(0x0500_0010, 0xABCD);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, 0);
        cpu.execute_swi(0x0200_0200, 0xEF00_0001, 0x01, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read32(0x0200_0100), 0xCAFE_BABE);
        assert_eq!(bus.read16(0x0500_0010), 0xABCD);
    }

    #[test]
    fn bios_sqrt_handles_zero_perfect_squares_and_u32_max() {
        let mut bus = bus_with_program(&[]);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);

        for (input, expected) in [
            (0, 0),
            (1, 1),
            (2, 1),
            (15, 3),
            (16, 4),
            (81, 9),
            (u32::MAX, 65_535),
        ] {
            cpu.set_register(0, input);
            cpu.execute_swi(0x0200_0200, 0xEF00_0008, 0x08, false, &mut bus)
                .unwrap();
            assert_eq!(cpu.register(0), Some(expected), "sqrt({input})");
        }
    }

    #[test]
    fn bios_cpu_set_copies_halfwords_and_repeats_the_first_value_for_fill() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        for (index, value) in [0x1234_u16, 0xABCD, 0x00FF].into_iter().enumerate() {
            bus.write16(source + index as u32 * 2, value);
        }
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.set_register(2, 3);
        cpu.execute_swi(0x0200_0200, 0xEF00_000B, 0x0B, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read16(destination), 0x1234);
        assert_eq!(bus.read16(destination + 2), 0xABCD);
        assert_eq!(bus.read16(destination + 4), 0x00FF);

        cpu.set_register(0, source);
        cpu.set_register(1, destination + 0x10);
        cpu.set_register(2, (1 << 24) | 3);
        cpu.execute_swi(0x0200_0204, 0xEF00_000B, 0x0B, false, &mut bus)
            .unwrap();
        for offset in (0..6).step_by(2) {
            assert_eq!(bus.read16(destination + 0x10 + offset), 0x1234);
        }
    }

    #[test]
    fn bios_cpu_set_hle_cycles_advance_timers_and_match_transfer_work() {
        let mut bus = bus_with_program(&[0xEF00_000B]); // SWI CpuSet.
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0000);
        cpu.set_register(0, 0x0200_0100);
        cpu.set_register(1, 0x0200_0200);
        cpu.set_register(2, (1 << 26) | 4); // Four word copies.
        for index in 0..4 {
            bus.write32(0x0200_0100 + index * 4, 0x1234_0000 + index);
        }
        bus.write16(0x0400_0100, 0xFFFF); // Timer 0 overflows on each cycle.
        bus.write16(0x0400_0102, 0x0080); // Enable, prescaler 1.
        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();

        assert!(matches!(outcome, StepOutcome::Executed { .. }));
        assert_eq!(cycles, 9); // Base + one read and one write per word.
        for index in 0..4 {
            assert_eq!(
                bus.read32(0x0200_0200 + index * 4),
                0x1234_0000 + index
            );
        }
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
        assert!(bus.timer_counter(0).unwrap() >= 0xFFFF);
    }

    #[test]
    fn bios_cpu_fast_set_rounds_count_up_to_eight_words() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        for index in 0..8_u32 {
            bus.write32(source + index * 4, 0xA000_0000 | index);
        }
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source);
        cpu.set_register(1, destination);
        cpu.set_register(2, 3); // A request for three words copies one full 8-word block.
        cpu.execute_swi(0x0200_0200, 0xEF00_000C, 0x0C, false, &mut bus)
            .unwrap();
        for index in 0..8_u32 {
            assert_eq!(
                bus.read32(destination + index * 4),
                0xA000_0000 | index,
                "word {index}"
            );
        }

        cpu.set_register(0, source);
        cpu.set_register(1, destination + 0x40);
        cpu.set_register(2, (1 << 24) | 1); // Fill mode also rounds to eight words.
        cpu.execute_swi(0x0200_0204, 0xEF00_000C, 0x0C, false, &mut bus)
            .unwrap();
        for index in 0..8_u32 {
            assert_eq!(bus.read32(destination + 0x40 + index * 4), 0xA000_0000);
        }
    }

    #[test]
    fn bios_intr_wait_returns_immediately_for_an_already_pending_match() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0400_0200, (1 << 0) | (1 << 1));
        bus.request_interrupt((1 << 0) | (1 << 1));
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, 0);
        cpu.set_register(1, 1 << 0);

        cpu.execute_swi(0x0200_0200, 0xEF00_0004, 0x04, false, &mut bus)
            .unwrap();

        assert!(!cpu.is_halted());
        assert_eq!(cpu.register(15), Some(0x0200_0204));
        assert_eq!(bus.pending_interrupts(), 1 << 1);
    }

    #[test]
    fn bios_cpu_set_copies_and_fills_aligned_words() {
        let mut bus = bus_with_program(&[]);
        let source = 0x0200_0000;
        let destination = 0x0200_0100;
        for (index, value) in [0x1234_5678_u32, 0xABCD_EF01, 0x0BAD_F00D]
            .into_iter()
            .enumerate()
        {
            bus.write32(source + index as u32 * 4, value);
        }

        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, source + 1);
        cpu.set_register(1, destination + 3);
        cpu.set_register(2, (1 << 26) | 3);
        cpu.execute_swi(0x0200_0200, 0xEF00_000B, 0x0B, false, &mut bus)
            .unwrap();
        assert_eq!(bus.read32(destination), 0x1234_5678);
        assert_eq!(bus.read32(destination + 4), 0xABCD_EF01);
        assert_eq!(bus.read32(destination + 8), 0x0BAD_F00D);

        cpu.set_register(0, source);
        cpu.set_register(1, destination + 0x20);
        cpu.set_register(2, (1 << 24) | (1 << 26) | 3);
        cpu.execute_swi(0x0200_0204, 0xEF00_000B, 0x0B, false, &mut bus)
            .unwrap();
        for index in 0..3_u32 {
            assert_eq!(bus.read32(destination + 0x20 + index * 4), 0x1234_5678);
        }
    }

    #[test]
    fn bios_intr_wait_ignores_unselected_interrupts_and_consumes_match() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0400_0200, (1 << 0) | (1 << 1));
        bus.request_interrupt(1 << 1);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, 0);
        cpu.set_register(1, 1 << 0);
        cpu.execute_swi(0x0200_0200, 0xEF00_0004, 0x04, false, &mut bus)
            .unwrap();
        assert!(cpu.is_halted());

        bus.request_interrupt(1 << 0);
        let (outcome, cycles) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(!matches!(outcome, StepOutcome::Halted));
        assert!(cycles >= 1);
        assert_eq!(bus.pending_interrupts(), 1 << 1);
        assert!(!cpu.is_halted());
    }

    #[test]
    fn bios_intr_wait_discard_old_clears_selected_flags_before_waiting() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0400_0200, (1 << 0) | (1 << 1));
        bus.request_interrupt((1 << 0) | (1 << 1));
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.set_register(0, 1);
        cpu.set_register(1, 1 << 0);
        cpu.execute_swi(0x0200_0200, 0xEF00_0004, 0x04, false, &mut bus)
            .unwrap();
        assert!(cpu.is_halted());
        assert_eq!(bus.pending_interrupts(), 1 << 1);

        let (outcome, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(outcome, StepOutcome::Halted);
        assert!(cpu.is_halted());
    }

    #[test]
    fn bios_vblank_intr_wait_uses_vblank_mask_and_discards_old_request() {
        let mut bus = bus_with_program(&[]);
        bus.write16(0x0400_0200, (1 << 0) | (1 << 1));
        bus.request_interrupt((1 << 0) | (1 << 1));
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0200);
        cpu.execute_swi(0x0200_0200, 0xEF00_0005, 0x05, false, &mut bus)
            .unwrap();
        assert!(cpu.is_halted());
        assert_eq!(bus.pending_interrupts(), 1 << 1);

        bus.request_interrupt(1 << 1);
        let (outcome, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert_eq!(outcome, StepOutcome::Halted);
        assert!(cpu.is_halted());

        bus.request_interrupt(1 << 0);
        let (outcome, _) = cpu.step_with_cycles(&mut bus).unwrap();
        assert!(!matches!(outcome, StepOutcome::Halted));
        assert!(!cpu.is_halted());
        assert_eq!(bus.pending_interrupts(), 1 << 1);
    }

    #[test]
    fn bios_cpu_set_supports_word_fill_and_preserves_neighboring_memory() {
        let mut bus = bus_with_program(&[]);
        bus.write32(0x0200_0000, 0xA1B2_C3D4);
        bus.write32(0x0300_0010, 0xDEAD_BEEF);
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0000);
        cpu.set_register(1, 0x0300_0000);
        cpu.set_register(2, (1 << 24) | (1 << 26) | 4);
        cpu.execute_swi(0x0200_0100, 0xEF00_000B, 0x0B, false, &mut bus)
            .unwrap();

        for index in 0..4 {
            assert_eq!(bus.read32(0x0300_0000 + index * 4), 0xA1B2_C3D4);
        }
        assert_eq!(bus.read32(0x0300_0010), 0xDEAD_BEEF);
    }

    #[test]
    fn bios_cpu_fast_set_rounds_copy_length_up_to_eight_words() {
        let mut bus = bus_with_program(&[]);
        for index in 0..17 {
            bus.write32(0x0200_0000 + index * 4, 0xCAFE_0000 | index);
            bus.write32(0x0300_0000 + index * 4, 0xDEAD_BEEF);
        }
        let mut cpu = Cpu::new();
        cpu.reset(0x0200_0100);
        cpu.set_register(0, 0x0200_0000);
        cpu.set_register(1, 0x0300_0000);
        cpu.set_register(2, 9); // CpuFastSet rounds 9 words up to 16.
        cpu.execute_swi(0x0200_0100, 0xEF00_000C, 0x0C, false, &mut bus)
            .unwrap();

        for index in 0..16 {
            assert_eq!(bus.read32(0x0300_0000 + index * 4), 0xCAFE_0000 | index);
        }
        assert_eq!(bus.read32(0x0300_0040), 0xDEAD_BEEF);
    }

    #[test]
    fn bios_memory_hle_cycle_estimates_scale_with_transfer_size() {
        let mut cpu = Cpu::new();

        // CpuSet: four-word copy costs one setup cycle plus a read/write pair
        // for each word, rather than the old fixed one-cycle SWI estimate.
        cpu.set_register(2, (1 << 26) | 4);
        assert_eq!(cpu.estimate_cycles(0xEF00_000B), 9);

        // Fill reads the source once and writes each of four destination words.
        cpu.set_register(2, (1 << 26) | (1 << 24) | 4);
        assert_eq!(cpu.estimate_cycles(0xEF00_000B), 5);

        // CpuFastSet rounds a one-word request up to eight words.
        cpu.set_register(2, 1);
        assert_eq!(cpu.estimate_cycles(0xEF00_000C), 17);

        // Fill mode performs one source read plus one write per element.
        cpu.set_register(2, (1 << 24) | 4);
        assert_eq!(cpu.estimate_cycles(0xEF00_000B), 6);
        cpu.set_register(2, 1 << 24); // A zero-count fill performs no transfer.
        assert_eq!(cpu.estimate_cycles(0xEF00_000B), 1);

        // CpuFastSet also reads the fill value only once for its rounded burst.
        cpu.set_register(2, (1 << 24) | 1);
        assert_eq!(cpu.estimate_cycles(0xEF00_000C), 10);

        // RegisterRamReset's EWRAM flag clears 256 KiB; its timing estimate
        // must account for that work instead of treating it like a tiny SWI.
        cpu.set_register(0, 1);
        assert_eq!(cpu.estimate_cycles(0xEF00_0001), 1 + 2 * 256 * 1024);
    }
}
