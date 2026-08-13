//! Incremental NDJSON / JSON collector for upstream timing telemetry.
//!
//! Never mutates forwarded chunks. Unterminated frames are capped at 1 MiB.

use serde_json::Value;

/// Telemetry is best-effort. An unterminated frame must not grow without bound.
pub const MAX_INCOMPLETE_FRAME_BYTES: usize = 1024 * 1024;

/// Captured upstream telemetry from the last complete JSON frame.
#[derive(Clone, Debug)]
pub struct UpstreamTiming {
    pub total_ns: Option<f64>,
    pub load_ns: Option<f64>,
    pub prompt_eval_ns: Option<f64>,
    pub eval_ns: Option<f64>,
    pub prompt_tokens: Option<f64>,
    pub eval_tokens: Option<f64>,
    wall_start: std::time::Instant,
}

impl UpstreamTiming {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn wall_seconds(&self) -> f64 {
        self.wall_start.elapsed().as_secs_f64()
    }

    fn feed_frame(&mut self, obj: &Value) {
        let Some(map) = obj.as_object() else {
            return;
        };
        for (src, dst) in [
            ("total_duration", &mut self.total_ns),
            ("load_duration", &mut self.load_ns),
            ("prompt_eval_duration", &mut self.prompt_eval_ns),
            ("eval_duration", &mut self.eval_ns),
            ("prompt_eval_count", &mut self.prompt_tokens),
            ("eval_count", &mut self.eval_tokens),
        ] {
            if let Some(value) = map.get(src).and_then(Value::as_f64) {
                if value > 0.0 {
                    *dst = Some(value);
                }
            } else if let Some(value) = map.get(src).and_then(Value::as_i64) {
                if value > 0 {
                    *dst = Some(value as f64);
                }
            }
        }
    }
}

impl Default for UpstreamTiming {
    fn default() -> Self {
        Self {
            total_ns: None,
            load_ns: None,
            prompt_eval_ns: None,
            eval_ns: None,
            prompt_tokens: None,
            eval_tokens: None,
            wall_start: std::time::Instant::now(),
        }
    }
}

/// Feeds raw byte chunks; keeps only the last incomplete line in memory.
pub struct IncrementalCollector {
    pub timing: UpstreamTiming,
    buf: Vec<u8>,
    discarding_oversize_frame: bool,
}

impl IncrementalCollector {
    pub fn new() -> Self {
        Self {
            timing: UpstreamTiming::new(),
            buf: Vec::new(),
            discarding_oversize_frame: false,
        }
    }

    /// Buffer length (tests assert the 1 MiB cap).
    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }

    /// Feed one raw chunk from the upstream stream. Does not modify `chunk`.
    pub fn feed(&mut self, chunk: &[u8]) {
        let mut data = std::mem::take(&mut self.buf);
        data.extend_from_slice(chunk);
        let mut parts = data.split(|b| *b == b'\n');
        let remainder = parts.next_back().unwrap_or(&[]).to_vec();
        let mut complete: Vec<&[u8]> = parts.collect();
        if self.discarding_oversize_frame {
            if complete.is_empty() {
                return;
            }
            complete.remove(0);
            self.discarding_oversize_frame = false;
        }
        for line in complete {
            let stripped = trim_ascii(line);
            if stripped.is_empty() {
                continue;
            }
            if let Ok(obj) = serde_json::from_slice::<Value>(stripped) {
                self.timing.feed_frame(&obj);
            }
        }
        if remainder.len() > MAX_INCOMPLETE_FRAME_BYTES {
            self.buf.clear();
            self.discarding_oversize_frame = true;
            return;
        }
        self.buf = remainder;
    }

    /// Parse any remaining partial line (non-NDJSON responses land here).
    pub fn flush(&mut self) {
        if self.discarding_oversize_frame {
            self.buf.clear();
            self.discarding_oversize_frame = false;
            return;
        }
        if self.buf.is_empty() {
            return;
        }
        let stripped = trim_ascii(&self.buf);
        if !stripped.is_empty() {
            if let Ok(obj) = serde_json::from_slice::<Value>(stripped) {
                self.timing.feed_frame(&obj);
            }
        }
        self.buf.clear();
    }
}

impl Default for IncrementalCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    if start >= end {
        &[]
    } else {
        &bytes[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_collector_bounds_oversized_incomplete_frame_and_recovers() {
        let mut collector = IncrementalCollector::new();
        collector.feed(&vec![b'x'; MAX_INCOMPLETE_FRAME_BYTES + 1]);
        assert!(collector.pending_bytes() <= MAX_INCOMPLETE_FRAME_BYTES);
        collector.feed(
            br#"
{"eval_count": 7}
"#,
        );
        assert_eq!(collector.timing.eval_tokens, Some(7.0));
    }

    #[test]
    fn feed_does_not_alter_caller_chunk() {
        let mut collector = IncrementalCollector::new();
        let chunk = b"{\"eval_count\":1}\n";
        let before = chunk.to_vec();
        collector.feed(chunk);
        assert_eq!(chunk, before.as_slice());
    }
}
