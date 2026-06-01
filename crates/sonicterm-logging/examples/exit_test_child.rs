//! Child binary used by `tests/exit_trace.rs` to exercise every exit
//! path under a real process. Dispatched on the `SONIC_EXIT_TEST_MODE`
//! env var. Log + crash directories are taken from
//! `SONIC_LOG_DIR` so each test run uses a fresh tempdir and asserts
//! on its contents without racing the user's real logs.

use std::path::PathBuf;

fn main() {
    let mode = std::env::var("SONIC_EXIT_TEST_MODE").unwrap_or_else(|_| "clean".to_string());
    let log_dir = PathBuf::from(std::env::var("SONIC_LOG_DIR").expect("SONIC_LOG_DIR env not set"));
    std::fs::create_dir_all(&log_dir).expect("mkdir log_dir");
    sonicterm_logging::install_panic_hook(log_dir.clone());
    let cfg = sonicterm_logging::LoggingConfig {
        level: std::env::var("SONIC_EXIT_TEST_FILTER").ok(),
        ..Default::default()
    };
    let _g = sonicterm_logging::init(&cfg).expect("init log");
    let _exit_guard = sonicterm_logging::install_exit_logging(&log_dir);

    match mode.as_str() {
        "panic_main" => {
            panic!("exit-test: main-thread panic");
        }
        "panic_thread" => {
            let h = std::thread::spawn(|| panic!("exit-test: spawned-thread panic"));
            let _ = h.join();
            // Give the appender time to flush.
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        "segv" => {
            tracing::info!("about to SIGSEGV");
            // Best-effort flush of the appender before the trap.
            std::thread::sleep(std::time::Duration::from_millis(50));
            unsafe {
                let p: *mut u8 = std::ptr::null_mut();
                std::ptr::write_volatile(p, 1);
            }
        }
        "exit3" => {
            // Raw process::exit — drop guard should still log "clean exit".
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::process::exit(3);
        }
        "exit_with" => {
            sonicterm_logging::exit_with(4, "test-explicit-exit");
        }
        "clean" => {
            tracing::info!("clean child: returning normally");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        "target_filter_clean" => {
            tracing::info!(target: "sonic_exit", "filtered clean child: returning normally");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        other => {
            eprintln!("unknown SONIC_EXIT_TEST_MODE={other}");
            std::process::exit(99);
        }
    }
}
