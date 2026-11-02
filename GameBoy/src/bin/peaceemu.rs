//! Minimal cross-platform desktop frontend for the experimental GBA core.

use gameboy::{GbaButton, GbaCartridge, GbaMachine, SCREEN_HEIGHT, SCREEN_WIDTH};
use minifb::{Key, Scale, Window, WindowOptions};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_STEPS_PER_FRAME: usize = 500_000;

fn main() -> Result<(), Box<dyn Error>> {
    let rom_path = env::args_os().nth(1).map(PathBuf::from).ok_or(
        "usage: peaceemu-gba <game.gba>\nPass a legally obtained GBA ROM image as the first argument.",
    )?;
    let rom = fs::read(&rom_path)?;
    let cartridge = GbaCartridge::parse(&rom)?;
    let title = if cartridge.title().is_empty() {
        rom_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Game Boy Advance")
    } else {
        cartridge.title()
    };

    let save_path = save_path_for(&rom_path);
    let mut machine = GbaMachine::new(cartridge);
    if let Ok(save) = fs::read(&save_path) {
        match machine.load_save_data(&save) {
            Ok(true) => println!("Loaded battery save: {}", save_path.display()),
            Ok(false) => eprintln!("Ignoring save file: cartridge has no detected save device"),
            Err(error) => eprintln!("Ignoring incompatible save file: {error}"),
        }
    }

    let mut window = Window::new(
        &format!("PeaceEmu GBA — {title}"),
        SCREEN_WIDTH,
        SCREEN_HEIGHT,
        WindowOptions {
            scale: Scale::X4,
            resize: true,
            ..WindowOptions::default()
        },
    )?;
    window.limit_update_rate(Some(Duration::from_micros(16_667)));
    let mut framebuffer = vec![0_u32; SCREEN_WIDTH * SCREEN_HEIGHT];

    println!("PeaceEmu GBA running: {title}");
    println!("Controls: Z=A, X=B, A=L, S=R, Enter=Start, Backspace=Select, arrows=D-pad, Esc=Quit");

    while window.is_open() && !window.is_key_down(Key::Escape) {
        update_input(&window, &mut machine);
        match machine.run_frame(MAX_STEPS_PER_FRAME) {
            Ok(Some(frame)) => {
                for (destination, source) in framebuffer.iter_mut().zip(frame.pixels.iter()) {
                    *destination = *source & 0x00FF_FFFF;
                }
            }
            Ok(None) => {
                eprintln!("The emulated CPU did not reach a renderable frame within the instruction budget.");
                break;
            }
            Err(error) => {
                eprintln!("Emulation stopped on an unsupported CPU instruction: {error}");
                break;
            }
        }
        window.update_with_buffer(&framebuffer, SCREEN_WIDTH, SCREEN_HEIGHT)?;
    }

    if let Some(save) = machine.save_data() {
        fs::write(&save_path, save)?;
        println!("Saved battery data: {}", save_path.display());
    }
    Ok(())
}

fn save_path_for(rom_path: &Path) -> PathBuf {
    let mut path = rom_path.to_path_buf();
    path.set_extension("sav");
    path
}

fn update_input(window: &Window, machine: &mut GbaMachine) {
    let bindings = [
        (Key::Z, GbaButton::A),
        (Key::X, GbaButton::B),
        (Key::Backspace, GbaButton::Select),
        (Key::Enter, GbaButton::Start),
        (Key::Right, GbaButton::Right),
        (Key::Left, GbaButton::Left),
        (Key::Up, GbaButton::Up),
        (Key::Down, GbaButton::Down),
        (Key::S, GbaButton::R),
        (Key::A, GbaButton::L),
    ];
    for (key, button) in bindings {
        machine.set_button_pressed(button, window.is_key_down(key));
    }
}
