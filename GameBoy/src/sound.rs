//! GBA legacy PSG: two pulse channels, programmable wave playback and noise.

/// Four-channel programmable sound generator for the GBA legacy audio unit.
#[derive(Clone, Debug)]
pub struct Psg {
    registers: [u8; 0x22],
    square: [SquareChannel; 2],
    wave: WaveChannel,
    noise: NoiseChannel,
    wave_ram: [u8; 32],
    master_enabled: bool,
    length_clock: u32,
    sweep_clock: u32,
}

#[derive(Clone, Debug, Default)]
struct SquareChannel {
    enabled: bool,
    dac_enabled: bool,
    duty: u8,
    length: u16,
    length_enabled: bool,
    frequency: u16,
    phase: u8,
    timer: u32,
    initial_volume: u8,
    volume: u8,
    envelope_increase: bool,
    envelope_period: u8,
    envelope_timer: u32,
    sweep_period: u8,
    sweep_decrease: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_shadow: u16,
    sweep_enabled: bool,
    sweep_negate_used: bool,
}

#[derive(Clone, Debug, Default)]
struct WaveChannel {
    enabled: bool,
    dac_enabled: bool,
    volume_code: u8,
    length: u16,
    length_enabled: bool,
    frequency: u16,
    position: u8,
    timer: u32,
    two_bank_mode: bool,
    bank_select: usize,
}

#[derive(Clone, Debug, Default)]
struct NoiseChannel {
    enabled: bool,
    dac_enabled: bool,
    length: u16,
    length_enabled: bool,
    initial_volume: u8,
    volume: u8,
    envelope_increase: bool,
    envelope_period: u8,
    envelope_timer: u32,
    divisor_code: u8,
    clock_shift: u8,
    narrow_mode: bool,
    lfsr: u16,
    timer: u32,
}

impl Default for Psg {
    fn default() -> Self {
        Self {
            registers: [0; 0x22],
            square: std::array::from_fn(|_| SquareChannel::default()),
            wave: WaveChannel::default(),
            noise: NoiseChannel {
                lfsr: 0x7FFF,
                ..NoiseChannel::default()
            },
            wave_ram: [0; 32],
            master_enabled: false,
            length_clock: 0,
            sweep_clock: 0,
        }
    }
}

