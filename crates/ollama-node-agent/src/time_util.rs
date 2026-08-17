//! Shared timestamps for setup state, capacity reports, and pressure.

/// RFC3339 UTC timestamp for `collected_at` and converge state.
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

/// Unix epoch seconds with fractional part for pressure `collected_at`.
pub fn epoch_now() -> Option<f64> {
    let ns = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    Some(ns as f64 / 1_000_000_000.0)
}
