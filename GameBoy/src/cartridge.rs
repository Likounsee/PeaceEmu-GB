//! Validation and metadata extraction for Game Boy Advance ROM images.

use core::fmt;

/// Minimum byte length required to contain the complete GBA cartridge header.
pub const GBA_HEADER_SIZE: usize = 0xC0;
/// Largest supported raw GBA ROM image size (32 MiB).
pub const MAX_ROM_SIZE: usize = 32 * 1024 * 1024;

/// A validated Game Boy Advance cartridge image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GbaCartridge {
    rom: Vec<u8>,
    /// Mask for the physical address-line window, cached to avoid recomputing it on every ROM fetch.
    rom_address_mask: usize,
    title: String,
    game_code: String,
    maker_code: String,
    save_type: Option<crate::save::SaveType>,
}

impl GbaCartridge {
    /// Parses a ROM and validates its size and Nintendo header complement checksum.
    ///
    /// The input is copied only after the size and header checks succeed. This
    /// parser does not claim that a ROM is playable or that every header field
    /// is valid; execution support is tracked separately.
    pub fn parse(bytes: &[u8]) -> Result<Self, CartridgeError> {
        if bytes.len() < GBA_HEADER_SIZE {
            return Err(CartridgeError::TooSmall {
                actual: bytes.len(),
            });
        }
        if bytes.len() > MAX_ROM_SIZE {
            return Err(CartridgeError::TooLarge {
                actual: bytes.len(),
            });
        }

        let expected = bytes[0xBD];
        let checksum = bytes[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        if checksum != expected {
            return Err(CartridgeError::InvalidHeaderChecksum {
                expected,
                calculated: checksum,
            });
        }

        Ok(Self {
            rom: bytes.to_vec(),
            rom_address_mask: bytes.len().next_power_of_two() - 1,
            title: header_string(&bytes[0xA0..0xAC]),
            game_code: header_string(&bytes[0xAC..0xB0]),
            maker_code: header_string(&bytes[0xB0..0xB2]),
            save_type: crate::save::SaveType::detect(bytes),
        })
    }

    /// Returns the original validated ROM bytes.
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }

    /// Returns the 12-byte title field with trailing NULs and spaces removed.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the four-byte game code.
    pub fn game_code(&self) -> &str {
        &self.game_code
    }

    /// Returns the two-byte maker code.
    pub fn maker_code(&self) -> &str {
        &self.maker_code
    }

    /// Returns the cartridge header's fixed byte at offset 0xB2.
    ///
    /// Retail GBA ROMs normally store 0x96 here. This is exposed for frontend
    /// diagnostics and is not used as a hard rejection criterion, since homebrew
    /// and modified images may intentionally differ.
    pub fn header_fixed_value(&self) -> u8 {
        self.rom[0xB2]
    }

    /// Returns the GBA software revision byte from header offset 0xBC.
    pub fn software_version(&self) -> u8 {
        self.rom[0xBC]
    }

    /// Returns the stored complement checksum byte from header offset 0xBD.
    ///
    /// Parsing already verifies this value against the title, game code, maker
    /// code, and intervening header bytes.
    pub fn header_checksum(&self) -> u8 {
        self.rom[0xBD]
    }

    /// Returns the ROM image size in bytes.
    pub fn len(&self) -> usize {
        self.rom.len()
    }

    /// Returns whether the ROM image is empty. Valid cartridges are never empty.
    pub fn is_empty(&self) -> bool {
        self.rom.is_empty()
    }

    /// Detects the cartridge backup device from common ROM signatures.
    ///
    /// The result is only a hint: homebrew may omit signatures, and ROM hacks
    /// may preserve stale ones. Unknown signatures return `None` rather than
    /// silently assuming SRAM or another device.
    pub fn save_type(&self) -> Option<crate::save::SaveType> {
        self.save_type
    }

    pub(crate) fn read_rom_byte(&self, offset: usize) -> u8 {
        // Game Pak ROM address lines mirror at the next power-of-two window.
        // A non-power-of-two dump leaves the unpopulated tail of that window
        // open as 0xFF; modulo-by-file-length would incorrectly repeat it.
        self.rom
            .get(offset & self.rom_address_mask)
            .copied()
            .unwrap_or(0xFF)
    }
}

