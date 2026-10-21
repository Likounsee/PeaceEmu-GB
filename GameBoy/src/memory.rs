//! Initial memory map for the Game Boy Advance.
//!
//! This models address decoding, backing storage, timer progression, scanline/DISPSTAT
//! timing, immediate/HBlank/VBlank DMA, and timer-triggered sound FIFO DMA.
//! Open-bus behavior, BIOS protection, and several peripheral side effects remain
//! in development.

use std::collections::VecDeque;

use crate::GbaCartridge;

/// EWRAM backing-store size (256 KiB).
pub const EWRAM_SIZE: usize = 256 * 1024;
/// IWRAM backing-store size (32 KiB).
pub const IWRAM_SIZE: usize = 32 * 1024;
/// Palette RAM backing-store size (1 KiB).
pub const PALETTE_RAM_SIZE: usize = 1024;
/// VRAM backing-store size (96 KiB).
pub const VRAM_SIZE: usize = 96 * 1024;
/// OAM backing-store size (1 KiB).
pub const OAM_SIZE: usize = 1024;
/// I/O register window size (1 KiB).
pub const IO_SIZE: usize = 1024;
/// SRAM backup-device capacity (32 KiB); the 64 KiB bus window mirrors it.
pub const SRAM_SIZE: usize = 32 * 1024;

/// Snapshot of one Direct Sound FIFO's frontend-relevant state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioFifoStatus {
    /// Number of queued sample bytes (0 through 32).
    pub queued_bytes: usize,
    /// Hardware queue capacity in bytes (32).
    pub capacity: usize,
    /// Last signed 8-bit sample latched by the FIFO DAC.
    pub last_sample: i8,
    /// Whether the queue has reached the 16-byte-or-less refill threshold.
    /// This is a status hint, not proof that a valid DMA channel is configured.
    pub refill_threshold_reached: bool,
}

/// A physical button on the Game Boy Advance keypad.
///
/// The discriminant is the button's bit in the active-high pressed-key mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum GbaButton {
    /// A button.
    A = 1 << 0,
    /// B button.
    B = 1 << 1,
    /// Select button.
    Select = 1 << 2,
    /// Start button.
    Start = 1 << 3,
    /// Right direction.
    Right = 1 << 4,
    /// Left direction.
    Left = 1 << 5,
    /// Up direction.
    Up = 1 << 6,
    /// Down direction.
    Down = 1 << 7,
    /// Right shoulder button.
    R = 1 << 8,
    /// Left shoulder button.
    L = 1 << 9,
}

impl GbaButton {
    /// All ten physical buttons in stable bit-index order.
    pub const ALL: [Self; 10] = [
        Self::A,
        Self::B,
        Self::Select,
        Self::Start,
        Self::Right,
        Self::Left,
        Self::Up,
        Self::Down,
        Self::R,
        Self::L,
    ];

    /// Returns this button's bit in the pressed-key mask.
    pub const fn mask(self) -> u16 {
        self as u16
    }
}

/// GBA bus and writable memory regions.
#[derive(Clone, Debug)]
pub struct MemoryBus {
    cartridge: GbaCartridge,
    save_type: Option<crate::SaveType>,
    flash: Option<crate::save::FlashDevice>,
    eeprom: Option<crate::save::EepromDevice>,
    ewram: Vec<u8>,
    iwram: Vec<u8>,
    io: Vec<u8>,
    palette: Vec<u8>,
    vram: Vec<u8>,
    oam: Vec<u8>,
    sram: Vec<u8>,
    /// Active-low KEYINPUT state is stored internally as a pressed-key bitmask.
    keys_pressed: u16,
    key_control: u16,
    interrupt_enable: u16,
    interrupt_flags: u16,
    interrupt_master_enable: u16,
    /// Game Pak wait-state configuration (WAITCNT, with reserved bits masked).
    wait_control: u16,
    /// Timer reload values (TM0CNT_L through TM3CNT_L).
    timer_reload: [u16; 4],
    /// Current timer counters, advanced by `advance_cycles`.
    timer_counter: [u16; 4],
    /// Timer control bits (prescaler, cascade, IRQ and enable).
    timer_control: [u16; 4],
    /// Input cycles accumulated toward each timer's selected prescaler.
    timer_prescaler_phase: [u16; 4],
    /// CPU cycles elapsed in the current 1232-cycle video scanline.
    video_cycles_in_line: u16,
    /// Current visible/blanking scanline counter (0..227).
    vcount: u16,
    /// DISPSTAT writable controls plus live VBlank/HBlank/VCOUNT-match status.
    dispstat: u16,
    /// Sound FIFO A/B byte queues (hardware capacity: 32 bytes each).
    audio_fifos: [VecDeque<u8>; 2],
    /// Last signed PCM sample held when a sound FIFO underflows.
    audio_last_samples: [i8; 2],
}

/// Error returned when loading save data whose size does not match the device capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SramSizeError {
    /// Number of bytes required by the save device.
    pub expected: usize,
    /// Number of bytes supplied by the caller.
    pub actual: usize,
}

impl core::fmt::Display for SramSizeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "invalid save image size: expected {} bytes, got {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for SramSizeError {}

impl MemoryBus {
    /// Creates a fresh bus with zeroed RAM and the supplied validated cartridge.
    pub fn new(cartridge: GbaCartridge) -> Self {
        let save_type = cartridge.save_type();
        let flash = save_type.and_then(crate::save::FlashDevice::new);
        let eeprom =
            (save_type == Some(crate::SaveType::Eeprom)).then(crate::save::EepromDevice::new);
        Self {
            cartridge,
            save_type,
            flash,
            eeprom,
            ewram: vec![0; EWRAM_SIZE],
            iwram: vec![0; IWRAM_SIZE],
            io: vec![0; IO_SIZE],
            palette: vec![0; PALETTE_RAM_SIZE],
            vram: vec![0; VRAM_SIZE],
            oam: vec![0; OAM_SIZE],
            sram: vec![0xFF; SRAM_SIZE],
            keys_pressed: 0,
            key_control: 0,
            interrupt_enable: 0,
            interrupt_flags: 0,
            interrupt_master_enable: 0,
            wait_control: 0,
            timer_reload: [0; 4],
            timer_counter: [0; 4],
            timer_control: [0; 4],
            timer_prescaler_phase: [0; 4],
            video_cycles_in_line: 0,
            vcount: 0,
            // VCOUNT starts at line 0, so its compare status is initially active.
            dispstat: 1 << 2,
            audio_fifos: std::array::from_fn(|_| VecDeque::with_capacity(32)),
            audio_last_samples: [0; 2],
        }
    }

    /// Returns read-only external work RAM (EWRAM, 256 KiB).
    ///
    /// Use bus reads and writes for emulated accesses so mirroring and bus
    /// semantics remain enforced.
    pub fn external_work_ram(&self) -> &[u8] {
        &self.ewram
    }

    /// Returns read-only internal work RAM (IWRAM, 32 KiB).
    pub fn internal_work_ram(&self) -> &[u8] {
        &self.iwram
    }

    /// Returns read-only palette RAM (1 KiB).
    pub fn palette_ram(&self) -> &[u8] {
        &self.palette
    }

    /// Returns read-only video RAM (96 KiB backing store).
    pub fn video_ram(&self) -> &[u8] {
        &self.vram
    }

    /// Returns read-only object attribute memory (OAM, 1 KiB).
    pub fn object_attribute_memory(&self) -> &[u8] {
        &self.oam
    }

    /// Returns the backup-device type inferred from the loaded cartridge ROM.
    ///
    /// This is signature-based metadata; SRAM and Flash are mapped to their
    /// respective devices; EEPROM uses the serial Game Pak access path.
    pub fn cartridge_save_type(&self) -> Option<crate::SaveType> {
        // This is detected once when the bus is constructed. Re-scanning a ROM
        // (which may be 32 MiB) on every save snapshot is unnecessary work.
        self.save_type
    }

    /// Returns the most recently latched signed PCM sample for FIFO A (0) or B (1).
    pub fn audio_fifo_last_sample(&self, fifo: usize) -> Option<i8> {
        self.audio_last_samples.get(fifo).copied()
    }

    /// Returns the number of queued bytes in FIFO A (0) or B (1).
    pub fn audio_fifo_len(&self, fifo: usize) -> Option<usize> {
        self.audio_fifos.get(fifo).map(VecDeque::len)
    }

    /// Returns a coherent status snapshot for Direct Sound FIFO A (0) or B (1).
    ///
    /// Returns `None` for any other FIFO index. The refill threshold is true at
    /// 16 queued bytes or fewer, matching the GBA sound-DMA refill point; actual
    /// DMA service still depends on the configured channel and timer.
    pub fn audio_fifo_status(&self, fifo: usize) -> Option<AudioFifoStatus> {
        let queue = self.audio_fifos.get(fifo)?;
        Some(AudioFifoStatus {
            queued_bytes: queue.len(),
            capacity: 32,
            last_sample: self.audio_last_samples[fifo],
            refill_threshold_reached: queue.len() <= 16,
        })
    }

    /// Returns the number of additional sample bytes before a FIFO reaches
    /// its 16-byte-or-less refill threshold.
    ///
    /// Returns zero if refill is already due, or None for an invalid FIFO index.
    pub fn audio_fifo_bytes_until_refill(&self, fifo: usize) -> Option<usize> {
        self.audio_fifos
            .get(fifo)
            .map(|queue| queue.len().saturating_sub(16))
    }

    /// Mixes the current Direct Sound FIFO DAC latches into a stereo sample.
    ///
    /// The returned pair is (left, right). SOUNDCNT_H routing and half/full
    /// volume controls are applied, and disabled master sound produces silence.
    /// This is a raw integer mix, not a resampled or filtered audio stream.
    pub fn direct_sound_stereo_sample(&self) -> (i16, i16) {
        if self.read16(0x0400_0084) & 0x0080 == 0 {
            return (0, 0);
        }

        let control = self.read16(0x0400_0082);
        let mut left = 0_i16;
        let mut right = 0_i16;
        for fifo in 0..2 {
            let sample = i16::from(self.audio_last_samples[fifo]);
            // SOUNDCNT_H bits 2 and 3 select full (1) or half (0) volume.
            let volume_bit = if fifo == 0 { 2 } else { 3 };
            let scaled = if control & (1 << volume_bit) != 0 {
                sample * 2
            } else {
                sample
            };
            let (left_bit, right_bit) = if fifo == 0 { (9, 8) } else { (13, 12) };
            if control & (1 << left_bit) != 0 {
                left += scaled;
            }
            if control & (1 << right_bit) != 0 {
                right += scaled;
            }
        }
        (left, right)
    }

    /// Returns the current live counter value for a timer (TM0..TM3).
    ///
    /// The counter is updated by advance_cycles, not merely by reading the MMIO
    /// register. Returns None for an invalid index.
    pub fn timer_counter(&self, timer: usize) -> Option<u16> {
        self.timer_counter.get(timer).copied()
    }

    /// Returns the configured reload value for a timer (TM0..TM3).
    ///
    /// A reload value is copied into the live counter on enable and overflow.
    /// Returns None for an invalid index.
    pub fn timer_reload_value(&self, timer: usize) -> Option<u16> {
        self.timer_reload.get(timer).copied()
    }

    /// Returns whether a timer (TM0..TM3) is enabled.
    ///
    /// Returns None for an invalid index.
    pub fn timer_enabled(&self, timer: usize) -> Option<bool> {
        self.timer_control
            .get(timer)
            .map(|control| control & 0x0080 != 0)
    }

    /// Returns the masked control register for a timer (TM0..TM3).
    ///
    /// Bits 0–1 select the prescaler, bit 2 selects cascade mode (TM1–TM3),
    /// bit 6 enables the timer overflow IRQ, and bit 7 enables the timer.
    /// Returns None for an invalid index.
    pub fn timer_control(&self, timer: usize) -> Option<u16> {
        self.timer_control.get(timer).copied()
    }

    /// Returns the timer's selected prescaler divisor (1, 64, 256, or 1024).
    ///
    /// This reports the selected divisor even when the timer is disabled or
    /// configured for cascade mode. Returns None for an invalid index.
    pub fn timer_prescaler_divisor(&self, timer: usize) -> Option<u16> {
        self.timer_control
            .get(timer)
            .map(|control| match control & 3 {
                0 => 1,
                1 => 64,
                2 => 256,
                _ => 1024,
            })
    }

    /// Returns the current prescaler phase for a timer (TM0..TM3).
    ///
    /// The phase is the number of CPU cycles accumulated toward the next
    /// prescaled tick and is always smaller than the selected divisor. For
    /// cascade-mode timers this value is not used to clock the timer. Returns
    /// None for an invalid index.
    pub fn timer_prescaler_phase(&self, timer: usize) -> Option<u16> {
        self.timer_prescaler_phase.get(timer).copied()
    }

    /// Returns whether a timer is configured to raise an IRQ on overflow.
    ///
    /// Returns None for an invalid index.
    pub fn timer_irq_enabled(&self, timer: usize) -> Option<bool> {
        self.timer_control
            .get(timer)
            .map(|control| control & 0x0040 != 0)
    }

    /// Returns whether a timer uses cascade clocking.
    ///
    /// Timer 0 cannot cascade; the method reports the register bit as configured.
    /// Returns None for an invalid index.
    pub fn timer_cascade_enabled(&self, timer: usize) -> Option<bool> {
        self.timer_control
            .get(timer)
            .map(|control| control & 0x0004 != 0)
    }

    /// Estimates CPU cycles until the next overflow for a prescaled timer.
    ///
    /// Returns None for an invalid index, a disabled timer, or a timer using
    /// cascade mode because its clock depends on another timer's overflow.
    /// The estimate includes the current prescaler phase and assumes the timer
    /// configuration remains unchanged.
    pub fn timer_cycles_until_overflow(&self, timer: usize) -> Option<u64> {
        let control = *self.timer_control.get(timer)?;
        if control & 0x0080 == 0 || (timer != 0 && control & 0x0004 != 0) {
            return None;
        }
        let divisor = match control & 3 {
            0 => 1_u64,
            1 => 64,
            2 => 256,
            _ => 1024,
        };
        let counter = u64::from(*self.timer_counter.get(timer)?);
        let phase = u64::from(*self.timer_prescaler_phase.get(timer)?);
        Some((u64::from(u16::MAX) + 1 - counter) * divisor - phase)
    }

    /// Returns the masked DMA control register for channel 0–3.
    ///
    /// The returned value reflects the register's writable bits after hardware
    /// masking. Returns None for an invalid channel.
    pub fn dma_control(&self, channel: usize) -> Option<u16> {
        if channel >= 4 {
            return None;
        }
        Some(self.read16(0x0400_00BA + channel as u32 * 12))
    }

    /// Returns whether a DMA channel is currently enabled.
    pub fn dma_enabled(&self, channel: usize) -> Option<bool> {
        self.dma_control(channel)
            .map(|control| control & 0x8000 != 0)
    }

    /// Returns the DMA start timing selector (0 immediate, 1 VBlank, 2 HBlank, 3 special).
    pub fn dma_start_timing(&self, channel: usize) -> Option<u8> {
        self.dma_control(channel)
            .map(|control| ((control >> 12) & 3) as u8)
    }

    /// Returns whether a DMA channel transfers 32-bit words rather than halfwords.
    pub fn dma_word_transfer(&self, channel: usize) -> Option<bool> {
        self.dma_control(channel)
            .map(|control| control & (1 << 10) != 0)
    }

