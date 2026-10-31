//! Sintonia sul PC: tutto sta nella libreria (`lib.rs`), che su Android parte da `android_main`.

// Niente console in release su Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    sintonia_app::main()
}
