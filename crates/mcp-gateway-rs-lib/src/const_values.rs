use std::time::Duration;

pub const LRU_CACHE_ENTRIES: usize = 50_000;
pub const LRU_CACHE_EXPIRY_DURATION: Duration = Duration::from_hours(1);