    /// Returns whether a DMA channel is configured to request an IRQ on completion.
    pub fn dma_irq_enabled(&self, channel: usize) -> Option<bool> {
        self.dma_control(channel)
            .map(|control| control & (1 << 14) != 0)
    }

    /// Returns the currently enabled interrupt sources (IE), with reserved bits removed.
    pub fn interrupt_enable_mask(&self) -> u16 {
        self.interrupt_enable & 0x3FFF
    }

    /// Returns the latched interrupt request flags (IF), with reserved bits removed.
    pub fn interrupt_request_flags(&self) -> u16 {
        self.interrupt_flags & 0x3FFF
    }

    /// Returns whether the master interrupt enable (IME) bit is set.
    pub fn interrupt_master_enabled(&self) -> bool {
        self.interrupt_master_enable & 1 != 0
    }

    /// Returns the currently configured Game Pak wait-state register (WAITCNT).
    pub fn gamepak_wait_control(&self) -> u16 {
        self.wait_control
    }

    /// Returns the configured Game Pak access duration in CPU cycles.
    ///
    /// This decodes WAITCNT for ROM0 (0x08000000–0x09FFFFFF), ROM1
    /// (0x0A000000–0x0BFFFFFF), ROM2 (0x0C000000–0x0DFFFFFF), and SRAM
    /// (0x0E000000–0x0FFFFFFF). The result is the total access duration,
    /// not an extra-cycle count. Sequential timing is ignored for SRAM, whose
    /// accesses always use its configured wait duration. This helper exposes
    /// register decoding for timing integrations; it does not by itself add
    /// wait states to CPU execution.
    pub fn gamepak_access_cycles(&self, address: u32, sequential: bool) -> Option<u32> {
        let waitcnt = self.wait_control;
        match address {
            0x0800_0000..=0x09FF_FFFF => {
                if sequential {
                    Some(if waitcnt & (1 << 4) != 0 { 1 } else { 2 })
                } else {
                    Some(match (waitcnt >> 2) & 3 {
                        0 => 4,
                        1 => 3,
                        2 => 2,
                        _ => 8,
                    })
                }
            }
            0x0A00_0000..=0x0BFF_FFFF => {
                if sequential {
                    Some(if waitcnt & (1 << 7) != 0 { 1 } else { 4 })
                } else {
                    Some(match (waitcnt >> 5) & 3 {
                        0 => 4,
                        1 => 3,
                        2 => 2,
                        _ => 8,
                    })
                }
            }
            0x0C00_0000..=0x0DFF_FFFF => {
                if sequential {
                    Some(if waitcnt & (1 << 10) != 0 { 1 } else { 8 })
                } else {
                    Some(match (waitcnt >> 8) & 3 {
                        0 => 4,
                        1 => 3,
                        2 => 2,
                        _ => 8,
                    })
                }
            }
            0x0E00_0000..=0x0FFF_FFFF => Some(match waitcnt & 3 {
                0 => 4,
                1 => 3,
                2 => 2,
                _ => 8,
            }),
            _ => None,
        }
    }

    /// Returns total Game Pak bus cycles for an 8-, 16-, or 32-bit access.
    ///
    /// ROM uses a 16-bit external data bus: byte and halfword accesses consume
    /// one halfword access, while a word consumes the requested first access
    /// plus a sequential second halfword. SRAM uses an 8-bit bus, so wider
    /// accesses consume one SRAM wait-state duration per byte. This helper
    /// reports bus duration only; CPU execution does not yet apply it.
    /// Unsupported widths and addresses outside the Game Pak window return None.
    pub fn gamepak_access_cycles_for_width(
        &self,
        address: u32,
        sequential: bool,
        width_bytes: u8,
    ) -> Option<u32> {
        if !matches!(width_bytes, 1 | 2 | 4) {
            return None;
        }
        let first = self.gamepak_access_cycles(address, sequential)?;
        match address {
            0x0800_0000..=0x0DFF_FFFF => {
                if width_bytes == 4 {
                    let second_address = address.checked_add(2)?;
                    let first_region = address >> 25;
                    if second_address >> 25 != first_region {
                        return None;
                    }
                    first.checked_add(self.gamepak_access_cycles(second_address, true)?)
                } else {
                    Some(first)
                }
            }
            0x0E00_0000..=0x0FFF_FFFF => first.checked_mul(u32::from(width_bytes)),
            _ => None,
        }
    }

    /// Returns the total Game Pak bus cycles for a sequence of same-width accesses.
    ///
    /// The first transfer uses the requested sequential/non-sequential setting;
    /// subsequent transfers are treated as sequential. This estimates timing only
    /// and does not advance the bus. A zero-length sequence costs zero cycles.
    pub fn gamepak_sequence_cycles(
        &self,
        address: u32,
        first_sequential: bool,
        width_bytes: u8,
        access_count: usize,
    ) -> Option<u64> {
        if access_count == 0 {
            return Some(0);
        }
        if !matches!(width_bytes, 1 | 2 | 4) {
            return None;
        }
        let stride = u32::from(width_bytes);
        let mut total = 0_u64;
        for index in 0..access_count {
            let offset = u32::try_from(index).ok()?.checked_mul(stride)?;
            let current = address.checked_add(offset)?;
            let cycles = self.gamepak_access_cycles_for_width(
                current,
                if index == 0 { first_sequential } else { true },
                width_bytes,
            )?;
            total = total.checked_add(u64::from(cycles))?;
        }
        Some(total)
    }

    fn write_sound_control_high(&mut self, value: u16) {
        if value & (1 << 11) != 0 {
            self.audio_fifos[0].clear();
            self.audio_last_samples[0] = 0;
        }
        if value & (1 << 15) != 0 {
            self.audio_fifos[1].clear();
            self.audio_last_samples[1] = 0;
        }
        // FIFO reset bits are write-one commands and read back as zero.
        let control = value & 0x770F;
        self.io[0x82..0x84].copy_from_slice(&control.to_le_bytes());
    }

    fn write_sound_master_control(&mut self, value: u16) {
        let was_enabled = self.io[0x84] & 0x80 != 0;
        let enabled = value & 0x0080 != 0;
        self.io[0x84] = if enabled { 0x80 } else { 0 };
        self.io[0x85] = 0;
        // Turning the sound master off resets the Direct Sound FIFO state too.
        if was_enabled && !enabled {
            // Disabling the master sound circuit resets the implemented
            // Direct Sound control state as well as both FIFO/DAC latches.
            self.io[0x80..0x84].fill(0);
            for fifo in 0..2 {
                self.audio_fifos[fifo].clear();
                self.audio_last_samples[fifo] = 0;
            }
        }
    }

    fn write_audio_fifo_word(&mut self, fifo: usize, value: u32) {
        if let Some(queue) = self.audio_fifos.get_mut(fifo) {
            for byte in value.to_le_bytes() {
                if queue.len() == 32 {
                    break;
                }
                queue.push_back(byte);
            }
        }
    }

    fn clock_audio_fifo(&mut self, timer: usize) {
        if timer > 1 || self.read16(0x0400_0084) & 0x0080 == 0 {
            return;
        }
        let sound_control = self.read16(0x0400_0082);
        for fifo in 0..2 {
            // FIFO consumption follows its selected timer even when neither
            // speaker output is routed. SOUNDCNT_H routing only gates the DAC
            // mix; it must not pause the FIFO or its low-water DMA requests.
            let timer_bit = if fifo == 0 { 10 } else { 14 };
            if usize::from((sound_control >> timer_bit) & 1) != timer {
                continue;
            }
            // Direct Sound holds the last DAC sample if the FIFO runs dry;
            // only a successful pop changes the output latch.
            if let Some(sample) = self.audio_fifos[fifo].pop_front() {
                self.audio_last_samples[fifo] = sample as i8;
            }
            if self.audio_fifos[fifo].len() <= 16 {
                self.trigger_audio_fifo_dma(fifo);
            }
        }
    }

    fn trigger_audio_fifo_dma(&mut self, fifo: usize) {
        // FIFO A is serviced by DMA1, FIFO B by DMA2; each request transfers four words.
        let channel = fifo + 1;
        let base = 0xB0 + channel * 12;
        let control_offset = base + 10;
        let control = u16::from_le_bytes([self.io[control_offset], self.io[control_offset + 1]]);
        let destination = u32::from_le_bytes(self.io[base + 4..base + 8].try_into().unwrap()) & !3;
        let fifo_address = if fifo == 0 { 0x0400_00A0 } else { 0x0400_00A4 };
        // Sound FIFO DMA is supported only on DMA1/2, with word-sized
        // transfers, repeat enabled, incrementing source and fixed FIFO target.
        // Reject malformed configurations instead of silently applying FIFO
        // semantics to a channel programmed for an incompatible transfer.
        let source_mode = (control >> 7) & 3;
        let destination_mode = (control >> 5) & 3;
        let word_transfer = control & (1 << 10) != 0;
        let repeat = control & (1 << 9) != 0;
        if control & 0x8000 == 0
            || (control >> 12) & 3 != 3
            || !word_transfer
            || !repeat
            || source_mode != 0
            || destination_mode != 2
            || destination != fifo_address
        {
            return;
        }
        let source = u32::from_le_bytes(self.io[base..base + 4].try_into().unwrap()) & !3;
        for index in 0..4 {
            let word = self.read32(source.wrapping_add(index * 4));
            self.write_audio_fifo_word(fifo, word);
        }
        self.io[base..base + 4].copy_from_slice(&source.wrapping_add(16).to_le_bytes());
        if control & (1 << 14) != 0 {
            self.request_interrupt(1 << (8 + channel));
        }
    }

    /// Returns the Flash save image for persistence, when the cartridge uses Flash.
    pub fn flash_data(&self) -> Option<&[u8]> {
        self.flash.as_ref().map(crate::save::FlashDevice::data)
    }

