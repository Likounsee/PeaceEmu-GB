//! Cartridge backup-device identification plus SRAM, Flash, and serial EEPROM emulation.

use crate::memory::SramSizeError;
use std::cell::Cell;

/// Backup-device type inferred from ROM signatures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveType {
    /// 32 KiB byte-wide static RAM.
    Sram,
    /// 64 KiB flash memory.
    Flash64K,
    /// 128 KiB flash memory, indicated by the `FLASH1M_V` signature.
    Flash128K,
    /// Serial EEPROM with a capacity that must be determined separately.
    Eeprom,
}

impl SaveType {
    /// Detects a common GBA backup signature. Unknown images are not guessed.
    pub fn detect(rom: &[u8]) -> Option<Self> {
        let has = |needle: &[u8]| rom.windows(needle.len()).any(|window| window == needle);

        if has(b"FLASH1M_V") {
            Some(Self::Flash128K)
        } else if has(b"FLASH512_V") || has(b"FLASH_V") {
            Some(Self::Flash64K)
        } else if has(b"EEPROM_V") {
            Some(Self::Eeprom)
        } else if has(b"SRAM_V") {
            Some(Self::Sram)
        } else {
            None
        }
    }

    /// Returns the known capacity; EEPROM size is not guessed from its marker.
    pub const fn capacity_bytes(self) -> Option<usize> {
        match self {
            Self::Sram => Some(32 * 1024),
            Self::Flash64K => Some(64 * 1024),
            Self::Flash128K => Some(128 * 1024),
            Self::Eeprom => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eeprom_serial_write_read_and_capacity_detection() {
        let mut eeprom = EepromDevice::new();
        let address = 63_usize;
        let value = 0x1234_5678_9ABC_DEF0_u64;
        let mut write_bits = vec![1, 1, 0];
        write_bits.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u8));
        write_bits.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u8));
        write_bits.push(0);
        for bit in write_bits {
            eeprom.write_bit(u16::from(bit));
        }
        assert_eq!(eeprom.capacity_bytes(), 512);
        assert_eq!(
            &eeprom.data()[address * 8..address * 8 + 8],
            &value.to_be_bytes()
        );

        let mut read_bits = vec![1, 1, 1];
        read_bits.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u8));
        read_bits.push(0);
        for bit in read_bits {
            eeprom.write_bit(u16::from(bit));
        }
        for _ in 0..4 {
            assert_eq!(eeprom.read_bit(), 0);
        }
        let mut result = 0_u64;
        for _ in 0..64 {
            result = (result << 1) | u64::from(eeprom.read_bit());
        }
        assert_eq!(result, value);

        let large_image = vec![0xA5; 8 * 1024];
        eeprom.load_data(&large_image).unwrap();
        assert_eq!(eeprom.capacity_bytes(), 8 * 1024);
        assert_eq!(eeprom.data()[0], 0xA5);

        let large_address = 0x03FF_usize;
        let large_value = 0x0F1E_2D3C_4B5A_6978_u64;
        let mut long_write = vec![1, 1, 0];
        long_write.extend((0..14).rev().map(|bit| ((large_address >> bit) & 1) as u8));
        long_write.extend((0..64).rev().map(|bit| ((large_value >> bit) & 1) as u8));
        long_write.push(0);
        for bit in long_write {
            eeprom.write_bit(u16::from(bit));
        }
        assert_eq!(
            &eeprom.data()[large_address * 8..large_address * 8 + 8],
            &large_value.to_be_bytes()
        );

        let mut long_read = vec![1, 1, 1];
        long_read.extend((0..14).rev().map(|bit| ((large_address >> bit) & 1) as u8));
        long_read.push(0);
        for bit in long_read {
            eeprom.write_bit(u16::from(bit));
        }
        for _ in 0..4 {
            assert_eq!(eeprom.read_bit(), 0);
        }
        let mut long_result = 0_u64;
        for _ in 0..64 {
            long_result = (long_result << 1) | u64::from(eeprom.read_bit());
        }
        assert_eq!(long_result, large_value);
    }

    #[test]
    fn malformed_eeprom_stop_bit_does_not_poison_next_transaction() {
        let mut eeprom = EepromDevice::new();
        let address = 2_usize;
        let value = 0x0123_4567_89AB_CDEF_u64;
        let mut malformed = vec![1, 1, 0];
        malformed.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u8));
        malformed.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u8));
        malformed.push(1);
        for bit in malformed {
            eeprom.write_bit(u16::from(bit));
        }
        assert_eq!(&eeprom.data()[address * 8..address * 8 + 8], &[0xFF; 8]);

        let mut valid = vec![1, 1, 0];
        valid.extend((0..6).rev().map(|bit| ((address >> bit) & 1) as u8));
        valid.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u8));
        valid.push(0);
        for bit in valid {
            eeprom.write_bit(u16::from(bit));
        }
        assert_eq!(
            &eeprom.data()[address * 8..address * 8 + 8],
            &value.to_be_bytes()
        );
    }

    #[test]
    fn eeprom_dma_length_selects_large_capacity_before_first_command() {
        let mut eeprom = EepromDevice::new();
        eeprom.configure_transfer_length(82);
        assert_eq!(eeprom.capacity_bytes(), 8 * 1024);

        let address = 0x03FF_usize;
        let value = 0x89AB_CDEF_0123_4567_u64;
        let mut write_bits = vec![1, 1, 0];
        write_bits.extend((0..14).rev().map(|bit| ((address >> bit) & 1) as u8));
        write_bits.extend((0..64).rev().map(|bit| ((value >> bit) & 1) as u8));
        write_bits.push(0);
        for bit in write_bits {
            eeprom.write_bit(u16::from(bit));
        }
        assert_eq!(
            &eeprom.data()[address * 8..address * 8 + 8],
            &value.to_be_bytes()
        );

        eeprom.configure_transfer_length(18);
        let mut read_bits = vec![1, 1, 1];
        read_bits.extend((0..14).rev().map(|bit| ((address >> bit) & 1) as u8));
        read_bits.push(0);
        for bit in read_bits {
            eeprom.write_bit(u16::from(bit));
        }
        for _ in 0..4 {
            assert_eq!(eeprom.read_bit(), 0);
        }
        let mut result = 0_u64;
        for _ in 0..64 {
            result = (result << 1) | u64::from(eeprom.read_bit());
        }
        assert_eq!(result, value);
    }

    #[test]
    fn detects_known_signatures_and_leaves_unknown_roms_unknown() {
        let cases = [
            (&b"SRAM_V"[..], SaveType::Sram),
            (&b"FLASH_V"[..], SaveType::Flash64K),
            (&b"FLASH512_V"[..], SaveType::Flash64K),
            (&b"FLASH1M_V"[..], SaveType::Flash128K),
            (&b"EEPROM_V"[..], SaveType::Eeprom),
        ];

        for (marker, expected) in cases {
            assert_eq!(SaveType::detect(marker), Some(expected));
        }

        assert_eq!(SaveType::detect(b"no save marker"), None);
    }

    #[test]
    fn reports_only_capacities_known_from_the_save_type() {
        assert_eq!(SaveType::Sram.capacity_bytes(), Some(32 * 1024));
        assert_eq!(SaveType::Flash64K.capacity_bytes(), Some(64 * 1024));
        assert_eq!(SaveType::Flash128K.capacity_bytes(), Some(128 * 1024));
        assert_eq!(SaveType::Eeprom.capacity_bytes(), None);
    }
}

