// Без консольного окна при запуске двойным кликом в Windows
#![cfg_attr(windows, windows_subsystem = "windows")]

mod browser;
mod cities;
mod cli;
mod events;
mod filter;
mod gui;
mod job;
mod lang;
mod model;
mod parser;
mod writer;

fn main() {
    // С аргументами — командная строка, без — оконный интерфейс
    if std::env::args_os().len() > 1 {
        #[cfg(windows)]
        attach_console();
        std::process::exit(cli::main());
    }
    if let Err(e) = gui::run() {
        #[cfg(windows)]
        attach_console();
        eprintln!("Не удалось открыть окно: {e}");
        std::process::exit(1);
    }
}

/// GUI-сборка не имеет своей консоли — подключаемся к консоли, из которой
/// запустили программу (cmd/PowerShell), чтобы был виден вывод CLI.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