    /// Loads a complete Flash save image; returns false when this cartridge is not Flash.
    pub fn load_flash_data(&mut self, data: &[u8]) -> Result<bool, SramSizeError> {
        match self.flash.as_mut() {
            Some(flash) => {
                flash.load_data(data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Returns the serial EEPROM save image for persistence, when present.
    pub fn eeprom_data(&self) -> Option<&[u8]> {
        self.eeprom.as_ref().map(crate::save::EepromDevice::data)
    }

    /// Loads a complete EEPROM save image; returns false when this cartridge is not EEPROM.
    pub fn load_eeprom_data(&mut self, data: &[u8]) -> Result<bool, SramSizeError> {
        match self.eeprom.as_mut() {
            Some(eeprom) => {
                eeprom.load_data(data)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Returns the SRAM save data for persistence by the frontend.
    ///
    /// The returned slice is the complete 32 KiB backing store, including
    /// mirrored address regions only once. The slice is read-only so callers
    /// cannot bypass the bus's memory semantics.
    pub fn sram_data(&self) -> &[u8] {
        &self.sram
    }

    /// Loads a complete SRAM save image.
    ///
    /// The size is validated before any bytes are changed, so a malformed save
    /// cannot partially overwrite the currently loaded save.
    pub fn load_sram_data(&mut self, data: &[u8]) -> Result<(), SramSizeError> {
        if data.len() != SRAM_SIZE {
            return Err(SramSizeError {
                expected: SRAM_SIZE,
                actual: data.len(),
            });
        }
        self.sram.copy_from_slice(data);
        Ok(())
    }

    /// Reads one byte using the current basic GBA address decoder.
    ///
    /// Unmapped and BIOS addresses return zero in this early bus model.
    pub fn read8(&self, address: u32) -> u8 {
        // SOUNDCNT_X channel-status bits are read-only. Channel status is not
        // synthesized yet, so expose only the writable sound-master bit.
        if (0x0400_0084..=0x0400_0085).contains(&address) {
            return if address == 0x0400_0084 {
                self.io[0x84] & 0x80
            } else {
                0
            };
        }
        // Timer counters are live hardware state; control registers expose only
        // the documented low three bits plus IRQ and enable.
        if (0x0400_0100..=0x0400_010F).contains(&address) {
            let offset = address - 0x0400_0100;
            let timer = (offset / 4) as usize;
            let register_offset = offset % 4;
            let value = if register_offset < 2 {
                self.timer_counter[timer]
            } else {
                self.timer_control[timer]
            };
            return ((value >> ((register_offset & 1) * 8)) & 0xFF) as u8;
        }
        // Video timing registers expose live status rather than the raw I/O bytes.
        if (0x0400_0004..=0x0400_0005).contains(&address) {
            return ((self.dispstat >> ((address - 0x0400_0004) * 8)) & 0xFF) as u8;
        }
        if (0x0400_0006..=0x0400_0007).contains(&address) {
            return ((self.vcount >> ((address - 0x0400_0006) * 8)) & 0xFF) as u8;
        }
        // Interrupt controller: IE, IF and IME. Reserved bits read as zero.
        for (base, value) in [
            (0x0400_0200, self.interrupt_enable),
            (0x0400_0202, self.interrupt_flags),
            (0x0400_0204, self.wait_control),
            (0x0400_0208, self.interrupt_master_enable),
        ] {
            if (base..=base + 1).contains(&address) {
                return ((value >> ((address - base) * 8)) & 0xFF) as u8;
            }
        }

        // KEYINPUT (0x04000130) is active-low and read-only. Bits 0..9 map
        // to A, B, Select, Start, Right, Left, Up, Down, R and L.
        if (0x0400_0130..=0x0400_0131).contains(&address) {
            let keyinput = (!self.keys_pressed) & 0x03FF;
            return ((keyinput >> ((address - 0x0400_0130) * 8)) & 0xFF) as u8;
        }
        if (0x0400_0132..=0x0400_0133).contains(&address) {
            return ((self.key_control >> ((address - 0x0400_0132) * 8)) & 0xFF) as u8;
        }

        match address {
            0x0200_0000..=0x02FF_FFFF => self.ewram[(address as usize - 0x0200_0000) % EWRAM_SIZE],
            0x0300_0000..=0x03FF_FFFF => self.iwram[(address as usize - 0x0300_0000) % IWRAM_SIZE],
            0x0400_0000..=0x0400_03FF => self.io[(address as usize - 0x0400_0000) % IO_SIZE],
            0x0500_0000..=0x05FF_FFFF => {
                self.palette[(address as usize - 0x0500_0000) % PALETTE_RAM_SIZE]
            }
            0x0600_0000..=0x06FF_FFFF => {
                let offset = (address as usize - 0x0600_0000) % 0x2_0000;
                let offset = if offset >= 0x1_8000 {
                    offset - 0x8_000
                } else {
                    offset
                };
                self.vram[offset % VRAM_SIZE]
            }
            0x0700_0000..=0x07FF_FFFF => self.oam[(address as usize - 0x0700_0000) % OAM_SIZE],
            // Each 32 MiB Game Pak wait-state region mirrors the ROM
            // independently: regions 0x08/0x09, 0x0A/0x0B and 0x0C/0x0D
            // all begin at ROM offset zero.
            0x0800_0000..=0x09FF_FFFF => {
                self.cartridge.read_rom_byte(address as usize - 0x0800_0000)
            }
            0x0A00_0000..=0x0BFF_FFFF => {
                self.cartridge.read_rom_byte(address as usize - 0x0A00_0000)
            }
            0x0C00_0000..=0x0DFF_FFFF => {
                self.cartridge.read_rom_byte(address as usize - 0x0C00_0000)
            }
            // GBA save SRAM is mapped only in the first 64 KiB of region 0x0E.
            // The rest of regions 0x0E/0x0F are not SRAM aliases.
            0x0E00_0000..=0x0E00_FFFF => {
                if let Some(flash) = &self.flash {
                    flash.read(address as u16)
                } else if self.save_type == Some(crate::SaveType::Eeprom) {
                    0xFF
                } else {
                    self.sram[(address as usize - 0x0E00_0000) % SRAM_SIZE]
                }
            }
            _ => 0,
        }
    }

    /// Sets the pressed GBA buttons using a bitmask (bit 0=A, 1=B, 2=Select,
    /// 3=Start, 4=Right, 5=Left, 6=Up, 7=Down, 8=R, 9=L).
    ///
    /// The hardware exposes these bits active-low through KEYINPUT; callers
    /// pass 1 for a pressed key and 0 for a released key.
    pub fn set_keys_pressed(&mut self, pressed: u16) {
        let was_requested = self.keypad_irq_requested();
        self.keys_pressed = pressed & 0x03FF;
        self.latch_keypad_irq_on_rising_edge(was_requested);
    }

    /// Returns the active-high mask of physically pressed buttons (KEYINPUT is inverted).
    pub fn keys_pressed(&self) -> u16 {
        self.keys_pressed
    }

    /// Sets one button's state without changing the other nine buttons.
    ///
    /// `pressed = true` means physically pressed; KEYINPUT remains active-low.
    /// A newly satisfied KEYCNT condition raises the keypad interrupt request.
    pub fn set_button_pressed(&mut self, button: GbaButton, pressed: bool) {
        let mask = button.mask();
        let next = if pressed {
            self.keys_pressed | mask
        } else {
            self.keys_pressed & !mask
        };
        self.set_keys_pressed(next);
    }

    /// Returns whether the specified physical button is currently pressed.
    pub fn is_button_pressed(&self, button: GbaButton) -> bool {
        self.keys_pressed & button.mask() != 0
    }

    /// Returns whether KEYCNT's configured keypad interrupt condition is met.
    pub fn keypad_irq_requested(&self) -> bool {
        if self.key_control & (1 << 14) == 0 {
            return false;
        }
        let selected = self.key_control & 0x03FF;
        if selected == 0 {
            return false;
        }
        if self.key_control & (1 << 15) == 0 {
            self.keys_pressed & selected != 0
        } else {
            self.keys_pressed & selected == selected
        }
    }

    /// Advances timers and video timing by a number of CPU clock cycles.
    ///
    /// Timer overflow reloads the configured value and raises the matching IF
    /// bit when enabled. Timers 1–3 can count overflows from the previous timer
    /// in cascade mode. Video timing advances 1232 cycles per scanline and updates
    /// DISPSTAT/VCOUNT and enabled HBlank, VBlank, and VCOUNT-match IRQs. The caller
    /// supplies elapsed CPU cycles; instruction-specific bus wait states are not yet modeled.
    pub fn advance_cycles(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.advance_video_cycle();
            let mut previous_overflow = false;
            for timer in 0..4 {
                let control = self.timer_control[timer];
                if control & 0x0080 == 0 {
                    previous_overflow = false;
                    continue;
                }

                let cascade = timer != 0 && control & 0x0004 != 0;
                let tick = if cascade {
                    previous_overflow
                } else {
                    let divisor = match control & 3 {
                        0 => 1,
                        1 => 64,
                        2 => 256,
                        _ => 1024,
                    };
                    self.timer_prescaler_phase[timer] += 1;
                    if self.timer_prescaler_phase[timer] >= divisor {
                        self.timer_prescaler_phase[timer] = 0;
                        true
                    } else {
                        false
                    }
                };

                previous_overflow = false;
                if tick {
                    let (next, overflow) = self.timer_counter[timer].overflowing_add(1);
                    if overflow {
                        self.timer_counter[timer] = self.timer_reload[timer];
                        if control & 0x0040 != 0 {
                            self.request_interrupt(1 << (3 + timer));
                        }
                        self.clock_audio_fifo(timer);
                        previous_overflow = true;
                    } else {
                        self.timer_counter[timer] = next;
                    }
                }
            }
        }
    }

    /// Writes a DMA control register and performs immediate-timing transfers.
    ///
    /// HBlank and VBlank requests are driven by video timing; sound FIFO
    /// requests are serviced by the timer-overflow path. The external Game Pak
    /// DRQ request line is not modeled yet.
    fn write_dma_control(&mut self, channel: usize, value: u16) {
        if channel >= 4 {
            return;
        }
        let base = 0xB0 + channel as u32 * 12;
        let control_offset = (base + 10) as usize;
        let old = u16::from_le_bytes([self.io[control_offset], self.io[control_offset + 1]]);
        // DMA3 alone implements bit 11 (Game Pak DRQ); it is reserved on DMA0–2.
        let control_mask = if channel == 3 { 0xFFE0 } else { 0xF7E0 };
        let control = value & control_mask;
        self.io[control_offset..control_offset + 2].copy_from_slice(&control.to_le_bytes());
        if control & 0x8000 == 0 || old & 0x8000 != 0 {
            return;
        }
        // DMA3's Game Pak DRQ bit selects an external request line regardless
        // of the timing-selector bits. Do not mistake selector 0 for an
        // immediate request while DRQ mode is enabled.
        let game_pak_drq = channel == 3 && control & (1 << 11) != 0;
        if !game_pak_drq && (control >> 12) & 3 == 0 {
            self.perform_dma_transfer(channel, control);
        }
    }

    /// Fires enabled DMA channels whose timing selector matches a peripheral event.
    fn trigger_dma_timing(&mut self, timing: u16) {
        for channel in 0..4 {
            let control_offset = 0xB0 + channel * 12 + 10;
            let control = u16::from_le_bytes([
                self.io[control_offset as usize],
                self.io[control_offset as usize + 1],
            ]);
            // DMA3 Game Pak DRQ selects an external request source, not
            // the normal VBlank/HBlank/FIFO timing events. Until the cartridge
            // DRQ line is modeled, leave such transfers pending rather than
            // incorrectly firing them from the video scheduler.
            let game_pak_drq = channel == 3 && control & (1 << 11) != 0;
            if control & 0x8000 != 0 && !game_pak_drq && (control >> 12) & 3 == timing {
                self.perform_dma_transfer(channel as usize, control);
            }
        }
    }

    /// Executes one configured DMA transfer and updates its address registers.
    fn perform_dma_transfer(&mut self, channel: usize, control: u16) {
        let base = 0xB0 + channel as u32 * 12;
        let source_offset = base as usize;
        let destination_offset = (base + 4) as usize;
        let count_offset = (base + 8) as usize;
        let source = u32::from_le_bytes(
            self.io[source_offset..source_offset + 4]
                .try_into()
                .unwrap(),
        );
        let destination = u32::from_le_bytes(
            self.io[destination_offset..destination_offset + 4]
                .try_into()
                .unwrap(),
        );
        let raw_count = u16::from_le_bytes([self.io[count_offset], self.io[count_offset + 1]]);
        let count_mask = if channel == 3 { 0xFFFF } else { 0x3FFF };
        let programmed_count = raw_count & count_mask;
        let count = if programmed_count == 0 {
            if channel == 3 {
                0x1_0000
            } else {
                0x4000
            }
        } else {
            u32::from(programmed_count)
        };
        let word = control & (1 << 10) != 0;
        let step = if word { 4_u32 } else { 2_u32 };
        let source_mode = (control >> 7) & 3;
        let destination_mode = (control >> 5) & 3;
        let mut src = source & !(step - 1);
        let mut dst = destination & !(step - 1);

        // EEPROM command width is selected by the halfword DMA length. The
        // following read-data DMA is 68 halfwords and keeps this selection.
        if !word && (0x0D00_0000..=0x0DFF_FFFF).contains(&dst) && matches!(count, 10 | 18 | 74 | 82)
        {
            if let Some(eeprom) = &mut self.eeprom {
                eeprom.configure_transfer_length(count);
            }
        }

        for _ in 0..count {
            if word {
                let data = self.read32(src);
                self.write32(dst, data);
            } else {
                let data = self.read16(src);
                self.write16(dst, data);
            }
            src = match source_mode {
                1 => src.wrapping_sub(step),
                2 => src,
                _ => src.wrapping_add(step),
            };
            dst = match destination_mode {
                1 => dst.wrapping_sub(step),
                2 => dst,
                _ => dst.wrapping_add(step),
            };
        }

        self.io[source_offset..source_offset + 4].copy_from_slice(&src.to_le_bytes());
        let timing = (control >> 12) & 3;
        let repeat = control & (1 << 9) != 0 && timing != 0;
        // Increment/reload only reloads the destination between repeated
        // timed transfers. An immediate transfer leaves the advanced address.
        let next_destination = if destination_mode == 3 && repeat {
            destination & !(step - 1)
        } else {
            dst
        };
        self.io[destination_offset..destination_offset + 4]
            .copy_from_slice(&next_destination.to_le_bytes());

        if control & (1 << 14) != 0 {
            self.request_interrupt(1 << (8 + channel));
        }
        if !repeat {
            let control_offset = (base + 10) as usize;
            self.io[control_offset..control_offset + 2]
                .copy_from_slice(&(control & !(1 << 15)).to_le_bytes());
        }
    }

    /// Advances the PPU timing state by one CPU cycle.
    ///
    /// A scanline lasts 1232 cycles: HBlank begins at cycle 960, and VBlank
    /// begins at scanline 160. DISPSTAT status bits and IRQ flags are updated
    /// on their hardware transition edges.
    fn advance_video_cycle(&mut self) {
        self.video_cycles_in_line += 1;
        if self.video_cycles_in_line == 960 {
            self.dispstat |= 1 << 1;
            // HBlank DMA transfers are for visible scanlines only. HBlank
            // status and its IRQ still occur during VBlank, but the DMA
            // request must not start another display transfer there.
            if self.vcount < 160 {
                self.trigger_dma_timing(2);
            }
            if self.dispstat & (1 << 4) != 0 {
                self.request_interrupt(1 << 1);
            }
        }
        if self.video_cycles_in_line < 1232 {
            return;
        }

        self.video_cycles_in_line = 0;
        self.vcount += 1;
        if self.vcount == 228 {
            self.vcount = 0;
        }
        self.dispstat &= !(1 << 1);
        if self.vcount == 160 {
            self.dispstat |= 1;
            self.trigger_dma_timing(1);
            if self.dispstat & (1 << 3) != 0 {
                self.request_interrupt(1);
            }
        } else if self.vcount == 0 {
            self.dispstat &= !1;
        }

        let was_match = self.dispstat & (1 << 2) != 0;
        let matches = self.vcount == (self.dispstat >> 8);
        if matches {
            self.dispstat |= 1 << 2;
            if !was_match && self.dispstat & (1 << 5) != 0 {
                self.request_interrupt(1 << 2);
            }
        } else {
            self.dispstat &= !(1 << 2);
        }
    }

    /// Returns the current scanline counter (VCOUNT), in the range 0..227.
    pub fn video_scanline(&self) -> u16 {
        self.vcount
    }

    /// Returns CPU cycles elapsed within the current scanline (0..1231).
    pub fn video_cycles_into_scanline(&self) -> u16 {
        self.video_cycles_in_line
    }

    /// Returns CPU cycles until the next HBlank entry (the cycle-960 boundary).
    ///
    /// Returns 960 cycles at the start of a scanline and 1 cycle immediately
    /// before HBlank. During HBlank, the result wraps to the next scanline.
    pub fn video_cycles_until_hblank(&self) -> u32 {
        let position = u32::from(self.video_cycles_in_line);
        if position < 960 {
            960 - position
        } else {
            1232 - position + 960
        }
    }

    /// Returns CPU cycles until the next VBlank entry at the start of line 160.
    ///
    /// If VBlank is already active, this returns the distance to the next
    /// frame's VBlank entry rather than zero.
    pub fn video_cycles_until_vblank(&self) -> u32 {
        let line = u32::from(self.vcount);
        let position = u32::from(self.video_cycles_in_line);
        let lines_until_entry = if line < 160 {
            160 - line
        } else {
            228 - line + 160
        };
        lines_until_entry * 1232 - position
    }

    /// Returns whether the video timing state is currently in VBlank.
    pub fn video_in_vblank(&self) -> bool {
        self.dispstat & (1 << 0) != 0
    }

    /// Returns whether the video timing state is currently in HBlank.
    pub fn video_in_hblank(&self) -> bool {
        self.dispstat & (1 << 1) != 0
    }

    /// Returns whether VCOUNT currently matches the programmed DISPSTAT compare value.
    pub fn video_vcount_match(&self) -> bool {
        self.dispstat & (1 << 2) != 0
    }

    /// Requests one or more GBA interrupt sources by setting their IF bits.
    ///
    /// Only the 14 defined interrupt sources are retained. Peripheral models
    /// such as video, DMA, and serial can use this without bypassing IF semantics.
    pub fn request_interrupt(&mut self, sources: u16) {
        self.interrupt_flags |= sources & 0x3FFF;
    }

    /// Acknowledges interrupt sources by clearing their IF bits.
    ///
    /// Only the 14 implemented GBA sources are affected. This matches the
    /// hardware's write-one-to-clear behavior and is useful to CPU/peripheral
    /// integrations that need to acknowledge an interrupt without synthesizing
    /// an MMIO write.
    pub fn acknowledge_interrupts(&mut self, sources: u16) {
        self.interrupt_flags &= !(sources & 0x3FFF);
    }

    /// Returns enabled interrupt sources, independent of IME.
    ///
    /// This is the IE/IF intersection used by CPU exception entry once IRQ
    /// modes and interrupt masking are implemented.
    pub fn pending_interrupts(&self) -> u16 {
        self.interrupt_enable & self.interrupt_flags & 0x3FFF
    }

    /// Returns true when IME is enabled and an enabled interrupt is pending.
    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_master_enable & 1 != 0 && self.pending_interrupts() != 0
    }

    /// Latches the keypad IRQ (interrupt bit 12) when its condition changes
    /// from inactive to active. IF remains latched until software writes one to clear it.
    fn latch_keypad_irq_on_rising_edge(&mut self, was_requested: bool) {
        if !was_requested && self.keypad_irq_requested() {
            self.request_interrupt(1 << 12);
        }
    }

    /// Reads a little-endian halfword, aligned down to a two-byte boundary.
    pub fn read16(&self, address: u32) -> u16 {
        // EEPROM is a serial device: DMA halfword reads clock one output bit.
        if (0x0D00_0000..=0x0DFF_FFFF).contains(&address) {
            if let Some(eeprom) = &self.eeprom {
                return eeprom.read_bit();
            }
        }
        // SRAM is byte-wide and uses the original byte address even for an
        // unsupported wider access; unlike ordinary halfword RAM, do not align it.
        if (0x0E00_0000..=0x0E00_FFFF).contains(&address) {
            let byte = u16::from(self.read8(address));
            return byte | (byte << 8);
        }
        let address = address & !1;
        u16::from_le_bytes([self.read8(address), self.read8(address.wrapping_add(1))])
    }

    /// Reads a little-endian word, aligned down to a four-byte boundary.
    pub fn read32(&self, address: u32) -> u32 {
        // The 8-bit SRAM data bus repeats the byte selected by the original
        // address for wider reads, including addresses that are not word-aligned.
        if (0x0E00_0000..=0x0E00_FFFF).contains(&address) {
            return u32::from(self.read8(address)) * 0x0101_0101;
        }
        let address = address & !3;
        u32::from_le_bytes([
            self.read8(address),
            self.read8(address.wrapping_add(1)),
            self.read8(address.wrapping_add(2)),
            self.read8(address.wrapping_add(3)),
        ])
    }

    /// Writes one byte to writable memory. Cartridge ROM and BIOS writes are ignored.
    pub fn write8(&mut self, address: u32, value: u8) {
        // DMA control's high byte contains the enable bit; support STRB writes.
        if (0x0400_00B0..=0x0400_00DF).contains(&address) {
            let offset = address - 0x0400_0000;
            let register_offset = offset - 0x00B0;
            if register_offset % 12 == 10 || register_offset % 12 == 11 {
                let control_address = address & !1;
                let current = self.read16(control_address);
                let shift = (address & 1) * 8;
                let byte_mask = 0x00FF_u16 << shift;
                let next = (current & !byte_mask) | (u16::from(value) << shift);
                self.write_dma_control((register_offset / 12) as usize, next);
                return;
            }
        }
        // Timer registers are 16-bit. Byte writes update the corresponding
        // half of the reload/control register without bypassing timer state.
        if (0x0400_0100..=0x0400_010F).contains(&address) {
            let offset = address - 0x0400_0100;
            let timer = (offset / 4) as usize;
            let shift = (offset & 1) * 8;
            if offset % 4 < 2 {
                let mask = 0xFF_u16 << shift;
                self.timer_reload[timer] =
                    (self.timer_reload[timer] & !mask) | ((value as u16) << shift);
                let io_offset = (address - 0x0400_0000) as usize;
                self.io[io_offset] = value;
            } else {
                let old_control = self.timer_control[timer];
                let mask = 0x00C7_u16 & (0xFF_u16 << shift);
                self.timer_control[timer] =
                    (old_control & !mask) | (((value as u16) << shift) & mask);
                if old_control & 0x0080 == 0 && self.timer_control[timer] & 0x0080 != 0 {
                    self.timer_counter[timer] = self.timer_reload[timer];
                    self.timer_prescaler_phase[timer] = 0;
                } else if self.timer_control[timer] & 0x0080 == 0 {
                    // Keep byte-wide control writes consistent with write16:
                    // a disabled timer must not retain a partial prescaler phase.
                    self.timer_prescaler_phase[timer] = 0;
                }
                let io_offset = (address - 0x0400_0000) as usize;
                self.io[io_offset] = ((self.timer_control[timer] >> shift) & 0xFF) as u8;
            }
            return;
        }
        // DISPSTAT: status bits 0..2 are read-only; IRQ enables and compare
        // scanline are writable. VCOUNT itself is read-only.
        if (0x0400_0004..=0x0400_0005).contains(&address) {
            let old_match = self.dispstat & (1 << 2) != 0;
            let shift = (address - 0x0400_0004) * 8;
            let writable_mask = 0xFF_u16 << shift & 0xFF38;
            self.dispstat =
                (self.dispstat & !writable_mask) | ((u16::from(value) << shift) & writable_mask);
            let matches = self.vcount == (self.dispstat >> 8);
            if matches {
                self.dispstat |= 1 << 2;
                if !old_match && self.dispstat & (1 << 5) != 0 {
                    self.request_interrupt(1 << 2);
                }
            } else {
                self.dispstat &= !(1 << 2);
            }
            return;
        }
        if (0x0400_0006..=0x0400_0007).contains(&address) {
            return;
        }
        // IE: only IRQ sources 0..13 are implemented by the GBA interrupt controller.
        if (0x0400_0200..=0x0400_0201).contains(&address) {
            let shift = (address - 0x0400_0200) * 8;
            let byte_mask = 0xFF_u16 << shift;
            self.interrupt_enable =
                ((self.interrupt_enable & !byte_mask) | ((value as u16) << shift)) & 0x3FFF;
            return;
        }
        // IF is write-one-to-clear; writing zero leaves pending flags unchanged.
        if (0x0400_0202..=0x0400_0203).contains(&address) {
            let shift = (address - 0x0400_0202) * 8;
            self.acknowledge_interrupts((u16::from(value) << shift) & 0x3FFF);
            return;
        }
        // WAITCNT exposes SRAM/Game Pak wait-state, PHI, and prefetch controls;
        // bit 15 reads as zero; the modeled timing and high-byte controls are preserved.
        if (0x0400_0204..=0x0400_0205).contains(&address) {
            let shift = (address - 0x0400_0204) * 8;
            let byte_mask = 0xFF_u16 << shift;
            self.wait_control =
                ((self.wait_control & !byte_mask) | ((value as u16) << shift & byte_mask)) & 0x7FFF;
            return;
        }
        // IME exposes only bit 0.
        if (0x0400_0208..=0x0400_0209).contains(&address) {
            if address == 0x0400_0208 {
                self.interrupt_master_enable = (value as u16) & 1;
            }
            return;
        }

        // SOUNDCNT_H supports byte writes too; reset commands in its high
        // byte must not be lost by the generic I/O backing-store path.
        if (0x0400_0082..=0x0400_0083).contains(&address) {
            let current = u16::from_le_bytes([self.io[0x82], self.io[0x83]]);
            let shift = (address & 1) * 8;
            let mask = 0x00FF_u16 << shift;
            let next = (current & !mask) | (u16::from(value) << shift);
            self.write_sound_control_high(next);
            return;
        }

        // SOUNDCNT_X exposes only the sound-master enable bit to software.
        if (0x0400_0084..=0x0400_0085).contains(&address) {
            if address == 0x0400_0084 {
                self.write_sound_master_control(u16::from(value));
            }
            return;
        }

        // Sound FIFO ports are write-only and only accept 32-bit pushes.
        if (0x0400_00A0..=0x0400_00A7).contains(&address) {
            return;
        }

        // KEYINPUT is read-only; KEYCNT accepts only key-select, IRQ-enable and
        // condition-mode bits (0..9, 14 and 15).
        if (0x0400_0130..=0x0400_0131).contains(&address) {
            return;
        }
        if (0x0400_0132..=0x0400_0133).contains(&address) {
            let was_requested = self.keypad_irq_requested();
            let shift = (address - 0x0400_0132) * 8;
            let mask = 0xC3FF_u16;
            let byte_mask = 0xFF_u16 << shift;
            let incoming = (value as u16) << shift;
            self.key_control =
                (self.key_control & !(byte_mask & mask)) | (incoming & byte_mask & mask);
            self.latch_keypad_irq_on_rising_edge(was_requested);
            return;
        }

        match address {
            0x0200_0000..=0x02FF_FFFF => {
                self.ewram[(address as usize - 0x0200_0000) % EWRAM_SIZE] = value
            }
            0x0300_0000..=0x03FF_FFFF => {
                self.iwram[(address as usize - 0x0300_0000) % IWRAM_SIZE] = value
            }
            0x0400_0000..=0x0400_03FF => {
                self.io[(address as usize - 0x0400_0000) % IO_SIZE] = value
            }
            0x0500_0000..=0x05FF_FFFF => {
                // Palette RAM byte writes replicate the byte across its halfword.
                let offset = (address as usize - 0x0500_0000) % PALETTE_RAM_SIZE;
                let offset = offset & !1;
                self.palette[offset] = value;
                self.palette[offset + 1] = value;
            }
            0x0600_0000..=0x06FF_FFFF => {
                // VRAM byte writes replicate across a halfword. Apply the GBA's
                // 96 KiB mirroring rule before aligning the physical offset.
                let offset = (address as usize - 0x0600_0000) % 0x2_0000;
                let offset = if offset >= 0x1_8000 {
                    offset - 0x8_000
                } else {
                    offset
                };
                let offset = (offset % VRAM_SIZE) & !1;
                self.vram[offset] = value;
                self.vram[offset + 1] = value;
            }
            // OAM does not support byte writes on GBA hardware.
            0x0700_0000..=0x07FF_FFFF => {}
            0x0E00_0000..=0x0E00_FFFF => {
                if let Some(flash) = &mut self.flash {
                    flash.write(address as u16, value);
                } else if self.save_type != Some(crate::SaveType::Eeprom) {
                    self.sram[(address as usize - 0x0E00_0000) % SRAM_SIZE] = value;
                }
            }
            _ => {}
        }
    }

    /// Writes a little-endian halfword, aligned down to a two-byte boundary.
    pub fn write16(&mut self, address: u32, value: u16) {
        // EEPROM command streams are transferred as halfword bit values by DMA.
        if (0x0D00_0000..=0x0DFF_FFFF).contains(&address) {
            if let Some(eeprom) = &mut self.eeprom {
                eeprom.write_bit(value);
                return;
            }
        }
        if (0x0400_00BA..=0x0400_00DF).contains(&address) && (address - 0x0400_00B0) % 12 == 10 {
            self.write_dma_control(((address - 0x0400_00B0) / 12) as usize, value);
            return;
        }
        if (0x0400_0004..=0x0400_0005).contains(&address) {
            let old_match = self.dispstat & (1 << 2) != 0;
            self.dispstat = (self.dispstat & 0x0007) | (value & 0xFF38);
            let matches = self.vcount == (self.dispstat >> 8);
            if matches {
                self.dispstat |= 1 << 2;
                if !old_match && self.dispstat & (1 << 5) != 0 {
                    self.request_interrupt(1 << 2);
                }
            } else {
                self.dispstat &= !(1 << 2);
            }
            return;
        }
        if (0x0400_0006..=0x0400_0007).contains(&address) {
            return;
        }
        if (0x0400_0100..=0x0400_010F).contains(&address) {
            let address = address & !1;
            let offset = address - 0x0400_0100;
            let timer = (offset / 4) as usize;
            if offset % 4 == 0 {
                self.timer_reload[timer] = value;
                let io_offset = (address - 0x0400_0000) as usize;
                let bytes = value.to_le_bytes();
                self.io[io_offset] = bytes[0];
                self.io[io_offset + 1] = bytes[1];
            } else {
                let old_control = self.timer_control[timer];
                self.timer_control[timer] = value & 0x00C7;
                if old_control & 0x0080 == 0 && self.timer_control[timer] & 0x0080 != 0 {
                    self.timer_counter[timer] = self.timer_reload[timer];
                    self.timer_prescaler_phase[timer] = 0;
                } else if self.timer_control[timer] & 0x0080 == 0 {
                    self.timer_prescaler_phase[timer] = 0;
                }
                let io_offset = (address - 0x0400_0000) as usize;
                let bytes = self.timer_control[timer].to_le_bytes();
                self.io[io_offset] = bytes[0];
                self.io[io_offset + 1] = bytes[1];
            }
            return;
        }
        if (0x0400_0204..=0x0400_0205).contains(&address) {
            self.wait_control = value & 0x7FFF;
            return;
        }
        if (address == 0x0400_0084 || address == 0x0400_0085) {
            // SOUNDCNT_X's master-enable bit lives only in the low byte;
            // writes to the reserved high byte must not change sound state.
            if address == 0x0400_0084 {
                self.write_sound_master_control(value);
            }
            return;
        }
        if (address & !1) == 0x0400_0082 {
            self.write_sound_control_high(value);
            return;
        }
        if (0x0400_00A0..=0x0400_00A7).contains(&address) {
            return;
        }
        // SRAM is byte-wide. The bus selects the addressed byte lane, so an
        // odd-address halfword store presents the source's high byte.
        if (0x0E00_0000..=0x0E00_FFFF).contains(&address) {
            let byte = value.rotate_right((address & 1) * 8) as u8;
            self.write8(address, byte);
            return;
        }
        let address = address & !1;

        // KEYCNT is a single 16-bit register. Apply a halfword write atomically
        // so intermediate byte values cannot spuriously latch a keypad IRQ.
        if address == 0x0400_0132 {
            let was_requested = self.keypad_irq_requested();
            self.key_control = value & 0xC3FF;
            self.latch_keypad_irq_on_rising_edge(was_requested);
            return;
        }

        let bytes = value.to_le_bytes();
        // These video-memory regions have width-specific write semantics, so
        // halfword stores must bypass write8's byte-write behavior.
        match address {
            0x0500_0000..=0x05FF_FFFF => {
                let offset = (address as usize - 0x0500_0000) % PALETTE_RAM_SIZE;
                self.palette[offset] = bytes[0];
                self.palette[offset + 1] = bytes[1];
            }
            0x0600_0000..=0x06FF_FFFF => {
                let offset = (address as usize - 0x0600_0000) % 0x2_0000;
                let offset = if offset >= 0x1_8000 {
                    offset - 0x8_000
                } else {
                    offset
                };
                let offset = (offset % VRAM_SIZE) & !1;
                self.vram[offset] = bytes[0];
                self.vram[offset + 1] = bytes[1];
            }
            0x0700_0000..=0x07FF_FFFF => {
                let offset = (address as usize - 0x0700_0000) % OAM_SIZE;
                self.oam[offset] = bytes[0];
                self.oam[offset + 1] = bytes[1];
            }
            _ => {
                self.write8(address, bytes[0]);
                self.write8(address.wrapping_add(1), bytes[1]);
            }
        }
    }

    /// Writes a little-endian word, aligned down to a four-byte boundary.
    pub fn write32(&mut self, address: u32, value: u32) {
        if (0x0400_00A0..=0x0400_00A7).contains(&address) {
            let fifo = usize::from(address >= 0x0400_00A4);
            self.write_audio_fifo_word(fifo, value);
            return;
        }
        // SRAM is byte-wide. Rotate by the original address so the selected
        // byte lane reaches SRAM, and perform only one physical byte write.
        if (0x0E00_0000..=0x0E00_FFFF).contains(&address) {
            let byte = value.rotate_right((address & 3) * 8) as u8;
            self.write8(address, byte);
            return;
        }
        let address = address & !3;
        let bytes = value.to_le_bytes();
        self.write16(address, u16::from_le_bytes([bytes[0], bytes[1]]));
        self.write16(
            address.wrapping_add(2),
            u16::from_le_bytes([bytes[2], bytes[3]]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GbaCartridge;

    fn cartridge() -> GbaCartridge {
        let mut rom = vec![0; 0xC0];
        let checksum = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        rom[0xBD] = checksum;
        GbaCartridge::parse(&rom).expect("test ROM header should validate")
    }

    #[test]
    fn button_catalog_covers_all_hardware_key_bits() {
        let combined = GbaButton::ALL
            .iter()
            .fold(0_u16, |mask, button| mask | button.mask());
        assert_eq!(GbaButton::ALL.len(), 10);
        assert_eq!(combined, 0x03FF);
        assert_eq!(
            GbaButton::ALL
                .iter()
                .map(|button| button.mask())
                .collect::<Vec<_>>(),
            (0..10).map(|bit| 1_u16 << bit).collect::<Vec<_>>()
        );
    }

    #[test]
    fn individual_button_updates_preserve_other_keys_and_trigger_keypad_irq() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(
            0x0400_0132,
            (1 << 14) | GbaButton::A.mask() | GbaButton::Start.mask(), // IRQ enabled, OR mode
        );
        bus.set_button_pressed(GbaButton::Start, true);
        assert!(bus.is_button_pressed(GbaButton::Start));
        assert!(!bus.is_button_pressed(GbaButton::A));
        assert_eq!(
            bus.read16(0x0400_0130) & 0x03FF,
            !(GbaButton::Start.mask()) & 0x03FF
        );
        assert_eq!(bus.interrupt_request_flags() & (1 << 12), 1 << 12);

        bus.set_button_pressed(GbaButton::A, true);
        assert!(bus.is_button_pressed(GbaButton::A));
        assert!(bus.is_button_pressed(GbaButton::Start));
        bus.set_button_pressed(GbaButton::Start, false);
        assert!(!bus.is_button_pressed(GbaButton::Start));
        assert!(bus.is_button_pressed(GbaButton::A));
    }

    #[test]
    fn keypad_and_mode_waits_for_every_selected_button() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(
            0x0400_0132,
            (1 << 14) | (1 << 15) | GbaButton::A.mask() | GbaButton::Start.mask(),
        );

        bus.set_button_pressed(GbaButton::Start, true);
        assert_eq!(bus.interrupt_request_flags() & (1 << 12), 0);
        bus.set_button_pressed(GbaButton::A, true);
        assert_eq!(bus.interrupt_request_flags() & (1 << 12), 1 << 12);
    }

    #[test]
    fn memory_region_snapshots_reflect_bus_writes_and_keep_hardware_sizes() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write8(0x0200_0012, 0xA5);
        bus.write16(0x0300_0004, 0xBEEF);
        bus.write16(0x0500_0002, 0x1234);
        bus.write16(0x0600_0006, 0x5678);
        bus.write16(0x0700_0008, 0x9ABC);

        assert_eq!(bus.external_work_ram().len(), EWRAM_SIZE);
        assert_eq!(bus.internal_work_ram().len(), IWRAM_SIZE);
        assert_eq!(bus.palette_ram().len(), PALETTE_RAM_SIZE);
        assert_eq!(bus.video_ram().len(), VRAM_SIZE);
        assert_eq!(bus.object_attribute_memory().len(), OAM_SIZE);
        assert_eq!(bus.external_work_ram()[0x12], 0xA5);
        assert_eq!(&bus.internal_work_ram()[4..6], &[0xEF, 0xBE]);
        assert_eq!(&bus.palette_ram()[2..4], &[0x34, 0x12]);
        assert_eq!(&bus.video_ram()[6..8], &[0x78, 0x56]);
        assert_eq!(&bus.object_attribute_memory()[8..10], &[0xBC, 0x9A]);
    }

    #[test]
    fn video_timing_accessors_track_scanline_blank_and_compare_state() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.video_scanline(), 0);
        assert!(!bus.video_in_vblank());
        assert!(!bus.video_in_hblank());
        assert!(bus.video_vcount_match());

        // HBlank begins after 960 cycles and ends at the 1232-cycle line boundary.
        bus.advance_cycles(960);
        assert_eq!(bus.video_scanline(), 0);
        assert_eq!(bus.video_cycles_into_scanline(), 960);
        assert!(bus.video_in_hblank());
        assert!(!bus.video_in_vblank());

        bus.advance_cycles(272);
        assert_eq!(bus.video_scanline(), 1);
        assert!(!bus.video_in_hblank());
        assert!(!bus.video_in_vblank());
        assert!(!bus.video_vcount_match());

        // VCOUNT compare is set when the programmed line is reached.
        bus.write16(0x0400_0004, 1 << 8);
        assert!(bus.video_vcount_match());

        // Enter VBlank at line 160.
        bus.advance_cycles(1232 * 159);
        assert_eq!(bus.video_scanline(), 160);
        assert!(bus.video_in_vblank());
    }

    #[test]
    fn video_cycle_distance_helpers_track_next_blank_events() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.video_cycles_until_hblank(), 960);
        assert_eq!(bus.video_cycles_until_vblank(), 160 * 1232);

        bus.advance_cycles(959);
        assert_eq!(bus.video_cycles_until_hblank(), 1);
        bus.advance_cycles(1);
        assert_eq!(bus.video_cycles_until_hblank(), 1232);

        bus.advance_cycles(272);
        bus.advance_cycles(1232 * 159);
        assert_eq!(bus.video_scanline(), 160);
        assert!(bus.video_in_vblank());
        assert_eq!(bus.video_cycles_until_vblank(), (228 - 160 + 160) * 1232);
    }

    #[test]
    fn ram_regions_support_little_endian_access_and_mirroring() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write32(0x0200_0001, 0x1234_5678);
        assert_eq!(bus.read32(0x0200_0000), 0x1234_5678);
        assert_eq!(bus.read32(0x0204_0000), 0x1234_5678);

        bus.write16(0x0300_0002, 0xABCD);
        assert_eq!(bus.read16(0x0300_0002), 0xABCD);
        assert_eq!(bus.read16(0x0300_8002), 0xABCD);
    }

