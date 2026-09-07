#![allow(dead_code)]
//! Exit codes. These are part of the public interface: a scheduler unit and a
//! supervisor script both branch on them.

/// Everything asked for was done.
pub const SUCCESS: i32 = 0;
/// An unexpected internal failure.
pub const INTERNAL: i32 = 1;
/// The configuration is missing, unparseable or invalid.
pub const CONFIG: i32 = 2;
/// Nothing matched, so there was nothing to do. Not a failure.
pub const NOTHING_TO_DO: i32 = 3;
/// Some work succeeded and some did not.
pub const PARTIAL: i32 = 4;
