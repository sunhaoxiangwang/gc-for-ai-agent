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

#[test]
fn parse_rfc3339_round_trips_what_we_emit() {
    for secs in [0u64, 1_700_000_000, 1_709_164_800] {
        let t = UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t), "round trip at {secs}");
    }
}

#[test]
fn parse_rfc3339_accepts_offsets_and_fractions() {
    let utc = parse_rfc3339("2024-02-29T00:00:00Z").unwrap();
    assert_eq!(parse_rfc3339("2024-02-29T00:00:00.123456Z"), Some(utc));
    assert_eq!(parse_rfc3339("2024-02-29T02:30:00+02:30"), Some(utc));
    assert_eq!(parse_rfc3339("2024-02-28T21:30:00-02:30"), Some(utc));
}

#[test]
fn parse_rfc3339_refuses_what_it_cannot_place_in_time() {
    // No zone at all: assuming one could misjudge a heartbeat's age by hours.
    assert_eq!(parse_rfc3339("2024-02-29T00:00:00"), None);
    assert_eq!(parse_rfc3339(""), None);
    assert_eq!(parse_rfc3339("yesterday"), None);
    assert_eq!(parse_rfc3339("2024-13-01T00:00:00Z"), None);
    assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), None);
}