    #[test]
    fn rom_is_readable_and_writes_are_ignored() {
        let mut rom = vec![0; 0xC0];
        rom[0] = 0xEA;
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let cartridge = GbaCartridge::parse(&rom).unwrap();
        let mut bus = MemoryBus::new(cartridge);
        assert_eq!(bus.read8(0x0800_0000), 0xEA);
        // A short dump leaves the unpopulated tail of its address window at 0xFF;
        // it must not wrap at the exact file length as modulo arithmetic would.
        assert_eq!(bus.read8(0x0800_00C0), 0xFF);
        assert_eq!(bus.read8(0x0800_0100), 0xEA);
        bus.write8(0x0800_0000, 0x11);
        assert_eq!(bus.read8(0x0800_0000), 0xEA);
    }

    #[test]
    fn rom_mirrors_from_the_start_in_each_game_pak_region() {
        let mut rom = vec![0; 0xC0];
        rom[0] = 0xEA;
        rom[1] = 0x12;
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let cartridge = GbaCartridge::parse(&rom).unwrap();
        let bus = MemoryBus::new(cartridge);

        for address in [0x0800_0000, 0x0A00_0000, 0x0C00_0000] {
            assert_eq!(bus.read8(address), 0xEA, "ROM base at {address:#010X}");
            assert_eq!(
                bus.read8(address + 1),
                0x12,
                "ROM byte at {:#010X}",
                address + 1
            );
        }
    }

