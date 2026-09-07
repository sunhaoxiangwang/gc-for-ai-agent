//! Core of `reap`: configuration, rule matching, the planner and the guard
//! stack.
//!
//! This crate is read-only by construction. It contains no call that creates,
//! renames, truncates or removes anything on disk, so "the planner cannot
//! delete" is a property of what is compiled rather than a convention. The
//! `reap-cli` crate is the only place that mutates.
//!
//! It depends on `reap-platform` for the two trait definitions the guard stack
//! needs, not for any platform-conditional code of its own.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod fsmeta;
pub mod plan;
pub mod schema;
pub mod time;
