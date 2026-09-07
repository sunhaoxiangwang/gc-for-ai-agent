//! Time and unit formatting.

use std::time::{Duration, UNIX_EPOCH};

use reap_core::time::*;

#[test]
fn rfc3339_matches_known_instants() {
    assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    assert_eq!(
        rfc3339(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        "2023-11-14T22:13:20Z"
    );
    // A leap day, to exercise the era arithmetic.
    assert_eq!(
        rfc3339(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
        "2024-02-29T00:00:00Z"
    );
}

#[test]
fn human_bytes_is_stable() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(999), "999 B");
    assert_eq!(human_bytes(1024), "1.00 KiB");
    assert_eq!(human_bytes(13_314_398_617), "12.4 GiB");
}

#[test]
fn human_duration_picks_one_unit() {
    assert_eq!(human_duration(Duration::from_secs(45)), "45s");
    assert_eq!(human_duration(Duration::from_secs(3600)), "60m");
    assert_eq!(human_duration(Duration::from_secs(86_400)), "24h");
    assert_eq!(human_duration(Duration::from_secs(864_000)), "10d");
}
