use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub fn mark(label: &str) {
    let Some(path) = std::env::var_os("ZS_STARTUP_PROFILE") else {
        return;
    };
    let start = *START.get_or_init(Instant::now);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{ms:.3}\t{label}");
    }
}

pub fn exit_after_first_paint() -> bool {
    std::env::var_os("ZS_EXIT_AFTER_FIRST_PAINT").is_some()
}
