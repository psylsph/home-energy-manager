// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--headless") {
        givenergy_local::run_headless(&args);
    } else {
        // Must precede the Tauri builder: on Raspberry Pi hardware the
        // accelerated WebKitGTK path corrupts the first paint (issue #298).
        givenergy_local::pi_render::apply_pi_webkit_workaround();
        givenergy_local::run();
    }
}
