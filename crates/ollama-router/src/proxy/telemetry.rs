//! Incremental NDJSON / SSE collector for upstream timing telemetry.
//!
//! Never mutates forwarded chunks. Unterminated frames are capped at 1 MiB.

use serde_json::Value;

/// Telemetry is best-effort. An unterminated frame must not grow without bound.
pub const MAX_INCOMPLETE_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FrameMode {
    #[default]
    Ndjson,
    Sse,
}

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
            if let Some(value) = json_positive_f64(map.get(src)) {
                *dst = Some(value);
            }
        }
        if let Some(usage) = map.get("usage").and_then(Value::as_object) {
            if let Some(value) = json_positive_f64(usage.get("prompt_tokens")) {
                self.prompt_tokens = Some(value);
            }
            if let Some(value) = json_positive_f64(usage.get("completion_tokens")) {
                self.eval_tokens = Some(value);
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
    mode: FrameMode,
}

impl IncrementalCollector {
    pub fn new() -> Self {
        Self::with_mode(FrameMode::Ndjson)
    }

    /// SSE when `Content-Type` is `text/event-stream` (parameter-insensitive).
    pub fn for_content_type(content_type: Option<&str>) -> Self {
        let sse = content_type.is_some_and(|ct| {
            ct.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("text/event-stream"))
        });
        Self::with_mode(if sse {
            FrameMode::Sse
        } else {
            FrameMode::Ndjson
        })
    }

    fn with_mode(mode: FrameMode) -> Self {
        Self {
            timing: UpstreamTiming::new(),
            buf: Vec::new(),
            discarding_oversize_frame: false,
            mode,
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
            self.feed_complete_line(line);
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
        let pending = std::mem::take(&mut self.buf);
        self.feed_complete_line(&pending);
    }

    fn feed_complete_line(&mut self, line: &[u8]) {
        let stripped = trim_ascii(line);
        if stripped.is_empty() {
            return;
        }
        let payload = match self.mode {
            FrameMode::Ndjson => stripped,
            FrameMode::Sse => {
                if stripped.first() == Some(&b':') {
                    return;
                }
                let Some(rest) = stripped.strip_prefix(b"data:") else {
                    return;
                };
                let payload = trim_ascii(rest);
                if payload == b"[DONE]" {
                    return;
                }
                payload
            }
        };
        if let Ok(obj) = serde_json::from_slice::<Value>(payload) {
            self.timing.feed_frame(&obj);
        }
    }
}

impl Default for IncrementalCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn json_positive_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let n = value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))?;
    (n > 0.0).then_some(n)
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

    #[test]
    fn sse_mode_maps_usage_and_ignores_done() {
        let mut collector =
            IncrementalCollector::for_content_type(Some("text/event-stream; charset=utf-8"));
        let chunk = concat!(
            "data: {\"id\":\"cmpl-1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n",
            "\n",
            "data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":5}}\n",
            "\n",
            "data: [DONE]\n",
            "\n",
        );
        let before = chunk.as_bytes().to_vec();
        collector.feed(chunk.as_bytes());
        assert_eq!(chunk.as_bytes(), before.as_slice());
        collector.flush();
        assert_eq!(collector.timing.prompt_tokens, Some(11.0));
        assert_eq!(collector.timing.eval_tokens, Some(5.0));
    }

    #[test]
    fn sse_mode_caps_unterminated_frame() {
        let mut collector = IncrementalCollector::for_content_type(Some("text/event-stream"));
        let mut huge = b"data: ".to_vec();
        huge.resize(6 + MAX_INCOMPLETE_FRAME_BYTES + 1, b'x');
        collector.feed(&huge);
        assert!(collector.pending_bytes() <= MAX_INCOMPLETE_FRAME_BYTES);
        collector.feed(b"\ndata: {\"usage\":{\"completion_tokens\":3}}\n\n");
        assert_eq!(collector.timing.eval_tokens, Some(3.0));
    }

    #[test]
    fn ndjson_content_type_stays_ndjson() {
        let mut collector = IncrementalCollector::for_content_type(Some("application/x-ndjson"));
        collector.feed(b"data: {\"eval_count\":9}\n");
        assert_eq!(collector.timing.eval_tokens, None);
        collector.feed(b"{\"eval_count\":9}\n");
        assert_eq!(collector.timing.eval_tokens, Some(9.0));
    }
}
