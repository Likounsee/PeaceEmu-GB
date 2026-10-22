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
fn cascade_timer_ticks_only_when_preceding_timer_overflows() {
    let mut bus = MemoryBus::new(cartridge());
    bus.write16(0x0400_0100, 0xFFFF); // TM0 overflows every cycle
    bus.write16(0x0400_0104, 0xFFFE); // TM1 reloads to FFFE
    bus.write16(0x0400_0106, 0x0084); // TM1 cascade, enabled
    bus.write16(0x0400_0102, 0x0080); // TM0 enabled
    bus.advance_cycles(1);

    assert_eq!(bus.timer_counter(0), Some(0xFFFF));
    assert_eq!(bus.timer_counter(1), Some(0xFFFF));
    assert_eq!(bus.timer_cycles_until_overflow(1), None);
}
