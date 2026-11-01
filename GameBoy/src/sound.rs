//! Initial Game Boy Advance PSG square-wave synthesis.
//!
//! This models the two pulse channels, their duty patterns, length counters,
//! volume envelopes, master volume, and stereo routing. Sweep, wave RAM, and
//! noise-channel synthesis are not implemented yet.

/// Two-channel programmable sound generator for the GBA's legacy PSG.
#[derive(Clone, Debug, Default)]
pub struct Psg {
    registers: [u8; 0x22],
    channels: [SquareChannel; 2],
    master_enabled: bool,
    length_clock: u32,
}

#[derive(Clone, Debug, Default)]
struct SquareChannel {
    enabled: bool,
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
}

impl Psg {
    /// Writes one byte in the PSG register window (offset 0x60..=0x81).
    /// Returns the value retained by the register after hardware masking.
    pub fn write(&mut self, offset: usize, value: u8) -> Option<u8> {
        if !(0x60..=0x81).contains(&offset) {
            return None;
        }
        if !self.master_enabled {
            return Some(0);
        }
        let index = offset - 0x60;
        let masked = match offset {
            0x62 | 0x68 => value & 0xC0,
            0x65 | 0x6D => value & 0xC7,
            0x78..=0x7F => 0,
            _ => value,
        };
        self.registers[index] = masked;
        match offset {
            0x62 | 0x68 => {
                let channel = if offset == 0x62 { 0 } else { 1 };
                self.channels[channel].duty = masked >> 6;
                self.channels[channel].length = 64 - u16::from(value & 0x3F);
            }
            0x63 | 0x69 => {
                let channel = if offset == 0x63 { 0 } else { 1 };
                let state = &mut self.channels[channel];
                state.initial_volume = value >> 4;
                state.envelope_increase = value & 0x08 != 0;
                state.envelope_period = value & 0x07;
                if value & 0xF8 == 0 {
                    state.enabled = false;
                }
            }
            0x64 | 0x6C => {
                let channel = if offset == 0x64 { 0 } else { 1 };
                self.channels[channel].frequency =
                    (self.channels[channel].frequency & 0x0700) | u16::from(value);
            }
            0x65 | 0x6D => {
                let channel = if offset == 0x65 { 0 } else { 1 };
                let state = &mut self.channels[channel];
                state.frequency =
                    (state.frequency & 0x00FF) | (u16::from(value & 0x07) << 8);
                state.length_enabled = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    state.enabled = true;
                    state.phase = 0;
                    state.timer = Self::period(state.frequency);
                    state.volume = state.initial_volume;
                    state.envelope_timer = Self::envelope_period(state.envelope_period);
                    if state.length == 0 {
                        state.length = 64;
                    }
                }
            }
            _ => {}
        }
        Some(masked)
    }

    /// Enables or disables the PSG master circuit.
    pub fn set_master_enabled(&mut self, enabled: bool) {
        if self.master_enabled && !enabled {
            self.registers.fill(0);
            self.channels = std::array::from_fn(|_| SquareChannel::default());
            self.length_clock = 0;
        }
        self.master_enabled = enabled;
    }

    /// Advances both pulse oscillators and their envelope/length clocks.
    pub fn advance(&mut self, cycles: u32) {
        if !self.master_enabled || cycles == 0 {
            return;
        }
        self.length_clock = self.length_clock.saturating_add(cycles);
        let length_ticks = self.length_clock / 65536;
        self.length_clock %= 65536;
        for channel in &mut self.channels {
            if !channel.enabled {
                continue;
            }
            let period = Self::period(channel.frequency);
            let mut remaining = cycles;
            while remaining >= channel.timer {
                remaining -= channel.timer;
                channel.phase = (channel.phase + 1) & 7;
                channel.timer = period;
            }
            channel.timer -= remaining;
            if channel.length_enabled && length_ticks != 0 {
                channel.length = channel.length.saturating_sub(length_ticks as u16);
                if channel.length == 0 {
                    channel.enabled = false;
                }
            }
            if channel.envelope_period != 0 {
                let envelope_period = Self::envelope_period(channel.envelope_period);
                channel.envelope_timer = channel.envelope_timer.saturating_sub(cycles);
                while channel.envelope_timer == 0 && channel.enabled {
                    if channel.envelope_increase {
                        if channel.volume == 15 {
                            break;
                        }
                        channel.volume += 1;
                    } else {
                        if channel.volume == 0 {
                            break;
                        }
                        channel.volume -= 1;
                    }
                    channel.envelope_timer = envelope_period;
                }
            }
        }
    }

    /// Returns the two pulse-channel output levels before stereo routing.
    pub fn channel_samples(&self) -> [i16; 2] {
        if !self.master_enabled {
            return [0; 2];
        }
        std::array::from_fn(|index| {
            let channel = &self.channels[index];
            if !channel.enabled || channel.volume == 0 {
                return 0;
            }
            const DUTY: [[bool; 8]; 4] = [
                [false, false, false, false, false, false, false, true],
                [true, false, false, false, false, false, false, true],
                [true, false, false, false, false, true, true, true],
                [false, true, true, true, true, true, true, false],
            ];
            let magnitude = i16::from(channel.volume) * 2;
            if DUTY[channel.duty as usize][channel.phase as usize] {
                magnitude
            } else {
                -magnitude
            }
        })
    }

    /// Mixes the two pulse channels using NR50 master volume and NR51 routing.
    pub fn stereo_sample(&self) -> (i16, i16) {
        let samples = self.channel_samples();
        let routing = self.registers[0x21];
        let volume = self.registers[0x20];
        let left_volume = i16::from((volume >> 4) & 7) + 1;
        let right_volume = i16::from(volume & 7) + 1;
        let mut left = 0;
        let mut right = 0;
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

    /// Returns the currently enabled pulse-channel bits for SOUNDCNT_X.
    pub fn status_bits(&self) -> u8 {
        u8::from(self.channels[0].enabled) | (u8::from(self.channels[1].enabled) << 1)
    }

    fn period(frequency: u16) -> u32 {
        u32::from(2048_u16.saturating_sub(frequency & 0x07FF)).max(1) * 16
    }

    fn envelope_period(period: u8) -> u32 {
        u32::from(period.max(1)) * 65536
    }
}

#[cfg(test)]
mod tests {
    use super::Psg;

    #[test]
    fn pulse_channels_require_master_enable_and_trigger() {
        let mut psg = Psg::default();
        psg.write(0x62, 0x80);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0x80);
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
    fn disabling_master_sound_stops_channels_and_clears_status() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0x80);
        assert_eq!(psg.status_bits(), 1);
        psg.set_master_enabled(false);
        assert_eq!(psg.status_bits(), 0);
        assert_eq!(psg.stereo_sample(), (0, 0));
    }

    #[test]
    fn length_enable_expires_across_multiple_small_cycle_advances() {
        let mut psg = Psg::default();
        psg.set_master_enabled(true);
        psg.write(0x62, 0x3F);
        psg.write(0x63, 0xF0);
        psg.write(0x65, 0xC0);
        for _ in 0..256 {
            psg.advance(256);
        }
        assert_eq!(psg.status_bits(), 0);
    }
}
