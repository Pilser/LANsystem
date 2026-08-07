use crate::DateTime;

/// Current UTC time as a HelixDB [`DateTime`] (epoch milliseconds).
pub fn now() -> DateTime {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    DateTime::from_millis(millis)
}

/// Current UTC time as the canonical RFC3339 string Helix expects for
/// `DateTime`-typed query parameters.
pub fn now_rfc3339() -> Option<String> {
    now().to_rfc3339()
}