/// Serial EEPROM backup device used by GBA cartridges.
///
/// Transactions are clocked through bit 0 of halfword accesses in Game Pak
/// region 0x0D. The first completed transaction selects the 512-byte or 8-KiB
/// address space according to its address width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EepromDevice {
    data: Vec<u8>,
    input_bits: Vec<u8>,
    output_bits: Vec<u8>,
    output_cursor: Cell<usize>,
    address_bits: usize,
}

impl Default for EepromDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl EepromDevice {
    /// Creates an erased 512-byte EEPROM; long-address transactions expand it to 8 KiB.
    pub fn new() -> Self {
        Self {
            data: vec![0xFF; 512],
            input_bits: Vec::with_capacity(82),
            output_bits: Vec::with_capacity(68),
            output_cursor: Cell::new(0),
            address_bits: 6,
        }
    }

    /// Returns the currently selected EEPROM capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.data.len()
    }

    /// Returns the persistent EEPROM image.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Loads a complete EEPROM image, accepting only hardware-supported capacities.
    pub fn load_data(&mut self, data: &[u8]) -> Result<(), SramSizeError> {
        if data.len() != 512 && data.len() != 8 * 1024 {
            return Err(SramSizeError {
                expected: self.data.len(),
                actual: data.len(),
            });
        }
        self.data.clear();
        self.data.extend_from_slice(data);
        self.address_bits = if data.len() == 8 * 1024 { 14 } else { 6 };
        self.input_bits.clear();
        self.output_bits.clear();
        self.output_cursor.set(0);
        Ok(())
    }

    /// Selects the EEPROM command address width from a DMA transfer length.
    /// GBA EEPROM commands use 10/74 halfwords for 512-byte devices and
    /// 18/82 halfwords for 8-KiB devices (read/write command streams).
    pub fn configure_transfer_length(&mut self, halfwords: u32) {
        match halfwords {
            10 | 74 => self.address_bits = 6,
            18 | 82 => {
                self.address_bits = 14;
                if self.data.len() != 8 * 1024 {
                    self.data.resize(8 * 1024, 0xFF);
                }
            }
            _ => {}
        }
    }

    /// Clocks one input bit into the EEPROM command stream.
    pub fn write_bit(&mut self, value: u16) {
        self.input_bits.push((value & 1) as u8);
        if self.input_bits.first() != Some(&1) {
            self.input_bits.clear();
            return;
        }
        let len = self.input_bits.len();
        if len > 82 {
            self.input_bits.clear();
            return;
        }
        if len < 10 {
            return;
        }
        let opcode = (self.input_bits[1] << 1) | self.input_bits[2];
        let long_address = self.address_bits == 14;
        let expected_read_len = if long_address { 18 } else { 10 };
        let expected_write_len = if long_address { 82 } else { 74 };
        let expected_len = match opcode {
            2 => expected_write_len,
            3 => expected_read_len,
            _ => {
                self.input_bits.clear();
                return;
            }
        };
        // Do not interpret payload zeros as a stop bit. Only inspect the stop
        // bit at the exact length of the command selected by the DMA protocol.
        if len != expected_len {
            return;
        }
        if self.input_bits.last() != Some(&0) {
            self.input_bits.clear();
            return;
        }
        let bits = std::mem::take(&mut self.input_bits);
        let address_bits = if long_address { 14 } else { 6 };
        let address = bits[3..3 + address_bits]
            .iter()
            .fold(0usize, |acc, bit| (acc << 1) | usize::from(*bit));
        if address_bits == 14 && self.data.len() != 8 * 1024 {
            self.data.resize(8 * 1024, 0xFF);
        }
        let block_count = self.data.len() / 8;
        let offset = (address % block_count) * 8;
        match (opcode, bits.len()) {
            (2, 74 | 82) => {
                for byte in 0..8 {
                    let mut value = 0_u8;
                    for bit in 0..8 {
                        value = (value << 1) | bits[3 + address_bits + byte * 8 + bit];
                    }
                    self.data[offset + byte] = value;
                }
            }
            (3, 10 | 18) => {
                self.output_bits.clear();
                self.output_cursor.set(0);
                self.output_bits.extend([0, 0, 0, 0]);
                for byte in &self.data[offset..offset + 8] {
                    for bit in (0..8).rev() {
                        self.output_bits.push((byte >> bit) & 1);
                    }
                }
            }
            _ => {}
        }
    }

    /// Reads the next serial output bit; idle EEPROM reads return one.
    pub fn read_bit(&self) -> u16 {
        let cursor = self.output_cursor.get();
        let bit = self.output_bits.get(cursor).copied().unwrap_or(1);
        if cursor < self.output_bits.len() {
            self.output_cursor.set(cursor + 1);
        }
        u16::from(bit)
    }
}

