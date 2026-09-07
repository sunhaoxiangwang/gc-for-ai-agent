//! Measurement helpers: tree size and idle age.
//!
//! These live here rather than beside the code because `reap-core` is checked
//! for the absence of filesystem write calls (see `tests/invariants.rs`), and
//! building a fixture requires writing.

use std::time::{Duration, SystemTime};

use reap_core::fsmeta::*;

#[test]
fn tree_size_counts_a_hard_link_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    std::fs::write(&a, vec![0u8; 8192]).unwrap();
    std::fs::hard_link(&a, dir.path().join("b")).unwrap();

    let size = tree_size(dir.path());
    assert_eq!(size.files, 2, "both links are visited");
    // One 8 KiB payload plus the directory's own blocks, well under two
    // payloads. If the dedup broke this would be at least 16 KiB.
    assert!(size.bytes < 16_384, "counted {} bytes", size.bytes);
}

#[test]
fn tree_size_does_not_follow_symlinks_out_of_the_tree() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("big"), vec![0u8; 1024 * 1024]).unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

    let size = tree_size(dir.path());
    assert_eq!(size.files, 1, "the symlink itself, not what it points at");
    assert!(size.bytes < 1024 * 1024);
}

#[test]
fn idle_for_sees_a_freshly_written_child() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fresh"), b"x").unwrap();
    let idle = idle_for(dir.path(), SystemTime::now(), IDLE_PROBE_LIMIT).unwrap();
    assert!(idle < Duration::from_secs(60), "idle was {idle:?}");
}

#[test]
fn idle_for_reports_zero_when_the_clock_runs_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    assert_eq!(
        idle_for(dir.path(), past, IDLE_PROBE_LIMIT).unwrap(),
        Duration::ZERO
    );
}
