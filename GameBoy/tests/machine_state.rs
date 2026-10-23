use gameboy::{GbaCartridge, GbaMachine};

fn looping_cartridge() -> GbaCartridge {
    // ARM branch to itself at the cartridge entry point (0x0800_0000).
    let mut rom = vec![0; 0xC0];
    rom[0..4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    rom[0xB2] = 0x96;
    let sum = rom[0xA0..=0xBC]
        .iter()
        .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte));
    rom[0xBD] = sum.wrapping_neg();
    GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid")
}

#[test]
fn save_state_restores_cpu_video_and_execution_counters_for_replay() {
    let mut machine = GbaMachine::new(looping_cartridge());
    machine.step().expect("branch instruction should execute");
    let checkpoint = machine.save_state();
    let checkpoint_snapshot = machine.diagnostic_snapshot();

    machine
        .run_for_instructions(12)
        .expect("loop should keep executing");
    let future_snapshot = machine.diagnostic_snapshot();
    assert_ne!(future_snapshot, checkpoint_snapshot);

    machine.load_state(&checkpoint);
    assert_eq!(machine.diagnostic_snapshot(), checkpoint_snapshot);

    machine
        .run_for_instructions(12)
        .expect("restored loop should execute");
    assert_eq!(machine.diagnostic_snapshot(), future_snapshot);
}

#[test]
fn zero_sized_run_budgets_do_not_advance_machine_state() {
    let mut machine = GbaMachine::new(looping_cartridge());
    let before = machine.diagnostic_snapshot();

    assert_eq!(machine.run_for_instructions(0).unwrap(), 0);
    assert_eq!(machine.run_for_cycles(0).unwrap(), 0);
    assert_eq!(machine.diagnostic_snapshot(), before);
}

#[test]
fn video_position_runner_reaches_requested_raster_offset() {
    let mut machine = GbaMachine::new(looping_cartridge());

    let executed = machine
        .run_until_video_position(0, 12, 32)
        .expect("valid video position should not produce a CPU error")
        .expect("loop should reach the requested raster offset");

    assert!(executed > 0);
    let snapshot = machine.diagnostic_snapshot();
    assert_eq!(snapshot.video_scanline, 0);
    assert!(snapshot.video_cycles_into_scanline >= 12);
}

#[test]
fn invalid_video_position_does_not_execute_instructions() {
    let mut machine = GbaMachine::new(looping_cartridge());
    let before = machine.diagnostic_snapshot();

    assert_eq!(machine.run_until_video_position(228, 0, 100).unwrap(), None);
    assert_eq!(
        machine.run_until_video_position(0, 1232, 100).unwrap(),
        None
    );
    assert_eq!(machine.diagnostic_snapshot(), before);
}

#[test]
fn cycle_runner_executes_whole_instruction_and_reports_its_cycles() {
    let mut machine = GbaMachine::new(looping_cartridge());

    let elapsed = machine.run_for_cycles(1).unwrap();

    assert!(elapsed >= 1);
    assert_eq!(
        machine.diagnostic_snapshot().cycles_elapsed,
        u64::from(elapsed)
    );
    assert_eq!(machine.diagnostic_snapshot().instructions_executed, 1);
}

#[test]
fn video_position_runner_returns_zero_if_already_at_target() {
    let mut machine = GbaMachine::new(looping_cartridge());
    let before = machine.diagnostic_snapshot();

    assert_eq!(machine.run_until_video_position(0, 0, 10).unwrap(), Some(0));
    assert_eq!(machine.diagnostic_snapshot(), before);
}

#[test]
fn save_state_preserves_stop_wait_and_keypad_wake_resumes_program() {
    let mut rom = vec![0; 0xC0];
    rom[0..4].copy_from_slice(&0xEF00_0003_u32.to_le_bytes()); // SWI Stop.
    rom[4..8].copy_from_slice(&0xE3A0_0007_u32.to_le_bytes()); // MOV r0, #7.
    rom[0xB2] = 0x96;
    let sum = rom[0xA0..=0xBC]
        .iter()
        .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte));
    rom[0xBD] = sum.wrapping_neg();
    let cartridge = GbaCartridge::parse(&rom).expect("synthetic GBA header should be valid");
    let mut machine = GbaMachine::new(cartridge);

    machine.step().expect("Stop instruction should execute");
    assert!(machine.diagnostic_snapshot().cpu.stopped);
    let checkpoint = machine.save_state();

    machine.bus_mut().write16(0x0400_0132, 0x4001); // KEYCNT: IRQ on A.
    machine.bus_mut().write16(0x0400_0200, 1 << 12); // IE: keypad IRQ.
    machine.set_button_pressed(gameboy::GbaButton::A, true);
    machine.step().expect("keypad IRQ should wake Stop");
    assert!(!machine.diagnostic_snapshot().cpu.stopped);
    assert_eq!(machine.diagnostic_snapshot().cpu.registers[0], 7);

    machine.load_state(&checkpoint);
    assert!(machine.diagnostic_snapshot().cpu.stopped);
    assert_eq!(machine.diagnostic_snapshot().cpu.pc, 0x0800_0004);

    machine.bus_mut().write16(0x0400_0132, 0x4001);
    machine.bus_mut().write16(0x0400_0200, 1 << 12);
    machine.set_button_pressed(gameboy::GbaButton::A, true);
    machine.step().expect("restored Stop should also wake");
    assert!(!machine.diagnostic_snapshot().cpu.stopped);
    assert_eq!(machine.diagnostic_snapshot().cpu.registers[0], 7);
}

