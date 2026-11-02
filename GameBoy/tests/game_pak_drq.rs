use gameboy::{GbaCartridge, MemoryBus};

fn cartridge() -> GbaCartridge {
    let mut rom = vec![0; 0xC0];
    rom[0xB2] = 0x96;
    let checksum = rom[0xA0..=0xBC]
        .iter()
        .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte));
    rom[0xBD] = checksum.wrapping_neg();
    GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid")
}

#[test]
fn repeated_game_pak_drq_requests_reuse_programmed_count_and_keep_dma_armed() {
    let mut bus = MemoryBus::new(cartridge());

    bus.write16(0x0200_0100, 0xCAFE);
    bus.write16(0x0200_0102, 0xBEEF);
    bus.write32(0x0400_00D4, 0x0200_0100); // DMA3 source: increment.
    bus.write32(0x0400_00D8, 0x0300_0100); // DMA3 destination: fixed.
    bus.write16(0x0400_00DC, 1); // One halfword per external request.
    // Enable + IRQ + repeat + Game Pak DRQ + special timing + fixed destination.
    bus.write16(0x0400_00DE, 0xFA40);

    assert!(bus.request_game_pak_dma());
    assert_eq!(bus.read16(0x0300_0100), 0xCAFE);
    assert_eq!(bus.dma_source_address(3), Some(0x0200_0102));
    assert_eq!(bus.dma_destination_address(3), Some(0x0300_0100));
    assert_eq!(bus.dma_enabled(3), Some(true));
    assert_eq!(bus.interrupt_request_flags() & (1 << 11), 1 << 11);

    bus.write16(0x0400_0202, 1 << 11); // Acknowledge first completion IRQ.
    assert!(bus.request_game_pak_dma());
    assert_eq!(bus.read16(0x0300_0100), 0xBEEF);
    assert_eq!(bus.dma_source_address(3), Some(0x0200_0104));
    assert_eq!(bus.dma_destination_address(3), Some(0x0300_0100));
    assert_eq!(bus.dma_enabled(3), Some(true));
    assert_eq!(bus.interrupt_request_flags() & (1 << 11), 1 << 11);
}

#[test]
fn game_pak_drq_request_does_not_advance_video_or_timer_clocks() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0200_0100, 0x1234);
    bus.write32(0x0400_00D4, 0x0200_0100);
    bus.write32(0x0400_00D8, 0x0300_0100);
    bus.write16(0x0400_00DC, 1);
    bus.write16(0x0400_00DE, 0xD800); // Enable + IRQ + DRQ.

    let video_before = bus.video_scanline();
    let video_cycles_before = bus.video_cycles_into_scanline();
    let timer_before = bus.timer_counter(0);

    assert!(bus.request_game_pak_dma());

    assert_eq!(bus.video_scanline(), video_before);
    assert_eq!(bus.video_cycles_into_scanline(), video_cycles_before);
    assert_eq!(bus.timer_counter(0), timer_before);
    assert_eq!(bus.read16(0x0300_0100), 0x1234);
}
