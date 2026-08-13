//! Optional Windows display-adapter names (no fake VRAM).

/// Parse `Get-CimInstance Win32_VideoController` Name lines.
pub fn parse_video_controller_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.eq_ignore_ascii_case("name"))
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adapter_names() {
        let raw = "Name\r\nNVIDIA GeForce RTX 3070\r\n";
        assert_eq!(
            parse_video_controller_names(raw),
            ["NVIDIA GeForce RTX 3070"]
        );
    }
}