/// Flash command-sequencer state for the GBA's byte-wide backup devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlashCommand {
    Ready,
    Unlock1,
    Unlock2,
    Program,
    EraseUnlock1,
    EraseUnlock2,
    EraseConfirm,
    BankSelect,
}

/// A GBA Flash backup device (64 KiB or 128 KiB).
///
/// Programming can only clear bits; sector/chip erase restores them to 1.
/// Command addresses are the low 16 bits of the Game Pak save-window address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlashDevice {
    data: Vec<u8>,
    bank: usize,
    id_mode: bool,
    command: FlashCommand,
}

impl FlashDevice {
    /// Creates a blank Flash device for a supported Flash save type.
    pub fn new(save_type: SaveType) -> Option<Self> {
        let capacity = match save_type {
            SaveType::Flash64K => 64 * 1024,
            SaveType::Flash128K => 128 * 1024,
            _ => return None,
        };
        Some(Self {
            data: vec![0xFF; capacity],
            bank: 0,
            id_mode: false,
            command: FlashCommand::Ready,
        })
    }

    /// Returns the Flash capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.data.len()
    }

    /// Returns the persistent Flash image.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Replaces the complete persistent image, rejecting mismatched sizes atomically.
    pub fn load_data(&mut self, data: &[u8]) -> Result<(), SramSizeError> {
        if data.len() != self.data.len() {
            return Err(SramSizeError {
                expected: self.data.len(),
                actual: data.len(),
            });
        }
        self.data.copy_from_slice(data);
        // Loading a save image restores persistent bytes, not an in-flight
        // command sequence or a previously selected hardware bank.
        self.bank = 0;
        self.id_mode = false;
        self.command = FlashCommand::Ready;
        Ok(())
    }

    /// Reads one byte from the currently selected Flash bank.
    pub fn read(&self, address: u16) -> u8 {
        if self.id_mode {
            // Common Macronix-compatible IDs used by GBA Flash cartridges.
            return if self.data.len() == 64 * 1024 {
                match address {
                    0 => 0xC2,
                    1 => 0x1C,
                    _ => 0xFF,
                }
            } else {
                match address {
                    0 => 0x62,
                    1 => 0x13,
                    _ => 0xFF,
                }
            };
        }
        self.data[self.bank * 0x1_0000 + usize::from(address)]
    }

    /// Feeds a byte write into the Flash command protocol.
    pub fn write(&mut self, address: u16, value: u8) {
        // While the device is in identification mode, ordinary command and
        // program writes are ignored. Only the reset command exits ID mode.
        if self.id_mode {
            if value == 0xF0 {
                self.id_mode = false;
                self.command = FlashCommand::Ready;
            }
            return;
        }
        if value == 0xF0 && self.command != FlashCommand::Program {
            self.id_mode = false;
            self.command = FlashCommand::Ready;
            return;
        }

        let command = self.command;
        self.command = FlashCommand::Ready;
        match command {
            FlashCommand::Ready => {
                if address == 0x5555 && value == 0xAA {
                    self.command = FlashCommand::Unlock1;
                }
            }
            FlashCommand::Unlock1 => {
                if address == 0x2AAA && value == 0x55 {
                    self.command = FlashCommand::Unlock2;
                }
            }
            FlashCommand::Unlock2 if address == 0x5555 => match value {
                0xA0 => self.command = FlashCommand::Program,
                0x80 => self.command = FlashCommand::EraseUnlock1,
                0x90 => self.id_mode = true,
                0xB0 if self.data.len() == 128 * 1024 => {
                    self.command = FlashCommand::BankSelect;
                }
                _ => {}
            },
            FlashCommand::Program => {
                let index = self.bank * 0x1_0000 + usize::from(address);
                self.data[index] &= value;
            }
            FlashCommand::EraseUnlock1 => {
                if address == 0x5555 && value == 0xAA {
                    self.command = FlashCommand::EraseUnlock2;
                }
            }
            FlashCommand::EraseUnlock2 => {
                if address == 0x2AAA && value == 0x55 {
                    self.command = FlashCommand::EraseConfirm;
                }
            }
            FlashCommand::EraseConfirm => {
                if value == 0x10 && address == 0x5555 {
                    self.data.fill(0xFF);
                } else if value == 0x30 {
                    let start = self.bank * 0x1_0000 + (usize::from(address) & !0x0FFF);
                    self.data[start..start + 0x1000].fill(0xFF);
                }
            }
            FlashCommand::BankSelect => {
                if address == 0 {
                    self.bank = usize::from(value & 1);
                }
            }
            FlashCommand::Unlock2 => {}
        }
    }
}