fn header_string(field: &[u8]) -> String {
    let end = field
        .iter()
        .rposition(|byte| *byte != 0 && *byte != b' ')
        .map_or(0, |index| index + 1);
    field[..end]
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Why a cartridge image was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CartridgeError {
    /// The image is too short to contain the full cartridge header.
    TooSmall {
        /// Actual image length in bytes.
        actual: usize,
    },
    /// The image exceeds the supported 32 MiB raw-ROM limit.
    TooLarge {
        /// Actual image length in bytes.
        actual: usize,
    },
    /// The header's stored complement checksum differs from the calculated value.
    InvalidHeaderChecksum {
        /// Checksum byte stored in the ROM header.
        expected: u8,
        /// Checksum calculated from the header fields.
        calculated: u8,
    },
}

impl fmt::Display for CartridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall { actual } => write!(
                formatter,
                "GBA ROM is too small: expected at least {GBA_HEADER_SIZE} bytes, got {actual}"
            ),
            Self::TooLarge { actual } => write!(
                formatter,
                "GBA ROM exceeds the {MAX_ROM_SIZE}-byte limit: got {actual} bytes"
            ),
            Self::InvalidHeaderChecksum { expected, calculated } => write!(
                formatter,
                "invalid GBA header checksum: stored 0x{expected:02X}, calculated 0x{calculated:02X}"
            ),
        }
    }
}

impl std::error::Error for CartridgeError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_rom(size: usize) -> Vec<u8> {
        let mut rom = vec![0; size];
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"TGME");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        let checksum = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        rom[0xBD] = checksum;
        rom
    }

    #[test]
    fn parses_title_identifiers_and_additional_header_metadata() {
        let mut rom = valid_rom(GBA_HEADER_SIZE);
        rom[0xB2] = 0x96;
        rom[0xBC] = 3;
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let cartridge = GbaCartridge::parse(&rom).unwrap();
        assert_eq!(cartridge.title(), "TEST GAME");
        assert_eq!(cartridge.game_code(), "TGME");
        assert_eq!(cartridge.maker_code(), "01");
        assert_eq!(cartridge.header_fixed_value(), 0x96);
        assert_eq!(cartridge.software_version(), 3);
        assert_eq!(cartridge.header_checksum(), rom[0xBD]);
        assert_eq!(cartridge.len(), GBA_HEADER_SIZE);
    }

    #[test]
    fn rom_address_lines_mirror_power_of_two_images_and_open_unpopulated_tail() {
        let mut rom = valid_rom(0x180);
        rom[0x140] = 0x42;
        let cartridge = GbaCartridge::parse(&rom).unwrap();

        assert_eq!(cartridge.read_rom_byte(0x140), 0x42);
        assert_eq!(cartridge.read_rom_byte(0x180), 0xFF);
        assert_eq!(cartridge.read_rom_byte(0x1FF), 0xFF);
        assert_eq!(cartridge.read_rom_byte(0x200), rom[0]);
    }

    #[test]
    fn detects_save_type_from_rom_signature() {
        let mut rom = valid_rom(GBA_HEADER_SIZE + 16);
        rom[GBA_HEADER_SIZE..GBA_HEADER_SIZE + 7].copy_from_slice(b"SRAM_V1");
        let cartridge = GbaCartridge::parse(&rom).unwrap();
        assert_eq!(cartridge.save_type(), Some(crate::save::SaveType::Sram));
    }

    #[test]
    fn unknown_save_type_is_not_guessed() {
        let cartridge = GbaCartridge::parse(&valid_rom(GBA_HEADER_SIZE)).unwrap();
        assert_eq!(cartridge.save_type(), None);
    }

    #[test]
    fn rejects_truncated_header() {
        assert_eq!(
            GbaCartridge::parse(&[0; GBA_HEADER_SIZE - 1]),
            Err(CartridgeError::TooSmall {
                actual: GBA_HEADER_SIZE - 1
            })
        );
    }

    #[test]
    fn rejects_invalid_header_checksum() {
        let mut rom = valid_rom(GBA_HEADER_SIZE);
        rom[0xBD] ^= 0xFF;
        assert!(matches!(
            GbaCartridge::parse(&rom),
            Err(CartridgeError::InvalidHeaderChecksum { .. })
        ));
    }

    #[test]
    fn rejects_images_over_the_raw_rom_limit() {
        let mut rom = vec![0; MAX_ROM_SIZE + 1];
        rom[0xBD] = 0;
        assert_eq!(
            GbaCartridge::parse(&rom),
            Err(CartridgeError::TooLarge {
                actual: MAX_ROM_SIZE + 1
            })
        );
    }


    #[test]
    fn sanitizes_non_printable_header_metadata_without_rejecting_rom() {
        let mut rom = valid_rom(GBA_HEADER_SIZE);
        rom[0xA0..0xAC].fill(0);
        rom[0xA0] = 0x01;
        rom[0xA1] = b'A';
        rom[0xA2] = 0x7F;
        rom[0xA3] = b' ';
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();

        let cartridge = GbaCartridge::parse(&rom).unwrap();
        assert_eq!(cartridge.title(), "?A?");
    }

    #[test]
    fn rejects_a_header_checksum_changed_after_rom_creation() {
        let mut rom = valid_rom(GBA_HEADER_SIZE);
        rom[0xBD] ^= 0x01;

        assert!(matches!(
            GbaCartridge::parse(&rom),
            Err(CartridgeError::InvalidHeaderChecksum { .. })
        ));
    }

}
