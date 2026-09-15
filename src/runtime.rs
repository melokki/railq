//! Process-owned runtime sources.
//!
//! Wall-clock time and nondeterministic world seeds enter RailQ here. Domain,
//! application, and presentation code receive those values explicitly instead
//! of reading process state themselves.

use std::time::{SystemTime, UNIX_EPOCH};

use railq::model::UtcSeconds;

/// Returns the current wall-clock time as RailQ's persisted UTC timestamp.
pub fn current_utc_seconds() -> UtcSeconds {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    UtcSeconds::from_unix_seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

/// Produces a best-effort process-local seed for a newly generated world.
pub fn new_world_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    nanos ^ u64::from(std::process::id())
}
