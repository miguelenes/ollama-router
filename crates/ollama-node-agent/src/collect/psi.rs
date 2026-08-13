//! Linux PSI (`/proc/pressure/*`) `some avg10=` parser.

/// Parse `some avg10=` from a `/proc/pressure/{memory,cpu}` snippet.
pub fn parse_psi_some_avg10(text: &str) -> Option<f64> {
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("some") {
            continue;
        }
        for part in line.split_whitespace() {
            if let Some(rest) = part.strip_prefix("avg10=") {
                return rest
                    .parse()
                    .ok()
                    .filter(|v: &f64| v.is_finite() && *v >= 0.0);
            }
        }
    }
    None
}

/// `(memory_some_avg10, cpu_some_avg10)` when `/proc/pressure` is readable.
pub fn read_psi() -> (Option<f64>, Option<f64>) {
    #[cfg(target_os = "linux")]
    {
        let mem = std::fs::read_to_string("/proc/pressure/memory")
            .ok()
            .and_then(|raw| parse_psi_some_avg10(&raw));
        let cpu = std::fs::read_to_string("/proc/pressure/cpu")
            .ok()
            .and_then(|raw| parse_psi_some_avg10(&raw));
        (mem, cpu)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canned_proc_snippet() {
        let raw = "some avg10=1.25 avg60=0.80 avg300=0.40 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=1\n";
        assert!((parse_psi_some_avg10(raw).unwrap() - 1.25).abs() < 1e-9);
    }

    #[test]
    fn missing_some_line_is_none() {
        assert!(parse_psi_some_avg10("full avg10=9.00\n").is_none());
        assert!(parse_psi_some_avg10("").is_none());
    }
}