impl Psg {
    /// Writes a byte in the PSG register window (offset 0x60..=0x81).
    pub fn write(&mut self, offset: usize, value: u8) -> Option<u8> {
        if !(0x60..=0x81).contains(&offset) {
            return None;
        }
        if !self.master_enabled {
            return Some(0);
        }
        let index = offset - 0x60;
        let masked = match offset {
            0x60 => value & 0x7F,
            0x62 | 0x68 => value & 0xC0,
            0x65 | 0x6D | 0x75 | 0x7D => value & 0xC7,
            0x70 => value & 0xE0,
            0x72 => value & 0xE0,
            0x78 => value & 0x3F,
            0x7C => value & 0x4F,
            _ => value,
        };
        self.registers[index] = masked;
        match offset {
            0x60 => {
                let sweep = &mut self.square[0];
                sweep.sweep_period = (value >> 4) & 7;
                sweep.sweep_decrease = value & 8 != 0;
                sweep.sweep_shift = value & 7;
                if !sweep.sweep_decrease && sweep.sweep_negate_used {
                    sweep.enabled = false;
                }
            }
            0x62 | 0x68 => {
                let c = usize::from(offset == 0x68);
                self.square[c].duty = masked >> 6;
                self.square[c].length = 64 - u16::from(value & 0x3F);
            }
            0x63 | 0x69 => {
                let c = usize::from(offset == 0x69);
                let s = &mut self.square[c];
                s.initial_volume = value >> 4;
                s.envelope_increase = value & 8 != 0;
                s.envelope_period = value & 7;
                s.dac_enabled = value & 0xF8 != 0;
                if !s.dac_enabled {
                    s.enabled = false;
                }
                s.volume = s.initial_volume;
            }
            0x64 | 0x6C => {
                let c = usize::from(offset == 0x6C);
                self.square[c].frequency = (self.square[c].frequency & 0x0700) | u16::from(value);
            }
            0x65 | 0x6D => {
                let c = usize::from(offset == 0x6D);
                let s = &mut self.square[c];
                s.frequency = (s.frequency & 0x00FF) | (u16::from(value & 7) << 8);
                s.length_enabled = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    Self::trigger_square(s);
                }
            }
            0x70 => {
                self.wave.dac_enabled = value & 0x80 != 0;
                if !self.wave.dac_enabled {
                    self.wave.enabled = false;
                }
                self.wave.two_bank_mode = value & 0x40 != 0;
                self.wave.bank_select = usize::from(value & 0x20 != 0);
            }
            0x72 => self.wave.volume_code = (value >> 5) & 3,
            0x73 => self.wave.length = 256 - u16::from(value),
            0x74 => self.wave.frequency = (self.wave.frequency & 0x0700) | u16::from(value),
            0x75 => {
                self.wave.frequency = (self.wave.frequency & 0x00FF) | (u16::from(value & 7) << 8);
                self.wave.length_enabled = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    self.wave.enabled = self.wave.dac_enabled;
                    self.wave.position = 0;
                    self.wave.timer = Self::wave_period(self.wave.frequency);
                    if self.wave.length == 0 {
                        self.wave.length = 256;
                    }
                }
            }
            0x78 => self.noise.length = 64 - u16::from(value & 0x3F),
            0x79 => {
                self.noise.initial_volume = value >> 4;
                self.noise.envelope_increase = value & 8 != 0;
                self.noise.envelope_period = value & 7;
                self.noise.dac_enabled = value & 0xF8 != 0;
                if !self.noise.dac_enabled {
                    self.noise.enabled = false;
                }
            }
            0x7C => {
                self.noise.divisor_code = value & 7;
                self.noise.narrow_mode = value & 8 != 0;
                self.noise.clock_shift = value >> 4;
            }
            0x7D => {
                self.noise.length_enabled = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    self.noise.enabled = self.noise.dac_enabled;
                    self.noise.lfsr = 0x7FFF;
                    self.noise.timer =
                        Self::noise_period(self.noise.divisor_code, self.noise.clock_shift);
                    self.noise.volume = self.noise.initial_volume;
                    self.noise.envelope_timer = Self::envelope_period(self.noise.envelope_period);
                    if self.noise.length == 0 {
                        self.noise.length = 64;
                    }
                }
            }
            _ => {}
        }
        Some(masked)
    }

    /// Writes one byte of the 16-byte wave sample RAM.
    pub fn write_wave_ram(&mut self, index: usize, value: u8) -> bool {
        if index >= 16 {
            return false;
        }
        self.wave_ram[self.wave.bank_select * 16 + index] = value;
        true
    }

    /// Reads one byte of wave sample RAM.
    pub fn read_wave_ram(&self, index: usize) -> Option<u8> {
        (index < 16).then(|| self.wave_ram[self.wave.bank_select * 16 + index])
    }

    /// Enables or disables the PSG master circuit.
    pub fn set_master_enabled(&mut self, enabled: bool) {
        if self.master_enabled && !enabled {
            self.registers.fill(0);
            self.square = std::array::from_fn(|_| SquareChannel::default());
            self.wave = WaveChannel::default();
            self.noise = NoiseChannel {
                lfsr: 0x7FFF,
                ..NoiseChannel::default()
            };
            self.length_clock = 0;
            self.sweep_clock = 0;
        }
        self.master_enabled = enabled;
    }

    /// Advances oscillators, envelope clocks and the shared 256 Hz length clock.
    pub fn advance(&mut self, cycles: u32) {
        if !self.master_enabled || cycles == 0 {
            return;
        }
        self.length_clock = self.length_clock.saturating_add(cycles);
        let ticks = self.length_clock / 65536;
        self.length_clock %= 65536;
        self.sweep_clock = self.sweep_clock.saturating_add(cycles);
        while self.sweep_clock >= 131_072 {
            self.sweep_clock -= 131_072;
            Self::clock_sweep(&mut self.square[0]);
        }

        // Length counters are clocked by the shared frame sequencer even
        // while their channels are disabled. Keeping this separate from
        // oscillator advancement matters when software arms length before
        // triggering a channel, or leaves a disabled counter running.
        if ticks != 0 {
            for channel in &mut self.square {
                if channel.length_enabled {
                    channel.length = channel.length.saturating_sub(ticks.min(u32::from(u16::MAX)) as u16);
                    if channel.length == 0 {
                        channel.enabled = false;
                    }
                }
            }
            if self.wave.length_enabled {
                self.wave.length =
                    self.wave.length.saturating_sub(ticks.min(u32::from(u16::MAX)) as u16);
                if self.wave.length == 0 {
                    self.wave.enabled = false;
                }
            }
            if self.noise.length_enabled {
                self.noise.length =
                    self.noise.length.saturating_sub(ticks.min(u32::from(u16::MAX)) as u16);
                if self.noise.length == 0 {
                    self.noise.enabled = false;
                }
            }
        }

        for channel in &mut self.square {
            if !channel.enabled {
                continue;
            }
            let period = Self::square_period(channel.frequency);
            let mut remaining = cycles;
            while remaining >= channel.timer {
                remaining -= channel.timer;
                channel.phase = (channel.phase + 1) & 7;
                channel.timer = period;
            }
            channel.timer -= remaining;
            Self::clock_envelope(
                &mut channel.volume,
                channel.initial_volume,
                channel.envelope_increase,
                channel.envelope_period,
                &mut channel.envelope_timer,
                cycles,
            );
        }
        if self.wave.enabled {
            let mut remaining = cycles;
            while remaining >= self.wave.timer {
                remaining -= self.wave.timer;
                let position_mask = if self.wave.two_bank_mode { 63 } else { 31 };
                self.wave.position = (self.wave.position + 1) & position_mask;
                self.wave.timer = Self::wave_period(self.wave.frequency);
            }
            self.wave.timer -= remaining;
        }
        if self.noise.enabled {
            let mut remaining = cycles;
            while remaining >= self.noise.timer {
                remaining -= self.noise.timer;
                let feedback = (self.noise.lfsr ^ (self.noise.lfsr >> 1)) & 1;
                self.noise.lfsr = (self.noise.lfsr >> 1) | (feedback << 14);
                if self.noise.narrow_mode {
                    self.noise.lfsr = (self.noise.lfsr & !(1 << 6)) | (feedback << 6);
                }
                self.noise.timer =
                    Self::noise_period(self.noise.divisor_code, self.noise.clock_shift);
            }
            self.noise.timer -= remaining;
            Self::clock_envelope(
                &mut self.noise.volume,
                self.noise.initial_volume,
                self.noise.envelope_increase,
                self.noise.envelope_period,
                &mut self.noise.envelope_timer,
                cycles,
            );
        }
    }

    fn clock_envelope(
        volume: &mut u8,
        _initial: u8,
        increase: bool,
        period: u8,
        timer: &mut u32,
        cycles: u32,
    ) {
        if period == 0 {
            return;
        }
        let step = Self::envelope_period(period);
        if *timer == 0 {
            *timer = step;
        }

        // Preserve elapsed cycles across envelope edges. A single large
        // advance must produce the same volume and phase as many small advances.
        let mut remaining = cycles;
        while remaining >= *timer {
            remaining -= *timer;
            if increase {
                if *volume == 15 {
                    *timer = step;
                    return;
                }
                *volume += 1;
            } else {
                if *volume == 0 {
                    *timer = step;
                    return;
                }
                *volume -= 1;
            }
            *timer = step;
        }
        *timer -= remaining;
    }

    fn trigger_square(channel: &mut SquareChannel) {
        channel.enabled = channel.dac_enabled;
        channel.phase = 0;
        channel.timer = Self::square_period(channel.frequency);
        channel.volume = channel.initial_volume;
        channel.envelope_timer = Self::envelope_period(channel.envelope_period);
        if channel.length == 0 {
            channel.length = 64;
        }
        channel.sweep_shadow = channel.frequency;
        channel.sweep_timer = if channel.sweep_period == 0 {
            8
        } else {
            channel.sweep_period
        };
        channel.sweep_enabled = channel.sweep_period != 0 || channel.sweep_shift != 0;
        channel.sweep_negate_used = false;
        if channel.sweep_shift != 0 && Self::sweep_calculate(channel).is_none() {
            channel.enabled = false;
        }
    }

    fn sweep_calculate(channel: &mut SquareChannel) -> Option<u16> {
        let delta = channel.sweep_shadow >> channel.sweep_shift;
        let next = if channel.sweep_decrease {
            channel.sweep_negate_used = true;
            channel.sweep_shadow.checked_sub(delta)?
        } else {
            channel.sweep_shadow.checked_add(delta)?
        };
        (next <= 2047).then_some(next)
    }

    fn clock_sweep(channel: &mut SquareChannel) {
        if !channel.enabled || !channel.sweep_enabled {
            return;
        }
        if channel.sweep_timer > 0 {
            channel.sweep_timer -= 1;
        }
        if channel.sweep_timer != 0 {
            return;
        }
        channel.sweep_timer = if channel.sweep_period == 0 {
            8
        } else {
            channel.sweep_period
        };
        if channel.sweep_period == 0 || channel.sweep_shift == 0 {
            return;
        }
        let Some(next) = Self::sweep_calculate(channel) else {
            channel.enabled = false;
            return;
        };
        channel.sweep_shadow = next;
        channel.frequency = next;
        channel.timer = Self::square_period(next);
        if Self::sweep_calculate(channel).is_none() {
            channel.enabled = false;
        }
    }

    /// Returns raw signed samples for Square 1, Square 2, Wave and Noise.
    pub fn channel_samples(&self) -> [i16; 4] {
        if !self.master_enabled {
            return [0; 4];
        }
        const DUTY: [[bool; 8]; 4] = [
            [false, false, false, false, false, false, false, true],
            [true, false, false, false, false, false, false, true],
            [true, false, false, false, false, true, true, true],
            [false, true, true, true, true, true, true, false],
        ];
        let mut output = [0_i16; 4];
        for (i, channel) in self.square.iter().enumerate() {
            if channel.enabled && channel.volume != 0 {
                let amp = i16::from(channel.volume) * 2;
                output[i] = if DUTY[channel.duty as usize][channel.phase as usize] {
                    amp
                } else {
                    -amp
                };
            }
        }
        if self.wave.enabled && self.wave.volume_code != 0 {
            let sample_index = (self.wave.position / 2) as usize;
            // In 64-sample mode playback begins at the selected bank and
            // then traverses the other bank. In 32-sample mode only the
            // selected bank is addressed.
            let sample_index = if self.wave.two_bank_mode {
                (self.wave.bank_select * 16 + sample_index) & 31
            } else {
                self.wave.bank_select * 16 + sample_index
            };
            let sample_byte = self.wave_ram[sample_index];
            let nibble = if self.wave.position & 1 == 0 {
                sample_byte >> 4
            } else {
                sample_byte & 0x0F
            };
            let shift = match self.wave.volume_code {
                1 => 0,
                2 => 1,
                3 => 2,
                _ => 4,
            };
            output[2] = (i16::from(nibble) - 8) * 2 >> shift;
        }
        if self.noise.enabled && self.noise.volume != 0 {
            let amp = i16::from(self.noise.volume) * 2;
            output[3] = if self.noise.lfsr & 1 == 0 { amp } else { -amp };
        }
        output
    }

    /// Mixes all PSG channels using NR50 master volume and NR51 stereo routing.
    pub fn stereo_sample(&self) -> (i16, i16) {
        let samples = self.channel_samples();
        let routing = self.registers[0x21];
        let volume = self.registers[0x20];
        // NR50 has eight non-zero attenuation steps: code 0 is the
        // minimum (1/8), and code 7 is the maximum (8/8).
        let left_volume = i16::from((volume >> 4) & 7) + 1;
        let right_volume = i16::from(volume & 7) + 1;
        let mut left = 0_i16;
        let mut right = 0_i16;
        for (index, sample) in samples.into_iter().enumerate() {
            if routing & (1 << (index + 4)) != 0 {
                left += sample * left_volume;
            }
            if routing & (1 << index) != 0 {
                right += sample * right_volume;
            }
        }
        (left, right)
    }

    /// Returns channel 1–4 status bits for SOUNDCNT_X.
    pub fn status_bits(&self) -> u8 {
        u8::from(self.square[0].enabled)
            | (u8::from(self.square[1].enabled) << 1)
            | (u8::from(self.wave.enabled) << 2)
            | (u8::from(self.noise.enabled) << 3)
    }

    fn square_period(frequency: u16) -> u32 {
        u32::from(2048_u16.saturating_sub(frequency & 0x07FF)).max(1) * 16
    }
    fn wave_period(frequency: u16) -> u32 {
        u32::from(2048_u16.saturating_sub(frequency & 0x07FF)).max(1) * 8
    }
    fn noise_period(divisor: u8, shift: u8) -> u32 {
        let base = if divisor == 0 {
            8
        } else {
            u32::from(divisor) * 16
        };
        // NR43 exposes a four-bit clock shift (0..=15). The largest
        // hardware period still fits in u32: 7 * 16 * 2^15.
        base.saturating_mul(1_u32 << shift.min(15))
    }
    fn envelope_period(period: u8) -> u32 {
        u32::from(period.max(1)) * 262_144
    }
}