    #[test]
    fn audio_fifo_refill_distance_tracks_queue_threshold() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.audio_fifo_bytes_until_refill(0), Some(0));
        assert_eq!(bus.audio_fifo_bytes_until_refill(2), None);

        bus.write32(0x0400_00A0, 0x0102_0304);
        assert_eq!(bus.audio_fifo_len(0), Some(4));
        assert_eq!(bus.audio_fifo_bytes_until_refill(0), Some(0));
        for _ in 0..7 {
            bus.write32(0x0400_00A0, 0x0102_0304);
        }
        assert_eq!(bus.audio_fifo_len(0), Some(32));
        assert_eq!(bus.audio_fifo_bytes_until_refill(0), Some(16));
    }

    #[test]
    fn interrupt_requests_are_masked_and_pending_sources_ignore_ime() {
        let mut bus = MemoryBus::new(cartridge());

        bus.write16(0x0400_0200, 0xFFFF); // IE: reserved bits are ignored.
        bus.request_interrupt(0xFFFF); // IF accepts only defined sources.
        assert_eq!(bus.interrupt_enable_mask(), 0x3FFF);
        assert_eq!(bus.interrupt_request_flags(), 0x3FFF);
        assert!(!bus.interrupt_master_enabled());
        assert_eq!(bus.read16(0x0400_0202), 0x3FFF);
        assert_eq!(bus.pending_interrupts(), 0x3FFF);
        assert!(!bus.interrupt_pending()); // IME remains disabled.

        bus.write16(0x0400_0208, 1);
        assert!(bus.interrupt_pending());
        bus.write16(0x0400_0202, 0x3FFF); // IF is write-one-to-clear.
        assert_eq!(bus.pending_interrupts(), 0);
        assert!(!bus.interrupt_pending());
    }

    #[test]
    fn interrupt_acknowledgement_clears_only_selected_defined_sources() {
        let mut bus = MemoryBus::new(cartridge());
        bus.request_interrupt(0xFFFF);
        bus.acknowledge_interrupts((1 << 2) | (1 << 12) | (1 << 15));
        assert_eq!(
            bus.interrupt_request_flags(),
            0x3FFF & !((1 << 2) | (1 << 12))
        );
        // IF remains write-one-to-clear when acknowledged through MMIO.
        bus.write8(0x0400_0202, 1 << 3);
        assert_eq!(
            bus.interrupt_request_flags(),
            0x3FFF & !((1 << 2) | (1 << 3) | (1 << 12))
        );
    }

    #[test]
    fn pending_interrupts_respect_ie_if_and_global_ime() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0200, (1 << 0) | (1 << 3));
        bus.request_interrupt((1 << 3) | (1 << 4));
        assert_eq!(bus.pending_interrupts(), 1 << 3);
        assert!(!bus.interrupt_pending());

        bus.write16(0x0400_0208, 1);
        assert!(bus.interrupt_pending());
        bus.write16(0x0400_0202, 1 << 3);
        assert_eq!(bus.pending_interrupts(), 0);
        assert!(!bus.interrupt_pending());

        // IF flags not enabled in IE remain pending but cannot trigger readiness.
        assert_eq!(bus.interrupt_request_flags(), 1 << 4);
    }

    #[test]
    fn interrupt_and_waitcnt_accessors_track_mmio_writes() {
        let mut bus = MemoryBus::new(cartridge());

        bus.write16(0x0400_0200, 1 << 3);
        bus.request_interrupt(1 << 3);
        assert_eq!(bus.interrupt_enable_mask(), 1 << 3);
        assert_eq!(bus.interrupt_request_flags(), 1 << 3);
        assert!(!bus.interrupt_master_enabled());

        bus.write16(0x0400_0208, 1);
        assert!(bus.interrupt_master_enabled());
        assert!(bus.interrupt_pending());

        // IF is write-one-to-clear; the diagnostic accessor reflects the live register.
        bus.write16(0x0400_0202, 1 << 3);
        assert_eq!(bus.interrupt_request_flags(), 0);
        assert!(!bus.interrupt_pending());

        bus.write16(0x0400_0204, 0x7FFD);
        assert_eq!(bus.gamepak_wait_control(), bus.read16(0x0400_0204));
        assert_eq!(bus.gamepak_wait_control(), 0x7FFD);
    }

    #[test]
    fn waitcnt_masks_reserved_bits_and_supports_byte_writes() {
        let mut bus = MemoryBus::new(cartridge());

        // WAITCNT exposes timing/PHI/prefetch fields; bit 15 is reserved in this model.
        bus.write16(0x0400_0204, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0204), 0x7FFF);

        bus.write16(0x0400_0204, 0);
        bus.write8(0x0400_0204, 0xA5);
        bus.write8(0x0400_0205, 0x7F);
        assert_eq!(bus.read16(0x0400_0204), 0x7FA5);
        assert_eq!(bus.read8(0x0400_0204), 0xA5);
        assert_eq!(bus.read8(0x0400_0205), 0x7F);
    }

    #[test]
    fn keyinput_is_active_low_read_only_and_masks_unused_bits() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.read16(0x0400_0130), 0x03FF);

        bus.set_keys_pressed(0b10_0000_0001);
        assert_eq!(bus.read16(0x0400_0130), 0b01_1111_1110);
        assert_eq!(bus.read8(0x0400_0130), 0xFE);
        assert_eq!(bus.read8(0x0400_0131), 0x01);

        bus.write16(0x0400_0130, 0);
        assert_eq!(bus.read16(0x0400_0130), 0b01_1111_1110);
        bus.set_keys_pressed(u16::MAX);
        assert_eq!(bus.read16(0x0400_0130), 0);
    }

    #[test]
    fn keycnt_masks_reserved_bits_and_evaluates_or_and_conditions() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.read16(0x0400_0132), 0);
        assert!(!bus.keypad_irq_requested());

        // IRQ enabled, A or B pressed: OR mode triggers for either selected key.
        bus.write16(0x0400_0132, 0x4003);
        bus.set_keys_pressed(0b10);
        assert!(bus.keypad_irq_requested());
        bus.set_keys_pressed(0b100);
        assert!(!bus.keypad_irq_requested());

        // AND mode requires both selected keys to be pressed simultaneously.
        bus.write16(0x0400_0132, 0xC003);
        bus.set_keys_pressed(0b1);
        assert!(!bus.keypad_irq_requested());
        bus.set_keys_pressed(0b11);
        assert!(bus.keypad_irq_requested());

        // Reserved bits are ignored on writes and never appear on reads.
        bus.write16(0x0400_0132, 0x3C00);
        assert_eq!(bus.read16(0x0400_0132), 0);
        assert!(!bus.keypad_irq_requested());
    }

    #[test]
    fn keycnt_halfword_write_does_not_latch_irq_from_intermediate_value() {
        let mut bus = MemoryBus::new(cartridge());
        bus.set_keys_pressed(1);

        // IRQ enabled but no keys selected: condition remains inactive.
        bus.write16(0x0400_0132, 0x4000);
        assert_eq!(bus.read16(0x0400_0202), 0);

        // Final value selects A but disables IRQ. A byte-at-a-time write
        // would transiently combine A with the old IRQ-enable bit.
        bus.write16(0x0400_0132, 0x0001);
        assert_eq!(bus.read16(0x0400_0132), 0x0001);
        assert_eq!(bus.read16(0x0400_0202), 0);
    }

    #[test]
    fn interrupt_registers_latch_keypad_irq_and_obey_masks_and_acknowledgement() {
        let mut bus = MemoryBus::new(cartridge());

        // Select A, enable keypad IRQ, then press A: IF bit 12 must latch.
        bus.write16(0x0400_0132, 0x4001);
        bus.set_keys_pressed(1);
        assert_eq!(bus.read16(0x0400_0202), 1 << 12);
        assert!(!bus.interrupt_pending());

        // IE and IME jointly gate the CPU-visible pending interrupt.
        bus.write16(0x0400_0200, 1 << 12);
        assert!(!bus.interrupt_pending());
        bus.write16(0x0400_0208, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0208), 1);
        assert!(bus.interrupt_pending());

        // IF is write-one-to-clear, including byte writes; zero doesn't clear.
        bus.write8(0x0400_0202, 0);
        assert_eq!(bus.read16(0x0400_0202), 1 << 12);
        bus.write8(0x0400_0203, 0x10);
        assert_eq!(bus.read16(0x0400_0202), 0);
        assert!(!bus.interrupt_pending());

        // A still-held key does not immediately relatch after IF is cleared;
        // release and press again creates a new keypad condition edge.
        bus.set_keys_pressed(0);
        bus.set_keys_pressed(1);
        assert_eq!(bus.read16(0x0400_0202), 1 << 12);

        // IE reserved bits and IME high bits are masked.
        bus.write16(0x0400_0200, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0200), 0x3FFF);
        bus.write16(0x0400_0208, 0x0100);
        assert_eq!(bus.read16(0x0400_0208), 0);
    }

    #[test]
    fn timers_reload_raise_irq_and_cascade() {
        let mut bus = MemoryBus::new(cartridge());

        // Timer 0 overflows after two ticks and raises IRQ 3.
        bus.write16(0x0400_0100, 0xFFFE);
        bus.write16(0x0400_0102, 0x00C0); // enable + IRQ, divisor 1
        assert_eq!(bus.read16(0x0400_0100), 0xFFFE);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
        assert_eq!(bus.read16(0x0400_0102), 0x00C0);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 0xFFFE);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 3), 1 << 3);

        // Timer 1 in cascade mode ticks once for timer 0's overflow.
        bus.write16(0x0400_0102, 0x0000); // disable and reset prescaler
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080); // re-enable to load FFFF
        bus.write16(0x0400_0104, 0x1234);
        bus.write16(0x0400_0106, 0x0084); // enable + cascade
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0104), 0x1235);
    }

    #[test]
    fn timer_cascade_propagates_overflow_through_multiple_timers() {
        let mut bus = MemoryBus::new(cartridge());

        // Each timer is one tick from overflow. Timer 0's overflow must tick
        // timer 1, whose own overflow must then tick timer 2 in the same cycle.
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080);
        bus.write16(0x0400_0104, 0xFFFF);
        bus.write16(0x0400_0106, 0x0084);
        bus.write16(0x0400_0108, 0x1234);
        bus.write16(0x0400_010A, 0x0084);

        bus.advance_cycles(1);

        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
        assert_eq!(bus.read16(0x0400_0104), 0xFFFF);
        assert_eq!(bus.read16(0x0400_0108), 0x1235);
    }

    #[test]
    fn disabling_timer_with_byte_write_resets_prescaler_phase() {
        let mut bus = MemoryBus::new(cartridge());

        // Divisor 64: accumulate a partial prescaler phase, then disable the
        // timer through its low control byte (the byte-wide register path).
        bus.write16(0x0400_0100, 0);
        bus.write16(0x0400_0102, 0x0081);
        bus.advance_cycles(32);
        assert_eq!(bus.read16(0x0400_0100), 0);

        bus.write8(0x0400_0102, 0x01); // Disable, retain divisor 64.
        bus.write8(0x0400_0102, 0x81); // Re-enable with divisor 64.
        bus.advance_cycles(63);
        assert_eq!(bus.read16(0x0400_0100), 0);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 1);
    }

    #[test]
    fn timer_byte_writes_update_reload_without_changing_live_counter() {
        let mut bus = MemoryBus::new(cartridge());

        bus.write8(0x0400_0100, 0xFE);
        bus.write8(0x0400_0101, 0xFF);
        bus.write8(0x0400_0102, 0x80); // Enable timer 0, divisor 1.
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);

        // Updating the reload register while enabled must not overwrite the
        // live counter; the new reload value takes effect at the next overflow.
        bus.write8(0x0400_0100, 0x10);
        assert_eq!(bus.read16(0x0400_0100), 0xFFFF);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 0xFF10);

        // Disabling and re-enabling loads the latest reload value immediately.
        bus.write8(0x0400_0102, 0);
        bus.write8(0x0400_0102, 0x80);
        assert_eq!(bus.read16(0x0400_0100), 0xFF10);
    }

    #[test]
    fn timer_inspection_reports_live_reload_and_overflow_distance() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.timer_counter(0), Some(0));
        assert_eq!(bus.timer_reload_value(0), Some(0));
        assert_eq!(bus.timer_enabled(0), Some(false));
        assert_eq!(bus.timer_cycles_until_overflow(0), None);
        assert_eq!(bus.timer_prescaler_phase(0), Some(0));
        assert_eq!(bus.timer_prescaler_phase(4), None);
        assert_eq!(bus.timer_counter(4), None);

        bus.write16(0x0400_0100, 0xFFFE);
        bus.write16(0x0400_0102, 0x0081);
        assert_eq!(bus.timer_counter(0), Some(0xFFFE));
        assert_eq!(bus.timer_reload_value(0), Some(0xFFFE));
        assert_eq!(bus.timer_cycles_until_overflow(0), Some(128));
        assert_eq!(bus.timer_prescaler_phase(0), Some(0));

        bus.advance_cycles(17);
        assert_eq!(bus.timer_prescaler_phase(0), Some(17));
        assert_eq!(bus.timer_cycles_until_overflow(0), Some(111));
        bus.advance_cycles(111);
        assert_eq!(bus.timer_counter(0), Some(0xFFFE));
        assert_eq!(bus.timer_cycles_until_overflow(0), Some(128));

        bus.write16(0x0400_0106, 0x0084);
        assert_eq!(bus.timer_cycles_until_overflow(1), None);
    }

    #[test]
    fn timers_apply_prescalers_and_mask_reserved_control_bits() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0100, 0);
        bus.write16(0x0400_0102, 0xFFFF); // divisor 1024; reserved bits ignored
        assert_eq!(bus.read16(0x0400_0102), 0x00C7);
        bus.advance_cycles(1023);
        assert_eq!(bus.read16(0x0400_0100), 0);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0100), 1);
    }

    #[test]
    fn timer_prescaler_phase_tracks_each_divisor() {
        for (selector, divisor) in [(0_u16, 1_u16), (1, 64), (2, 256), (3, 1024)] {
            let mut bus = MemoryBus::new(cartridge());
            bus.write16(0x0400_0102, 0x0080 | selector);
            assert_eq!(bus.timer_prescaler_divisor(0), Some(divisor));

            if divisor > 1 {
                bus.advance_cycles(u32::from(divisor - 1));
                assert_eq!(bus.timer_prescaler_phase(0), Some(divisor - 1));
                assert_eq!(bus.timer_counter(0), Some(0));
            }

            bus.advance_cycles(1);
            assert_eq!(bus.timer_prescaler_phase(0), Some(0));
            assert_eq!(bus.timer_counter(0), Some(1));
        }
    }

    #[test]
    fn dma_control_inspection_reports_masked_configuration() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.dma_control(4), None);
        assert_eq!(bus.dma_enabled(4), None);
        assert_eq!(bus.dma_start_timing(4), None);

        bus.write16(0x0400_00BA, 0xC400);
        assert_eq!(bus.dma_control(0), Some(0xC400));
        assert_eq!(bus.dma_enabled(0), Some(true));
        assert_eq!(bus.dma_start_timing(0), Some(0));
        assert_eq!(bus.dma_word_transfer(0), Some(true));
        assert_eq!(bus.dma_irq_enabled(0), Some(true));

        bus.write16(0x0400_00BA, 0x6000);
        assert_eq!(bus.dma_enabled(0), Some(false));
        assert_eq!(bus.dma_start_timing(0), Some(2));
        assert_eq!(bus.dma_word_transfer(0), Some(false));
        assert_eq!(bus.dma_irq_enabled(0), Some(true));
    }

    #[test]
    fn video_memory_obeys_byte_and_halfword_write_widths() {
        let mut bus = MemoryBus::new(cartridge());

        // Palette and VRAM byte writes replicate across the aligned halfword.
        bus.write8(0x0500_0001, 0x12);
        assert_eq!(bus.read16(0x0500_0000), 0x1212);
        bus.write16(0x0500_0000, 0x3456);
        assert_eq!(bus.read16(0x0500_0000), 0x3456);

        bus.write8(0x0600_0003, 0xAB);
        assert_eq!(bus.read16(0x0600_0002), 0xABAB);
        bus.write16(0x0600_0002, 0x1234);
        assert_eq!(bus.read16(0x0600_0002), 0x1234);

        // OAM byte stores are ignored, while halfword stores are accepted.
        bus.write16(0x0700_0000, 0xFFFF);
        bus.write8(0x0700_0000, 0x12);
        assert_eq!(bus.read16(0x0700_0000), 0xFFFF);
        bus.write32(0x0700_0000, 0x1234_5678);
        assert_eq!(bus.read32(0x0700_0000), 0x1234_5678);
    }

    #[test]
    fn video_timing_updates_hblank_vblank_and_vcount_irq_flags() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0004, (1 << 3) | (1 << 4) | (160 << 8));

        bus.advance_cycles(959);
        assert_eq!(bus.read16(0x0400_0004) & 0x0003, 0);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0400_0004) & 0x0002, 0x0002);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 1), 1 << 1);

        bus.advance_cycles(272); // finish line 0
        assert_eq!(bus.read16(0x0400_0006), 1);
        assert_eq!(bus.read16(0x0400_0004) & 0x0002, 0);

        bus.advance_cycles(1232 * 159);
        assert_eq!(bus.read16(0x0400_0006), 160);
        assert_eq!(bus.read16(0x0400_0004) & 0x0001, 1);
        assert_eq!(bus.read16(0x0400_0202) & 1, 1);
        assert_eq!(bus.read16(0x0400_0004) & 0x0004, 0x0004);
    }

    #[test]
    fn vcount_register_is_read_only_and_dispstat_status_bits_are_read_only() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0006, 123);
        bus.write16(0x0400_0004, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0006), 0);
        assert_eq!(bus.read16(0x0400_0004) & 0x0007, 0x0000);
        assert_eq!(bus.read16(0x0400_0004) & 0xFF38, 0xFF38);
    }

    #[test]
    fn vcount_compare_irq_fires_on_scanline_transition() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0004, (1 << 5) | (2 << 8));
        bus.advance_cycles(1232);
        assert_eq!(bus.read16(0x0400_0006), 1);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 2), 0);
        bus.advance_cycles(1232);
        assert_eq!(bus.read16(0x0400_0006), 2);
        assert_eq!(bus.read16(0x0400_0004) & (1 << 2), 1 << 2);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 2), 1 << 2);
    }

    #[test]
    fn immediate_dma_copies_halfwords_and_raises_optional_irq() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0x1234);
        bus.write16(0x0200_0002, 0xABCD);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 2);
        bus.write16(0x0400_00BA, 0xC000);
        assert_eq!(bus.read16(0x0300_0000), 0x1234);
        assert_eq!(bus.read16(0x0300_0002), 0xABCD);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 8), 1 << 8);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0);
    }

    #[test]
    fn immediate_dma_supports_word_transfers_and_fixed_destination() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write32(0x0200_0000, 0x1234_5678);
        bus.write32(0x0200_0004, 0xAABB_CCDD);
        bus.write32(0x0400_00BC, 0x0200_0000);
        bus.write32(0x0400_00C0, 0x0300_0000);
        bus.write16(0x0400_00C4, 2);
        bus.write16(0x0400_00C6, 0x8440);
        assert_eq!(bus.read32(0x0300_0000), 0xAABB_CCDD);
        assert_eq!(bus.read16(0x0400_00C6) & 0x8000, 0);
    }

    #[test]
    fn dma_enable_bit_written_as_byte_starts_immediate_transfer() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0x5AA5);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 1);
        bus.write8(0x0400_00BA, 0x00);
        bus.write8(0x0400_00BB, 0x80);

        assert_eq!(bus.read16(0x0300_0000), 0x5AA5);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0);
    }

    #[test]
    fn dma_decrement_modes_walk_source_and_destination_backwards() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0x1111);
        bus.write16(0x0200_0002, 0x2222);
        bus.write16(0x0200_0004, 0x3333);
        bus.write32(0x0400_00B0, 0x0200_0004);
        bus.write32(0x0400_00B4, 0x0300_0004);
        bus.write16(0x0400_00B8, 3);
        bus.write16(0x0400_00BA, 0x80A0); // decrement source and destination

        assert_eq!(bus.read16(0x0300_0004), 0x3333);
        assert_eq!(bus.read16(0x0300_0002), 0x2222);
        assert_eq!(bus.read16(0x0300_0000), 0x1111);
        assert_eq!(bus.read32(0x0400_00B0), 0x01FF_FFFE);
        assert_eq!(bus.read32(0x0400_00B4), 0x02FF_FFFE);
    }

    #[test]
    fn dma_count_is_limited_by_channel_register_width() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);
        bus.write16(0x0200_0002, 0xBABE);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 0x4001); // DMA0 count is only 14 bits
        bus.write16(0x0400_00BA, 0x8000);

        // 0x4001 is masked to 0x0001 for DMA0, so only one halfword moves.
        assert_eq!(bus.read16(0x0300_0000), 0xCAFE);
        assert_eq!(bus.read16(0x0300_0002), 0);
        assert_eq!(bus.read16(0x0300_7FFE), 0);
    }

    #[test]
    fn immediate_dma_increment_reload_mode_keeps_advanced_destination() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write32(0x0200_0000, 0x1111_2222);
        bus.write32(0x0200_0004, 0x3333_4444);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 2);
        bus.write16(0x0400_00BA, 0x8460); // immediate, 32-bit, increment/reload destination

        assert_eq!(bus.read32(0x0300_0000), 0x1111_2222);
        assert_eq!(bus.read32(0x0300_0004), 0x3333_4444);
        assert_eq!(bus.read32(0x0400_00B4), 0x0300_0008);
    }

    #[test]
    fn dma_control_low_byte_writes_apply_reserved_bit_mask() {
        let mut bus = MemoryBus::new(cartridge());
        // Only control bits 5–7 are writable in the low byte; bits 0–4
        // are reserved and must be masked even for STRB writes.
        bus.write8(0x0400_00BA, 0xFF);
        assert_eq!(bus.read16(0x0400_00BA) & 0x00FF, 0x00E0);

        // The low-byte path must preserve the high byte, including enable.
        bus.write8(0x0400_00BB, 0x90); // enable on VBlank, not immediate
        bus.write8(0x0400_00BA, 0x00);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0x8000);
    }

    #[test]
    fn dma3_preserves_game_pak_drq_control_bit_but_dma0_masks_it() {
        let mut bus = MemoryBus::new(cartridge());

        bus.write16(0x0400_00BA, 0x9800); // DMA0: reserved bit 11, VBlank, enabled
        assert_eq!(bus.read16(0x0400_00BA) & 0x0800, 0);

        bus.write16(0x0400_00DE, 0x9800); // DMA3: Game Pak DRQ, VBlank, enabled
        assert_eq!(bus.read16(0x0400_00DE) & 0x0800, 0x0800);
    }

    #[test]
    fn dma_control_high_byte_masks_drq_by_channel() {
        let mut bus = MemoryBus::new(cartridge());
        // A high-byte STRB write must preserve DMA3's DRQ selector but mask
        // the reserved bit on DMA0. VBlank timing avoids an immediate transfer.
        bus.write8(0x0400_00BB, 0x98);
        bus.write8(0x0400_00DF, 0x98);
        assert_eq!(bus.read16(0x0400_00BA), 0x9000);
        assert_eq!(bus.read16(0x0400_00DE), 0x9800);
    }

    #[test]
    fn dma3_game_pak_drq_waits_for_external_request_not_vblank() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);
        bus.write32(0x0400_00D4, 0x0200_0000);
        bus.write32(0x0400_00D8, 0x0300_0000);
        bus.write16(0x0400_00DC, 1);
        bus.write16(0x0400_00DE, 0x9800); // DMA3 DRQ + VBlank selector + enable

        bus.advance_cycles(1232 * 160);

        assert_eq!(bus.read16(0x0300_0000), 0);
        assert_eq!(bus.read16(0x0400_00DE) & 0x8000, 0x8000);
    }

    #[test]
    fn dma3_game_pak_drq_does_not_start_as_immediate_dma() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);
        bus.write32(0x0400_00D4, 0x0200_0000);
        bus.write32(0x0400_00D8, 0x0300_0000);
        bus.write16(0x0400_00DC, 1);
        // DRQ is an external request source; selector 0 must not override it.
        bus.write16(0x0400_00DE, 0x8800);
        assert_eq!(bus.read16(0x0300_0000), 0);
        assert_eq!(bus.read16(0x0400_00DE) & 0x8000, 0x8000);
    }

    #[test]
    fn timed_dma_stays_enabled_until_its_trigger_exists() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);
        bus.write16(0x0200_0002, 0xFACE);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 1);
        bus.write16(0x0400_00BA, 0x9000);
        assert_eq!(bus.read16(0x0300_0000), 0);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0x8000);
    }
    #[test]
    fn hblank_dma_does_not_trigger_during_vblank_scanlines() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);

        // Reach VBlank before enabling HBlank DMA so earlier visible-line
        // HBlank requests cannot consume the one-word transfer.
        bus.advance_cycles(1232 * 160);
        assert_eq!(bus.read16(0x0400_0006), 160);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 1);
        bus.write16(0x0400_00BA, 0xA000); // enable HBlank DMA
        bus.advance_cycles(960);

        assert_eq!(bus.read16(0x0300_0000), 0);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0x8000);
        // DISPSTAT still reports HBlank during VBlank.
        assert_eq!(bus.read16(0x0400_0004) & 2, 2);
    }

    #[test]
    fn vblank_dma_transfers_only_when_vblank_begins_and_repeats_when_configured() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xCAFE);
        bus.write16(0x0200_0002, 0xFACE);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 1);
        bus.write16(0x0400_00BA, 0x9260); // enable, repeat, VBlank, destination reload

        assert_eq!(bus.read16(0x0300_0000), 0);
        bus.advance_cycles(1232 * 159);
        assert_eq!(bus.read16(0x0300_0000), 0);
        bus.advance_cycles(1232);
        assert_eq!(bus.read16(0x0300_0000), 0xCAFE);
        assert_eq!(bus.read32(0x0400_00B4), 0x0300_0000);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0x8000);
        assert_eq!(bus.read16(0x0400_0202) & (1 << 8), 0);

        // A repeated VBlank transfer reuses the count and continues the source
        // address on the following frame.
        bus.advance_cycles(1232 * 228);
        assert_eq!(bus.read16(0x0300_0000), 0xFACE);
        assert_eq!(bus.read16(0x0300_0002), 0);
        assert_eq!(bus.read32(0x0400_00B4), 0x0300_0000);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0x8000);
    }

    #[test]
    fn hblank_dma_fires_at_the_hblank_boundary() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0200_0000, 0xBEEF);
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0300_0000);
        bus.write16(0x0400_00B8, 1);
        bus.write16(0x0400_00BA, 0xA000); // enable, HBlank timing

        bus.advance_cycles(959);
        assert_eq!(bus.read16(0x0300_0000), 0);
        bus.advance_cycles(1);
        assert_eq!(bus.read16(0x0300_0000), 0xBEEF);
        assert_eq!(bus.read16(0x0400_00BA) & 0x8000, 0);
    }

    #[test]
    fn sram_save_data_round_trips_and_rejects_wrong_sizes_atomically() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(bus.sram_data().len(), SRAM_SIZE);
        assert!(bus.sram_data().iter().all(|byte| *byte == 0xFF));

        let mut save = vec![0xFF; SRAM_SIZE];
        save[0x1234] = 0x5A;
        save[0x7FFF] = 0xA5;
        bus.load_sram_data(&save).unwrap();
        assert_eq!(bus.read8(0x0E00_1234), 0x5A);
        assert_eq!(bus.read8(0x0E00_FFFF), 0xA5);
        assert_eq!(bus.sram_data(), save.as_slice());

        let before = bus.sram_data().to_vec();
        let error = bus.load_sram_data(&save[..save.len() - 1]).unwrap_err();
        assert_eq!(error.expected, SRAM_SIZE);
        assert_eq!(error.actual, SRAM_SIZE - 1);
        assert_eq!(bus.sram_data(), before.as_slice());

        let error = bus.load_sram_data(&[0; SRAM_SIZE + 1]).unwrap_err();
        assert_eq!(error.actual, SRAM_SIZE + 1);
        assert_eq!(bus.sram_data(), before.as_slice());
    }

    #[test]
    fn sram_wide_accesses_follow_the_eight_bit_data_bus() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write8(0x0E00_0000, 0x12);
        bus.write8(0x0E00_0001, 0x34);

        assert_eq!(bus.read16(0x0E00_0000), 0x1212);
        assert_eq!(bus.read32(0x0E00_0000), 0x1212_1212);

        bus.write16(0x0E00_0000, 0xABCD);
        assert_eq!(bus.read8(0x0E00_0000), 0xCD);
        assert_eq!(bus.read8(0x0E00_0001), 0x34);
        bus.write16(0x0E00_0001, 0xABCD);
        assert_eq!(bus.read8(0x0E00_0001), 0xAB);

        bus.write32(0x0E00_0000, 0x7654_3210);
        assert_eq!(bus.read8(0x0E00_0000), 0x10);
        assert_eq!(bus.read8(0x0E00_0001), 0xAB);
        bus.write32(0x0E00_0003, 0x7654_3210);
        assert_eq!(bus.read8(0x0E00_0003), 0x76);
    }

    #[test]
    fn sound_master_control_masks_channel_status_and_resets_fifos_when_disabled() {
        let mut bus = MemoryBus::new(cartridge());

        // Channel status is read-only, while bit 7 is the master enable.
        bus.write16(0x0400_0084, 0x008F);
        assert_eq!(bus.read16(0x0400_0084), 0x0080);
        bus.write32(0x0400_00A0, 0x1234_5678);
        assert_eq!(bus.audio_fifo_len(0), Some(4));

        bus.write8(0x0400_0085, 0xFF);
        assert_eq!(bus.read16(0x0400_0084), 0x0080);
        bus.write16(0x0400_0084, 0);
        assert_eq!(bus.read16(0x0400_0084), 0);
        assert_eq!(bus.audio_fifo_len(0), Some(0));
        assert_eq!(bus.audio_fifo_len(1), Some(0));
        assert_eq!(bus.read16(0x0400_0082), 0);

        // The disabled master cannot be re-enabled by writing reserved status bits.
        bus.write8(0x0400_0085, 0xFF);
        assert_eq!(bus.read16(0x0400_0084), 0);
    }

    #[test]
    fn sound_control_high_byte_writes_trigger_fifo_reset_commands() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write32(0x0400_00A0, 0x1122_3344);
        bus.write32(0x0400_00A4, 0x5566_7788);
        assert_eq!(bus.audio_fifo_len(0), Some(4));
        assert_eq!(bus.audio_fifo_len(1), Some(4));

        // Bit 3 of the high byte is SOUNDCNT_H bit 11 (FIFO A reset).
        bus.write8(0x0400_0083, 0x08);
        assert_eq!(bus.audio_fifo_len(0), Some(0));
        assert_eq!(bus.audio_fifo_len(1), Some(4));
        assert_eq!(bus.read16(0x0400_0082) & (1 << 11), 0);

        // Bit 7 of the high byte is FIFO B reset (bit 15).
        bus.write8(0x0400_0083, 0x80);
        assert_eq!(bus.audio_fifo_len(1), Some(0));
        assert_eq!(bus.read16(0x0400_0082) & (1 << 15), 0);
    }

    #[test]
    fn unaligned_sound_control_halfword_write_uses_aligned_register() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write32(0x0400_00A0, 0xAABB_CCDD);
        assert_eq!(bus.audio_fifo_len(0), Some(4));

        // Halfword MMIO stores align down; this still addresses SOUNDCNT_H.
        bus.write16(0x0400_0083, 1 << 11);
        assert_eq!(bus.audio_fifo_len(0), Some(0));
        assert_eq!(bus.read16(0x0400_0082) & (1 << 11), 0);
    }

    #[test]
    fn sound_master_control_ignores_reserved_high_byte_writes() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write8(0x0400_0085, 0x80);
        assert_eq!(bus.read16(0x0400_0084), 0);
        bus.write8(0x0400_0084, 0x80);
        assert_eq!(bus.read16(0x0400_0084), 0x0080);
        bus.write8(0x0400_0085, 0);
        assert_eq!(bus.read16(0x0400_0084), 0x0080);
    }

    #[test]
    fn direct_sound_stereo_mix_obeys_master_volume_and_routing() {
        let mut bus = MemoryBus::new(cartridge());
        bus.audio_last_samples = [64, -32];

        // Disabled master sound must mute the external mixer output.
        bus.write16(0x0400_0082, (1 << 2) | (1 << 8) | (1 << 9));
        assert_eq!(bus.direct_sound_stereo_sample(), (0, 0));

        bus.write16(0x0400_0084, 0x0080);
        // FIFO A: full volume, routed left and right. FIFO B: half volume,
        // routed left only. Expected left = 128 - 32, right = 128.
        bus.write16(0x0400_0082, (1 << 2) | (1 << 8) | (1 << 9) | (1 << 13));
        assert_eq!(bus.direct_sound_stereo_sample(), (96, 128));
    }

    #[test]
    fn sound_fifo_waits_for_master_enable_and_selected_timer() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write32(0x0400_00A0, 0x4433_2211);
        bus.write16(0x0400_0082, 1 << 8); // FIFO A uses timer 0 (select bit clear).

        // Timer 1 overflow must not clock a FIFO assigned to timer 0.
        bus.write16(0x0400_0104, 0xFFFF);
        bus.write16(0x0400_0106, 0x0080);
        bus.advance_cycles(1);
        assert_eq!(bus.audio_fifo_len(0), Some(4));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0));

        // Timer 0 is selected, but FIFO playback is still gated by SOUNDCNT_X.
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080);
        bus.advance_cycles(1);
        assert_eq!(bus.audio_fifo_len(0), Some(4));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0));

        // Enabling the sound master allows the selected timer to consume one byte.
        bus.write16(0x0400_0084, 0x0080);
        bus.advance_cycles(1);
        assert_eq!(bus.audio_fifo_len(0), Some(3));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0x11));
    }

    #[test]
    fn sound_fifo_consumes_samples_even_when_output_is_not_routed() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080); // Sound master enabled.
        bus.write32(0x0400_00A0, 0x4433_2211);
        // No SOUNDCNT_H speaker-routing bits are enabled.
        bus.write16(0x0400_0082, 0);
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080); // Timer 0 overflows every cycle.

        bus.advance_cycles(1);

        assert_eq!(bus.audio_fifo_len(0), Some(3));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0x11));
        assert_eq!(bus.direct_sound_stereo_sample(), (0, 0));
    }

    #[test]
    fn sound_fifo_underflow_holds_the_last_output_sample() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080); // Sound master enabled.
        bus.write16(0x0400_0082, 1 << 8); // FIFO A uses timer 0.
                                          // FIFO bytes are little-endian; make the last queued sample nonzero.
        bus.write32(0x0400_00A0, 0x7F00_0000);
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080); // Timer 0 enabled, divisor 1.

        bus.advance_cycles(4);
        assert_eq!(bus.audio_fifo_len(0), Some(0));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(127));

        // Clock an empty FIFO: the DAC latch must keep the most recent sample.
        bus.advance_cycles(1);
        assert_eq!(bus.audio_fifo_last_sample(0), Some(127));
    }

    #[test]
    fn sound_fifo_status_reports_capacity_samples_and_refill_threshold() {
        let mut bus = MemoryBus::new(cartridge());
        let empty = bus.audio_fifo_status(0).unwrap();
        assert_eq!(empty.queued_bytes, 0);
        assert_eq!(empty.capacity, 32);
        assert_eq!(empty.last_sample, 0);
        assert!(empty.refill_threshold_reached);
        assert_eq!(bus.audio_fifo_status(2), None);

        for word in 0..4_u32 {
            bus.write32(0x0400_00A0, 0x8081_8283 + word * 0x0404_0404);
        }
        let half_full = bus.audio_fifo_status(0).unwrap();
        assert_eq!(half_full.queued_bytes, 16);
        assert!(half_full.refill_threshold_reached);

        bus.write32(0x0400_00A0, 0x8485_8687);
        let above_threshold = bus.audio_fifo_status(0).unwrap();
        assert_eq!(above_threshold.queued_bytes, 20);
        assert!(!above_threshold.refill_threshold_reached);
    }

    #[test]
    fn sound_fifo_accepts_word_writes_caps_at_32_bytes_and_resets() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write8(0x0400_00A0, 0xAA);
        bus.write16(0x0400_00A0, 0xBBBB);
        assert_eq!(bus.audio_fifo_len(0), Some(0));

        for word in 0..10_u32 {
            bus.write32(0x0400_00A0, 0x0302_0100 + word * 0x0404_0404);
        }
        assert_eq!(bus.audio_fifo_len(0), Some(32));
        bus.write16(0x0400_0082, (1 << 8) | (1 << 11));
        assert_eq!(bus.audio_fifo_len(0), Some(0));
        assert_eq!(bus.read16(0x0400_0082) & (1 << 11), 0);
    }

    #[test]
    fn gamepak_waitstate_cycles_decode_all_regions_and_access_types() {
        let mut bus = MemoryBus::new(cartridge());
        // Reset WAITCNT: all non-sequential accesses use four cycles;
        // sequential access durations are 2/4/8 cycles by ROM region.
        assert_eq!(bus.gamepak_access_cycles(0x0800_0000, false), Some(4));
        assert_eq!(bus.gamepak_access_cycles(0x0A00_0000, false), Some(4));
        assert_eq!(bus.gamepak_access_cycles(0x0C00_0000, false), Some(4));
        assert_eq!(bus.gamepak_access_cycles(0x0800_0000, true), Some(2));
        assert_eq!(bus.gamepak_access_cycles(0x0A00_0000, true), Some(4));
        assert_eq!(bus.gamepak_access_cycles(0x0C00_0000, true), Some(8));
        assert_eq!(bus.gamepak_access_cycles(0x0E00_0000, true), Some(4));
        assert_eq!(bus.gamepak_access_cycles(0x0200_0000, false), None);

        // Exercise the slowest non-sequential ROM and fastest sequential timings;
        // SRAM independently selects its encoded wait duration.
        bus.write16(0x0400_0204, 0x7FFF);
        assert_eq!(bus.gamepak_wait_control(), 0x7FFF);
        assert_eq!(bus.gamepak_access_cycles(0x0800_0000, false), Some(8));
        assert_eq!(bus.gamepak_access_cycles(0x0800_0000, true), Some(1));
        assert_eq!(bus.gamepak_access_cycles(0x0A00_0000, true), Some(1));
        assert_eq!(bus.gamepak_access_cycles(0x0C00_0000, true), Some(1));
        assert_eq!(bus.gamepak_access_cycles(0x0E00_0000, false), Some(8));

        // All four SRAM wait-state encodings map to 4, 3, 2, and 8 cycles.
        for (encoding, expected) in [(0_u16, 4_u32), (1, 3), (2, 2), (3, 8)] {
            bus.write16(0x0400_0204, 0x7FFC | encoding);
            assert_eq!(
                bus.gamepak_access_cycles(0x0E00_0000, false),
                Some(expected),
                "SRAM wait-state encoding {encoding:02b}"
            );
        }
    }

    #[test]
    fn gamepak_sequence_cycles_accumulates_sequential_accesses_safely() {
        let mut bus = MemoryBus::new(cartridge());
        assert_eq!(
            bus.gamepak_sequence_cycles(0x0800_0000, false, 2, 0),
            Some(0)
        );
        assert_eq!(
            bus.gamepak_sequence_cycles(0x0800_0000, false, 2, 1),
            Some(4)
        );
        assert_eq!(
            bus.gamepak_sequence_cycles(0x0800_0000, false, 2, 3),
            Some(8)
        );
        assert_eq!(bus.gamepak_sequence_cycles(0x0200_0000, false, 2, 1), None);
        assert_eq!(bus.gamepak_sequence_cycles(0x0800_0000, false, 3, 2), None);
        assert_eq!(bus.gamepak_sequence_cycles(0xFFFF_FFFE, false, 4, 2), None);
        bus.write16(0x0400_0204, 1 << 4);
        assert_eq!(
            bus.gamepak_sequence_cycles(0x0800_0000, false, 2, 3),
            Some(6)
        );
    }

    #[test]
    #[test]
    fn gamepak_width_timing_models_rom_halfwords_and_sram_bytes() {
        let mut bus = MemoryBus::new(cartridge());

        // A ROM word is two 16-bit accesses: non-sequential first, sequential second.
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0800_0000, false, 1),
            Some(4)
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0800_0000, false, 2),
            Some(4)
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(6)
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0A00_0000, false, 4),
            Some(8)
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0C00_0000, false, 4),
            Some(12)
        );

        // SRAM is byte-wide, so each byte incurs the configured wait duration.
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0E00_0000, false, 4),
            Some(16)
        );
        bus.write16(0x0400_0204, 0x7FFF);
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0800_0000, false, 4),
            Some(9)
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0E00_0000, false, 4),
            Some(32)
        );

        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0800_0000, false, 3),
            None
        );
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x0200_0000, false, 4),
            None
        );
        // A word straddling two independently configured ROM wait-state regions
        // is intentionally not represented as one homogeneous transfer.
        assert_eq!(
            bus.gamepak_access_cycles_for_width(0x09FF_FFFE, false, 4),
            None
        );

        // Verify every non-sequential encoding and both sequential choices for each ROM bank.
        for (bank, nonseq_shift, seq_bit, expected_seq_slow) in [
            (0x0800_0000, 2, 4, 2),
            (0x0A00_0000, 5, 7, 4),
            (0x0C00_0000, 8, 10, 8),
        ] {
            for (encoding, expected) in [(0_u16, 4_u32), (1, 3), (2, 2), (3, 8)] {
                bus.write16(0x0400_0204, encoding << nonseq_shift);
                assert_eq!(
                    bus.gamepak_access_cycles(bank, false),
                    Some(expected),
                    "bank {bank:08X}, non-sequential encoding {encoding}"
                );
            }
            bus.write16(0x0400_0204, 0);
            assert_eq!(
                bus.gamepak_access_cycles(bank, true),
                Some(expected_seq_slow)
            );
            bus.write16(0x0400_0204, 1 << seq_bit);
            assert_eq!(bus.gamepak_access_cycles(bank, true), Some(1));
        }
    }

    #[test]
    fn sound_timer_overflow_requests_fifo_dma_in_four_word_burst() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write16(0x0400_0082, 1 << 8); // FIFO A uses timer 0.
        for i in 0..4_u32 {
            bus.write32(0x0200_0000 + i * 4, 0x1111_1111 * (i + 1));
            bus.write32(0x0400_00A0, 0x8081_8283 + i * 0x0404_0404);
        }
        assert_eq!(bus.audio_fifo_len(0), Some(16));

        // DMA1: EWRAM -> FIFO A, fixed destination, 32-bit, repeat, FIFO timing.
        bus.write32(0x0400_00BC, 0x0200_0000);
        bus.write32(0x0400_00C0, 0x0400_00A0);
        bus.write16(0x0400_00C4, 1);
        bus.write16(0x0400_00C6, 0xB640);
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080);
        bus.advance_cycles(1);

        assert_eq!(bus.audio_fifo_len(0), Some(31));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0x83_u8 as i8));
        assert_eq!(bus.read32(0x0400_00BC), 0x0200_0010);
        assert_eq!(bus.read16(0x0400_00C6) & 0x8000, 0x8000);
    }

    #[test]
    fn sound_fifo_dma_requests_the_matching_channel_interrupt() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write16(0x0400_0082, 1 << 8);
        for i in 0..4_u32 {
            bus.write32(0x0200_0200 + i * 4, 0x1111_1111 * (i + 1));
            bus.write32(0x0400_00A0, 0x0101_0101 * (i + 1));
        }
        bus.write32(0x0400_00BC, 0x0200_0200);
        bus.write32(0x0400_00C0, 0x0400_00A0);
        bus.write16(0x0400_00C4, 1);
        bus.write16(0x0400_00C6, 0xF640); // DMA1 FIFO mode with IRQ enabled.
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080);
        bus.advance_cycles(1);

        assert_eq!(bus.read16(0x0400_0202) & (1 << 9), 1 << 9);
        assert_eq!(bus.read32(0x0400_00BC), 0x0200_0210);
    }

    #[test]
    fn sound_fifo_dma_matches_word_aligned_destination_address() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write16(0x0400_0082, 1 << 8); // FIFO A, timer 0.
        bus.write32(0x0200_0000, 0x4433_2211);
        bus.write32(0x0200_0004, 0x8877_6655);
        bus.write32(0x0200_0008, 0xCCBB_AA99);
        bus.write32(0x0200_000C, 0x00FF_EEDD);
        bus.write32(0x0400_00BC, 0x0200_0000); // DMA1 source.
        bus.write32(0x0400_00C0, 0x0400_00A3); // Low bits are ignored by the bus.
        bus.write16(0x0400_00C4, 1);
        bus.write16(0x0400_00C6, 0xB640); // Enable, repeat, word, FIFO timing.

        // Overflow timer 0 immediately to issue a Direct Sound FIFO request.
        bus.write16(0x0400_0100, 0xFFFF); // Timer 0 reload.
        bus.write16(0x0400_0102, 0x0080); // Enable timer 0.
        bus.advance_cycles(1);

        assert_eq!(bus.audio_fifo_len(0), Some(16));
        assert_eq!(bus.read32(0x0400_00BC), 0x0200_0010);
    }

    #[test]
    fn sound_fifo_dma_rejects_incompatible_channel_configuration() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write16(0x0400_0082, 1 << 8);
        bus.write32(0x0200_0000, 0x4433_2211);
        bus.write32(0x0400_00A0, 0x8877_6655);

        // DMA1 has the FIFO destination and special timing, but no word/repeat
        // configuration. It must not perform a forced four-word FIFO burst.
        bus.write32(0x0400_00BC, 0x0200_0000);
        bus.write32(0x0400_00C0, 0x0400_00A0);
        bus.write16(0x0400_00C4, 1);
        bus.write16(0x0400_00C6, 0xB000);
        bus.write16(0x0400_0100, 0xFFFF);
        bus.write16(0x0400_0102, 0x0080);
        bus.advance_cycles(1);

        assert_eq!(bus.audio_fifo_len(0), Some(3));
        assert_eq!(bus.audio_fifo_last_sample(0), Some(0x55));
        assert_eq!(bus.read32(0x0400_00BC), 0x0200_0000);
        assert_eq!(bus.read16(0x0400_00C6) & 0x8000, 0x8000);
    }

    #[test]
    fn sound_fifo_b_uses_timer_one_and_dma_channel_two() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write16(0x0400_0084, 0x0080);
        bus.write16(0x0400_0082, (1 << 12) | (1 << 14)); // FIFO B, timer 1.
        for i in 0..4_u32 {
            bus.write32(0x0200_0100 + i * 4, 0xA3A2_A1A0 + i * 0x0404_0404);
            bus.write32(0x0400_00A4, 0xA3A2_A1A0 + i * 0x0404_0404);
        }

        // DMA2: EWRAM -> FIFO B, fixed destination, word-sized FIFO burst.
        bus.write32(0x0400_00C8, 0x0200_0100);
        bus.write32(0x0400_00CC, 0x0400_00A4);
        bus.write16(0x0400_00D0, 1);
        bus.write16(0x0400_00D2, 0xB640);
        bus.write16(0x0400_0104, 0xFFFF);
        bus.write16(0x0400_0106, 0x0080);
        bus.advance_cycles(1);

        assert_eq!(bus.audio_fifo_len(1), Some(31));
        assert_eq!(bus.audio_fifo_last_sample(1), Some(0xA0_u8 as i8));
        assert_eq!(bus.read32(0x0400_00C8), 0x0200_0110);
        assert_eq!(bus.read16(0x0400_00D2) & 0x8000, 0x8000);
    }

    #[test]
    fn vram_mirror_and_sram_address_window_are_consistent() {
        let mut bus = MemoryBus::new(cartridge());
        bus.write8(0x0601_8000, 0x5A);
        assert_eq!(bus.read8(0x0601_0000), 0x5A);

        // Only 0x0E00_0000..=0x0E00_FFFF is mapped to save SRAM.
        bus.write8(0x0E00_1234, 0xA5);
        assert_eq!(bus.read8(0x0E00_1234), 0xA5);
        assert_eq!(bus.read8(0x0E01_1234), 0);
        assert_eq!(bus.read8(0x0F00_1234), 0);

        // Writes outside the SRAM window must not alias into the save.
        bus.write8(0x0F00_1234, 0x3C);
        assert_eq!(bus.read8(0x0E00_1234), 0xA5);
        assert_eq!(bus.read8(0x0F00_1234), 0);
    }

    fn cartridge_with_save_signature(signature: &[u8]) -> GbaCartridge {
        let mut rom = vec![0; 0xC0 + signature.len()];
        rom[0xC0..].copy_from_slice(signature);
        let checksum = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        rom[0xBD] = checksum;
        GbaCartridge::parse(&rom).expect("test ROM header should validate")
    }

    #[test]
    fn eeprom_bus_clocks_dma_halfword_bits_and_persists_images() {
        let mut bus = MemoryBus::new(cartridge_with_save_signature(b"EEPROM_V"));
        assert_eq!(bus.cartridge_save_type(), Some(crate::SaveType::Eeprom));
        assert_eq!(bus.eeprom_data().map(<[u8]>::len), Some(512));

        let address = 63_usize;
        let value = 0x1234_5678_9ABC_DEF0_u64;
        let mut write_bits = vec![1_u16, 1, 0];
        write_bits.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u16));
        write_bits.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u16));
        write_bits.push(0);
        for bit in write_bits {
            bus.write16(0x0D00_0000, bit);
        }
        assert_eq!(
            &bus.eeprom_data().unwrap()[address * 8..address * 8 + 8],
            &value.to_be_bytes()
        );

        let mut read_bits = vec![1_u16, 1, 1];
        read_bits.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u16));
        read_bits.push(0);
        for bit in read_bits {
            bus.write16(0x0D00_0000, bit);
        }
        for _ in 0..4 {
            assert_eq!(bus.read16(0x0D00_0000), 0);
        }
        let mut result = 0_u64;
        for _ in 0..64 {
            result = (result << 1) | u64::from(bus.read16(0x0D00_0000) & 1);
        }
        assert_eq!(result, value);

        let image = vec![0x5A; 8 * 1024];
        assert_eq!(bus.load_eeprom_data(&image), Ok(true));
        assert_eq!(bus.eeprom_data().map(<[u8]>::len), Some(8 * 1024));
        assert_eq!(
            bus.load_eeprom_data(&[0; 7]).unwrap_err().expected,
            8 * 1024
        );
        assert_eq!(bus.eeprom_data().unwrap()[0], 0x5A);
    }

    #[test]
    fn dma_selects_large_eeprom_protocol_and_writes_serial_bits() {
        let mut bus = MemoryBus::new(cartridge_with_save_signature(b"EEPROM_V"));
        let address = 0x03FF_usize;
        let value = 0x89AB_CDEF_0123_4567_u64;
        let mut bits = vec![1_u16, 1, 0];
        bits.extend((0..14).rev().map(|bit| ((address >> bit) & 1) as u16));
        bits.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u16));
        bits.push(0);
        for (index, bit) in bits.into_iter().enumerate() {
            bus.write16(0x0200_0000 + (index as u32) * 2, bit);
        }

        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0D00_0000);
        bus.write16(0x0400_00B8, 82);
        bus.write16(0x0400_00BA, 0x8040); // Enable, fixed EEPROM destination.
        assert_eq!(bus.eeprom_data().map(<[u8]>::len), Some(8 * 1024));
        assert_eq!(
            &bus.eeprom_data().unwrap()[address * 8..address * 8 + 8],
            &value.to_be_bytes()
        );
    }

    #[test]
    fn large_eeprom_dma_write_then_read_round_trips_all_serial_bits() {
        let mut bus = MemoryBus::new(cartridge_with_save_signature(b"EEPROM_V"));
        let address = 0x03FF_usize;
        let value = 0x89AB_CDEF_0123_4567_u64;

        let mut write_bits = vec![1_u16, 1, 0];
        write_bits.extend((0..14).rev().map(|bit| ((address >> bit) & 1) as u16));
        write_bits.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u16));
        write_bits.push(0);
        for (index, bit) in write_bits.into_iter().enumerate() {
            bus.write16(0x0200_0000 + index as u32 * 2, bit);
        }
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0D00_0000);
        bus.write16(0x0400_00B8, 82);
        bus.write16(0x0400_00BA, 0x8040);

        let mut read_command = vec![1_u16, 1, 1];
        read_command.extend((0..14).rev().map(|bit| ((address >> bit) & 1) as u16));
        read_command.push(0);
        for (index, bit) in read_command.into_iter().enumerate() {
            bus.write16(0x0200_1000 + index as u32 * 2, bit);
        }
        bus.write32(0x0400_00B0, 0x0200_1000);
        bus.write32(0x0400_00B4, 0x0D00_0000);
        bus.write16(0x0400_00B8, 18);
        bus.write16(0x0400_00BA, 0x8040);

        bus.write32(0x0400_00B0, 0x0D00_0000);
        bus.write32(0x0400_00B4, 0x0200_2000);
        bus.write16(0x0400_00B8, 68);
        bus.write16(0x0400_00BA, 0x8000);

        let mut result = 0_u64;
        for index in 4..68 {
            result = (result << 1) | u64::from(bus.read16(0x0200_2000 + index * 2));
        }
        assert_eq!(result, value);
    }

    #[test]
    fn flash_cartridge_bus_executes_program_and_sector_erase_commands() {
        let cart = cartridge_with_save_signature(b"FLASH_V");
        let mut bus = MemoryBus::new(cart);
        assert_eq!(bus.cartridge_save_type(), Some(crate::SaveType::Flash64K));
        assert_eq!(bus.flash_data().map(<[u8]>::len), Some(64 * 1024));

        bus.write8(0x0E00_5555, 0xAA);
        bus.write8(0x0E00_2AAA, 0x55);
        bus.write8(0x0E00_5555, 0xA0);
        bus.write8(0x0E00_1234, 0x35);
        assert_eq!(bus.read8(0x0E00_1234), 0x35);
        assert_eq!(bus.read16(0x0E00_1234), 0x3535);

        bus.write8(0x0E00_5555, 0xAA);
        bus.write8(0x0E00_2AAA, 0x55);
        bus.write8(0x0E00_5555, 0x80);
        bus.write8(0x0E00_5555, 0xAA);
        bus.write8(0x0E00_2AAA, 0x55);
        bus.write8(0x0E00_1234, 0x30);
        assert_eq!(bus.read8(0x0E00_1234), 0xFF);
    }

    #[test]
    fn flash_save_image_round_trips_and_eeprom_is_not_mapped_as_sram() {
        let mut flash_bus = MemoryBus::new(cartridge_with_save_signature(b"FLASH1M_V"));
        let image = vec![0xA5; 128 * 1024];
        assert_eq!(flash_bus.load_flash_data(&image), Ok(true));
        assert_eq!(flash_bus.flash_data().unwrap()[0], 0xA5);
        assert_eq!(
            flash_bus.load_flash_data(&[0; 4]).unwrap_err().expected,
            128 * 1024
        );
        assert_eq!(flash_bus.flash_data().unwrap()[0], 0xA5);

        let mut eeprom_bus = MemoryBus::new(cartridge_with_save_signature(b"EEPROM_V"));
        eeprom_bus.write8(0x0E00_0000, 0x12);
        assert_eq!(eeprom_bus.read8(0x0E00_0000), 0xFF);
        assert_eq!(eeprom_bus.flash_data(), None);
    }
}
