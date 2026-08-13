//! Load average. Windows reports 0 and is not a real signal.

use sysinfo::System;

/// `(load1, load5, load15)`. All `None` on Windows.
pub fn load_averages() -> (Option<f64>, Option<f64>, Option<f64>) {
    if cfg!(target_os = "windows") {
        return (None, None, None);
    }
    let load = System::load_average();
    (
        Some(load.one.max(0.0)),
        Some(load.five.max(0.0)),
        Some(load.fifteen.max(0.0)),
    )
}