#[cfg(test)]
mod flash_tests {
    use super::*;

    fn unlock_flash(flash: &mut FlashDevice) {
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
    }

    #[test]
    fn flash_programming_only_clears_bits_and_persists_data() {
        let mut flash = FlashDevice::new(SaveType::Flash64K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0123, 0x5A);
        assert_eq!(flash.read(0x0123), 0x5A);

        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0124, 0xF0);
        assert_eq!(flash.read(0x0124), 0xF0);

        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0123, 0xFF);
        assert_eq!(flash.read(0x0123), 0x5A);
    }

    #[test]
    fn flash_sector_and_chip_erase_restore_erased_bytes() {
        let mut flash = FlashDevice::new(SaveType::Flash64K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x1234, 0x00);
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x2234, 0x00);

        unlock_flash(&mut flash);
        flash.write(0x5555, 0x80);
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x1234, 0x30);
        assert_eq!(flash.read(0x1234), 0xFF);
        assert_eq!(flash.read(0x2234), 0x00);

        unlock_flash(&mut flash);
        flash.write(0x5555, 0x80);
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0x10);
        assert_eq!(flash.read(0x2234), 0xFF);
    }

    #[test]
    fn flash128k_bank_switching_isolated_and_64k_does_not_switch() {
        let mut flash = FlashDevice::new(SaveType::Flash128K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0001, 0x12);
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xB0);
        flash.write(0x0000, 1);
        assert_eq!(flash.read(1), 0xFF);
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0001, 0x34);
        assert_eq!(flash.read(1), 0x34);
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xB0);
        flash.write(0x0000, 0);
        assert_eq!(flash.read(1), 0x12);
    }

    #[test]
    fn flash_id_mode_and_save_image_size_validation() {
        let mut flash = FlashDevice::new(SaveType::Flash64K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0x90);
        assert_eq!(flash.read(0), 0xC2);
        assert_eq!(flash.read(1), 0x1C);
        // A rejected image must not disturb the active device state.
        assert!(flash.load_data(&[0; 32]).is_err());
        assert_eq!(flash.read(0), 0xC2);
        flash.write(0, 0xF0);
        assert_eq!(flash.read(0), 0xFF);
        assert_eq!(flash.data().len(), 64 * 1024);
    }

    #[test]
    fn flash_id_mode_ignores_program_commands_until_reset() {
        let mut flash = FlashDevice::new(SaveType::Flash64K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0x90);
        assert_eq!(flash.read(0), 0xC2);

        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0123, 0x00);
        assert_eq!(flash.read(0), 0xC2);
        assert_eq!(flash.read(0x0123), 0xFF); // ID-mode reads do not expose array data.

        flash.write(0, 0xF0);
        assert_eq!(flash.read(0), 0xFF);
        assert_eq!(flash.read(0x0123), 0xFF); // Ignored program did not change erased data.
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0123, 0x5A);
        assert_eq!(flash.read(0x0123), 0x5A);
    }

    #[test]
    fn loading_flash_image_resets_bank_and_command_protocol() {
        let mut flash = FlashDevice::new(SaveType::Flash128K).unwrap();
        let mut image = vec![0xFF; 128 * 1024];
        image[0] = 0x12;
        image[1] = 0x34;
        image[64 * 1024] = 0x56;
        image[64 * 1024 + 1] = 0x78;

        unlock_flash(&mut flash);
        flash.write(0x5555, 0xB0);
        flash.write(0, 1);
        unlock_flash(&mut flash);
        flash.write(0x5555, 0x90);
        assert_eq!(flash.read(0), 0x62);

        flash.load_data(&image).unwrap();
        assert_eq!(flash.read(0), 0x12);
        assert_eq!(flash.read(1), 0x34);
        assert_eq!(flash.capacity_bytes(), 128 * 1024);
    }

    #[test]
    fn flash_invalid_unlock_sequence_cannot_program_or_erase_data() {
        let mut flash = FlashDevice::new(SaveType::Flash64K).unwrap();
        unlock_flash(&mut flash);
        flash.write(0x5555, 0xA0);
        flash.write(0x0123, 0x00);
        assert_eq!(flash.read(0x0123), 0x00);

        // A wrong second unlock address cancels the sequence. The following
        // erase-looking writes must not be interpreted as an erase command.
        flash.write(0x5555, 0xAA);
        flash.write(0x1234, 0x55);
        flash.write(0x5555, 0x80);
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0x10);
        assert_eq!(flash.read(0x0123), 0x00);
    }
}
