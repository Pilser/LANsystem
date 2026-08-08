//! Minimal unique id generator: millis << 3 | sequence, monotone within a
//! millisecond. Good enough for a single-process LAN server.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQUENCE: AtomicI64 = AtomicI64::new(0);

/// Returns a new monotone-ish id. Collisions are impossible within a single
/// process (sequence counter); across restarts the timestamp component
/// dominates.
pub fn next() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed) & 0x3ff;
    millis.wrapping_shl(10) | sequence
}