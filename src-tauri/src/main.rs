#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--supervise-ffmpeg" {
        let code = ffmpeg_queue_lib::platform::run_supervisor_entrypoint(&args[2..]);
        std::process::exit(code);
    }
    ffmpeg_queue_lib::run();
}
