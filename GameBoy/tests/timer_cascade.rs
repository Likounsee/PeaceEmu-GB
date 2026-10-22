use gameboy::GbaCartridge;
use gameboy::MemoryBus;

fn cartridge() -> GbaCartridge {
    let mut rom = vec![0; 0xC0];
    rom[0xB2] = 0x96;
    let sum = rom[0xA0..=0xBC]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    rom[0xBD] = 0_u8.wrapping_sub(sum).wrapping_sub(0x19);
    GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid")
}

#[test]
fn timer_overflow_reloads_counter_and_latches_irq() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0400_0100, 0xFFFF); // TM0 reload
    bus.write16(0x0400_0102, 0x0040); // IRQ enabled, timer disabled
    bus.write16(0x0400_0102, 0x00C0); // enable
    bus.advance_cycles(1);

    assert_eq!(bus.timer_counter(0), Some(0xFFFF));
    assert_eq!(bus.interrupt_request_flags() & (1 << 3), 1 << 3);
}

#[test]
fn cascade_timer_waits_for_the_prescaled_source_overflow() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0400_0100, 0xFFFF); // TM0 reloads on every tick
    bus.write16(0x0400_0102, 0x0081); // TM0 enabled, prescaler /64
    bus.write16(0x0400_0104, 0xFFFE); // TM1 reloads to FFFE
    bus.write16(0x0400_0106, 0x0084); // TM1 enabled in cascade mode

    bus.advance_cycles(63);
    assert_eq!(bus.timer_counter(0), Some(0xFFFF));
    assert_eq!(bus.timer_prescaler_phase(0), Some(63));
    assert_eq!(bus.timer_counter(1), Some(0xFFFE));

    bus.advance_cycles(1);
    assert_eq!(bus.timer_prescaler_phase(0), Some(0));
    assert_eq!(bus.timer_counter(1), Some(0xFFFF));
    assert_eq!(bus.timer_cycles_until_overflow(1), None);
}

#[test]
fn timer_overflow_cascades_through_multiple_enabled_timers() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0400_0100, 0xFFFF);
    bus.write16(0x0400_0104, 0xFFFF);
    bus.write16(0x0400_0106, 0x0084); // TM1 cascades from TM0
    bus.write16(0x0400_0108, 0xFFFE);
    bus.write16(0x0400_010A, 0x0084); // TM2 cascades from TM1
    bus.write16(0x0400_0102, 0x00C0); // TM0 enabled and IRQ enabled

    bus.advance_cycles(1);

    assert_eq!(bus.timer_counter(0), Some(0xFFFF));
    assert_eq!(bus.timer_counter(1), Some(0xFFFF));
    assert_eq!(bus.timer_counter(2), Some(0xFFFF));
}

#[test]
fn hblank_dma_waits_for_the_hblank_edge_then_copies_data() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0200_0000, 0xCAFE);
    bus.write32(0x0400_00B0, 0x0200_0000); // DMA0 source
    bus.write32(0x0400_00B4, 0x0300_0000); // DMA0 destination
    bus.write16(0x0400_00B8, 1);
    bus.write16(0x0400_00BA, 0xA000); // enabled, HBlank timing

    bus.advance_cycles(959);
    assert_eq!(bus.read16(0x0300_0000), 0);
    assert_eq!(bus.dma_enabled(0), Some(true));

    bus.advance_cycles(1);
    assert_eq!(bus.read16(0x0300_0000), 0xCAFE);
    assert_eq!(bus.dma_enabled(0), Some(false));
}

#[test]
fn keypad_irq_supports_or_and_conditions_and_write_one_to_clear() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0400_0132, 0x4003); // IRQ on A OR B
    bus.set_button_pressed(gameboy::GbaButton::A, true);
    assert_eq!(bus.interrupt_request_flags() & (1 << 12), 1 << 12);

    bus.write16(0x0400_0202, 1 << 12); // acknowledge keypad IRQ
    assert_eq!(bus.interrupt_request_flags() & (1 << 12), 0);

    bus.write16(0x0400_0132, 0xC003); // IRQ on A AND B
    assert_eq!(bus.interrupt_request_flags() & (1 << 12), 0);
    bus.set_button_pressed(gameboy::GbaButton::B, true);
    assert_eq!(bus.interrupt_request_flags() & (1 << 12), 1 << 12);
}