#[cfg(test)]
mod tests {
    use super::Psg;

    #[test]
    fn pulse_channels_require_master_enable_and_trigger() {
        let mut psg = Psg::default();
        psg.write(0x62, 0x80);
        assert_eq!(psg.status_bits(), 0);
        psg.set_master_enabled(true);
        psg.write(0x62, 0xC0);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0x80);
        assert_eq!(psg.status_bits() & 1, 1);
        assert_ne!(psg.channel_samples()[0], 0);
    }

    #[test]
    fn pulse_frequency_advances_duty_phase_and_stereo_routing() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x62, 0xC0);
        psg.write(0x63, 0xF0);
        psg.write(0x64, 0xFF);
        psg.write(0x65, 0x87);
        psg.write(0x80, 0x77);
        psg.write(0x81, 0x10);
        let before = psg.channel_samples()[0];
        psg.advance(16);
        assert_ne!(psg.channel_samples()[0], before);
        let (left, right) = psg.stereo_sample();
        assert_ne!(left, 0);
        assert_eq!(right, 0);
    }

    #[test]
    fn nr50_volume_zero_is_minimum_attenuation_not_mute() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x62, 0x80);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0x80);
        psg.write(0x81, 0x11); // Route square 1 to both outputs.

        let full = psg.channel_samples()[0];
        assert_ne!(full, 0);

        psg.write(0x80, 0x00);
        let (left, right) = psg.stereo_sample();
        assert_eq!(left, full);
        assert_eq!(right, full);

        psg.write(0x80, 0x77);
        let (left, right) = psg.stereo_sample();
        assert_eq!(left, full * 8);
        assert_eq!(right, full * 8);
    }

    #[test]
    fn wave_channel_reads_high_then_low_nibbles_and_obeys_volume() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write_wave_ram(0, 0xF0);
        psg.write(0x70, 0x80);
        psg.write(0x72, 0x20);
        psg.write(0x74, 0xFF);
        psg.write(0x75, 0x87);
        assert_eq!(psg.status_bits() & 4, 4);
        assert_eq!(psg.channel_samples()[2], 14);
        psg.advance(8);
        assert_eq!(psg.channel_samples()[2], -16);
    }

    #[test]
    fn noise_period_honors_full_four_bit_clock_shift_range() {
        assert_eq!(Psg::noise_period(0, 0), 8);
        assert_eq!(Psg::noise_period(1, 0), 16);
        assert_eq!(Psg::noise_period(7, 13), 917_504);
        assert_eq!(Psg::noise_period(7, 15), 3_670_016);
        assert_eq!(Psg::noise_period(7, 0xFF), 3_670_016);
    }

    #[test]
    fn noise_channel_uses_lfsr_and_supports_seven_bit_mode() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x79, 0xF0);
        psg.write(0x7C, 0x08);
        psg.write(0x7D, 0x80);
        let before = psg.channel_samples()[3];
        psg.advance(120);
        assert_ne!(psg.channel_samples()[3], before);
        assert_eq!(psg.status_bits() & 8, 8);
    }

    #[test]
    fn disabling_master_sound_stops_all_channels_and_clears_status() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x79, 0xF0);
        psg.write(0x7D, 0x80);
        assert_eq!(psg.status_bits(), 8);
        psg.set_master_enabled(false);
        assert_eq!(psg.status_bits(), 0);
        assert_eq!(psg.stereo_sample(), (0, 0));
    }

    #[test]
    fn square_one_sweep_increases_frequency_and_disables_on_overflow() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x60, 0x11); // Sweep every 1 unit, shift right by 1.
        psg.write(0x62, 0x80);
        psg.write(0x63, 0xF0);
        psg.write(0x64, 0xE8); // Initial frequency 1000.
        psg.write(0x65, 0x83);
        assert_eq!(psg.square[0].frequency, 1000);
        psg.advance(131_072);
        assert_eq!(psg.square[0].frequency, 1500);
        psg.write(0x64, 0xFF);
        psg.write(0x65, 0x87); // Re-trigger at a high frequency; overflow check.
        assert_eq!(psg.status_bits() & 1, 0);
    }

    #[test]
    fn wave_ram_bank_selection_and_two_bank_playback_are_supported() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x70, 0xA0); // DAC enabled, bank 1 selected.
        psg.write_wave_ram(0, 0xF0);
        assert_eq!(psg.read_wave_ram(0), Some(0xF0));
        psg.write(0x70, 0x80); // Select bank 0.
        psg.write_wave_ram(0, 0x10);
        psg.write(0x70, 0xA0);
        assert_eq!(psg.read_wave_ram(0), Some(0xF0));
        psg.write(0x70, 0xE0); // 64-sample mode traverses both banks.
        psg.write(0x72, 0x20);
        psg.write(0x74, 0xFF);
        psg.write(0x75, 0x87);
        // Two-bank playback starts from the selected bank (bank 1),
        // then continues through bank 0 after 32 samples.
        assert_eq!(psg.channel_samples()[2], 14);
        psg.advance(8 * 32);
        assert_eq!(psg.wave.position, 32);
        assert_eq!(psg.channel_samples()[2], -14);
    }

    #[test]
    fn length_clock_accumulates_across_small_cycle_advances() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x62, 0x3F);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0xC0);
        for _ in 0..256 {
            psg.advance(256);
        }
        assert_eq!(psg.status_bits() & 1, 0);
    }
    #[test]
    fn envelope_clock_is_invariant_to_cycle_chunking() {
        let mut one_shot = Psg::default();
        let mut chunked = Psg::default();
        for psg in [&mut one_shot, &mut chunked] {
            psg.set_master_enabled(true);
            psg.write(0x63, 0xA9); // Start at 10, increase every envelope period.
            psg.write(0x65, 0x80);
        }

        one_shot.advance(262_144 + 123);
        chunked.advance(262_144);
        chunked.advance(123);

        assert_eq!(one_shot.square[0].volume, 11);
        assert_eq!(one_shot.square[0].volume, chunked.square[0].volume);
        assert_eq!(
            one_shot.square[0].envelope_timer,
            chunked.square[0].envelope_timer
        );
    }

    #[test]
    fn envelope_clock_processes_multiple_steps_in_one_advance() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x63, 0x21); // Start at 2, decrease every envelope period.
        psg.write(0x65, 0x80);

        psg.advance(3 * 262_144 + 17);

        assert_eq!(psg.square[0].volume, 0);
        assert_eq!(psg.square[0].envelope_timer, 262_144);
    }

    #[test]
    fn noise_envelope_uses_the_same_cycle_accurate_clock() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x79, 0x19); // Start at 1, increase every envelope period.
        psg.write(0x7D, 0x80);

        psg.advance(2 * 262_144 + 9);

        assert_eq!(psg.noise.volume, 3);
        assert_eq!(psg.noise.envelope_timer, 262_144 - 9);
    }
    #[test]
    fn oscillator_and_envelope_state_are_invariant_to_cycle_chunking() {
        let mut whole = Psg::default();
        let mut chunked = Psg::default();
        for psg in [&mut whole, &mut chunked] {
            psg.set_master_enabled(true);
            psg.write(0x63, 0xA9);
            psg.write(0x65, 0xC0);
            psg.write(0x79, 0xB3);
            psg.write(0x7C, 0x07);
            psg.write(0x7D, 0xC0);
        }

        let total = 1_048_579;
        whole.advance(total);
        let mut remaining = total;
        for chunk in [1, 7, 31, 255, 4_096, 65_537, 17, 8_192] {
            let step = chunk.min(remaining);
            chunked.advance(step);
            remaining -= step;
        }
        while remaining != 0 {
            let step = remaining.min(32_771);
            chunked.advance(step);
            remaining -= step;
        }

        assert_eq!(whole.channel_samples(), chunked.channel_samples());
        assert_eq!(whole.status_bits(), chunked.status_bits());
        assert_eq!(whole.square[0].phase, chunked.square[0].phase);
        assert_eq!(whole.square[0].volume, chunked.square[0].volume);
        assert_eq!(
            whole.square[0].envelope_timer,
            chunked.square[0].envelope_timer
        );
        assert_eq!(whole.noise.lfsr, chunked.noise.lfsr);
        assert_eq!(whole.noise.timer, chunked.noise.timer);
        assert_eq!(whole.noise.volume, chunked.noise.volume);
        assert_eq!(whole.noise.envelope_timer, chunked.noise.envelope_timer);
        assert_eq!(whole.length_clock, chunked.length_clock);
        assert_eq!(whole.sweep_clock, chunked.sweep_clock);
    }

    #[test]
    fn square_dac_disable_stops_channel_until_retriggered() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0x80);
        assert_eq!(psg.status_bits() & 1, 1);

        psg.write(0x63, 0x00); // NR12 disables the channel DAC.
        assert_eq!(psg.status_bits() & 1, 0);
        psg.write(0x63, 0xF0); // Re-enabling DAC alone does not retrigger.
        assert_eq!(psg.status_bits() & 1, 0);
        psg.write(0x65, 0x80);
        assert_eq!(psg.status_bits() & 1, 1);
    }

    #[test]
    fn wave_dac_disable_stops_playback_immediately() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x70, 0x80);
        psg.write(0x72, 0x20);
        psg.write(0x75, 0x80);
        assert_eq!(psg.status_bits() & 4, 4);

        psg.write(0x70, 0x00);
        assert_eq!(psg.status_bits() & 4, 0);
        assert_eq!(psg.channel_samples()[2], 0);
    }

    #[test]
    fn noise_dac_disable_stops_channel_until_retriggered() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x79, 0xF0);
        psg.write(0x7D, 0x80);
        assert_eq!(psg.status_bits() & 8, 8);

        psg.write(0x79, 0x00); // NR42 disables the noise DAC.
        assert_eq!(psg.status_bits() & 8, 0);
        psg.write(0x79, 0xF0);
        assert_eq!(psg.status_bits() & 8, 0);
        psg.write(0x7D, 0x80);
        assert_eq!(psg.status_bits() & 8, 8);
    }

    #[test]
    fn square_and_noise_triggers_cannot_enable_disabled_dacs() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x63, 0x00);
        psg.write(0x65, 0x80);
        psg.write(0x79, 0x00);
        psg.write(0x7D, 0x80);

        assert_eq!(psg.status_bits() & 0b1001, 0);
    }

    #[test]
    fn square_length_counter_runs_before_channel_trigger() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x62, 1); // 63 length units.
        psg.write(0x65, 0x40); // Enable length without triggering.
        psg.advance(65_536);
        psg.write(0x63, 0xF0); // Enable DAC.
        psg.write(0x65, 0xC0); // Trigger with 62 units remaining.
        psg.advance(62 * 65_536);
        assert_eq!(psg.status_bits() & 1, 0);
    }

    #[test]
    fn wave_length_counter_runs_before_channel_trigger() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x73, 1); // 255 length units.
        psg.write(0x75, 0x40); // Enable length without triggering.
        psg.advance(65_536);
        psg.write(0x70, 0x80); // Enable wave DAC.
        psg.write(0x75, 0xC0); // Trigger with 254 units remaining.
        psg.advance(254 * 65_536);
        assert_eq!(psg.status_bits() & 4, 0);
    }

    #[test]
    fn noise_length_counter_runs_before_channel_trigger() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x78, 1); // 63 length units.
        psg.write(0x7D, 0x40); // Enable length without triggering.
        psg.advance(65_536);
        psg.write(0x79, 0xF0); // Enable noise DAC.
        psg.write(0x7D, 0xC0); // Trigger with 62 units remaining.
        psg.advance(62 * 65_536);
        assert_eq!(psg.status_bits() & 8, 0);
    }

}
