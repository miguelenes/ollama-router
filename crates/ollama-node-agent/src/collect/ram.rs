//! RAM available-source labels. Never report Linux `MemAvailable` off Linux.

pub fn ram_available_source() -> &'static str {
    if cfg!(target_os = "linux") {
        "MemAvailable"
    } else {
        "sysinfo"
    }
}
